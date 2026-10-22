# Agent State

Active Change: 无
Phase: facade 可用：Kernel::open/close/flush/stats、Namespace::collections、Collection<T> 七个 async 方法；change_password、rotate_data_key 与真正的只读打开未实现
Blocked: 无
Next: 开变更补 change_password（重新包裹 DK）与 rotate_data_key（生成新 key_id、重加密全部记录），以及用 redb 只读库实现真正的 read_only
