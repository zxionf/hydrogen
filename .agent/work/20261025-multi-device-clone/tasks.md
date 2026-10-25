# 20261025-multi-device-clone 任务清单

- [x] 步骤 1 设计：克隆与 `restore` 的语义边界（同库新设备 vs 同设备恢复）、保留/重置键清单、按新身份重建完整日志
- [x] 步骤 2 core 类型：`CloneInfo`（格式、字节数、`db_id`、前后 `device_id`、`peer_count`、`log_entries`）
- [x] 步骤 3 core 入口：`Engine::clone_backup`（目标不存在或 0 字节、同目录临时文件、原子改名、`create`/`read_only` 忽略）
- [x] 步骤 4 core 身份重置：`reset_clone_identity`（新随机非零 `device_id`、刷新 `enabled_at`、`log_start=1`、清 `peer/`/`cur/`/`heard/`/`migration`/`baseline_key`、清空 `shio.changes`）
- [x] 步骤 5 core 日志重建：复用 `run_baseline` 重发全部记录与 `CurrentKey`、写 `cur/<新设备>` 并置 `baseline_done`
- [x] 步骤 6 C ABI：`shio_clone_backup`（返回句柄 + 摘要 JSON）与 `shio.h` 声明，`SHIO_ABI_VERSION` 保持 1
- [x] 步骤 7 facade：`shio::clone_backup`、`CloneInfo` 导出、目标 `.lock` 独占
- [x] 步骤 8 测试：core（身份/日志/`peer` 清理/新配对设备只靠日志拿到全量/旧设备增量/非同步库/错误路径）、FFI 端到端、facade 端到端
- [x] 步骤 9 Android harness：`clone` 模式（源库启用同步并写 3 条 → 备份 → 克隆 → 校验身份、日志 `1..4`、记录可读）
- [x] 步骤 10 文档与验收：`api.md` §3/§4/§8/§9/§12、`data.md` §15.5、`notes.md`、`阶段计划.md`、`STATE.md`、决策 #58；x86_64 模拟器 `CLONE_OK`，`smoke`/`sync`/`migration-progress`/`maintenance`/`cooperative`/`compact`/`auto-compact`/`backup-restore` 回归通过；`cargo test --workspace --all-features`、`fmt --check`、`clippy -D warnings`、`cc -fsyntax-only` 通过（arm64 真机当轮未连接，待补跑）
