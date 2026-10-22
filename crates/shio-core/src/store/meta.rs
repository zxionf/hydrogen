//! `shio.meta`：格式版本与当前数据密钥。
//!
//! ```text
//! magic "SHIO" | format u16 | key_id u16 | created_at i64
//! | app_id_len u8 | app_id | key_check_nonce [24] | key_check_ct [32]
//! ```

use redb::TableDefinition;

use crate::codec::{NONCE_LEN, TAG_LEN};
use crate::crypto::keys::KeyId;
use crate::error::{Error, Result};
use crate::store::FORMAT;

/// 键固定为 [`super::META_KEY`]，值为 `MetaBlob`。
pub const TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("shio.meta");

/// `MetaBlob` 的魔数。
pub const MAGIC: &[u8; 4] = b"SHIO";

/// key check 的固定明文。
pub const KEY_CHECK_PLAINTEXT: &[u8; 16] = b"shio-keycheck-v1";

/// key check 密文长度：明文 16 字节 + tag。
pub const KEY_CHECK_CT_LEN: usize = 16 + TAG_LEN;

/// `app_id` 的最大字节数。
pub const MAX_APP_ID_LEN: usize = 64;

/// 不含 `app_id` 的固定长度。
pub const FIXED_LEN: usize = 4 + 2 + 2 + 8 + 1 + NONCE_LEN + KEY_CHECK_CT_LEN;

const FORMAT_OFFSET: usize = 4;
const KEY_ID_OFFSET: usize = 6;
const CREATED_AT_OFFSET: usize = 8;
const APP_ID_LEN_OFFSET: usize = 16;
const APP_ID_OFFSET: usize = 17;

/// `shio.meta` 的值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaBlob {
    /// 整个数据库的主格式版本。
    pub format: u16,
    /// 当前数据密钥编号。
    pub key_id: u16,
    /// 库创建时间（Unix 毫秒）。
    pub created_at: i64,
    /// 应用实例标识，可为空。
    pub app_id: String,
    /// key check 的 nonce。
    pub key_check_nonce: [u8; NONCE_LEN],
    /// key check 的密文，明文固定为 [`KEY_CHECK_PLAINTEXT`]。
    pub key_check_ct: [u8; KEY_CHECK_CT_LEN],
}

impl MetaBlob {
    /// 构造并校验字段。
    pub fn new(
        key_id: u16,
        created_at: i64,
        app_id: &str,
        key_check_nonce: [u8; NONCE_LEN],
        key_check_ct: [u8; KEY_CHECK_CT_LEN],
    ) -> Result<Self> {
        KeyId::new(key_id)?;
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

    /// 编码为落盘字节。
    pub fn encode(&self) -> Result<Vec<u8>> {
        KeyId::new(self.key_id)?;
        let app_id_len = u8::try_from(self.app_id.len())
            .map_err(|_| Error::InvalidArgument("app id exceeds 64 bytes"))?;
        if usize::from(app_id_len) > MAX_APP_ID_LEN {
            return Err(Error::InvalidArgument("app id exceeds 64 bytes"));
        }

        let mut out = Vec::with_capacity(FIXED_LEN + self.app_id.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.format.to_be_bytes());
        out.extend_from_slice(&self.key_id.to_be_bytes());
        out.extend_from_slice(&self.created_at.to_be_bytes());
        out.push(app_id_len);
        out.extend_from_slice(self.app_id.as_bytes());
        out.extend_from_slice(&self.key_check_nonce);
        out.extend_from_slice(&self.key_check_ct);
        Ok(out)
    }

    /// 从落盘字节解析；任何结构不符都返回 [`Error::Corrupted`]。
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

        let key_id = u16::from_be_bytes([bytes[KEY_ID_OFFSET], bytes[KEY_ID_OFFSET + 1]]);
        KeyId::new(key_id).map_err(|_| Error::Corrupted("meta key id out of range"))?;

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
            .ok_or(Error::Corrupted("meta key check offset overflow"))?;
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
            1,
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

        let mut bad_magic = bytes.clone();
        bad_magic[0] = b'X';
        assert!(matches!(
            MetaBlob::decode(&bad_magic),
            Err(Error::Corrupted(_))
        ));

        let mut bad_format = bytes.clone();
        bad_format[FORMAT_OFFSET + 1] = 2;
        assert!(matches!(
            MetaBlob::decode(&bad_format),
            Err(Error::UnsupportedFormat { found: 2, .. })
        ));

        assert!(MetaBlob::decode(&bytes[..FIXED_LEN - 1]).is_err());
        assert!(MetaBlob::decode(&[bytes.as_slice(), b"\x00"].concat()).is_err());
    }

    #[test]
    fn rejects_bad_app_id() {
        let mut bytes = sample().encode().unwrap();
        bytes[APP_ID_LEN_OFFSET] = MAX_APP_ID_LEN as u8 + 1;
        assert!(MetaBlob::decode(&bytes).is_err());

        let long = "a".repeat(MAX_APP_ID_LEN + 1);
        assert!(MetaBlob::new(1, 0, &long, [0u8; NONCE_LEN], [0u8; KEY_CHECK_CT_LEN]).is_err());
    }
}
