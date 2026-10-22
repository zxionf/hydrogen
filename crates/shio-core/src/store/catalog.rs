//! `shio.catalog`：命名空间与集合清单。
//!
//! 键为 `CatalogKey`（长度前缀编码，见 [`crate::codec::encode_catalog_key`]），
//! 值为 `CatalogBlob`：`created_at i64 | catalog_version u16`。

use redb::TableDefinition;

/// 键为 `CatalogKey` 编码后的字节，值为 `CatalogBlob`。
pub const TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("shio.catalog");

/// 当前 catalog 版本。
pub const CATALOG_VERSION: u16 = 1;
