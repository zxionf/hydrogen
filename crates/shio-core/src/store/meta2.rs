//! `format=2` 的 `shio.meta` 值。
//!
//! ```text
//! magic "SHIO" | format u16 | key_id 16 | created_at i64
//! | app_id_len u8 | app_id | key_check_nonce [24] | key_check_ct [32]
//! ```

use redb::TableDefinition;

use crate::codec::{NONCE_LEN, TAG_LEN};
use crate::error::{Error, Result};
use crate::model::KeyId;

/// 与 v1 共用 `shio.meta` 表。
pub const TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("shio.meta");

/// 魔数。
pub const MAGIC: &[u8; 4] = b"SHIO";

/// 主格式版本。
pub const FORMAT: u16 = 2;

/// key check 密文长度。
pub const KEY_CHECK_CT_LEN: usize = 16 + TAG_LEN;

/// `app_id` 最大字节数。
pub const MAX_APP_ID_LEN: usize = 64;

/// 不含 `app_id` 的固定长度。
pub const FIXED_LEN: usize = 4 + 2 + 16 + 8 + 1 + NONCE_LEN + KEY_CHECK_CT_LEN;

const FORMAT_OFFSET: usize = 4;
const KEY_ID_OFFSET: usize = 6;
const CREATED_AT_OFFSET: usize = 22;
const APP_ID_LEN_OFFSET: usize = 30;
const APP_ID_OFFSET: usize = 31;

/// `format=2` 的 meta 值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaBlob {
    /// 主格式版本，必须为 2。
    pub format: u16,
    /// 当前写 key。
    pub key_id: KeyId,
    /// 库创建时间。
    pub created_at: i64,
    /// 应用标识。
    pub app_id: String,
    /// key check nonce。
    pub key_check_nonce: [u8; NONCE_LEN],
    /// key check 密文。
    pub key_check_ct: [u8; KEY_CHECK_CT_LEN],
}

impl MetaBlob {
    /// 构造并校验。
    pub fn new(
        key_id: KeyId,
        created_at: i64,
        app_id: &str,
        key_check_nonce: [u8; NONCE_LEN],
        key_check_ct: [u8; KEY_CHECK_CT_LEN],
    ) -> Result<Self> {
        if key_id.is_zero() {
            return Err(Error::InvalidArgument("key id must not be zero"));
        }
        if app_id.len() > MAX_APP_ID_LEN {
            return Err(Error::InvalidArgument("app id exceeds 64 bytes"));
        }
        Ok(Self {
            format: FORMAT,
            key_id,
            created_at,
            app_id: app_id.to_owned(),
            key_check_nonce,
            key_check_ct,
        })
    }

    /// 编码。
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.format != FORMAT {
            return Err(Error::UnsupportedFormat {
                found: self.format,
                supported: FORMAT,
            });
        }
        if self.key_id.is_zero() {
            return Err(Error::InvalidArgument("key id must not be zero"));
        }
        let app_id_len = u8::try_from(self.app_id.len())
            .map_err(|_| Error::InvalidArgument("app id exceeds 64 bytes"))?;
        if usize::from(app_id_len) > MAX_APP_ID_LEN {
            return Err(Error::InvalidArgument("app id exceeds 64 bytes"));
        }

