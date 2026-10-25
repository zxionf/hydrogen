# 20261025-file-compaction 任务清单

- [x] 步骤 1 core：`CompactReport`、`Engine::compact_file`（`metadata` → `Database::open` → `compact` → `check_integrity` → 前后大小）
- [x] 步骤 2 core 错误映射：`From<redb::CompactionError>`（`TransactionInProgress`/`SavepointExists` → `Busy`），`DatabaseAlreadyOpen` → `Locked`，`NotFound`/`InvalidArgument` 前置判定
- [x] 步骤 3 core 测试：删除大量记录后压缩变小、连续压缩 `compacted=false`、记录与 key 不变、句柄未关 `Locked`、路径不存在 `NotFound`、0 字节 `InvalidArgument`
- [x] 步骤 4 facade：`shio::compact_file(PathBuf)` 先取 `.lock` 独占再 `spawn_blocking`；导出 `CompactReport`
- [x] 步骤 5 facade 测试：句柄未关 `Locked`、关闭后压缩成功、重开记录可读、缺失路径 `NotFound`
- [x] 步骤 6 C ABI：`shio_compact_file(path, out_json, out_json_len)` 与 `shio.h` 声明，返回 `{"before_bytes","after_bytes","compacted"}`
- [x] 步骤 7 C ABI 测试：句柄未关 `SHIO_LOCKED` 且无缓冲区、压缩摘要、重开读取、缺失路径 `SHIO_NOT_FOUND`
- [x] 步骤 8 文档：`api.md` §3/§4/§8/§9、`notes.md` 用法/踩坑/导出清单、`阶段计划.md` 状态、`STATE.md`、决策 #56
- [x] 步骤 9 Android 冒烟：`android_smoke.c` 增加 `compact` 模式（86016 → 57344、连续压缩 `compacted=0`、重开校验）；x86_64 模拟器与 arm64 真机均输出 `COMPACT_OK`，同轮回归 `smoke`/`sync`/`migration-progress`/`maintenance`/`cooperative`/`backup-restore` 通过
- [x] 步骤 10 验收：`cargo test --workspace --all-features`、`cargo fmt --check`、`cargo clippy --all-features -D warnings`、`cc -fsyntax-only` 头文件检查
