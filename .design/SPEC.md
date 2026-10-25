# Spec

<!-- 当前设计真相源：一个概念一节，只写现状。历史见 git，取舍理由见 DECISIONS.log。 -->

## 技术栈
- 语言：Rust（edition 2021，MSRV 1.90；工具链由 `rust-toolchain.toml` 固定为 1.95.0，带 `rustfmt` 与 `clippy`）
- 工作区：纯 workspace（无根包），成员为 `crates/*` 与 `tests`；依赖集中在根 `workspace.dependencies`，版本锁定于 `Cargo.lock`
- 存储引擎：redb 4.4（单文件、嵌入式、MVCC、单写多读）
- 序列化：serde 1 + serde_json 1（记录体编码，落盘前整体加密）
- 异步：tokio 1.54（仅外层 facade；core 保持同步）
- 加密：Argon2id（口令派生，argon2 0.6）、HKDF-SHA256（子密钥，hkdf 0.13 + sha2 0.11）、XChaCha20-Poly1305（AEAD，chacha20poly1305 0.11）
- 错误：thiserror 2；标识：UUIDv7（uuid 1.28）；随机数：getrandom 0.4；密钥清零：zeroize 1.9；口令封装：secrecy 0.10
- 稳定性：`shio-core`、`shio` 与 `shio-sync` 使用 `#![forbid(unsafe_code)]`；`shio-ffi` 经用户批准，crate 级 `deny(unsafe_code)`，仅 `ffi` 边界模块局部允许；可恢复失败只返回错误，不以 panic 表达

## 目标
- 可嵌入的加密持久化内核：命名空间 → 集合 → 记录，单文件 redb，记录级 AEAD。
- 内核同步、facade async；两者禁止 unsafe，可恢复失败一律以 `Result` 返回。
- 当前形态：core 与 facade 均已可用——Argon2id/HKDF/AEAD、redb 表、同步 CRUD、异步句柄、改口令、数据密钥轮换、真正的只读打开；多端同步（format 2、设备配对、变更日志、快照、轮换）已可用；`format=2` 迁移支持批次大小、批次间暂停、旁路进度与批次边界取消；C ABI v1 已可用，覆盖 open/close、CRUD、batch、改口令、轮换、迁移进度/取消、stats、flush 与 P4 同步/配对面。包版本为 `0.3.0`。
- 记录格式 `format=1` 从首个实现起保持向后兼容；同步库使用不可回退的 `format=2`；API 在 1.0 前允许调整。

## 约束
- 稳定性优先：内核不得因未定义行为、越界、未处理错误或 panic 而崩溃；可恢复失败一律以 `Result` 返回。
- 非测试代码禁用 `unwrap` / `expect` / `panic!`；所有落盘输入视为不可信，先校验后使用。
- 静态护栏由 workspace lints 机械执行：`unsafe_code = "forbid"`（`shio-ffi` 为已批准的受控例外，见下），clippy 的 `unwrap_used` / `expect_used` / `panic` / 占位宏 / `dbg_macro` 均为 deny；测试代码局部放宽。
- `shio-core`、`shio` 与 `shio-sync` 禁止 unsafe。`shio-ffi` 是唯一例外：经用户明确批准，crate 级改为 `deny(unsafe_code)`，只有 `ffi` 模块局部允许，范围限裸指针校验、句柄转换、缓冲区所有权和 `catch_unwind`。
- 密钥、口令、明文不得出现在日志、错误信息或 panic 信息中。
- 单写多读；每次公开写操作必须原子提交。
- `format=1` 与 `format=2` 的记录布局与 AAD 都是兼容性契约；变更需提升版本并提供迁移。
- 不引入服务端或网络依赖；内核可离线运行；`shio-sync` 只生产/消费字节帧，不持有连接或后台线程。

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
│   │   │   ├── model.rs        # 名称校验、RecordId、KeyId、DeviceId、Timestamp、meta/分页类型
│   │   │   ├── codec.rs        # format 1 复合键、header、RecordBlob 视图、AAD
│   │   │   ├── codec2.rs       # format 2 记录 header/blob 与 16 字节 key_id AAD
│   │   │   ├── sync.rs         # 同步公开类型、限额与配对指纹
│   │   │   ├── crypto/         # kdf.rs / aead.rs / keys.rs：Argon2id、HKDF、AEAD 与密钥类型
│   │   │   └── store/          # meta/meta2、keys/keys2、catalog、records、changes、sync_state、engine
│   │   └── tests/{engine,sync}.rs
│   ├── shio/                   # async facade（forbid unsafe）
│   │   ├── src/{lib,kernel,collection}.rs
│   │   ├── examples/{notes,sync}.rs
│   │   └── tests/{facade,sync}.rs
│   ├── shio-ffi/               # C ABI v1：include/shio.h，唯一受控 unsafe 边界
│   └── shio-sync/              # SHY2 帧、配对文本与拥有型同步句柄
│       ├── src/{lib,frame,base32}.rs
│       └── tests/frames.rs
└── tests/                      # shio-tests：跨 crate 集成测试包
    ├── src/lib.rs
    └── smoke.rs
