# 工具

sele CLI用于管理 `.design/` 与 `.agent/` 目录的 C++20 命令行工具，支撑「创建变更 → 追加决策 → 查询状态 → 规则检查 → 归档预览」这一最小闭环。

## 构建与测试

- `cargo build --workspace` 构建全部 crate；产物在 `target/`，已加入 `.gitignore`。
- `cargo run -p shio --example notes` 运行最小示例（临时库，结束即删除）。
- `cargo test --workspace`：全部单元/集成/冒烟用例（含 shio-ffi 的 C ABI 同步、并发与错误边界用例）。
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
- 维护模式：`begin_maintenance()` / `end_maintenance()` / `migrate_step(to, max_records)` / `migrate_tick(to, budget, max_records)` 都是 async；前两个走读路径，后两个走写路径。窗口内 `Collection` 的写方法返回 `Busy`，读方法不受影响；`migrate_tick` 不要求维护模式，调用间隙普通读写照常。
- `shio::version()` 返回编译期包版本，不需要打开数据库；库的格式版本从 `Kernel::stats().format_version` 读取。
- 离线物理压缩：`shio::compact_file(PathBuf)` 是自由函数（不是 `Kernel` 方法），要求先关闭所有句柄；内部取 `.lock` 独占，句柄未关返回 `Locked`。返回 `CompactReport { before_bytes, after_bytes, compacted }`，不需要口令。

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
- `format=2` 迁移的进度与取消状态放在 `Engine` 的原子字段里，查询进度和请求取消不获取 session 锁；取消标志在新迁移开始时清除，取消只在批次提交后的边界生效。
- `MigrationOptions` 默认 256 条/批、无暂停；`batch_size` 上限 1024，`pause_ms` 上限 60000。暂停按 50ms 切片，便于取消尽快返回。
- 恢复中断轮换的入口是 `rotation_state()` 取 `to` 后调用 `migrate_all(to)`/`migrate_all_with_options`；`rotate_data_key()` 只用于发起新轮换，不再承担恢复。
- 维护模式：`begin_maintenance()` 置进程内标志（不落盘），期间普通写返回 `Busy`、读放行；`end_maintenance()` 返回 `MaintenanceOutcome { done }`，`done=false` 表示游标仍在。
- 步进迁移：`migrate_step(to, max_records)` 一批一锁，提交即释放；`max_records=0` 取默认 256、上限 1024；最后一批之后同一次调用内收尾并返回 `done=true`。与 `migrate_all` 共用持久化游标，可混用。
- 合作式调度：`migrate_tick(to, budget, max_records)` 在预算内连续跑若干小批次，每批单独提交；`budget` 为 0 取默认 10ms、上限 1s，`max_records` 为 0 取默认 32、上限 256。不要求维护模式、不冻结写，会话写锁只在单次调用内持有。
- tick 的预算检查放在每批之后，保证每次调用至少推进一批；`Duration::from_nanos(1)` 这种极小预算下也只是「一批就返回」，不会空转。短批次（不足 `max_records`）视为扫描结束，收尾与最后一批不在同一事务。
- tick 的进度计数在序列开始时（目标 key 没有游标）清零，单次返回值只含本次调用；取消标志在每次 tick 开始时清除，与 `migrate_step` 一致，停止调度就是不再调用。
- tick 与 `migrate_all` / `migrate_step` 共用游标和会话写锁，可任意混用；`retire_key` 自带「仍有记录引用该 key 则返回 `Busy`」，迁移途中误调用不会删掉旧 key。
- 压缩只整理 redb 空闲页：`Database::compact` 需要唯一可变借用且没有在途事务/savepoint，所以先做离线入口；`Engine` 被 `Arc` 共享、`Handle` 直接持有 `Database`，在线压缩要先改成 `RwLock<Handle>`，且并发读会让它退化成 `Busy` 重试。
- redb 关闭句柄时会收缩预分配空间，`Kernel::stats().db_bytes`（关闭前）与 `compact_file` 的 `before_bytes`（关闭后）可能不相等；测试与文档不要假设两者相同。
- `compact_file` 的错误映射：`.lock` 被占或 `DatabaseAlreadyOpen` → `Locked`；文件不存在 → `NotFound`；0 字节 → `InvalidArgument`；`TransactionInProgress`/`SavepointExists` → `Busy`；压缩后 `check_integrity` 失败 → `Corrupted`。
- 压缩不受 format 限制、不需要 DK；format=1/2 都可压缩，压缩后 `db_id`/key 链/同步游标不变。
- S2 备份在一个 redb 读事务里按已知表定义逐表复制原始键值，不解密、不重加密；表名与键类型不一致会编译失败，新增表必须同步更新 `engine_backup.rs` 的复制清单。
- 备份先写目标同目录的随机临时文件，复制后只读打开校验每张表的行数与 `meta` 原始字节，再 `rename` 发布；目标已存在直接返回 `InvalidArgument`。临时文件由 `Drop` 清理，不引入 `tempfile` 运行时依赖。
- 恢复只接受不存在或 0 字节目标；先 `fs::copy` 到临时文件，用原口令只读打开完成 key check/结构校验，再移除空目标并 `rename`，最后按调用方选项重新打开。最终打开失败会删除目标；错误口令/损坏备份不会发布目标。
- facade 的 `Kernel::restore` 强制 `create=false`，并在 Android 上先取得目标 `.lock` 独占锁；恢复后的库保留源 `db_id`/`device_id`/key 链/同步游标，禁止同一备份恢复到两台设备后同时同步。
- `change_password` 的 `Busy` 判定要放在派生新 KEK 之前，避免白跑一次 Argon2id。
- redb 的 `range` 传 `(Bound, Bound)` 元组时推不出 `KR`，要写 `table.range::<&[u8]>(range)`。
- 私有 helper 参数超过 7 个会触发 `clippy::too_many_arguments`；把 `(namespace, collection, id)` 收进 `RecordPath`。
- 在 PowerShell 里 `cargo ... 2>&1 | Select-Object` 会把 cargo 的进度输出当成错误，退出码不可靠；判断成败看输出末行或 `$LASTEXITCODE` 前先确认不是管道假阳性。
- 维护闸门放在 `Engine::require_writable`，`migrate_step` 走 `require_writable_internal` 绕开；以后新增写入口必须继续调用 `require_writable`，否则会漏过维护窗口。
- `begin_maintenance` / `end_maintenance` 会取会话写锁与迁移串行化；不要在已持有该会话锁的路径里调用（会自锁）。
- `migrate_step` 每步开始时清除取消标志：`cancel_migration` 只对正在执行的那一步有效，在两步之间调用不会累积到下一步。
- 步进模式的 `scanned` / `migrated` 在 `begin_maintenance` 时清零，之后每步累加；`migrate_step` 的返回值只含本步统计。
- `purge` 的判定顺序是 `SyncEnabled` 先于 `Busy`：同步启用后物理删除永久禁止，不应被维护窗口的错误码盖住。
- 收尾（扫引用 + 公告 `Migrated` + 删游标）与最后一批不在同一事务；收尾前崩溃时游标仍在末尾，重跑一次 `migrate_step` 即完成。

