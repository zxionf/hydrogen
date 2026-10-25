//! 逻辑记录导出/导入：版本化 JSONL、两遍校验与 LWW 合并。
//!
//! 导出文件是**明文**：只应在用户明确要求时调用，并自行控制落盘位置与清理。
//! 导入先在内存外完整校验（结构、计数、SHA-256），再按批写入；每批一个写事务，
//! 批内原子、整体非原子，重跑靠 LWW 与明文比较收敛。

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use redb::{ReadableTable, WriteTransaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::engine_backup::{path_exists, TempPath};
use super::*;

/// 当前导出文件版本。
const EXPORT_VERSION: u8 = 1;

/// JSONL 行。
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "t")]
enum Line {
    #[serde(rename = "head")]
    Head {
        v: u8,
        fmt: u16,
        at: i64,
        #[serde(default)]
        deleted: bool,
    },
    #[serde(rename = "col")]
    Col { ns: String, col: String, at: i64 },
    #[serde(rename = "rec")]
    Rec {
        ns: String,
        col: String,
        id: String,
        rev: u64,
        created: i64,
        updated: i64,
        dv: u16,
        #[serde(default)]
        del: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        v: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        v64: Option<String>,
    },
    #[serde(rename = "end")]
    End { rec: u64, col: u64, sha: String },
}

/// 校验并解析后的一行记录。
struct ParsedRecord {
    namespace: String,
    collection: String,
    id: RecordId,
    rev: u64,
    created: i64,
    updated: i64,
    dv: u16,
    deleted: bool,
    plaintext: Option<Vec<u8>>,
}

/// 文件校验摘要。
struct ExportSummary {
    records: u64,
    collections: u64,
    bytes: u64,
}

/// 单条记录的落库计划。
struct ApplyPlan {
    rev: u64,
    created: i64,
    updated: i64,
    /// 仅墓碑需要：沿用本地明文，保证记录仍可解密。
    plaintext: Option<Zeroizing<Vec<u8>>>,
}

impl Engine {
    /// 把逻辑记录导出为明文 JSONL。
    ///
    /// **危险**：输出文件包含全部记录值的明文；请自行控制落盘位置、权限与清理。
    /// 目标已存在且 `options.overwrite=false` 时返回 [`Error::InvalidArgument`]。
    pub fn export_records(&self, target: &Path, options: &ExportOptions) -> Result<ExportInfo> {
        if options.collection.is_some() && options.namespace.is_none() {
            return Err(Error::InvalidArgument(
                "collection filter requires namespace",
            ));
        }
        if let Some(namespace) = &options.namespace {
            validate_name(NameKind::Namespace, namespace)?;
        }
        if let Some(collection) = &options.collection {
            validate_name(NameKind::Collection, collection)?;
        }
        if path_exists(target)? && !options.overwrite {
            return Err(Error::InvalidArgument("export target already exists"));
        }

        let session = self.read_session()?;
        let temp = TempPath::sibling(target)?;
        let mut writer = BufWriter::new(std::fs::File::create(temp.path())?);
        write_line(
            &mut writer,
            &Line::Head {
                v: EXPORT_VERSION,
                fmt: session.format,
                at: Timestamp::now().as_millis(),
                deleted: options.include_deleted,
            },
        )?;

        let mut hasher = Sha256::new();
        let mut records = 0u64;
        let mut deleted = 0u64;
        let mut collections = 0u64;
        {
            let txn = self.handle.begin_read()?;
            {
                let table = txn.open_table(catalog::TABLE)?;
                for item in table.iter()? {
                    let (key, value) = item?;
                    let (namespace, collection) = codec::decode_catalog_key(key.value())?;
                    if !in_scope(options, &namespace, &collection) {
                        continue;
                    }
                    let blob = catalog::CatalogBlob::decode(value.value())?;
                    write_body(
                        &mut writer,
                        &mut hasher,
                        &Line::Col {
                            ns: namespace,
                            col: collection,
                            at: blob.created_at,
                        },
                    )?;
                    collections += 1;
                }
            }
            {
                let table = txn.open_table(records::TABLE)?;
                for item in table.iter()? {
                    let (key, value) = item?;
                    let (namespace, collection, id) = codec::decode_record_key(key.value())?;
                    if !in_scope(options, &namespace, &collection) {
                        continue;
                    }
                    let blob = BlobView::decode(value.value())?;
                    let is_deleted = blob.is_deleted();
                    if is_deleted && !options.include_deleted {
                        continue;
                    }
                    let path = RecordPath {
                        namespace: &namespace,
                        collection: &collection,
                        id,
                    };
                    let encoded = if is_deleted {
                        None
                    } else {
                        let plaintext = self.decrypt(&session, path, &blob)?;
                        Some(BASE64.encode(plaintext.as_slice()))
                    };
                    write_body(
                        &mut writer,
                        &mut hasher,
                        &Line::Rec {
                            ns: namespace,
                            col: collection,
                            id: id.to_string(),
                            rev: blob.rev(),
                            created: blob.created_at(),
                            updated: blob.updated_at(),
                            dv: blob.data_version(),
                            del: is_deleted,
                            v: None,
                            v64: encoded,
                        },
                    )?;
                    records += 1;
                    if is_deleted {
                        deleted += 1;
                    }
                }
            }
        }

        let sha = hex(hasher.finalize().as_slice());
        write_line(
            &mut writer,
            &Line::End {
                rec: records,
                col: collections,
                sha,
            },
        )?;
        writer.flush()?;
        drop(writer);

        if path_exists(target)? {
            std::fs::remove_file(target)?;
        }
        std::fs::rename(temp.path(), target)?;
        let bytes = std::fs::metadata(target)?.len();

        Ok(ExportInfo {
            format_version: session.format,
            records,
            deleted,
            collections,
            bytes,
        })
    }

