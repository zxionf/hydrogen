# 20261025-forget-device 任务清单

- [x] 步骤 1 设计：解绑语义（删除键清单、拒绝本机、幂等）、设备回来后的重同步行为、错误顺序
- [x] 步骤 2 core 类型：`ForgetReport`（`device`、三个 bool、`acks`/`migrations` 计数）与核心根导出
- [x] 步骤 3 core 实现：单写事务删除 `peer/`/`cur/`/`heard/`，扫描 `ack/*` 与 `mig/*` 按尾部 16 字节匹配；`ReadOnly`/`Busy`/`SyncDisabled`/本机拒绝
- [x] 步骤 4 facade：`Kernel::forget_device`（写路径）与 `ForgetReport` 重导出
- [x] 步骤 5 C ABI：`shio_sync_forget_device`、摘要 JSON 与 `shio.h` 声明（ABI 仍为 1）
- [x] 步骤 6 core 测试：僵尸设备阻塞自动裁剪 → 解绑后继续；轮换 ack/mig 清理后 `retire_key` 成功；本机/未知/幂等/只读/维护/未启用同步
- [x] 步骤 7 FFI 测试：摘要字段、本机拒绝、未知设备全零
- [x] 步骤 8 facade 测试：配对 → 进度上报 → 解绑 → 幂等与本机拒绝
- [x] 步骤 9 Android harness：`forget` 模式（A/B/C，裁剪先 0 后非 0），x86_64 模拟器通过；同轮 10 个既有模式回归全 0
- [x] 步骤 10 发布准备与验收：workspace 版本 `0.2.0` → `0.3.0`（`SPEC.md` 同步）；`cargo test --workspace --all-features`、`fmt --check`、`clippy -D warnings`、`cc -fsyntax-only` 通过；arm64 真机 `10AE4N38SA000LK` 复跑 `forget` 与 11 模式回归通过
