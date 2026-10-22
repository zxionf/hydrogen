//! 口令派生参数。
//!
//! 算法固定为 Argon2id；参数随库存储，允许未来升级与按设备下调。
//! 这里只描述参数与取值范围，派生运算尚未接线。

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
}
