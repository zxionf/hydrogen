# 20261022-android-kernel

<!-- 为什么改、边界与不属于本次的范围 -->

## 平台与互操作

本次变更新增 Android 作为正式支持平台，但不改变内核的存储与加密模型。

- Android 首版至少覆盖 `aarch64-linux-android` 真机和 `x86_64-linux-android` 模拟器；`armv7-linux-androideabi` 仅在明确需要 32 位时加入。
- 首版 minSdk 锁定为 24；Rust 链接参数与 `.note.android.ident` 均按 API 24 验证。
- `shio-core` 与 `shio` 继续保持 `forbid(unsafe_code)`；Android 调用边界收敛到 `shio-ffi`。
- 用户已正式批准 `shio-ffi` 的 `unsafe` 例外。例外范围只限 `shio-ffi` 的 FFI 边界模块：裸指针校验、句柄转换、返回缓冲区所有权和 `catch_unwind`；crate 级保持 `deny(unsafe_code)`，仅边界模块局部允许。
- FFI 内部持有 tokio runtime，对外提供阻塞调用；Kotlin/Java 侧必须从 worker 线程调用，主线程不执行 Argon2id 或数据库打开。
- 数据库位于应用私有目录；内核不依赖 Android Keystore，口令保护由应用层决定。
- 内核保持单写多读。Android 多进程场景只能有一个可写 owner；独立同步进程不得同时写同一库。
- 同步传输、TLS、二维码、后台调度和重试不属于内核；内核只提供 `SHY2` 帧与配对文本。
- 记录格式不因 Android 改变；同步库仍使用 `format=2`，升级不可回退。
- Android 构建需要验证 `minSdk`、`getrandom`、redb 文件锁、`fsync` 与 16 KB page size。实测 redb 4.4 的字节范围锁在 Android 上返回 `Unsupported`，因此 Android 改用旁路 `<db>.lock` + `rustix::fs::flock`：写句柄独占、只读句柄共享，冲突映射为 `Locked`。

## C ABI

`shio-ffi` 的最小可用边界包括：不透明句柄、稳定错误码、线程局部错误详情、显式长度的字节串、调用方释放返回缓冲区、ABI 版本查询，以及 `catch_unwind` 防止 panic 跨 C ABI。契约头文件为 `crates/shio-ffi/include/shio.h`。

- v1 已实现 open/close、insert/put/get/delete/list/batch、change_password、rotate_data_key、stats 和 flush；`shio_abi_version()` 返回 `1`。
- sync/pairing 不进入 v1；是否纳入由 Android 首版是否需要多端同步决定。若纳入，先补 `.design/api.md` §8 的 sync 契约。
- FFI 关闭必须等待在途调用结束；`shio_close` 可重复调用且不释放句柄，之后操作返回 `SHIO_CLOSED`；`shio_kernel_destroy` 才释放句柄，调用前必须确保没有其它线程继续使用。
- 返回的明文缓冲区由调用方通过 `shio_free` 释放；口令临时缓冲区在边界内用 `zeroize` 清零，`shio_free` 也会在释放前清零返回缓冲区。
- FFI 内部为每个句柄持有 tokio multi-thread runtime，所有导出函数阻塞；Android 必须从 worker 线程调用。
- ABI 在 1.0 前允许调整，但 Android 仓库必须锁定内核提交并检查 ABI 版本。

## Android 集成

内核只稳定 C ABI；JNI 层由 `shio-android` 实现，负责 Kotlin/Java 类型、worker executor、`System.loadLibrary("shio_ffi")`、错误码到异常的映射和缓冲区释放。这样内核不必绑定 Android UI 技术栈，也不会把 Java 类名写进 ABI。

- `.so` 放入 `app/src/main/jniLibs/{arm64-v8a,x86_64}/libshio_ffi.so`；`abiFilters` 只保留这两个 ABI，`minSdk 24`。
- 数据库路径使用应用私有目录（如 `filesDir`），不要放外部存储；备份策略必须排除数据库、`<db>.lock` 与临时文件。
- 所有 open/close、CRUD、改口令、轮换都在 worker 线程调用；主线程只接收结果。
- Kotlin 侧用 `shio_kernel_destroy` 释放句柄，用 `shio_free` 释放返回缓冲区；两者都必须在 `close` 之后、且没有并发调用时执行。
- 16 KB page size 由 NDK 链接参数和 `.so` 的 `LOAD` 对齐保证；Gradle/AGP 打包时不要重压缩 native 库。

## 验证结果

- x86_64 模拟器（Android 35 Google APIs）已通过 C ABI 冒烟：create/open、stats、CRUD、batch、关闭/重开、改口令、轮换、只读拒写，以及同进程与跨进程第二写者拒绝、`kill -9` 后重开。
- 跨进程锁：`hold` 模式持锁，另一进程 `probe` 得到 `SHIO_LOCKED`；杀进程后 `check` 可重新打开。旁路锁在 `close` 后在途操作归零时释放。
- 两个 target 的 release `.so` 均导出 16 个 `shio_*` 符号，`LOAD` 对齐 `0x4000`，`.note.android.ident` 为 API 24。
- 仍待补：arm64 真机加载与 JNI/打包联调、轮换中断续跑、独立进程只读、低端机默认 KDF 实测。

## 范围边界

本次不包含：Android UI、JNI/Kotlin 包装层、Keystore 封装、网络/TLS、二维码扫描、后台同步调度、多写进程、插件系统，以及应用层备份策略。JNI 层由 `shio-android` 维护，内核只提供 C ABI。

## 兼容性

- 不改 `format=1` / `format=2` 的记录布局、AAD 或密钥流程。
- 启用同步仍是不可回退的 `format=2` 升级；Android 新库若计划多端同步，应在首次建库时启用。
- KDF 参数只在建库时生效；低端机参数策略需在首次建库前确定，已有库改参数属于后续独立变更。
