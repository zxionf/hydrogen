//! `shio.sync`：同步状态、游标、轮换与升级进度的字节键。
//!
//! 值格式见 `.design/data.md` §15.5。这里只定义表与键构造函数，读写由引擎
//! 在事务内完成。

use redb::TableDefinition;

use crate::error::{Error, Result};
use crate::model::{DeviceId, KeyId};

/// 字节键表。
pub const TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("shio.sync");

/// 同步状态版本。
pub const VERSION: u16 = 1;

/// `b"v"`。
pub const KEY_VERSION: &[u8] = b"v";
/// `b"db"`。
pub const KEY_DB: &[u8] = b"db";
/// `b"dev"`。
pub const KEY_DEVICE: &[u8] = b"dev";
/// `b"enabled_at"`。
pub const KEY_ENABLED_AT: &[u8] = b"enabled_at";
/// `b"baseline_done"`。
pub const KEY_BASELINE_DONE: &[u8] = b"baseline_done";
/// `b"baseline_key"`。
pub const KEY_BASELINE_KEY: &[u8] = b"baseline_key";
/// `b"log_start"`。
pub const KEY_LOG_START: &[u8] = b"log_start";
/// `b"upgrade"`。
pub const KEY_UPGRADE: &[u8] = b"upgrade";
/// `b"migration"`：进行中的 `format=2` 迁移游标。
///
/// 实际应用远端记录时删除，避免旧 key 记录落在游标之前被跳过。
pub const KEY_MIGRATION: &[u8] = b"migration";

const PEER_PREFIX: &[u8] = b"peer/";
const ROTATION_PREFIX: &[u8] = b"rot/";
const ACK_PREFIX: &[u8] = b"ack/";
const CURRENT_PREFIX: &[u8] = b"cur/";
const MIGRATED_PREFIX: &[u8] = b"mig/";
const UPGRADE_KEY_PREFIX: &[u8] = b"upgrade/key/";

/// 对端游标键。
pub fn peer(device: DeviceId) -> Vec<u8> {
    prefixed(PEER_PREFIX, device.as_bytes())
}

/// 轮换 offer 键。
pub fn rotation(to: KeyId) -> Vec<u8> {
    prefixed(ROTATION_PREFIX, to.as_bytes())
}

/// ack 键。
pub fn ack(to: KeyId, device: DeviceId) -> Vec<u8> {
    let mut out = Vec::with_capacity(ACK_PREFIX.len() + 32);
    out.extend_from_slice(ACK_PREFIX);
    out.extend_from_slice(to.as_bytes());
    out.extend_from_slice(device.as_bytes());
    out
}

/// 当前 key 公告键。
pub fn current(device: DeviceId) -> Vec<u8> {
    prefixed(CURRENT_PREFIX, device.as_bytes())
}

/// 迁移完成键。
pub fn migrated(key: KeyId, device: DeviceId) -> Vec<u8> {
    let mut out = Vec::with_capacity(MIGRATED_PREFIX.len() + 32);
    out.extend_from_slice(MIGRATED_PREFIX);
    out.extend_from_slice(key.as_bytes());
    out.extend_from_slice(device.as_bytes());
    out
}

/// 旧 u16 key id 映射键。
pub fn upgrade_key(legacy: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(UPGRADE_KEY_PREFIX.len() + 2);
    out.extend_from_slice(UPGRADE_KEY_PREFIX);
    out.extend_from_slice(&legacy.to_be_bytes());
    out
}

/// 所有 `ACk_PREFIX`/`CURRENT_PREFIX`/`MIGRATED_PREFIX` 等前缀，供迭代。
pub const fn peer_prefix() -> &'static [u8] {
    PEER_PREFIX
}
/// 轮换 offer 前缀。
pub const fn rotation_prefix() -> &'static [u8] {
    ROTATION_PREFIX
}
/// ack 前缀。
pub const fn ack_prefix() -> &'static [u8] {
    ACK_PREFIX
}
/// 当前 key 前缀。
pub const fn current_prefix() -> &'static [u8] {
    CURRENT_PREFIX
}
/// 迁移完成前缀。
pub const fn migrated_prefix() -> &'static [u8] {
    MIGRATED_PREFIX
}
/// 升级映射前缀。
pub const fn upgrade_key_prefix() -> &'static [u8] {
    UPGRADE_KEY_PREFIX
}

