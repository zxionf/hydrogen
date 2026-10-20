# 20261020-encrypted-persistence-core

<!-- 为什么改、边界与不属于本次的范围 -->

当前 SPEC 只有方向性目标，没有可执行的数据结构与 API 契约。本次把 shio 的首个能力——**加密持久化内核**——的定位、分层、数据模型、加密流程、存储布局与 API 边界定下来，作为后续实现的唯一依据。本次不写实现代码。

## 技术栈

- 语言：Rust（edition 2021，最低工具链随 `rust-toolchain.toml` 固定）
- 存储引擎：redb（单文件、嵌入式、MVCC、单写多读）
- 序列化：serde + serde_json（记录体编码，落盘前整体加密）
- 异步：tokio（仅外层 facade；core 保持同步）
- 加密：Argon2id（口令派生）、HKDF-SHA256（子密钥）、XChaCha20-Poly1305（AEAD）
- 错误：thiserror；标识：UUIDv7
- 稳定性：`shio-core` 与 `shio` 使用 `#![forbid(unsafe_code)]`；可恢复失败只返回错误，不以 panic 表达

## 分层与目录结构

依赖方向严格单向，core 不依赖 tokio，保证 Android、服务端与未来守护进程复用同一内核。

```
shio/
├── Cargo.toml                  # workspace
├── rust-toolchain.toml
├── crates/
│   ├── shio-core/              # 同步内核：模型、加密、存储、事务
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── error.rs
│   │       ├── model.rs        # Namespace/Collection/RecordMeta/RecordId
│   │       ├── codec.rs        # 复合键、header、AAD 的字节编码
│   │       ├── crypto/         # kdf、aead、keys
│   │       └── store/          # meta、keys、records 表
│   ├── shio/                   # async facade（tokio），面向 Rust 应用
│   │   └── src/{lib,kernel,collection}.rs
│   ├── shio-ffi/               # C ABI（后续；unsafe 例外待决策）
│   └── shio-sync/              # 多端同步（后续，占位）
└── tests/                      # 跨 crate 集成测试
```

- `shio-core`：无 async、无 tokio；持有 redb 与密钥材料，所有操作为同步函数。
- `shio`：把 core 的阻塞调用经 `spawn_blocking` 暴露为 async；管理生命周期与句柄。
- `shio-ffi`：仅为 C++/Vulkan GUI 与 Android 提供 C ABI，不承载业务逻辑。
- `shio-sync`：后续接入，只依赖 core 的模型与存储接口，不修改记录格式。

## 数据模型

三层寻址：**命名空间 → 集合 → 记录**。

| 层 | 含义 | 示例 |
|---|---|---|
| Namespace | 应用域，隔离不同功能与未来插件 | `notes`、`clipboard`、`memo` |
| Collection | 域内的记录类型 | `note`、`attachment`、`tag` |
| Record | 一条带 ID 的文档 | UUIDv7 + JSON 载荷 |

- `RecordId`：UUIDv7（16 字节，时间有序，跨端可生成且不冲突）。
- 记录分为两层：**明文 header**（版本、标志、修订号、时间戳、`key_id`）与**加密载荷**（序列化后的 JSON）。
- `rev` 单调递增；删除写墓碑（`deleted` 标志）而非物理删除，为多端同步保留删除事实。
- 名称约束：1–64 字节，字符集 `[a-z0-9._-]`，不得以 `.` 开头，不得为空。
- 记录体默认上限 16 MiB，可配置；超限返回错误，不截断。
- 存储层不感知业务类型；`T` 只存在于 API 层，通过 serde 编解码。

## 加密与密钥

密钥层级：

```
口令 ──Argon2id(salt, 参数)──▶ KEK
KEK ──XChaCha20-Poly1305 解包──▶ DK（32 字节随机数据密钥）
DK ──HKDF-SHA256(info 区分用途)──▶ 记录密钥 / 元数据密钥 / 校验密钥
记录密钥 ──XChaCha20-Poly1305(random 24B nonce, AAD)──▶ 记录密文
```

- **口令不落盘**；落盘的是 Argon2id 参数、盐、被 KEK 包裹的 DK，以及校验值。
- **记录级 AEAD**：每条记录独立 nonce（24 字节随机），AAD 绑定命名空间、集合、记录 ID 与明文 header。
- AAD 使记录无法在命名空间/集合/ID 之间搬运，header 篡改会导致解密失败。
- 解锁失败统一返回 `InvalidKey`，不区分“口令错误”和“数据损坏”，避免口令预言机。
- **改口令**只重新包裹 DK，代价 O(1)；**轮换数据密钥**生成新 `key_id`、重加密全部记录，旧密钥在迁移完成后删除。
- 所有密钥材料用 `zeroize` 在离开作用域时清零；错误信息、日志、panic 信息不得包含密钥或明文。
- Argon2id 默认参数：`m=65536 KiB, t=3, p=1`，盐 16 字节；参数随库存储，允许未来升级与按设备下调。

完整字节布局与 AAD 编码见 `.design/data.md`；API 契约见 `.design/api.md`。

## 存储布局

- 单文件 redb，默认文件名 `shio.redb`，位于应用私有目录。
- 四张表：`meta`（格式版本与当前密钥）、`keys`（按 `key_id` 存放被包裹的 DK）、`catalog`（命名空间/集合清单）、`records`（复合键 → 记录 blob）。
- 复合键采用**长度前缀编码**，不依赖分隔符，避免 `a/b` 与 `a`+`/b` 之类的歧义。
- 记录值 = 明文 header + nonce + 密文；列表操作只读 header，不解密。
- 每次公开写操作是一个 redb 写事务，天然原子；批量写在同一事务内提交。
- 格式版本 `format=1`；未知主版本拒绝打开，不猜测、不降级。

