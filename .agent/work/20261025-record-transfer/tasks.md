# 20261025-record-transfer 任务清单

- [x] 步骤 1 设计：JSONL v1（`head`/`col`/`rec`/`end`、SHA-256、精确计数）、导出/导入语义与危险边界
- [x] 步骤 2 core 类型：`ExportOptions`/`ExportInfo`/`ImportMode`/`ImportOptions`/`ImportReport` 与批大小常量
- [x] 步骤 3 core 导出：单读事务扫描 `shio.catalog` + `shio.records`、解密与 base64、临时文件 + 原子发布、命名空间/集合过滤与覆盖开关
- [x] 步骤 4 core 校验：两遍扫描、结构/名称/UUID/值编码/值长度/精确计数/SHA-256、截断与尾后数据拒绝
- [x] 步骤 5 core 导入：LWW 合并、相等时间戳的明文比较与幂等、`replace` 的 rev/时间戳规则、墓碑压制、分批事务与集合补登记
- [x] 步骤 6 core 导入同步：format=2 时按本机 `device_id` 追加 `shio.changes`（对端可拉到），format=1 不写日志
- [x] 步骤 7 facade：`Kernel::export_records`/`import_records` 与 `ExportInfo`/`ImportMode` 等根导出
- [x] 步骤 8 C ABI：`shio_export`/`shio_import`、options JSON（未知字段拒绝）、报告 JSON、`shio.h` 声明（ABI 仍为 1）
- [x] 步骤 9 测试：core 11 项（跨 format 往返、LWW/replace、同步传播、坏文件零写入、过滤/覆盖、只读、dry-run、二进制值、超限、批大小）、FFI 端到端、facade 端到端
- [x] 步骤 10 验收：Android `transfer` 模式在 x86_64 模拟器输出 `TRANSFER_OK`，`smoke`/`sync`/`migration-progress`/`maintenance`/`cooperative`/`compact`/`auto-compact`/`clone`/`backup-restore` 回归全 0；`cargo test --workspace --all-features`、`fmt --check`、`clippy -D warnings`、`cc -fsyntax-only` 通过；arm64 真机 `10AE4N38SA000LK` 复跑 `transfer` 与 11 模式回归通过
