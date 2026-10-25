# 20261025-cooperative-scheduling 任务清单

- [x] 步骤 1 core：新增 `DEFAULT_TICK_BUDGET_MS`/`MAX_TICK_BUDGET_MS`/`DEFAULT_TICK_BATCH_SIZE`/`MAX_TICK_BATCH_SIZE` 并从 `shio-core`、`shio-sync`、`shio` 重导出
- [x] 步骤 2 core：`MigrationState::reset_counters`，`Engine::migrate_tick` + `run_tick`（预算内多批、每批单独提交、预算检查在批后、短批收尾、取消检查）
- [x] 步骤 3 core 测试：切片推进、tick 之间读写不受限、进度累计与序列开始清零、未启用同步/只读/预算越界/批次越界/空 key 拒绝
- [x] 步骤 4 facade：`Kernel::migrate_tick`（async + `spawn_blocking` 写路径）与 `SyncHandle::migrate_tick` 同步版本
- [x] 步骤 5 facade 测试：不进入维护模式的 tick 循环、tick 之间写入并读回、`done` 后记录可读
- [x] 步骤 6 C ABI：`shio_sync_migrate_tick(handle, to, budget_ms, max_records, out_json, out_json_len)` 与 `shio.h` 声明
- [x] 步骤 7 C ABI 测试：tick 循环、tick 之间 `shio_insert`、进度 `done`、`budget_ms=1001`/`max_records=257`/空 key 返回 `SHIO_INVALID_ARGUMENT`
- [x] 步骤 8 文档：`api.md` §4/§8/§12、`notes.md` 用法与踩坑、`阶段计划.md` 状态、`STATE.md`、决策 #54/#55
- [x] 步骤 9 Android 冒烟：`android_smoke.c` 增加 `cooperative` 模式；x86_64 模拟器（Android 17）与 arm64 真机（Android 16）均输出 `COOPERATIVE_OK`，同轮回归 `smoke`/`sync`/`migration-progress`/`backup-restore` 通过
- [x] 步骤 10 验收：`cargo test --workspace --all-features`、`cargo fmt --check`、`cargo clippy --all-features -D warnings`、`cc -fsyntax-only` 头文件检查
