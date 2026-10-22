//! `shio.changes`：追加式本地变更日志。
//!
//! 表的键是本地 `u64` 序号，值是 `ChangeBlob`。控制条目与记录共用序号。

use redb::TableDefinition;

use crate::codec;
use crate::codec2;
use crate::error::{Error, Result};
use crate::model::{DeviceId, KeyId, Timestamp};
use crate::sync::{ChangeEntry, RecordChange, RotationOffer};

/// 键为日志序号。
pub const TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("shio.changes");

/// 变更条目魔数。
pub const MAGIC: &[u8; 4] = b"SHC2";

/// 变更格式版本。
pub const FORMAT: u8 = 2;

/// 公共头长度。
pub const HEADER_LEN: usize = 8;

/// 记录条目固定开销：key_id + rev + updated_at + writer + key_len + blob_len。
pub const RECORD_OVERHEAD: usize = 16 + 8 + 8 + 16 + 2 + 4;

/// 轮换 offer 固定开销。
pub const OFFER_OVERHEAD: usize = 16 + 16 + 8 + 16 + 24 + 4;

/// kind 编码。
pub const KIND_RECORD: u8 = 0;
/// kind 编码。
pub const KIND_OFFER: u8 = 1;
/// kind 编码。
pub const KIND_ACK: u8 = 2;
/// kind 编码。
pub const KIND_CURRENT: u8 = 3;
/// kind 编码。
pub const KIND_MIGRATED: u8 = 4;

/// 墓碑标志。
pub const FLAG_DELETED: u8 = 0b0000_0001;

/// 编码一条变更。
pub fn encode(entry: &ChangeEntry) -> Result<Vec<u8>> {
    match entry {
        ChangeEntry::Record(record) => encode_record(record),
        ChangeEntry::RotationOffer(offer) => encode_offer(offer),
        ChangeEntry::RotationAck { to, device, at } => {
            let mut out = header(KIND_ACK, 0, 16 + 16 + 8);
            out.extend_from_slice(to.as_bytes());
            out.extend_from_slice(device.as_bytes());
            out.extend_from_slice(&at.as_millis().to_be_bytes());
            Ok(out)
        }
        ChangeEntry::CurrentKey { key_id, at } => {
            let mut out = header(KIND_CURRENT, 0, 16 + 8);
            out.extend_from_slice(key_id.as_bytes());
            out.extend_from_slice(&at.as_millis().to_be_bytes());
            Ok(out)
        }
        ChangeEntry::Migrated { key_id, at } => {
            let mut out = header(KIND_MIGRATED, 0, 16 + 8);
            out.extend_from_slice(key_id.as_bytes());
            out.extend_from_slice(&at.as_millis().to_be_bytes());
            Ok(out)
        }
    }
}

fn encode_record(record: &RecordChange) -> Result<Vec<u8>> {
    let key = codec::encode_record_key(&record.namespace, &record.collection, &record.id)?;
    let key_len = u16::try_from(key.len())
        .map_err(|_| Error::InvalidArgument("record key too long for change log"))?;
    let blob_len = u32::try_from(record.blob.len())
        .map_err(|_| Error::InvalidArgument("record blob too long for change log"))?;
    let flags = if record.deleted { FLAG_DELETED } else { 0 };
    let mut out = header(KIND_RECORD, flags, 48 + key.len() + 4 + record.blob.len());
    out.extend_from_slice(record.key_id.as_bytes());
    out.extend_from_slice(&record.rev.to_be_bytes());
    out.extend_from_slice(&record.updated_at.as_millis().to_be_bytes());
    out.extend_from_slice(record.writer.as_bytes());
    out.extend_from_slice(&key_len.to_be_bytes());
    out.extend_from_slice(&key);
    out.extend_from_slice(&blob_len.to_be_bytes());
    out.extend_from_slice(&record.blob);
    Ok(out)
}

