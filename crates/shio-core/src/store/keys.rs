//! `shio.keys`：按 `key_id` 存放被 KEK 包裹的数据密钥。
//!
//! ```text
//! key_id u16 | kdf_id u8 | flags u8 | m_cost u32 | t_cost u32 | p_cost u32
//! | salt [16] | wrap_nonce [24] | wrapped_dk [48] | created_at i64
//! | label_len u8 | label
//! ```

use redb::TableDefinition;

use crate::codec::NONCE_LEN;
use crate::crypto::kdf::{KdfId, KdfParams};
use crate::crypto::keys::{LegacyKeyId, MAX_KEY_ID};
use crate::error::{Error, Result};

/// 键为 `key_id`，值为 `KeyBlob`。
pub const TABLE: TableDefinition<u16, &[u8]> = TableDefinition::new("shio.keys");

/// 被包裹数据密钥的长度：32 字节 DK + 16 字节 tag。
pub const WRAPPED_DK_LEN: usize = 48;

/// 盐长度。
pub const SALT_LEN: usize = 16;

/// 不含 label 的固定长度。
pub const FIXED_LEN: usize = 2 + 1 + 1 + 4 + 4 + 4 + SALT_LEN + NONCE_LEN + WRAPPED_DK_LEN + 8 + 1;

const KDF_ID_OFFSET: usize = 2;
const FLAGS_OFFSET: usize = 3;
const M_COST_OFFSET: usize = 4;
const T_COST_OFFSET: usize = 8;
const P_COST_OFFSET: usize = 12;
const SALT_OFFSET: usize = 16;
const WRAP_NONCE_OFFSET: usize = SALT_OFFSET + SALT_LEN;
const WRAPPED_DK_OFFSET: usize = WRAP_NONCE_OFFSET + NONCE_LEN;
const CREATED_AT_OFFSET: usize = WRAPPED_DK_OFFSET + WRAPPED_DK_LEN;
const LABEL_LEN_OFFSET: usize = CREATED_AT_OFFSET + 8;
const LABEL_OFFSET: usize = LABEL_LEN_OFFSET + 1;

/// `shio.keys` 的值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBlob {
    /// 与键一致的密钥编号。
    pub key_id: u16,
    /// 派生算法标识。
    pub kdf_id: u8,
    /// 保留位，写 0。
    pub flags: u8,
    /// Argon2id 内存开销，KiB。
    pub m_cost_kib: u32,
    /// Argon2id 迭代次数。
    pub t_cost: u32,
    /// Argon2id 并行度。
    pub p_cost: u32,
    /// 口令派生盐。
    pub salt: [u8; SALT_LEN],
    /// 包裹 DK 用的 nonce。
    pub wrap_nonce: [u8; NONCE_LEN],
    /// 被 KEK 包裹的 DK（32 字节 + 16 字节 tag）。
    pub wrapped_dk: [u8; WRAPPED_DK_LEN],
    /// 该密钥行的创建时间（Unix 毫秒）。
    pub created_at: i64,
    /// 诊断标签，不参与密码学。
    pub label: String,
}

impl KeyBlob {
    /// 派生参数视图。
    pub fn kdf_params(&self) -> KdfParams {
        KdfParams::new(self.m_cost_kib, self.t_cost, self.p_cost)
    }

    /// 编码为落盘字节。
    pub fn encode(&self) -> Result<Vec<u8>> {
        LegacyKeyId::new(self.key_id)?;
        if self.flags != 0 {
            return Err(Error::InvalidArgument("key blob flags are reserved"));
        }
        KdfId::from_u8(self.kdf_id)?;
        self.kdf_params().validate()?;
        let label_len = u8::try_from(self.label.len())
            .map_err(|_| Error::InvalidArgument("key label exceeds 255 bytes"))?;

        let mut out = Vec::with_capacity(FIXED_LEN + self.label.len());
        out.extend_from_slice(&self.key_id.to_be_bytes());
        out.push(self.kdf_id);
        out.push(self.flags);
        out.extend_from_slice(&self.m_cost_kib.to_be_bytes());
        out.extend_from_slice(&self.t_cost.to_be_bytes());
        out.extend_from_slice(&self.p_cost.to_be_bytes());
        out.extend_from_slice(&self.salt);
        out.extend_from_slice(&self.wrap_nonce);
        out.extend_from_slice(&self.wrapped_dk);
        out.extend_from_slice(&self.created_at.to_be_bytes());
        out.push(label_len);
        out.extend_from_slice(self.label.as_bytes());
        Ok(out)
    }

    /// 从落盘字节解析。
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < FIXED_LEN {
            return Err(Error::Corrupted("key blob too short"));
        }

        let key_id = u16::from_be_bytes([bytes[0], bytes[1]]);
        if key_id > MAX_KEY_ID {
            return Err(Error::Corrupted("key blob id out of range"));
        }
        LegacyKeyId::new(key_id).map_err(|_| Error::Corrupted("key blob id out of range"))?;

