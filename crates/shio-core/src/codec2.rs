//! `format=2` 的记录字节编码与 AAD。
//!
//! 与 [`crate::codec`] 的 v1 编码并列：`RecordKey` 与 catalog 编码保持不变，
//! 只有 meta / key / record 的字段宽度和 AAD 域分隔改变。所有输入仍按不可信
//! 处理，长度先校验再切片。

use crate::codec::NONCE_LEN;
use crate::error::{Error, Result};
use crate::model::validate_name;
use crate::model::{KeyId, NameKind, RecordId};

/// 记录值开头的魔数。
pub const RECORD_MAGIC: &[u8; 4] = b"SHR2";

/// 记录格式版本。
pub const RECORD_FORMAT: u8 = 2;

/// 明文 header 长度（偏移 `0..48`）。
pub const HEADER_LEN: usize = 48;

/// nonce 偏移。
pub const NONCE_OFFSET: usize = HEADER_LEN;

/// `ct_len` 偏移。
pub const CT_LEN_OFFSET: usize = NONCE_OFFSET + NONCE_LEN;

/// `ct_len` 字段宽度。
pub const CT_LEN_SIZE: usize = 4;

/// 密文偏移。
pub const CT_OFFSET: usize = CT_LEN_OFFSET + CT_LEN_SIZE;

/// AEAD tag 长度。
pub const TAG_LEN: usize = 16;

/// 一条记录值的最小长度。
pub const MIN_RECORD_LEN: usize = CT_OFFSET + TAG_LEN;

/// 记录 AAD 域分隔符。
pub const AAD_RECORD_DOMAIN: &[u8] = b"shio/v2/record";

/// 密钥包裹 AAD 域分隔符。
pub const AAD_KEYWRAP_DOMAIN: &[u8] = b"shio/v2/keywrap";

/// 密钥校验 AAD 域分隔符。
pub const AAD_KEYCHECK_DOMAIN: &[u8] = b"shio/v2/keycheck";

/// 轮换包裹 AAD 域分隔符。
pub const AAD_ROTATION_DOMAIN: &[u8] = b"shio/v2/rotation";

/// 配对包 AAD 域分隔符。
pub const AAD_PAIRING_DOMAIN: &[u8] = b"shio/v2/pairing";

/// key check 的固定明文。
pub const KEY_CHECK_PLAINTEXT: &[u8; 16] = b"shio-keycheck-v2";

/// 记录 flags 中的墓碑位。
pub const FLAG_DELETED: u8 = 0b0000_0001;

/// 记录 flags 的保留位。
pub const FLAG_RESERVED_MASK: u8 = 0b1111_1110;

/// `format=2` 的记录 header。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordHeader {
    /// 记录格式版本，当前为 2。
    pub format: u8,
    /// 标志位，bit0 为墓碑。
    pub flags: u8,
    /// 加密该记录所用数据密钥。
    pub key_id: KeyId,
    /// 修订号，从 1 开始。
    pub rev: u64,
    /// 创建时间（Unix 毫秒）。
    pub created_at: i64,
    /// 更新时间（Unix 毫秒）。
    pub updated_at: i64,
    /// 载荷版本。
    pub data_version: u16,
}

impl RecordHeader {
    /// 构造一条非墓碑 header。
    pub const fn new(
        key_id: KeyId,
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

    /// 返回指定墓碑状态的 header。
    pub const fn with_deleted(mut self, deleted: bool) -> Self {
        if deleted {
            self.flags |= FLAG_DELETED;
        } else {
            self.flags &= !FLAG_DELETED;
        }
        self
    }

    /// 编码为 48 字节明文 header。
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[0..4].copy_from_slice(RECORD_MAGIC);
        out[4] = self.format;
        out[5] = self.flags;
        out[6..22].copy_from_slice(self.key_id.as_bytes());
        out[22..30].copy_from_slice(&self.rev.to_be_bytes());
        out[30..38].copy_from_slice(&self.created_at.to_be_bytes());
        out[38..46].copy_from_slice(&self.updated_at.to_be_bytes());
        out[46..48].copy_from_slice(&self.data_version.to_be_bytes());
        out
    }

    /// 从 48 字节解码并校验。
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
        if bytes[5] & FLAG_RESERVED_MASK != 0 {
            return Err(Error::Corrupted("record reserved flags are set"));
        }
        let key_id_bytes: [u8; 16] = bytes[6..22]
            .try_into()
            .map_err(|_| Error::Corrupted("record key id truncated"))?;
        let key_id = KeyId::from_bytes(key_id_bytes);
        if key_id.is_zero() {
            return Err(Error::Corrupted("record key id must not be zero"));
        }

