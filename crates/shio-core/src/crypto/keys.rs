//! 密钥材料：`KEK` 与由 `DK` 派生的子密钥。
//!
//! 每种密钥都是固定 32 字节、可清零的新类型。它们在离开作用域时由
//! `zeroize` 覆写；`Debug` 只打印类型名，避免密钥随日志外泄。

use std::fmt;

use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{Error, Result};

/// 对称密钥长度（字节）。
pub const KEY_LEN: usize = 32;

/// 允许的最小 `key_id`。
pub const MIN_KEY_ID: u16 = 1;

/// 允许的最大 `key_id`；0 与 65535 保留。
pub const MAX_KEY_ID: u16 = 65_534;

/// 数据密钥编号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyId(u16);

impl KeyId {
    /// 构造 `key_id`，取值范围 `1..=65534`。
    pub const fn new(value: u16) -> Result<Self> {
        if value < MIN_KEY_ID || value > MAX_KEY_ID {
            return Err(Error::InvalidArgument("key id must be within 1..=65534"));
        }
        Ok(Self(value))
    }

    /// 原始值。
    pub const fn get(self) -> u16 {
        self.0
    }

    /// 下一个可用编号；到达上限时返回 `None`，不静默回绕。
    pub const fn next(self) -> Option<Self> {
        if self.0 >= MAX_KEY_ID {
            None
        } else {
            Some(Self(self.0 + 1))
        }
    }
}

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

macro_rules! secret_key {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Zeroize, ZeroizeOnDrop)]
        pub struct $name([u8; KEY_LEN]);

        impl $name {
            /// 由原始字节构造。
            pub const fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
                Self(bytes)
            }

            /// 原始字节，仅供加密运算读取。
            pub const fn as_bytes(&self) -> &[u8; KEY_LEN] {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "(<redacted>)"))
            }
        }
    };
}

secret_key! {
    /// 口令派生出的密钥加密密钥，只用于包裹数据密钥。
    Kek
}

secret_key! {
    /// 数据密钥，经 HKDF 派生出各用途子密钥。
    DataKey
}

secret_key! {
    /// 记录加密密钥，derived from `DK` with info `shio/v1/record-key`。
    RecordKey
}

secret_key! {
    /// 元数据密钥，保留用途。
    MetaKey
}

secret_key! {
    /// 密钥校验密钥，用于验证解锁是否成功。
    CheckKey
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_id_range() {
        assert!(KeyId::new(0).is_err());
        assert!(KeyId::new(1).is_ok());
        assert_eq!(KeyId::new(MAX_KEY_ID).unwrap().next(), None);
        assert_eq!(KeyId::new(1).unwrap().next().map(KeyId::get), Some(2));
    }

    #[test]
    fn secrets_do_not_leak_through_debug() {
        let key = DataKey::from_bytes([0x42; KEY_LEN]);
        let text = format!("{key:?}");
        assert_eq!(text, "DataKey(<redacted>)");
        assert!(!text.contains("66"));
        assert_eq!(key.as_bytes(), &[0x42; KEY_LEN]);
    }
}
