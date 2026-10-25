# 20261025-plaintext-tool 任务清单

- [x] 步骤 1 设计：独立 CLI 边界、四个子命令、口令来源、危险闸门与退出码
- [x] 步骤 2 core：把文件扫描提炼为 `verify_export(path, max_value_size)` 并复用导入校验，导出到 core 根与 `shio` 根
- [x] 步骤 3 crate 骨架：`crates/shio-tool`（bin、`rpassword`/`serde_json`/`tokio`/`zeroize`、workspace lints）
- [x] 步骤 4 参数解析：`export`/`import`/`verify`/`stats`、`-h/--help`、`-V/--version`、未知选项与缺参数退出码 2
- [x] 步骤 5 口令：`--password-stdin`、`SHIO_PASSWORD`、终端不回显；`Zeroizing` 搬运，不接受 argv
- [x] 步骤 6 export：只读打开库、stderr 明文警告、`--out`/`--namespace`/`--collection`/`--with-deleted`/`--force`
- [x] 步骤 7 import：`--yes` 闸门、`--dry-run` 只读、`--replace`、`--batch`
- [x] 步骤 8 verify/stats：不打开数据库的完整校验；`stats` 按 `(namespace, collection)` 汇总记录与墓碑
- [x] 步骤 9 测试：core `verify_export` 1 项（合法/截断/超限/缺失）；CLI 4 项（往返与幂等、覆盖与 replace/batch、口令与用法、截断与篡改）
- [x] 步骤 10 文档与验收：`api.md`、`notes.md`、`阶段计划.md`、`STATE.md`、决策 #60；`cargo test --workspace --all-features`、`fmt --check`、`clippy -D warnings` 通过
