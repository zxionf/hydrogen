//! redb 表定义与引擎。
//!
//! 表名是落盘契约（见 `.design/data.md` §3、§15）：改名等于改格式。
//! `format=1` 与 `format=2` 的记录键、catalog 表保持不变；`format=2` 新增
//! `shio.keys2`、`shio.changes`、`shio.sync`。

pub mod catalog;
pub mod changes;
pub mod engine;
pub mod keys;
pub mod keys2;
pub mod meta;
pub mod meta2;
pub mod records;
pub mod sync_state;

pub use engine::{Engine, EngineOptions, RawWriteOp, Stats};

/// 当前默认写出的数据库格式版本。
pub const FORMAT: u16 = 1;

/// 同步库的主格式版本。
pub const FORMAT_V2: u16 = 2;

/// `shio.meta` 表的固定键。
pub const META_KEY: &str = "meta";