        Ok(Self {
            format,
            flags: bytes[5],
            key_id,
            rev: u64::from_be_bytes(
                bytes[22..30]
                    .try_into()
                    .map_err(|_| Error::Corrupted("record header revision truncated"))?,
            ),
            created_at: i64::from_be_bytes(
                bytes[30..38]
                    .try_into()
                    .map_err(|_| Error::Corrupted("record header created_at truncated"))?,
            ),
            updated_at: i64::from_be_bytes(
                bytes[38..46]
                    .try_into()
                    .map_err(|_| Error::Corrupted("record header updated_at truncated"))?,
            ),
            data_version: u16::from_be_bytes([bytes[46], bytes[47]]),
        })
    }
}

/// 已校验结构的 v2 记录值视图。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordBlob<'a> {
    /// 明文 header。
    pub header: RecordHeader,
    /// 24 字节 nonce。
    pub nonce: &'a [u8; NONCE_LEN],
    /// 密文，含 16 字节 tag。
    pub ciphertext: &'a [u8],
}

impl<'a> RecordBlob<'a> {
    /// 解析并校验一条 v2 记录值。
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
        let ct_len = usize::try_from(ct_len)
            .map_err(|_| Error::Corrupted("record ciphertext length does not fit in memory"))?;
        if ct_len < TAG_LEN {
            return Err(Error::Corrupted("record ciphertext shorter than tag"));
        }
        let expected = CT_OFFSET
            .checked_add(ct_len)
            .ok_or(Error::Corrupted("record ciphertext length overflow"))?;
        if expected != value.len() {
            return Err(Error::Corrupted("record ciphertext length mismatch"));
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

/// 构造 v2 记录 AAD。
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

/// 构造 v2 密钥包裹 AAD。
pub fn keywrap_aad(key_id: KeyId) -> Vec<u8> {
    let mut out = Vec::with_capacity(AAD_KEYWRAP_DOMAIN.len() + 16);
    out.extend_from_slice(AAD_KEYWRAP_DOMAIN);
    out.extend_from_slice(key_id.as_bytes());
    out
}

/// v2 密钥校验 AAD。
pub const fn keycheck_aad() -> &'static [u8] {
    AAD_KEYCHECK_DOMAIN
}

/// 构造轮换包裹 AAD。
pub fn rotation_aad(db_id: [u8; 16], from: KeyId, to: KeyId) -> Vec<u8> {
    let mut out = Vec::with_capacity(AAD_ROTATION_DOMAIN.len() + 16 + 16 + 16);
    out.extend_from_slice(AAD_ROTATION_DOMAIN);
    out.extend_from_slice(&db_id);
    out.extend_from_slice(from.as_bytes());
    out.extend_from_slice(to.as_bytes());
    out
}

fn push_name(out: &mut Vec<u8>, kind: NameKind, name: &str) -> Result<()> {
    validate_name(kind, name)?;
    let len = u8::try_from(name.len())
        .map_err(|_| Error::InvalidArgument("name length does not fit in one byte"))?;
    out.push(len);
    out.extend_from_slice(name.as_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_id(value: u8) -> KeyId {
        KeyId::from_bytes([value; 16])
    }

    #[test]
    fn header_roundtrip_and_bad_fields() {
        let header = RecordHeader::new(key_id(7), 9, 100, 200, 3).with_deleted(true);
        let bytes = header.encode();
        assert_eq!(RecordHeader::decode(&bytes).unwrap(), header);

        let mut bad = bytes;
        bad[0] = b'X';
        assert!(RecordHeader::decode(&bad).is_err());

        let mut bad = bytes;
        bad[5] = FLAG_RESERVED_MASK;
        assert!(RecordHeader::decode(&bad).is_err());

        let mut bad = bytes;
        bad[6..22].fill(0);
        assert!(RecordHeader::decode(&bad).is_err());
    }

    #[test]
    fn blob_view_validates_lengths() {
        let header = RecordHeader::new(key_id(1), 1, 1, 2, 1);
        let mut raw = header.encode().to_vec();
        raw.extend_from_slice(&[1u8; NONCE_LEN]);
        let ct = [2u8; TAG_LEN];
        raw.extend_from_slice(&(ct.len() as u32).to_be_bytes());
        raw.extend_from_slice(&ct);

        let blob = RecordBlob::decode(&raw).unwrap();
        assert_eq!(blob.header, header);
        assert_eq!(blob.encoded_len(), raw.len());
        assert!(RecordBlob::decode(&raw[..raw.len() - 1]).is_err());
    }

    #[test]
    fn aad_domains_are_distinct() {
        let id = RecordId::from_bytes([3; 16]);
        let header = RecordHeader::new(key_id(1), 1, 0, 0, 1).encode();
        let aad = record_aad("notes", "note", &id, &header).unwrap();
        assert!(aad.starts_with(AAD_RECORD_DOMAIN));
        assert_ne!(aad, record_aad("notes", "other", &id, &header).unwrap());
        assert_eq!(keywrap_aad(key_id(1)).len(), AAD_KEYWRAP_DOMAIN.len() + 16);
        assert_eq!(keycheck_aad(), b"shio/v2/keycheck");
    }
}