## Android 构建

- 目标：`aarch64-linux-android` 真机、`x86_64-linux-android` 模拟器；`armv7` 暂不支持。
- 应用 minSdk 29 已锁定（2026-10-24 确认；此前曾按 24 记录）；NDK 使用 `29.0.14206865`（r29），路径在 `%LOCALAPPDATA%\Android\Sdk\ndk`。
- 安装 Rust 标准库：`rustup target add aarch64-linux-android x86_64-linux-android`。
- 仅类型检查不需要 NDK：`cargo check -p shio-core -p shio -p shio-ffi --target aarch64-linux-android`，x86_64 同理；加 `-p shio --features sync` 检查同步 feature。
- `shio-ffi` 已配置 `crate-type = ["rlib", "cdylib"]`；内核 `.so` 仍按 API 24 构建基线链接：设置 `CARGO_TARGET_<TARGET>_LINKER` 为 NDK 的 `<triple>24-clang.cmd`，并加 `RUSTFLAGS="-C link-arg=--target=<triple>24"`。
- 验证 `.so`：`llvm-readelf -l` 的 `LOAD` 行显示 `Align 0x4000`（16 KB）；`llvm-readelf -n` 的 `.note.android.ident` 数据首 4 字节为 `0x18`，即 API 24（这是内核构建基线，不是应用 minSdk）。
- Android target 额外依赖 `rustix 1.1`（`fs` feature），仅用于旁路文件锁；非 Android 构建不启用。
- x86_64 模拟器已通过设备冒烟；arm64 真机加载与 JNI 联调已由 `oxygen`（vivo V2359A / Android 16）完成。轮换中断续跑、独立进程只读与双库同步冒烟已在 x86_64 模拟器 C harness 验证。默认 KDF 保持 `64 MiB、t=3`，低端机不纳入首版验证。