fn encode_offer(offer: &RotationOffer) -> Result<Vec<u8>> {
    let ct_len = u32::try_from(offer.wrapped_dk.len())
        .map_err(|_| Error::InvalidArgument("wrapped key too long"))?;
    let mut out = header(KIND_OFFER, 0, 80 + offer.wrapped_dk.len());
    out.extend_from_slice(offer.from.as_bytes());
    out.extend_from_slice(offer.to.as_bytes());
    out.extend_from_slice(&offer.created_at.as_millis().to_be_bytes());
    out.extend_from_slice(offer.origin.as_bytes());
    out.extend_from_slice(&offer.wrap_nonce);
    out.extend_from_slice(&ct_len.to_be_bytes());
    out.extend_from_slice(&offer.wrapped_dk);
    Ok(out)
}

fn header(kind: u8, flags: u8, body_len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + body_len);
    out.extend_from_slice(MAGIC);
    out.push(FORMAT);
    out.push(kind);
    out.push(flags);
    out.push(0);
    out
}

/// 解码一条变更，并校验记录条目与 `RecordBlob` 的一致性。
pub fn decode(bytes: &[u8]) -> Result<ChangeEntry> {
    if bytes.len() < HEADER_LEN {
        return Err(Error::Corrupted("change blob too short"));
    }
    if &bytes[0..4] != MAGIC {
        return Err(Error::Corrupted("change magic mismatch"));
    }
    if bytes[4] != FORMAT {
        return Err(Error::UnsupportedFormat {
            found: u16::from(bytes[4]),
            supported: u16::from(FORMAT),
        });
    }
    let kind = bytes[5];
    let flags = bytes[6];
    if bytes[7] != 0 {
        return Err(Error::Corrupted("change reserved byte is set"));
    }
    let body = bytes
        .get(HEADER_LEN..)
        .ok_or(Error::Corrupted("change body truncated"))?;

    match kind {
        KIND_RECORD => decode_record(flags, body),
        KIND_OFFER => decode_offer(flags, body),
        KIND_ACK => {
            if flags != 0 {
                return Err(Error::Corrupted("change flags are reserved"));
            }
            expect_len(body, 40, "ack body")?;
            Ok(ChangeEntry::RotationAck {
                to: read_key_id(&body[0..16])?,
                device: read_device_id(&body[16..32])?,
                at: Timestamp::from_millis(read_i64(&body[32..40])?),
            })
        }
        KIND_CURRENT => {
            if flags != 0 {
                return Err(Error::Corrupted("change flags are reserved"));
            }
            expect_len(body, 24, "current key body")?;
            Ok(ChangeEntry::CurrentKey {
                key_id: read_key_id(&body[0..16])?,
                at: Timestamp::from_millis(read_i64(&body[16..24])?),
            })
        }
        KIND_MIGRATED => {
            if flags != 0 {
                return Err(Error::Corrupted("change flags are reserved"));
            }
            expect_len(body, 24, "migrated body")?;
            Ok(ChangeEntry::Migrated {
                key_id: read_key_id(&body[0..16])?,
                at: Timestamp::from_millis(read_i64(&body[16..24])?),
            })
        }
        _ => Err(Error::Corrupted("unknown change kind")),
    }
}

