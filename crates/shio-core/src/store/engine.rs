//! 同步内核引擎：打开库、解锁密钥、按命名空间/集合/记录读写。
//!
//! 事务边界：每次公开写操作是一个 redb 写事务，`batch` 的所有操作在同一事务内，
//! 任一失败则整批不生效。读操作使用独立读事务，看到一致快照。
//!
//! 密钥生命周期：会话同时持有 `format=1` 的旧密钥与 `format=2` 的新密钥，记录按
//! 自身 header 的 `key_id` 选择解密器，因此升级与轮换中途崩溃后两种记录都能读。
//! 同步相关方法集中在子模块 [`engine_sync`]，配对包在 [`pairing`]。

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Bound;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use redb::{
    Database, ReadOnlyDatabase, ReadableDatabase, ReadableTable, Table, TableHandle,
    WriteTransaction,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use zeroize::Zeroizing;

use crate::codec::{self, CT_OFFSET, NONCE_LEN};
use crate::codec2;
use crate::crypto::aead::{self, Nonce};
use crate::crypto::kdf::{self, KdfId, KdfParams, Subkeys};
use crate::crypto::keys::{DataKey, Kek, LegacyKeyId, KEY_LEN};
use crate::error::{Error, Result};
use crate::model::{
    validate_name, DeviceId, KeyId, ListOptions, NameKind, Page, RecordId, RecordMeta, Stored,
    Timestamp, WriteOp, DEFAULT_MAX_VALUE_SIZE,
};
use crate::store::{
    catalog, changes, keys, keys2, meta, meta2, records, sync_state, FORMAT, FORMAT_V2, META_KEY,
};
use crate::sync::{ChangeEntry, MigrationProgress, RecordChange};

mod engine_backup;
mod engine_sync;
mod pairing;

/// 轮换时每个写事务迁移的记录条数。
const ROTATE_BATCH: usize = 256;

/// 打开引擎的参数。
#[derive(Debug, Clone)]
pub struct EngineOptions {
    /// 数据库文件路径。
    pub path: PathBuf,
    /// 文件不存在时是否创建。
    pub create: bool,
    /// 只读打开；与 `create` 互斥。
    pub read_only: bool,
    /// 单条记录明文上限。
    pub max_value_size: usize,
    /// 应用实例标识，可为空。
    pub app_id: String,
    /// 库创建时使用的 Argon2id 参数。
    pub kdf: KdfParams,
    /// redb 页缓存字节数；`None` 表示使用 redb 默认值。
    pub cache_size: Option<usize>,
}

impl EngineOptions {
    /// 以路径构造，其余取默认值。
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            create: false,
            read_only: false,
            max_value_size: DEFAULT_MAX_VALUE_SIZE,
            app_id: String::new(),
            kdf: KdfParams::default(),
            cache_size: None,
        }
    }

    /// 文件不存在时是否创建。
    pub fn create(mut self, yes: bool) -> Self {
        self.create = yes;
        self
    }

    /// 是否以只读方式打开。
    pub fn read_only(mut self, yes: bool) -> Self {
        self.read_only = yes;
        self
    }

    /// 单条记录明文上限。
    pub fn max_value_size(mut self, bytes: usize) -> Self {
        self.max_value_size = bytes;
        self
    }

    /// 应用实例标识。
    pub fn app_id(mut self, app_id: impl Into<String>) -> Self {
        self.app_id = app_id.into();
        self
    }

    /// 库创建时使用的 Argon2id 参数；已存在的库以落盘参数为准。
    pub fn kdf(mut self, kdf: KdfParams) -> Self {
        self.kdf = kdf;
        self
    }

    /// redb 页缓存字节数。
    pub fn cache_size(mut self, bytes: usize) -> Self {
        self.cache_size = Some(bytes);
        self
    }
}

/// 引擎统计信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// 数据库格式版本。
    pub format_version: u16,
    /// 当前数据密钥编号。
    pub key_id: KeyId,
    /// 库文件字节数。
    pub db_bytes: u64,
}

/// 在线备份摘要；不包含明文或密钥材料。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupInfo {
    /// 源库格式版本。
    pub format_version: u16,
    /// 备份文件字节数。
    pub db_bytes: u64,
    /// 复制出的表数量。
    pub table_count: u64,
    /// 复制出的键值行总数。
    pub row_count: u64,
}

/// 批量写操作，载荷为已序列化的明文。
#[derive(Debug, Clone, Copy)]
pub enum RawWriteOp<'a> {
    /// 指定 ID 的 upsert。
    Put {
        /// 目标记录 ID。
        id: RecordId,
        /// 已序列化的明文。
        plaintext: &'a [u8],
    },
    /// 写入墓碑。
    Delete {
        /// 目标记录 ID。
        id: RecordId,
    },
}

/// 一条记录的三元坐标；传参时避免长参数列表。
#[derive(Clone, Copy)]
struct RecordPath<'a> {
    namespace: &'a str,
    collection: &'a str,
    id: RecordId,
}

/// 同步内核句柄。
///
/// 持有 redb 数据库与解锁后的密钥材料；`Send + Sync`，可在阻塞线程池中共享。
pub struct Engine {
    handle: Handle,
    path: PathBuf,
    max_value_size: usize,
    session: RwLock<Session>,
    migration: MigrationState,
}

/// 迁移的旁路状态：查询进度与请求取消不获取会话锁。
#[derive(Default)]
struct MigrationState {
    active: AtomicBool,
    cancel: AtomicBool,
    scanned: AtomicU64,
    migrated: AtomicU64,
    done: AtomicBool,
}

impl MigrationState {
    fn begin(&self) {
        self.cancel.store(false, Ordering::SeqCst);
        self.scanned.store(0, Ordering::SeqCst);
        self.migrated.store(0, Ordering::SeqCst);
        self.done.store(false, Ordering::SeqCst);
        self.active.store(true, Ordering::SeqCst);
    }

    fn finish(&self, done: bool) {
        self.active.store(false, Ordering::SeqCst);
        self.done.store(done, Ordering::SeqCst);
    }

    fn request_cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn add_scanned(&self, count: u64) {
        self.scanned.fetch_add(count, Ordering::SeqCst);
    }

    fn add_migrated(&self, count: u64) {
        self.migrated.fetch_add(count, Ordering::SeqCst);
    }

    fn snapshot(&self) -> MigrationProgress {
        MigrationProgress {
            active: self.active.load(Ordering::SeqCst),
            scanned: self.scanned.load(Ordering::SeqCst),
            migrated: self.migrated.load(Ordering::SeqCst),
            done: self.done.load(Ordering::SeqCst),
        }
    }
}

/// 数据库句柄：只读与读写共用读事务。
enum Handle {
    ReadWrite(Database),
    ReadOnly(ReadOnlyDatabase),
}

impl Handle {
    fn begin_read(&self) -> Result<redb::ReadTransaction> {
        match self {
            Self::ReadWrite(db) => Ok(db.begin_read()?),
            Self::ReadOnly(db) => Ok(db.begin_read()?),
        }
    }

    fn begin_write(&self) -> Result<redb::WriteTransaction> {
        match self {
            Self::ReadWrite(db) => Ok(db.begin_write()?),
            Self::ReadOnly(_) => Err(Error::ReadOnly),
        }
    }

    fn is_read_only(&self) -> bool {
        matches!(self, Self::ReadOnly(_))
    }
}