```

- `shio-core`：无 async、无 tokio；持有 redb 与密钥材料，所有操作为同步函数。`model`/`error`/`codec`/`codec2` 完整可用；`crypto` 提供 KDF、子密钥派生与 AEAD；`store::Engine` 提供创建/打开、泛型读写、`*_raw`、同步日志/快照/轮换/配对接口。
- `shio`：把 core 的阻塞调用经 `spawn_blocking` 暴露为 async；管理句柄生命周期、在途操作与关闭等待；写路径在调用线程序列化，读路径在阻塞线程反序列化；`examples/notes.rs` 与 `examples/sync.rs` 给出最小可运行用法；`sync` feature 暴露 `Kernel::sync()` 与配对。
- `shio-ffi`：仅为 C++/Vulkan GUI 与 Android 提供阻塞 C ABI，不承载业务逻辑；导出 `shio_open`/`shio_close`、CRUD、batch、改口令、轮换、迁移进度/取消、stats、flush、ABI 版本与错误详情。每个句柄持有 tokio multi-thread runtime；`ffi` 模块是唯一 unsafe 边界。
- `shio-sync`：只依赖 `shio-core`，不引入网络/TLS/后台线程；负责 `SHY2` 帧、Crockford Base32 配对 secret 与拥有型 `SyncHandle`。
- `tests`：`autotests = false` 的成员包，通过显式 `[[test]]` 承载跨 crate 用例。

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
- **多密钥会话**：打开时扫描 `keys2`（format 2）或 `keys`（format 1/升级期），用当前行的盐与参数派生 KEK，解包全部密钥行；记录按自身 header 的 `key_id` 选择记录密钥解密。这样轮换中途崩溃后，meta 仍指向旧密钥时已迁移记录也能读。
- **format 2**：`key_id` 为 16 字节随机值；记录 header 与 AAD 使用 v2 布局，`shio.keys2` 每行独立 nonce。format 1 的 u16 `key_id` 只在升级期与旧库中保留。
- **改口令**只重新包裹 DK：生成新盐与新 KEK，用新 nonce 重写密钥行；format 2 在同一事务内重包裹所有 `keys2` 行，format 1 仍要求单行。存在升级/轮换工作状态时返回 `Busy`。
- **format 1 轮换**：`key_id` 递增（超上限拒绝），先写新 key 行，再按 256 条一批重加密记录，最后更新 meta 并删除旧行。
- **format 2 轮换**：任意设备可 `begin_rotation`，生成随机 `to` 并发布 `kind=1` offer 与 `kind=3` 公告；其他设备安装 offer 后回 ack。写 key 按 `(created_at, key_id)` 收敛，并发轮换形成 key DAG。`migrate_all(to)` 分批重加密，`migrate_all_with_options` 额外支持 `batch_size`、批次间暂停、旁路进度与批次边界取消；迁移期间仍持有 session 写锁，`retire(from)` 需所有已知设备公告迁移完成。
- **设备配对**：`SHB2` 明文携带整条密钥链（≤32），用 32 字节 pairing secret 经 Argon2id 派生的密钥加密为 `SHW2`；secret 以 Crockford Base32 文本进入二维码，指纹为外层 SHA-256 前 3 字节。目标设备得到同一组 DK 与新 device id，直接是 format 2。
- 会话保留 KEK 与 DK 供改口令/轮换/配对使用；所有密钥材料用 `zeroize` 在离开作用域时清零；错误信息、日志、panic 信息不得包含密钥或明文。
- Argon2id 默认参数：`m=65536 KiB, t=3, p=1`，盐 16 字节；参数随库存储，允许未来升级与按设备下调（包括测试用的小参数）。
- 只读打开同样需要口令：解包与 key check 不变，只是句柄不允许写。

完整字节布局与 AAD 编码见 `.design/data.md`；API 契约见 `.design/api.md`。

## 存储布局
- 单文件 redb，默认文件名 `shio.redb`，位于应用私有目录。
- format 1 四张表：`meta`（格式版本与当前密钥）、`keys`（按 u16 `key_id` 存放被包裹的 DK）、`catalog`（命名空间/集合清单）、`records`（复合键 → 记录 blob）。
- format 2 在保留 `catalog`/`records` 的同时新增 `meta2`、`keys2`（16 字节 `key_id`）、`changes`（本地变更日志）与 `sync`（db/device/游标/轮换状态）。
- 创建新库仍写 format 1；`enable_sync` 在单事务内建立 format 2 meta/keys/日志并切到不可回退的 format 2，再分批把 `SHR1` 迁成 `SHR2`。
- 复合键采用**长度前缀编码**，不依赖分隔符，避免 `a/b` 与 `a`+`/b` 之类的歧义；catalog 键与记录键共享同一前缀布局。
- 记录值 = 明文 header + nonce + 密文；format 2 header 为 76 字节、`key_id` 16 字节；列表操作只读 header，不解密。
- 每次公开写操作是一个 redb 写事务，天然原子；批量写在同一事务内提交，任一操作失败则整批回滚；同步模式把记录与 `kind=0` 日志写在同一事务。
- `batch` 中删除不存在的记录返回 `NotFound` 并回滚，使返回的元信息与输入一一对应；单条 `delete` 对不存在的记录返回 `false`，不产生墓碑。
- 未知主版本拒绝打开，不猜测、不降级；旧二进制在 meta 阶段拒绝 format 2。

## API 风格
- 对外 async（tokio），内核同步；阻塞调用统一走 `spawn_blocking`，不占用 executor 线程。
- core 的同步入口是 `store::Engine`：`Engine::open(EngineOptions, password)`，选项含路径、是否创建、是否只读、明文上限、`app_id`、建库 KDF 参数与 redb 页缓存。
- `Engine` 提供 `insert`/`put`/`get`/`delete`/`purge`/`list`/`batch`/`collections`/`stats`/`flush`、密钥生命周期的 `change_password`/`rotate_data_key`，以及收发已序列化明文的 `insert_raw`/`put_raw`/`get_raw`/`batch_raw`；同步面提供 `enable_sync`/`sync_info`/`changes_since`/`apply_page`/快照/轮换/配对，以及 `migrate_all_with_options`/`migration_progress`/`cancel_migration`；存储维护 S2 提供 `backup`/`restore`；`Send + Sync`。
- facade 的 `Kernel::open(OpenOptions, Credential)` 完成参数校验、口令派生与解锁；`Credential::password` 接收 `Into<SecretString>`，`Debug` 输出脱敏。
- `Kernel` 是廉价可克隆句柄（内部 `Arc`），`Send + Sync`；提供 `namespace`、`flush`、`close`、`stats`、`change_password`、`rotate_data_key`、`backup`/`restore`；`sync` feature 下提供 `sync_info`、`sync()` 与 `create_pairing_offer`。`close` 标记关闭并等待在途操作结束，之后新操作返回 `Closed`，重复调用返回 `Ok(())`。
- `shio-sync` 的 `Sync<'a>` 借用 `Engine`，`SyncHandle` 持有 `Arc<Engine>`；二者负责 `SHY2` 编解码、快照迭代与配对文本，不自带传输层。`shio::open_with_pairing` 只创建新库，secret 用 `Zeroizing<String>` 搬运。
- `shio::version()` 返回编译期内核版本（与包版本一致），不需要打开数据库；库的数据格式版本从 `Kernel::stats().format_version` 读取。
- `Namespace` 与 `Collection<T>` 是从 `Kernel` 派生的轻量句柄，不复制连接；名称在构造时校验，不访问磁盘。`Namespace` 提供 `collections`，`Collection<T>` 提供 `insert/put/get/delete/purge/list/batch`。
- 线程边界：写路径在调用线程把 `T` 序列化为 `Zeroizing<Vec<u8>>`，再由阻塞线程加密落盘；读路径在阻塞线程解密并反序列化。`T` 只需 `Serialize + DeserializeOwned + Send + Sync + 'static`，不要求 `Clone`。
- `read_only=true` 时 core 的写入口直接返回 `ReadOnly`，底层以 redb 只读库打开、不持有写锁；facade 保留进入阻塞线程前的快速闸门，`create` 与 `read_only` 组合非法。
- `change_password` 与 `rotate_data_key` 都是写操作，经 `spawn_blocking` 执行；只读句柄返回 `ReadOnly`。
- 事务边界：v0.1 以 `batch` 作为唯一原子业务写单元，不暴露长生命周期事务对象；密钥轮换按批提交，但通过多密钥会话保证任意中断点都可读。
- 备份边界：在线备份在一个 redb 读事务中复制全部已知表，不解密、不重加密；目标必须不存在，先在目标同目录写临时文件，校验表行数与 `meta` 原始字节后原子改名。恢复目标必须不存在或为 0 字节；先复制到临时文件，用原口令完成 key check/结构校验，再替换空目标。失败清理临时文件，不留下半成品。
- 可恢复错误一律返回 `Result`，不以 panic 表达业务失败；公开签名与错误枚举见 `.design/api.md`。