    /// 从明文 JSONL 导入记录。
    ///
    /// 先完整校验（结构、计数、SHA-256），再分批写入；`dry_run=true` 只校验并给出预估。
    /// 冲突按 [`ImportOptions::mode`] 处理，写入保留源时间戳并用当前 key 重新加密。
    pub fn import_records(&self, source: &Path, options: &ImportOptions) -> Result<ImportReport> {
        let batch_size = match options.batch_size {
            0 => DEFAULT_IMPORT_BATCH,
            size if size <= MAX_IMPORT_BATCH => size,
            _ => {
                return Err(Error::InvalidArgument(
                    "import batch size exceeds the limit",
                ))
            }
        };
        if !options.dry_run {
            self.require_writable()?;
        }
        if options.dry_run {
            return self.plan_import(source, options.mode);
        }

        // 第一遍：完整校验，同时收集集合登记信息。
        let mut catalogs: BTreeMap<(String, String), i64> = BTreeMap::new();
        let summary = self.scan_export(
            source,
            |namespace, collection, at| {
                catalogs.insert((namespace.to_owned(), collection.to_owned()), at);
                Ok(())
            },
            |_| Ok(()),
        )?;

        // 第二遍：按批写入。
        let mut batch: Vec<ParsedRecord> = Vec::new();
        let mut applied = 0u64;
        let mut skipped = 0u64;
        let mut deleted = 0u64;
        self.scan_export(
            source,
            |_, _, _| Ok(()),
            |record| {
                batch.push(record);
                if batch.len() >= batch_size {
                    let (written, skipped_rows, deleted_rows) =
                        self.flush_import_batch(&mut batch, &catalogs, options.mode)?;
                    applied += written;
                    skipped += skipped_rows;
                    deleted += deleted_rows;
                }
                Ok(())
            },
        )?;
        if !batch.is_empty() {
            let (written, skipped_rows, deleted_rows) =
                self.flush_import_batch(&mut batch, &catalogs, options.mode)?;
            applied += written;
            skipped += skipped_rows;
            deleted += deleted_rows;
        }

        Ok(ImportReport {
            records: summary.records,
            applied,
            skipped,
            deleted,
            collections: summary.collections,
            bytes: summary.bytes,
            dry_run: false,
        })
    }

