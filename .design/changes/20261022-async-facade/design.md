# 20261022-async-facade

<!-- 本文件是本次变更合并进 SPEC 的终稿：小节标题与 `.design/SPEC.md` 的概念节一一对应。 -->

## 目标

- 可嵌入的加密持久化内核：命名空间 → 集合 → 记录，单文件 redb，记录级 AEAD。
- 内核同步、facade async；两者禁止 unsafe，可恢复失败一律以 `Result` 返回。
- 当前形态：core 可用（Argon2id/HKDF/AEAD、四张 redb 表、同步 CRUD），facade 已接线（`open`/读写/`close`）；`change_password`、`rotate_data_key` 与真正的只读打开尚未实现。
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
│   │   ├── src/{lib,kernel,collection}.rs
│   │   └── tests/facade.rs     # 异步端到端用例
│   ├── shio-ffi/               # C ABI：文档占位，受 unsafe 约束未实现
│   └── shio-sync/              # 多端同步：文档占位
└── tests/                      # shio-tests：跨 crate 集成测试包
    ├── src/lib.rs
    └── smoke.rs
```

- `shio-core`：无 async、无 tokio；持有 redb 与密钥材料，所有操作为同步函数。`model`/`error`/`codec` 完整可用；`crypto` 提供 KDF、子密钥派生与 AEAD；`store::Engine` 提供创建/打开、泛型读写与接收已序列化明文的 `*_raw` 接口。
- `shio`：把 core 的阻塞调用经 `spawn_blocking` 暴露为 async；管理句柄生命周期、在途操作与关闭等待；写路径在调用线程序列化，读路径在阻塞线程反序列化。
- `shio-ffi`：仅为 C++/Vulkan GUI 与 Android 提供 C ABI，不承载业务逻辑；未取得 unsafe 例外前不导出任何符号。
- `shio-sync`：后续接入，只依赖 core 的模型与存储接口，不修改记录格式。
- `tests`：`autotests = false` 的成员包，通过显式 `[[test]]` 承载跨 crate 用例。

## API 风格

- 对外 async（tokio），内核同步；阻塞调用统一走 `spawn_blocking`，不占用 executor 线程。
- core 的同步入口是 `store::Engine`：`Engine::open(EngineOptions, password)`，选项含路径、是否创建、明文上限、`app_id`、建库 KDF 参数与 redb 页缓存。
- `Engine` 提供 `insert`/`put`/`get`/`delete`/`purge`/`list`/`batch`/`collections`/`stats`/`flush`，以及收发已序列化明文的 `insert_raw`/`put_raw`/`get_raw`/`batch_raw`；`Send + Sync`。
- facade 的 `Kernel::open(OpenOptions, Credential)` 完成参数校验、口令派生与解锁；`Credential::password` 接收 `Into<SecretString>`，`Debug` 输出脱敏。
- `Kernel` 是廉价可克隆句柄（内部 `Arc`），`Send + Sync`；提供 `namespace`、`flush`、`close`、`stats`。`close` 标记关闭并等待在途操作结束，之后新操作返回 `Closed`，重复调用返回 `Ok(())`。
- `Namespace` 与 `Collection<T>` 是从 `Kernel` 派生的轻量句柄，不复制连接；名称在构造时校验，不访问磁盘。`Namespace` 提供 `collections`，`Collection<T>` 提供 `insert/put/get/delete/purge/list/batch`。
- 线程边界：写路径在调用线程把 `T` 序列化为 `Zeroizing<Vec<u8>>`，再由阻塞线程加密落盘；读路径在阻塞线程解密并反序列化。`T` 只需 `Serialize + DeserializeOwned + Send + Sync + 'static`，不要求 `Clone`。
- `read_only=true` 时写方法在进入阻塞线程前返回 `ReadOnly`；v0.1 底层仍以读写模式打开 redb，真正的只读并发留待后续。
- 事务边界：v0.1 以 `batch` 作为唯一原子写单元，不暴露长生命周期事务对象，避免 async 生命周期复杂化。
- 可恢复错误一律返回 `Result`，不以 panic 表达业务失败；公开签名与错误枚举见 `.design/api.md`。

