//! `SHY2` 帧编码与解码。
//!
//! 帧头固定 77 字节，`frame_len` 不包含自身 4 字节。变更页与 offer 页共用
//! 帧头；快照页的条目是 `u16 key_len | record_key | u32 blob_len | RecordBlob`。

use shio_core::codec::{decode_record_key, encode_record_key};
use shio_core::codec2;
use shio_core::store::changes;
use shio_core::sync::{
    ChangeEntry, ChangePage, OfferPage, RecordChange, SnapshotPage, SnapshotRecord,
};
use shio_core::{DeviceId, Error, Result};

pub const MAGIC: &[u8; 4] = b"SHY2";
pub const FORMAT: u8 = 2;
pub const HEADER_LEN: usize = 77;
pub const KIND_CHANGES: u8 = 0;
pub const KIND_OFFERS: u8 = 1;
pub const KIND_SNAPSHOT: u8 = 2;
pub const KIND_PROGRESS: u8 = 3;

const FRAME_LEN_OFFSET: usize = 0;
const MAGIC_OFFSET: usize = 4;
const FORMAT_OFFSET: usize = 8;
const KIND_OFFSET: usize = 9;
const FLAGS_OFFSET: usize = 10;
const DB_OFFSET: usize = 11;
const SENDER_OFFSET: usize = 27;
const AFTER_OFFSET: usize = 43;
const LOG_START_OFFSET: usize = 51;
const LOG_END_OFFSET: usize = 59;
const NEXT_OFFSET: usize = 67;
const COUNT_OFFSET: usize = 75;

#[derive(Debug, Clone, Copy)]
pub struct Header {
    pub kind: u8,
    pub db_id: [u8; 16],
    pub sender: DeviceId,
    pub after: u64,
    pub log_start: u64,
    pub log_end: u64,
    pub next: u64,
    pub count: usize,
}

struct FrameMeta {
    kind: u8,
    db_id: [u8; 16],
    sender: DeviceId,
    after: u64,
    log_start: u64,
    log_end: u64,
    next: u64,
}

pub fn encode_change_page(db_id: [u8; 16], page: &ChangePage) -> Result<Vec<u8>> {
    if page.entries.len() > shio_core::sync::MAX_FRAME_ENTRIES {
        return Err(Error::InvalidArgument("too many entries in one frame"));
    }
    let mut entries = Vec::with_capacity(page.entries.len());
    for entry in &page.entries {
        if let ChangeEntry::Record(change) = entry {
            validate_record_change(change)?;
        }
        entries.push(changes::encode(entry)?);
    }
    encode_frame(
        FrameMeta {
            kind: KIND_CHANGES,
            db_id,
            sender: page.sender,
            after: page.after,
            log_start: page.log_start,
            log_end: page.log_end,
            next: page.next,
        },
        entries,
    )
}

pub fn decode_change_page(expected_db: [u8; 16], frame: &[u8]) -> Result<ChangePage> {
    let (header, raw_entries) = parse_frame(frame)?;
    if header.kind != KIND_CHANGES {
        return Err(Error::InvalidArgument("frame is not a change page"));
    }
    if header.db_id != expected_db {
        return Err(Error::InvalidArgument("frame belongs to another database"));
    }
    let mut entries = Vec::with_capacity(raw_entries.len());
    for raw in raw_entries {
        entries.push(changes::decode(raw)?);
    }
    if header.next < header.after || header.next > header.log_end {
        return Err(Error::Corrupted("change page cursor is inconsistent"));
    }
    let has_more = header.next < header.log_end;
    Ok(ChangePage {
        sender: header.sender,
        after: header.after,
        next: header.next,
        has_more,
        log_start: header.log_start,
        log_end: header.log_end,
        entries,
    })
}

/// 进度帧：`sender` 报告自己在接收方日志中的已消费游标（帧头 `next`）。
pub fn encode_progress(db_id: [u8; 16], device: DeviceId, cursor: u64) -> Result<Vec<u8>> {
    encode_frame(
        FrameMeta {
            kind: KIND_PROGRESS,
            db_id,
            sender: device,
            after: 0,
            log_start: 0,
            log_end: 0,
            next: cursor,
        },
        Vec::new(),
    )
}

