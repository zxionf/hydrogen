//! `format=1` 的字节编码：复合键、记录 header、记录 blob 视图与 AAD。
//!
//! 这一层是落盘契约的实现，对应 `.design/data.md` §2、§7、§8。所有整数大端，
//! 所有长度先校验再切片；输入来自磁盘时一律视为不可信，非法结构返回
//! [`Error::Corrupted`]，越界与整数溢出都不可能发生。

use crate::crypto::keys::{KeyId, MAX_KEY_ID, MIN_KEY_ID};
use crate::error::{Error, Result};
use crate::model::{validate_name, NameKind, RecordId, MAX_NAME_LEN};

/// 记录值开头的魔数。
pub const RECORD_MAGIC: &[u8; 4] = b"SHR1";

/// 当前记录格式版本。
pub const RECORD_FORMAT: u8 = 1;

/// 明文 header 长度（偏移 `0..34`）。
pub const HEADER_LEN: usize = 34;

/// nonce 偏移，紧跟 header。
pub const NONCE_OFFSET: usize = HEADER_LEN;

/// nonce 长度。
pub const NONCE_LEN: usize = 24;

/// `ct_len` 偏移。
pub const CT_LEN_OFFSET: usize = NONCE_OFFSET + NONCE_LEN;

/// `ct_len` 字段宽度。
pub const CT_LEN_SIZE: usize = 4;

/// 密文偏移。
pub const CT_OFFSET: usize = CT_LEN_OFFSET + CT_LEN_SIZE;

/// AEAD tag 长度。
pub const TAG_LEN: usize = 16;

/// 一条记录值的最小长度：header + nonce + ct_len + tag。
pub const MIN_RECORD_LEN: usize = CT_OFFSET + TAG_LEN;

/// 记录 AAD 的域分隔符。
pub const AAD_RECORD_DOMAIN: &[u8] = b"shio/v1/record";

/// 密钥包裹 AAD 的域分隔符。
pub const AAD_KEYWRAP_DOMAIN: &[u8] = b"shio/v1/keywrap";

/// 密钥校验 AAD 的域分隔符。
pub const AAD_KEYCHECK_DOMAIN: &[u8] = b"shio/v1/keycheck";

/// 记录 flags 中的墓碑位。
pub const FLAG_DELETED: u8 = 0b0000_0001;

/// 记录 flags 中保留位（bit1..7），读取时忽略，写入时置 0。
pub const FLAG_RESERVED_MASK: u8 = 0b1111_1110;

/// 记录明文 header。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordHeader {
    /// 记录格式版本，当前为 [`RECORD_FORMAT`]。
    pub format: u8,
    /// 标志位，bit0 为墓碑。
    pub flags: u8,
    /// 加密该记录所用数据密钥的编号。
    pub key_id: u16,
    /// 修订号，从 1 开始。
    pub rev: u64,
    /// 创建时间（Unix 毫秒）。
    pub created_at: i64,
    /// 更新时间（Unix 毫秒）。
    pub updated_at: i64,
    /// 载荷版本，由调用方解释。
    pub data_version: u16,
}

impl RecordHeader {
    /// 构造一条非墓碑记录 header。
    pub const fn new(
        key_id: u16,
        rev: u64,
        created_at: i64,
        updated_at: i64,
        data_version: u16,
    ) -> Self {
        Self {
            format: RECORD_FORMAT,
            flags: 0,
            key_id,
            rev,
            created_at,
            updated_at,
            data_version,
        }
    }

    /// 是否为墓碑。
    pub const fn is_deleted(self) -> bool {
        self.flags & FLAG_DELETED != 0
    }

    /// 返回带指定墓碑状态的 header。
    pub const fn with_deleted(mut self, deleted: bool) -> Self {
        if deleted {
            self.flags |= FLAG_DELETED;
        } else {
            self.flags &= !FLAG_DELETED;
        }
        self
    }

