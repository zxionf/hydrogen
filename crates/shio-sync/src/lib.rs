//! `shio` 的多端同步传输层。
//!
//! 本 crate 只依赖 `shio-core`，不引入网络、TLS 或后台线程。它负责：
//!
//! - 把 `Engine` 的变更页/offer 页/快照页编码成 `SHY2` 帧；
//! - 在接收端校验 `db_id` 后整页交给 `Engine`；
//! - 配对 secret 的 Crockford Base32 文本表示；
//! - 拥有型 `SyncHandle`，供 facade 在 `spawn_blocking` 中调用。

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod base32;
pub mod frame;

use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;

pub use base32::{decode_secret as decode_pairing_secret, encode_secret};
pub use shio_core::sync::{
    ApplyReport, ChangeEntry, ChangePage, MaintenanceOutcome, MigrationOptions, MigrationProgress,
    MigrationStep, OfferPage, PairingOffer, RotationInfo, RotationOffer, SnapshotPage,
    SnapshotRecord, SyncInfo, DEFAULT_MIGRATION_BATCH_SIZE, DEFAULT_TICK_BATCH_SIZE,
    DEFAULT_TICK_BUDGET_MS, MAX_MIGRATION_BATCH_SIZE, MAX_MIGRATION_PAUSE_MS, MAX_TICK_BATCH_SIZE,
    MAX_TICK_BUDGET_MS,
};
pub use shio_core::{DeviceId, Engine, EngineOptions, Error, KeyId, Result};

use frame::validate_record_change;

/// 借用 `Engine` 的同步包装。
#[derive(Debug, Clone, Copy)]
pub struct Sync<'a> {
    engine: &'a Engine,
}

impl<'a> Sync<'a> {
    pub fn new(engine: &'a Engine) -> Self {
        Self { engine }
    }

    pub fn enable_sync(&self, device_id: Option<DeviceId>) -> Result<SyncInfo> {
        self.engine.enable_sync(device_id)
    }

    pub fn info(&self) -> Result<SyncInfo> {
        self.engine.sync_info()
    }

    pub fn changes_since(&self, after: u64, limit: usize) -> Result<ChangePage> {
        self.engine.changes_since(after, limit)
    }

    pub fn encode(&self, page: &ChangePage) -> Result<Vec<u8>> {
        let info = self.info()?;
        frame::encode_change_page(info.db_id, page)
    }

    pub fn decode(&self, frame: &[u8]) -> Result<ChangePage> {
        let info = self.info()?;
        let page = frame::decode_change_page(info.db_id, frame)?;
        for entry in &page.entries {
            if let ChangeEntry::Record(change) = entry {
                validate_record_change(change)?;
            }
        }
        Ok(page)
    }

    pub fn apply(&self, frame: &[u8]) -> Result<ApplyReport> {
        let page = self.decode(frame)?;
        self.engine.apply_page(&page)
    }

    pub fn offers(&self) -> Result<OfferPage> {
        let info = self.info()?;
        Ok(OfferPage {
            sender: info.device_id,
            offers: self.engine.rotation_offers()?,
        })
    }

    pub fn encode_offers(&self, page: &OfferPage) -> Result<Vec<u8>> {
        let info = self.info()?;
        frame::encode_offers(info.db_id, page)
    }

    pub fn decode_offers(&self, frame: &[u8]) -> Result<OfferPage> {
        let info = self.info()?;
        frame::decode_offers(info.db_id, frame)
    }

    pub fn apply_offers(&self, frame: &[u8]) -> Result<()> {
        let page = self.decode_offers(frame)?;
        let entries = page
            .offers
            .into_iter()
            .map(ChangeEntry::RotationOffer)
            .collect();
        self.engine.apply_page(&ChangePage {
            sender: page.sender,
            after: 0,
            next: 0,
            has_more: false,
            log_start: 0,
            log_end: 0,
            entries,
        })?;
        Ok(())
    }