    /// 只读预演：按当前库状态推演 `applied`/`skipped`，不写入。
    fn plan_import(&self, source: &Path, mode: ImportMode) -> Result<ImportReport> {
        let session = self.read_session()?;
        let txn = self.handle.begin_read()?;
        let table = txn.open_table(records::TABLE)?;
        let mut applied = 0u64;
        let mut skipped = 0u64;
        let mut deleted = 0u64;
        let summary = self.scan_export(
            source,
            |_, _, _| Ok(()),
            |record| {
                let key =
                    codec::encode_record_key(&record.namespace, &record.collection, &record.id)?;
                let local = table
                    .get(key.as_slice())?
                    .map(|guard| guard.value().to_vec());
                match self.plan_record(&session, &record, local.as_deref(), mode)? {
                    Some(_) => {
                        applied += 1;
                        if record.deleted {
                            deleted += 1;
                        }
                    }
                    None => skipped += 1,
                }
                Ok(())
            },
        )?;
        Ok(ImportReport {
            records: summary.records,
            applied,
            skipped,
            deleted,
            collections: summary.collections,
            bytes: summary.bytes,
            dry_run: true,
        })
    }

    /// 解析并校验导出文件；`on_col`/`on_rec` 只会在校验通过的行上调用。
    fn scan_export<F, G>(
        &self,
        source: &Path,
        mut on_col: F,
        mut on_rec: G,
    ) -> Result<ExportSummary>
    where
        F: FnMut(&str, &str, i64) -> Result<()>,
        G: FnMut(ParsedRecord) -> Result<()>,
    {
        let mut reader = BufReader::new(std::fs::File::open(source)?);
        let mut hasher = Sha256::new();
        let mut raw = String::new();
        let mut bytes = 0u64;
        let mut records = 0u64;
        let mut collections = 0u64;
        let mut header = false;
        let mut footer = false;

        loop {
            raw.clear();
            let read = reader.read_line(&mut raw)?;
            if read == 0 {
                break;
            }
            bytes += read as u64;
            let line = raw.trim_end_matches(['\r', '\n']);
            if line.trim().is_empty() {
                continue;
            }
            if footer {
                return Err(Error::Corrupted("export has data after its footer"));
            }
            match parse_line(line)? {
                Line::Head { v, fmt, .. } => {
                    if header {
                        return Err(Error::Corrupted("export has a duplicate header"));
                    }
                    if v != EXPORT_VERSION {
                        return Err(Error::Corrupted("unsupported export version"));
                    }
                    if fmt != FORMAT && fmt != FORMAT_V2 {
                        return Err(Error::Corrupted(
                            "unsupported source format version in export",
                        ));
                    }
                    header = true;
                }
                Line::Col { ns, col, at } => {
                    if !header {
                        return Err(Error::Corrupted("export is missing its header"));
                    }
                    validate_name(NameKind::Namespace, &ns)?;
                    validate_name(NameKind::Collection, &col)?;
                    hasher.update(line.as_bytes());
                    hasher.update(b"\n");
                    on_col(&ns, &col, at)?;
                    collections += 1;
                }
                Line::Rec {
                    ns,
                    col,
                    id,
                    rev,
                    created,
                    updated,
                    dv,
                    del,
                    v,
                    v64,
                } => {
                    if !header {
                        return Err(Error::Corrupted("export is missing its header"));
                    }
                    validate_name(NameKind::Namespace, &ns)?;
                    validate_name(NameKind::Collection, &col)?;
                    let id: RecordId = id
                        .parse()
                        .map_err(|_| Error::Corrupted("export record id is not a valid UUID"))?;
                    if rev == 0 {
                        return Err(Error::Corrupted("export record revision is zero"));
                    }
                    let plaintext = if del {
                        if v.is_some() || v64.is_some() {
                            return Err(Error::Corrupted("deleted record carries a value"));
                        }
                        None
                    } else {
                        let value = match (v, v64) {
                            (Some(text), None) => text.into_bytes(),
                            (None, Some(encoded)) => BASE64.decode(encoded).map_err(|_| {
                                Error::Corrupted("export value is not valid base64")
                            })?,
                            _ => {
                                return Err(Error::Corrupted(
                                    "record needs exactly one value encoding",
                                ))
                            }
                        };
                        self.check_plaintext_len(value.len())?;
                        Some(value)
                    };
                    hasher.update(line.as_bytes());
                    hasher.update(b"\n");
                    on_rec(ParsedRecord {
                        namespace: ns,
                        collection: col,
                        id,
                        rev,
                        created,
                        updated,
                        dv,
                        deleted: del,
                        plaintext,
                    })?;
                    records += 1;
                }
                Line::End { rec, col, sha } => {
                    if !header {
                        return Err(Error::Corrupted("export is missing its header"));
                    }
                    if rec != records || col != collections {
                        return Err(Error::Corrupted("export row count mismatch"));
                    }
                    if sha != hex(hasher.clone().finalize().as_slice()) {
                        return Err(Error::Corrupted("export checksum mismatch"));
                    }
                    footer = true;
                }
            }
        }

        if !header {
            return Err(Error::Corrupted("export is empty"));
        }
        if !footer {
            return Err(Error::Corrupted("export is truncated"));
        }
        Ok(ExportSummary {
            records,
            collections,
            bytes,
        })
    }

