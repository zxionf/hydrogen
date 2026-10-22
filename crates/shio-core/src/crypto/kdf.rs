//! 口令派生参数。
//!
//! 算法固定为 Argon2id；参数随库存储，允许未来升级与按设备下调。

use argon2::{Algorithm, Argon2, Version};
use hkdf::Hkdf;
use sha2::Sha256;

use crate::crypto::keys::{CheckKey, DataKey, Kek, MetaKey, RecordKey, KEY_LEN};
use crate::error::{Error, Result};

/// `kdf_id` 当前取值：Argon2id v1.3。
pub const KDF_ID_ARGON2ID_V1: u8 = 1;

/// 盐长度。
pub const SALT_LEN: usize = 16;

/// `KEK` 长度。
pub const KEK_LEN: usize = 32;

/// 默认内存开销（KiB）。
pub const DEFAULT_M_COST_KIB: u32 = 65_536;

/// 默认迭代次数。
pub const DEFAULT_T_COST: u32 = 3;

/// 默认并行度。
pub const DEFAULT_P_COST: u32 = 1;

/// HKDF 的固定盐。
pub const HKDF_SALT: &[u8] = b"shio/v1/hkdf-salt";

/// 记录加密子密钥的 HKDF info。
pub const INFO_RECORD_KEY: &[u8] = b"shio/v1/record-key";

/// 元数据子密钥的 HKDF info（当前保留）。
pub const INFO_META_KEY: &[u8] = b"shio/v1/meta-key";

/// 密钥校验子密钥的 HKDF info。
pub const INFO_CHECK_KEY: &[u8] = b"shio/v1/check-key";

/// 由数据密钥派生出的三个子密钥。
#[derive(Clone)]
pub struct Subkeys {
    /// 记录加密密钥。
    pub record: RecordKey,
    /// 元数据密钥，当前保留。
    pub meta: MetaKey,
    /// 密钥校验密钥。
    pub check: CheckKey,
}

/// 参数上限，用于拒绝明显恶意的落盘参数。
const MAX_M_COST_KIB: u32 = 4 * 1024 * 1024;
const MAX_T_COST: u32 = 64;
const MAX_P_COST: u32 = 64;

/// 派生算法标识。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KdfId {
    /// Argon2id v1.3。
    Argon2idV1,
}

impl KdfId {
    /// 落盘编码。
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Argon2idV1 => KDF_ID_ARGON2ID_V1,
        }
    }

    /// 解析落盘编码；未知值返回 [`Error::UnsupportedFormat`]。
    pub const fn from_u8(value: u8) -> Result<Self> {
        match value {
            KDF_ID_ARGON2ID_V1 => Ok(Self::Argon2idV1),
            other => Err(Error::UnsupportedFormat {
                found: other as u16,
                supported: KDF_ID_ARGON2ID_V1 as u16,
            }),
        }
    }
}

/// Argon2id 参数组。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParams {
    /// 内存开销，KiB。
    pub m_cost_kib: u32,
    /// 迭代次数。
    pub t_cost: u32,
    /// 并行度。
    pub p_cost: u32,
}

impl Default for KdfParams {
    fn default() -> Self {
        Self {
            m_cost_kib: DEFAULT_M_COST_KIB,
            t_cost: DEFAULT_T_COST,
            p_cost: DEFAULT_P_COST,
        }
    }
}

impl KdfParams {
    /// 显式构造。
    pub const fn new(m_cost_kib: u32, t_cost: u32, p_cost: u32) -> Self {
        Self {
            m_cost_kib,
            t_cost,
            p_cost,
        }
    }

    /// 校验取值范围；来自磁盘的参数不合法时返回 [`Error::Corrupted`]。
    ///
    /// 约束：`p_cost ∈ 1..=64`、`t_cost ∈ 1..=64`、`m_cost_kib ∈ 8..=4 MiB`，
    /// 且 `m_cost_kib >= 8 * p_cost`（Argon2 规范对每条 lane 的最小内存要求）。
    pub fn validate(&self) -> Result<()> {
        if self.p_cost == 0 || self.p_cost > MAX_P_COST {
            return Err(Error::Corrupted("argon2 parallelism out of range"));
        }
        if self.t_cost == 0 || self.t_cost > MAX_T_COST {
            return Err(Error::Corrupted("argon2 iterations out of range"));
        }
        if self.m_cost_kib < 8 || self.m_cost_kib > MAX_M_COST_KIB {
            return Err(Error::Corrupted("argon2 memory cost out of range"));
        }
        let Some(minimum) = self.p_cost.checked_mul(8) else {
            return Err(Error::Corrupted("argon2 parallelism overflow"));
        };
        if self.m_cost_kib < minimum {
            return Err(Error::Corrupted(
                "argon2 memory cost below per-lane minimum",
            ));
        }
        Ok(())
    }
}

