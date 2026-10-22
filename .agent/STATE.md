# Agent State

Active Change: 无
Phase: core 可用：Argon2id/HKDF/AEAD 与四张 redb 表已接线，同步 CRUD/列表/批量/集合清单有端到端用例；facade 尚未包装
Blocked: 无
Next: 开变更把 facade 的 Kernel/Namespace/Collection 包到 Engine 上（open/insert/get/list/batch + async），之后再补 change_password、rotate_data_key 与 read_only