    /// 快照起始位置与分页迭代器。
    pub fn snapshot(&self) -> Result<SnapshotIter<'a>> {
        let at_seq = self.engine.snapshot_start()?;
        Ok(SnapshotIter::borrowed(self.engine, at_seq))
    }

    /// 快照起点；用于无状态分页。
    pub fn snapshot_start(&self) -> Result<u64> {
        self.engine.snapshot_start()
    }

    /// 无状态读取一页快照；`after` 为上一页返回的不透明游标。
    pub fn snapshot_page(&self, after: Option<&[u8]>, limit: usize) -> Result<SnapshotPage> {
        self.engine.snapshot_page(after, limit)
    }

    pub fn encode_snapshot(&self, page: &SnapshotPage) -> Result<Vec<u8>> {
        let info = self.info()?;
        frame::encode_snapshot(info.db_id, page)
    }

    pub fn decode_snapshot(&self, frame: &[u8]) -> Result<SnapshotPage> {
        let info = self.info()?;
        frame::decode_snapshot(info.db_id, frame)
    }

    pub fn apply_snapshot(&self, page: &SnapshotPage) -> Result<ApplyReport> {
        self.engine.apply_snapshot_page(page)
    }

    pub fn apply_snapshot_frame(&self, frame: &[u8]) -> Result<ApplyReport> {
        let page = self.decode_snapshot(frame)?;
        self.engine.apply_snapshot_page(&page)
    }

    pub fn begin_rotation(&self) -> Result<RotationInfo> {
        self.engine.begin_rotation()
    }

    pub fn rotation_offers(&self) -> Result<Vec<RotationOffer>> {
        self.engine.rotation_offers()
    }

    /// 当前轮换状态；没有进行中的轮换时返回 `None`。
    pub fn rotation_state(&self) -> Result<Option<RotationInfo>> {
        self.engine.rotation_state()
    }

    /// 显式确认某个轮换目标 key；安装 offer 时通常已自动 ack。
    pub fn acknowledge_rotation(&self, to: KeyId) -> Result<()> {
        self.engine.acknowledge_rotation(to)
    }

    pub fn migrate_all(&self, to: KeyId) -> Result<u64> {
        self.engine.migrate_all(to)
    }

    /// 带批次与暂停参数的迁移；取消在批次边界生效。
    pub fn migrate_all_with_options(&self, to: KeyId, options: MigrationOptions) -> Result<u64> {
        self.engine.migrate_all_with_options(to, options)
    }

    /// 迁移进度；不获取会话锁。
    pub fn migration_progress(&self) -> MigrationProgress {
        self.engine.migration_progress()
    }

    /// 请求取消正在执行的迁移。
    pub fn cancel_migration(&self) {
        self.engine.cancel_migration();
    }

    pub fn retire(&self, from: KeyId) -> Result<()> {
        self.engine.retire_key(from)
    }

    pub fn prune_offers(&self, to: KeyId) -> Result<()> {
        self.engine.prune_rotation_offers(to)
    }

    pub fn compact(&self, before: u64) -> Result<u64> {
        self.engine.compact_changes(before)
    }

    /// 生成本机上报给 `peer` 的进度帧；未消费过对方日志时游标为 0。
    pub fn progress_frame(&self, peer: DeviceId) -> Result<Vec<u8>> {
        let info = self.info()?;
        let cursor = self.engine.peer_cursor(peer)?.unwrap_or(0);
        frame::encode_progress(info.db_id, info.device_id, cursor)
    }

    /// 应用对端的进度帧；返回落盘后的已消费游标。
    pub fn apply_progress(&self, frame: &[u8]) -> Result<u64> {
        let info = self.info()?;
        let (device, cursor) = frame::decode_progress(info.db_id, frame)?;
        self.engine.apply_peer_progress(device, cursor)
    }

    /// 按对端进度自动裁剪变更日志；返回删除条数。
    pub fn compact_changes_auto(&self) -> Result<u64> {
        self.engine.compact_changes_auto()
    }

    pub fn pairing_offer(&self, ttl: Duration) -> Result<PairingOffer> {
        let (frame, mut secret) = self.engine.create_pairing_bundle_with_secret(ttl)?;
        let secret_text = encode_secret(&secret);
        secret.fill(0);
        let fingerprint = shio_core::sync::pairing_fingerprint(&frame);
        Ok(PairingOffer {
            frame,
            secret: secret_text,
            fingerprint,
        })
    }

    pub fn accept_pairing(
        options: EngineOptions,
        password: &[u8],
        offer: &PairingOffer,
        secret: &[u8],
    ) -> Result<Engine> {
        if shio_core::sync::pairing_fingerprint(&offer.frame) != offer.fingerprint {
            return Err(Error::PairingInvalid);
        }
        Engine::open_with_pairing(options, password, &offer.frame, secret)
    }
}

