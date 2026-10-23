//! 内核句柄与打开参数。
//!
//! `Kernel` 是廉价可克隆的句柄，内部共享同一份引擎状态；`Namespace` 与
//! `Collection<T>` 由它派生，不复制连接，也不缓存数据。
//!
//! 所有触及存储的调用都经 `spawn_blocking` 搬到阻塞线程池；句柄关闭后新操作
//! 返回 [`Error::Closed`]，关闭前已开始的操作会自然完成。

use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
#[cfg(feature = "sync")]
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use shio_core::model::{validate_name, NameKind, DEFAULT_MAX_VALUE_SIZE};
use shio_core::store::{Engine, EngineOptions, Stats};
#[cfg(feature = "sync")]
use shio_sync::{PairingOffer, Sync as SyncApi, SyncHandle, SyncInfo};
use tokio::sync::Notify;
use zeroize::Zeroizing;

use crate::collection::Collection;
use crate::lock::DataFileLock;
use crate::{Error, Result};

/// 打开数据库的参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenOptions {
    /// 数据库文件路径。
    pub path: PathBuf,
    /// 文件不存在时是否创建，默认 `false`。
    pub create: bool,
    /// 只读打开，默认 `false`。
    pub read_only: bool,
    /// 单条记录体上限，默认 16 MiB。
    pub max_value_size: usize,
    /// redb 页缓存大小；`None` 表示使用引擎默认值。
    pub cache_size: Option<usize>,
}

impl OpenOptions {
    /// 以路径构造，其余取默认值。
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            create: false,
            read_only: false,
            max_value_size: DEFAULT_MAX_VALUE_SIZE,
            cache_size: None,
        }
    }

    /// 文件不存在时是否创建。
    pub fn create(mut self, yes: bool) -> Self {
        self.create = yes;
        self
    }

    /// 是否只读打开。
    ///
    /// 只读句柄仍需要口令解锁，但所有写操作返回 [`Error::ReadOnly`]。
    pub fn read_only(mut self, yes: bool) -> Self {
        self.read_only = yes;
        self
    }

    /// 单条记录体上限。
    pub fn max_value_size(mut self, bytes: usize) -> Self {
        self.max_value_size = bytes;
        self
    }

    /// redb 页缓存大小。
    pub fn cache_size(mut self, bytes: usize) -> Self {
        self.cache_size = Some(bytes);
        self
    }

    /// 校验参数组合。
    pub fn validate(&self) -> Result<()> {
        if self.path.as_os_str().is_empty() {
            return Err(Error::InvalidArgument("database path must not be empty"));
        }
        if self.read_only && self.create {
            return Err(Error::InvalidArgument(
                "read_only cannot be combined with create",
            ));
        }
        if self.max_value_size == 0 {
            return Err(Error::InvalidArgument("max_value_size must be positive"));
        }
        if self.cache_size == Some(0) {
            return Err(Error::InvalidArgument("cache_size must be positive"));
        }
        Ok(())
    }
}

/// 解锁凭据。
///
/// 枚举为未来恢复码、系统钥匙串等方式留位；当前只有口令。
#[derive(Clone)]
pub enum Credential {
    /// 口令解锁。
    Password(SecretString),
}

impl Credential {
    /// 以口令构造凭据。
    pub fn password(value: impl Into<SecretString>) -> Self {
        Self::Password(value.into())
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credential::Password(<redacted>)")
    }
}

/// 内核句柄。
///
/// `Clone + Send + Sync`，克隆只增加引用计数。
#[derive(Clone)]
pub struct Kernel {
    inner: Arc<Inner>,
}

/// 句柄共享的内核状态。
pub(crate) struct Inner {
    state: Mutex<State>,
    idle: Notify,
    read_only: bool,
    data_lock: Mutex<Option<DataFileLock>>,
}

struct State {
    engine: Option<Arc<Engine>>,
    in_flight: usize,
}

fn lock_error() -> Error {
    Error::Internal("kernel state lock poisoned")
}

impl Inner {
    fn new(engine: Engine, read_only: bool, data_lock: DataFileLock) -> Self {
        Self {
            state: Mutex::new(State {
                engine: Some(Arc::new(engine)),
                in_flight: 0,
            }),
            idle: Notify::new(),
            read_only,
            data_lock: Mutex::new(Some(data_lock)),
        }
    }

    /// 取引擎但不登记在途操作（只用于 `stats` 这类瞬时调用）。
    fn engine(&self) -> Result<Arc<Engine>> {
        let state = self.state.lock().map_err(|_| lock_error())?;
        state.engine.clone().ok_or(Error::Closed)
    }