/// 会话密钥状态：格式版本、当前密钥编号、按编号索引的密钥与 KEK。
///
/// `format=2` 使用 [`keys2`]；升级期间同时保留 `shio.keys` 的旧行，直到迁移完成。
struct Session {
    format: u16,
    created_at: i64,
    app_id: String,
    current: KeyId,
    keys: BTreeMap<KeyId, KeyEntry>,
    legacy_current: Option<u16>,
    legacy_keys: BTreeMap<u16, LegacyKeyEntry>,
    legacy_map: BTreeMap<u16, KeyId>,
    kek: Kek,
    salt: [u8; 16],
    kdf_id: u8,
    kdf_params: KdfParams,
    key_check_nonce: [u8; NONCE_LEN],
    key_check_ct: [u8; meta::KEY_CHECK_CT_LEN],
    sync: Option<DeviceId>,
    upgrading: bool,
}

impl Session {
    fn active(&self) -> Result<&KeyEntry> {
        self.keys
            .get(&self.current)
            .ok_or(Error::Internal("active data key missing"))
    }

    fn legacy_active(&self) -> Result<(u16, &LegacyKeyEntry)> {
        let id = self
            .legacy_current
            .ok_or(Error::Internal("active legacy data key missing"))?;
        let entry = self
            .legacy_keys
            .get(&id)
            .ok_or(Error::Internal("active legacy data key missing"))?;
        Ok((id, entry))
    }

    fn key_created_at(&self, key_id: KeyId) -> Option<i64> {
        self.keys
            .get(&key_id)
            .map(|entry| entry.blob.created_at)
            .or_else(|| {
                key_id
                    .as_u16()
                    .and_then(|id| self.legacy_keys.get(&id))
                    .map(|entry| entry.blob.created_at)
            })
    }

    fn meta2_bytes(
        &self,
        current: KeyId,
        key_check_nonce: [u8; NONCE_LEN],
        key_check_ct: [u8; meta::KEY_CHECK_CT_LEN],
    ) -> Result<Vec<u8>> {
        meta2::MetaBlob::new(
            current,
            self.created_at,
            &self.app_id,
            key_check_nonce,
            key_check_ct,
        )?
        .encode()
    }
}

/// 一把 `format=2` 的已解包密钥。
struct KeyEntry {
    blob: keys2::KeyBlob,
    data_key: DataKey,
    subkeys: Subkeys,
}

/// 一把 `format=1` 的已解包密钥。
struct LegacyKeyEntry {
    blob: keys::KeyBlob,
    data_key: DataKey,
    subkeys: Subkeys,
}

/// 已校验结构的记录值视图：`SHR1` 或 `SHR2`。
enum BlobView<'a> {
    V1(codec::RecordBlob<'a>),
    V2(codec2::RecordBlob<'a>),
}

impl<'a> BlobView<'a> {
    fn decode(value: &'a [u8]) -> Result<Self> {
        if value.len() < 5 {
            return Err(Error::Corrupted("record blob too short"));
        }
        match &value[..4] {
            b"SHR1" => Ok(Self::V1(codec::RecordBlob::decode(value)?)),
            b"SHR2" => Ok(Self::V2(codec2::RecordBlob::decode(value)?)),
            _ => Err(Error::Corrupted("record magic mismatch")),
        }
    }

    fn key_id(&self) -> KeyId {
        match self {
            Self::V1(blob) => KeyId::from_u16(blob.header.key_id),
            Self::V2(blob) => blob.header.key_id,
        }
    }

    fn rev(&self) -> u64 {
        match self {
            Self::V1(blob) => blob.header.rev,
            Self::V2(blob) => blob.header.rev,
        }
    }

    fn created_at(&self) -> i64 {
        match self {
            Self::V1(blob) => blob.header.created_at,
            Self::V2(blob) => blob.header.created_at,
        }
    }

    fn updated_at(&self) -> i64 {
        match self {
            Self::V1(blob) => blob.header.updated_at,
            Self::V2(blob) => blob.header.updated_at,
        }
    }

    fn data_version(&self) -> u16 {
        match self {
            Self::V1(blob) => blob.header.data_version,
            Self::V2(blob) => blob.header.data_version,
        }
    }

    fn is_deleted(&self) -> bool {
        match self {
            Self::V1(blob) => blob.header.is_deleted(),
            Self::V2(blob) => blob.header.is_deleted(),
        }
    }

    fn ciphertext(&self) -> &'a [u8] {
        match self {
            Self::V1(blob) => blob.ciphertext,
            Self::V2(blob) => blob.ciphertext,
        }
    }
}

impl fmt::Debug for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = f.debug_struct("Engine");
        out.field("path", &self.path)
            .field("read_only", &self.handle.is_read_only())
            .field("max_value_size", &self.max_value_size);
        if let Ok(session) = self.session.read() {
            out.field("format", &session.format)
                .field("key_id", &session.current);
        }
        out.finish_non_exhaustive()
    }
}

impl Engine {
    /// 打开或创建数据库，并完成口令派生、DK 解包与 key check。
    pub fn open(options: EngineOptions, password: &[u8]) -> Result<Self> {
        validate_open_options(&options)?;

        if !options.create && !options.path.exists() {
            return Err(Error::NotFound);
        }

        let handle = if options.read_only {
            let mut builder = Database::builder();
            if let Some(bytes) = options.cache_size {
                builder.set_cache_size(bytes);
            }
            Handle::ReadOnly(builder.open_read_only(&options.path)?)
        } else {
            let db = match options.cache_size {
                Some(bytes) => {
                    let mut builder = Database::builder();
                    builder.set_cache_size(bytes);
                    if options.create {
                        builder.create(&options.path)?
                    } else {
                        builder.open(&options.path)?
                    }
                }
                None => {
                    if options.create {
                        Database::create(&options.path)?
                    } else {
                        Database::open(&options.path)?
                    }
                }
            };
            if options.create {
                initialize(&db, password, &options)?;
            }
            Handle::ReadWrite(db)
        };

        let session = unlock(&handle, password)?;

        Ok(Self {
            handle,
            path: options.path,
            max_value_size: options.max_value_size,
            session: RwLock::new(session),
            migration: MigrationState::default(),
        })
    }

    /// 生成 UUIDv7 并插入一条记录。
    pub fn insert<T: Serialize>(
        &self,
        namespace: &str,
        collection: &str,
        value: &T,
    ) -> Result<(RecordId, RecordMeta)> {
        let plaintext = self.encode_value(value)?;
        self.insert_raw(namespace, collection, &plaintext)
    }

    /// 生成 UUIDv7 并插入已序列化的明文。
    pub fn insert_raw(
        &self,
        namespace: &str,
        collection: &str,
        plaintext: &[u8],
    ) -> Result<(RecordId, RecordMeta)> {
        let id = RecordId::new_v7();
        let meta = self.put_raw(namespace, collection, id, plaintext, 1)?;
        Ok((id, meta))
    }

    /// 指定 ID 的 upsert；墓碑记录会被复活，`rev` 递增。
    pub fn put<T: Serialize>(
        &self,
        namespace: &str,
        collection: &str,
        id: RecordId,
        value: &T,
        data_version: u16,
    ) -> Result<RecordMeta> {
        let plaintext = self.encode_value(value)?;
        self.put_raw(namespace, collection, id, &plaintext, data_version)
    }

    /// 指定 ID 的 upsert，接收已序列化的明文。
    ///
    /// 供 facade 使用：facade 在调用线程序列化，再把字节搬进阻塞线程池。
    pub fn put_raw(
        &self,
        namespace: &str,
        collection: &str,
        id: RecordId,
        plaintext: &[u8],
        data_version: u16,
    ) -> Result<RecordMeta> {
        validate_names(namespace, collection)?;
        self.check_plaintext_len(plaintext.len())?;
        self.require_writable()?;

        let session = self.read_session()?;
        let txn = self.handle.begin_write()?;
        let path = RecordPath {
            namespace,
            collection,
            id,
        };
        let meta = {
            let mut table = txn.open_table(records::TABLE)?;
            self.stage_put(&session, &txn, &mut table, path, plaintext, data_version)?
        };
        self.stage_catalog(&txn, namespace, collection)?;
        txn.commit()?;
        Ok(meta)
    }

