# 20261022-android-kernel 任务清单

## P0：Android 编译与设备基线

- [x] 确定首版 ABI：`aarch64-linux-android` 真机 + `x86_64-linux-android` 模拟器；`armv7` 暂不支持
- [x] 与 `shio-android` 确认 minSdk 24 并锁定
- [x] 在 CI 中加入 `shio-core`、`shio` 与 `shio-ffi` 的 Android target `cargo check`（`.github/workflows/android.yml`）
- [x] 验证 `getrandom`、`uuid`、`redb`、`tokio` 在 Android target 上编译通过（默认与 `sync` feature）
- [x] 配置 `shio-ffi` 为 `cdylib`，构建两种 ABI 的 `.so`
- [x] 检查 `.so`：ELF64、目标架构正确、`LOAD` 对齐 `0x4000`、`.note.android.ident` 为 API 24
- [x] x86_64 模拟器加载 `.so` 并验证 C ABI 调用
- [ ] 真机加载 `.so` 并验证 JNI/FFI 调用（与 P2 冒烟合并）
- [x] x86_64 模拟器冒烟：create/open、CRUD、batch、close、重开
- [x] x86_64 模拟器验证文件锁、进程被杀后重开、只读打开与完整轮换
- [ ] 真机/模拟器验证轮换中断续跑
- [x] 记录 Android 构建命令、NDK/工具链版本与 x86_64 模拟器冒烟结果

## P1：unsafe 例外与 C ABI 契约

- [x] 向用户提交 `shio-ffi` 的 unsafe 例外方案：允许范围、隔离方式、审计清单、`catch_unwind`、测试策略
- [x] 获批后把 `shio-ffi` 改为受控边界：crate 级拒绝，最小范围局部允许
- [x] 定义 ABI v1：句柄、错误码、`shio_last_error`、`shio_free`、ABI 版本、线程与关闭语义
- [x] 定义 v1 最小接口：open/close、put/get/delete/list/batch、change_password、rotate_data_key、stats、flush
- [x] 决定 sync/pairing 不进入 v1；Android 首版需要时另补 `.design/api.md` §8
- [x] 编写 `shio.h` 与 Rust 常量/类型对照表
- [x] 定义空指针、长度、编码、错误映射和缓冲区所有权规则
- [x] 设计并实现 FFI 并发测试：close 与在途调用竞态、重复 close、错误详情线程局部

## P2：FFI 与 JNI 桥

- [x] 实现 `shio-ffi` v1 的 blocking open/close、CRUD、batch 与错误边界
- [x] 实现口令、RecordId、JSON 字节串和返回缓冲区的转换
- [x] FFI 内部 runtime 生命周期；`close` 等待在途调用结束
- [x] 明确 JNI 层由 `shio-android` 实现，内核只稳定 C ABI（见变更设计「Android 集成」）
- [ ] Android 冒烟：Kotlin/Java 调用 CRUD、错误映射、关闭与重开
- [x] 处理 `shio_get` 缓冲区释放与明文清零（`shio_free` 释放前 zeroize）
- [x] `shio-ffi` 的单元测试、`clippy`、`fmt` 与 Android target 编译全部通过
- [ ] 与 Android 仓库完成一次端到端联调，记录 `.so` 打包与加载方式

## P3：平台加固与验收

- [ ] 在中低端 arm64 设备上实测默认 Argon2id 参数（64 MiB、t=3）的耗时与内存
- [ ] 确定建库 KDF 参数策略；若需要修改已有库参数，另开变更设计迁移 API
- [ ] 验证 16 KB page size、minSdk、ABI 分包和 native 库打包
- [ ] 验证应用私有目录、自动备份排除/恢复、`flush`/`close` 时机
- [x] 验证单写多进程约束（x86_64 模拟器；Android 旁路 `.lock` + `flock`，同进程与跨进程均拒绝第二写者）
- [ ] 验证独立进程只读或通过 IPC 访问
- [ ] 设备矩阵：arm64 真机、x86_64 模拟器；按需要补 armv7
- [x] 回填 `.design/api.md` 的 C ABI 契约与 `.design/SPEC.md` 的平台事实
- [x] 补充 Android 构建、打包、排错与已知限制文档（`.agent/notes.md`、变更设计「Android 集成」）

## P4：同步面（按首版需求决定）

- [ ] 决定 Android 首版是否启用多端同步
- [ ] 若启用，定义 FFI 的 sync_info、changes、snapshot、offer/ack、pairing 接口
- [ ] 暴露帧 encode/decode/apply，不让 Kotlin 解析内部 Rust 类型
- [ ] 验证配对文本、TTL、指纹错误映射与二维码输入边界
- [ ] 桌面与 Android 两端完成帧往返、配对、快照与轮换冒烟
- [ ] 明确网络/TLS、二维码、重试和后台调度属于 `shio-android`

## 完成定义

- [x] Android 两个主要 target 编译通过并进入 CI
- [x] x86_64 模拟器通过 CRUD、重开、进程被杀、文件锁与轮换测试
- [ ] 真机通过 CRUD、重开、进程被杀、文件锁与轮换测试
- [ ] Kotlin/Java 通过 FFI/JNI 完成首版所需操作
- [ ] 低端机 KDF、16 KB page、minSdk 与打包验证有记录
- [ ] 若首版带同步，两端帧与配对端到端通过