## Android 设备冒烟

- AVD：`shio_x86_64`，系统镜像 `system-images;android-35;google_apis;x86_64`；Emulator 37.2.12。Linux 开发机上当前 AVD 为 `Medium_Phone`（x86_64、Android 17），另有一台 arm64 真机 `10AE4N38SA000LK`（Android 16）。
- NDK：`ANDROID_HOME=/home/zxionf/Android/Sdk`，`NDK_HOME=/home/zxionf/Android/Sdk/ndk/29.0.14206865`（另有 30.0.14904198）；`rustup target` 已装 `aarch64-linux-android` 与 `x86_64-linux-android`。
- Harness：`crates/shio-ffi/examples/android_smoke.c`，模式 `smoke`、`sync`、`hold`、`probe`、`check`、`read-hold`、`read-probe`、`read-locked`、`rotate-many`、`resume-rotation`、`migration-progress`、`maintenance`、`cooperative`、`compact`、`backup-restore`。用 NDK 的 `x86_64-linux-android24-clang` 编译，与 `libshio_ffi.so` 一起 push 到 `/data/local/tmp/shio-smoke`，运行时设 `LD_LIBRARY_PATH=.`。
- 结果：`smoke` 输出 `ANDROID_SMOKE_OK`，覆盖 create/open、stats、insert/get/put/list/batch/delete、close/重开、改口令、轮换、只读拒写；同进程第二写者返回 `SHIO_LOCKED`。
- 跨进程锁：`hold` 模式持锁 20 秒，另一进程 `probe` 输出 `PROBE_LOCKED`；`kill -9 $(pidof android_smoke)` 后 `check` 输出 `CHECK_OK`，确认锁随进程退出释放且库可重开。
- 只读并发：写者持锁时 `read-locked` 输出 `READ_LOCKED`；`read-hold` 持共享锁时 `probe` 输出 `PROBE_LOCKED`、`read-probe` 输出 `READ_PROBE_OK`。若写者被 `kill -9`，redb 可能先要求写句柄修复，直接只读打开会返回 `Database repair aborted`；先跑一次 `check`（写打开）再只读即可。
- 同步 C ABI：`sync` 模式创建 A 库并 `sync_enable`，生成配对包后创建 B 库，双向变更、快照、轮换、ack 与压缩后重同步全部通过，输出 `ANDROID_SYNC_OK`；变更/快照使用 `limit=2`，确认多页路径。轮换现在通过 `shio_sync_migrate_all_ex` 执行，并检查进度 JSON。
- 迁移取消：`migration-progress` 模式插入 40 条记录后启动 `batch_size=1,pause_ms=20` 的后台迁移，主线程轮询到 `active=true`/`migrated>=1` 后调用 `shio_sync_cancel_migration`；worker 返回 `SHIO_CANCELLED`，随后再次调用 `shio_sync_migrate_all_ex` 续跑并检查 `done=true`。x86_64 模拟器输出 `MIGRATION_PROGRESS_OK`。
- 维护模式：`maintenance` 模式写入 24 条记录并发起轮换，`shio_sync_begin_maintenance` 后 `shio_put` 返回 `SHIO_BUSY` 且不产生缓冲区，`shio_get`/`shio_collections` 正常，`shio_purge` 返回 `SHIO_SYNC_ENABLED`（优先于 `SHIO_BUSY`）；随后每步 5 条分 5 步迁移到 `done`，`shio_sync_end_maintenance` 返回 `done=1`，退出后写入并读回新值。x86_64 模拟器 `emulator-5554`（Android 17）与 arm64 真机 `10AE4N38SA000LK`（Android 16）均输出 `MAINTENANCE_OK`；同轮回归 `smoke`、`sync`、`migration-progress`、`backup-restore` 全部通过。
- 迁移续跑：`format=2` 每批提交时把目标 key 与最后扫描键写入 `shio.sync` 的 `b"migration"`；取消、崩溃或重开后同一 `to` 从未扫描位置继续，不重新扫描已提交范围。切换目标 key 会忽略旧游标，完成迁移或实际应用远端记录会删除游标并从头扫描。C ABI 未变化。
- 物理压缩：`compact` 模式写入 40 条并删除 39 条，句柄未关闭时 `shio_compact_file` 返回 `SHIO_LOCKED` 且不产生缓冲区；关闭后压缩把文件从 86016 字节收缩到 57344 字节，连续压缩返回 `compacted=0`，重开后保留记录可读并可继续写入。x86_64 模拟器 `emulator-5554` 与 arm64 真机 `10AE4N38SA000LK` 均输出 `COMPACT_OK`。
- 合作式调度：`cooperative` 模式写入 18 条记录并发起轮换，`shio_sync_migrate_tick(kernel, to, 1, 4, ...)` 首次只推进一批；随后直接 `shio_put` 写入中间记录并读回（不进维护模式、写不被拒绝），继续 tick 到 `done=1`，检查进度 `active=false`/`done=true`、首尾记录可读，最后断言 `budget_ms=1001` 与 `max_records=257` 返回 `SHIO_INVALID_ARGUMENT`。x86_64 模拟器 `emulator-5554`、arm64 真机 `10AE4N38SA000LK` 均输出 `COOPERATIVE_OK`。
- 轮换中断：`rotate-many` 建 60000 条记录后打印 `ROTATE_START` 再执行 `shio_rotate_data_key`；外部在启动后 `kill -9`，随后 `resume-rotation` 读取 `shio_sync_rotation_state` 的 `to`，用 `shio_sync_migrate_all_ex` 续跑并读首尾记录、写入新记录，输出 `ROTATE_RESUME_OK`。
- 备份恢复：`backup-restore` 模式在 `format=2` 源库写入记录并 `sync_enable`，调用 `shio_backup` 检查摘要，拒绝已存在备份目标和非空恢复目标，错误口令返回 `SHIO_INVALID_KEY` 且不建目标；随后 `shio_restore` 打开目标、读取原记录并比较 `db_id`/`device_id`。x86_64 模拟器输出 `BACKUP_RESTORE_OK`。
- redb 4.4 在 Android 上没有可用的文件锁：字节范围锁返回 `Unsupported`，跨进程第二写者原本可以直接打开。现在由 `crates/shio/src/lock.rs` 在数据库旁创建 `<db>.lock`，用 `rustix::fs::flock` 非阻塞加锁：写句柄独占、只读共享；冲突映射为 `Error::Locked`，`close` 后在途操作归零再释放。
- `.lock` 文件不要在手柄打开期间删除；它可以在下次打开时重建。S2 的 `shio_backup` 只写调用方指定的独立路径，不复制源库的 `.lock`；应用层自行做整目录文件备份时仍应排除数据库与 `.lock`，或成对处理。
- 模拟器子进程会随调用的 shell 结束被回收；启动模拟器、等待 boot、push、运行测试必须放在同一次 shell 调用里。若 AVD 被清理，用 `avdmanager create avd -n shio_x86_64 -k 'system-images;android-35;google_apis;x86_64' --device pixel_2` 重建。

