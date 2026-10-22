//! shio 同步内核。
//!
//! 本 crate 不依赖 tokio，也不做任何异步调度：所有函数都是同步的，由上层
//! [`shio`] facade 通过阻塞线程池调用。数据模型是「命名空间 → 集合 → 记录」，
//! 记录体加密后落在 redb 单文件里。
//!
//! 当前阶段的边界：
//!
//! - [`model`]、[`error`]、[`codec`] 是完整实现，包含 `data.md` 定义的字节契约；
//! - [`crypto`] 只有密钥材料类型，KDF 与 AEAD 运算尚未接线；
//! - [`store`] 只有四张表的定义，读写事务尚未接线。
//!
//! 约束：本 crate 禁止 `unsafe`；非测试代码不使用 `unwrap` / `expect` / `panic!`；
//! 来自磁盘的字节一律先校验再使用。

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod codec;
pub mod crypto;
pub mod error;
pub mod model;
pub mod store;

pub use error::{Error, Result};
pub use store::engine::{Engine, EngineOptions, Stats};