## 稳定性与错误处理
内核稳定性是硬约束，不是优化项：

- 所有可恢复失败通过 `Result` 返回，不用 panic/abort 表达业务失败。
- 非测试代码禁止 `unwrap` / `expect` / `panic!`；仅当不变式已由类型或前置校验保证、失败即代码缺陷时，才允许 `debug_assert` 后返回 `Internal`。
- 落盘数据一律视为不可信：解析 header、键、长度、AAD、JSON 前先校验；损坏或恶意输入只返回 `Corrupted` / `Deserialize`，不得越界或崩溃。
- 由于所有写操作都会读取并复用已有 header，写入路径同样执行完整解码与校验。
- 长度与偏移计算使用 `checked_*`，防止整数溢出；不靠 unsafe 做性能捷径。
- 内存分配失败、文件锁冲突、磁盘只读等环境错误都返回错误，不 abort；redb 的 `DatabaseAlreadyOpen` 映射为 `Locked`，Android 旁路 `.lock` 的冲突同样映射为 `Locked`，其余存储错误归入 `Database`。
- DK 解包与 key check 失败映射为 `InvalidKey`；记录结构或认证失败映射为 `Corrupted`；库文件缺失映射为 `NotFound`；轮换/升级未完成导致的工作状态冲突映射为 `Busy`；只读句柄的写操作映射为 `ReadOnly`；迁移在批次边界取消映射为 `Cancelled`（C ABI 为 `SHIO_CANCELLED`）。备份/恢复的目标已存在或非空映射为 `InvalidArgument`，错误口令仍映射为 `InvalidKey`。
- 同步错误：未启用同步返回 `SyncDisabled`，游标被压缩返回 `SyncResyncRequired`，缺记录 key 返回 `RotationRequired`，同步模式物理删除返回 `SyncEnabled`，配对过期/失败返回 `PairingExpired`/`PairingInvalid`。
- 密钥轮换/升级的每个中断点都保持可读：新 key 行与升级状态先落盘，记录按自身 `key_id` 解密，meta 最后切换；重跑轮换/升级可复用已写入状态并从中断处继续。
- facade 不把 panic 带过线程边界：`spawn_blocking` 的 `JoinError` 统一转为 `Internal`；句柄状态锁与密钥会话锁中毒同样返回 `Internal`。
- 关闭语义：取引擎时登记在途计数，`close` 置空引擎并等待计数归零；已开始的操作持有 `Arc<Engine>` 完成，之后数据库才释放文件锁。
- FFI 边界用 `catch_unwind` 把 panic 转为 `SHIO_INTERNAL`，绝不让 panic 跨 C ABI 传播；错误详情线程局部，返回缓冲区由 `shio_free` 释放并清零。
- 测试覆盖截断值、随机字节、错误口令、并发关闭与磁盘只读；关键编解码做 fuzz/property 测试。当前已覆盖错误口令、截断/篡改的 blob 与记录、分页、批量回滚、句柄关闭、只读拒写、改口令、全量轮换与轮换中断后重跑，以及同步升级、双端 LWW/墓碑、快照/压缩、轮换/ack/退役、配对密钥链、帧截断/超限/`db_id` 不匹配、迁移进度/取消/续跑、格式 1/2 在线备份与恢复、在线写入期间备份、错误口令/损坏备份/目标已存在/非空目标与同步身份保持；Android x86_64 C ABI 覆盖双库同步、只读并发锁、轮换中断续跑、迁移取消/续跑与备份恢复。
- 静态护栏：workspace lints 拒绝 `unsafe`、`unwrap` / `expect` / `panic!` 与未实现的占位宏，新增 crate 默认继承。
- 编解码用例必须覆盖损坏输入：魔数不符、未知格式版本、`key_id` 越界、截断、`ct_len` 与总长不符、尾随字节。

