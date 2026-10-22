//! shio 的 async facade。
//!
//! 这一层负责三件事：管理内核生命周期、把同步调用搬到阻塞线程池、向 Rust 应用
//! 暴露 `Kernel` / `Namespace` / `Collection<T>` 句柄。当前阶段只定型：
//!
//! - 参数与名称校验是完整实现；
//! - 触及存储与密钥的方法（`open`、读写、`close`）在存储层接线后补齐，
//!   签名以 `.design/api.md` 为准。
//!
//! 本 crate 禁止 `unsafe`，也不直接实现密码学或存储逻辑。

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod collection;
pub mod kernel;

pub use collection::Collection;
pub use kernel::{Credential, Kernel, Namespace, OpenOptions};

pub use shio_core::error::{Error, Result};
pub use shio_core::model::{
    validate_name, ListOptions, NameKind, Page, RecordId, RecordMeta, Stored, Timestamp, WriteOp,
    DEFAULT_LIST_LIMIT, DEFAULT_MAX_VALUE_SIZE, MAX_LIST_LIMIT, MAX_NAME_LEN,
};
