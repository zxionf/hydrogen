//! AEAD 输入：nonce 与非密钥常量。
//!
//! 记录使用 XChaCha20-Poly1305：24 字节随机 nonce、16 字节 tag。
//! 加解密本身尚未接线，本模块只提供 nonce 类型与随机数来源。

use std::fmt;

use crate::error::{Error, Result};

/// 记录 AEAD 的 nonce 长度。
pub const NONCE_LEN: usize = 24;

/// Poly1305 tag 长度。
pub const TAG_LEN: usize = 16;

/// 24 字节随机 nonce。
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Nonce([u8; NONCE_LEN]);

impl Nonce {
    /// 由原始字节构造。
    pub const fn from_bytes(bytes: [u8; NONCE_LEN]) -> Self {
        Self(bytes)
    }

    /// 原始字节。
    pub const fn as_bytes(&self) -> &[u8; NONCE_LEN] {
        &self.0
    }

    /// 从操作系统 CSPRNG 取一个新的 nonce。
    ///
    /// 每条记录写操作都必须生成新 nonce；取不到随机数时返回 [`Error::Internal`]，
    /// 绝不退化为时间戳或计数器。
    pub fn random() -> Result<Self> {
        let mut bytes = [0u8; NONCE_LEN];
        getrandom::fill(&mut bytes)
            .map_err(|_| Error::Internal("operating system CSPRNG unavailable"))?;
        Ok(Self(bytes))
    }
}

impl fmt::Debug for Nonce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // nonce 不是秘密，但逐字节打印没有诊断价值。
        write!(f, "Nonce({:02x?}..)", self.0[0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_nonce_changes() {
        let first = Nonce::random().unwrap();
        let second = Nonce::random().unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn nonce_roundtrip() {
        let nonce = Nonce::from_bytes([9u8; NONCE_LEN]);
        assert_eq!(nonce.as_bytes(), &[9u8; NONCE_LEN]);
    }
}
