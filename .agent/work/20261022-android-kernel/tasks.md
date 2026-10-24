# 20261022-android-kernel 任务清单

> 2026-10-24 状态回填：应用 minSdk 已确认 29；`oxygen` 的 `20261023-kernel-integration-demo` 已完成 P0–P4，真机/JNI/打包项据此勾选。内核 `.so` 仍按 API 24 构建基线链接。

## P0：Android 编译与设备基线

- [x] 确定首版 ABI：`aarch64-linux-android` 真机 + `x86_64-linux-android` 模拟器；`armv7` 暂不支持
- [x] 与 `shio-android` 确认应用 minSdk 29 并锁定（2026-10-24）
- [x] 在 CI 中加入 `shio-core`、`shio` 与 `shio-ffi` 的 Android target `cargo check`（`.github/workflows/android.yml`）
- [x] 验证 `getrandom`、`uuid`、`redb`、`tokio` 在 Android target 上编译通过（默认与 `sync` feature）
- [x] 配置 `shio-ffi` 为 `cdylib`，构建两种 ABI 的 `.so`
- [x] 检查 `.so`：ELF64、目标架构正确、`LOAD` 对齐 `0x4000`、`.note.android.ident` 为 API 24（内核构建基线；应用 minSdk 29）
- [x] x86_64 模拟器加载 `.so` 并验证 C ABI 调用
- [x] 真机加载 `.so` 并验证 JNI/FFI 调用（`oxygen` 的 vivo V2359A / arm64 已完成，见 `oxygen/.agent/work/20261023-kernel-integration-demo/handover.md`）
- [x] x86_64 模拟器冒烟：create/open、CRUD、batch、close、重开
- [x] x86_64 模拟器验证文件锁、进程被杀后重开、只读打开与完整轮换
- [x] 真机/模拟器验证轮换中断续跑（x86_64 模拟器：60k 记录轮换中 `kill -9`，重开后重跑 `rotate_data_key`，首尾记录可读并可写）
- [x] 记录 Android 构建命令、NDK/工具链版本与 x86_64 模拟器冒烟结果

## P1：unsafe 例外与 C ABI 契约

- [x] 向用户提交 `shio-ffi` 的 unsafe 例外方案：允许范围、隔离方式、审计清单、`catch_unwind`、测试策略
- [x] 获批后把 `shio-ffi` 改为受控边界：crate 级拒绝，最小范围局部允许
- [x] 定义 ABI v1：句柄、错误码、`shio_last_error`、`shio_free`、ABI 版本、线程与关闭语义
- [x] 定义 v1 最小接口：open/close、put/get/delete/list/batch、change_password、rotate_data_key、stats、flush
- [x] P1 时暂不进入 v1；P4 以增量符号追加 sync/pairing（决策 #42，ABI 仍为 1）
- [x] 编写 `shio.h` 与 Rust 常量/类型对照表
- [x] 定义空指针、长度、编码、错误映射和缓冲区所有权规则
- [x] 设计并实现 FFI 并发测试：close 与在途调用竞态、重复 close、错误详情线程局部

## P2：FFI 与 JNI 桥

- [x] 实现 `shio-ffi` v1 的 blocking open/close、CRUD、batch 与错误边界
- [x] 实现口令、RecordId、JSON 字节串和返回缓冲区的转换
- [x] FFI 内部 runtime 生命周期；`close` 等待在途调用结束
- [x] 明确 JNI 层由 `shio-android` 实现，内核只稳定 C ABI（见变更设计「Android 集成」）
- [x] Android 冒烟：Kotlin/Java 调用 CRUD、错误映射、关闭与重开（`oxygen` 的 `SmokeActivity` 已覆盖）
- [x] 处理 `shio_get` 缓冲区释放与明文清零（`shio_free` 释放前 zeroize）
- [x] `shio-ffi` 的单元测试、`clippy`、`fmt` 与 Android target 编译全部通过
- [x] 与 Android 仓库完成一次端到端联调，记录 `.so` 打包与加载方式（`oxygen` P0–P4 已完成 APK 打包、加载与双设备验收）

## P3：平台加固与验收

