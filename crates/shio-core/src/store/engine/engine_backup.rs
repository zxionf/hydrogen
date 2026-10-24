//! 在线备份与同设备恢复。
//!
//! 备份在源库的一个读事务里复制全部已知表，不解密、不重加密；恢复先把
//! 备份文件复制到目标同目录的临时文件，用原口令打开校验后再原子改名。

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use redb::{
    Database, ReadTransaction, ReadableTable, ReadableTableMetadata, TableDefinition, TableHandle,
    UntypedTableHandle, WriteTransaction,
};

use super::*;
use crate::store::{catalog, changes, keys, keys2, meta, meta2, records, sync_state, META_KEY};

/// 目标已存在时是否允许清空；恢复只允许 0 字节文件。
fn path_exists(path: &Path) -> Result<bool> {
    match std::fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// 同目录临时路径；`Drop` 时删除尚未发布的文件。
struct TempPath {
    path: PathBuf,
}

impl TempPath {
    fn sibling(target: &Path) -> Result<Self> {
        let name = target
            .file_name()
            .ok_or(Error::InvalidArgument("target path has no file name"))?;
        let mut random = [0u8; 8];
        getrandom::fill(&mut random)
            .map_err(|_| Error::Internal("operating system CSPRNG unavailable"))?;
        let suffix = u64::from_le_bytes(random);
        let mut file_name = OsString::from(".");
        file_name.push(name);
        file_name.push(format!(".{suffix:016x}.tmp"));
        Ok(Self {
            path: target.with_file_name(file_name),
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn copy_bytes_table(
    source: &ReadTransaction,
    target: &WriteTransaction,
    source_def: TableDefinition<&[u8], &[u8]>,
    target_def: TableDefinition<&[u8], &[u8]>,
) -> Result<Option<u64>> {
    let source_table = match source.open_table(source_def) {
        Ok(table) => table,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut target_table = target.open_table(target_def)?;
    let mut rows = 0u64;
    for item in source_table.iter()? {
        let (key, value) = item?;
        target_table.insert(key.value(), value.value())?;
        rows += 1;
    }
    Ok(Some(rows))
}

fn copy_str_table(
    source: &ReadTransaction,
    target: &WriteTransaction,
    source_def: TableDefinition<&str, &[u8]>,
    target_def: TableDefinition<&str, &[u8]>,
) -> Result<Option<u64>> {
    let source_table = match source.open_table(source_def) {
        Ok(table) => table,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut target_table = target.open_table(target_def)?;
    let mut rows = 0u64;
    for item in source_table.iter()? {
        let (key, value) = item?;
        target_table.insert(key.value(), value.value())?;
        rows += 1;
    }
    Ok(Some(rows))
}

fn copy_u16_table(
    source: &ReadTransaction,
    target: &WriteTransaction,
    source_def: TableDefinition<u16, &[u8]>,
    target_def: TableDefinition<u16, &[u8]>,
) -> Result<Option<u64>> {
    let source_table = match source.open_table(source_def) {
        Ok(table) => table,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut target_table = target.open_table(target_def)?;
    let mut rows = 0u64;
    for item in source_table.iter()? {
        let (key, value) = item?;
        target_table.insert(key.value(), value.value())?;
        rows += 1;
    }
    Ok(Some(rows))
}

fn copy_u64_table(
    source: &ReadTransaction,
    target: &WriteTransaction,
    source_def: TableDefinition<u64, &[u8]>,
    target_def: TableDefinition<u64, &[u8]>,
) -> Result<Option<u64>> {
    let source_table = match source.open_table(source_def) {
        Ok(table) => table,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut target_table = target.open_table(target_def)?;
    let mut rows = 0u64;
    for item in source_table.iter()? {
        let (key, value) = item?;
        target_table.insert(key.value(), value.value())?;
        rows += 1;
    }
    Ok(Some(rows))
}

fn copy_all_tables(
    source: &ReadTransaction,
    target: &WriteTransaction,
) -> Result<Vec<(&'static str, Option<u64>)>> {
    Ok(vec![
        (
            "shio.meta",
            copy_str_table(source, target, meta::TABLE, meta::TABLE)?,
        ),
        (
            "shio.keys",
            copy_u16_table(source, target, keys::TABLE, keys::TABLE)?,
        ),
        (
            "shio.keys2",
            copy_bytes_table(source, target, keys2::TABLE, keys2::TABLE)?,
        ),
        (
            "shio.catalog",
            copy_bytes_table(source, target, catalog::TABLE, catalog::TABLE)?,
        ),
        (
            "shio.records",
            copy_bytes_table(source, target, records::TABLE, records::TABLE)?,
        ),
        (
            "shio.changes",
            copy_u64_table(source, target, changes::TABLE, changes::TABLE)?,
        ),
        (
            "shio.sync",
            copy_bytes_table(source, target, sync_state::TABLE, sync_state::TABLE)?,
        ),
    ])
}

fn read_meta(source: &ReadTransaction) -> Result<Vec<u8>> {
    let table = source.open_table(meta::TABLE)?;
    let value = table
        .get(META_KEY)?
        .ok_or(Error::Corrupted("meta row is missing"))?;
    Ok(value.value().to_vec())
}

fn validate_format(format: u16, meta_bytes: &[u8]) -> Result<()> {
    match format {
        FORMAT => {
            meta::MetaBlob::decode(meta_bytes)?;
        }
        FORMAT_V2 => {
            meta2::MetaBlob::decode(meta_bytes)?;
        }
        other => {
            return Err(Error::UnsupportedFormat {
                found: other,
                supported: FORMAT_V2,
            });
        }
    }
    Ok(())
}

fn validate_backup(path: &Path, expected: &[(&str, Option<u64>)], meta_bytes: &[u8]) -> Result<()> {
    let builder = Database::builder();
    let db = builder.open_read_only(path)?;
    let txn = db.begin_read()?;
    let mut handles: BTreeMap<String, UntypedTableHandle> = BTreeMap::new();
    for handle in txn.list_tables()? {
        handles.insert(handle.name().to_owned(), handle);
    }

    for (name, rows) in expected {
        match rows {
            Some(rows) => {
                let handle = handles
                    .remove(*name)
                    .ok_or(Error::Corrupted("backup is missing an expected table"))?;
                let table = txn.open_untyped_table(handle)?;
                if table.len()? != *rows {
                    return Err(Error::Corrupted("backup table length mismatch"));
                }
            }
            None => {
                if handles.remove(*name).is_some() {
                    return Err(Error::Corrupted("backup contains an unexpected table"));
                }
            }
        }
    }

    let table = txn.open_table(meta::TABLE)?;
    let value = table
        .get(META_KEY)?
        .ok_or(Error::Corrupted("backup meta row is missing"))?;
    if value.value() != meta_bytes {
        return Err(Error::Corrupted("backup meta row mismatch"));
    }
    Ok(())
}

impl Engine {
    /// 在线备份到 `target`；目标必须不存在。
    pub fn backup(&self, target: &Path) -> Result<BackupInfo> {
        if path_exists(target)? {
            return Err(Error::InvalidArgument("backup target already exists"));
        }

        let format = self.read_session()?.format;
        let temp = TempPath::sibling(target)?;
        let (expected, meta_bytes) = {
            let source_txn = self.handle.begin_read()?;
            let meta_bytes = read_meta(&source_txn)?;
            validate_format(format, &meta_bytes)?;

            let builder = Database::builder();
            let target_db = builder.create(temp.path())?;
            let target_txn = target_db.begin_write()?;
            let expected = copy_all_tables(&source_txn, &target_txn)?;
            target_txn.commit()?;
            drop(target_db);
            (expected, meta_bytes)
        };
        let table_count = expected.iter().filter(|(_, rows)| rows.is_some()).count() as u64;
        let row_count = expected.iter().filter_map(|(_, rows)| *rows).sum::<u64>();

        validate_backup(temp.path(), &expected, &meta_bytes)?;
        std::fs::rename(temp.path(), target)?;
        let db_bytes = std::fs::metadata(target)?.len();

        Ok(BackupInfo {
            format_version: format,
            db_bytes,
            table_count,
            row_count,
        })
    }

    /// 从 `backup` 恢复到 `options.path`；目标必须不存在或为空。
    ///
    /// 恢复出的库与源库保持相同身份（`db_id`/`device_id`/key 链/游标）。
    pub fn restore(backup: &Path, options: EngineOptions, password: &[u8]) -> Result<Self> {
        validate_open_options(&options)?;
        let backup_meta = std::fs::metadata(backup).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::NotFound
            } else {
                Error::Io(error)
            }
        })?;
        if !backup_meta.is_file() {
            return Err(Error::InvalidArgument("backup path is not a file"));
        }

        let target = options.path.clone();
        match std::fs::metadata(&target) {
            Ok(metadata) => {
                if !metadata.is_file() || metadata.len() != 0 {
                    return Err(Error::InvalidArgument("restore target is not empty"));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }

        let temp = TempPath::sibling(&target)?;
        std::fs::copy(backup, temp.path())?;

        let mut check_options = options.clone();
        check_options.path = temp.path().to_path_buf();
        check_options.create = false;
        check_options.read_only = true;
        {
            let checked = Self::open(check_options, password)?;
            drop(checked);
        }

        if path_exists(&target)? {
            std::fs::remove_file(&target)?;
        }
        std::fs::rename(temp.path(), &target)?;

        let mut open_options = options;
        open_options.create = false;
        match Self::open(open_options, password) {
            Ok(engine) => Ok(engine),
            Err(error) => {
                let _ = std::fs::remove_file(&target);
                Err(error)
            }
        }
    }
}