    /// 编码为 34 字节明文 header；header 是 AAD 的一部分，原样参与认证。
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[0..4].copy_from_slice(RECORD_MAGIC);
        out[4] = self.format;
        out[5] = self.flags;
        out[6..8].copy_from_slice(&self.key_id.to_be_bytes());
        out[8..16].copy_from_slice(&self.rev.to_be_bytes());
        out[16..24].copy_from_slice(&self.created_at.to_be_bytes());
        out[24..32].copy_from_slice(&self.updated_at.to_be_bytes());
        out[32..34].copy_from_slice(&self.data_version.to_be_bytes());
        out
    }

    /// 从 34 字节解码并校验魔数、格式与 `key_id` 取值范围。
    pub fn decode(bytes: &[u8; HEADER_LEN]) -> Result<Self> {
        if &bytes[0..4] != RECORD_MAGIC {
            return Err(Error::Corrupted("record magic mismatch"));
        }
        let format = bytes[4];
        if format != RECORD_FORMAT {
            return Err(Error::UnsupportedFormat {
                found: u16::from(format),
                supported: u16::from(RECORD_FORMAT),
            });
        }
        let key_id = u16::from_be_bytes([bytes[6], bytes[7]]);
        validate_key_id(key_id).map_err(|_| Error::Corrupted("record key id out of range"))?;

        Ok(Self {
            format,
            flags: bytes[5],
            key_id,
            rev: u64::from_be_bytes(
                bytes[8..16]
                    .try_into()
                    .map_err(|_| Error::Corrupted("record header revision truncated"))?,
            ),
            created_at: i64::from_be_bytes(
                bytes[16..24]
                    .try_into()
                    .map_err(|_| Error::Corrupted("record header created_at truncated"))?,
            ),
            updated_at: i64::from_be_bytes(
                bytes[24..32]
                    .try_into()
                    .map_err(|_| Error::Corrupted("record header updated_at truncated"))?,
            ),
            data_version: u16::from_be_bytes([bytes[32], bytes[33]]),
        })
    }
}

/// 已校验结构的记录值视图。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordBlob<'a> {
    /// 明文 header。
    pub header: RecordHeader,
    /// 该记录独立的 24 字节随机 nonce。
    pub nonce: &'a [u8; NONCE_LEN],
    /// 密文，含 16 字节 tag。
    pub ciphertext: &'a [u8],
}

impl<'a> RecordBlob<'a> {
    /// 解析并校验一条记录值：
    ///
    /// - 长度至少为 [`MIN_RECORD_LEN`]；
    /// - 密文长度不短于 tag；
    /// - `62 + ct_len == value.len()`。
    pub fn decode(value: &'a [u8]) -> Result<Self> {
        if value.len() < MIN_RECORD_LEN {
            return Err(Error::Corrupted("record blob too short"));
        }

        let header_bytes: &[u8; HEADER_LEN] = value
            .get(..HEADER_LEN)
            .and_then(|slice| slice.try_into().ok())
            .ok_or(Error::Corrupted("record header truncated"))?;
        let header = RecordHeader::decode(header_bytes)?;

        let nonce: &[u8; NONCE_LEN] = value
            .get(NONCE_OFFSET..CT_LEN_OFFSET)
            .and_then(|slice| slice.try_into().ok())
            .ok_or(Error::Corrupted("record nonce truncated"))?;

        let ct_len_bytes: [u8; CT_LEN_SIZE] = value
            .get(CT_LEN_OFFSET..CT_OFFSET)
            .and_then(|slice| slice.try_into().ok())
            .ok_or(Error::Corrupted("record ciphertext length truncated"))?;
        let ct_len = u32::from_be_bytes(ct_len_bytes);

        let expected =
            CT_OFFSET
                .checked_add(usize::try_from(ct_len).map_err(|_| {
                    Error::Corrupted("record ciphertext length does not fit in memory")
                })?)
                .ok_or(Error::Corrupted("record ciphertext length overflow"))?;
        if expected != value.len() {
            return Err(Error::Corrupted("record ciphertext length mismatch"));
        }
        if usize::try_from(ct_len).map_err(|_| Error::Corrupted("invalid ct_len"))? < TAG_LEN {
            return Err(Error::Corrupted("record ciphertext shorter than tag"));
        }

        let ciphertext = value
            .get(CT_OFFSET..)
            .ok_or(Error::Corrupted("record ciphertext truncated"))?;

        Ok(Self {
            header,
            nonce,
            ciphertext,
        })
    }