- [x] KDF 参数策略：保持默认 Argon2id `64 MiB、t=3`；用户确认目标手机可接受，低端机不纳入首版验证（决策 #41）
- [x] 验证 16 KB page size、minSdk、ABI 分包和 native 库打包（`oxygen`：`LOAD 0x4000`、minSdk 29、两个 ABI、APK 打包与 `zipalign -P 16` 通过）
- [x] 验证应用私有目录、自动备份排除/恢复与 `close`/`destroy` 时机（`oxygen`：`filesDir`、`allowBackup=false`、杀进程后重开）
- [ ] 验证 `flush` 时机（JNI 尚未暴露 `shio_flush`，当前密码库流程不依赖）
- [x] 验证单写多进程约束（x86_64 模拟器；Android 旁路 `.lock` + `flock`，同进程与跨进程均拒绝第二写者）
- [x] 验证独立进程只读或通过 IPC 访问（x86_64 模拟器：写者持锁时读者 `SHIO_LOCKED`；读者持共享锁时写者 `SHIO_LOCKED`、另一读者可打开）
- [x] 设备矩阵：arm64 真机、x86_64 模拟器；按需要补 armv7（`oxygen`：vivo V2359A / Android 16 / arm64 + API 37 / x86_64 / 16 KB）
- [x] 回填 `.design/api.md` 的 C ABI 契约与 `.design/SPEC.md` 的平台事实
- [x] 补充 Android 构建、打包、排错与已知限制文档（`.agent/notes.md`、变更设计「Android 集成」）

## P4：同步面（首版启用）

- [x] 决定 Android 首版启用多端同步；首次真实 `vault.db` 以 `format=2` 初始化（决策 #40）
- [x] 在 `.design/api.md` §8 与 `include/shio.h` 定义 FFI 同步契约
- [x] 在 `shio-ffi` 启用 `shio/sync`，实现 sync_enable/info/changes/apply
- [x] 实现 offers/apply_offers 与轮换 begin/ack/migrate/retire/prune/compact
- [x] 实现 snapshot_start/page/apply_snapshot
- [x] 实现 pairing_create 与 open_with_pairing，secret 用后清零
- [x] 验证配对文本、TTL、指纹、`db_id` 不匹配、游标压缩与缺 key 的错误映射（缺 key 快照返回 `SHIO_ROTATION_REQUIRED`）
- [x] `shio.h` 与 Rust 常量/函数逐项对齐，帧只以不透明字节暴露
- [x] Android 两个 ABI 构建，校验 16 KB、符号与头文件（NDK r29；18 个新符号、`LOAD 0x4000`、API 24 note）
- [x] 桌面 C ABI 冒烟完成帧往返、配对、快照与轮换（`crates/shio-ffi/tests/sync.rs`）
- [x] Android 真机/模拟器完成同一组同步冒烟（x86_64 模拟器 C harness：双库配对、双页变更、双页快照、轮换、压缩后重同步）
- [ ] `oxygen` 补 JNI/Kotlin sync 包装，M12 首次同步建库验收
- [x] 明确网络/TLS、二维码、重试和后台调度属于 `shio-android`（内核不实现）

## 完成定义

- [x] Android 两个主要 target 编译通过并进入 CI
- [x] x86_64 模拟器通过 CRUD、重开、进程被杀、文件锁与轮换测试
- [x] 真机通过 CRUD、重开、进程被杀与同进程文件锁（`oxygen` vivo V2359A；旋转由 x86_64 模拟器覆盖）
- [x] 真机/模拟器验证轮换中断续跑（与 P0/P3 同一项；x86_64 模拟器已完成）
- [x] Kotlin/Java 通过 FFI/JNI 完成首版所需操作（`oxygen` 的 `NativeShio` / `ShioDatabase` / `SmokeActivity`）
- [x] 16 KB page、minSdk 29 与打包验证有记录（`oxygen`）
- [x] 默认 KDF 参数确认：`64 MiB、t=3`，低端机不纳入首版验证（决策 #41）
- [x] 首版同步的两端帧与配对端到端通过（P4 内核验收：x86_64 模拟器双库 C ABI；`oxygen` M12 应用侧另列）
