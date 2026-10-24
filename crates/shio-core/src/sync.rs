//! 多端同步的公开数据类型。
//!
//! 这些类型是 `20261022-sync-design` 的公开面；字节编码在 [`crate::store`] 与
//! `shio-sync` 中实现。这里的结构不包含任何密钥材料。

use std::fmt;
use std::time::Duration;

use crate::error::{Error, Result};
use crate::model::{DeviceId, KeyId, RecordId, Timestamp};

/// 单帧默认上限：4 MiB。
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// 单帧默认最大条目数。
pub const MAX_FRAME_ENTRIES: usize = 1000;

/// 单次日志/快照拉取默认上限。
pub const MAX_PAGE_ENTRIES: usize = 1000;

/// 配对包默认有效期。
pub const DEFAULT_PAIRING_TTL: Duration = Duration::from_secs(10 * 60);

/// 单库 key 行上限。
pub const MAX_KEY_COUNT: usize = 32;

/// 迁移每批默认记录数。
pub const DEFAULT_MIGRATION_BATCH_SIZE: usize = 256;

/// 迁移每批允许的最大记录数。
pub const MAX_MIGRATION_BATCH_SIZE: usize = 1024;

/// 批次间暂停允许的最大毫秒数。
pub const MAX_MIGRATION_PAUSE_MS: u64 = 60_000;

/// `format=2` 数据密钥迁移参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationOptions {
    /// 每个写事务迁移的记录数。
    pub batch_size: usize,
    /// 每个批次提交后的暂停时间；`Duration::ZERO` 表示不暂停。
    pub pause: Duration,
}

impl Default for MigrationOptions {
    fn default() -> Self {
        Self {
            batch_size: DEFAULT_MIGRATION_BATCH_SIZE,
            pause: Duration::ZERO,
        }
    }
}

impl MigrationOptions {
    /// 校验参数范围；非法值返回 [`Error::InvalidArgument`]。
    pub fn validate(&self) -> Result<()> {
        if self.batch_size == 0 || self.batch_size > MAX_MIGRATION_BATCH_SIZE {
            return Err(Error::InvalidArgument("batch_size must be within 1..=1024"));
        }
        if self.pause.as_millis() > u128::from(MAX_MIGRATION_PAUSE_MS) {
            return Err(Error::InvalidArgument("pause_ms must be within 0..=60000"));
        }
        Ok(())
    }
}

/// 迁移进度快照；查询时不获取会话锁。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MigrationProgress {
    /// 是否有迁移正在执行。
    pub active: bool,
    /// 本次调用已扫描的记录数，包含无需迁移的记录。
    pub scanned: u64,
    /// 本次已实际重新加密并提交的记录数。
    pub migrated: u64,
    /// 最近一次迁移是否完整成功。
    pub done: bool,
}

/// 同步库摘要。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncInfo {
    /// 逻辑数据库 ID。
    pub db_id: [u8; 16],
    /// 本设备 ID。
    pub device_id: DeviceId,
    /// 当前写 key。
    pub current_key_id: KeyId,
    /// 当前保留的最早日志序号。
    pub log_start: u64,
    /// 当前日志末尾；空日志为 0。
    pub log_end: u64,
    /// 首次基线是否完成。
    pub baseline_done: bool,
}

/// 一页变更日志。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangePage {
    /// 发送方设备。
    pub sender: DeviceId,
    /// 本页游标起点。
    pub after: u64,
    /// 下一游标。
    pub next: u64,
    /// 是否还有更多。
    pub has_more: bool,
    /// 发送方日志起点。
    pub log_start: u64,
    /// 发送方日志终点。
    pub log_end: u64,
    /// 本页条目。
    pub entries: Vec<ChangeEntry>,
}

