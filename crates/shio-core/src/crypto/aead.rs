//! AEAD 输入：nonce 与非密钥常量。
//!
//! 记录使用 XChaCha20-Poly1305：24 字节随机 nonce、16 字节 tag。
//! 加解密本身尚未接线，本模块只提供 nonce 类型与随机数来源。

use std::fmt;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};

use crate::crypto::keys::KEY_LEN;
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

/// 用给定密钥封装明文，返回「明文长度 + 16 字节 tag」的密文。
///
/// nonce 由调用方提供且必须唯一；失败只可能来自实现或内存问题，返回 `Internal`。
pub fn seal(key: &[u8; KEY_LEN], nonce: &Nonce, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(&Key::from(*key));
    let nonce = XNonce::from(*nonce.as_bytes());
    cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| Error::Internal("aead encryption failed"))
}

/// 解封密文。
///
/// 认证失败返回 [`Error::Corrupted`]；密钥包裹与 key check 场景由调用方改判为
/// [`Error::InvalidKey`]，避免向调用方泄露失败细节。
pub fn open(key: &[u8; KEY_LEN], nonce: &Nonce, aad: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(&Key::from(*key));
    let nonce = XNonce::from(*nonce.as_bytes());
    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| Error::Corrupted("aead authentication failed"))
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

    #[test]
    fn seal_open_roundtrip_and_failure_modes() {
        let key = [7u8; KEY_LEN];
        let nonce = Nonce::from_bytes([1u8; NONCE_LEN]);
        let aad = b"shio/v1/record";

        let ciphertext = seal(&key, &nonce, aad, b"hello").unwrap();
        assert_eq!(ciphertext.len(), 5 + TAG_LEN);
        assert_eq!(open(&key, &nonce, aad, &ciphertext).unwrap(), b"hello");

        // AAD 不同 → 认证失败。
        assert!(open(&key, &nonce, b"other", &ciphertext).is_err());

        // 换密钥 → 认证失败。
        let other_key = [8u8; KEY_LEN];
        assert!(open(&other_key, &nonce, aad, &ciphertext).is_err());

        // 篡改密文 → 认证失败且归类为 Corrupted。
        let mut tampered = ciphertext.clone();
        tampered[0] ^= 0x01;
        assert!(matches!(
            open(&key, &nonce, aad, &tampered),
            Err(Error::Corrupted(_))
        ));
    }
}
