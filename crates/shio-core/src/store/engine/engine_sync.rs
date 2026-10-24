//! 同步相关的引擎方法：状态、日志、应用、快照、升级与基线。
//!
//! 本文件是 `engine` 的子模块，可直接访问父模块的私有会话与辅助函数。

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;

use redb::{ReadableTable, WriteTransaction};
use zeroize::Zeroizing;

use super::*;
use crate::codec2;
use crate::crypto::keys::{DataKey, KEY_LEN};
use crate::error::{Error, Result};
use crate::model::{DeviceId, KeyId, Timestamp};
use crate::store::{catalog, changes, keys, keys2, meta2, records, sync_state, FORMAT, FORMAT_V2};
use crate::sync::{
    ApplyReport, ChangeEntry, ChangePage, RecordChange, RotationInfo, RotationOffer, SnapshotPage,
    SnapshotRecord, SyncInfo, MAX_KEY_COUNT, MAX_PAGE_ENTRIES,
};

/// 已读取的同步状态。
#[derive(Debug, Clone, Copy)]
pub(super) struct SyncState {
    pub db_id: [u8; 16],
    pub device: DeviceId,
    pub baseline_done: bool,
    pub log_start: u64,
}

impl Engine {
    /// 当前同步摘要；未启用同步时返回 [`Error::SyncDisabled`]。
    pub fn sync_info(&self) -> Result<SyncInfo> {
        let state = self.read_sync_state()?.ok_or(Error::SyncDisabled)?;
        let session = self.read_session()?;
        self.sync_info_from(&session, &state)
    }

    /// 从 `after` 之后读取最多 `limit` 条变更。
    pub fn changes_since(&self, after: u64, limit: usize) -> Result<ChangePage> {
        if limit == 0 || limit > MAX_PAGE_ENTRIES {
            return Err(Error::InvalidArgument("page limit is out of range"));
        }
        let state = self.read_sync_state()?.ok_or(Error::SyncDisabled)?;
        let txn = self.handle.begin_read()?;
        let table = txn.open_table(changes::TABLE)?;
        let log_end = table.last()?.map(|(key, _)| key.value()).unwrap_or(0);

        if after > log_end {
            return Err(Error::InvalidArgument("cursor is past the end of the log"));
        }
        let start = after
            .checked_add(1)
            .ok_or(Error::InvalidArgument("cursor overflow"))?;
        if start < state.log_start {
            return Err(Error::SyncResyncRequired {
                log_start: state.log_start,
            });
        }

        let mut entries = Vec::new();
        let mut last = after;
        if start <= log_end {
            for item in table.range(start..)? {
                let (key, value) = item?;
                let seq = key.value();
                if entries.len() >= limit {
                    break;
                }
                entries.push(changes::decode(value.value())?);
                last = seq;
            }
        }
        let has_more = last < log_end;
        Ok(ChangePage {
            sender: state.device,
            after,
            next: last,
            has_more,
            log_start: state.log_start,
            log_end,
            entries,
        })
    }

    /// 压缩早于 `before` 的日志；返回删除条数。
    pub fn compact_changes(&self, before: u64) -> Result<u64> {
        self.require_writable()?;
        let state = self.read_sync_state()?.ok_or(Error::SyncDisabled)?;
        let txn = self.handle.begin_write()?;
        let log_end = {
            let table = txn.open_table(changes::TABLE)?;
            let value = table.last()?.map(|(key, _)| key.value()).unwrap_or(0);
            value
        };
        if before < 1 || before > log_end.saturating_add(1) {
            return Err(Error::InvalidArgument(
                "compaction point is outside the log range",
            ));
        }

        let removed = {
            let mut table = txn.open_table(changes::TABLE)?;
            let keys: Vec<u64> = table
                .range(..before)?
                .map(|item| item.map(|(key, _)| key.value()))
                .collect::<std::result::Result<_, _>>()?;
            let count = keys.len() as u64;
            for key in keys {
                table.remove(key)?;
            }
            count
        };

        if before > state.log_start {
            let mut table = txn.open_table(sync_state::TABLE)?;
            table.insert(sync_state::KEY_LOG_START, before.to_be_bytes().as_slice())?;
        }
        txn.commit()?;
        Ok(removed)
    }

    /// 读取本机同步状态。
    pub(super) fn read_sync_state(&self) -> Result<Option<SyncState>> {
        let txn = self.handle.begin_read()?;
        self.read_sync_state_txn(&txn)
    }

    pub(super) fn read_sync_state_txn(
        &self,
        txn: &redb::ReadTransaction,
    ) -> Result<Option<SyncState>> {
        if !table_exists(txn, "shio.sync")? {
            return Ok(None);
        }
        let table = txn.open_table(sync_state::TABLE)?;
        let Some(device) = table.get(sync_state::KEY_DEVICE)? else {
            return Ok(None);
        };
        let db_id: [u8; 16] = table
            .get(sync_state::KEY_DB)?
            .ok_or(Error::Corrupted("sync db id missing"))?
            .value()
            .try_into()
            .map_err(|_| Error::Corrupted("sync db id has wrong length"))?;
        let device: [u8; 16] = device
            .value()
            .try_into()
            .map_err(|_| Error::Corrupted("sync device id has wrong length"))?;
        if table.get(sync_state::KEY_ENABLED_AT)?.is_none() {
            return Err(Error::Corrupted("sync enabled_at missing"));
        }
        let baseline_done = match table.get(sync_state::KEY_BASELINE_DONE)? {
            Some(value) => value.value() == [1u8],
            None => false,
        };
        let log_start = match table.get(sync_state::KEY_LOG_START)? {
            Some(value) => u64::from_be_bytes(
                value
                    .value()
                    .try_into()
                    .map_err(|_| Error::Corrupted("sync log_start has wrong length"))?,
            ),
            None => 1,
        };
        Ok(Some(SyncState {
            db_id,
            device: DeviceId::from_bytes(device),
            baseline_done,
            log_start,
        }))
    }

    pub(super) fn sync_info_from(&self, session: &Session, state: &SyncState) -> Result<SyncInfo> {
        let log_end = self.log_end()?;
        Ok(SyncInfo {
            db_id: state.db_id,
            device_id: state.device,
            current_key_id: session.current,
            log_start: state.log_start,
            log_end,
            baseline_done: state.baseline_done,
        })
    }

    pub(super) fn log_end(&self) -> Result<u64> {
        let txn = self.handle.begin_read()?;
        if !table_exists(&txn, "shio.changes")? {
            return Ok(0);
        }
        let table = txn.open_table(changes::TABLE)?;
        let last = table.last()?.map(|(key, _)| key.value()).unwrap_or(0);
        Ok(last)
    }

