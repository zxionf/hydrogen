//! 同步内核引擎：打开库、解锁密钥、按命名空间/集合/记录读写。
//!
//! 事务边界：每次公开写操作是一个 redb 写事务，`batch` 的所有操作在同一事务内，
//! 任一失败则整批不生效。读操作使用独立读事务，看到一致快照。
//!
//! 当前不支持 `read_only`、改口令与数据密钥轮换；这些能力在契约中保留，后续补齐。

use std::fmt;
use std::path::PathBuf;

use redb::{Database, ReadableDatabase, ReadableTable, Table};
use serde::de::DeserializeOwned;
use serde::Serialize;
use zeroize::Zeroizing;

use crate::codec::{self, RecordBlob, RecordHeader, CT_OFFSET, NONCE_LEN};
use crate::crypto::aead::{self, Nonce};
use crate::crypto::kdf::{self, KdfId, KdfParams, Subkeys};
use crate::crypto::keys::{DataKey, Kek, KeyId, KEY_LEN};
use crate::error::{Error, Result};
use crate::model::{
    validate_name, ListOptions, NameKind, Page, RecordId, RecordMeta, Stored, Timestamp, WriteOp,
    DEFAULT_MAX_VALUE_SIZE,
};
use crate::store::{catalog, keys, meta, records, META_KEY};

