//! `shio.catalog`：命名空间与集合清单。
//!
//! 键为 `CatalogKey`（长度前缀编码，见 [`crate::codec::encode_catalog_key`]），
//! 值为 `CatalogBlob`：`created_at i64 | catalog_version u16`。

use redb::TableDefinition;

use crate::error::{Error, Result};

/// 键为 `CatalogKey` 编码后的字节，值为 `CatalogBlob`。
pub const TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("shio.catalog");

/// 当前 catalog 版本。
pub const CATALOG_VERSION: u16 = 1;

/// `CatalogBlob` 的固定长度。
pub const BLOB_LEN: usize = 8 + 2;

const VERSION_OFFSET: usize = 8;

/// `shio.catalog` 的值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogBlob {
    /// 集合首次登记时间（Unix 毫秒）。
    pub created_at: i64,
    /// catalog 版本。
    pub catalog_version: u16,
}

impl CatalogBlob {
    /// 构造当前版本的 catalog 值。
    pub const fn new(created_at: i64) -> Self {
        Self {
            created_at,
            catalog_version: CATALOG_VERSION,
        }
    }

    /// 编码为 10 字节。
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(BLOB_LEN);
        out.extend_from_slice(&self.created_at.to_be_bytes());
        out.extend_from_slice(&self.catalog_version.to_be_bytes());
        out
    }

    /// 解析并校验长度与版本。
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != BLOB_LEN {
            return Err(Error::Corrupted("catalog blob length mismatch"));
        }
        let created_at = i64::from_be_bytes(
            bytes[..VERSION_OFFSET]
                .try_into()
                .map_err(|_| Error::Corrupted("catalog created_at truncated"))?,
        );
        let catalog_version = u16::from_be_bytes(
            bytes[VERSION_OFFSET..]
                .try_into()
                .map_err(|_| Error::Corrupted("catalog version truncated"))?,
        );
        if catalog_version != CATALOG_VERSION {
            return Err(Error::UnsupportedFormat {
                found: catalog_version,
                supported: CATALOG_VERSION,
            });
        }
        Ok(Self {
            created_at,
            catalog_version,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let blob = CatalogBlob::new(1_700_000_000_000);
        let bytes = blob.encode();
        assert_eq!(bytes.len(), BLOB_LEN);
        assert_eq!(CatalogBlob::decode(&bytes).unwrap(), blob);
    }

    #[test]
    fn rejects_bad_structure() {
        let bytes = CatalogBlob::new(1).encode();
        assert!(CatalogBlob::decode(&bytes[..BLOB_LEN - 1]).is_err());
        assert!(CatalogBlob::decode(&[bytes.as_slice(), b"\x00"].concat()).is_err());

        let mut bad_version = bytes.clone();
        bad_version[VERSION_OFFSET + 1] = 2;
        assert!(matches!(
            CatalogBlob::decode(&bad_version),
            Err(Error::UnsupportedFormat { found: 2, .. })
        ));
    }
}