    /// 该记录值占用的字节数。
    pub fn encoded_len(&self) -> usize {
        CT_OFFSET.saturating_add(self.ciphertext.len())
    }
}

/// 编码 `CatalogKey`：`u8 len | ns | u8 len | coll`。
pub fn encode_catalog_key(namespace: &str, collection: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(2 + namespace.len() + collection.len());
    push_name(&mut out, NameKind::Namespace, namespace)?;
    push_name(&mut out, NameKind::Collection, collection)?;
    Ok(out)
}

/// 解析 `CatalogKey`；带尾随字节视为损坏。
pub fn decode_catalog_key(bytes: &[u8]) -> Result<(String, String)> {
    let mut reader = Reader::new(bytes);
    let namespace = reader.name(NameKind::Namespace)?;
    let collection = reader.name(NameKind::Collection)?;
    reader.finish()?;
    Ok((namespace, collection))
}

/// 编码 `RecordKey`：`u8 len | ns | u8 len | coll | 16 字节 UUID`。
pub fn encode_record_key(namespace: &str, collection: &str, id: &RecordId) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(2 + namespace.len() + collection.len() + 16);
    push_name(&mut out, NameKind::Namespace, namespace)?;
    push_name(&mut out, NameKind::Collection, collection)?;
    out.extend_from_slice(id.as_bytes());
    Ok(out)
}

/// 解析 `RecordKey`；带尾随字节视为损坏。
pub fn decode_record_key(bytes: &[u8]) -> Result<(String, String, RecordId)> {
    let mut reader = Reader::new(bytes);
    let namespace = reader.name(NameKind::Namespace)?;
    let collection = reader.name(NameKind::Collection)?;
    let id_bytes: [u8; 16] = reader
        .take(16)?
        .try_into()
        .map_err(|_| Error::Corrupted("record key id truncated"))?;
    reader.finish()?;
    Ok((namespace, collection, RecordId::from_bytes(id_bytes)))
}

/// 构造记录 AAD：
///
/// ```text
/// "shio/v1/record" || u8 ns_len || ns || u8 coll_len || coll || 16 字节 UUID || header[0..34]
/// ```
pub fn record_aad(
    namespace: &str,
    collection: &str,
    id: &RecordId,
    header: &[u8; HEADER_LEN],
) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(
        AAD_RECORD_DOMAIN.len() + 2 + namespace.len() + collection.len() + 16 + HEADER_LEN,
    );
    out.extend_from_slice(AAD_RECORD_DOMAIN);
    push_name(&mut out, NameKind::Namespace, namespace)?;
    push_name(&mut out, NameKind::Collection, collection)?;
    out.extend_from_slice(id.as_bytes());
    out.extend_from_slice(header);
    Ok(out)
}

/// 构造密钥包裹 AAD：`"shio/v1/keywrap" || u16 key_id`。
pub fn keywrap_aad(key_id: KeyId) -> Vec<u8> {
    let mut out = Vec::with_capacity(AAD_KEYWRAP_DOMAIN.len() + 2);
    out.extend_from_slice(AAD_KEYWRAP_DOMAIN);
    out.extend_from_slice(&key_id.get().to_be_bytes());
    out
}

/// 密钥校验 AAD：固定域分隔符。
pub const fn keycheck_aad() -> &'static [u8] {
    AAD_KEYCHECK_DOMAIN
}

/// 校验 `key_id` 是否落在 `1..=65534`。
pub const fn validate_key_id(key_id: u16) -> Result<()> {
    if key_id < MIN_KEY_ID || key_id > MAX_KEY_ID {
        return Err(Error::InvalidArgument("key id must be within 1..=65534"));
    }
    Ok(())
}

