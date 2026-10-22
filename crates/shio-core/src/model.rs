//! 数据模型：名称约束、记录标识、时间戳与读写参数。
//!
//! 这一层是纯逻辑，不触碰磁盘，也不依赖加密材料，因此可以独立测试。
//! 字节层表示见 [`crate::codec`]。

use std::fmt;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use uuid::Uuid;

use crate::error::{Error, Result};

/// 名称（命名空间或集合）的最大字节数。
pub const MAX_NAME_LEN: usize = 64;

/// 列表分页的默认条数。
pub const DEFAULT_LIST_LIMIT: usize = 100;

/// 列表分页允许的最大条数。
pub const MAX_LIST_LIMIT: usize = 1000;

/// 单条记录体的默认上限：16 MiB。
pub const DEFAULT_MAX_VALUE_SIZE: usize = 16 * 1024 * 1024;

/// 名称所属的层。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NameKind {
    /// 命名空间：应用域隔离。
    Namespace,
    /// 集合：域内的记录类型。
    Collection,
}

impl NameKind {
    /// 稳定的字面量表示，用于日志与错误信息。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Namespace => "namespace",
            Self::Collection => "collection",
        }
    }
}

impl fmt::Display for NameKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 校验名称是否合法。
///
/// 约束（见 `SPEC.md` 数据模型）：1–64 字节、字符集 `[a-z0-9._-]`、不得以 `.` 开头。
/// 输入按 UTF-8 字节长度计算，非 ASCII 字符一律拒绝。
pub fn validate_name(kind: NameKind, value: &str) -> Result<()> {
    let invalid = || Error::InvalidName {
        kind,
        value: value.to_owned(),
    };

    if value.is_empty() || value.len() > MAX_NAME_LEN {
        return Err(invalid());
    }
    if value.starts_with('.') {
        return Err(invalid());
    }

    let allowed = value
        .bytes()
        .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'));
    if !allowed {
        return Err(invalid());
    }

    Ok(())
}

/// `format=2` 的数据密钥编号：16 字节随机值。
///
/// `format=1` 的 u16 编号通过 [`KeyId::from_u16`] 映射为「前 14 字节 0 + 后 2 字节
/// 大端」，因此两种格式在公开面上使用同一个类型。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyId([u8; 16]);

impl KeyId {
    /// 由 16 字节原始值构造。全零值保留，表示未设置。
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// 原始 16 字节。
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// `format=1` 的 u16 编号映射。
    pub const fn from_u16(value: u16) -> Self {
        let bytes = value.to_be_bytes();
        let mut out = [0u8; 16];
        out[14] = bytes[0];
        out[15] = bytes[1];
        Self(out)
    }

    /// 若该编号是 `format=1` 映射，返回原始 u16。
    pub const fn as_u16(&self) -> Option<u16> {
        let mut index = 0;
        while index < 14 {
            if self.0[index] != 0 {
                return None;
            }
            index += 1;
        }
        Some(u16::from_be_bytes([self.0[14], self.0[15]]))
    }

    /// 是否为全零值。
    pub const fn is_zero(&self) -> bool {
        let mut index = 0;
        while index < self.0.len() {
            if self.0[index] != 0 {
                return false;
            }
            index += 1;
        }
        true
    }
}

impl fmt::Debug for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyId(")?;
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        f.write_str(")")
    }
}

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl PartialEq<u16> for KeyId {
    fn eq(&self, other: &u16) -> bool {
        self.as_u16() == Some(*other)
    }
}

/// 设备标识：首次启用同步时生成，之后不可更换。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId([u8; 16]);

impl DeviceId {
    /// 由 16 字节原始值构造。
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// 原始 16 字节。
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// 是否为全零值。
    pub const fn is_zero(&self) -> bool {
        let mut index = 0;
        while index < self.0.len() {
            if self.0[index] != 0 {
                return false;
            }
            index += 1;
        }
        true
    }
}

impl fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceId(")?;
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        f.write_str(")")
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// 记录 ID：UUIDv7 的 16 字节原始值。
///
/// 时间有序，跨端无需协调即可生成且不冲突。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RecordId(Uuid);

impl RecordId {
    /// 生成新的 UUIDv7。
    pub fn new_v7() -> Self {
        Self(Uuid::now_v7())
    }

    /// 由 16 字节原始值构造，不校验版本位。
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(Uuid::from_bytes(bytes))
    }

    /// 取 16 字节原始值。
    pub const fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }

    /// 取内部 UUID，供需要格式化或比较版本位的地方使用。
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl fmt::Display for RecordId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl FromStr for RecordId {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        Uuid::parse_str(value)
            .map(Self)
            .map_err(|_| Error::InvalidArgument("record id is not a valid UUID"))
    }
}

/// Unix 毫秒时间戳（UTC）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(i64);

impl Timestamp {
    /// 由毫秒数构造。
    pub const fn from_millis(millis: i64) -> Self {
        Self(millis)
    }

    /// 取当前时间；早于 Unix 纪元的时钟返回 0，不回绕也不 panic。
    pub fn now() -> Self {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
            .unwrap_or(0);
        Self(millis)
    }

