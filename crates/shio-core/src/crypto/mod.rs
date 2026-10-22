//! 加密材料与参数。
//!
//! 这一层只定义类型与取值范围：密钥派生的运算、AEAD 封装与解封留到后续变更。
//! 所有密钥材料在离开作用域时清零，`Debug` 输出永远不含字节。

pub mod aead;
pub mod kdf;
pub mod keys;

pub use aead::{Nonce, NONCE_LEN, TAG_LEN};
pub use kdf::{KdfId, KdfParams};
pub use keys::{
    CheckKey, DataKey, Kek, LegacyKeyId, MetaKey, RecordKey, KEY_LEN, MAX_KEY_ID, MIN_KEY_ID,
};