    /// 首次启用同步：必要时升级到 `format=2`，然后写状态并跑基线。
    pub fn enable_sync(&self, device_id: Option<DeviceId>) -> Result<SyncInfo> {
        self.require_writable()?;
        let mut session = self.write_session()?;

        if self.read_sync_state()?.is_some() {
            let state = self
                .read_sync_state()?
                .ok_or(Error::Internal("sync state disappeared"))?;
            if !state.baseline_done {
                session.sync = Some(state.device);
                self.run_baseline(&mut session, state.device)?;
                let state = self
                    .read_sync_state()?
                    .ok_or(Error::Internal("sync state disappeared"))?;
                return self.sync_info_from(&session, &state);
            }
            return self.sync_info_from(&session, &state);
        }

        if session.format == FORMAT || session.upgrading {
            self.upgrade_to_v2(&mut session)?;
        }

        let device = match device_id {
            Some(device) if !device.is_zero() => device,
            Some(_) => {
                return Err(Error::InvalidArgument("device id must not be zero"));
            }
            None => DeviceId::from_bytes(random_16()?),
        };
        let db_id = random_16()?;
        let now = Timestamp::now().as_millis();

        let txn = self.handle.begin_write()?;
        {
            let mut table = txn.open_table(sync_state::TABLE)?;
            table.insert(
                sync_state::KEY_VERSION,
                sync_state::VERSION.to_be_bytes().as_slice(),
            )?;
            table.insert(sync_state::KEY_DB, db_id.as_slice())?;
            table.insert(sync_state::KEY_DEVICE, device.as_bytes().as_slice())?;
            table.insert(sync_state::KEY_ENABLED_AT, now.to_be_bytes().as_slice())?;
            table.insert(sync_state::KEY_BASELINE_DONE, [0u8].as_slice())?;
            table.insert(sync_state::KEY_LOG_START, 1u64.to_be_bytes().as_slice())?;
        }
        {
            let _records = txn.open_table(records::TABLE)?;
            let _catalog = txn.open_table(catalog::TABLE)?;
            let _changes = txn.open_table(changes::TABLE)?;
        }
        txn.commit()?;

        session.sync = Some(device);
        self.run_baseline(&mut session, device)?;

        let state = self
            .read_sync_state()?
            .ok_or(Error::Internal("sync state missing after enable"))?;
        self.sync_info_from(&session, &state)
    }

    /// 把 `format=1` 会话升级到 `format=2`，并继续未完成的迁移。
    pub(super) fn upgrade_to_v2(&self, session: &mut Session) -> Result<()> {
        if session.format == FORMAT_V2 {
            if session.upgrading {
                return self.continue_upgrade(session);
            }
            return Ok(());
        }
        if session.legacy_keys.is_empty() {
            return Err(Error::Internal("upgrade requires a legacy data key"));
        }

        let mut used: BTreeSet<KeyId> = BTreeSet::new();
        let mut new_entries: Vec<(u16, KeyId, keys2::KeyBlob, DataKey, Subkeys)> = Vec::new();
        let mut mapping: BTreeMap<u16, KeyId> = BTreeMap::new();

        for (legacy_id, entry) in &session.legacy_keys {
            let new_id = random_unique_key_id(&used)?;
            used.insert(new_id);
            mapping.insert(*legacy_id, new_id);

            let wrap_nonce = Nonce::random()?;
            let wrapped = aead::seal(
                session.kek.as_bytes(),
                &wrap_nonce,
                &codec2::keywrap_aad(new_id),
                entry.data_key.as_bytes(),
            )?;
            let wrapped_dk: [u8; keys2::WRAPPED_DK_LEN] = wrapped
                .as_slice()
                .try_into()
                .map_err(|_| Error::Internal("wrapped data key has unexpected length"))?;
            let blob = keys2::KeyBlob {
                key_id: new_id,
                kdf_id: entry.blob.kdf_id,
                flags: 0,
                m_cost_kib: entry.blob.m_cost_kib,
                t_cost: entry.blob.t_cost,
                p_cost: entry.blob.p_cost,
                salt: session.salt,
                wrap_nonce: *wrap_nonce.as_bytes(),
                wrapped_dk,
                created_at: entry.blob.created_at,
                label: entry.blob.label.clone(),
            };
            new_entries.push((
                *legacy_id,
                new_id,
                blob,
                entry.data_key.clone(),
                entry.subkeys.clone(),
            ));
        }

        let legacy_current = session
            .legacy_current
            .ok_or(Error::Internal("legacy current key missing"))?;
        let current_new = *mapping
            .get(&legacy_current)
            .ok_or(Error::Internal("legacy current key mapping missing"))?;

        let mut keys_map = BTreeMap::new();
        let mut key_bytes_vec = Vec::new();
        for (legacy_id, new_id, blob, data_key, subkeys) in new_entries {
            let bytes = blob.encode()?;
            key_bytes_vec.push((new_id, bytes));
            let _ = legacy_id;
            keys_map.insert(
                new_id,
                KeyEntry {
                    blob,
                    data_key,
                    subkeys,
                },
            );
        }

        let current_entry = keys_map
            .get(&current_new)
            .ok_or(Error::Internal("new current key missing"))?;
        let check_nonce = Nonce::random()?;
        let check_ct = aead::seal(
            current_entry.subkeys.check.as_bytes(),
            &check_nonce,
            codec2::keycheck_aad(),
            codec2::KEY_CHECK_PLAINTEXT,
        )?;
        let check_ct: [u8; meta2::KEY_CHECK_CT_LEN] = check_ct
            .as_slice()
            .try_into()
            .map_err(|_| Error::Internal("key check has unexpected length"))?;
        let meta_bytes = meta2::MetaBlob::new(
            current_new,
            session.created_at,
            &session.app_id,
            *check_nonce.as_bytes(),
            check_ct,
        )?
        .encode()?;

        let upgrade_bytes = sync_state::encode_upgrade(1, &[])?;
        let txn = self.handle.begin_write()?;
        {
            let mut table = txn.open_table(keys2::TABLE)?;
            for (key_id, bytes) in &key_bytes_vec {
                table.insert(key_id.as_bytes().as_slice(), bytes.as_slice())?;
            }
        }
        {
            let mut table = txn.open_table(sync_state::TABLE)?;
            for (legacy_id, new_id) in &mapping {
                table.insert(
                    sync_state::upgrade_key(*legacy_id).as_slice(),
                    new_id.as_bytes().as_slice(),
                )?;
            }
            table.insert(sync_state::KEY_UPGRADE, upgrade_bytes.as_slice())?;
        }
        {
            let mut table = txn.open_table(meta::TABLE)?;
            table.insert(META_KEY, meta_bytes.as_slice())?;
        }
        {
            let _records = txn.open_table(records::TABLE)?;
            let _catalog = txn.open_table(catalog::TABLE)?;
            let _changes = txn.open_table(changes::TABLE)?;
        }
        txn.commit()?;

        session.format = FORMAT_V2;
        session.keys = keys_map;
        session.current = current_new;
        session.legacy_map = mapping;
        session.legacy_current = None;
        session.upgrading = true;
        session.key_check_nonce = *check_nonce.as_bytes();
        session.key_check_ct = check_ct;

        self.continue_upgrade(session)
    }