    /// 计算单条记录的落库计划；`None` 表示跳过。
    fn plan_record(
        &self,
        session: &Session,
        record: &ParsedRecord,
        local: Option<&[u8]>,
        mode: ImportMode,
    ) -> Result<Option<ApplyPlan>> {
        let path = RecordPath {
            namespace: &record.namespace,
            collection: &record.collection,
            id: record.id,
        };
        let local_blob = match local {
            Some(bytes) => Some(BlobView::decode(bytes)?),
            None => None,
        };

        let Some(blob) = local_blob else {
            // 墓碑只用于压制本地已有记录；新库里没有可压制的对象。
            if record.deleted {
                return Ok(None);
            }
            return Ok(Some(ApplyPlan {
                rev: record.rev,
                created: record.created,
                updated: record.updated,
                plaintext: None,
            }));
        };

        let local_rev = blob.rev();
        let local_updated = blob.updated_at();
        let ordering = (local_updated, local_rev).cmp(&(record.updated, record.rev));

        if mode == ImportMode::Replace {
            return Ok(Some(ApplyPlan {
                rev: local_rev.saturating_add(1).max(record.rev),
                created: blob.created_at(),
                updated: record.updated.max(local_updated.saturating_add(1)),
                plaintext: if record.deleted {
                    Some(self.decrypt(session, path, &blob)?)
                } else {
                    None
                },
            }));
        }

        if ordering == Ordering::Greater {
            return Ok(None);
        }
        if ordering == Ordering::Equal {
            if record.deleted {
                if blob.is_deleted() {
                    return Ok(None);
                }
                return Ok(Some(ApplyPlan {
                    rev: record.rev,
                    created: record.created,
                    updated: record.updated,
                    plaintext: Some(self.decrypt(session, path, &blob)?),
                }));
            }
            let local_plaintext = self.decrypt(session, path, &blob)?;
            let incoming = record.plaintext.as_deref().unwrap_or(&[]);
            if local_plaintext.as_slice() >= incoming {
                return Ok(None);
            }
        }

        Ok(Some(ApplyPlan {
            rev: record.rev,
            created: record.created,
            updated: record.updated,
            plaintext: if record.deleted {
                Some(self.decrypt(session, path, &blob)?)
            } else {
                None
            },
        }))
    }