## 平台与互操作
- **Rust 应用**：直接依赖 `shio`。
- **C++/Vulkan GUI 与 Android**：经 `shio-ffi` 的 C ABI 调用；句柄不透明，字节串带长度，错误用显式错误码 + 线程局部错误详情。JNI/Kotlin 包装层由 `shio-android` 维护，内核不绑定 Java 包名。
- **Android**：数据库放应用私有目录；不依赖系统钥匙串，口令解锁因此在无头/服务环境同样可用。应用 minSdk 为 29；首版目标为 `aarch64-linux-android` 真机与 `x86_64-linux-android` 模拟器，NDK r29。内核 `libshio_ffi.so` 仍按 API 24 构建基线链接，`.note.android.ident` 为 API 24，可在 minSdk 29 上加载；`LOAD` 对齐为 16 KB。x86_64 模拟器已通过 C ABI 冒烟：CRUD、batch、关闭/重开、改口令、轮换、只读、同进程与跨进程文件锁、杀进程后重开、双库同步/配对/快照、迁移进度/取消/续跑、轮换中断续跑、备份恢复与独立进程只读；`oxygen` 已在 vivo V2359A（arm64 / Android 16）完成 JNI、APK 打包与杀进程重开验收。redb 4.4 的字节范围锁在 Android 上返回 `Unsupported`，facade 因此在该平台使用旁路 `<db>.lock` 文件与 `rustix::fs::flock`：写句柄独占、只读句柄共享；冲突映射为 `Locked`。默认 KDF 保持 `64 MiB、t=3`，低端机不纳入首版验证（决策 #41）。
- **服务端**：同一存储格式，仅改变打开路径、并发部署与备份策略，不引入第二套内核。
- **稳定性与 unsafe**：内核的首要要求是不出现未定义行为、不因内部错误崩溃。`shio-core` / `shio` / `shio-sync` 保持 `forbid(unsafe_code)`；`shio-ffi` 的 unsafe 已经用户明确批准，仅限 C ABI 边界模块，逐条以 SAFETY 注释审计。C ABI v1 已实现并通过 host 冒烟、Android 两 target 编译。
- 未来插件系统通过“每插件独立命名空间 + 能力白名单”接入，不改变存储格式；本次只预留命名空间隔离。

## 多端同步
- 逻辑库有随机 16 字节 `db_id`，每设备有独立 `device_id`；UUIDv7 提供跨端无协调的记录 ID。
- 本地写在同一事务内追加 `shio.changes` 的 `kind=0` 日志；`changes_since` 按本地序号拉页，`apply_page` 整页原子应用并推进 `peer/<sender>`。
- 冲突按 LWW `(updated_at, rev, RecordBlob 字节序)` 收敛；墓碑保留删除事实；`migrate_all` 重加密时推进 `updated_at`，避免旧 key 的同一逻辑版本翻回。
- 新设备用配对包拿到整条密钥链并标记基线完成，记录通过已有设备的 `snapshot_start/page` 重建；日志被压缩时同样走快照，再从 `at_seq+1` 续拉；`compact_changes` 只手动调用。
- 任意设备可发起随机 `key_id` 轮换；offer/ack/`kind=3` 公告形成 key DAG，写 key 按 `(created_at, key_id)` 收敛；`retire` 需所有已知设备公告迁移完成。
- 传输层不在内核：`shio-sync` 只定义 `SHY2` 帧与 Crockford Base32 配对文本；网络、TLS、二维码与重试由应用负责。
- 同步库 `format=2` 不支持回退；升级完成后旧二进制在 meta 阶段拒绝打开。

## 范围边界
当前范围：加密持久化内核（命名空间/集合/记录、加密、事务）与多端同步字节层（变更日志、快照、轮换、配对、`SHY2` 帧）；详细契约见 `api.md` 与 `data.md`。

