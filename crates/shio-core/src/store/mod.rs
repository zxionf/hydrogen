//! redb 表定义。
//!
//! 表名是 `format=1` 的契约（见 `.design/data.md` §3）：改名等于改格式。
//! 本模块只声明表结构，读写事务与 blob 编解码尚未接线。

pub mod catalog;
pub mod keys;
pub mod meta;
pub mod records;

/// 当前数据库格式版本。
pub const FORMAT: u16 = 1;

/// `shio.meta` 表的固定键。
pub const META_KEY: &str = "meta";