    /// 写入一批导入记录；批内一个写事务。
    fn flush_import_batch(
        &self,
        batch: &mut Vec<ParsedRecord>,
        catalogs: &BTreeMap<(String, String), i64>,
        mode: ImportMode,
    ) -> Result<(u64, u64, u64)> {
        if batch.is_empty() {
            return Ok((0, 0, 0));
        }
        self.require_writable()?;
        let session = self.read_session()?;
        let txn = self.handle.begin_write()?;
        let mut applied = 0u64;
        let mut skipped = 0u64;
        let mut deleted = 0u64;
        let mut touched: BTreeSet<(String, String)> = BTreeSet::new();

        {
            let mut table = txn.open_table(records::TABLE)?;
            for record in batch.iter() {
                let path = RecordPath {
                    namespace: &record.namespace,
                    collection: &record.collection,
                    id: record.id,
                };
                let key =
                    codec::encode_record_key(&record.namespace, &record.collection, &record.id)?;
                let local = table
                    .get(key.as_slice())?
                    .map(|guard| guard.value().to_vec());
                let Some(plan) = self.plan_record(&session, record, local.as_deref(), mode)? else {
                    skipped += 1;
                    continue;
                };
                let plaintext: &[u8] = if record.deleted {
                    plan.plaintext
                        .as_ref()
                        .map(|bytes| bytes.as_slice())
                        .unwrap_or(&[])
                } else {
                    record.plaintext.as_deref().unwrap_or(&[])
                };
                let (key_id, blob) =
                    self.build_import_blob(&session, path, plaintext, record, &plan)?;
                table.insert(key.as_slice(), blob.as_slice())?;
                applied += 1;
                if record.deleted {
                    deleted += 1;
                }
                touched.insert((record.namespace.clone(), record.collection.clone()));
                if let Some(device) = session.sync {
                    if session.format == FORMAT_V2 {
                        append_change(
                            &txn,
                            device,
                            &ChangeEntry::Record(RecordChange {
                                namespace: record.namespace.clone(),
                                collection: record.collection.clone(),
                                id: record.id,
                                key_id,
                                rev: plan.rev,
                                updated_at: Timestamp::from_millis(plan.updated),
                                deleted: record.deleted,
                                writer: device,
                                blob,
                            }),
                        )?;
                    }
                }
            }
        }

        let now = Timestamp::now().as_millis();
        for (namespace, collection) in &touched {
            let key = (namespace.clone(), collection.clone());
            let created_at = catalogs.get(&key).copied().unwrap_or(now);
            self.stage_catalog_at(&txn, namespace, collection, created_at)?;
        }
        txn.commit()?;
        batch.clear();
        Ok((applied, skipped, deleted))
    }

    /// 用目标库当前 key 与保留的 header 重新加密一条记录。
    fn build_import_blob(
        &self,
        session: &Session,
        path: RecordPath<'_>,
        plaintext: &[u8],
        record: &ParsedRecord,
        plan: &ApplyPlan,
    ) -> Result<(KeyId, Vec<u8>)> {
        if session.format == FORMAT {
            let (legacy_id, entry) = session.legacy_active()?;
            let mut header = codec::RecordHeader::new(
                legacy_id,
                plan.rev,
                plan.created,
                plan.updated,
                record.dv,
            );
            if record.deleted {
                header = header.with_deleted(true);
            }
            let blob = self.build_record_v1(&entry.subkeys, path, plaintext, header)?;
            Ok((KeyId::from_u16(legacy_id), blob))
        } else {
            let entry = session.active()?;
            let mut header = codec2::RecordHeader::new(
                session.current,
                plan.rev,
                plan.created,
                plan.updated,
                record.dv,
            );
            if record.deleted {
                header = header.with_deleted(true);
            }
            let blob = self.build_record_v2(&entry.subkeys, path, plaintext, header)?;
            Ok((session.current, blob))
        }
    }

    /// 集合登记：缺失时按导出文件里的创建时间补上，已存在时保持原值。
    fn stage_catalog_at(
        &self,
        txn: &WriteTransaction,
        namespace: &str,
        collection: &str,
        created_at: i64,
    ) -> Result<()> {
        let key = codec::encode_catalog_key(namespace, collection)?;
        let mut table = txn.open_table(catalog::TABLE)?;
        if table.get(key.as_slice())?.is_none() {
            let blob = catalog::CatalogBlob::new(created_at);
            table.insert(key.as_slice(), blob.encode().as_slice())?;
        }
        Ok(())
    }
}

fn in_scope(options: &ExportOptions, namespace: &str, collection: &str) -> bool {
    if let Some(want) = options.namespace.as_deref() {
        if want != namespace {
            return false;
        }
    }
    if let Some(want) = options.collection.as_deref() {
        if want != collection {
            return false;
        }
    }
    true
}

fn parse_line(line: &str) -> Result<Line> {
    serde_json::from_str(line).map_err(|_| Error::Corrupted("export line is not valid JSON"))
}

fn write_line<W: Write>(writer: &mut W, line: &Line) -> Result<()> {
    let bytes =
        serde_json::to_vec(line).map_err(|_| Error::Internal("export line encoding failed"))?;
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn write_body<W: Write>(writer: &mut W, hasher: &mut Sha256, line: &Line) -> Result<()> {
    let bytes =
        serde_json::to_vec(line).map_err(|_| Error::Internal("export line encoding failed"))?;
    hasher.update(&bytes);
    hasher.update(b"\n");
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}