/// 快照分页迭代器；可借用或拥有 `Engine`。
pub struct SnapshotIter<'a> {
    source: Source<'a>,
    after: Option<Vec<u8>>,
    at_seq: u64,
    done: bool,
}

enum Source<'a> {
    Borrowed(&'a Engine),
    Owned(Arc<Engine>),
}

impl Deref for Source<'_> {
    type Target = Engine;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Borrowed(engine) => engine,
            Self::Owned(engine) => engine,
        }
    }
}

impl<'a> SnapshotIter<'a> {
    fn borrowed(engine: &'a Engine, at_seq: u64) -> Self {
        Self {
            source: Source::Borrowed(engine),
            after: None,
            at_seq,
            done: false,
        }
    }

    fn owned(engine: Arc<Engine>, at_seq: u64) -> Self {
        Self {
            source: Source::Owned(engine),
            after: None,
            at_seq,
            done: false,
        }
    }

    pub fn at_seq(&self) -> u64 {
        self.at_seq
    }

    /// 读取下一页；返回 `None` 表示快照已结束。
    pub fn next_page(&mut self, limit: usize) -> Result<Option<SnapshotPage>> {
        if self.done {
            return Ok(None);
        }
        let page = self.source.snapshot_page(self.after.as_deref(), limit)?;
        if page.entries.is_empty() {
            self.done = true;
            return Ok(None);
        }
        self.after = page.next_key.clone();
        if page.next_key.is_none() {
            self.done = true;
        }
        Ok(Some(page))
    }
}

/// 拥有 `Arc<Engine>` 的同步句柄。
#[derive(Debug, Clone)]
pub struct SyncHandle {
    engine: Arc<Engine>,
}

impl SyncHandle {
    pub fn new(engine: Arc<Engine>) -> Self {
        Self { engine }
    }