明确不做：

- 插件运行时、权限模型、插件间通信
- 网络传输、TLS、后台同步线程与服务端部署；这些由应用在 `shio-sync` 之上实现
- 全文检索、二级索引、压缩
- GUI/渲染、剪贴板监听、Markdown 渲染等应用功能
- 多写进程

## 影响与兼容性
- format 1 的 AAD 与 blob 布局不变；不启用同步的已有数据库无需迁移。
- `enable_sync` 把库升级为 format 2 后不可回退；旧二进制在 meta 阶段拒绝打开。升级按记录批提交，可中断续跑。
- `change_password` 在 format 2 下重包裹全部 `keys2` 行；`rotate_data_key` 在同步模式下等价于 `begin_rotation` + `migrate_all`，不自动退役。
- `read_only` 真正以 redb 只读库打开，不再持有写锁，可用于多进程只读并发。
- 轮换/升级中途崩溃会留下工作状态：库仍可读，但相关写操作返回 `Busy`；重跑或继续升级即可完成。
- `sync` feature 是可选依赖：关闭时 `shio` 的默认 API 与依赖不变；开启时新增 `Kernel::sync()`、`Kernel::create_pairing_offer` 与 `shio::open_with_pairing`。
- 新增 `shio::version()` 与 `shio_core::VERSION`，纯增量，不影响已有签名、记录格式与密钥层级。
- API 在 1.0 前允许调整，但任何影响记录格式或密钥层级的变化都必须追加 `DECISIONS.log`。

## 构建与验证
- 工具链由 `rust-toolchain.toml` 固定（1.95.0），MSRV 1.90（redb 4.4 的要求）；低于 MSRV 时 cargo 直接拒绝。
- `cargo build --workspace` 构建全部 crate，产物落 `target/`（不入库）。
- `cargo run -p shio --example notes` 与 `cargo run -p shio --features sync --example sync` 运行最小示例（临时库，结束即删除）。
- `cargo test --workspace`：当前 101 个用例（core 库内 49、引擎端到端 21、core 同步 8、facade 库内 4、facade 端到端 5、`shio-ffi` 库内 1 + C ABI 端到端 3、`shio-sync` 库内 3、帧端到端 4、跨 crate 集成 3）。
- `cargo test -p shio --features sync` 额外跑 facade 的配对/同步端到端用例。
- 引擎端到端使用 `tempfile` 与 8 MiB 的 Argon2id 参数，含 300 条记录的跨批轮换与「篡改记录 → 轮换中断 → 还原 → 续跑」；facade 端到端用默认参数（64 MiB、t=3）。
- 版本查询断言常量与 `CARGO_PKG_VERSION` 一致，且不依赖打开数据库。
- Android 类型检查：`cargo check -p shio-core -p shio -p shio-ffi --target aarch64-linux-android` 与 `x86_64-linux-android`；链接 `.so` 时把 `CARGO_TARGET_<TARGET>_LINKER` 指向 NDK r29 的 `<triple>24-clang.cmd`，并设置 `RUSTFLAGS="-C link-arg=--target=<triple>24"`（内核 `.so` 的 API 24 构建基线；应用 minSdk 为 29）。
- `cargo clippy --workspace --all-targets -- -D warnings`、`cargo clippy -p shio --features sync --all-targets -- -D warnings` 与 `cargo fmt --all -- --check` 是合并前的必过项。
- 依赖版本由 `Cargo.lock` 锁定并随仓库提交；升级依赖需重新跑上述三项检查。

## 背景
自动日志裁剪的「已知对端」是 `cur/` 与 `heard/` 的并集（排除本机）；只要其中一个设备没有
上报 `heard/`，`compact_changes_auto` 就返回 0。轮换退役与 offer 清理也依赖「所有已知设备
都有 `ack/` / `mig/`」。

已经不再同步的设备（换机、卸载、退场）不会自己消失：

- `cur/<device>`、`heard/<device>` 永久占住裁剪门控；
- `ack/<to>/<device>`、`mig/<key>/<device>` 让 `retire_key` / `prune_rotation_offers` 一直等待；
- `peer/<device>` 保留着一个可能早已过期、甚至低于 `log_start` 的游标。

决策 #57 明确记录了这一点（「当前没有解绑 API，需要时可另开变更」）。本次补上显式解绑。

## 本阶段范围
- `Engine::forget_device(device)`：在一个写事务里删除该设备的全部设备级同步状态。
- 拒绝解绑本机；对未知设备幂等（返回全零报告）。
- 不影响记录、密钥、`rot/`、`upgrade`、变更日志。
- 解绑后自动裁剪与轮换退役不再等待该设备。
- facade + C ABI + Android harness。

不在本次范围：设备黑名单/撤销、强制删除该设备写入的记录、解绑时的网络通知、自动判断
「设备已退场」。

## 语义
删除的键（全部幂等）：

| 键 | 含义 |
|---|---|
| `peer/<device>` | 本机在该设备日志中的消费游标 |
| `cur/<device>` | 该设备公告的当前写 key |
| `heard/<device>` | 该设备上报的「已读到本机日志哪一条」 |
| `ack/<to>/<device>` | 该设备对某个轮换 key 的确认（扫描全部 `to`） |
| `mig/<key>/<device>` | 该设备已完成某个 key 的迁移（扫描全部 `key`） |

