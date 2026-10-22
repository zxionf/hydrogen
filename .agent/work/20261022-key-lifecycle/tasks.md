# 20261022-key-lifecycle 任务清单

- [x] 步骤 1：`Error::Busy` 与 `EngineOptions.read_only`
- [x] 步骤 2：`Engine` 会话化（`Session`/`KeyEntry`/多密钥解锁）
- [x] 步骤 3：`Handle` 枚举：读写库与只读库共用读事务
- [x] 步骤 4：`change_password`（单行重包裹、`Busy`）
- [x] 步骤 5：`rotate_data_key`（新 key 行、分批重加密、meta 切换、删旧行）
- [x] 步骤 6：facade 两个方法与只读透传
- [x] 步骤 7：core 用例：只读、改口令、轮换、轮换后改口令
- [x] 步骤 8：facade 用例：改口令/轮换/只读
- [x] 步骤 9：fmt / clippy / test 全绿
- [x] 步骤 10：追加决策、更新 STATE 与文档、折叠关闭
