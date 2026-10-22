//! `shio.records`：加密记录。
//!
//! 键为 `RecordKey`（见 [`crate::codec::encode_record_key`]），
//! 值为 `RecordBlob`（header + nonce + ct_len + 密文）。

use redb::TableDefinition;

/// 键为 `RecordKey` 编码后的字节，值为 `RecordBlob`。
pub const TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("shio.records");