## 稳定性与错误处理

内核稳定性是硬约束，不是优化项：

- 所有可恢复失败通过 `Result` 返回，不用 panic/abort 表达业务失败。
- 非测试代码禁止 `unwrap` / `expect` / `panic!`；仅当不变式已由类型或前置校验保证、失败即代码缺陷时，才允许 `debug_assert` 后返回 `Internal`。
- 落盘数据一律视为不可信：解析 header、键、长度、AAD、JSON 前先校验；损坏或恶意输入只返回 `Corrupted` / `Deserialize`，不得越界或崩溃。
- 由于所有写操作都会读取并复用已有 header，写入路径同样执行完整解码与校验。
- 长度与偏移计算使用 `checked_*`，防止整数溢出；不靠 unsafe 做性能捷径。
- 内存分配失败、文件锁冲突、磁盘只读等环境错误都返回错误，不 abort；redb 的 `DatabaseAlreadyOpen` 映射为 `Locked`，其余存储错误归入 `Database`。
- DK 解包与 key check 失败映射为 `InvalidKey`；记录结构或认证失败映射为 `Corrupted`；库文件缺失映射为 `NotFound`。
- facade 不把 panic 带过线程边界：`spawn_blocking` 的 `JoinError` 统一转为 `Internal`；句柄状态锁中毒同样返回 `Internal`。
- 关闭语义：取引擎时登记在途计数，`close` 置空引擎并等待计数归零；已开始的操作持有 `Arc<Engine>` 完成，之后数据库才释放文件锁。
- FFI 边界（未来）必须用 `catch_unwind` 把 panic 转为错误码，绝不让 panic 跨 C ABI 传播。
- 测试覆盖截断值、随机字节、错误口令、并发关闭与磁盘只读；关键编解码做 fuzz/property 测试。当前已覆盖错误口令、截断/篡改的 blob 与记录、分页、批量回滚、句柄关闭与只读拒写。
- 静态护栏：workspace lints 拒绝 `unsafe`、`unwrap` / `expect` / `panic!` 与未实现的占位宏，新增 crate 默认继承。
- 编解码用例必须覆盖损坏输入：魔数不符、未知格式版本、`key_id` 越界、截断、`ct_len` 与总长不符、尾随字节。

## 影响与兼容性

- 不改变 `format=1`、AAD 与 blob 布局；已有数据库无需迁移。
- facade 新增的 `open`/读写/`close` 与 `api.md` 签名一致；`change_password`、`rotate_data_key` 仍未实现，下一变更补齐。
- `read_only` 目前只是 facade 的逻辑闸门，底层仍以读写模式打开；调用方不应依赖它做多进程只读并发。
- API 在 1.0 前允许调整，但任何影响记录格式或密钥层级的变化都必须追加 `DECISIONS.log`。

## 构建与验证

- 工具链由 `rust-toolchain.toml` 固定（1.95.0），MSRV 1.90（redb 4.4 的要求）；低于 MSRV 时 cargo 直接拒绝。
- `cargo build --workspace` 构建全部 crate，产物落 `target/`（不入库）。
- `cargo test --workspace`：当前 60 个用例（core 库内 34、引擎端到端 16、facade 库内 3、facade 端到端 4、跨 crate 集成 3）。
- 引擎端到端使用 `tempfile` 与 8 MiB 的 Argon2id 参数；facade 端到端用默认参数（64 MiB、t=3），4 个用例约 6 秒。
- `cargo clippy --workspace --all-targets -- -D warnings` 与 `cargo fmt --all -- --check` 是合并前的必过项。
- 依赖版本由 `Cargo.lock` 锁定并随仓库提交；升级依赖需重新跑上述三项检查。