    /// 继续 `format=1 → 2` 的记录迁移；幂等，可重复调用。
    pub(super) fn continue_upgrade(&self, session: &mut Session) -> Result<()> {
        loop {
            let Some((phase, cursor)) = self.read_upgrade_state()? else {
                session.upgrading = false;
                return Ok(());
            };
            if phase >= 3 {
                self.cleanup_upgrade(session)?;
                return Ok(());
            }

            let mut batch: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(ROTATE_BATCH);
            {
                let txn = self.handle.begin_read()?;
                let table = txn.open_table(records::TABLE)?;
                let range: (Bound<&[u8]>, Bound<&[u8]>) = if cursor.is_empty() {
                    (Bound::Unbounded, Bound::Unbounded)
                } else {
                    (Bound::Excluded(cursor.as_slice()), Bound::Unbounded)
                };
                for item in table.range::<&[u8]>(range)? {
                    let (key, value) = item?;
                    batch.push((key.value().to_vec(), value.value().to_vec()));
                    if batch.len() >= ROTATE_BATCH {
                        break;
                    }
                }
            }

            let Some(last) = batch.last() else {
                let bytes = sync_state::encode_upgrade(3, &[])?;
                let txn = self.handle.begin_write()?;
                {
                    let mut table = txn.open_table(sync_state::TABLE)?;
                    table.insert(sync_state::KEY_UPGRADE, bytes.as_slice())?;
                }
                txn.commit()?;
                continue;
            };
            let last_key = last.0.clone();

            let txn = self.handle.begin_write()?;
            {
                let mut table = txn.open_table(records::TABLE)?;
                for (key, value) in &batch {
                    let blob = BlobView::decode(value)?;
                    match &blob {
                        BlobView::V2(_) => continue,
                        BlobView::V1(v1) => {
                            let new_id = session
                                .legacy_map
                                .get(&v1.header.key_id)
                                .copied()
                                .ok_or(Error::Corrupted("legacy key mapping missing"))?;
                            let entry = session
                                .keys
                                .get(&new_id)
                                .ok_or(Error::Corrupted("mapped key missing"))?;
                            let (namespace, collection, id) = codec::decode_record_key(key)?;
                            let path = RecordPath {
                                namespace: &namespace,
                                collection: &collection,
                                id,
                            };
                            let plaintext = self.decrypt(session, path, &blob)?;
                            let header = codec2::RecordHeader::new(
                                new_id,
                                v1.header.rev,
                                v1.header.created_at,
                                v1.header.updated_at,
                                v1.header.data_version,
                            )
                            .with_deleted(v1.header.is_deleted());
                            let out =
                                self.build_record_v2(&entry.subkeys, path, &plaintext, header)?;
                            table.insert(key.as_slice(), out.as_slice())?;
                        }
                    }
                }
            }
            {
                let bytes = sync_state::encode_upgrade(2, &last_key)?;
                let mut table = txn.open_table(sync_state::TABLE)?;
                table.insert(sync_state::KEY_UPGRADE, bytes.as_slice())?;
            }
            txn.commit()?;
        }
    }

    fn read_upgrade_state(&self) -> Result<Option<(u8, Vec<u8>)>> {
        let txn = self.handle.begin_read()?;
        if !table_exists(&txn, "shio.sync")? {
            return Ok(None);
        }
        let table = txn.open_table(sync_state::TABLE)?;
        match table.get(sync_state::KEY_UPGRADE)? {
            Some(value) => Ok(Some(sync_state::decode_upgrade(value.value())?)),
            None => Ok(None),
        }
    }

    fn cleanup_upgrade(&self, session: &mut Session) -> Result<()> {
        let txn = self.handle.begin_write()?;
        {
            let mut table = txn.open_table(keys::TABLE)?;
            let ids: Vec<u16> = table
                .iter()?
                .map(|item| item.map(|(key, _)| key.value()))
                .collect::<std::result::Result<_, _>>()?;
            for id in ids {
                table.remove(id)?;
            }
        }
        {
            let mut table = txn.open_table(sync_state::TABLE)?;
            let prefix = sync_state::upgrade_key_prefix();
            let keys: Vec<Vec<u8>> = table
                .range(prefix..)?
                .map(|item| item.map(|(key, _)| key.value().to_vec()))
                .collect::<std::result::Result<_, _>>()?;
            for key in keys {
                if !key.starts_with(prefix) {
                    break;
                }
                table.remove(key.as_slice())?;
            }
            table.remove(sync_state::KEY_UPGRADE)?;
        }
        txn.commit()?;

        session.legacy_keys.clear();
        session.legacy_map.clear();
        session.legacy_current = None;
        session.upgrading = false;
        Ok(())
    }