保留：记录与墓碑、`shio.keys2`、`meta`、`rot/`、`ack/*/<其它设备>`、`mig/*/<其它设备>`、
`upgrade`、`shio.changes`。

错误与顺序：

- 只读句柄 → `ReadOnly`；维护窗口 → `Busy`；未启用同步 → `SyncDisabled`；
- `device == 本机` → `InvalidArgument`（不能把自己从自己的裁剪门控里删掉）；
- 未知设备 → 成功，报告全为 `false` / 0（便于重试）。

## 为什么保留 key 相关状态
`rot/` 是仍未剪掉的轮换 offer，`ack/` 是「某设备已安装某个 key」，`mig/` 是
「某设备已无某个 key 的记录」。这些描述的是**库与 key 的事实**，不是本设备的同步进度：

- 清掉 `rot/` 后，缺新 key 的对端会收到 `RotationRequired`，却无法再从这里取 offer；
- 清掉 `ack/` 会让 `prune_rotation_offers` 失去依据；
- `mig/` 影响 `retire_key` 的安全判断。

`upgrade`（format 1→2 的数据库级迁移游标）同理保留。

相反，`cur/` 虽然是「某设备公告的当前 key」，但它同时是 `known_devices` 的来源，
会参与自动日志裁剪的门控。克隆清掉后只剩自己，等对端重新上报，符合「新设备刚开始
同步」的事实。

## 为什么重建完整日志
配对包只携带身份与 key，不携带记录；新设备拿到数据靠拉日志或快照。如果克隆的日志
只有一条 `CurrentKey`，新配对设备拉完日志仍没有记录，会静默得到空库。沿用
`run_baseline` 的全部记录条目可以保证：

- 新设备只靠日志就能拿到全量数据；
- 老设备拉克隆日志时，相同记录按 LWW `(updated_at, rev, blob)` 被判为旧版本而跳过。

记录 blob 的 header 不含 `writer`，日志条目的 `writer` 只表示发布者，因此重基线
不会改变任何冲突结果。

代价是克隆会写 `记录数 + 1` 条日志，大库有一次性 O(记录数) 写入；这与首次
`enable_sync` 的成本相同。

## 阶段 API
```rust
pub struct ForgetReport {
    pub device: DeviceId,
    pub removed_peer_cursor: bool,
    pub removed_current_key: bool,
    pub removed_progress: bool,
    pub removed_acks: u64,
    pub removed_migrations: u64,
}

impl Engine {
    pub fn forget_device(&self, device: DeviceId) -> Result<ForgetReport>;
}

// facade（`sync` feature）
impl Kernel {
    pub async fn forget_device(&self, device: DeviceId) -> Result<ForgetReport>;
}
```

```c
int32_t shio_sync_forget_device(shio_kernel *handle,
                                const uint8_t *device_id,
                                uint8_t **out_json,
                                size_t *out_json_len);
```

摘要 JSON：

```json
{"device":"32hex","peer_cursor":true,"current_key":true,"progress":true,"acks":1,"migrations":2}
```

增量符号，`SHIO_ABI_VERSION` 保持 1。

## 验收
- 三设备（A、B、C）：B 上报进度、C 不上报时 `compact_changes_auto() == 0`；`forget_device(C)`
  后返回报告包含 `current_key`/`progress`，再次裁剪返回非 0。
- 解绑本机返回 `InvalidArgument`；未知设备返回全零报告；连续解绑幂等。
- 轮换场景：B 安装 offer 并 ack 后，A 的 `ack/<to>/B` 存在；`forget_device(B)` 清掉它，
  `removed_acks >= 1`；迁移公告同理清掉 `mig/`。
- 未启用同步返回 `SyncDisabled`；只读返回 `ReadOnly`；维护窗口返回 `Busy`。
- FFI：符号往返、摘要 JSON、错误码；facade：async 包装与 `compact_changes_auto` 联动。
- Android harness：`forget` 模式（A/B/C 三库，裁剪先 0 后非 0，报告字段正确），
  x86_64 模拟器与 arm64 真机；同轮回归既有模式。
- `cargo test --workspace --all-features`、`fmt --check`、`clippy -D warnings`、`cc -fsyntax-only` 通过。
- 文档：`api.md` §4/§8/§9/§12、`data.md` §15.5、`notes.md`（用法与「不是拉黑」的边界）、
  `阶段计划.md`、`STATE.md`、决策 1 条。

## 文件格式 v1：`.shio.jsonl`
UTF-8、逐行 JSON、流式读写。首行 `head`，尾行 `end`，中间只有 `col` 与 `rec`。

```json
{"t":"head","v":1,"fmt":2,"at":1761350400000,"deleted":false}
{"t":"col","ns":"vault","col":"entries","at":1758758400000}
{"t":"rec","ns":"vault","col":"entries","id":"018f3c1e-9f0a-7c3e-8f5d-2b1a0c9d8e7f","rev":3,"created":1758758400000,"updated":1758758500123,"dv":1,"del":false,"v64":"eyJuYW1lIjoi5byg5LiJIn0="}
{"t":"rec","ns":"vault","col":"entries","id":"018f3c1e-9f0a-7c3e-8f5d-2b1a0c9d8e80","rev":4,"created":1758758400000,"updated":1758758600000,"dv":1,"del":true}
{"t":"end","rec":2,"col":1,"sha":"<64 位小写 hex>"}
```

规则：

