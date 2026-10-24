//! shio 同步内核。
//!
//! 本 crate 不依赖 tokio，也不做任何异步调度：所有函数都是同步的，由上层
//! [`shio`] facade 通过阻塞线程池调用。数据模型是「命名空间 → 集合 → 记录」，
//! 记录体加密后落在 redb 单文件里。
//!
//! 当前形态：
//!
//! - [`model`]、[`error`]、[`codec`] 是完整实现，包含 `data.md` 定义的字节契约；
//! - [`crypto`] 提供 Argon2id 密钥派生、HKDF 子密钥与 XChaCha20-Poly1305；
//! - [`store::Engine`] 提供创建/打开、同步 CRUD、原子批量、改口令与数据密钥轮换。
//!
//! 约束：本 crate 禁止 `unsafe`；非测试代码不使用 `unwrap` / `expect` / `panic!`；
//! 来自磁盘的字节一律先校验再使用。

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod codec;
pub mod codec2;
pub mod crypto;
pub mod error;
pub mod model;
pub mod store;
pub mod sync;

pub use error::{Error, Result};
pub use model::{DeviceId, KeyId};
pub use store::engine::{BackupInfo, Engine, EngineOptions, RawWriteOp, Stats};
pub use sync::{
    ApplyReport, ChangeEntry, ChangePage, MigrationOptions, MigrationProgress, OfferPage,
    PairingOffer, RecordChange, RotationInfo, RotationOffer, SnapshotPage, SnapshotRecord,
    SyncInfo, DEFAULT_MIGRATION_BATCH_SIZE, MAX_MIGRATION_BATCH_SIZE, MAX_MIGRATION_PAUSE_MS,
};

/// 内核版本，与 crate 包版本一致（编译期常量）。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_package_version() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }
}