    fn run_baseline(&self, session: &mut Session, device: DeviceId) -> Result<()> {
        let mut after: Vec<u8> = {
            let txn = self.handle.begin_read()?;
            let table = txn.open_table(sync_state::TABLE)?;
            table
                .get(sync_state::KEY_BASELINE_KEY)?
                .map(|value| value.value().to_vec())
                .unwrap_or_default()
        };
        loop {
            let mut batch: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(ROTATE_BATCH);
            {
                let txn = self.handle.begin_read()?;
                let table = txn.open_table(records::TABLE)?;
                let range: (Bound<&[u8]>, Bound<&[u8]>) = if after.is_empty() {
                    (Bound::Unbounded, Bound::Unbounded)
                } else {
                    (Bound::Excluded(after.as_slice()), Bound::Unbounded)
                };
                for item in table.range::<&[u8]>(range)? {
                    let (key, value) = item?;
                    batch.push((key.value().to_vec(), value.value().to_vec()));
                    if batch.len() >= ROTATE_BATCH {
                        break;
                    }
                }
            }

            let Some(last) = batch.last() else {
                break;
            };
            let last_key = last.0.clone();

            let txn = self.handle.begin_write()?;
            for (key, raw) in &batch {
                let blob = BlobView::decode(raw)?;
                let (namespace, collection, id) = codec::decode_record_key(key)?;
                append_change(
                    &txn,
                    device,
                    &ChangeEntry::Record(RecordChange {
                        namespace,
                        collection,
                        id,
                        key_id: blob.key_id(),
                        rev: blob.rev(),
                        updated_at: Timestamp::from_millis(blob.updated_at()),
                        deleted: blob.is_deleted(),
                        writer: device,
                        blob: raw.clone(),
                    }),
                )?;
            }
            {
                let mut table = txn.open_table(sync_state::TABLE)?;
                table.insert(sync_state::KEY_BASELINE_KEY, last_key.as_slice())?;
            }
            txn.commit()?;
            after = last_key;
        }

        let txn = self.handle.begin_write()?;
        {
            let mut table = txn.open_table(sync_state::TABLE)?;
            table.insert(sync_state::KEY_BASELINE_DONE, [1u8].as_slice())?;
        }
        append_change(
            &txn,
            device,
            &ChangeEntry::CurrentKey {
                key_id: session.current,
                at: Timestamp::now(),
            },
        )?;
        {
            let mut table = txn.open_table(sync_state::TABLE)?;
            table.insert(
                sync_state::current(device).as_slice(),
                session.current.as_bytes().as_slice(),
            )?;
        }
        txn.commit()?;
        Ok(())
    }
}

fn random_16() -> Result<[u8; 16]> {
    let mut bytes = [0u8; 16];
    loop {
        getrandom::fill(&mut bytes)
            .map_err(|_| Error::Internal("operating system CSPRNG unavailable"))?;
        if bytes != [0u8; 16] {
            return Ok(bytes);
        }
    }
}

fn random_unique_key_id(used: &BTreeSet<KeyId>) -> Result<KeyId> {
    loop {
        let id = KeyId::from_bytes(random_16()?);
        if !used.contains(&id) {
            return Ok(id);
        }
    }
}
impl Engine {
    /// 整页应用：记录、轮换 offer 与控制条目在同一个写事务内生效。
    pub fn apply_page(&self, page: &ChangePage) -> Result<ApplyReport> {
        self.require_writable()?;
        let mut session = self.write_session()?;
        let state = self.read_sync_state()?.ok_or(Error::SyncDisabled)?;

        let mut pending: Vec<(KeyId, KeyEntry)> = Vec::new();
        let mut current = session.current;
        let mut current_created = session.key_created_at(current).unwrap_or(i64::MIN);
        let mut switched = false;
        let mut report = ApplyReport::default();
        let txn = self.handle.begin_write()?;

        for entry in &page.entries {
            match entry {
                ChangeEntry::Record(change) => {
                    let installed = session.keys.contains_key(&change.key_id)
                        || pending.iter().any(|(id, _)| *id == change.key_id);
                    if !installed {
                        return Err(Error::RotationRequired {
                            key_id: change.key_id,
                        });
                    }
                    if self.stage_remote_record(&txn, change)? {
                        report.applied += 1;
                    } else {
                        report.skipped += 1;
                    }
                }
                ChangeEntry::RotationOffer(offer) => {
                    let installed = session.keys.contains_key(&offer.to)
                        || pending.iter().any(|(id, _)| *id == offer.to);
                    if !installed {
                        if let Some(entry) =
                            self.install_offer_txn(&txn, &session, &state, offer, &pending)?
                        {
                            pending.push((offer.to, entry));
                        }
                    } else {
                        // 配对可能已经装上了这条 offer 的 key；仍要补 ack，否则发起方
                        // 永远等不到该设备，prune 无法完成。
                        let ack_key = sync_state::ack(offer.to, state.device);
                        let already_acked = {
                            let table = txn.open_table(sync_state::TABLE)?;
                            let value = table.get(ack_key.as_slice())?.is_some();
                            value
                        };
                        if !already_acked {
                            {
                                let mut table = txn.open_table(sync_state::TABLE)?;
                                table.insert(ack_key.as_slice(), [1u8].as_slice())?;
                            }
                            append_change(
                                &txn,
                                state.device,
                                &ChangeEntry::RotationAck {
                                    to: offer.to,
                                    device: state.device,
                                    at: Timestamp::now(),
                                },
                            )?;
                        }
                    }
                    // offer 的 created_at 是这条 key 的权威创建时间；即使 key 是配对
                    // 预先装上的，也按它参与当前写 key 的收敛。
                    if (offer.created_at.as_millis(), offer.to) > (current_created, current) {
                        current = offer.to;
                        current_created = offer.created_at.as_millis();
                        switched = true;
                    }
                    report.controls += 1;
                }
                ChangeEntry::RotationAck { to, device, at: _ } => {
                    if *device != page.sender {
                        return Err(Error::Corrupted(
                            "rotation ack device does not match the sender",
                        ));
                    }
                    let mut table = txn.open_table(sync_state::TABLE)?;
                    table.insert(sync_state::ack(*to, *device).as_slice(), [1u8].as_slice())?;
                    report.controls += 1;
                }
                ChangeEntry::CurrentKey { key_id, at } => {
                    let mut table = txn.open_table(sync_state::TABLE)?;
                    table.insert(
                        sync_state::current(page.sender).as_slice(),
                        key_id.as_bytes().as_slice(),
                    )?;
                    drop(table);
                    let created = session.key_created_at(*key_id).or_else(|| {
                        pending
                            .iter()
                            .find(|(id, _)| id == key_id)
                            .map(|(_, entry)| entry.blob.created_at)
                    });
                    if let Some(created_at) = created {
                        if (created_at, *key_id) > (current_created, current) {
                            current = *key_id;
                            current_created = created_at;
                            switched = true;
                        }
                    }
                    let _ = at;
                    report.controls += 1;
                }
                ChangeEntry::Migrated { key_id, at: _ } => {
                    let mut table = txn.open_table(sync_state::TABLE)?;
                    table.insert(
                        sync_state::migrated(*key_id, page.sender).as_slice(),
                        [1u8].as_slice(),
                    )?;
                    report.controls += 1;
                }
            }
        }

        let mut new_check: Option<([u8; NONCE_LEN], [u8; meta2::KEY_CHECK_CT_LEN])> = None;
        if switched && current != session.current {
            let entry = session
                .keys
                .get(&current)
                .or_else(|| {
                    pending
                        .iter()
                        .find(|(id, _)| *id == current)
                        .map(|(_, entry)| entry)
                })
                .ok_or(Error::Internal("selected write key is missing"))?;
            let check_nonce = Nonce::random()?;
            let check_ct = aead::seal(
                entry.subkeys.check.as_bytes(),
                &check_nonce,
                codec2::keycheck_aad(),
                codec2::KEY_CHECK_PLAINTEXT,
            )?;
            let check_ct: [u8; meta2::KEY_CHECK_CT_LEN] = check_ct
                .as_slice()
                .try_into()
                .map_err(|_| Error::Internal("key check has unexpected length"))?;
            new_check = Some((*check_nonce.as_bytes(), check_ct));

            let meta_bytes = session.meta2_bytes(current, *check_nonce.as_bytes(), check_ct)?;
            {
                let mut table = txn.open_table(meta::TABLE)?;
                table.insert(META_KEY, meta_bytes.as_slice())?;
            }
            append_change(
                &txn,
                state.device,
                &ChangeEntry::CurrentKey {
                    key_id: current,
                    at: Timestamp::now(),
                },
            )?;
            {
                let mut table = txn.open_table(sync_state::TABLE)?;
                table.insert(
                    sync_state::current(state.device).as_slice(),
                    current.as_bytes().as_slice(),
                )?;
            }
        }

        {
            let mut table = txn.open_table(sync_state::TABLE)?;
            let key = sync_state::peer(page.sender);
            let previous = table
                .get(key.as_slice())?
                .map(|value| {
                    value
                        .value()
                        .try_into()
                        .map(u64::from_be_bytes)
                        .map_err(|_| Error::Corrupted("peer cursor has wrong length"))
                })
                .transpose()?
                .unwrap_or(0);
            if page.next > previous {
                table.insert(key.as_slice(), page.next.to_be_bytes().as_slice())?;
            }
        }
        txn.commit()?;

        for (key_id, entry) in pending {
            session.keys.insert(key_id, entry);
        }
        if let Some((nonce, ct)) = new_check {
            session.current = current;
            session.key_check_nonce = nonce;
            session.key_check_ct = ct;
        }
        Ok(report)
    }