    /// 读取一条记录；墓碑与不存在的记录都返回 `None`。
    pub fn get<T: DeserializeOwned>(
        &self,
        namespace: &str,
        collection: &str,
        id: &RecordId,
    ) -> Result<Option<Stored<T>>> {
        let Some((meta, plaintext)) = self.get_raw(namespace, collection, id)? else {
            return Ok(None);
        };
        let value = serde_json::from_slice(&plaintext).map_err(Error::Deserialize)?;
        Ok(Some(Stored { meta, value }))
    }

    /// 读取一条记录的元信息与明文；返回的明文在离开作用域时清零。
    pub fn get_raw(
        &self,
        namespace: &str,
        collection: &str,
        id: &RecordId,
    ) -> Result<Option<(RecordMeta, Zeroizing<Vec<u8>>)>> {
        let key = codec::encode_record_key(namespace, collection, id)?;

        let session = self.read_session()?;
        let txn = self.handle.begin_read()?;
        let raw = {
            let table = txn.open_table(records::TABLE)?;
            table
                .get(key.as_slice())?
                .map(|guard| guard.value().to_vec())
        };
        let Some(raw) = raw else {
            return Ok(None);
        };

        let blob = BlobView::decode(&raw)?;
        if blob.is_deleted() {
            return Ok(None);
        }

        let plaintext = self.decrypt(
            &session,
            RecordPath {
                namespace,
                collection,
                id: *id,
            },
            &blob,
        )?;
        let meta = record_meta(*id, &blob)?;
        Ok(Some((meta, plaintext)))
    }

    /// 写入墓碑；记录不存在时返回 `false`。
    pub fn delete(&self, namespace: &str, collection: &str, id: &RecordId) -> Result<bool> {
        validate_names(namespace, collection)?;
        self.require_writable()?;

        let session = self.read_session()?;
        let txn = self.handle.begin_write()?;
        let existed = {
            let mut table = txn.open_table(records::TABLE)?;
            self.stage_delete(
                &session,
                &txn,
                &mut table,
                RecordPath {
                    namespace,
                    collection,
                    id: *id,
                },
            )?
            .is_some()
        };
        txn.commit()?;
        Ok(existed)
    }

    /// 物理删除；多端同步启用后不得使用。
    pub fn purge(&self, namespace: &str, collection: &str, id: &RecordId) -> Result<bool> {
        let key = codec::encode_record_key(namespace, collection, id)?;
        self.require_writable()?;
        if self.read_session()?.sync.is_some() {
            return Err(Error::SyncEnabled);
        }

        let txn = self.handle.begin_write()?;
        let existed = {
            let mut table = txn.open_table(records::TABLE)?;
            let removed = table.remove(key.as_slice())?.is_some();
            removed
        };
        txn.commit()?;
        Ok(existed)
    }

    /// 按记录 ID 升序分页；只读 header，不解密。
    pub fn list(
        &self,
        namespace: &str,
        collection: &str,
        options: ListOptions,
    ) -> Result<Page<RecordMeta>> {
        options.validate()?;
        let prefix = codec::encode_catalog_key(namespace, collection)?;

        let txn = self.handle.begin_read()?;
        let table = txn.open_table(records::TABLE)?;
        let mut items: Vec<RecordMeta> = Vec::new();
        let mut next = None;

        for entry in table.range(prefix.as_slice()..)? {
            let (key, value) = entry?;
            let key_bytes = key.value();
            if !key_bytes.starts_with(prefix.as_slice()) {
                break;
            }

            let (_, _, id) = codec::decode_record_key(key_bytes)?;
            if let Some(after) = options.after {
                if id <= after {
                    continue;
                }
            }

            let blob = BlobView::decode(value.value())?;
            if blob.is_deleted() && !options.include_deleted {
                continue;
            }

            items.push(record_meta(id, &blob)?);
            if items.len() > options.limit {
                items.pop();
                next = items.last().map(|meta| meta.id);
                break;
            }
        }

        Ok(Page { items, next })
    }

    /// 原子批量写；任一操作失败则整批不生效。
    pub fn batch<T: Serialize>(
        &self,
        namespace: &str,
        collection: &str,
        ops: &[WriteOp<T>],
    ) -> Result<Vec<RecordMeta>> {
        let mut encoded: Vec<Zeroizing<Vec<u8>>> = Vec::new();
        for op in ops {
            if let WriteOp::Put { value, .. } = op {
                encoded.push(self.encode_value(value)?);
            }
        }

        let mut raw = Vec::with_capacity(ops.len());
        let mut next = 0;
        for op in ops {
            match op {
                WriteOp::Put { id, .. } => {
                    let plaintext = encoded
                        .get(next)
                        .ok_or(Error::Internal("batch encoding mismatch"))?;
                    raw.push(RawWriteOp::Put {
                        id: *id,
                        plaintext: plaintext.as_slice(),
                    });
                    next += 1;
                }
                WriteOp::Delete { id } => raw.push(RawWriteOp::Delete { id: *id }),
            }
        }

        self.batch_raw(namespace, collection, &raw)
    }