/// 一条变更。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChangeEntry {
    /// 记录写入或墓碑。
    Record(RecordChange),
    /// 轮换 offer。
    RotationOffer(RotationOffer),
    /// 轮换 ack。
    RotationAck {
        /// 目标 key。
        to: KeyId,
        /// ack 设备。
        device: DeviceId,
        /// 时间。
        at: Timestamp,
    },
    /// 当前写 key 公告。
    CurrentKey {
        /// 公告的 key。
        key_id: KeyId,
        /// 时间。
        at: Timestamp,
    },
    /// 迁移完成公告。
    Migrated {
        /// 旧 key。
        key_id: KeyId,
        /// 时间。
        at: Timestamp,
    },
}

/// 记录变更。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordChange {
    /// 命名空间。
    pub namespace: String,
    /// 集合。
    pub collection: String,
    /// 记录 ID。
    pub id: RecordId,
    /// 加密该记录的 key。
    pub key_id: KeyId,
    /// 修订号。
    pub rev: u64,
    /// 更新时间。
    pub updated_at: Timestamp,
    /// 是否墓碑。
    pub deleted: bool,
    /// 最初写入设备。
    pub writer: DeviceId,
    /// 原样转发的 `RecordBlob` v2 字节。
    pub blob: Vec<u8>,
}

/// 轮换 offer。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotationOffer {
    /// 起始 key。
    pub from: KeyId,
    /// 新 key。
    pub to: KeyId,
    /// 创建时间。
    pub created_at: Timestamp,
    /// 发起设备。
    pub origin: DeviceId,
    /// 包裹 nonce。
    pub wrap_nonce: [u8; 24],
    /// 用 from_DK 派生的 RK 包裹的 DK_to。
    pub wrapped_dk: Vec<u8>,
}

/// 一页轮换 offer。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferPage {
    /// 发送方设备。
    pub sender: DeviceId,
    /// offer 列表，按创建时间升序。
    pub offers: Vec<RotationOffer>,
}

/// 轮换状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotationInfo {
    /// 起始 key。
    pub from: KeyId,
    /// 新 key。
    pub to: KeyId,
    /// 创建时间。
    pub created_at: Timestamp,
    /// 已 ack 的设备。
    pub acked: Vec<DeviceId>,
}

/// 应用一页后的统计。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApplyReport {
    /// 实际覆盖的记录数。
    pub applied: usize,
    /// 因 LWW 或重复跳过的记录数。
    pub skipped: usize,
    /// 应用的控制条目数。
    pub controls: usize,
}

/// 一页快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotPage {
    /// 快照对应的日志位置。
    pub at_seq: u64,
    /// 下一页游标；`None` 表示结束。
    pub next_key: Option<Vec<u8>>,
    /// 记录条目。
    pub entries: Vec<SnapshotRecord>,
}

/// 快照中的一条记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRecord {
    /// 命名空间。
    pub namespace: String,
    /// 集合。
    pub collection: String,
    /// 记录 ID。
    pub id: RecordId,
    /// 原始 `RecordBlob` v2 字节。
    pub blob: Vec<u8>,
}

/// 配对 offer。
#[derive(Clone, PartialEq, Eq)]
pub struct PairingOffer {
    /// 外层 `SHW2` 帧。
    pub frame: Vec<u8>,
    /// Base32（Crockford）配对 secret；调用方负责移入 `Zeroizing`。
    pub secret: String,
    /// 外层帧 SHA-256 前 3 字节的 6 位十六进制。
    pub fingerprint: String,
}

impl fmt::Debug for PairingOffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairingOffer")
            .field("frame_len", &self.frame.len())
            .field("secret", &"<redacted>")
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

/// 计算配对包指纹：外层帧 SHA-256 的前 3 字节，小写十六进制。
pub fn pairing_fingerprint(frame: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(frame);
    let mut out = String::with_capacity(6);
    for byte in &digest[..3] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_offer_debug_redacts_secret() {
        let offer = PairingOffer {
            frame: vec![1, 2, 3],
            secret: "SECRET-TEXT".to_owned(),
            fingerprint: "abcdef".to_owned(),
        };
        let text = format!("{offer:?}");
        assert!(text.contains("redacted"));
        assert!(!text.contains("SECRET-TEXT"));
    }
}