    fn stage_remote_record(&self, txn: &WriteTransaction, change: &RecordChange) -> Result<bool> {
        let remote_blob = codec2::RecordBlob::decode(&change.blob)?;
        let header = remote_blob.header;
        if header.key_id != change.key_id
            || header.rev != change.rev
            || header.updated_at != change.updated_at.as_millis()
            || header.is_deleted() != change.deleted
        {
            return Err(Error::Corrupted("change entry does not match record blob"));
        }

        let key = codec::encode_record_key(&change.namespace, &change.collection, &change.id)?;
        let local = {
            let table = txn.open_table(records::TABLE)?;
            let value = table
                .get(key.as_slice())?
                .map(|guard| guard.value().to_vec());
            value
        };
        if let Some(local) = &local {
            let local_blob = BlobView::decode(local)?;
            let local_tuple = (local_blob.updated_at(), local_blob.rev(), local);
            let remote_tuple = (header.updated_at, header.rev, &change.blob);
            if remote_tuple <= local_tuple {
                return Ok(false);
            }
        }

        {
            let mut table = txn.open_table(records::TABLE)?;
            table.insert(key.as_slice(), change.blob.as_slice())?;
        }
        self.stage_catalog(txn, &change.namespace, &change.collection)?;
        append_change(
            txn,
            DeviceId::from_bytes([0u8; 16]),
            &ChangeEntry::Record(change.clone()),
        )?;
        Ok(true)
    }

    fn install_offer_txn(
        &self,
        txn: &WriteTransaction,
        session: &Session,
        state: &SyncState,
        offer: &crate::sync::RotationOffer,
        pending: &[(KeyId, KeyEntry)],
    ) -> Result<Option<KeyEntry>> {
        if offer.from == offer.to || offer.from.is_zero() || offer.to.is_zero() {
            return Err(Error::Corrupted("rotation offer has invalid key ids"));
        }
        if session.keys.len() + pending.len() >= crate::sync::MAX_KEY_COUNT {
            return Err(Error::InvalidArgument("data key count limit reached"));
        }

        let from_entry = session
            .keys
            .get(&offer.from)
            .or_else(|| {
                pending
                    .iter()
                    .find(|(id, _)| *id == offer.from)
                    .map(|(_, entry)| entry)
            })
            .ok_or(Error::RotationRequired { key_id: offer.from })?;

        let rk = kdf::derive_rotation_key(&from_entry.data_key, &state.db_id)?;
        let nonce = Nonce::from_bytes(offer.wrap_nonce);
        let aad = codec2::rotation_aad(state.db_id, offer.from, offer.to);
        let dk_bytes = Zeroizing::new(
            aead::open(rk.as_bytes(), &nonce, &aad, &offer.wrapped_dk)
                .map_err(|_| Error::Corrupted("rotation offer authentication failed"))?,
        );
        let dk_bytes: [u8; KEY_LEN] = dk_bytes
            .as_slice()
            .try_into()
            .map_err(|_| Error::Corrupted("rotation offer carries a wrong key length"))?;
        let data_key = DataKey::from_bytes(dk_bytes);
        let subkeys = kdf::derive_subkeys(&data_key)?;

        let wrap_nonce = Nonce::random()?;
        let wrapped = aead::seal(
            session.kek.as_bytes(),
            &wrap_nonce,
            &codec2::keywrap_aad(offer.to),
            data_key.as_bytes(),
        )?;
        let wrapped_dk: [u8; keys2::WRAPPED_DK_LEN] = wrapped
            .as_slice()
            .try_into()
            .map_err(|_| Error::Internal("wrapped data key has unexpected length"))?;
        let blob = keys2::KeyBlob {
            key_id: offer.to,
            kdf_id: session.kdf_id,
            flags: 0,
            m_cost_kib: session.kdf_params.m_cost_kib,
            t_cost: session.kdf_params.t_cost,
            p_cost: session.kdf_params.p_cost,
            salt: session.salt,
            wrap_nonce: *wrap_nonce.as_bytes(),
            wrapped_dk,
            created_at: offer.created_at.as_millis(),
            label: "rotation".to_owned(),
        };
        let bytes = blob.encode()?;
        {
            let mut table = txn.open_table(keys2::TABLE)?;
            table.insert(offer.to.as_bytes().as_slice(), bytes.as_slice())?;
        }

        let ack_key = sync_state::ack(offer.to, state.device);
        let already_acked = {
            let table = txn.open_table(sync_state::TABLE)?;
            let value = table.get(ack_key.as_slice())?.is_some();
            value
        };
        if !already_acked {
            {
                let mut table = txn.open_table(sync_state::TABLE)?;
                table.insert(ack_key.as_slice(), [1u8].as_slice())?;
            }
            append_change(
                txn,
                state.device,
                &ChangeEntry::RotationAck {
                    to: offer.to,
                    device: state.device,
                    at: Timestamp::now(),
                },
            )?;
        }

        Ok(Some(KeyEntry {
            blob,
            data_key,
            subkeys,
        }))
    }