/// 解码进度帧，返回 `(报告方设备, 已消费游标)`。
pub fn decode_progress(expected_db: [u8; 16], frame: &[u8]) -> Result<(DeviceId, u64)> {
    let (header, entries) = parse_frame(frame)?;
    if header.kind != KIND_PROGRESS {
        return Err(Error::InvalidArgument("frame is not a progress report"));
    }
    if header.db_id != expected_db {
        return Err(Error::InvalidArgument("frame belongs to another database"));
    }
    if header.count != 0 || !entries.is_empty() {
        return Err(Error::Corrupted("progress frame carries entries"));
    }
    Ok((header.sender, header.next))
}

pub fn encode_offers(db_id: [u8; 16], page: &OfferPage) -> Result<Vec<u8>> {
    if page.offers.len() > shio_core::sync::MAX_FRAME_ENTRIES {
        return Err(Error::InvalidArgument("too many offers in one frame"));
    }
    let mut entries = Vec::with_capacity(page.offers.len());
    for offer in &page.offers {
        entries.push(changes::encode(&ChangeEntry::RotationOffer(offer.clone()))?);
    }
    encode_frame(
        FrameMeta {
            kind: KIND_OFFERS,
            db_id,
            sender: page.sender,
            after: 0,
            log_start: 0,
            log_end: 0,
            next: 0,
        },
        entries,
    )
}

pub fn decode_offers(expected_db: [u8; 16], frame: &[u8]) -> Result<OfferPage> {
    let (header, raw_entries) = parse_frame(frame)?;
    if header.kind != KIND_OFFERS {
        return Err(Error::InvalidArgument("frame is not an offer page"));
    }
    if header.db_id != expected_db {
        return Err(Error::InvalidArgument("frame belongs to another database"));
    }
    let mut offers = Vec::with_capacity(raw_entries.len());
    for raw in raw_entries {
        match changes::decode(raw)? {
            ChangeEntry::RotationOffer(offer) => offers.push(offer),
            _ => return Err(Error::Corrupted("offer frame contains a non-offer entry")),
        }
    }
    offers.sort_by_key(|offer| (offer.created_at, offer.to));
    Ok(OfferPage {
        sender: header.sender,
        offers,
    })
}

pub fn encode_snapshot(db_id: [u8; 16], page: &SnapshotPage) -> Result<Vec<u8>> {
    if page.entries.len() > shio_core::sync::MAX_FRAME_ENTRIES {
        return Err(Error::InvalidArgument(
            "too many snapshot records in one frame",
        ));
    }
    let mut entries = Vec::with_capacity(page.entries.len());
    for record in &page.entries {
        let key = encode_record_key(&record.namespace, &record.collection, &record.id)?;
        let key_len = u16::try_from(key.len())
            .map_err(|_| Error::InvalidArgument("record key is too long"))?;
        let blob_len = u32::try_from(record.blob.len())
            .map_err(|_| Error::InvalidArgument("record blob is too large"))?;
        let mut entry = Vec::with_capacity(2 + key.len() + 4 + record.blob.len());
        entry.extend_from_slice(&key_len.to_be_bytes());
        entry.extend_from_slice(&key);
        entry.extend_from_slice(&blob_len.to_be_bytes());
        entry.extend_from_slice(&record.blob);
        entries.push(entry);
    }
    encode_frame(
        FrameMeta {
            kind: KIND_SNAPSHOT,
            db_id,
            sender: DeviceId::from_bytes([0u8; 16]),
            after: 0,
            log_start: page.at_seq,
            log_end: page.at_seq,
            next: u64::from(page.next_key.is_some()),
        },
        entries,
    )
}