    /// 毫秒数。
    pub const fn as_millis(self) -> i64 {
        self.0
    }
}

/// 记录的明文元信息，列表操作只读它，不解密。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordMeta {
    /// 记录 ID。
    pub id: RecordId,
    /// 修订号，从 1 开始单调递增。
    pub rev: u64,
    /// 首次写入时间。
    pub created_at: Timestamp,
    /// 最近一次写入时间。
    pub updated_at: Timestamp,
    /// 载荷版本，由调用方解释。
    pub data_version: u16,
    /// 是否为墓碑。
    pub deleted: bool,
    /// 密文字节数（含 16 字节 tag）。
    pub ciphertext_len: u32,
}

/// 解密后的记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored<T> {
    /// 明文元信息。
    pub meta: RecordMeta,
    /// 反序列化后的载荷。
    pub value: T,
}

/// 列表分页参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListOptions {
    /// 游标：上一页最后一个 ID；`None` 表示从头开始。
    pub after: Option<RecordId>,
    /// 本页最多返回条数，默认 [`DEFAULT_LIST_LIMIT`]，上限 [`MAX_LIST_LIMIT`]。
    pub limit: usize,
    /// 是否包含墓碑。
    pub include_deleted: bool,
}

impl Default for ListOptions {
    fn default() -> Self {
        Self {
            after: None,
            limit: DEFAULT_LIST_LIMIT,
            include_deleted: false,
        }
    }
}

impl ListOptions {
    /// 设置分页条数；非法值在 [`ListOptions::validate`] 时报错。
    pub const fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// 设置游标。
    pub const fn after(mut self, id: RecordId) -> Self {
        self.after = Some(id);
        self
    }

    /// 是否包含墓碑。
    pub const fn include_deleted(mut self, yes: bool) -> Self {
        self.include_deleted = yes;
        self
    }

    /// 校验取值范围：`limit` 必须在 `1..=1000`。
    pub fn validate(&self) -> Result<()> {
        if self.limit == 0 || self.limit > MAX_LIST_LIMIT {
            return Err(Error::InvalidArgument("list limit must be within 1..=1000"));
        }
        Ok(())
    }
}

/// 一页结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    /// 本页条目。
    pub items: Vec<T>,
    /// 下一页游标；`None` 表示没有更多。
    pub next: Option<RecordId>,
}

/// 批量写操作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOp<T> {
    /// 指定 ID 的 upsert。
    Put {
        /// 目标记录 ID。
        id: RecordId,
        /// 载荷。
        value: T,
    },
    /// 写入墓碑。
    Delete {
        /// 目标记录 ID。
        id: RecordId,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_names() {
        for name in ["notes", "a", "my.coll-1", "a_b.c-d9"] {
            validate_name(NameKind::Collection, name).unwrap();
        }
    }

    #[test]
    fn rejects_invalid_names() {
        let long = "a".repeat(MAX_NAME_LEN + 1);
        for name in ["", ".hidden", "Upper", "with space", "中文", "a/b", &long] {
            assert!(
                validate_name(NameKind::Namespace, name).is_err(),
                "{name:?}"
            );
        }
        let allowed = "a".repeat(MAX_NAME_LEN);
        assert!(validate_name(NameKind::Namespace, &allowed).is_ok());
    }

    #[test]
    fn record_id_roundtrips() {
        let id = RecordId::new_v7();
        let bytes = *id.as_bytes();
        let restored = RecordId::from_bytes(bytes);
        assert_eq!(id, restored);

        let text = id.to_string();
        assert_eq!(text.parse::<RecordId>().unwrap(), id);
        assert!(id.as_uuid().get_version_num() == 7);
    }

    #[test]
    fn record_id_rejects_bad_text() {
        assert!(matches!(
            "not-a-uuid".parse::<RecordId>(),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn timestamp_now_is_positive() {
        assert!(Timestamp::now().as_millis() > 0);
    }

    #[test]
    fn key_id_maps_legacy_u16() {
        let legacy = KeyId::from_u16(0x1234);
        assert_eq!(legacy.as_u16(), Some(0x1234));
        assert_eq!(legacy.to_string(), "00000000000000000000000000001234");
        assert!(!legacy.is_zero());
        assert!(KeyId::from_bytes([0; 16]).is_zero());
        assert_eq!(KeyId::from_bytes([1; 16]).as_u16(), None);
    }

    #[test]
    fn device_id_display_is_hex() {
        let device = DeviceId::from_bytes([0xAB; 16]);
        assert_eq!(device.to_string(), "ab".repeat(16));
        assert!(format!("{device:?}").starts_with("DeviceId(ab"));
    }

    #[test]
    fn list_options_limits() {
        let options = ListOptions::default();
        assert_eq!(options.limit, DEFAULT_LIST_LIMIT);
        options.validate().unwrap();

        assert!(ListOptions::default().with_limit(0).validate().is_err());
        assert!(ListOptions::default()
            .with_limit(MAX_LIST_LIMIT)
            .validate()
            .is_ok());
        assert!(ListOptions::default()
            .with_limit(MAX_LIST_LIMIT + 1)
            .validate()
            .is_err());
    }
}