    fn begin(&self) -> Result<Arc<Engine>> {
        let mut state = self.state.lock().map_err(|_| lock_error())?;
        let engine = state.engine.clone().ok_or(Error::Closed)?;
        state.in_flight += 1;
        Ok(engine)
    }

    fn begin_write(&self) -> Result<Arc<Engine>> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        self.begin()
    }

    fn finish(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.in_flight = state.in_flight.saturating_sub(1);
            if state.in_flight == 0 {
                self.idle.notify_one();
            }
        }
    }

    async fn dispatch<T, F>(&self, engine: Result<Arc<Engine>>, op: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Engine) -> Result<T> + Send + 'static,
    {
        let engine = engine?;
        let joined = tokio::task::spawn_blocking(move || op(&engine)).await;
        self.finish();
        joined.map_err(|_| Error::Internal("blocking task panicked"))?
    }

    pub(crate) async fn run<T, F>(&self, op: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Engine) -> Result<T> + Send + 'static,
    {
        self.dispatch(self.begin(), op).await
    }

    pub(crate) async fn run_write<T, F>(&self, op: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Engine) -> Result<T> + Send + 'static,
    {
        self.dispatch(self.begin_write(), op).await
    }

    async fn close(&self) -> Result<()> {
        loop {
            let pending = {
                let mut state = self.state.lock().map_err(|_| lock_error())?;
                state.engine = None;
                state.in_flight > 0
            };

            if !pending {
                if let Ok(mut data_lock) = self.data_lock.lock() {
                    data_lock.take();
                }
                return Ok(());
            }
            self.idle.notified().await;
        }
    }
}

impl fmt::Debug for Kernel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let closed = self.inner.engine().is_err();
        f.debug_struct("Kernel")
            .field("closed", &closed)
            .finish_non_exhaustive()
    }
}

fn engine_options(options: &OpenOptions) -> EngineOptions {
    let mut engine_options = EngineOptions::new(options.path.clone())
        .create(options.create)
        .read_only(options.read_only)
        .max_value_size(options.max_value_size);
    if let Some(bytes) = options.cache_size {
        engine_options = engine_options.cache_size(bytes);
    }
    engine_options
}

fn password_bytes(credential: Credential) -> Zeroizing<Vec<u8>> {
    match credential {
        Credential::Password(secret) => Zeroizing::new(secret.expose_secret().as_bytes().to_vec()),
    }
}

impl Kernel {
    /// 打开或创建数据库，并完成口令派生、DK 解包与校验。
    pub async fn open(options: OpenOptions, credential: Credential) -> Result<Self> {
        options.validate()?;
        let read_only = options.read_only;
        let data_lock = DataFileLock::acquire(&options.path, read_only)?;
        let engine_options = engine_options(&options);
        let password = password_bytes(credential);

        let engine =
            tokio::task::spawn_blocking(move || Engine::open(engine_options, password.as_slice()))
                .await
                .map_err(|_| Error::Internal("blocking task panicked"))??;

        Ok(Self {
            inner: Arc::new(Inner::new(engine, read_only, data_lock)),
        })
    }

    /// 取命名空间句柄；名称在此完成校验，不访问磁盘。
    pub fn namespace(&self, name: &str) -> Result<Namespace> {
        validate_name(NameKind::Namespace, name)?;
        Ok(Namespace {
            inner: Arc::clone(&self.inner),
            name: name.to_owned(),
        })
    }

    /// 持久化屏障；redb 每次提交即落盘。
    pub async fn flush(&self) -> Result<()> {
        self.inner.run(|engine| engine.flush()).await
    }

    /// 关闭句柄并等待进行中的操作结束。
    ///
    /// 重复调用返回 `Ok(())`；关闭后任何新操作返回 `Error::Closed`。
    pub async fn close(&self) -> Result<()> {
        self.inner.close().await
    }

    /// 当前库的统计信息；同步调用，不进入阻塞线程池。
    pub fn stats(&self) -> Result<Stats> {
        self.inner.engine()?.stats()
    }

    /// 同步摘要；未启用同步时返回 [`Error::SyncDisabled`]。
    #[cfg(feature = "sync")]
    pub fn sync_info(&self) -> Result<SyncInfo> {
        self.inner.engine()?.sync_info()
    }