        let mut out = Vec::with_capacity(FIXED_LEN + self.app_id.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&FORMAT.to_be_bytes());
        out.extend_from_slice(self.key_id.as_bytes());
        out.extend_from_slice(&self.created_at.to_be_bytes());
        out.push(app_id_len);
        out.extend_from_slice(self.app_id.as_bytes());
        out.extend_from_slice(&self.key_check_nonce);
        out.extend_from_slice(&self.key_check_ct);
        Ok(out)
    }

    /// 解码并校验。
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < FIXED_LEN {
            return Err(Error::Corrupted("meta blob too short"));
        }
        if &bytes[0..4] != MAGIC {
            return Err(Error::Corrupted("meta magic mismatch"));
        }
        let format = u16::from_be_bytes([bytes[FORMAT_OFFSET], bytes[FORMAT_OFFSET + 1]]);
        if format != FORMAT {
            return Err(Error::UnsupportedFormat {
                found: format,
                supported: FORMAT,
            });
        }
        let key_id: [u8; 16] = bytes[KEY_ID_OFFSET..CREATED_AT_OFFSET]
            .try_into()
            .map_err(|_| Error::Corrupted("meta key id truncated"))?;
        let key_id = KeyId::from_bytes(key_id);
        if key_id.is_zero() {
            return Err(Error::Corrupted("meta key id must not be zero"));
        }
        let created_at = i64::from_be_bytes(
            bytes[CREATED_AT_OFFSET..APP_ID_LEN_OFFSET]
                .try_into()
                .map_err(|_| Error::Corrupted("meta created_at truncated"))?,
        );
        let app_id_len = usize::from(bytes[APP_ID_LEN_OFFSET]);
        if app_id_len > MAX_APP_ID_LEN {
            return Err(Error::Corrupted("meta app id length out of range"));
        }
        let expected = FIXED_LEN
            .checked_add(app_id_len)
            .ok_or(Error::Corrupted("meta length overflow"))?;
        if expected != bytes.len() {
            return Err(Error::Corrupted("meta blob length mismatch"));
        }
        let app_id_end = APP_ID_OFFSET
            .checked_add(app_id_len)
            .ok_or(Error::Corrupted("meta app id length overflow"))?;
        let app_id = std::str::from_utf8(
            bytes
                .get(APP_ID_OFFSET..app_id_end)
                .ok_or(Error::Corrupted("meta app id truncated"))?,
        )
        .map_err(|_| Error::Corrupted("meta app id is not valid UTF-8"))?
        .to_owned();

        let nonce_end = app_id_end
            .checked_add(NONCE_LEN)
            .ok_or(Error::Corrupted("meta nonce offset overflow"))?;
        let key_check_nonce: [u8; NONCE_LEN] = bytes
            .get(app_id_end..nonce_end)
            .and_then(|slice| slice.try_into().ok())
            .ok_or(Error::Corrupted("meta key check nonce truncated"))?;
        let ct_end = nonce_end
            .checked_add(KEY_CHECK_CT_LEN)
            .ok_or(Error::Corrupted("meta key check offset overflow"))?;
        let key_check_ct: [u8; KEY_CHECK_CT_LEN] = bytes
            .get(nonce_end..ct_end)
            .and_then(|slice| slice.try_into().ok())
            .ok_or(Error::Corrupted("meta key check ciphertext truncated"))?;

        Ok(Self {
            format,
            key_id,
            created_at,
            app_id,
            key_check_nonce,
            key_check_ct,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MetaBlob {
        MetaBlob::new(
            KeyId::from_bytes([7; 16]),
            1_700_000_000_000,
            "notes",
            [3u8; NONCE_LEN],
            [4u8; KEY_CHECK_CT_LEN],
        )
        .unwrap()
    }

    #[test]
    fn roundtrip() {
        let blob = sample();
        let bytes = blob.encode().unwrap();
        assert_eq!(bytes.len(), FIXED_LEN + "notes".len());
        assert_eq!(MetaBlob::decode(&bytes).unwrap(), blob);
    }

    #[test]
    fn rejects_bad_structure() {
        let bytes = sample().encode().unwrap();
        assert!(MetaBlob::decode(&bytes[..FIXED_LEN - 1]).is_err());
        assert!(MetaBlob::decode(&[bytes.as_slice(), b"\x00"].concat()).is_err());

        let mut bad = bytes;
        bad[5] = 1;
        assert!(matches!(
            MetaBlob::decode(&bad),
            Err(Error::UnsupportedFormat { found: 1, .. })
        ));
    }
}
