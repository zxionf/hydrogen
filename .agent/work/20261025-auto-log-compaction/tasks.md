# 20261025-auto-log-compaction 任务清单

- [x] 步骤 1 设计：`SHY2` kind=3 进度帧、`heard/<device>`、全对端报告后才裁剪的规则与保留最新一条的约束
- [x] 步骤 2 core 存储键：`sync_state::heard(device)` / `heard_prefix()`
- [x] 步骤 3 core 读取与写入：`peer_cursor`、`apply_peer_progress`（单调推进、按 `log_end` 截断、维护模式 `Busy`）
- [x] 步骤 4 core 自动裁剪：`compact_changes_auto`（`cur/` ∪ `heard/` 排除本机、缺报告返回 0、裁剪到 `min+1` 且保留最新一条）
- [x] 步骤 5 shio-sync：`frame::encode_progress` / `decode_progress`、`parse_frame` kind 白名单加入 3
- [x] 步骤 6 shio-sync API：`Sync` 与 `SyncHandle` 的 `progress_frame` / `apply_progress` / `compact_changes_auto`
- [x] 步骤 7 C ABI：`shio_sync_progress_frame`、`shio_sync_apply_progress`、`shio_sync_compact_auto` 与 `shio.h` 声明（ABI 仍为 1）
- [x] 步骤 8 facade：`Kernel::sync_progress_frame` / `apply_sync_progress` / `compact_changes_auto`
- [x] 步骤 9 测试：core（门控、单调、截断、`SyncResyncRequired`、维护模式、未启用同步）、shio-sync（帧往返/`db_id`/kind 负例、裁剪后增量）、FFI 端到端、facade 端到端
- [x] 步骤 10 文档与验收：`api.md` §4/§8/§9/§12、`data.md` §15.5、`notes.md`、`阶段计划.md`、`STATE.md`、决策 #57；Android `auto-compact` 双设备通过，`smoke`/`sync`/`migration-progress`/`maintenance`/`cooperative`/`compact`/`backup-restore` 回归通过；`cargo test --workspace --all-features`、`fmt --check`、`clippy -D warnings`、`cc -fsyntax-only` 通过