- `head.v` 当前为 1；`head.fmt` 是源库格式版本（1 或 2），只作来源提示，导入不依赖。
- `head.at` 是导出时间，`head.deleted` 记录本次是否包含墓碑。
- 值二选一、逐行自描述：`v64`（标准 base64，默认）或 `v`（合法 UTF-8 字符串，工具可读模式）。
  非墓碑记录必须且只能有一个；`del=true` 不带值。
- `sha` 覆盖 `head` 之后、`end` 之前的每条体行字节加一个 `\n`（行内容不含换行），因此 CRLF
  文件也能校验通过；`rec`/`col` 是精确行数。允许空行（导入时跳过，不参与计数与哈希）。
- 不写 `db_id`、key、口令、日志、游标或密码学参数；`db_id` 是稳定指纹，写进明文文件会造成
  跨导出关联，默认排除。
- 记录按编码键序（namespace, collection, id）输出，同一库同样参数导出除 `head.at` 外字节级可复现。
- 未知字段忽略（向前兼容），未知 `t`、未知 `head.v` 直接拒绝。

## 导出
```rust
pub struct ExportOptions {
    pub namespace: Option<String>,
    pub collection: Option<String>,
    pub include_deleted: bool,
    pub overwrite: bool,
}
pub struct ExportInfo {
    pub format_version: u16,
    pub records: u64,
    pub deleted: u64,
    pub collections: u64,
    pub bytes: u64,
}
```

- `collection` 必须与 `namespace` 同时给出；否则 `InvalidArgument`。
- 整个导出在单个 redb 读事务里完成（与 `backup` 一致，快照一致；代价是延迟页回收）。
- 先写目标同目录临时文件，成功 `flush` 后原子改名；目标已存在且 `overwrite=false` 返回
  `InvalidArgument`，失败不留半成品。
- 只读句柄可导出（`read_only=true` 也可用）。
- `col` 行来自 `shio.catalog`，空集合也输出，保留原始 `created_at`。
- 墓碑默认不导出（`include_deleted=false`），此时 `ExportInfo.deleted = 0`。

## 导入
```rust
pub enum ImportMode { Merge, Replace }
pub struct ImportOptions { pub mode: ImportMode, pub dry_run: bool, pub batch_size: usize }
pub struct ImportReport {
    pub records: u64, pub applied: u64, pub skipped: u64, pub deleted: u64,
    pub collections: u64, pub bytes: u64, pub dry_run: bool,
}
```

**两遍处理**：第一遍只校验（`head`、逐行 JSON、名称、UUID、`rev`/时间戳、值编码、值长度、
精确计数、SHA-256、`end` 之后无内容），任何问题在写入前失败；第二遍按批导入。
`dry_run=true` 只跑第一遍，报告里的 `applied`/`skipped` 按校验结果推算，不写库。

**Merge（默认，与同步 LWW 对齐）**：

1. 本地不存在 → 写入（墓碑行跳过并计入 `skipped`）。
2. 本地存在且 `(updated_at, rev)` 更大 → 跳过。
3. 相等 → 解密本地比较明文：相同则跳过（**重跑幂等**）；不同则按明文字节序决定，规则固定可复现。
4. 墓碑行且本地存在：按上面的 LWW 判断后写墓碑（保留源 `rev`/`updated_at`），明文沿用本地值；
   相等时墓碑优先。
5. 墓碑行且本地不存在：跳过（新库不需要墓碑）。

**Replace**：本地优先覆盖，但只保证压过本地副本：

- `rev = max(local.rev + 1, src.rev)`（本地不存在时 `max(1, src.rev)`），避免 rev 回退；
- `updated_at = max(src.updated_at, local.updated_at + 1)`（本地不存在时用源值）；
- `created_at` 沿用本地已有值（覆盖不改变首次创建时间）；
- 对端若存在更新的版本，仍按 LWW 胜出，不做“全局强制”。

写入细节：

- 用目标库当前 key 重新加密；format=1 用 v1 记录格式，format=2 用 v2，与 `stage_put` 同路径。
- 保留源 `created_at`/`updated_at`/`rev`/`data_version`/墓碑标志。
- 同步启用时按本机 `device_id` 追加 `shio.changes` 条目，对端能正常拉到；format=1 不写日志。
- 每批（默认 256，上限 4096，0 取默认）一个写事务，批内原子、整体非原子；失败后重跑靠幂等收敛。
- `col` 行只补登记缺失的集合，保留源 `created_at`。
- 普通写入路径：维护窗口返回 `Busy`，只读返回 `ReadOnly`，超 `max_value_size` 返回 `ValueTooLarge`。
- 不校验业务类型（内核不知道值的 schema），只校验名称、ID、长度与格式；应用需自行校验。

## 危险性与边界
- 导出文件是**明文**：文档、CLI、函数文档都要标注；不默认写当前目录，不自动上传。
- 导出不是备份：不含 key 链、`db_id`、日志、游标，不能恢复同步身份；跨设备仍用 `clone_backup` 或配对。
- 导入会在同步库中表现为一次大规模本机写入，日志会持续增长，之后由正常裁剪处理。
- 导入不清理任何既有数据；`Replace` 只覆盖同 ID 记录。

## API 面
```rust
impl Engine {
    pub fn export_records(&self, target: &Path, options: &ExportOptions) -> Result<ExportInfo>;
    pub fn import_records(&self, source: &Path, options: &ImportOptions) -> Result<ImportReport>;
}

// facade
impl Kernel {
    pub async fn export_records(&self, target: PathBuf, options: ExportOptions) -> Result<ExportInfo>;
    pub async fn import_records(&self, source: PathBuf, options: ImportOptions) -> Result<ImportReport>;
}
```