## API 风格

- 对外 async（tokio），内核同步；阻塞调用统一走 `spawn_blocking`，不占用 executor 线程。
- `Kernel` 是廉价可克隆句柄（内部 `Arc`），`Send + Sync`；关闭后所有操作返回 `Closed`。
- `Namespace` 与 `Collection<T>` 是从 `Kernel` 派生的轻量句柄，不复制连接。
- 核心操作：`put`（upsert）、`insert`（自动生成 ID）、`get`、`delete`（墓碑）、`list`（游标分页）、`batch`（多记录原子）。
- 事务边界：v0.1 以 `batch` 作为唯一原子写单元，不暴露长生命周期事务对象，避免 async 生命周期复杂化。
- 可恢复错误一律返回 `Result`，不以 panic 表达业务失败。
- 公开签名与错误枚举见 `.design/api.md`。

## 稳定性与错误处理

内核稳定性是硬约束，不是优化项：

- 所有可恢复失败通过 `Result` 返回，不用 panic/abort 表达业务失败。
- 非测试代码禁止 `unwrap` / `expect` / `panic!`；仅当不变式已由类型或前置校验保证、失败即代码缺陷时，才允许 `debug_assert` 后返回 `Internal`。
- 落盘数据一律视为不可信：解析 header、键、长度、AAD、JSON 前先校验；损坏或恶意输入只返回 `Corrupted` / `Deserialize`，不得越界或崩溃。
- 长度与偏移计算使用 `checked_*`，防止整数溢出；不靠 unsafe 做性能捷径。
- 内存分配失败、文件锁冲突、磁盘只读等环境错误都返回错误，不 abort。
- FFI 边界（未来）必须用 `catch_unwind` 把 panic 转为错误码，绝不让 panic 跨 C ABI 传播。
- 测试覆盖截断值、随机字节、错误口令、并发关闭与磁盘只读；关键编解码做 fuzz/property 测试。

## 平台与互操作

- **Rust 应用**：直接依赖 `shio`。
- **C++/Vulkan GUI 与 Android**：经 `shio-ffi` 的 C ABI 调用；句柄不透明，字节串带长度，错误用显式错误码 + 线程局部错误详情。
- **Android**：数据库放应用私有目录；不依赖系统钥匙串，口令解锁因此在无头/服务环境同样可用。
- **服务端**：同一存储格式，仅改变打开路径、并发部署与备份策略，不引入第二套内核。
- **稳定性与 unsafe**：内核的首要要求是不出现未定义行为、不因内部错误崩溃。`shio-core` / `shio` 保持 `forbid(unsafe_code)`；C ABI 若未来引入 unsafe，必须隔离在最小边界、逐条审计并证明无 UB，且经用户明确批准后才实施。FFI 当前仅停留在设计。
- 未来插件系统通过“每插件独立命名空间 + 能力白名单”接入，不改变存储格式；本次只预留命名空间隔离。

## 同步预留

多端同步不在本次范围，但当前模型必须不挡路：

- UUIDv7 提供跨端无协调的全局唯一 ID。
- `rev` + `updated_at` 提供记录级因果比较的起点。
- 墓碑保留删除事实，避免“删除后又被旧副本复活”。
- 记录格式中的 `key_id` 与 `format_version` 支持密钥轮换与格式迁移。
- 后续 `shio-sync` 以**追加变更日志**的方式接入，记录格式不因同步而改写；变更日志表在同步设计时新增。
- 多端共享同一 DK 的密钥分发（设备配对、密钥包裹）留到同步阶段单独决策。

## 范围边界

本次交付：设计文档（本文件、`api.md`、`data.md`）与后续实现所需的任务拆解。

明确不做：

- 插件运行时、权限模型、插件间通信
- 同步协议、变更日志、冲突解决、设备配对
- 全文检索、二级索引、压缩
- GUI/渲染、剪贴板监听、Markdown 渲染等应用功能
- 多写进程、网络服务端部署
- FFI 的实际实现（受 unsafe 约束，先决策后动工）

## 约束

- 稳定性优先：内核不得因未定义行为、越界、未处理错误或 panic 而崩溃；可恢复失败一律以 `Result` 返回。
- 非测试代码禁用 `unwrap` / `expect` / `panic!`；所有落盘输入视为不可信，先校验后使用。
- `shio-core` 与 `shio` 禁止 unsafe；FFI 若引入 unsafe，必须隔离、审计并证明无 UB，且需用户批准。
- 密钥、口令、明文不得出现在日志、错误信息或 panic 信息中。
- 单写多读；每次公开写操作必须原子提交。
- `format=1` 的记录布局与 AAD 是兼容性契约；变更需提升版本并提供迁移。
- 不引入服务端或网络依赖；内核可离线运行。

## 影响与兼容性

- 这是首个能力，不涉及既有数据迁移；`format=1` 是初始格式。
- `format` 与 `data_version` 字段从第一天存在，后续演进按“新增字段 + 提升版本 + 显式迁移”处理。
- API 在 1.0 前允许调整，但任何影响记录格式或密钥层级的变化都必须追加 `DECISIONS.log`。
