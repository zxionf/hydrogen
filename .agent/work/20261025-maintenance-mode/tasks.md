# 20261025-maintenance-mode 任务清单

- [x] 步骤 1 core：`MigrationState` 增 `reset_for_maintenance/begin_step/end_step`，`require_writable` 增加维护闸门与 `require_writable_internal` 放行
- [x] 步骤 2 core：`migrate_step(to, max_records)` 复用游标、批内持锁、收尾与 `done`；迁移循环拆成 `read_migration_batch`/`migrate_batch`/`finalize_migration`
- [x] 步骤 3 core 测试：写拒绝 `Busy`、读放行、步进续跑、中途退出、重开续跑、`SyncEnabled` 优先、未启用同步/只读拒绝、参数越界
- [x] 步骤 4 facade：`Kernel::begin_maintenance/end_maintenance/migrate_step`（async + spawn_blocking）与 `SyncHandle` 同步版本
- [x] 步骤 5 facade 测试：维护期写 `Busy`、读放行、步进到 `done`、退出返回值、写恢复
- [x] 步骤 6 C ABI：`shio_sync_begin_maintenance`、`shio_sync_end_maintenance`、`shio_sync_migrate_step` 与 `shio.h`
- [x] 步骤 7 C ABI 测试：维护期写返回 `SHIO_BUSY`、步进 JSON、`out_done`、未启用同步与参数越界
- [x] 步骤 8 文档：`api.md` §4/§8/§12、`notes.md` 导出清单与踩坑、`阶段计划.md` 状态、`STATE.md`
- [x] 步骤 9 Android 冒烟：`android_smoke.c` 增加 `maintenance` 模式；x86_64 模拟器（Android 17）与 arm64 真机（Android 16）均输出 `MAINTENANCE_OK`，同轮回归 `smoke`/`sync`/`migration-progress`/`backup-restore` 通过
- [x] 步骤 10 验收：`cargo test --workspace --all-features`、`cargo fmt --check`、`cargo clippy --all-features -D warnings`、`cc -fsyntax-only` 头文件检查
