//! shio 的多端同步。
//!
//! 后续以「追加变更日志」的方式接入：新增 `shio.changes` 表，不修改
//! `format=1` 的现有表结构与记录格式。UUIDv7、`rev` 与墓碑已经为同步预留了
//! 比较与删除传播所需的事实。
//!
//! 当前不提供任何类型或函数，只固定依赖方向：本 crate 只依赖 `shio-core`，
//! 不触及 facade 与 FFI。

#![forbid(unsafe_code)]
