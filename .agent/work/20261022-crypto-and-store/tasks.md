# 20261022-crypto-and-store 任务清单

- [x] 步骤 1：`crypto::kdf` 补 Argon2id 派生与 HKDF 子密钥
- [x] 步骤 2：`crypto::aead` 补 XChaCha20-Poly1305 封装/解封
- [x] 步骤 3：`store::meta`/`store::keys`/`store::catalog` 的 blob 编解码
- [x] 步骤 4：`store::engine` 打开/创建与密钥解锁
- [x] 步骤 5：记录写入、读取、删除、清除
- [x] 步骤 6：列表（游标 + 分页 + 墓碑）、集合清单、stats
- [x] 步骤 7：批量写（单事务、原子回滚）
- [x] 步骤 8：`crates/shio-core/tests/engine.rs` 端到端用例（临时文件）
- [x] 步骤 9：fmt / clippy / test 全绿
- [x] 步骤 10：追加决策、更新 STATE 与文档、汇报