## C ABI v1

- `shio-ffi` 已从占位改为可链接的 `rlib + cdylib`，契约头文件：`crates/shio-ffi/include/shio.h`。
- 导出：`shio_abi_version`、`shio_last_error`、`shio_open`、`shio_close`、`shio_kernel_destroy`、`shio_free`、`shio_insert`、`shio_put`、`shio_get`、`shio_delete`、`shio_purge`、`shio_list`、`shio_collections`、`shio_batch`、`shio_change_password`、`shio_rotate_data_key`、`shio_flush`、`shio_stats`。
- `shio_purge` 是物理删除（facade `Collection::purge`）：`shio_delete` 只写墓碑；同步启用后物理删除返回 `SHIO_SYNC_ENABLED`，记录不存在时返回成功且 `out_existed=0`。
- `shio_collections` 返回命名空间下已登记集合名，JSON 字符串数组、按名称升序；未写入过任何集合时返回 `[]`。
- P4 追加 18 个同步/配对符号：`shio_sync_enable/info/changes/apply/offers/apply_offers/snapshot_start/snapshot_page/apply_snapshot/begin_rotation/rotation_state/ack_rotation/migrate_all/retire/prune_offers/compact/create_pairing` 与 `shio_open_with_pairing`；S1 再追加 `shio_sync_migrate_all_ex`、`shio_sync_migration_progress`、`shio_sync_cancel_migration` 与错误码 `SHIO_CANCELLED(-23)`；S2 追加 `shio_backup`、`shio_restore`；维护模式追加 `shio_sync_begin_maintenance`、`shio_sync_end_maintenance`、`shio_sync_migrate_step`（窗口内普通写返回 `SHIO_BUSY`，步进返回 `{"scanned","migrated","done"}`）；合作式调度追加 `shio_sync_migrate_tick`（时间预算内多批推进，不冻结写）；物理压缩追加 `shio_compact_file`（离线整理空闲页，不需要句柄）。完整签名见 `include/shio.h` 与 `.design/api.md` §8。ABI 版本仍为 1（只增符号）。
- `shio_open` 的 options JSON：`create`、`read_only`、`max_value_size`、`cache_size`；未知字段拒绝。字符串为 NUL 结尾 UTF-8，载荷/口令/ID 显式带长度。
- 同步业务帧是 `SHY2` 不透明字节，调用方不要把帧内容塞进 JSON 或自行改写；状态、轮换信息和应用报告才是 JSON。首版建库先 `shio_open`，紧接着 `shio_sync_enable`，成功后才写业务记录；失败就关闭并删除尚未使用的库。
- `shio_sync_changes` 的 `after` 是上一页 `out_next`；日志被压缩后返回 `SHIO_SYNC_RESYNC_REQUIRED`，改用 `snapshot_start/page`。`shio_sync_snapshot_page` 的 `out_next` 是另一条不透明游标，`out_done=1` 时为空。
- 每个句柄持有独立 tokio multi-thread runtime；所有函数阻塞，Android 必须从 worker 线程调用。
- `shio_close` 等待在途操作但不释放句柄；`shio_kernel_destroy` 才释放。关闭后再操作返回 `SHIO_CLOSED`。
- 返回缓冲区由调用方 `shio_free` 释放；释放前会 `zeroize`。口令临时缓冲在边界内清零。
- 并发测试覆盖 close 与在途 put 竞态、重复 close、关闭后拒绝、错误详情线程局部。
- 构建命令示例（PowerShell；以下是内核 `.so` 的 API 24 构建基线，应用侧 minSdk 为 29）：

