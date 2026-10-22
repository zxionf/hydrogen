# 20261022-crypto-and-store

<!-- 本文件是本次变更合并进 SPEC 的终稿：小节标题与 `.design/SPEC.md` 的概念节一一对应。 -->

## 目标

- 可嵌入的加密持久化内核：命名空间 → 集合 → 记录，单文件 redb，记录级 AEAD。
- 内核同步、facade async；两者禁止 unsafe，可恢复失败一律以 `Result` 返回。
- 当前形态：workspace、类型契约、纯逻辑层与同步内核已落地；`crypto`（Argon2id/HKDF/AEAD）与 `store`（四张 redb 表、引擎 CRUD）可用，facade 的 async `open`/读写尚未接线。
- 记录格式 `format=1` 从首个实现起保持向后兼容；API 在 1.0 前允许调整。

## 分层与目录结构

依赖方向严格单向，core 不依赖 tokio，保证 Android、服务端与未来守护进程复用同一内核。

```
shio/
├── Cargo.toml                  # 纯 workspace：members = crates/*, tests
├── rust-toolchain.toml         # 固定 1.95.0，要求 rustfmt/clippy
├── rustfmt.toml
├── crates/
│   ├── shio-core/              # 同步内核（forbid unsafe）
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── error.rs        # 错误分类，全量
│   │   │   ├── model.rs        # 名称校验、RecordId、Timestamp、meta/分页类型，全量
│   │   │   ├── codec.rs        # 复合键、34 字节 header、RecordBlob 视图、三类 AAD，全量
│   │   │   ├── crypto/         # kdf.rs / aead.rs / keys.rs：Argon2id、HKDF、AEAD 与密钥类型
│   │   │   └── store/          # meta / keys / catalog / records 编解码与 engine.rs（redb 事务）
│   │   └── tests/engine.rs     # 真实临时文件的端到端用例
│   ├── shio/                   # async facade（forbid unsafe）
│   │   └── src/{lib,kernel,collection}.rs
│   ├── shio-ffi/               # C ABI：文档占位，受 unsafe 约束未实现
│   └── shio-sync/              # 多端同步：文档占位
└── tests/                      # shio-tests：跨 crate 集成测试包
    ├── src/lib.rs
    └── smoke.rs
```

- `shio-core`：无 async、无 tokio；持有 redb 与密钥材料，所有操作为同步函数。`model`/`error`/`codec` 完整可用；`crypto` 提供 KDF、子密钥派生与 AEAD；`store::Engine` 提供创建/打开与记录读写。
- `shio`：把 core 的阻塞调用经 `spawn_blocking` 暴露为 async；当前只提供参数、凭据与句柄类型，名称校验在构造句柄时完成；包装 `Engine` 的方法尚不存在。
- `shio-ffi`：仅为 C++/Vulkan GUI 与 Android 提供 C ABI，不承载业务逻辑；未取得 unsafe 例外前不导出任何符号。
- `shio-sync`：后续接入，只依赖 core 的模型与存储接口，不修改记录格式。
- `tests`：`autotests = false` 的成员包，通过显式 `[[test]]` 承载跨 crate 用例。

## 加密与密钥

密钥层级：

```
口令 ──Argon2id(salt, 参数)──▶ KEK（32 字节）
KEK ──XChaCha20-Poly1305 解包──▶ DK（32 字节随机数据密钥）
DK ──HKDF-SHA256(info 区分用途)──▶ 记录密钥 / 元数据密钥 / 校验密钥
记录密钥 ──XChaCha20-Poly1305(random 24B nonce, AAD)──▶ 记录密文
```

- **口令不落盘**；落盘的是 Argon2id 参数、16 字节盐、被 KEK 包裹的 DK（48 = 32 + 16 tag），以及 key check（32 字节密文，明文固定为 `shio-keycheck-v1`）。
- 子密钥派生：HKDF-SHA256，盐固定 `shio/v1/hkdf-salt`，info 分别为 `shio/v1/record-key`、`shio/v1/meta-key`、`shio/v1/check-key`；三者在用途上隔离。
- **记录级 AEAD**：每条记录独立 nonce（24 字节随机），AAD 绑定命名空间、集合、记录 ID 与明文 header（域分隔符 `shio/v1/record`）。
- AAD 使记录无法在命名空间/集合/ID 之间搬运，header 篡改会导致解密失败；keywrap 与 keycheck 各自使用独立域分隔符。
- 解锁失败统一返回 `InvalidKey`，不区分“口令错误”和“数据损坏”，避免口令预言机。
- **改口令**只重新包裹 DK，代价 O(1)；**轮换数据密钥**生成新 `key_id`、重加密全部记录，旧密钥在迁移完成后删除。两者当前尚未实现。
- 所有密钥材料用 `zeroize` 在离开作用域时清零；错误信息、日志、panic 信息不得包含密钥或明文。
- Argon2id 默认参数：`m=65536 KiB, t=3, p=1`，盐 16 字节；参数随库存储，允许未来升级与按设备下调（包括测试用的小参数）。