    /// 快照对应的日志位置。
    pub fn snapshot_start(&self) -> Result<u64> {
        if self.read_sync_state()?.is_none() {
            return Err(Error::SyncDisabled);
        }
        self.log_end()
    }

    /// 按记录键升序读取一页快照。
    pub fn snapshot_page(&self, after: Option<&[u8]>, limit: usize) -> Result<SnapshotPage> {
        if limit == 0 || limit > MAX_PAGE_ENTRIES {
            return Err(Error::InvalidArgument("page limit is out of range"));
        }
        if self.read_sync_state()?.is_none() {
            return Err(Error::SyncDisabled);
        }
        let at_seq = self.log_end()?;
        let txn = self.handle.begin_read()?;
        let table = txn.open_table(records::TABLE)?;
        let range = match after {
            Some(key) => (Bound::Excluded(key), Bound::Unbounded),
            None => (Bound::Unbounded, Bound::Unbounded),
        };

        let mut entries = Vec::new();
        let mut next_key = None;
        let mut last_key: Option<Vec<u8>> = None;
        for item in table.range::<&[u8]>(range)? {
            let (key, value) = item?;
            if entries.len() >= limit {
                next_key = last_key.clone();
                break;
            }
            let key_bytes = key.value().to_vec();
            let (namespace, collection, id) = codec::decode_record_key(&key_bytes)?;
            let blob = BlobView::decode(value.value())?;
            if !matches!(blob, BlobView::V2(_)) {
                return Err(Error::Corrupted("sync library contains a v1 record"));
            }
            entries.push(SnapshotRecord {
                namespace,
                collection,
                id,
                blob: value.value().to_vec(),
            });
            last_key = Some(key_bytes);
        }

        Ok(SnapshotPage {
            at_seq,
            next_key,
            entries,
        })
    }

    /// 把一页快照按 LWW 合并进本地库，并以本机设备写日志。
    pub fn apply_snapshot_page(&self, page: &SnapshotPage) -> Result<ApplyReport> {
        self.require_writable()?;
        let session = self.write_session()?;
        let state = self.read_sync_state()?.ok_or(Error::SyncDisabled)?;
        let mut report = ApplyReport::default();
        let txn = self.handle.begin_write()?;
        for record in &page.entries {
            let blob = codec2::RecordBlob::decode(&record.blob)?;
            if !session.keys.contains_key(&blob.header.key_id) {
                return Err(Error::RotationRequired {
                    key_id: blob.header.key_id,
                });
            }
            let change = RecordChange {
                namespace: record.namespace.clone(),
                collection: record.collection.clone(),
                id: record.id,
                key_id: blob.header.key_id,
                rev: blob.header.rev,
                updated_at: Timestamp::from_millis(blob.header.updated_at),
                deleted: blob.header.is_deleted(),
                writer: state.device,
                blob: record.blob.clone(),
            };
            if self.stage_remote_record(&txn, &change)? {
                report.applied += 1;
            } else {
                report.skipped += 1;
            }
        }
        txn.commit()?;
        Ok(report)
    }
}
impl Engine {
    /// 发起一次轮换：生成随机新 key、写 offer 与当前 key 公告。
    pub fn begin_rotation(&self) -> Result<RotationInfo> {
        self.require_writable()?;
        let mut session = self.write_session()?;
        let state = self.read_sync_state()?.ok_or(Error::SyncDisabled)?;
        if session.upgrading {
            return Err(Error::Busy);
        }
        if session.format != FORMAT_V2 {
            return Err(Error::SyncDisabled);
        }
        if session.keys.len() >= MAX_KEY_COUNT {
            return Err(Error::InvalidArgument("data key count limit reached"));
        }

        let from = session.current;
        let from_entry = session.active()?;
        let mut used: BTreeSet<KeyId> = session.keys.keys().copied().collect();
        let to = random_unique_key_id(&used)?;
        used.insert(to);

        let now = Timestamp::now();
        let rk = kdf::derive_rotation_key(&from_entry.data_key, &state.db_id)?;
        let mut dk_bytes = [0u8; KEY_LEN];
        getrandom::fill(&mut dk_bytes)
            .map_err(|_| Error::Internal("operating system CSPRNG unavailable"))?;
        let data_key = DataKey::from_bytes(dk_bytes);
        let subkeys = kdf::derive_subkeys(&data_key)?;

        let wrap_nonce = Nonce::random()?;
        let aad = codec2::rotation_aad(state.db_id, from, to);
        let wrapped_dk = aead::seal(rk.as_bytes(), &wrap_nonce, &aad, data_key.as_bytes())?;

        let local_wrap_nonce = Nonce::random()?;
        let local_wrapped = aead::seal(
            session.kek.as_bytes(),
            &local_wrap_nonce,
            &codec2::keywrap_aad(to),
            data_key.as_bytes(),
        )?;
        let local_wrapped_dk: [u8; keys2::WRAPPED_DK_LEN] = local_wrapped
            .as_slice()
            .try_into()
            .map_err(|_| Error::Internal("wrapped data key has unexpected length"))?;
        let blob = keys2::KeyBlob {
            key_id: to,
            kdf_id: session.kdf_id,
            flags: 0,
            m_cost_kib: session.kdf_params.m_cost_kib,
            t_cost: session.kdf_params.t_cost,
            p_cost: session.kdf_params.p_cost,
            salt: session.salt,
            wrap_nonce: *local_wrap_nonce.as_bytes(),
            wrapped_dk: local_wrapped_dk,
            created_at: now.as_millis(),
            label: "rotation".to_owned(),
        };

        let offer = RotationOffer {
            from,
            to,
            created_at: now,
            origin: state.device,
            wrap_nonce: *wrap_nonce.as_bytes(),
            wrapped_dk,
        };

        let check_nonce = Nonce::random()?;
        let check_ct = aead::seal(
            subkeys.check.as_bytes(),
            &check_nonce,
            codec2::keycheck_aad(),
            codec2::KEY_CHECK_PLAINTEXT,
        )?;
        let check_ct: [u8; meta2::KEY_CHECK_CT_LEN] = check_ct
            .as_slice()
            .try_into()
            .map_err(|_| Error::Internal("key check has unexpected length"))?;
        let meta_bytes = session.meta2_bytes(to, *check_nonce.as_bytes(), check_ct)?;

        let blob_bytes = blob.encode()?;
        let offer_bytes = changes::encode(&ChangeEntry::RotationOffer(offer.clone()))?;
        let txn = self.handle.begin_write()?;
        {
            let mut table = txn.open_table(keys2::TABLE)?;
            table.insert(to.as_bytes().as_slice(), blob_bytes.as_slice())?;
        }
        {
            let mut table = txn.open_table(sync_state::TABLE)?;
            table.insert(sync_state::rotation(to).as_slice(), offer_bytes.as_slice())?;
        }
        append_change(
            &txn,
            state.device,
            &ChangeEntry::RotationOffer(offer.clone()),
        )?;
        {
            let mut table = txn.open_table(sync_state::TABLE)?;
            table.insert(
                sync_state::current(state.device).as_slice(),
                to.as_bytes().as_slice(),
            )?;
        }
        append_change(
            &txn,
            state.device,
            &ChangeEntry::CurrentKey {
                key_id: to,
                at: now,
            },
        )?;
        {
            let mut table = txn.open_table(meta::TABLE)?;
            table.insert(META_KEY, meta_bytes.as_slice())?;
        }
        txn.commit()?;

        session.keys.insert(
            to,
            KeyEntry {
                blob,
                data_key,
                subkeys,
            },
        );
        session.current = to;
        session.key_check_nonce = *check_nonce.as_bytes();
        session.key_check_ct = check_ct;

        Ok(RotationInfo {
            from,
            to,
            created_at: now,
            acked: Vec::new(),
        })
    }