```powershell
$bin = Join-Path $env:LOCALAPPDATA 'Android\Sdk\ndk\29.0.14206865\toolchains\llvm\prebuilt\windows-x86_64\bin'
$env:CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER = Join-Path $bin 'aarch64-linux-android24-clang.cmd'
$env:RUSTFLAGS = '-C link-arg=--target=aarch64-linux-android24'
cargo build -p shio-ffi --release --target aarch64-linux-android
```

- 已验证：两个 target 的 `cargo check` 通过；P4 重新构建两个 release `.so`，均导出 18 个同步/配对符号；两个 ABI 的 `LOAD` 对齐均为 `0x4000`，`.note.android.ident` 首 4 字节 `0x18`（API 24）；`include/shio.h` 已用 NDK clang 对两个 target 做 `-fsyntax-only` 检查。

## Android 打包与排错

- Gradle 目录：`app/src/main/jniLibs/arm64-v8a/libshio_ffi.so`、`app/src/main/jniLibs/x86_64/libshio_ffi.so`；`abiFilters += ["arm64-v8a", "x86_64"]`，应用 `minSdk = 29`。
- Kotlin 用 `System.loadLibrary("shio_ffi")` 加载；JNI 层由 `shio-android` 实现，内核只提供 C ABI。
- `UnsatisfiedLinkError`：先核对 ABI 目录名、`.so` 是否被 16 KB/压缩处理破坏、minSdk 与 `.note.android.ident`；再用 `llvm-readelf --dyn-syms` 确认 `shio_*` 符号存在。
- `shio_last_error` 是线程局部的；错误必须在发起调用的同一个 worker 线程读取。
- `shio_open`/`shio_close`/改口令/轮换会阻塞；不要在主线程调用。KDF 默认 64 MiB、t=3，低端机不纳入首版验证。
- 数据库放 `filesDir` 等私有目录；多进程只允许一个可写 owner。备份/自动恢复必须排除库文件，否则可能恢复到不一致状态。
- 释放顺序：`shio_close` → 无并发调用 → `shio_kernel_destroy`；所有 `uint8_t**` 返回值用 `shio_free(ptr, len)` 释放。