完整字节布局与 AAD 编码见 `.design/data.md`；API 契约见 `.design/api.md`。

## 存储布局

- 单文件 redb，默认文件名 `shio.redb`，位于应用私有目录。
- 四张表：`meta`（格式版本与当前密钥）、`keys`（按 `key_id` 存放被包裹的 DK）、`catalog`（命名空间/集合清单）、`records`（复合键 → 记录 blob）。
- 创建库时在单个写事务内写 `meta` 与 `keys`，并把 `records`/`catalog` 一并建表；读路径只读不建表，缺表视为损坏。
- 复合键采用**长度前缀编码**，不依赖分隔符，避免 `a/b` 与 `a`+`/b` 之类的歧义；catalog 键与记录键共享同一前缀布局。
- 记录值 = 明文 header + nonce + 密文；列表操作只读 header，不解密。
- 每次公开写操作是一个 redb 写事务，天然原子；批量写在同一事务内提交，任一操作失败则整批回滚。
- `batch` 中删除不存在的记录返回 `NotFound` 并回滚，使返回的元信息与输入一一对应；单条 `delete` 对不存在的记录返回 `false`，不产生墓碑。
- 格式版本 `format=1`；未知主版本拒绝打开，不猜测、不降级。

## API 风格

- 对外 async（tokio），内核同步；阻塞调用统一走 `spawn_blocking`，不占用 executor 线程。
- core 的同步入口是 `store::Engine`：`Engine::open(EngineOptions, password)`，选项含路径、是否创建、明文上限、`app_id` 与建库 KDF 参数（已存在的库以落盘参数为准）。
- `Engine` 提供 `insert`（自动 UUIDv7）、`put`（指定 ID，upsert）、`get`、`delete`（墓碑）、`purge`（物理删除）、`list`（游标分页）、`batch`、`collections`、`stats` 与 `flush`；`Send + Sync`，可在阻塞线程池中共享。
- `Kernel` 是廉价可克隆句柄（内部 `Arc`），`Send + Sync`；关闭后所有操作返回 `Closed`。facade 尚未包装 `Engine`。
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
- 由于所有写操作都会读取并复用已有 header，写入路径同样执行完整解码与校验。
- 长度与偏移计算使用 `checked_*`，防止整数溢出；不靠 unsafe 做性能捷径。
- 内存分配失败、文件锁冲突、磁盘只读等环境错误都返回错误，不 abort；redb 的 `DatabaseAlreadyOpen` 映射为 `Locked`，其余存储错误归入 `Database`。
- DK 解包与 key check 失败映射为 `InvalidKey`；记录结构或认证失败映射为 `Corrupted`；库文件缺失映射为 `NotFound`。
- FFI 边界（未来）必须用 `catch_unwind` 把 panic 转为错误码，绝不让 panic 跨 C ABI 传播。
- 测试覆盖截断值、随机字节、错误口令、并发关闭与磁盘只读；关键编解码做 fuzz/property 测试。当前已覆盖错误口令、截断/篡改的 blob 与记录、分页与批量回滚。
- 静态护栏：workspace lints 拒绝 `unsafe`、`unwrap` / `expect` / `panic!` 与未实现的占位宏，新增 crate 默认继承。
- 编解码用例必须覆盖损坏输入：魔数不符、未知格式版本、`key_id` 越界、截断、`ct_len` 与总长不符、尾随字节。

## 影响与兼容性

- 本变更首次产生真实的 `format=1` 库文件；此前没有落盘数据，因此不涉及迁移。
- `format` 与 `data_version` 字段从第一天存在，后续演进按“新增字段 + 提升版本 + 显式迁移”处理。
- `change_password`、`rotate_data_key`、`read_only`、facade 包装、FFI 与同步仍未实现；补齐这些能力不改变已写下的记录布局。
- API 在 1.0 前允许调整，但任何影响记录格式或密钥层级的变化都必须追加 `DECISIONS.log`。

## 构建与验证

- 工具链由 `rust-toolchain.toml` 固定（1.95.0），MSRV 1.90（redb 4.4 的要求）；低于 MSRV 时 cargo 直接拒绝。
- `cargo build --workspace` 构建全部 crate，产物落 `target/`（不入库）。
- `cargo test --workspace`：当前 56 个用例（core 库内 34、引擎端到端 16、facade 3、跨 crate 集成 3）。
- 引擎端到端用例使用 `tempfile` 建真实库文件，Argon2id 用小参数（8 MiB、1 轮、1 并行）保持测试在 1 秒内。
- `cargo clippy --workspace --all-targets -- -D warnings` 与 `cargo fmt --all -- --check` 是合并前的必过项。
- 依赖版本由 `Cargo.lock` 锁定并随仓库提交；升级依赖需重新跑上述三项检查。