    /// 本机发起的最近一次轮换及其 ack 集合。
    pub fn rotation_state(&self) -> Result<Option<RotationInfo>> {
        let state = self.read_sync_state()?.ok_or(Error::SyncDisabled)?;
        let offers = self.rotation_offers()?;
        let mut mine: Vec<RotationOffer> = offers
            .into_iter()
            .filter(|offer| offer.origin == state.device)
            .collect();
        mine.sort_by_key(|offer| (offer.created_at, offer.to));
        let Some(latest) = mine.pop() else {
            return Ok(None);
        };
        let acked = self.acked_devices(latest.to)?;
        Ok(Some(RotationInfo {
            from: latest.from,
            to: latest.to,
            created_at: latest.created_at,
            acked,
        }))
    }

    /// 已记录的轮换 offer，按 `(created_at, to)` 升序。
    pub fn rotation_offers(&self) -> Result<Vec<RotationOffer>> {
        if self.read_sync_state()?.is_none() {
            return Err(Error::SyncDisabled);
        }
        let txn = self.handle.begin_read()?;
        if !table_exists(&txn, "shio.sync")? {
            return Err(Error::SyncDisabled);
        }
        let table = txn.open_table(sync_state::TABLE)?;
        let prefix = sync_state::rotation_prefix();
        let mut offers = Vec::new();
        for item in table.range(prefix..)? {
            let (key, value) = item?;
            if !key.value().starts_with(prefix) {
                break;
            }
            if let ChangeEntry::RotationOffer(offer) = changes::decode(value.value())? {
                offers.push(offer);
            } else {
                return Err(Error::Corrupted("rotation state is not an offer"));
            }
        }
        offers.sort_by_key(|offer| (offer.created_at, offer.to));
        Ok(offers)
    }

    /// 本机显式确认某个 offer；安装 offer 时已自动 ack，这里是补救入口。
    pub fn acknowledge_rotation(&self, to: KeyId) -> Result<()> {
        self.require_writable()?;
        let state = self.read_sync_state()?.ok_or(Error::SyncDisabled)?;
        if !self.read_session()?.keys.contains_key(&to) {
            return Err(Error::RotationRequired { key_id: to });
        }
        let txn = self.handle.begin_write()?;
        let ack_key = sync_state::ack(to, state.device);
        let (already_acked, offered) = {
            let table = txn.open_table(sync_state::TABLE)?;
            let already_acked = table.get(ack_key.as_slice())?.is_some();
            let offered = table.get(sync_state::rotation(to).as_slice())?.is_some();
            (already_acked, offered)
        };
        // 接收方安装 offer 时已经自动 ack，此时没有本地 rotation 行；重复调用应幂等。
        if already_acked {
            return Ok(());
        }
        if !offered {
            return Err(Error::NotFound);
        }
        {
            let mut table = txn.open_table(sync_state::TABLE)?;
            table.insert(ack_key.as_slice(), [1u8].as_slice())?;
        }
        append_change(
            &txn,
            state.device,
            &ChangeEntry::RotationAck {
                to,
                device: state.device,
                at: Timestamp::now(),
            },
        )?;
        txn.commit()?;
        Ok(())
    }

