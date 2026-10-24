//! shio 的 async facade。
//!
//! 这一层负责三件事：管理内核生命周期、把同步调用搬到阻塞线程池、向 Rust 应用
//! 暴露 `Kernel` / `Namespace` / `Collection<T>` 句柄：
//!
//! - `Kernel::open` 完成参数与名称校验、口令派生、DK 解包与 key check；
//! - 记录读写经 `spawn_blocking` 调用 `shio_core::store::Engine`；
//! - `close` 标记关闭并等待进行中的操作结束。
//!
//! 本 crate 禁止 `unsafe`，也不直接实现密码学或存储逻辑。

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod collection;
pub mod kernel;
mod lock;

pub use collection::Collection;
#[cfg(feature = "sync")]
pub use kernel::open_with_pairing;
pub use kernel::{Credential, Kernel, Namespace, OpenOptions};

pub use shio_core::error::{Error, Result};
pub use shio_core::model::{
    validate_name, ListOptions, NameKind, Page, RecordId, RecordMeta, Stored, Timestamp, WriteOp,
    DEFAULT_LIST_LIMIT, DEFAULT_MAX_VALUE_SIZE, MAX_LIST_LIMIT, MAX_NAME_LEN,
};
pub use shio_core::store::{BackupInfo, Stats};
#[cfg(feature = "sync")]
pub use shio_core::sync::pairing_fingerprint;
#[cfg(feature = "sync")]
pub use shio_core::{DeviceId, KeyId};
#[cfg(feature = "sync")]
pub use shio_sync::{
    ApplyReport, ChangeEntry, ChangePage, MigrationOptions, MigrationProgress, OfferPage,
    PairingOffer, RotationInfo, RotationOffer, SnapshotIter, SnapshotPage, SnapshotRecord,
    SyncHandle, SyncInfo, DEFAULT_MIGRATION_BATCH_SIZE, MAX_MIGRATION_BATCH_SIZE,
    MAX_MIGRATION_PAUSE_MS,
};

/// 查询内核版本。
///
/// 与 `shio-core` 的包版本一致，是编译期常量；不需要口令、不打开数据库，
/// 句柄关闭后仍可调用。
pub fn version() -> &'static str {
    shio_core::VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_package_version() {
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
        assert!(!version().is_empty());
    }
}
