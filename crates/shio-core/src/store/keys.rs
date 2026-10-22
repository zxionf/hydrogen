//! `shio.keys`：按 `key_id` 存放被 KEK 包裹的数据密钥。
//!
//! ```text
//! key_id u16 | kdf_id u8 | flags u8 | m_cost u32 | t_cost u32 | p_cost u32
//! | salt [16] | wrap_nonce [24] | wrapped_dk [48] | created_at i64
//! | label_len u8 | label
//! ```

use redb::TableDefinition;

/// 键为 `key_id`，值为 `KeyBlob`。
pub const TABLE: TableDefinition<u16, &[u8]> = TableDefinition::new("shio.keys");

/// 被包裹数据密钥的长度：32 字节 DK + 16 字节 tag。
pub const WRAPPED_DK_LEN: usize = 48;