fn push_name(out: &mut Vec<u8>, kind: NameKind, name: &str) -> Result<()> {
    validate_name(kind, name)?;
    let len = u8::try_from(name.len())
        .map_err(|_| Error::InvalidArgument("name length does not fit in one byte"))?;
    out.push(len);
    out.extend_from_slice(name.as_bytes());
    Ok(())
}

/// 顺序读取复合键的游标。
struct Reader<'a> {
    input: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    const fn new(input: &'a [u8]) -> Self {
        Self { input, position: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(len)
            .ok_or(Error::Corrupted("key offset overflow"))?;
        let slice = self
            .input
            .get(self.position..end)
            .ok_or(Error::Corrupted("key truncated"))?;
        self.position = end;
        Ok(slice)
    }

    fn name(&mut self, kind: NameKind) -> Result<String> {
        let len = usize::from(self.take(1)?[0]);
        if len == 0 || len > MAX_NAME_LEN {
            return Err(Error::Corrupted("key name length out of range"));
        }
        let bytes = self.take(len)?;
        let text = std::str::from_utf8(bytes)
            .map_err(|_| Error::Corrupted("key name is not valid UTF-8"))?;
        validate_name(kind, text)
            .map_err(|_| Error::Corrupted("key name violates naming rules"))?;
        Ok(text.to_owned())
    }

    fn finish(&self) -> Result<()> {
        if self.position == self.input.len() {
            Ok(())
        } else {
            Err(Error::Corrupted("key has trailing bytes"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_id() -> RecordId {
        RecordId::from_bytes([0x01; 16])
    }

    fn sample_blob(ciphertext: &[u8]) -> Vec<u8> {
        let header = RecordHeader::new(1, 3, 1_700_000_000_000, 1_700_000_001_000, 1);
        let mut out = header.encode().to_vec();
        out.extend_from_slice(&[0xAB; NONCE_LEN]);
        let len = u32::try_from(ciphertext.len()).unwrap();
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(ciphertext);
        out
    }

    #[test]
    fn catalog_key_roundtrip_is_unambiguous() {
        let key = encode_catalog_key("notes", "note").unwrap();
        assert_eq!(key, b"\x05notes\x04note");
        assert_eq!(
            decode_catalog_key(&key).unwrap(),
            ("notes".to_owned(), "note".to_owned())
        );

        // 长度前缀保证 "a"+"b/c" 与 "a/b"+"c" 不会编码成同一串。
        let left = encode_catalog_key("a", "b.c").unwrap();
        let right = encode_catalog_key("a.b", "c").unwrap();
        assert_ne!(left, right);
    }

    #[test]
    fn catalog_key_rejects_bad_input() {
        assert!(decode_catalog_key(b"").is_err());
        assert!(decode_catalog_key(b"\x05notes\x04not").is_err());
        assert!(decode_catalog_key(b"\x05notes\x04note\x00").is_err());
        assert!(decode_catalog_key(b"\x05Notes\x04note").is_err());
        assert!(decode_catalog_key(b"\x00\x04note").is_err());
    }

    #[test]
    fn record_key_roundtrip() {
        let id = sample_id();
        let key = encode_record_key("notes", "note", &id).unwrap();
        assert_eq!(key.len(), 1 + 5 + 1 + 4 + 16);
        let (ns, coll, restored) = decode_record_key(&key).unwrap();
        assert_eq!(ns, "notes");
        assert_eq!(coll, "note");
        assert_eq!(restored, id);

        assert!(decode_record_key(&key[..key.len() - 1]).is_err());
        assert!(decode_record_key(&[key.as_slice(), b"\x00"].concat()).is_err());
    }

    #[test]
    fn header_roundtrip() {
        let header = RecordHeader::new(7, 9, 100, 200, 3).with_deleted(true);
        let bytes = header.encode();
        assert_eq!(bytes.len(), HEADER_LEN);
        assert_eq!(&bytes[0..4], RECORD_MAGIC);
        assert_eq!(RecordHeader::decode(&bytes).unwrap(), header);
        assert!(RecordHeader::decode(&bytes).unwrap().is_deleted());
        assert_eq!(header.with_deleted(false).flags & FLAG_DELETED, 0);
    }

    #[test]
    fn header_rejects_bad_fields() {
        let mut bytes = RecordHeader::new(1, 1, 0, 0, 1).encode();
        bytes[0] = b'X';
        assert!(matches!(
            RecordHeader::decode(&bytes),
            Err(Error::Corrupted(_))
        ));

        let mut bytes = RecordHeader::new(1, 1, 0, 0, 1).encode();
        bytes[4] = 2;
        assert!(matches!(
            RecordHeader::decode(&bytes),
            Err(Error::UnsupportedFormat { found: 2, .. })
        ));

        let mut bytes = RecordHeader::new(1, 1, 0, 0, 1).encode();
        bytes[6..8].copy_from_slice(&0u16.to_be_bytes());
        assert!(matches!(
            RecordHeader::decode(&bytes),
            Err(Error::Corrupted(_))
        ));
    }

    #[test]
    fn blob_view_validates_lengths() {
        let blob = sample_blob(&[0u8; TAG_LEN]);
        let view = RecordBlob::decode(&blob).unwrap();
        assert_eq!(view.header.rev, 3);
        assert_eq!(view.ciphertext.len(), TAG_LEN);
        assert_eq!(view.encoded_len(), blob.len());

        assert!(RecordBlob::decode(&blob[..MIN_RECORD_LEN - 1]).is_err());

        let mut truncated = blob.clone();
        truncated.pop();
        assert!(matches!(
            RecordBlob::decode(&truncated),
            Err(Error::Corrupted(_))
        ));

        let mut short_tag = sample_blob(&[0u8; TAG_LEN - 1]);
        assert!(RecordBlob::decode(&short_tag).is_err());
        short_tag.clear();
    }

    #[test]
    fn blob_rejects_absurd_ct_len() {
        let mut blob = sample_blob(&[0u8; TAG_LEN]);
        blob[CT_LEN_OFFSET..CT_OFFSET].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(matches!(
            RecordBlob::decode(&blob),
            Err(Error::Corrupted(_))
        ));
    }

    #[test]
    fn aad_binds_every_component() {
        let header = RecordHeader::new(1, 1, 10, 10, 1).encode();
        let base = record_aad("notes", "note", &sample_id(), &header).unwrap();
        assert!(base.starts_with(AAD_RECORD_DOMAIN));

        let other_ns = record_aad("memo", "note", &sample_id(), &header).unwrap();
        let other_coll = record_aad("notes", "tag", &sample_id(), &header).unwrap();
        let other_id =
            record_aad("notes", "note", &RecordId::from_bytes([0x02; 16]), &header).unwrap();
        let mut tampered = header;
        tampered[8] ^= 0x01;
        let other_header = record_aad("notes", "note", &sample_id(), &tampered).unwrap();

        for candidate in [other_ns, other_coll, other_id, other_header] {
            assert_ne!(base, candidate);
            assert!(candidate.starts_with(AAD_RECORD_DOMAIN));
        }
    }

    #[test]
    fn keywrap_and_keycheck_aad() {
        let first = KeyId::new(1).unwrap();
        let second = KeyId::new(2).unwrap();
        assert_ne!(keywrap_aad(first), keywrap_aad(second));
        assert_eq!(keywrap_aad(first).len(), AAD_KEYWRAP_DOMAIN.len() + 2);
        assert_eq!(keycheck_aad(), b"shio/v1/keycheck");
    }

    #[test]
    fn key_id_range() {
        assert!(validate_key_id(1).is_ok());
        assert!(validate_key_id(MAX_KEY_ID).is_ok());
        assert!(validate_key_id(0).is_err());
        assert!(validate_key_id(MAX_KEY_ID + 1).is_err());
    }
}