C ABI（增量符号，`SHIO_ABI_VERSION` 保持 1）：

```c
int32_t shio_export(shio_kernel *kernel, const char *target_path,
                    const uint8_t *options_json, size_t options_len,
                    uint8_t **out_json, size_t *out_json_len);
int32_t shio_import(shio_kernel *kernel, const char *source_path,
                    const uint8_t *options_json, size_t options_len,
                    uint8_t **out_json, size_t *out_json_len);
```

- export options：`{"namespace":"…","collection":"…","include_deleted":false,"overwrite":false}`；
  未知字段拒绝（与 `shio_open` 一致）。
- import options：`{"mode":"merge"|"replace","dry_run":false,"batch_size":256}`。
- 报告 JSON：export `{"format_version":N,"records":N,"deleted":N,"collections":N,"bytes":N}`；
  import `{"records":N,"applied":N,"skipped":N,"deleted":N,"collections":N,"bytes":N,"dry_run":bool}`。
- 格式错误映射 `SHIO_CORRUPTED`，参数错误 `SHIO_INVALID_ARGUMENT`，其余沿用既有映射。

## 实现落点
- 类型 `ForgetReport` 放 `shio-core/src/sync.rs`（与 `ApplyReport` 同处），核心根重导出。
- 实现放 `engine_sync.rs`，紧邻 `apply_peer_progress` / `compact_changes_auto`；单个写事务，
  `ack/` 与 `mig/` 按键前缀扫描后按尾部 16 字节匹配设备 ID。
- facade 在 `kernel.rs`（`#[cfg(feature = "sync")]`），C ABI 在 `ffi.rs` + `shio.h`。
- harness 增加 `forget` 模式。

## 命令
```
shio-tool export <db> --out <file> [--namespace N] [--collection C] [--with-deleted] [--force]
shio-tool import <db> <file> [--dry-run] [--replace] [--batch N] [--yes]
shio-tool verify <file>
shio-tool stats  <file>
```

- `export` 以只读方式打开库（`read_only=true`），输出走内核的临时文件 + 原子改名；目标已存在
  且没有 `--force` 时返回错误，不覆盖。
- `import` 以读写方式打开库；`--batch` 映射到 `ImportOptions::batch_size`；`--replace` 映射到
  `ImportMode::Replace`，默认 `Merge`。
- `verify` 输出 `format=<n> records=<n> deleted=<n> collections=<n> bytes=<n>`；失败时退出码 1。
- `stats` 先完整校验，再按 `(namespace, collection)` 汇总记录数与墓碑数。
- 退出码：`0` 成功，`1` 运行失败（错误/校验失败/口令错误），`2` 用法错误。
- `-h/--help` 与 `-V/--version` 顶层可用；内核版本与工具版本一起打印。

## 口令
按优先级：

1. `--password-stdin`：从标准输入读一行，去掉行尾换行；空口令拒绝。
2. `SHIO_PASSWORD` 环境变量。
3. 终端交互：不回显输入（工具级依赖 `rpassword`）。
4. 都没有（stdin 是管道且未加 `--password-stdin`）→ 用法错误，提示可用的两种方式。

口令在进程内用 `Zeroizing<String>` 搬运，读完即清零副本；不写日志、不回显。

## 危险闸门
- 导出：stderr 先打印「导出文件是明文，包含全部记录值；用完请自行清理」，再写文件。
- 导入：没有 `--yes` 时退出码 2，提示先 `--dry-run` 预览；`--dry-run` 不写库。
- 不提供「删除文件」「上传」「自动清理」等隐式行为；工具只做用户明确要求的那一步。

## 新增内核 API
`verify` / `stats` 需要在不打开数据库的情况下校验文件。为此把导出文件扫描从 `Engine` 方法提炼为
free function，并复用同一个校验实现：

```rust
// shio-core::store（shio 根重导出）
pub fn verify_export(source: &Path, max_value_size: usize) -> Result<ExportInfo>;
```

- 返回的 `ExportInfo` 含 `format_version`、`records`、`deleted`、`collections`、`bytes`。
- 校验规则与 `import_records` 第一遍完全一致（结构、名称、UUID、值编码与长度、计数、SHA-256、
  尾后数据）。
- 不引入新类型、新错误码；`max_value_size` 传 `DEFAULT_MAX_VALUE_SIZE` 即可。

## 依赖
- `shio`（默认 feature，不含 `sync`；format=2 的读写由 core 支持，不需要同步 feature）。
- `serde_json`（`stats` 汇总）、`tokio`（复用 facade 的异步入口）、`zeroize`（口令）。
- `rpassword`（仅工具用于不回显输入；不进入库与 Android 构建）。

## 设备回来后
没有黑名单，也不阻止重新配对：该设备的变更页会重新写入 `cur/`，进度帧会重新写入 `heard/`。
因为 `peer/` 已删除，本机会从 0 拉取；如果本机日志已经裁剪过，就会收到
`SyncResyncRequired`，走 `snapshot_start`/`snapshot_page`/`apply_snapshot` 重同步。

也就是说：解绑是「本机不再关心这个对端」的显式声明，不是永久拉黑；重新上线会按新对端
（或需要快照的老对端）处理。文档必须写清这一点，避免被当成远程撤销。