pub fn decode_snapshot(expected_db: [u8; 16], frame: &[u8]) -> Result<SnapshotPage> {
    let (header, raw_entries) = parse_frame(frame)?;
    if header.kind != KIND_SNAPSHOT {
        return Err(Error::InvalidArgument("frame is not a snapshot page"));
    }
    if header.db_id != expected_db {
        return Err(Error::InvalidArgument("frame belongs to another database"));
    }
    let mut entries = Vec::new();
    for raw in raw_entries {
        if raw.len() < 2 {
            return Err(Error::Corrupted("snapshot entry is too short"));
        }
        let key_len = usize::from(u16::from_be_bytes([raw[0], raw[1]]));
        let key_end = 2usize
            .checked_add(key_len)
            .ok_or(Error::Corrupted("snapshot key length overflow"))?;
        if key_end + 4 > raw.len() {
            return Err(Error::Corrupted("snapshot entry is truncated"));
        }
        let key = &raw[2..key_end];
        let blob_len = u32::from_be_bytes(
            raw[key_end..key_end + 4]
                .try_into()
                .map_err(|_| Error::Corrupted("snapshot blob length is truncated"))?,
        );
        let blob_start = key_end + 4;
        let blob_end = blob_start
            .checked_add(usize::try_from(blob_len).map_err(|_| Error::Corrupted("blob too large"))?)
            .ok_or(Error::Corrupted("snapshot blob length overflow"))?;
        if blob_end != raw.len() {
            return Err(Error::Corrupted("snapshot entry length mismatch"));
        }
        let (namespace, collection, id) = decode_record_key(key)?;
        codec2::RecordBlob::decode(&raw[blob_start..blob_end])?;
        entries.push(SnapshotRecord {
            namespace,
            collection,
            id,
            blob: raw[blob_start..blob_end].to_vec(),
        });
    }
    let next_key = if header.next != 0 {
        entries
            .last()
            .map(|record| encode_record_key(&record.namespace, &record.collection, &record.id))
            .transpose()?
    } else {
        None
    };
    Ok(SnapshotPage {
        at_seq: header.log_end,
        next_key,
        entries,
    })
}

fn encode_frame(meta: FrameMeta, entries: Vec<Vec<u8>>) -> Result<Vec<u8>> {
    if entries.len() > shio_core::sync::MAX_FRAME_ENTRIES {
        return Err(Error::InvalidArgument("too many entries in one frame"));
    }
    let count = u16::try_from(entries.len())
        .map_err(|_| Error::InvalidArgument("too many entries in one frame"))?;
    let mut body_len = HEADER_LEN - 4;
    for entry in &entries {
        let len = u32::try_from(entry.len())
            .map_err(|_| Error::InvalidArgument("frame entry is too large"))?;
        body_len = body_len
            .checked_add(4 + len as usize)
            .ok_or(Error::InvalidArgument("frame length overflow"))?;
    }
    let total = body_len
        .checked_add(4)
        .ok_or(Error::InvalidArgument("frame length overflow"))?;
    if total > shio_core::sync::MAX_FRAME_BYTES {
        return Err(Error::InvalidArgument("frame exceeds the size limit"));
    }
    let frame_len =
        u32::try_from(body_len).map_err(|_| Error::InvalidArgument("frame length overflow"))?;

    let mut frame = Vec::with_capacity(total);
    frame.extend_from_slice(&frame_len.to_be_bytes());
    frame.extend_from_slice(MAGIC);
    frame.push(FORMAT);
    frame.push(meta.kind);
    frame.push(0);
    frame.extend_from_slice(&meta.db_id);
    frame.extend_from_slice(meta.sender.as_bytes());
    frame.extend_from_slice(&meta.after.to_be_bytes());
    frame.extend_from_slice(&meta.log_start.to_be_bytes());
    frame.extend_from_slice(&meta.log_end.to_be_bytes());
    frame.extend_from_slice(&meta.next.to_be_bytes());
    frame.extend_from_slice(&count.to_be_bytes());
    for entry in entries {
        let len = u32::try_from(entry.len())
            .map_err(|_| Error::InvalidArgument("frame entry is too large"))?;
        frame.extend_from_slice(&len.to_be_bytes());
        frame.extend_from_slice(&entry);
    }
    Ok(frame)
}