    /// 取同步句柄，与当前内核共享同一引擎；句柄关闭后返回 [`Error::Closed`]。
    #[cfg(feature = "sync")]
    pub fn sync(&self) -> Result<SyncHandle> {
        Ok(SyncHandle::new(self.inner.engine()?))
    }

    /// 生成配对包；Argon2id 派生在阻塞线程池执行。
    #[cfg(feature = "sync")]
    pub async fn create_pairing_offer(&self, ttl: Duration) -> Result<PairingOffer> {
        self.inner
            .run(move |engine| SyncApi::new(engine).pairing_offer(ttl))
            .await
    }

    /// 改口令：用新口令重新包裹数据密钥，不重写记录。
    ///
    /// 只读句柄返回 [`Error::ReadOnly`]。
    pub async fn change_password(&self, new_password: SecretString) -> Result<()> {
        let password = Zeroizing::new(new_password.expose_secret().as_bytes().to_vec());
        self.inner
            .run_write(move |engine| engine.change_password(password.as_slice()))
            .await
    }

    /// 轮换数据密钥：生成新 `key_id` 并重加密全部记录。
    ///
    /// 只读句柄返回 [`Error::ReadOnly`]。
    pub async fn rotate_data_key(&self) -> Result<()> {
        self.inner
            .run_write(|engine| engine.rotate_data_key())
            .await
    }
}

/// 用配对包创建新库；目标必须不存在或为空。
///
/// `secret` 是配对 offer 中的 Base32 文本，调用方用 [`Zeroizing`] 搬运。
#[cfg(feature = "sync")]
pub async fn open_with_pairing(
    options: OpenOptions,
    credential: Credential,
    offer: &PairingOffer,
    secret: Zeroizing<String>,
) -> Result<Kernel> {
    options.validate()?;
    let data_lock = DataFileLock::acquire(&options.path, false)?;
    let engine_options = engine_options(&options);
    let password = password_bytes(credential);
    let offer = offer.clone();

    let engine = tokio::task::spawn_blocking(move || {
        let secret = shio_sync::decode_pairing_secret(&secret)?;
        SyncApi::accept_pairing(engine_options, password.as_slice(), &offer, &secret)
    })
    .await
    .map_err(|_| Error::Internal("blocking task panicked"))??;

    Ok(Kernel {
        inner: Arc::new(Inner::new(engine, false, data_lock)),
    })
}

/// 命名空间句柄。
#[derive(Clone)]
pub struct Namespace {
    pub(crate) inner: Arc<Inner>,
    name: String,
}

impl Namespace {
    /// 命名空间名称。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 取集合句柄；名称在此完成校验，不访问磁盘。
    pub fn collection<T>(&self, name: &str) -> Result<Collection<T>>
    where
        T: serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
    {
        validate_name(NameKind::Collection, name)?;
        Ok(Collection::new(
            Arc::clone(&self.inner),
            self.name.clone(),
            name.to_owned(),
        ))
    }

    /// 列出本命名空间下已登记的集合名，按名称升序。
    pub async fn collections(&self) -> Result<Vec<String>> {
        let name = self.name.clone();
        self.inner
            .run(move |engine| engine.collections(&name))
            .await
    }
}

impl fmt::Debug for Namespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Namespace")
            .field("name", &self.name)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_options_defaults_and_validate() {
        let options = OpenOptions::new("./data/shio.redb");
        assert!(!options.create);
        assert!(!options.read_only);
        assert_eq!(options.max_value_size, DEFAULT_MAX_VALUE_SIZE);
        options.validate().unwrap();

        let chained = OpenOptions::new("./data/shio.redb")
            .create(true)
            .max_value_size(1024)
            .cache_size(4096);
        assert!(chained.create);
        assert_eq!(chained.max_value_size, 1024);
        assert_eq!(chained.cache_size, Some(4096));
        chained.validate().unwrap();
    }

    #[test]
    fn open_options_reject_bad_combinations() {
        assert!(OpenOptions::new("").validate().is_err());
        assert!(OpenOptions::new("x")
            .create(true)
            .read_only(true)
            .validate()
            .is_err());
        assert!(OpenOptions::new("x").max_value_size(0).validate().is_err());
        assert!(OpenOptions::new("x").cache_size(0).validate().is_err());
    }

    #[test]
    fn credential_debug_is_redacted() {
        let credential = Credential::password("correct horse battery staple");
        let text = format!("{credential:?}");
        assert_eq!(text, "Credential::Password(<redacted>)");
        assert!(!text.contains("horse"));
    }
}
