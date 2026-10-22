# 20261022-rust-kernel-skeleton 任务清单

- [x] 步骤 1：根 workspace 清单、`rust-toolchain.toml`、`rustfmt.toml`、`.gitignore` 加 `/target`
- [x] 步骤 2：`crates/shio-core` 模块树与依赖（model/error/codec/crypto/store）
- [x] 步骤 3：model 纯逻辑（RecordId、Timestamp、名称校验、RecordMeta/Stored/ListOptions/Page/WriteOp）
- [x] 步骤 4：error 全量枚举与 Display
- [x] 步骤 5：codec 字节契约（键、header、blob 视图、三类 AAD）+ 单测
- [x] 步骤 6：crypto 类型与零化包装；store 四张表定义
- [x] 步骤 7：`crates/shio` facade 类型与纯校验；`shio-ffi`/`shio-sync` 占位
- [x] 步骤 8：根 `tests/` 集成测试包与冒烟用例
- [x] 步骤 9：`cargo fmt --check`、`cargo clippy`、`cargo test` 全绿
- [x] 步骤 10：追加设计决策、`sele lint` 通过、汇报
