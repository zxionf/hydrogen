# 20261022-async-facade 任务清单

- [x] 步骤 1：`Engine` 增加 `insert_raw`/`put_raw`/`get_raw`/`batch_raw` 与 `RawWriteOp`
- [x] 步骤 2：泛型 `insert`/`put`/`get`/`batch` 重构为原始接口的薄封装
- [x] 步骤 3：`EngineOptions.cache_size` 接到 redb builder
- [x] 步骤 4：facade `Inner` 生命周期（在途计数、`close` 等待、`Closed`）
- [x] 步骤 5：`Kernel::open/close/flush/stats` 与 `Namespace::collections`
- [x] 步骤 6：`Collection<T>` 七个 async 方法 + 只读闸门
- [x] 步骤 7：`crates/shio/tests/facade.rs` 端到端用例
- [x] 步骤 8：fmt / clippy / test 全绿
- [x] 步骤 9：追加决策、更新 STATE 与文档、汇报