fn parse_frame(frame: &[u8]) -> Result<(Header, Vec<&[u8]>)> {
    if frame.len() > shio_core::sync::MAX_FRAME_BYTES {
        return Err(Error::InvalidArgument("frame exceeds the size limit"));
    }
    if frame.len() < HEADER_LEN {
        return Err(Error::Corrupted("frame is shorter than the header"));
    }
    let frame_len = u32::from_be_bytes(
        frame[FRAME_LEN_OFFSET..FRAME_LEN_OFFSET + 4]
            .try_into()
            .map_err(|_| Error::Corrupted("frame length is truncated"))?,
    );
    if usize::try_from(frame_len).map_err(|_| Error::Corrupted("frame length overflow"))?
        != frame.len() - 4
    {
        return Err(Error::Corrupted("frame length does not match the buffer"));
    }
    if &frame[MAGIC_OFFSET..MAGIC_OFFSET + 4] != MAGIC {
        return Err(Error::Corrupted("frame magic mismatch"));
    }
    let format = frame[FORMAT_OFFSET];
    if format != FORMAT {
        return Err(Error::UnsupportedFormat {
            found: u16::from(format),
            supported: u16::from(FORMAT),
        });
    }
    let kind = frame[KIND_OFFSET];
    if !matches!(
        kind,
        KIND_CHANGES | KIND_OFFERS | KIND_SNAPSHOT | KIND_PROGRESS
    ) {
        return Err(Error::InvalidArgument("unknown frame kind"));
    }
    if frame[FLAGS_OFFSET] != 0 {
        return Err(Error::Corrupted("reserved frame flags are not zero"));
    }
    let db_id: [u8; 16] = frame[DB_OFFSET..DB_OFFSET + 16]
        .try_into()
        .map_err(|_| Error::Corrupted("frame db id is truncated"))?;
    let sender: [u8; 16] = frame[SENDER_OFFSET..SENDER_OFFSET + 16]
        .try_into()
        .map_err(|_| Error::Corrupted("frame sender is truncated"))?;
    let sender = DeviceId::from_bytes(sender);
    let after = read_u64(frame, AFTER_OFFSET)?;
    let log_start = read_u64(frame, LOG_START_OFFSET)?;
    let log_end = read_u64(frame, LOG_END_OFFSET)?;
    let next = read_u64(frame, NEXT_OFFSET)?;
    let count = usize::from(u16::from_be_bytes(
        frame[COUNT_OFFSET..COUNT_OFFSET + 2]
            .try_into()
            .map_err(|_| Error::Corrupted("frame count is truncated"))?,
    ));
    if count > shio_core::sync::MAX_FRAME_ENTRIES {
        return Err(Error::InvalidArgument("frame contains too many entries"));
    }

    let mut entries = Vec::with_capacity(count);
    let mut offset = HEADER_LEN;
    for _ in 0..count {
        if offset + 4 > frame.len() {
            return Err(Error::Corrupted("frame entry length is truncated"));
        }
        let len = u32::from_be_bytes(
            frame[offset..offset + 4]
                .try_into()
                .map_err(|_| Error::Corrupted("frame entry length is truncated"))?,
        );
        let len =
            usize::try_from(len).map_err(|_| Error::Corrupted("frame entry length overflow"))?;
        let start = offset + 4;
        let end = start
            .checked_add(len)
            .ok_or(Error::Corrupted("frame entry length overflow"))?;
        if end > frame.len() {
            return Err(Error::Corrupted("frame entry is truncated"));
        }
        entries.push(&frame[start..end]);
        offset = end;
    }
    if offset != frame.len() {
        return Err(Error::Corrupted("frame has trailing bytes"));
    }

    Ok((
        Header {
            kind,
            db_id,
            sender,
            after,
            log_start,
            log_end,
            next,
            count,
        },
        entries,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64> {
    Ok(u64::from_be_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .map_err(|_| Error::Corrupted("frame field is truncated"))?,
    ))
}

/// 帧里的记录变更与记录 blob 的 header 是否一致；供应用前的额外校验使用。
pub fn validate_record_change(change: &RecordChange) -> Result<()> {
    let blob = codec2::RecordBlob::decode(&change.blob)?;
    if blob.header.key_id != change.key_id
        || blob.header.rev != change.rev
        || blob.header.updated_at != change.updated_at.as_millis()
        || blob.header.is_deleted() != change.deleted
    {
        return Err(Error::Corrupted(
            "change entry does not match the record blob",
        ));
    }
    Ok(())
}