        let kdf_id = bytes[KDF_ID_OFFSET];
        KdfId::from_u8(kdf_id)?;

        if bytes[FLAGS_OFFSET] != 0 {
            return Err(Error::Corrupted("key blob reserved flags are set"));
        }

        let m_cost_kib = u32::from_be_bytes(
            bytes[M_COST_OFFSET..T_COST_OFFSET]
                .try_into()
                .map_err(|_| Error::Corrupted("key blob m_cost truncated"))?,
        );
        let t_cost = u32::from_be_bytes(
            bytes[T_COST_OFFSET..P_COST_OFFSET]
                .try_into()
                .map_err(|_| Error::Corrupted("key blob t_cost truncated"))?,
        );
        let p_cost = u32::from_be_bytes(
            bytes[P_COST_OFFSET..SALT_OFFSET]
                .try_into()
                .map_err(|_| Error::Corrupted("key blob p_cost truncated"))?,
        );
        KdfParams::new(m_cost_kib, t_cost, p_cost).validate()?;

        let salt: [u8; SALT_LEN] = bytes[SALT_OFFSET..WRAP_NONCE_OFFSET]
            .try_into()
            .map_err(|_| Error::Corrupted("key blob salt truncated"))?;

        let wrap_nonce: [u8; NONCE_LEN] = bytes[WRAP_NONCE_OFFSET..WRAPPED_DK_OFFSET]
            .try_into()
            .map_err(|_| Error::Corrupted("key blob wrap nonce truncated"))?;

        let wrapped_dk: [u8; WRAPPED_DK_LEN] = bytes[WRAPPED_DK_OFFSET..CREATED_AT_OFFSET]
            .try_into()
            .map_err(|_| Error::Corrupted("key blob wrapped dk truncated"))?;

        let created_at = i64::from_be_bytes(
            bytes[CREATED_AT_OFFSET..LABEL_LEN_OFFSET]
                .try_into()
                .map_err(|_| Error::Corrupted("key blob created_at truncated"))?,
        );

        let label_len = usize::from(bytes[LABEL_LEN_OFFSET]);
        let expected = FIXED_LEN
            .checked_add(label_len)
            .ok_or(Error::Corrupted("key blob length overflow"))?;
        if expected != bytes.len() {
            return Err(Error::Corrupted("key blob length mismatch"));
        }

        let label = std::str::from_utf8(
            bytes
                .get(LABEL_OFFSET..)
                .ok_or(Error::Corrupted("key blob label truncated"))?,
        )
        .map_err(|_| Error::Corrupted("key blob label is not valid UTF-8"))?
        .to_owned();

        Ok(Self {
            key_id,
            kdf_id,
            flags: 0,
            m_cost_kib,
            t_cost,
            p_cost,
            salt,
            wrap_nonce,
            wrapped_dk,
            created_at,
            label,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::kdf::{DEFAULT_M_COST_KIB, DEFAULT_P_COST, DEFAULT_T_COST};

    fn sample() -> KeyBlob {
        KeyBlob {
            key_id: 1,
            kdf_id: KdfId::Argon2idV1.as_u8(),
            flags: 0,
            m_cost_kib: DEFAULT_M_COST_KIB,
            t_cost: DEFAULT_T_COST,
            p_cost: DEFAULT_P_COST,
            salt: [1u8; SALT_LEN],
            wrap_nonce: [2u8; NONCE_LEN],
            wrapped_dk: [3u8; WRAPPED_DK_LEN],
            created_at: 1_700_000_000_000,
            label: "primary".to_owned(),
        }
    }

    #[test]
    fn roundtrip() {
        let blob = sample();
        let bytes = blob.encode().unwrap();
        assert_eq!(bytes.len(), FIXED_LEN + "primary".len());
        assert_eq!(KeyBlob::decode(&bytes).unwrap(), blob);
    }

    #[test]
    fn rejects_bad_fields() {
        let bytes = sample().encode().unwrap();

        let mut bad_id = bytes.clone();
        bad_id[0..2].copy_from_slice(&0u16.to_be_bytes());
        assert!(KeyBlob::decode(&bad_id).is_err());

        let mut bad_kdf = bytes.clone();
        bad_kdf[KDF_ID_OFFSET] = 9;
        assert!(matches!(
            KeyBlob::decode(&bad_kdf),
            Err(Error::UnsupportedFormat { .. })
        ));

        let mut bad_flags = bytes.clone();
        bad_flags[FLAGS_OFFSET] = 1;
        assert!(KeyBlob::decode(&bad_flags).is_err());

        assert!(KeyBlob::decode(&bytes[..FIXED_LEN - 1]).is_err());
        assert!(KeyBlob::decode(&[bytes.as_slice(), b"\x00"].concat()).is_err());
    }

    #[test]
    fn rejects_invalid_params() {
        let mut blob = sample();
        blob.m_cost_kib = 1;
        assert!(blob.encode().is_err());

        let mut blob = sample();
        blob.p_cost = 0;
        assert!(blob.encode().is_err());
    }
}
