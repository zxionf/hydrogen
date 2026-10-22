//! 内核句柄与打开参数。
//!
//! `Kernel` 是廉价可克隆的句柄，内部共享同一份引擎状态；`Namespace` 与
//! `Collection<T>` 由它派生，不复制连接，也不缓存数据。

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use secrecy::SecretString;
use shio_core::model::{validate_name, NameKind, DEFAULT_MAX_VALUE_SIZE};

use crate::collection::Collection;
use crate::Error;

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

    /// 校验参数组合；真正的打开在存储层接线后进行。
    pub fn validate(&self) -> Result<(), Error> {
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
    pub fn password(value: impl Into<String>) -> Self {
        let password: String = value.into();
        Self::Password(SecretString::from(password))
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
///
/// 存储层接线后填入 redb `Database` 与派生密钥；本轮保持空壳，因此不提供构造入口。
#[allow(dead_code)]
pub(crate) struct Inner {}

impl Kernel {
    /// 取命名空间句柄；名称在此完成校验，不访问磁盘。
    pub fn namespace(&self, name: &str) -> Result<Namespace, Error> {
        validate_name(NameKind::Namespace, name)?;
        Ok(Namespace {
            inner: Arc::clone(&self.inner),
            name: name.to_owned(),
        })
    }
}

/// 命名空间句柄。
#[derive(Clone)]
pub struct Namespace {
    #[allow(dead_code)]
    pub(crate) inner: Arc<Inner>,
    name: String,
}

impl Namespace {
    /// 命名空间名称。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 取集合句柄；名称在此完成校验，不访问磁盘。
    pub fn collection<T>(&self, name: &str) -> Result<Collection<T>, Error>
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