/// 编码升级状态：`phase u8 | cursor_len u16 | cursor`。
pub fn encode_upgrade(phase: u8, cursor: &[u8]) -> Result<Vec<u8>> {
    if !(1..=3).contains(&phase) {
        return Err(Error::Internal("upgrade phase out of range"));
    }
    let cursor_len = u16::try_from(cursor.len())
        .map_err(|_| Error::InvalidArgument("upgrade cursor too long"))?;
    let mut out = Vec::with_capacity(3 + cursor.len());
    out.push(phase);
    out.extend_from_slice(&cursor_len.to_be_bytes());
    out.extend_from_slice(cursor);
    Ok(out)
}

/// 解码升级状态。
pub fn decode_upgrade(bytes: &[u8]) -> Result<(u8, Vec<u8>)> {
    if bytes.len() < 3 {
        return Err(Error::Corrupted("upgrade state too short"));
    }
    let phase = bytes[0];
    if !(1..=3).contains(&phase) {
        return Err(Error::Corrupted("upgrade phase out of range"));
    }
    let len = usize::from(u16::from_be_bytes([bytes[1], bytes[2]]));
    let expected = 3usize
        .checked_add(len)
        .ok_or(Error::Corrupted("upgrade cursor length overflow"))?;
    if expected != bytes.len() {
        return Err(Error::Corrupted("upgrade cursor length mismatch"));
    }
    Ok((phase, bytes[3..].to_vec()))
}

/// 编码迁移游标：`to[16] | cursor_len u16 | cursor`。
pub fn encode_migration(to: KeyId, cursor: &[u8]) -> Result<Vec<u8>> {
    let cursor_len = u16::try_from(cursor.len())
        .map_err(|_| Error::InvalidArgument("migration cursor too long"))?;
    let mut out = Vec::with_capacity(18 + cursor.len());
    out.extend_from_slice(to.as_bytes());
    out.extend_from_slice(&cursor_len.to_be_bytes());
    out.extend_from_slice(cursor);
    Ok(out)
}

/// 解码迁移游标。
pub fn decode_migration(bytes: &[u8]) -> Result<(KeyId, Vec<u8>)> {
    if bytes.len() < 18 {
        return Err(Error::Corrupted("migration state too short"));
    }
    let key: [u8; 16] = bytes[0..16]
        .try_into()
        .map_err(|_| Error::Corrupted("migration key length mismatch"))?;
    let len = usize::from(u16::from_be_bytes([bytes[16], bytes[17]]));
    let expected = 18usize
        .checked_add(len)
        .ok_or(Error::Corrupted("migration cursor length overflow"))?;
    if expected != bytes.len() {
        return Err(Error::Corrupted("migration cursor length mismatch"));
    }
    Ok((KeyId::from_bytes(key), bytes[18..].to_vec()))
}

fn prefixed(prefix: &[u8], id: &[u8; 16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(prefix.len() + 16);
    out.extend_from_slice(prefix);
    out.extend_from_slice(id);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_cursor_roundtrips() {
        let to = KeyId::from_bytes([0x11; 16]);
        let encoded = encode_migration(to, b"record-key").unwrap();
        assert_eq!(
            decode_migration(&encoded).unwrap(),
            (to, b"record-key".to_vec())
        );
    }

    #[test]
    fn migration_cursor_rejects_bad_lengths() {
        assert!(matches!(
            decode_migration(&[0u8; 17]),
            Err(Error::Corrupted(_))
        ));

        let mut encoded = encode_migration(KeyId::from_bytes([0x22; 16]), b"x").unwrap();
        encoded.pop();
        assert!(matches!(
            decode_migration(&encoded),
            Err(Error::Corrupted("migration cursor length mismatch"))
        ));
    }
}