fn decode_record(flags: u8, body: &[u8]) -> Result<ChangeEntry> {
    if flags & !FLAG_DELETED != 0 {
        return Err(Error::Corrupted("record change has reserved flags"));
    }
    if body.len() < 50 {
        return Err(Error::Corrupted("record change body too short"));
    }
    let key_id = read_key_id(&body[0..16])?;
    let rev = read_u64(&body[16..24])?;
    let updated_at = Timestamp::from_millis(read_i64(&body[24..32])?);
    let writer = read_device_id(&body[32..48])?;
    let key_len = usize::from(read_u16(&body[48..50])?);
    let key_end = 50usize
        .checked_add(key_len)
        .ok_or(Error::Corrupted("record key length overflow"))?;
    let key = body
        .get(50..key_end)
        .ok_or(Error::Corrupted("record key truncated"))?;
    let blob_len_end = key_end
        .checked_add(4)
        .ok_or(Error::Corrupted("record blob length overflow"))?;
    let blob_len = usize::try_from(read_u32(
        body.get(key_end..blob_len_end)
            .ok_or(Error::Corrupted("record blob length truncated"))?,
    )?)
    .map_err(|_| Error::Corrupted("record blob length does not fit in memory"))?;
    let blob_end = blob_len_end
        .checked_add(blob_len)
        .ok_or(Error::Corrupted("record blob length overflow"))?;
    if blob_end != body.len() {
        return Err(Error::Corrupted("record change body length mismatch"));
    }
    let blob = body
        .get(blob_len_end..blob_end)
        .ok_or(Error::Corrupted("record blob truncated"))?;
    let decoded_blob = codec2::RecordBlob::decode(blob)?;
    let header = decoded_blob.header;
    if header.key_id != key_id
        || header.rev != rev
        || header.updated_at != updated_at.as_millis()
        || header.is_deleted() != (flags & FLAG_DELETED != 0)
    {
        return Err(Error::Corrupted(
            "record change does not match record blob header",
        ));
    }
    let (namespace, collection, id) = codec::decode_record_key(key)?;
    Ok(ChangeEntry::Record(RecordChange {
        namespace,
        collection,
        id,
        key_id,
        rev,
        updated_at,
        deleted: header.is_deleted(),
        writer,
        blob: blob.to_vec(),
    }))
}

fn decode_offer(flags: u8, body: &[u8]) -> Result<ChangeEntry> {
    if flags != 0 {
        return Err(Error::Corrupted("change flags are reserved"));
    }
    if body.len() < 84 {
        return Err(Error::Corrupted("rotation offer body too short"));
    }
    let from = read_key_id(&body[0..16])?;
    let to = read_key_id(&body[16..32])?;
    if from.is_zero() || to.is_zero() || from == to {
        return Err(Error::Corrupted("rotation offer key ids are invalid"));
    }
    let created_at = Timestamp::from_millis(read_i64(&body[32..40])?);
    let origin = read_device_id(&body[40..56])?;
    let wrap_nonce: [u8; 24] = body[56..80]
        .try_into()
        .map_err(|_| Error::Corrupted("rotation offer nonce truncated"))?;
    let ct_len = usize::try_from(read_u32(&body[80..84])?)
        .map_err(|_| Error::Corrupted("rotation ciphertext length does not fit in memory"))?;
    let expected = 84usize
        .checked_add(ct_len)
        .ok_or(Error::Corrupted("rotation ciphertext length overflow"))?;
    if expected != body.len() || ct_len < codec2::TAG_LEN {
        return Err(Error::Corrupted("rotation ciphertext length mismatch"));
    }
    Ok(ChangeEntry::RotationOffer(RotationOffer {
        from,
        to,
        created_at,
        origin,
        wrap_nonce,
        wrapped_dk: body[84..].to_vec(),
    }))
}

fn expect_len(body: &[u8], expected: usize, what: &'static str) -> Result<()> {
    if body.len() == expected {
        Ok(())
    } else {
        Err(Error::Corrupted(what))
    }
}

fn read_key_id(bytes: &[u8]) -> Result<KeyId> {
    let bytes: [u8; 16] = bytes
        .try_into()
        .map_err(|_| Error::Corrupted("key id truncated"))?;
    let key = KeyId::from_bytes(bytes);
    if key.is_zero() {
        return Err(Error::Corrupted("key id must not be zero"));
    }
    Ok(key)
}

fn read_device_id(bytes: &[u8]) -> Result<DeviceId> {
    let bytes: [u8; 16] = bytes
        .try_into()
        .map_err(|_| Error::Corrupted("device id truncated"))?;
    Ok(DeviceId::from_bytes(bytes))
}

fn read_u16(bytes: &[u8]) -> Result<u16> {
    let bytes: [u8; 2] = bytes
        .try_into()
        .map_err(|_| Error::Corrupted("u16 truncated"))?;
    Ok(u16::from_be_bytes(bytes))
}

