//! `shio.meta`：格式版本与当前数据密钥。
//!
//! ```text
//! magic "SHIO" | format u16 | key_id u16 | created_at i64
//! | app_id_len u8 | app_id | key_check_nonce [24] | key_check_ct [32]
//! ```

use redb::TableDefinition;

/// 键固定为 [`super::META_KEY`]，值为 `MetaBlob`。
pub const TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("shio.meta");

/// `MetaBlob` 的魔数。
pub const MAGIC: &[u8; 4] = b"SHIO";

/// key check 的固定明文。
pub const KEY_CHECK_PLAINTEXT: &[u8; 16] = b"shio-keycheck-v1";