    /// 把全部记录重新加密到 `to`，并为不再被引用的 key 公告迁移完成。
    pub fn migrate_all(&self, to: KeyId) -> Result<u64> {
        self.require_writable()?;
        let session = self.write_session()?;
        let state = self.read_sync_state()?.ok_or(Error::SyncDisabled)?;
        if !session.keys.contains_key(&to) {
            return Err(Error::InvalidArgument("target data key is not installed"));
        }

        let mut migrated = 0u64;
        let mut after: Option<Vec<u8>> = None;
        loop {
            let mut batch: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(ROTATE_BATCH);
            {
                let txn = self.handle.begin_read()?;
                let table = txn.open_table(records::TABLE)?;
                let range: (Bound<&[u8]>, Bound<&[u8]>) = match after.as_deref() {
                    Some(last) => (Bound::Excluded(last), Bound::Unbounded),
                    None => (Bound::Unbounded, Bound::Unbounded),
                };
                for item in table.range::<&[u8]>(range)? {
                    let (key, value) = item?;
                    batch.push((key.value().to_vec(), value.value().to_vec()));
                    if batch.len() >= ROTATE_BATCH {
                        break;
                    }
                }
            }
            let Some(last) = batch.last() else {
                break;
            };
            let last_key = last.0.clone();

            let txn = self.handle.begin_write()?;
            {
                let mut table = txn.open_table(records::TABLE)?;
                for (key, raw) in &batch {
                    let blob = BlobView::decode(raw)?;
                    if blob.key_id() == to {
                        continue;
                    }
                    let (namespace, collection, id) = codec::decode_record_key(key)?;
                    let path = RecordPath {
                        namespace: &namespace,
                        collection: &collection,
                        id,
                    };
                    let plaintext = self.decrypt(&session, path, &blob)?;
                    let entry = session
                        .keys
                        .get(&to)
                        .ok_or(Error::Internal("target key missing"))?;
                    // 迁移改变了字节表示；推进 updated_at 使旧密钥的同一逻辑版本
                    // 不会在 LWW 的 blob 次序里把已迁移记录翻回旧密钥。
                    let updated_at = Timestamp::now()
                        .as_millis()
                        .max(blob.updated_at().saturating_add(1));
                    let header = codec2::RecordHeader::new(
                        to,
                        blob.rev(),
                        blob.created_at(),
                        updated_at,
                        blob.data_version(),
                    )
                    .with_deleted(blob.is_deleted());
                    let out = self.build_record_v2(&entry.subkeys, path, &plaintext, header)?;
                    table.insert(key.as_slice(), out.as_slice())?;
                    migrated += 1;
                }
            }
            txn.commit()?;
            after = Some(last_key);
        }

        let mut referenced: BTreeSet<KeyId> = BTreeSet::new();
        {
            let txn = self.handle.begin_read()?;
            let table = txn.open_table(records::TABLE)?;
            for item in table.iter()? {
                let (_, value) = item?;
                let blob = BlobView::decode(value.value())?;
                referenced.insert(blob.key_id());
            }
        }

        let announce: Vec<KeyId> = session
            .keys
            .keys()
            .copied()
            .filter(|key| *key != to && !referenced.contains(key))
            .collect();
        if !announce.is_empty() {
            let txn = self.handle.begin_write()?;
            for key_id in announce {
                append_change(
                    &txn,
                    state.device,
                    &ChangeEntry::Migrated {
                        key_id,
                        at: Timestamp::now(),
                    },
                )?;
                let mut table = txn.open_table(sync_state::TABLE)?;
                table.insert(
                    sync_state::migrated(key_id, state.device).as_slice(),
                    [1u8].as_slice(),
                )?;
            }
            txn.commit()?;
        }

        Ok(migrated)
    }

    /// 退役一个不再被任何记录引用的 key。
    pub fn retire_key(&self, from: KeyId) -> Result<()> {
        self.require_writable()?;
        let mut session = self.write_session()?;
        let state = self.read_sync_state()?.ok_or(Error::SyncDisabled)?;
        if from == session.current {
            return Err(Error::InvalidArgument("cannot retire the current data key"));
        }
        if !session.keys.contains_key(&from) {
            return Err(Error::NotFound);
        }

        {
            let txn = self.handle.begin_read()?;
            let table = txn.open_table(records::TABLE)?;
            for item in table.iter()? {
                let (_, value) = item?;
                let blob = BlobView::decode(value.value())?;
                if blob.key_id() == from {
                    return Err(Error::Busy);
                }
            }
        }

        let known = self.known_devices()?;
        for device in &known {
            let txn = self.handle.begin_read()?;
            let table = txn.open_table(sync_state::TABLE)?;
            if let Some(current) = table.get(sync_state::current(*device).as_slice())? {
                if current.value() == from.as_bytes() {
                    return Err(Error::Busy);
                }
            }
            if table
                .get(sync_state::migrated(from, *device).as_slice())?
                .is_none()
            {
                return Err(Error::Busy);
            }
        }

        for offer in self.rotation_offers()? {
            if offer.from == from && !session.keys.contains_key(&offer.to) {
                return Err(Error::Busy);
            }
        }

        let txn = self.handle.begin_write()?;
        {
            let mut table = txn.open_table(keys2::TABLE)?;
            table.remove(from.as_bytes().as_slice())?;
        }
        txn.commit()?;
        session.keys.remove(&from);
        let _ = state;
        Ok(())
    }

    /// 所有已知设备都 ack 后，删除 offer 记录。
    pub fn prune_rotation_offers(&self, to: KeyId) -> Result<()> {
        self.require_writable()?;
        let state = self.read_sync_state()?.ok_or(Error::SyncDisabled)?;
        {
            let txn = self.handle.begin_read()?;
            let table = txn.open_table(sync_state::TABLE)?;
            if table.get(sync_state::rotation(to).as_slice())?.is_none() {
                return Err(Error::NotFound);
            }
        }
        let known = self.known_devices()?;
        for device in known {
            if device == state.device {
                continue;
            }
            let txn = self.handle.begin_read()?;
            let table = txn.open_table(sync_state::TABLE)?;
            if table.get(sync_state::ack(to, device).as_slice())?.is_none() {
                return Err(Error::Busy);
            }
        }
        let txn = self.handle.begin_write()?;
        {
            let mut table = txn.open_table(sync_state::TABLE)?;
            table.remove(sync_state::rotation(to).as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    fn acked_devices(&self, to: KeyId) -> Result<Vec<DeviceId>> {
        let txn = self.handle.begin_read()?;
        if !table_exists(&txn, "shio.sync")? {
            return Ok(Vec::new());
        }
        let table = txn.open_table(sync_state::TABLE)?;
        let prefix = sync_state::ack_prefix();
        let mut out = Vec::new();
        for item in table.range(prefix..)? {
            let (key, _) = item?;
            let key = key.value();
            if !key.starts_with(prefix) {
                break;
            }
            if key.len() != prefix.len() + 32
                || &key[prefix.len()..prefix.len() + 16] != to.as_bytes()
            {
                continue;
            }
            let device: [u8; 16] = key[prefix.len() + 16..]
                .try_into()
                .map_err(|_| Error::Corrupted("rotation ack device id has wrong length"))?;
            out.push(DeviceId::from_bytes(device));
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    fn known_devices(&self) -> Result<Vec<DeviceId>> {
        let txn = self.handle.begin_read()?;
        if !table_exists(&txn, "shio.sync")? {
            return Ok(Vec::new());
        }
        let table = txn.open_table(sync_state::TABLE)?;
        let prefix = sync_state::current_prefix();
        let mut out = Vec::new();
        for item in table.range(prefix..)? {
            let (key, _) = item?;
            let key = key.value();
            if !key.starts_with(prefix) {
                break;
            }
            if key.len() != prefix.len() + 16 {
                continue;
            }
            let device: [u8; 16] = key[prefix.len()..]
                .try_into()
                .map_err(|_| Error::Corrupted("known device id has wrong length"))?;
            out.push(DeviceId::from_bytes(device));
        }
        out.sort();
        out.dedup();
        Ok(out)
    }
}
// __SYNC_MORE__
