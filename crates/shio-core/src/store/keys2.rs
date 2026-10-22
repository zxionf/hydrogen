//! `format=2` 的 `shio.keys2`：按 16 字节 `key_id` 存放被 KEK 包裹的 DK。
//!
//! ```text
//! key_id 16 | kdf_id u8 | flags u8 | m_cost u32 | t_cost u32 | p_cost u32
//! | salt 16 | wrap_nonce 24 | wrapped_dk 48 | created_at i64
//! | label_len u8 | label
//! ```

use redb::TableDefinition;

use crate::codec::NONCE_LEN;
use crate::crypto::kdf::{KdfId, KdfParams};
use crate::error::{Error, Result};
use crate::model::KeyId;

/// 键为 16 字节 `key_id`。
pub const TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("shio.keys2");

/// 被包裹 DK 长度。
pub const WRAPPED_DK_LEN: usize = 48;

/// 盐长度。
pub const SALT_LEN: usize = 16;

/// 单库 key 行上限。
pub const MAX_KEY_COUNT: usize = 32;

/// 不含 label 的固定长度。
pub const FIXED_LEN: usize = 16 + 1 + 1 + 4 + 4 + 4 + SALT_LEN + NONCE_LEN + WRAPPED_DK_LEN + 8 + 1;

const KDF_ID_OFFSET: usize = 16;
const FLAGS_OFFSET: usize = 17;
const M_COST_OFFSET: usize = 18;
const T_COST_OFFSET: usize = 22;
const P_COST_OFFSET: usize = 26;
const SALT_OFFSET: usize = 30;
const WRAP_NONCE_OFFSET: usize = SALT_OFFSET + SALT_LEN;
const WRAPPED_DK_OFFSET: usize = WRAP_NONCE_OFFSET + NONCE_LEN;
const CREATED_AT_OFFSET: usize = WRAPPED_DK_OFFSET + WRAPPED_DK_LEN;
const LABEL_LEN_OFFSET: usize = CREATED_AT_OFFSET + 8;
const LABEL_OFFSET: usize = LABEL_LEN_OFFSET + 1;

/// `shio.keys2` 的值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBlob {
    /// 与键一致的 key id。
    pub key_id: KeyId,
    /// KDF 算法。
    pub kdf_id: u8,
    /// 保留位。
    pub flags: u8,
    /// Argon2id 内存开销。
    pub m_cost_kib: u32,
    /// Argon2id 迭代次数。
    pub t_cost: u32,
    /// Argon2id 并行度。
    pub p_cost: u32,
    /// 口令派生盐。
    pub salt: [u8; SALT_LEN],
    /// 包裹 nonce。
    pub wrap_nonce: [u8; NONCE_LEN],
    /// 被包裹的 DK。
    pub wrapped_dk: [u8; WRAPPED_DK_LEN],
    /// 创建时间。
    pub created_at: i64,
    /// 诊断标签。
    pub label: String,
}

impl KeyBlob {
    /// KDF 参数视图。
    pub fn kdf_params(&self) -> KdfParams {
        KdfParams::new(self.m_cost_kib, self.t_cost, self.p_cost)
    }

    /// 编码。
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.key_id.is_zero() {
            return Err(Error::InvalidArgument("key id must not be zero"));
        }
        if self.flags != 0 {
            return Err(Error::InvalidArgument("key blob flags are reserved"));
        }
        KdfId::from_u8(self.kdf_id)?;
        self.kdf_params().validate()?;
        let label_len = u8::try_from(self.label.len())
            .map_err(|_| Error::InvalidArgument("key label exceeds 255 bytes"))?;

        let mut out = Vec::with_capacity(FIXED_LEN + self.label.len());
        out.extend_from_slice(self.key_id.as_bytes());
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

    /// 解码。
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < FIXED_LEN {
            return Err(Error::Corrupted("key blob too short"));
        }
        let key_id: [u8; 16] = bytes[..16]
            .try_into()
            .map_err(|_| Error::Corrupted("key blob id truncated"))?;
        let key_id = KeyId::from_bytes(key_id);
        if key_id.is_zero() {
            return Err(Error::Corrupted("key blob id must not be zero"));
        }
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
            key_id: KeyId::from_bytes([5; 16]),
            kdf_id: KdfId::Argon2idV1.as_u8(),
            flags: 0,
            m_cost_kib: DEFAULT_M_COST_KIB,
            t_cost: DEFAULT_T_COST,
            p_cost: DEFAULT_P_COST,
            salt: [1u8; SALT_LEN],
            wrap_nonce: [2u8; NONCE_LEN],
            wrapped_dk: [3u8; WRAPPED_DK_LEN],
            created_at: 7,
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
    fn rejects_bad_structure() {
        let bytes = sample().encode().unwrap();
        assert!(KeyBlob::decode(&bytes[..FIXED_LEN - 1]).is_err());
        assert!(KeyBlob::decode(&[bytes.as_slice(), b"\x00"].concat()).is_err());
        let mut zero = bytes;
        zero[..16].fill(0);
        assert!(KeyBlob::decode(&zero).is_err());
    }
}
