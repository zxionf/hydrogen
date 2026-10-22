# 20261022-version-query

## Context

内核目前能查询库的数据格式版本（`Kernel::stats().format_version`），但没有查询“内核自身版本”的入口。应用做关于页、日志、兼容性诊断时需要这个值，且不希望在打开数据库之前就被卡住。

本变更新增一个纯编译期的版本查询接口，不触及存储与密钥。

## API 风格
- 对外 async（tokio），内核同步；阻塞调用统一走 `spawn_blocking`，不占用 executor 线程。
- core 的同步入口是 `store::Engine`：`Engine::open(EngineOptions, password)`，选项含路径、是否创建、是否只读、明文上限、`app_id`、建库 KDF 参数与 redb 页缓存。
- `Engine` 提供 `insert`/`put`/`get`/`delete`/`purge`/`list`/`batch`/`collections`/`stats`/`flush`、密钥生命周期的 `change_password`/`rotate_data_key`，以及收发已序列化明文的 `insert_raw`/`put_raw`/`get_raw`/`batch_raw`；`Send + Sync`。
- facade 的 `Kernel::open(OpenOptions, Credential)` 完成参数校验、口令派生与解锁；`Credential::password` 接收 `Into<SecretString>`，`Debug` 输出脱敏。
- `Kernel` 是廉价可克隆句柄（内部 `Arc`），`Send + Sync`；提供 `namespace`、`flush`、`close`、`stats`、`change_password`、`rotate_data_key`。`close` 标记关闭并等待在途操作结束，之后新操作返回 `Closed`，重复调用返回 `Ok(())`。
- `shio::version()` 返回编译期内核版本（与包版本一致），不需要打开数据库；库的数据格式版本从 `Kernel::stats().format_version` 读取。
- `Namespace` 与 `Collection<T>` 是从 `Kernel` 派生的轻量句柄，不复制连接；名称在构造时校验，不访问磁盘。`Namespace` 提供 `collections`，`Collection<T>` 提供 `insert/put/get/delete/purge/list/batch`。
- 线程边界：写路径在调用线程把 `T` 序列化为 `Zeroizing<Vec<u8>>`，再由阻塞线程加密落盘；读路径在阻塞线程解密并反序列化。`T` 只需 `Serialize + DeserializeOwned + Send + Sync + 'static`，不要求 `Clone`。
- `read_only=true` 时 core 的写入口直接返回 `ReadOnly`，底层以 redb 只读库打开、不持有写锁；facade 保留进入阻塞线程前的快速闸门，`create` 与 `read_only` 组合非法。
- `change_password` 与 `rotate_data_key` 都是写操作，经 `spawn_blocking` 执行；只读句柄返回 `ReadOnly`。
- 事务边界：v0.1 以 `batch` 作为唯一原子业务写单元，不暴露长生命周期事务对象；密钥轮换按批提交，但通过多密钥会话保证任意中断点都可读。
- 可恢复错误一律返回 `Result`，不以 panic 表达业务失败；公开签名与错误枚举见 `.design/api.md`。

## 影响与兼容性
- 不改变 `format=1`、AAD 与 blob 布局；已有数据库无需迁移。
- `change_password`、`rotate_data_key` 与 `read_only` 已按 `api.md` 落地；facade 签名不变。
- `read_only` 现在真正以 redb 只读库打开，不再持有写锁，可用于多进程只读并发。
- 轮换中途崩溃会留下两行 key：库仍可读，但 `change_password` 返回 `Busy`；重新执行轮换即可完成并删除旧行。
- 新增 `shio::version()` 与 `shio_core::VERSION`，纯增量，不影响已有签名、记录格式与密钥层级。
- API 在 1.0 前允许调整，但任何影响记录格式或密钥层级的变化都必须追加 `DECISIONS.log`。

## 构建与验证
- 工具链由 `rust-toolchain.toml` 固定（1.95.0），MSRV 1.90（redb 4.4 的要求）；低于 MSRV 时 cargo 直接拒绝。
- `cargo build --workspace` 构建全部 crate，产物落 `target/`（不入库）。
- `cargo run -p shio --example notes` 运行最小示例（临时库，结束即删除）。
- `cargo test --workspace`：当前 68 个用例（core 库内 35、引擎端到端 21、facade 库内 4、facade 端到端 5、跨 crate 集成 3）。
- 引擎端到端使用 `tempfile` 与 8 MiB 的 Argon2id 参数，含 300 条记录的跨批轮换与「篡改记录 → 轮换中断 → 还原 → 续跑」；facade 端到端用默认参数（64 MiB、t=3），5 个用例约 13 秒。
- 版本查询断言常量与 `CARGO_PKG_VERSION` 一致，且不依赖打开数据库。
- `cargo clippy --workspace --all-targets -- -D warnings` 与 `cargo fmt --all -- --check` 是合并前的必过项。
- 依赖版本由 `Cargo.lock` 锁定并随仓库提交；升级依赖需重新跑上述三项检查。
