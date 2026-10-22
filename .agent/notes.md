# 工具

sele CLI用于管理 `.design/` 与 `.agent/` 目录的 C++20 命令行工具，支撑「创建变更 → 追加决策 → 查询状态 → 规则检查 → 归档预览」这一最小闭环。

## 构建与测试

- `cargo build --workspace` 构建全部 crate；产物在 `target/`，已加入 `.gitignore`。
- `cargo test --workspace`：66 个用例（shio-core 库内 34、引擎端到端 21、shio 库内 3、facade 端到端 5、跨 crate 集成 3）。
- facade 端到端用默认 Argon2id 参数（64 MiB、t=3），5 个用例约 13 秒；不要在这一层加更多建库用例，参数下调放到 core 测试。
- 提交前必过：`cargo fmt --all -- --check` 与 `cargo clippy --workspace --all-targets -- -D warnings`。
- 工具链由 `rust-toolchain.toml` 固定为 1.95.0（rustfmt + clippy）；首次在该目录执行 cargo 时 rustup 会自动补齐。MSRV 1.90，因为 redb 4.4 要求。
- 升级任何依赖后重跑三项检查；`Cargo.lock` 随仓库提交。

## 引擎用法

- 创建：`Engine::open(EngineOptions::new(path).create(true), password)`；打开：同参数但 `create(false)`。
- `EngineOptions::kdf` 只在创建时生效；已存在的库以 `shio.keys` 落盘参数为准，改参数不会影响旧库。
- 写入：`insert`（自动 UUIDv7）或 `put`（指定 ID、upsert）；`delete` 写墓碑，`purge` 才物理删除。
- 读取：`get` 返回 `Stored<T>`；`list` 只读 header，支持 `after`/`limit`/`include_deleted`。
- 批量：`batch` 单事务原子提交；纯删除批次需要写 `WriteOp::<T>::Delete` 帮助类型推断。
- 原始字节接口：`insert_raw`/`put_raw`/`get_raw`/`batch_raw` 直接收发已序列化的明文，供 facade 跨线程使用。
- `EngineOptions::cache_size` 走 `redb::Database::builder()`；`None` 时用 redb 默认值。
- `EngineOptions::read_only` 用 `redb::ReadOnlyDatabase` 打开；读事务照常，写操作返回 `ReadOnly`，`create + read_only` 在打开前被拒绝。
- `change_password` 只重包裹 DK；有多行 key 时返回 `Busy`。`rotate_data_key` 先生成新 key 行，再按 256 条一批重加密，最后切换 meta 并删除旧行；中断后重跑会复用已存在的下一号 key 行。
- 测试里 Argon2id 用 `KdfParams::new(8_192, 1, 1)`，所有用例在 1 秒内跑完。

## facade 用法

- `Kernel::open(OpenOptions::new(path).create(true), Credential::password("...")).await?`。
- `kernel.namespace("notes")?.collection::<Note>("note")?` 是纯构造；集合在首次写入时登记。
- `Collection<T>` 的 `insert/put/get/delete/purge/list/batch` 都是 async，内部 `spawn_blocking`。
- `close().await` 会等待已开始的操作结束；之后新操作返回 `Closed`，重复 `close` 返回 `Ok`。
- `read_only(true)` 现在真正以只读库打开，写操作（含改口令、轮换）返回 `ReadOnly`。
- `change_password(SecretString)` 与 `rotate_data_key()` 都是 async；轮换会长时间占用一个阻塞线程。

## 踩坑

- workspace lints 把 `clippy::unwrap_used` 等设为 deny：测试代码必须放宽，库内用 `#![cfg_attr(test, allow(...))]`，集成测试文件用 `#![allow(...)]`，否则 `-D warnings` 直接失败。
- `sele close` 按小节标题折叠，而本仓库 `SPEC.md` 是「一个概念一节」：合并时把 `design.md` 的小节标题写成与 SPEC 概念节同名，close 才会原地替换；否则会把模板四节当新小节追加。
- 禁词检查按子串匹配，`todo!` 这类写法会被当成 TODO 命中；在 `.design/` 里描述 lint 名单时改成「占位宏」之类措辞。
- redb 的读事务不会创建缺失的表：新库若在首次写入前就读，会报 `TableDoesNotExist`。初始化事务里必须把四张表都打开一次再提交。
- `EngineOptions::kdf` 只在创建时读取；测试里用小参数建库后，重开时传默认参数也不会重新派生。
- `batch` 的泛型 `T` 无法从纯 `Delete` 项推断，调用方要写 `WriteOp::<T>::Delete`；`insert`/`put` 的类型可由载荷推断。
- catalog 键是长度前缀编码，字节序不等于字母序（`beta` 会排在 `alpha` 前）；对外暴露集合清单前要显式排序。
- `spawn_blocking` 的闭包要求 `'static`：`Collection::insert(&T)` 的借用搬不进去。做法是调用线程序列化成 `Zeroizing<Vec<u8>>`，再交给 core 的 `*_raw` 接口；不要为此要求 `T: Clone`，更不要 transmute 生命周期。
- `std::sync::MutexGuard` 跨 await 会被 clippy 的 `await_holding_lock` 拒绝（即使手动 `drop`）：把「判断/计数」放进独立块，块结束再 await。
- 轮换中断后重跑：先检查 `key_id + 1` 的行是否已存在，存在就复用；否则会与上次写入的新行冲突。
- 轮换持有会话写锁期间，内部迁移只能接收 `&Session`，不能再去拿读锁（会自锁）。
- `change_password` 的 `Busy` 判定要放在派生新 KEK 之前，避免白跑一次 Argon2id。
- redb 的 `range` 传 `(Bound, Bound)` 元组时推不出 `KR`，要写 `table.range::<&[u8]>(range)`。
- 私有 helper 参数超过 7 个会触发 `clippy::too_many_arguments`；把 `(namespace, collection, id)` 收进 `RecordPath`。
- 在 PowerShell 里 `cargo ... 2>&1 | Select-Object` 会把 cargo 的进度输出当成错误，退出码不可靠；判断成败看输出末行或 `$LASTEXITCODE` 前先确认不是管道假阳性。


