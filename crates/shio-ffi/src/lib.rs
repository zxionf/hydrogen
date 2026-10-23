//! shio 的 C ABI 边界。
//!
//! 目标读者是 C++/Vulkan GUI 与 Android：句柄不透明、字节串显式带长度、错误用
//! 稳定错误码加线程局部详情。契约见 `.design/api.md` §8 与 `include/shio.h`。
//!
//! 本 crate 的 unsafe 只允许出现在 [`ffi`] 模块；核心、facade 与同步层仍保持
//! `forbid(unsafe_code)`。所有导出函数都经过 `catch_unwind`，panic 不跨 C ABI。

#![deny(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

#[allow(unsafe_code)]
pub mod ffi;

pub use ffi::*;