fn read_u32(bytes: &[u8]) -> Result<u32> {
    let bytes: [u8; 4] = bytes
        .try_into()
        .map_err(|_| Error::Corrupted("u32 truncated"))?;
    Ok(u32::from_be_bytes(bytes))
}

fn read_u64(bytes: &[u8]) -> Result<u64> {
    let bytes: [u8; 8] = bytes
        .try_into()
        .map_err(|_| Error::Corrupted("u64 truncated"))?;
    Ok(u64::from_be_bytes(bytes))
}

fn read_i64(bytes: &[u8]) -> Result<i64> {
    let bytes: [u8; 8] = bytes
        .try_into()
        .map_err(|_| Error::Corrupted("i64 truncated"))?;
    Ok(i64::from_be_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec2::RecordHeader;
    use crate::model::RecordId;

    fn record_entry() -> ChangeEntry {
        let blob = vec![0u8; 100];
        ChangeEntry::Record(RecordChange {
            namespace: "notes".to_owned(),
            collection: "note".to_owned(),
            id: RecordId::from_bytes([1; 16]),
            key_id: KeyId::from_bytes([2; 16]),
            rev: 3,
            updated_at: Timestamp::from_millis(4),
            deleted: true,
            writer: DeviceId::from_bytes([5; 16]),
            blob,
        })
    }

    #[test]
    fn control_roundtrip() {
        let entries = [
            ChangeEntry::RotationAck {
                to: KeyId::from_bytes([1; 16]),
                device: DeviceId::from_bytes([2; 16]),
                at: Timestamp::from_millis(3),
            },
            ChangeEntry::CurrentKey {
                key_id: KeyId::from_bytes([4; 16]),
                at: Timestamp::from_millis(5),
            },
            ChangeEntry::Migrated {
                key_id: KeyId::from_bytes([6; 16]),
                at: Timestamp::from_millis(7),
            },
        ];
        for entry in entries {
            let bytes = encode(&entry).unwrap();
            assert_eq!(decode(&bytes).unwrap(), entry);
        }
    }

    #[test]
    fn offer_roundtrip() {
        let entry = ChangeEntry::RotationOffer(RotationOffer {
            from: KeyId::from_bytes([1; 16]),
            to: KeyId::from_bytes([2; 16]),
            created_at: Timestamp::from_millis(3),
            origin: DeviceId::from_bytes([4; 16]),
            wrap_nonce: [5u8; 24],
            wrapped_dk: vec![6u8; 48],
        });
        let bytes = encode(&entry).unwrap();
        assert_eq!(decode(&bytes).unwrap(), entry);
        assert!(decode(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn record_roundtrip_and_validation() {
        let header = RecordHeader::new(KeyId::from_bytes([2; 16]), 3, 1, 4, 1).with_deleted(true);
        let mut blob = header.encode().to_vec();
        blob.extend_from_slice(&[1u8; 24]);
        let ct = [7u8; 16];
        blob.extend_from_slice(&(ct.len() as u32).to_be_bytes());
        blob.extend_from_slice(&ct);
        let entry = ChangeEntry::Record(RecordChange {
            namespace: "notes".to_owned(),
            collection: "note".to_owned(),
            id: RecordId::from_bytes([1; 16]),
            key_id: KeyId::from_bytes([2; 16]),
            rev: 3,
            updated_at: Timestamp::from_millis(4),
            deleted: true,
            writer: DeviceId::from_bytes([5; 16]),
            blob,
        });
        let bytes = encode(&entry).unwrap();
        assert_eq!(decode(&bytes).unwrap(), entry);

        let mut bad = bytes.clone();
        bad[HEADER_LEN + 16 + 8 + 8 + 16] ^= 1;
        assert!(decode(&bad).is_err());
    }

    #[test]
    fn rejects_short_and_unknown() {
        assert!(decode(&[]).is_err());
        let mut bytes = encode(&record_entry()).unwrap();
        bytes[5] = 9;
        assert!(decode(&bytes).is_err());
    }
}
