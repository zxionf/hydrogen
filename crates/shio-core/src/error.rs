//! 内核错误分类。
//!
//! 错误分类是公共契约：调用方依赖它做重试与提示，因此新增变体需要追加决策。
//! 错误信息不得包含口令、密钥或明文。

use crate::model::{KeyId, NameKind};

/// 内核统一错误类型。
///
/// 标注 `#[non_exhaustive]`：新增变体不算破坏性变更，但调用方必须保留兜底分支。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// 参数组合或取值范围非法。
    #[error("invalid argument: {0}")]
    InvalidArgument(&'static str),

    /// 命名空间或集合名称不符合约束。
    #[error("invalid {kind} name: {value:?}")]
    InvalidName {
        /// 出错的名称类别。
        kind: NameKind,
        /// 原始输入，便于提示；名称不是秘密。
        value: String,
    },

    /// 口令错误或密钥材料损坏，两者不区分，避免口令预言机。
    #[error("invalid key or corrupted key material")]
    InvalidKey,

    /// 数据库不存在，或指定记录不存在。
    #[error("not found")]
    NotFound,

    /// 句柄已关闭。
    #[error("kernel is closed")]
    Closed,

    /// 以只读方式打开后尝试写入。
    #[error("read-only kernel")]
    ReadOnly,

    /// 密钥轮换尚未完成（存在多行 key），需先完成轮换。
    #[error("key rotation in progress")]
    Busy,

    /// 操作在批次边界被调用方取消。
    #[error("operation cancelled")]
    Cancelled,

    /// 未启用多端同步。
    #[error("sync is not enabled")]
    SyncDisabled,

    /// 同步模式下禁止物理删除。
    #[error("purge is disabled while sync is enabled")]
    SyncEnabled,

    /// 游标早于日志起点，需要走快照。
    #[error("sync log was compacted; resync from {log_start}")]
    SyncResyncRequired {
        /// 当前保留的最早日志序号。
        log_start: u64,
    },

    /// 缺少记录引用的数据密钥，需要先安装轮换 offer。
    #[error("data key {key_id} is required before applying this page")]
    RotationRequired {
        /// 缺失的 key。
        key_id: KeyId,
    },

    /// 配对包已过期。
    #[error("pairing offer has expired")]
    PairingExpired,

    /// 配对包无效或认证失败。
    #[error("pairing offer is invalid")]
    PairingInvalid,

    /// 文件锁被其他句柄或进程占用。
    #[error("database is locked by another handle or process")]
    Locked,

    /// 记录体超过配置上限。
    #[error("value too large: {size} bytes (max {max})")]
    ValueTooLarge {
        /// 实际大小。
        size: usize,
        /// 允许上限。
        max: usize,
    },

    /// 落盘字节结构非法或认证失败。
    ///
    /// 认证失败也归入此处，避免向调用方泄露「哪一段被改过」。
    #[error("corrupted data: {0}")]
    Corrupted(&'static str),

    /// 未知或不支持的格式版本。
    #[error("unsupported format version {found} (supported {supported})")]
    UnsupportedFormat {
        /// 读到的版本。
        found: u16,
        /// 当前支持的版本。
        supported: u16,
    },

    /// JSON 编码失败。
    #[error("serialize error: {0}")]
    Serialize(serde_json::Error),

    /// JSON 解码失败。
    #[error("deserialize error: {0}")]
    Deserialize(serde_json::Error),

    /// 文件系统错误。
    #[error("io error: {0}")]
    Io(std::io::Error),

    /// 存储引擎错误，只保留文本，不向公共 API 暴露依赖类型。
    #[error("database error: {0}")]
    Database(String),

    /// 内部不变式被破坏，属于代码缺陷。
    #[error("internal error: {0}")]
    Internal(&'static str),
}

/// 内核统一 `Result`，默认错误类型为 [`Error`]。
pub type Result<T, E = Error> = core::result::Result<T, E>;

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<redb::Error> for Error {
    fn from(value: redb::Error) -> Self {
        Self::Database(value.to_string())
    }
}

impl From<redb::DatabaseError> for Error {
    fn from(value: redb::DatabaseError) -> Self {
        match value {
            redb::DatabaseError::DatabaseAlreadyOpen => Self::Locked,
            other => Self::Database(other.to_string()),
        }
    }
}

impl From<redb::CommitError> for Error {
    fn from(value: redb::CommitError) -> Self {
        Self::Database(value.to_string())
    }
}

impl From<redb::CompactionError> for Error {
    fn from(value: redb::CompactionError) -> Self {
        match value {
            redb::CompactionError::PersistentSavepointExists
            | redb::CompactionError::EphemeralSavepointExists
            | redb::CompactionError::TransactionInProgress => Self::Busy,
            redb::CompactionError::Storage(storage) => Self::Database(storage.to_string()),
            other => Self::Database(other.to_string()),
        }
    }
}

impl From<redb::TransactionError> for Error {
    fn from(value: redb::TransactionError) -> Self {
        Self::Database(value.to_string())
    }
}

impl From<redb::TableError> for Error {
    fn from(value: redb::TableError) -> Self {
        Self::Database(value.to_string())
    }
}

impl From<redb::StorageError> for Error {
    fn from(value: redb::StorageError) -> Self {
        Self::Database(value.to_string())
    }
}