    /// 原子批量写，接收已序列化的明文。
    ///
    /// 所有操作在同一个 redb 写事务内；任一失败则整批回滚。
    pub fn batch_raw(
        &self,
        namespace: &str,
        collection: &str,
        ops: &[RawWriteOp<'_>],
    ) -> Result<Vec<RecordMeta>> {
        validate_names(namespace, collection)?;
        for op in ops {
            if let RawWriteOp::Put { plaintext, .. } = op {
                self.check_plaintext_len(plaintext.len())?;
            }
        }
        self.require_writable()?;

        let session = self.read_session()?;
        let txn = self.handle.begin_write()?;
        let mut metas = Vec::with_capacity(ops.len());
        let mut has_put = false;

        {
            let mut table = txn.open_table(records::TABLE)?;
            for op in ops {
                match op {
                    RawWriteOp::Put { id, plaintext } => {
                        has_put = true;
                        metas.push(self.stage_put(
                            &session,
                            &txn,
                            &mut table,
                            RecordPath {
                                namespace,
                                collection,
                                id: *id,
                            },
                            plaintext,
                            1,
                        )?);
                    }
                    RawWriteOp::Delete { id } => {
                        let meta = self
                            .stage_delete(
                                &session,
                                &txn,
                                &mut table,
                                RecordPath {
                                    namespace,
                                    collection,
                                    id: *id,
                                },
                            )?
                            .ok_or(Error::NotFound)?;
                        metas.push(meta);
                    }
                }
            }
        }

        if has_put {
            self.stage_catalog(&txn, namespace, collection)?;
        }
        txn.commit()?;
        Ok(metas)
    }

    /// 列出某命名空间下已登记的集合名，按名称升序。
    pub fn collections(&self, namespace: &str) -> Result<Vec<String>> {
        validate_name(NameKind::Namespace, namespace)?;
        let prefix = codec::encode_namespace_prefix(namespace)?;

        let txn = self.handle.begin_read()?;
        let table = txn.open_table(catalog::TABLE)?;
        let mut out = Vec::new();

        for entry in table.range(prefix.as_slice()..)? {
            let (key, _) = entry?;
            let key_bytes = key.value();
            if !key_bytes.starts_with(prefix.as_slice()) {
                break;
            }
            let (found_ns, collection) = codec::decode_catalog_key(key_bytes)?;
            if found_ns == namespace {
                out.push(collection);
            }
        }

        out.sort_unstable();
        Ok(out)
    }

    /// 当前库的统计信息。
    pub fn stats(&self) -> Result<Stats> {
        let session = self.read_session()?;
        let db_bytes = std::fs::metadata(&self.path).map_or(0, |meta| meta.len());
        Ok(Stats {
            format_version: session.format,
            key_id: session.current,
            db_bytes,
        })
    }

    /// 读取迁移进度；不会获取会话锁，可在迁移线程之外调用。
    pub fn migration_progress(&self) -> MigrationProgress {
        self.migration.snapshot()
    }

    /// 请求取消正在执行的迁移；在下一次批次边界生效。
    ///
    /// 没有迁移在执行时调用也是安全的，下一次迁移开始时会清除取消标志。
    pub fn cancel_migration(&self) {
        self.migration.request_cancel();
    }

    /// 持久化屏障。
    ///
    /// redb 每次提交即落盘，因此这里是显式的空操作，用于对齐契约语义。
    pub fn flush(&self) -> Result<()> {
        Ok(())
    }

    /// 改口令：用新盐与新 KEK 重新包裹会话内的 DK，不触碰记录。
    ///
    /// 升级期间返回 [`Error::Busy`]；`format=1` 仍要求只有一行 key。
    pub fn change_password(&self, new_password: &[u8]) -> Result<()> {
        self.require_writable()?;
        let mut session = self.write_session()?;

        if session.upgrading {
            return Err(Error::Busy);
        }

        if session.format == FORMAT {
            if session.legacy_keys.len() != 1 {
                return Err(Error::Busy);
            }

            let (key_id_u16, new_kek, salt, blob) = {
                let (key_id_u16, entry) = session.legacy_active()?;

                let mut salt = [0u8; kdf::SALT_LEN];
                getrandom::fill(&mut salt)
                    .map_err(|_| Error::Internal("operating system CSPRNG unavailable"))?;
                let params = entry.blob.kdf_params();
                let new_kek = kdf::derive_kek(new_password, &salt, &params)?;

                let key_id = LegacyKeyId::new(key_id_u16)?;
                let wrap_nonce = Nonce::random()?;
                let wrapped = aead::seal(
                    new_kek.as_bytes(),
                    &wrap_nonce,
                    &codec::keywrap_aad(key_id),
                    entry.data_key.as_bytes(),
                )?;
                let wrapped_dk: [u8; keys::WRAPPED_DK_LEN] = wrapped
                    .as_slice()
                    .try_into()
                    .map_err(|_| Error::Internal("wrapped data key has unexpected length"))?;

                let blob = keys::KeyBlob {
                    key_id: key_id_u16,
                    kdf_id: entry.blob.kdf_id,
                    flags: entry.blob.flags,
                    m_cost_kib: entry.blob.m_cost_kib,
                    t_cost: entry.blob.t_cost,
                    p_cost: entry.blob.p_cost,
                    salt,
                    wrap_nonce: *wrap_nonce.as_bytes(),
                    wrapped_dk,
                    created_at: entry.blob.created_at,
                    label: entry.blob.label.clone(),
                };
                (key_id_u16, new_kek, salt, blob)
            };

            let bytes = blob.encode()?;
            let txn = self.handle.begin_write()?;
            {
                let mut table = txn.open_table(keys::TABLE)?;
                table.insert(key_id_u16, bytes.as_slice())?;
            }
            txn.commit()?;

            session.kek = new_kek;
            session.salt = salt;
            session.kdf_id = blob.kdf_id;
            session.kdf_params = blob.kdf_params();
            if let Some(entry) = session.legacy_keys.get_mut(&key_id_u16) {
                entry.blob = blob;
            }
            return Ok(());
        }

        if !session.legacy_keys.is_empty() {
            return Err(Error::Busy);
        }
        if session.keys.is_empty() {
            return Err(Error::Internal("no data key available"));
        }

        let mut salt = [0u8; kdf::SALT_LEN];
        getrandom::fill(&mut salt)
            .map_err(|_| Error::Internal("operating system CSPRNG unavailable"))?;
        let params = session.kdf_params;
        let new_kek = kdf::derive_kek(new_password, &salt, &params)?;

        let mut rewritten: Vec<(KeyId, keys2::KeyBlob, Vec<u8>)> =
            Vec::with_capacity(session.keys.len());
        for entry in session.keys.values() {
            let wrap_nonce = Nonce::random()?;
            let wrapped = aead::seal(
                new_kek.as_bytes(),
                &wrap_nonce,
                &codec2::keywrap_aad(entry.blob.key_id),
                entry.data_key.as_bytes(),
            )?;
            let wrapped_dk: [u8; keys2::WRAPPED_DK_LEN] = wrapped
                .as_slice()
                .try_into()
                .map_err(|_| Error::Internal("wrapped data key has unexpected length"))?;
            let blob = keys2::KeyBlob {
                key_id: entry.blob.key_id,
                kdf_id: entry.blob.kdf_id,
                flags: 0,
                m_cost_kib: entry.blob.m_cost_kib,
                t_cost: entry.blob.t_cost,
                p_cost: entry.blob.p_cost,
                salt,
                wrap_nonce: *wrap_nonce.as_bytes(),
                wrapped_dk,
                created_at: entry.blob.created_at,
                label: entry.blob.label.clone(),
            };
            let bytes = blob.encode()?;
            rewritten.push((blob.key_id, blob, bytes));
        }

        let txn = self.handle.begin_write()?;
        {
            let mut table = txn.open_table(keys2::TABLE)?;
            for (key_id, _, bytes) in &rewritten {
                table.insert(key_id.as_bytes().as_slice(), bytes.as_slice())?;
            }
        }
        txn.commit()?;

        session.kek = new_kek;
        session.salt = salt;
        session.kdf_params = params;
        for (key_id, blob, _) in rewritten {
            if let Some(entry) = session.keys.get_mut(&key_id) {
                entry.blob = blob;
            }
        }
        Ok(())
    }

    /// 轮换数据密钥。
    ///
    /// `format=1` 使用递增 u16 编号并在结束时删除旧行；`format=2` 生成随机
    /// `key_id`、写公告与迁移公告，并保留旧 key 直到 [`Self::retire_key`]。
    pub fn rotate_data_key(&self) -> Result<()> {
        self.require_writable()?;

        if self.read_session()?.format == FORMAT_V2 {
            let info = self.begin_rotation()?;
            self.migrate_all(info.to)?;
            return Ok(());
        }

        let mut session = self.write_session()?;
        let old_id = session
            .legacy_current
            .ok_or(Error::Internal("active legacy data key missing"))?;
        let new_id = LegacyKeyId::new(old_id)?
            .next()
            .ok_or(Error::InvalidArgument("data key id space exhausted"))?
            .get();

        if !session.legacy_keys.contains_key(&new_id) {
            let (data_key, subkeys, key_blob) = {
                let old = session
                    .legacy_keys
                    .get(&old_id)
                    .ok_or(Error::Internal("active data key missing"))?;

                let mut dk_bytes = [0u8; KEY_LEN];
                getrandom::fill(&mut dk_bytes)
                    .map_err(|_| Error::Internal("operating system CSPRNG unavailable"))?;
                let data_key = DataKey::from_bytes(dk_bytes);
                let subkeys = kdf::derive_subkeys(&data_key)?;

                let wrap_nonce = Nonce::random()?;
                let wrapped = aead::seal(
                    session.kek.as_bytes(),
                    &wrap_nonce,
                    &codec::keywrap_aad(LegacyKeyId::new(new_id)?),
                    data_key.as_bytes(),
                )?;
                let wrapped_dk: [u8; keys::WRAPPED_DK_LEN] = wrapped
                    .as_slice()
                    .try_into()
                    .map_err(|_| Error::Internal("wrapped data key has unexpected length"))?;

                let blob = keys::KeyBlob {
                    key_id: new_id,
                    kdf_id: old.blob.kdf_id,
                    flags: 0,
                    m_cost_kib: old.blob.m_cost_kib,
                    t_cost: old.blob.t_cost,
                    p_cost: old.blob.p_cost,
                    salt: old.blob.salt,
                    wrap_nonce: *wrap_nonce.as_bytes(),
                    wrapped_dk,
                    created_at: Timestamp::now().as_millis(),
                    label: "rotated".to_owned(),
                };
                (data_key, subkeys, blob)
            };

            let key_bytes = key_blob.encode()?;
            let txn = self.handle.begin_write()?;
            {
                let mut table = txn.open_table(keys::TABLE)?;
                table.insert(new_id, key_bytes.as_slice())?;
            }
            txn.commit()?;

            session.legacy_keys.insert(
                new_id,
                LegacyKeyEntry {
                    blob: key_blob,
                    data_key,
                    subkeys,
                },
            );
        }

        self.migrate_records_v1(&session, new_id)?;

        let (new_meta, old_ids) = {
            let entry = session
                .legacy_keys
                .get(&new_id)
                .ok_or(Error::Internal("new data key missing"))?;
            let check_nonce = Nonce::random()?;
            let check_ct = aead::seal(
                entry.subkeys.check.as_bytes(),
                &check_nonce,
                codec::keycheck_aad(),
                meta::KEY_CHECK_PLAINTEXT,
            )?;
            let check_ct: [u8; meta::KEY_CHECK_CT_LEN] = check_ct
                .as_slice()
                .try_into()
                .map_err(|_| Error::Internal("key check has unexpected length"))?;

            let new_meta = meta::MetaBlob::new(
                new_id,
                session.created_at,
                &session.app_id,
                *check_nonce.as_bytes(),
                check_ct,
            )?;

            let old_ids: Vec<u16> = session
                .legacy_keys
                .keys()
                .copied()
                .filter(|id| *id != new_id)
                .collect();
            (new_meta, old_ids)
        };

        let meta_bytes = new_meta.encode()?;
        let txn = self.handle.begin_write()?;
        {
            let mut table = txn.open_table(meta::TABLE)?;
            table.insert(META_KEY, meta_bytes.as_slice())?;
        }
        {
            let mut table = txn.open_table(keys::TABLE)?;
            for id in &old_ids {
                table.remove(*id)?;
            }
        }
        txn.commit()?;

        session.legacy_current = Some(new_id);
        session.current = KeyId::from_u16(new_id);
        session.key_check_nonce = new_meta.key_check_nonce;
        session.key_check_ct = new_meta.key_check_ct;
        session.legacy_keys.retain(|id, _| *id == new_id);
        Ok(())
    }

    fn migrate_records_v1(&self, session: &Session, new_id: u16) -> Result<()> {
        let new_entry = session
            .legacy_keys
            .get(&new_id)
            .ok_or(Error::Internal("new data key missing"))?;

        let mut after: Option<Vec<u8>> = None;
        loop {
            let mut batch: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(ROTATE_BATCH);
            {
                let txn = self.handle.begin_read()?;
                let table = txn.open_table(records::TABLE)?;
                let range: (Bound<&[u8]>, Bound<&[u8]>) = match after.as_deref() {
                    Some(last) => (Bound::Excluded(last), Bound::Unbounded),
                    None => (Bound::Unbounded, Bound::Unbounded),
                };
                for item in table.range::<&[u8]>(range)? {
                    let (key, value) = item?;
                    batch.push((key.value().to_vec(), value.value().to_vec()));
                    if batch.len() >= ROTATE_BATCH {
                        break;
                    }
                }
            }

            let Some(last) = batch.last() else {
                break;
            };
            let last_key = last.0.clone();

            let txn = self.handle.begin_write()?;
            {
                let mut table = txn.open_table(records::TABLE)?;
                for (key, value) in &batch {
                    let blob = BlobView::decode(value)?;
                    if blob.key_id() == KeyId::from_u16(new_id) {
                        continue;
                    }

                    let (namespace, collection, id) = codec::decode_record_key(key)?;
                    let path = RecordPath {
                        namespace: &namespace,
                        collection: &collection,
                        id,
                    };
                    let plaintext = self.decrypt(session, path, &blob)?;

                    let legacy_id = blob
                        .key_id()
                        .as_u16()
                        .ok_or(Error::Corrupted("v1 record has a v2 key id"))?;
                    let header = codec::RecordHeader::new(
                        new_id,
                        blob.rev(),
                        blob.created_at(),
                        blob.updated_at(),
                        blob.data_version(),
                    )
                    .with_deleted(blob.is_deleted());
                    let _ = legacy_id;
                    let out = self.build_record_v1(&new_entry.subkeys, path, &plaintext, header)?;
                    table.insert(key.as_slice(), out.as_slice())?;
                }
            }
            txn.commit()?;

            after = Some(last_key);
        }

        Ok(())
    }

    fn read_session(&self) -> Result<RwLockReadGuard<'_, Session>> {
        self.session
            .read()
            .map_err(|_| Error::Internal("key session lock poisoned"))
    }

