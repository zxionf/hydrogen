//! shio 的 C ABI 边界。
//!
//! 目标读者是 C++/Vulkan GUI 与 Android：句柄不透明、字节串显式带长度、错误用
//! 稳定错误码加线程局部详情。契约见 `.design/api.md` §8。
//!
//! 当前不提供任何导出符号：C ABI 的实现必然引入 `unsafe`，而项目契约禁止
//! `unsafe`，需要用户显式批准例外后才可动工。本 crate 现在只固定依赖方向，
//! 确保 facade 可以独立演进而无需回头调整 workspace 结构。

#![forbid(unsafe_code)]