    pub fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }

    fn sync(&self) -> Sync<'_> {
        Sync::new(&self.engine)
    }

    pub fn enable_sync(&self, device_id: Option<DeviceId>) -> Result<SyncInfo> {
        self.sync().enable_sync(device_id)
    }

    pub fn info(&self) -> Result<SyncInfo> {
        self.sync().info()
    }

    pub fn changes_since(&self, after: u64, limit: usize) -> Result<ChangePage> {
        self.sync().changes_since(after, limit)
    }

    pub fn encode(&self, page: &ChangePage) -> Result<Vec<u8>> {
        self.sync().encode(page)
    }

    pub fn decode(&self, frame: &[u8]) -> Result<ChangePage> {
        self.sync().decode(frame)
    }

    pub fn apply(&self, frame: &[u8]) -> Result<ApplyReport> {
        self.sync().apply(frame)
    }

    pub fn offers(&self) -> Result<OfferPage> {
        self.sync().offers()
    }

    pub fn encode_offers(&self, page: &OfferPage) -> Result<Vec<u8>> {
        self.sync().encode_offers(page)
    }

    pub fn decode_offers(&self, frame: &[u8]) -> Result<OfferPage> {
        self.sync().decode_offers(frame)
    }

    pub fn apply_offers(&self, frame: &[u8]) -> Result<()> {
        self.sync().apply_offers(frame)
    }

    pub fn snapshot(&self) -> Result<SnapshotIter<'_>> {
        let at_seq = self.engine.snapshot_start()?;
        Ok(SnapshotIter::owned(self.engine.clone(), at_seq))
    }

    /// 快照起点；用于无状态分页。
    pub fn snapshot_start(&self) -> Result<u64> {
        self.engine.snapshot_start()
    }

    /// 无状态读取一页快照；`after` 为上一页返回的不透明游标。
    pub fn snapshot_page(&self, after: Option<&[u8]>, limit: usize) -> Result<SnapshotPage> {
        self.engine.snapshot_page(after, limit)
    }

    pub fn encode_snapshot(&self, page: &SnapshotPage) -> Result<Vec<u8>> {
        self.sync().encode_snapshot(page)
    }

    pub fn decode_snapshot(&self, frame: &[u8]) -> Result<SnapshotPage> {
        self.sync().decode_snapshot(frame)
    }

    pub fn apply_snapshot(&self, page: &SnapshotPage) -> Result<ApplyReport> {
        self.sync().apply_snapshot(page)
    }

    pub fn apply_snapshot_frame(&self, frame: &[u8]) -> Result<ApplyReport> {
        self.sync().apply_snapshot_frame(frame)
    }

    pub fn begin_rotation(&self) -> Result<RotationInfo> {
        self.sync().begin_rotation()
    }

    /// 当前轮换状态；没有进行中的轮换时返回 `None`。
    pub fn rotation_state(&self) -> Result<Option<RotationInfo>> {
        self.engine.rotation_state()
    }

    /// 显式确认某个轮换目标 key；安装 offer 时通常已自动 ack。
    pub fn acknowledge_rotation(&self, to: KeyId) -> Result<()> {
        self.engine.acknowledge_rotation(to)
    }

    pub fn rotation_offers(&self) -> Result<Vec<RotationOffer>> {
        self.sync().rotation_offers()
    }

    pub fn migrate_all(&self, to: KeyId) -> Result<u64> {
        self.sync().migrate_all(to)
    }

    /// 带批次与暂停参数的迁移；取消在批次边界生效。
    pub fn migrate_all_with_options(&self, to: KeyId, options: MigrationOptions) -> Result<u64> {
        self.sync().migrate_all_with_options(to, options)
    }

    /// 进入维护模式：窗口内普通写返回 `Busy`，读操作不受影响。
    pub fn begin_maintenance(&self) -> Result<()> {
        self.engine.begin_maintenance()
    }

    /// 退出维护模式；`done` 表示当前已无待续跑的迁移游标。
    pub fn end_maintenance(&self) -> Result<MaintenanceOutcome> {
        self.engine.end_maintenance()
    }

    /// 单步迁移；`max_records` 为 0 时使用默认 256，上限 1024。
    pub fn migrate_step(&self, to: KeyId, max_records: usize) -> Result<MigrationStep> {
        self.engine.migrate_step(to, max_records)
    }

    /// 合作式调度：在时间预算内推进若干小批次；`budget` 为零取默认 10ms，
    /// `max_records` 为零取默认 32。
    pub fn migrate_tick(
        &self,
        to: KeyId,
        budget: Duration,
        max_records: usize,
    ) -> Result<MigrationStep> {
        self.engine.migrate_tick(to, budget, max_records)
    }

    /// 维护模式是否处于激活状态。
    pub fn maintenance_active(&self) -> bool {
        self.engine.maintenance_active()
    }

    /// 迁移进度；不获取会话锁。
    pub fn migration_progress(&self) -> MigrationProgress {
        self.engine.migration_progress()
    }

    /// 请求取消正在执行的迁移。
    pub fn cancel_migration(&self) {
        self.engine.cancel_migration();
    }

    pub fn retire(&self, from: KeyId) -> Result<()> {
        self.sync().retire(from)
    }

    pub fn prune_offers(&self, to: KeyId) -> Result<()> {
        self.sync().prune_offers(to)
    }

    pub fn compact(&self, before: u64) -> Result<u64> {
        self.sync().compact(before)
    }

    /// 生成本机上报给 `peer` 的进度帧。
    pub fn progress_frame(&self, peer: DeviceId) -> Result<Vec<u8>> {
        self.sync().progress_frame(peer)
    }

    /// 应用对端的进度帧；返回落盘后的已消费游标。
    pub fn apply_progress(&self, frame: &[u8]) -> Result<u64> {
        self.sync().apply_progress(frame)
    }

    /// 按对端进度自动裁剪变更日志；返回删除条数。
    pub fn compact_changes_auto(&self) -> Result<u64> {
        self.sync().compact_changes_auto()
    }

    pub fn pairing_offer(&self, ttl: Duration) -> Result<PairingOffer> {
        self.sync().pairing_offer(ttl)
    }

    pub fn accept_pairing(
        options: EngineOptions,
        password: &[u8],
        offer: &PairingOffer,
        secret: &[u8],
    ) -> Result<Engine> {
        Sync::accept_pairing(options, password, offer, secret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_text_is_stable_length() {
        let text = encode_secret(&[0u8; 32]);
        assert_eq!(text.len(), 52);
        assert_eq!(decode_pairing_secret(&text).unwrap(), [0u8; 32]);
    }
}