    fn write_session(&self) -> Result<RwLockWriteGuard<'_, Session>> {
        self.session
            .write()
            .map_err(|_| Error::Internal("key session lock poisoned"))
    }

    fn require_writable(&self) -> Result<()> {
        if self.handle.is_read_only() {
            return Err(Error::ReadOnly);
        }
        Ok(())
    }

    fn encode_value<T: Serialize>(&self, value: &T) -> Result<Zeroizing<Vec<u8>>> {
        let bytes = serde_json::to_vec(value).map_err(Error::Serialize)?;
        self.check_plaintext_len(bytes.len())?;
        Ok(Zeroizing::new(bytes))
    }

    fn check_plaintext_len(&self, size: usize) -> Result<()> {
        if size > self.max_value_size {
            return Err(Error::ValueTooLarge {
                size,
                max: self.max_value_size,
            });
        }
        Ok(())
    }

    fn stage_put(
        &self,
        session: &Session,
        txn: &WriteTransaction,
        table: &mut Table<'_, &[u8], &[u8]>,
        path: RecordPath<'_>,
        plaintext: &[u8],
        data_version: u16,
    ) -> Result<RecordMeta> {
        let key = codec::encode_record_key(path.namespace, path.collection, &path.id)?;
        let existing = table
            .get(key.as_slice())?
            .map(|guard| guard.value().to_vec());

        let now = Timestamp::now().as_millis();
        let (rev, created_at, previous_updated) = match &existing {
            Some(raw) => {
                let blob = BlobView::decode(raw)?;
                let rev = blob
                    .rev()
                    .checked_add(1)
                    .ok_or(Error::Internal("record revision overflow"))?;
                (rev, blob.created_at(), blob.updated_at())
            }
            None => (1, now, now),
        };
        let updated_at = if session.sync.is_some() {
            now.max(previous_updated.saturating_add(1))
        } else {
            now
        };

        let (id, blob) = if session.format == FORMAT {
            let (legacy_id, entry) = session.legacy_active()?;
            let header =
                codec::RecordHeader::new(legacy_id, rev, created_at, updated_at, data_version);
            let blob = self.build_record_v1(&entry.subkeys, path, plaintext, header)?;
            (KeyId::from_u16(legacy_id), blob)
        } else {
            let entry = session.active()?;
            let header = codec2::RecordHeader::new(
                session.current,
                rev,
                created_at,
                updated_at,
                data_version,
            );
            let blob = self.build_record_v2(&entry.subkeys, path, plaintext, header)?;
            (session.current, blob)
        };
        table.insert(key.as_slice(), blob.as_slice())?;

        let meta = record_meta_from(&blob, path.id)?;

        if let Some(device) = session.sync {
            if session.format == FORMAT_V2 {
                append_change(
                    txn,
                    device,
                    &ChangeEntry::Record(RecordChange {
                        namespace: path.namespace.to_owned(),
                        collection: path.collection.to_owned(),
                        id: path.id,
                        key_id: id,
                        rev,
                        updated_at: Timestamp::from_millis(updated_at),
                        deleted: false,
                        writer: device,
                        blob,
                    }),
                )?;
            }
        }

        Ok(meta)
    }

    fn stage_delete(
        &self,
        session: &Session,
        txn: &WriteTransaction,
        table: &mut Table<'_, &[u8], &[u8]>,
        path: RecordPath<'_>,
    ) -> Result<Option<RecordMeta>> {
        let key = codec::encode_record_key(path.namespace, path.collection, &path.id)?;
        let Some(raw) = table
            .get(key.as_slice())?
            .map(|guard| guard.value().to_vec())
        else {
            return Ok(None);
        };

        let blob = BlobView::decode(&raw)?;
        let plaintext = self.decrypt(session, path, &blob)?;

        let rev = blob
            .rev()
            .checked_add(1)
            .ok_or(Error::Internal("record revision overflow"))?;
        let now = Timestamp::now().as_millis();
        let updated_at = if session.sync.is_some() {
            now.max(blob.updated_at().saturating_add(1))
        } else {
            now
        };

        let (id, out) = if session.format == FORMAT {
            let (legacy_id, entry) = session.legacy_active()?;
            let header = codec::RecordHeader::new(
                legacy_id,
                rev,
                blob.created_at(),
                updated_at,
                blob.data_version(),
            )
            .with_deleted(true);
            let out = self.build_record_v1(&entry.subkeys, path, &plaintext, header)?;
            (KeyId::from_u16(legacy_id), out)
        } else {
            let entry = session.active()?;
            let header = codec2::RecordHeader::new(
                session.current,
                rev,
                blob.created_at(),
                updated_at,
                blob.data_version(),
            )
            .with_deleted(true);
            let out = self.build_record_v2(&entry.subkeys, path, &plaintext, header)?;
            (session.current, out)
        };
        table.insert(key.as_slice(), out.as_slice())?;

        let meta = record_meta_from(&out, path.id)?;
        if let Some(device) = session.sync {
            if session.format == FORMAT_V2 {
                append_change(
                    txn,
                    device,
                    &ChangeEntry::Record(RecordChange {
                        namespace: path.namespace.to_owned(),
                        collection: path.collection.to_owned(),
                        id: path.id,
                        key_id: id,
                        rev,
                        updated_at: Timestamp::from_millis(updated_at),
                        deleted: true,
                        writer: device,
                        blob: out,
                    }),
                )?;
            }
        }

        Ok(Some(meta))
    }

    fn build_record_v1(
        &self,
        subkeys: &Subkeys,
        path: RecordPath<'_>,
        plaintext: &[u8],
        header: codec::RecordHeader,
    ) -> Result<Vec<u8>> {
        let header_bytes = header.encode();
        let aad = codec::record_aad(path.namespace, path.collection, &path.id, &header_bytes)?;
        let nonce = Nonce::random()?;
        let ciphertext = aead::seal(subkeys.record.as_bytes(), &nonce, &aad, plaintext)?;
        let ct_len = u32::try_from(ciphertext.len())
            .map_err(|_| Error::Internal("ciphertext length overflow"))?;

        let mut out = Vec::with_capacity(CT_OFFSET + ciphertext.len());
        out.extend_from_slice(&header_bytes);
        out.extend_from_slice(nonce.as_bytes());
        out.extend_from_slice(&ct_len.to_be_bytes());
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    fn build_record_v2(
        &self,
        subkeys: &Subkeys,
        path: RecordPath<'_>,
        plaintext: &[u8],
        header: codec2::RecordHeader,
    ) -> Result<Vec<u8>> {
        let header_bytes = header.encode();
        let aad = codec2::record_aad(path.namespace, path.collection, &path.id, &header_bytes)?;
        let nonce = Nonce::random()?;
        let ciphertext = aead::seal(subkeys.record.as_bytes(), &nonce, &aad, plaintext)?;
        let ct_len = u32::try_from(ciphertext.len())
            .map_err(|_| Error::Internal("ciphertext length overflow"))?;

        let mut out = Vec::with_capacity(codec2::CT_OFFSET + ciphertext.len());
        out.extend_from_slice(&header_bytes);
        out.extend_from_slice(nonce.as_bytes());
        out.extend_from_slice(&ct_len.to_be_bytes());
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    fn decrypt(
        &self,
        session: &Session,
        path: RecordPath<'_>,
        blob: &BlobView<'_>,
    ) -> Result<Zeroizing<Vec<u8>>> {
        match blob {
            BlobView::V1(blob) => {
                let entry = session
                    .legacy_keys
                    .get(&blob.header.key_id)
                    .ok_or(Error::Corrupted("record references an unknown data key"))?;
                let aad = codec::record_aad(
                    path.namespace,
                    path.collection,
                    &path.id,
                    &blob.header.encode(),
                )?;
                let nonce = Nonce::from_bytes(*blob.nonce);
                let plaintext = aead::open(
                    entry.subkeys.record.as_bytes(),
                    &nonce,
                    &aad,
                    blob.ciphertext,
                )?;
                Ok(Zeroizing::new(plaintext))
            }
            BlobView::V2(blob) => {
                let entry =
                    session
                        .keys
                        .get(&blob.header.key_id)
                        .ok_or(Error::RotationRequired {
                            key_id: blob.header.key_id,
                        })?;
                let aad = codec2::record_aad(
                    path.namespace,
                    path.collection,
                    &path.id,
                    &blob.header.encode(),
                )?;
                let nonce = Nonce::from_bytes(*blob.nonce);
                let plaintext = aead::open(
                    entry.subkeys.record.as_bytes(),
                    &nonce,
                    &aad,
                    blob.ciphertext,
                )?;
                Ok(Zeroizing::new(plaintext))
            }
        }
    }

    fn stage_catalog(
        &self,
        txn: &redb::WriteTransaction,
        namespace: &str,
        collection: &str,
    ) -> Result<()> {
        let key = codec::encode_catalog_key(namespace, collection)?;
        let mut table = txn.open_table(catalog::TABLE)?;
        let exists = table.get(key.as_slice())?.is_some();
        if !exists {
            let blob = catalog::CatalogBlob::new(Timestamp::now().as_millis());
            table.insert(key.as_slice(), blob.encode().as_slice())?;
        }
        Ok(())
    }
}

fn validate_open_options(options: &EngineOptions) -> Result<()> {
    if options.max_value_size == 0 {
        return Err(Error::InvalidArgument("max_value_size must be positive"));
    }
    if options
        .max_value_size
        .checked_add(codec::TAG_LEN)
        .and_then(|size| u32::try_from(size).ok())
        .is_none()
    {
        return Err(Error::InvalidArgument(
            "max_value_size is too large for the record format",
        ));
    }
    if options.cache_size == Some(0) {
        return Err(Error::InvalidArgument("cache_size must be positive"));
    }
    if options.read_only && options.create {
        return Err(Error::InvalidArgument(
            "read_only cannot be combined with create",
        ));
    }
    Ok(())
}

fn initialize(db: &Database, password: &[u8], options: &EngineOptions) -> Result<()> {
    let txn = db.begin_write()?;
    {
        let table = txn.open_table(meta::TABLE)?;
        if table.get(META_KEY)?.is_some() {
            // 已初始化（并发创建时的后到者）：放弃本地事务，转为普通打开。
            return Ok(());
        }
    }

    let key_id = LegacyKeyId::new(1)?;
    let mut salt = [0u8; kdf::SALT_LEN];
    getrandom::fill(&mut salt)
        .map_err(|_| Error::Internal("operating system CSPRNG unavailable"))?;
    let kek = kdf::derive_kek(password, &salt, &options.kdf)?;

    let mut dk_bytes = [0u8; KEY_LEN];
    getrandom::fill(&mut dk_bytes)
        .map_err(|_| Error::Internal("operating system CSPRNG unavailable"))?;
    let data_key = DataKey::from_bytes(dk_bytes);

    let wrap_nonce = Nonce::random()?;
    let wrapped = aead::seal(
        kek.as_bytes(),
        &wrap_nonce,
        &codec::keywrap_aad(key_id),
        data_key.as_bytes(),
    )?;
    let wrapped_dk: [u8; keys::WRAPPED_DK_LEN] = wrapped
        .as_slice()
        .try_into()
        .map_err(|_| Error::Internal("wrapped data key has unexpected length"))?;

    let subkeys = kdf::derive_subkeys(&data_key)?;
    let check_nonce = Nonce::random()?;
    let check_ct = aead::seal(
        subkeys.check.as_bytes(),
        &check_nonce,
        codec::keycheck_aad(),
        meta::KEY_CHECK_PLAINTEXT,
    )?;
    let check_ct: [u8; meta::KEY_CHECK_CT_LEN] = check_ct
        .as_slice()
        .try_into()
        .map_err(|_| Error::Internal("key check has unexpected length"))?;

    let created_at = Timestamp::now().as_millis();
    let key_blob = keys::KeyBlob {
        key_id: key_id.get(),
        kdf_id: KdfId::Argon2idV1.as_u8(),
        flags: 0,
        m_cost_kib: options.kdf.m_cost_kib,
        t_cost: options.kdf.t_cost,
        p_cost: options.kdf.p_cost,
        salt,
        wrap_nonce: *wrap_nonce.as_bytes(),
        wrapped_dk,
        created_at,
        label: "primary".to_owned(),
    };
    let meta_blob = meta::MetaBlob::new(
        key_id.get(),
        created_at,
        &options.app_id,
        *check_nonce.as_bytes(),
        check_ct,
    )?;

    let key_bytes = key_blob.encode()?;
    let meta_bytes = meta_blob.encode()?;
    {
        let mut table = txn.open_table(keys::TABLE)?;
        table.insert(key_id.get(), key_bytes.as_slice())?;
    }
    {
        let mut table = txn.open_table(meta::TABLE)?;
        table.insert(META_KEY, meta_bytes.as_slice())?;
    }
    // 初始化时把四张表都建好：读路径不负责建表。
    {
        let _records = txn.open_table(records::TABLE)?;
        let _catalog = txn.open_table(catalog::TABLE)?;
    }
    txn.commit()?;
    Ok(())
}

fn unlock(handle: &Handle, password: &[u8]) -> Result<Session> {
    let txn = handle.begin_read()?;
    let raw = {
        let table = txn.open_table(meta::TABLE)?;
        table
            .get(META_KEY)?
            .map(|guard| guard.value().to_vec())
            .ok_or(Error::NotFound)?
    };
    if raw.len() < 6 || &raw[0..4] != meta::MAGIC {
        return Err(Error::Corrupted("meta magic mismatch"));
    }
    let format = u16::from_be_bytes([raw[4], raw[5]]);

    match format {
        FORMAT => unlock_v1(&txn, password, &raw),
        FORMAT_V2 => unlock_v2(&txn, password, &raw),
        other => Err(Error::UnsupportedFormat {
            found: other,
            supported: FORMAT_V2,
        }),
    }
}

fn unlock_v1(txn: &redb::ReadTransaction, password: &[u8], raw: &[u8]) -> Result<Session> {
    let meta_blob = meta::MetaBlob::decode(raw)?;
    let blobs = load_legacy_blobs(txn)?;

    let current_blob = blobs
        .get(&meta_blob.key_id)
        .ok_or(Error::Corrupted("meta references a missing data key"))?;
    let kek = kdf::derive_kek(password, &current_blob.salt, &current_blob.kdf_params())?;
    let map = unwrap_legacy(blobs, &kek)?;

    let current_entry = map
        .get(&meta_blob.key_id)
        .ok_or(Error::Corrupted("meta references a missing data key"))?;
    let check_nonce = Nonce::from_bytes(meta_blob.key_check_nonce);
    let check = Zeroizing::new(
        aead::open(
            current_entry.subkeys.check.as_bytes(),
            &check_nonce,
            codec::keycheck_aad(),
            &meta_blob.key_check_ct,
        )
        .map_err(|_| Error::InvalidKey)?,
    );
    if check.as_slice() != meta::KEY_CHECK_PLAINTEXT.as_slice() {
        return Err(Error::InvalidKey);
    }

    let salt = current_entry.blob.salt;
    let kdf_params = current_entry.blob.kdf_params();
    let kdf_id = current_entry.blob.kdf_id;
    Ok(Session {
        format: FORMAT,
        created_at: meta_blob.created_at,
        app_id: meta_blob.app_id.clone(),
        current: KeyId::from_u16(meta_blob.key_id),
        keys: BTreeMap::new(),
        legacy_current: Some(meta_blob.key_id),
        legacy_keys: map,
        legacy_map: BTreeMap::new(),
        kek,
        salt,
        kdf_id,
        kdf_params,
        key_check_nonce: meta_blob.key_check_nonce,
        key_check_ct: meta_blob.key_check_ct,
        sync: None,
        upgrading: false,
    })
}

fn unlock_v2(txn: &redb::ReadTransaction, password: &[u8], raw: &[u8]) -> Result<Session> {
    let meta_blob = meta2::MetaBlob::decode(raw)?;
    let key2_blobs = load_key2_blobs(txn)?;

    let current_blob = key2_blobs
        .iter()
        .find(|blob| blob.key_id == meta_blob.key_id)
        .ok_or(Error::Corrupted("meta references a missing data key"))?;
    let kek = kdf::derive_kek(password, &current_blob.salt, &current_blob.kdf_params())?;

    let mut keys = BTreeMap::new();
    for blob in key2_blobs {
        let wrap_nonce = Nonce::from_bytes(blob.wrap_nonce);
        let unwrapped = Zeroizing::new(
            aead::open(
                kek.as_bytes(),
                &wrap_nonce,
                &codec2::keywrap_aad(blob.key_id),
                &blob.wrapped_dk,
            )
            .map_err(|_| Error::InvalidKey)?,
        );
        let dk_bytes: [u8; KEY_LEN] = unwrapped
            .as_slice()
            .try_into()
            .map_err(|_| Error::Corrupted("unwrapped data key has wrong length"))?;
        let data_key = DataKey::from_bytes(dk_bytes);
        let subkeys = kdf::derive_subkeys(&data_key)?;
        keys.insert(
            blob.key_id,
            KeyEntry {
                blob,
                data_key,
                subkeys,
            },
        );
    }

    let current_entry = keys
        .get(&meta_blob.key_id)
        .ok_or(Error::Corrupted("meta references a missing data key"))?;
    let check_nonce = Nonce::from_bytes(meta_blob.key_check_nonce);
    let check = Zeroizing::new(
        aead::open(
            current_entry.subkeys.check.as_bytes(),
            &check_nonce,
            codec2::keycheck_aad(),
            &meta_blob.key_check_ct,
        )
        .map_err(|_| Error::InvalidKey)?,
    );
    if check.as_slice() != codec2::KEY_CHECK_PLAINTEXT.as_slice() {
        return Err(Error::InvalidKey);
    }

    let salt = current_entry.blob.salt;
    let kdf_id = current_entry.blob.kdf_id;
    let kdf_params = current_entry.blob.kdf_params();
    let mut session = Session {
        format: FORMAT_V2,
        created_at: meta_blob.created_at,
        app_id: meta_blob.app_id.clone(),
        current: meta_blob.key_id,
        keys,
        legacy_current: None,
        legacy_keys: BTreeMap::new(),
        legacy_map: BTreeMap::new(),
        kek,
        salt,
        kdf_id,
        kdf_params,
        key_check_nonce: meta_blob.key_check_nonce,
        key_check_ct: meta_blob.key_check_ct,
        sync: None,
        upgrading: false,
    };

    if table_exists(txn, "shio.keys")? {
        let blobs = load_legacy_blobs(txn)?;
        if !blobs.is_empty() {
            let legacy = unwrap_legacy(blobs, &session.kek)?;
            session.legacy_current = legacy.keys().next().copied();
            session.legacy_keys = legacy;
        }
        if table_exists(txn, "shio.sync")? {
            let table = txn.open_table(sync_state::TABLE)?;
            if let Some(value) = table.get(sync_state::KEY_UPGRADE)? {
                let (_, cursor) = sync_state::decode_upgrade(value.value())?;
                let _ = cursor;
                session.upgrading = true;
            }
            let prefix = sync_state::upgrade_key_prefix();
            for item in table.range(prefix..)? {
                let (key, value) = item?;
                let key = key.value();
                if !key.starts_with(prefix) {
                    break;
                }
                if key.len() != prefix.len() + 2 || value.value().len() != 16 {
                    return Err(Error::Corrupted("upgrade key mapping is malformed"));
                }
                let legacy = u16::from_be_bytes([key[prefix.len()], key[prefix.len() + 1]]);
                let mapped: [u8; 16] = value
                    .value()
                    .try_into()
                    .map_err(|_| Error::Corrupted("upgrade key mapping has wrong length"))?;
                session.legacy_map.insert(legacy, KeyId::from_bytes(mapped));
            }
        }
    }

    if table_exists(txn, "shio.sync")? {
        let table = txn.open_table(sync_state::TABLE)?;
        if let Some(value) = table.get(sync_state::KEY_DEVICE)? {
            if value.value().len() != 16 {
                return Err(Error::Corrupted("sync device id has wrong length"));
            }
            let bytes: [u8; 16] = value
                .value()
                .try_into()
                .map_err(|_| Error::Corrupted("sync device id has wrong length"))?;
            let device = DeviceId::from_bytes(bytes);
            if device.is_zero() {
                return Err(Error::Corrupted("sync device id is zero"));
            }
            session.sync = Some(device);
        }
    }

    Ok(session)
}

fn table_exists(txn: &redb::ReadTransaction, name: &str) -> Result<bool> {
    for handle in txn.list_tables()? {
        if handle.name() == name {
            return Ok(true);
        }
    }
    Ok(false)
}

fn load_key2_blobs(txn: &redb::ReadTransaction) -> Result<Vec<keys2::KeyBlob>> {
    let table = txn.open_table(keys2::TABLE)?;
    let mut out = Vec::new();
    for item in table.iter()? {
        let (_, value) = item?;
        out.push(keys2::KeyBlob::decode(value.value())?);
    }
    Ok(out)
}

fn load_legacy_blobs(txn: &redb::ReadTransaction) -> Result<BTreeMap<u16, keys::KeyBlob>> {
    let table = txn.open_table(keys::TABLE)?;
    let mut out = BTreeMap::new();
    for item in table.iter()? {
        let (_, value) = item?;
        let blob = keys::KeyBlob::decode(value.value())?;
        out.insert(blob.key_id, blob);
    }
    Ok(out)
}

fn unwrap_legacy(
    blobs: BTreeMap<u16, keys::KeyBlob>,
    kek: &Kek,
) -> Result<BTreeMap<u16, LegacyKeyEntry>> {
    let mut out = BTreeMap::new();
    for (id, blob) in blobs {
        let key_id = LegacyKeyId::new(id).map_err(|_| Error::Corrupted("invalid key id"))?;
        let wrap_nonce = Nonce::from_bytes(blob.wrap_nonce);
        let unwrapped = Zeroizing::new(
            aead::open(
                kek.as_bytes(),
                &wrap_nonce,
                &codec::keywrap_aad(key_id),
                &blob.wrapped_dk,
            )
            .map_err(|_| Error::InvalidKey)?,
        );
        let dk_bytes: [u8; KEY_LEN] = unwrapped
            .as_slice()
            .try_into()
            .map_err(|_| Error::Corrupted("unwrapped data key has wrong length"))?;
        let data_key = DataKey::from_bytes(dk_bytes);
        let subkeys = kdf::derive_subkeys(&data_key)?;
        out.insert(
            id,
            LegacyKeyEntry {
                blob,
                data_key,
                subkeys,
            },
        );
    }
    Ok(out)
}

fn record_meta_from(raw: &[u8], id: RecordId) -> Result<RecordMeta> {
    let blob = BlobView::decode(raw)?;
    let ciphertext_len = u32::try_from(blob.ciphertext().len())
        .map_err(|_| Error::Internal("ciphertext length overflow"))?;
    Ok(RecordMeta {
        id,
        rev: blob.rev(),
        created_at: Timestamp::from_millis(blob.created_at()),
        updated_at: Timestamp::from_millis(blob.updated_at()),
        data_version: blob.data_version(),
        deleted: blob.is_deleted(),
        ciphertext_len,
    })
}

fn record_meta(id: RecordId, blob: &BlobView<'_>) -> Result<RecordMeta> {
    let ciphertext_len = u32::try_from(blob.ciphertext().len())
        .map_err(|_| Error::Internal("ciphertext length overflow"))?;
    Ok(RecordMeta {
        id,
        rev: blob.rev(),
        created_at: Timestamp::from_millis(blob.created_at()),
        updated_at: Timestamp::from_millis(blob.updated_at()),
        data_version: blob.data_version(),
        deleted: blob.is_deleted(),
        ciphertext_len,
    })
}

fn validate_names(namespace: &str, collection: &str) -> Result<()> {
    validate_name(NameKind::Namespace, namespace)?;
    validate_name(NameKind::Collection, collection)?;
    Ok(())
}

/// 追加一条本地变更日志，返回分配到的序号。
fn append_change(txn: &WriteTransaction, _device: DeviceId, entry: &ChangeEntry) -> Result<u64> {
    let bytes = changes::encode(entry)?;
    let mut table = txn.open_table(changes::TABLE)?;
    let next = match table.last()? {
        Some((key, _)) => key
            .value()
            .checked_add(1)
            .ok_or(Error::Internal("change log sequence overflow"))?,
        None => 1,
    };
    table.insert(next, bytes.as_slice())?;
    Ok(next)
}