/// 用 Argon2id 把口令派生为 KEK。
///
/// 参数先校验；派生失败属于环境问题（内存不足等），返回 `Internal`。
pub fn derive_kek(password: &[u8], salt: &[u8; SALT_LEN], params: &KdfParams) -> Result<Kek> {
    params.validate()?;
    let argon_params = argon2::Params::new(
        params.m_cost_kib,
        params.t_cost,
        params.p_cost,
        Some(KEK_LEN),
    )
    .map_err(|_| Error::Corrupted("invalid argon2 parameters"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, argon_params);

    let mut kek = [0u8; KEK_LEN];
    argon
        .hash_password_into(password, salt, &mut kek)
        .map_err(|_| Error::Internal("argon2 key derivation failed"))?;
    Ok(Kek::from_bytes(kek))
}

/// 由数据密钥派生记录、元数据与校验子密钥。
pub fn derive_subkeys(data_key: &DataKey) -> Result<Subkeys> {
    let hkdf = Hkdf::<Sha256>::new(Some(HKDF_SALT), data_key.as_bytes());

    let mut record = [0u8; KEY_LEN];
    hkdf.expand(INFO_RECORD_KEY, &mut record)
        .map_err(|_| Error::Internal("hkdf expand failed for record key"))?;

    let mut meta = [0u8; KEY_LEN];
    hkdf.expand(INFO_META_KEY, &mut meta)
        .map_err(|_| Error::Internal("hkdf expand failed for meta key"))?;

    let mut check = [0u8; KEY_LEN];
    hkdf.expand(INFO_CHECK_KEY, &mut check)
        .map_err(|_| Error::Internal("hkdf expand failed for check key"))?;

    Ok(Subkeys {
        record: RecordKey::from_bytes(record),
        meta: MetaKey::from_bytes(meta),
        check: CheckKey::from_bytes(check),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        KdfParams::default().validate().unwrap();
    }

    #[test]
    fn rejects_out_of_range() {
        assert!(KdfParams::new(65_536, 3, 0).validate().is_err());
        assert!(KdfParams::new(65_536, 0, 1).validate().is_err());
        assert!(KdfParams::new(0, 3, 1).validate().is_err());
        assert!(KdfParams::new(8, 3, 2).validate().is_err());
        assert!(KdfParams::new(u32::MAX, 3, 1).validate().is_err());
    }

    #[test]
    fn kdf_id_roundtrip() {
        assert_eq!(KdfId::Argon2idV1.as_u8(), KDF_ID_ARGON2ID_V1);
        assert_eq!(KdfId::from_u8(1).unwrap(), KdfId::Argon2idV1);
        assert!(matches!(
            KdfId::from_u8(2),
            Err(Error::UnsupportedFormat { found: 2, .. })
        ));
    }

    #[test]
    fn kek_is_deterministic_and_input_bound() {
        // 小参数只为让测试快；生产默认值由 KdfParams::default 提供。
        let params = KdfParams::new(8_192, 1, 1);
        let salt = [7u8; SALT_LEN];

        let first = derive_kek(b"correct horse", &salt, &params).unwrap();
        let again = derive_kek(b"correct horse", &salt, &params).unwrap();
        assert_eq!(first.as_bytes(), again.as_bytes());

        let other_salt = derive_kek(b"correct horse", &[8u8; SALT_LEN], &params).unwrap();
        assert_ne!(first.as_bytes(), other_salt.as_bytes());

        let other_password = derive_kek(b"battery staple", &salt, &params).unwrap();
        assert_ne!(first.as_bytes(), other_password.as_bytes());
    }

    #[test]
    fn subkeys_are_distinct_and_stable() {
        let data_key = DataKey::from_bytes([3u8; KEY_LEN]);
        let keys = derive_subkeys(&data_key).unwrap();

        assert_ne!(keys.record.as_bytes(), keys.meta.as_bytes());
        assert_ne!(keys.record.as_bytes(), keys.check.as_bytes());
        assert_ne!(keys.meta.as_bytes(), keys.check.as_bytes());

        let again = derive_subkeys(&data_key).unwrap();
        assert_eq!(keys.record.as_bytes(), again.record.as_bytes());
        assert_eq!(keys.check.as_bytes(), again.check.as_bytes());
    }
}