/// 打开引擎的参数。
#[derive(Debug, Clone)]
pub struct EngineOptions {
    /// 数据库文件路径。
    pub path: PathBuf,
    /// 文件不存在时是否创建。
    pub create: bool,
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
    pub key_id: u16,
    /// 库文件字节数。
    pub db_bytes: u64,
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

/// 同步内核句柄。
///
/// 持有 redb 数据库与解锁后的密钥材料；`Send + Sync`，可在阻塞线程池中共享。
pub struct Engine {
    db: Database,
    path: PathBuf,
    meta: meta::MetaBlob,
    key_id: KeyId,
    ring: KeyRing,
    max_value_size: usize,
}

struct KeyRing {
    /// 会话内保留，供改口令与轮换使用。
    #[allow(dead_code)]
    kek: Kek,
    /// 会话内保留，供轮换使用。
    #[allow(dead_code)]
    data_key: DataKey,
    subkeys: Subkeys,
}

impl fmt::Debug for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Engine")
            .field("path", &self.path)
            .field("format", &self.meta.format)
            .field("key_id", &self.key_id.get())
            .field("max_value_size", &self.max_value_size)
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// 打开或创建数据库，并完成口令派生、DK 解包与 key check。
    pub fn open(options: EngineOptions, password: &[u8]) -> Result<Self> {
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

        if !options.create && !options.path.exists() {
            return Err(Error::NotFound);
        }

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

        let (meta_blob, ring) = unlock(&db, password)?;
        let key_id = KeyId::new(meta_blob.key_id)?;

        Ok(Self {
            db,
            path: options.path,
            meta: meta_blob,
            key_id,
            ring,
            max_value_size: options.max_value_size,
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

        let txn = self.db.begin_write()?;
        let meta = {
            let mut table = txn.open_table(records::TABLE)?;
            self.stage_put(
                &mut table,
                namespace,
                collection,
                id,
                plaintext,
                data_version,
            )?
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

        let txn = self.db.begin_read()?;
        let raw = {
            let table = txn.open_table(records::TABLE)?;
            table
                .get(key.as_slice())?
                .map(|guard| guard.value().to_vec())
        };
        let Some(raw) = raw else {
            return Ok(None);
        };

        let blob = RecordBlob::decode(&raw)?;
        if blob.header.is_deleted() {
            return Ok(None);
        }

        let plaintext = self.decrypt(
            namespace,
            collection,
            id,
            &blob.header,
            blob.nonce,
            blob.ciphertext,
        )?;
        let meta = record_meta(*id, &blob.header, blob.ciphertext.len())?;
        Ok(Some((meta, plaintext)))
    }

    /// 写入墓碑；记录不存在时返回 `false`。
    pub fn delete(&self, namespace: &str, collection: &str, id: &RecordId) -> Result<bool> {
        validate_names(namespace, collection)?;
        let txn = self.db.begin_write()?;
        let existed = {
            let mut table = txn.open_table(records::TABLE)?;
            self.stage_delete(&mut table, namespace, collection, id)?
                .is_some()
        };
        txn.commit()?;
        Ok(existed)
    }

    /// 物理删除；多端同步启用后不得使用。
    pub fn purge(&self, namespace: &str, collection: &str, id: &RecordId) -> Result<bool> {
        let key = codec::encode_record_key(namespace, collection, id)?;
        let txn = self.db.begin_write()?;
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

        let txn = self.db.begin_read()?;
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

            let blob = RecordBlob::decode(value.value())?;
            if blob.header.is_deleted() && !options.include_deleted {
                continue;
            }

            items.push(record_meta(id, &blob.header, blob.ciphertext.len())?);
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

        let txn = self.db.begin_write()?;
        let mut metas = Vec::with_capacity(ops.len());
        let mut has_put = false;

        {
            let mut table = txn.open_table(records::TABLE)?;
            for op in ops {
                match op {
                    RawWriteOp::Put { id, plaintext } => {
                        has_put = true;
                        metas.push(
                            self.stage_put(&mut table, namespace, collection, *id, plaintext, 1)?,
                        );
                    }
                    RawWriteOp::Delete { id } => {
                        let meta = self
                            .stage_delete(&mut table, namespace, collection, id)?
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

        let txn = self.db.begin_read()?;
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
        let db_bytes = std::fs::metadata(&self.path).map_or(0, |meta| meta.len());
        Ok(Stats {
            format_version: self.meta.format,
            key_id: self.key_id.get(),
            db_bytes,
        })
    }

    /// 持久化屏障。
    ///
    /// redb 每次提交即落盘，因此这里是显式的空操作，用于对齐契约语义。
    pub fn flush(&self) -> Result<()> {
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
        table: &mut Table<'_, &[u8], &[u8]>,
        namespace: &str,
        collection: &str,
        id: RecordId,
        plaintext: &[u8],
        data_version: u16,
    ) -> Result<RecordMeta> {
        let key = codec::encode_record_key(namespace, collection, &id)?;
        let existing = table
            .get(key.as_slice())?
            .map(|guard| guard.value().to_vec());

        let now = Timestamp::now().as_millis();
        let (rev, created_at) = match existing {
            Some(raw) => {
                let blob = RecordBlob::decode(&raw)?;
                let rev = blob
                    .header
                    .rev
                    .checked_add(1)
                    .ok_or(Error::Internal("record revision overflow"))?;
                (rev, blob.header.created_at)
            }
            None => (1, now),
        };

        let header = RecordHeader::new(self.key_id.get(), rev, created_at, now, data_version);
        let blob = self.build_record(namespace, collection, &id, plaintext, header)?;
        table.insert(key.as_slice(), blob.as_slice())?;

        record_meta(id, &header, blob.len() - CT_OFFSET)
    }

    fn stage_delete(
        &self,
        table: &mut Table<'_, &[u8], &[u8]>,
        namespace: &str,
        collection: &str,
        id: &RecordId,
    ) -> Result<Option<RecordMeta>> {
        let key = codec::encode_record_key(namespace, collection, id)?;
        let Some(raw) = table
            .get(key.as_slice())?
            .map(|guard| guard.value().to_vec())
        else {
            return Ok(None);
        };

        let blob = RecordBlob::decode(&raw)?;
        let plaintext = self.decrypt(
            namespace,
            collection,
            id,
            &blob.header,
            blob.nonce,
            blob.ciphertext,
        )?;

        let rev = blob
            .header
            .rev
            .checked_add(1)
            .ok_or(Error::Internal("record revision overflow"))?;
        let now = Timestamp::now().as_millis();
        let header = RecordHeader::new(
            self.key_id.get(),
            rev,
            blob.header.created_at,
            now,
            blob.header.data_version,
        )
        .with_deleted(true);

        let out = self.build_record(namespace, collection, id, &plaintext, header)?;
        table.insert(key.as_slice(), out.as_slice())?;

        Ok(Some(record_meta(*id, &header, out.len() - CT_OFFSET)?))
    }

    fn build_record(
        &self,
        namespace: &str,
        collection: &str,
        id: &RecordId,
        plaintext: &[u8],
        header: RecordHeader,
    ) -> Result<Vec<u8>> {
        let header_bytes = header.encode();
        let aad = codec::record_aad(namespace, collection, id, &header_bytes)?;
        let nonce = Nonce::random()?;
        let ciphertext = aead::seal(self.ring.subkeys.record.as_bytes(), &nonce, &aad, plaintext)?;
        let ct_len = u32::try_from(ciphertext.len())
            .map_err(|_| Error::Internal("ciphertext length overflow"))?;

        let mut out = Vec::with_capacity(CT_OFFSET + ciphertext.len());
        out.extend_from_slice(&header_bytes);
        out.extend_from_slice(nonce.as_bytes());
        out.extend_from_slice(&ct_len.to_be_bytes());
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    fn decrypt(
        &self,
        namespace: &str,
        collection: &str,
        id: &RecordId,
        header: &RecordHeader,
        nonce: &[u8; NONCE_LEN],
        ciphertext: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>> {
        if header.key_id != self.key_id.get() {
            return Err(Error::Corrupted(
                "record key id does not match the active key",
            ));
        }
        let aad = codec::record_aad(namespace, collection, id, &header.encode())?;
        let nonce = Nonce::from_bytes(*nonce);
        let plaintext = aead::open(
            self.ring.subkeys.record.as_bytes(),
            &nonce,
            &aad,
            ciphertext,
        )?;
        Ok(Zeroizing::new(plaintext))
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

fn initialize(db: &Database, password: &[u8], options: &EngineOptions) -> Result<()> {
    let txn = db.begin_write()?;
    {
        let table = txn.open_table(meta::TABLE)?;
        if table.get(META_KEY)?.is_some() {
            // 已初始化（并发创建时的后到者）：放弃本地事务，转为普通打开。
            return Ok(());
        }
    }

    let key_id = KeyId::new(1)?;
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

fn unlock(db: &Database, password: &[u8]) -> Result<(meta::MetaBlob, KeyRing)> {
    let txn = db.begin_read()?;
    let meta_blob = {
        let table = txn.open_table(meta::TABLE)?;
        let raw = table.get(META_KEY)?.ok_or(Error::NotFound)?;
        meta::MetaBlob::decode(raw.value())?
    };
    let key_blob = {
        let table = txn.open_table(keys::TABLE)?;
        let raw = table
            .get(meta_blob.key_id)?
            .ok_or(Error::Corrupted("meta references a missing data key"))?;
        keys::KeyBlob::decode(raw.value())?
    };
    drop(txn);

    let key_id = KeyId::new(key_blob.key_id).map_err(|_| Error::Corrupted("invalid key id"))?;
    let params = key_blob.kdf_params();
    let kek = kdf::derive_kek(password, &key_blob.salt, &params)?;

    let wrap_nonce = Nonce::from_bytes(key_blob.wrap_nonce);
    let unwrapped = Zeroizing::new(
        aead::open(
            kek.as_bytes(),
            &wrap_nonce,
            &codec::keywrap_aad(key_id),
            &key_blob.wrapped_dk,
        )
        .map_err(|_| Error::InvalidKey)?,
    );
    let dk_bytes: [u8; KEY_LEN] = unwrapped
        .as_slice()
        .try_into()
        .map_err(|_| Error::Corrupted("unwrapped data key has wrong length"))?;
    let data_key = DataKey::from_bytes(dk_bytes);

    let subkeys = kdf::derive_subkeys(&data_key)?;
    let check_nonce = Nonce::from_bytes(meta_blob.key_check_nonce);
    let check = Zeroizing::new(
        aead::open(
            subkeys.check.as_bytes(),
            &check_nonce,
            codec::keycheck_aad(),
            &meta_blob.key_check_ct,
        )
        .map_err(|_| Error::InvalidKey)?,
    );
    if check.as_slice() != meta::KEY_CHECK_PLAINTEXT.as_slice() {
        return Err(Error::InvalidKey);
    }

    Ok((
        meta_blob,
        KeyRing {
            kek,
            data_key,
            subkeys,
        },
    ))
}

fn validate_names(namespace: &str, collection: &str) -> Result<()> {
    validate_name(NameKind::Namespace, namespace)?;
    validate_name(NameKind::Collection, collection)?;
    Ok(())
}

fn record_meta(id: RecordId, header: &RecordHeader, ciphertext_len: usize) -> Result<RecordMeta> {
    let ciphertext_len =
        u32::try_from(ciphertext_len).map_err(|_| Error::Internal("ciphertext length overflow"))?;
    Ok(RecordMeta {
        id,
        rev: header.rev,
        created_at: Timestamp::from_millis(header.created_at),
        updated_at: Timestamp::from_millis(header.updated_at),
        data_version: header.data_version,
        deleted: header.is_deleted(),
        ciphertext_len,
    })
}
