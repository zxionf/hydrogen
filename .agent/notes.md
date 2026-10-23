# 工具

sele CLI用于管理 `.design/` 与 `.agent/` 目录的 C++20 命令行工具，支撑「创建变更 → 追加决策 → 查询状态 → 规则检查 → 归档预览」这一最小闭环。

## 构建与测试

- `cargo build --workspace` 构建全部 crate；产物在 `target/`，已加入 `.gitignore`。
- `cargo run -p shio --example notes` 运行最小示例（临时库，结束即删除）。
- `cargo test --workspace`：101 个用例（含 shio-ffi 的 C ABI 冒烟与并发用例）。
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
- `shio::version()` 返回编译期包版本，不需要打开数据库；库的格式版本从 `Kernel::stats().format_version` 读取。

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

## Android 构建

- 目标：`aarch64-linux-android` 真机、`x86_64-linux-android` 模拟器；`armv7` 暂不支持。
- minSdk 24 已锁定；NDK 使用 `29.0.14206865`（r29），路径在 `%LOCALAPPDATA%\Android\Sdk\ndk`。
- 安装 Rust 标准库：`rustup target add aarch64-linux-android x86_64-linux-android`。
- 仅类型检查不需要 NDK：`cargo check -p shio-core -p shio -p shio-ffi --target aarch64-linux-android`，x86_64 同理；加 `-p shio --features sync` 检查同步 feature。
- `shio-ffi` 已配置 `crate-type = ["rlib", "cdylib"]`；链接时设置 `CARGO_TARGET_<TARGET>_LINKER` 为 NDK 的 `<triple>24-clang.cmd`，并加 `RUSTFLAGS="-C link-arg=--target=<triple>24"`。
- 验证 `.so`：`llvm-readelf -l` 的 `LOAD` 行显示 `Align 0x4000`（16 KB）；`llvm-readelf -n` 的 `.note.android.ident` 数据首 4 字节为 `0x18`，即 API 24。
- Android target 额外依赖 `rustix 1.1`（`fs` feature），仅用于旁路文件锁；非 Android 构建不启用。
- x86_64 模拟器已通过设备冒烟；arm64 真机加载、JNI 联调与低端机 KDF 实测仍待补。

## Android 设备冒烟

- AVD：`shio_x86_64`，系统镜像 `system-images;android-35;google_apis;x86_64`；Emulator 37.2.12。
- Harness：`crates/shio-ffi/examples/android_smoke.c`，模式 `smoke`、`hold`、`probe`、`check`。用 NDK 的 `x86_64-linux-android24-clang` 编译，与 `libshio_ffi.so` 一起 push 到 `/data/local/tmp/shio-smoke`，运行时设 `LD_LIBRARY_PATH=.`。
- 结果：`smoke` 输出 `ANDROID_SMOKE_OK`，覆盖 create/open、stats、insert/get/put/list/batch/delete、close/重开、改口令、轮换、只读拒写；同进程第二写者返回 `SHIO_LOCKED`。
- 跨进程锁：`hold` 模式持锁 20 秒，另一进程 `probe` 输出 `PROBE_LOCKED`；`kill -9 $(pidof android_smoke)` 后 `check` 输出 `CHECK_OK`，确认锁随进程退出释放且库可重开。
- redb 4.4 在 Android 上没有可用的文件锁：字节范围锁返回 `Unsupported`，跨进程第二写者原本可以直接打开。现在由 `crates/shio/src/lock.rs` 在数据库旁创建 `<db>.lock`，用 `rustix::fs::flock` 非阻塞加锁：写句柄独占、只读共享；冲突映射为 `Error::Locked`，`close` 后在途操作归零再释放。
- `.lock` 文件不要在手柄打开期间删除；它可以在下次打开时重建。备份应排除数据库与 `.lock`，或成对处理。
- 模拟器子进程会随调用的 shell 结束被回收；启动模拟器、等待 boot、push、运行测试必须放在同一次 shell 调用里。若 AVD 被清理，用 `avdmanager create avd -n shio_x86_64 -k 'system-images;android-35;google_apis;x86_64' --device pixel_2` 重建。

## C ABI v1

- `shio-ffi` 已从占位改为可链接的 `rlib + cdylib`，契约头文件：`crates/shio-ffi/include/shio.h`。
- 导出：`shio_abi_version`、`shio_last_error`、`shio_open`、`shio_close`、`shio_kernel_destroy`、`shio_free`、`shio_insert`、`shio_put`、`shio_get`、`shio_delete`、`shio_list`、`shio_batch`、`shio_change_password`、`shio_rotate_data_key`、`shio_flush`、`shio_stats`。
- `shio_open` 的 options JSON：`create`、`read_only`、`max_value_size`、`cache_size`；未知字段拒绝。字符串为 NUL 结尾 UTF-8，载荷/口令/ID 显式带长度。
- 每个句柄持有独立 tokio multi-thread runtime；所有函数阻塞，Android 必须从 worker 线程调用。
- `shio_close` 等待在途操作但不释放句柄；`shio_kernel_destroy` 才释放。关闭后再操作返回 `SHIO_CLOSED`。
- 返回缓冲区由调用方 `shio_free` 释放；释放前会 `zeroize`。口令临时缓冲在边界内清零。
- 并发测试覆盖 close 与在途 put 竞态、重复 close、关闭后拒绝、错误详情线程局部。
- 构建命令示例（PowerShell）：

```powershell
$bin = Join-Path $env:LOCALAPPDATA 'Android\Sdk\ndk\29.0.14206865\toolchains\llvm\prebuilt\windows-x86_64\bin'
$env:CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER = Join-Path $bin 'aarch64-linux-android24-clang.cmd'
$env:RUSTFLAGS = '-C link-arg=--target=aarch64-linux-android24'
cargo build -p shio-ffi --release --target aarch64-linux-android
```

- 已验证：两个 target 的 `cargo check` 通过；两个 release `.so` 均导出全部 16 个符号；aarch64 的 `LOAD` 对齐 `0x4000`，`.note.android.ident` 首 4 字节 `0x18`（API 24）；`include/shio.h` 已用 NDK clang 对两个 target 做 `-fsyntax-only` 检查。

## Android 打包与排错

- Gradle 目录：`app/src/main/jniLibs/arm64-v8a/libshio_ffi.so`、`app/src/main/jniLibs/x86_64/libshio_ffi.so`；`abiFilters += ["arm64-v8a", "x86_64"]`，`minSdk = 24`。
- Kotlin 用 `System.loadLibrary("shio_ffi")` 加载；JNI 层由 `shio-android` 实现，内核只提供 C ABI。
- `UnsatisfiedLinkError`：先核对 ABI 目录名、`.so` 是否被 16 KB/压缩处理破坏、minSdk 与 `.note.android.ident`；再用 `llvm-readelf --dyn-syms` 确认 `shio_*` 符号存在。
- `shio_last_error` 是线程局部的；错误必须在发起调用的同一个 worker 线程读取。
- `shio_open`/`shio_close`/改口令/轮换会阻塞；不要在主线程调用。KDF 默认 64 MiB、t=3，低端机首次建库可能明显卡顿。
- 数据库放 `filesDir` 等私有目录；多进程只允许一个可写 owner。备份/自动恢复必须排除库文件，否则可能恢复到不一致状态。
- 释放顺序：`shio_close` → 无并发调用 → `shio_kernel_destroy`；所有 `uint8_t**` 返回值用 `shio_free(ptr, len)` 释放。


