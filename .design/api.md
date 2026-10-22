# shio API

<!-- 公共 API 的稳定契约。实现可演进；改变签名、语义或错误分类前，先追加 DECISIONS.log。 -->

本文描述**当前有效**的 API 契约；已定但尚未实现的同步接口集中在第 12 节，单独标注，不构成可用 API。实现细节（表、字节、AAD）见 `data.md`；定位与取舍见 `changes/20261020-encrypted-persistence-core/design.md` 与 `changes/20261022-sync-design/design.md`。

## 1. 范围与稳定性

- 目标平台：Rust 应用（原生）、Android、C++/Vulkan GUI（经 C ABI）。
- 近期形态：嵌入式库。同一内核可被服务端直接链接，不引入 daemon 协议。
- 版本策略：1.0 前 API 允许调整；记录格式 `format=1` 从首版起保持向后兼容。
- 线程模型：对外 async（tokio），内核同步；所有阻塞工作离开 executor。

## 2. crate 与模块

| crate | 职责 | 允许依赖 |
|---|---|---|
| `shio-core` | 模型、ID、加密、redb 存储、事务 | redb、serde、serde_json、argon2、chacha20poly1305、hkdf、zeroize、uuid、getrandom、thiserror |
| `shio` | async facade、生命周期、句柄 | `shio-core`、tokio、secrecy、serde、serde_json、zeroize；可选 `shio-sync`（`sync` feature） |
| `shio-ffi` | C ABI（后续） | `shio`、libc；唯一允许 unsafe 的 crate |
| `shio-sync` | 多端同步（后续） | `shio-core` |

`shio-core` 与 `shio` 均声明 `#![forbid(unsafe_code)]`。

## 3. 生命周期

```rust
use shio::{Credential, Kernel, OpenOptions};

let kernel = Kernel::open(
    OpenOptions::new("./data/shio.redb").create(true),
    Credential::password("correct horse battery staple"),
).await?;

// ... 读写 ...

kernel.close().await?;
```

- `open`：打开或创建数据库，完成口令派生、DK 解包与校验。`create=false` 且文件不存在时返回 `Error::NotFound`。
- `close`：标记关闭、等待进行中的操作结束、释放文件锁。之后任何操作返回 `Error::Closed`。
- 同一进程内可克隆 `Kernel` 句柄共享；跨进程由 redb 文件锁保证单写，冲突时返回 `Error::Locked`。
- `open` 是唯一需要口令的入口；解锁后的密钥材料只驻留内存。
- `shio::version()` 返回编译期内核版本（与包版本一致），不需要打开数据库；库的格式版本用 `Kernel::stats().format_version` 查询。

## 4. 核心类型

### 4.1 `OpenOptions`

```rust
pub struct OpenOptions {
    pub path: PathBuf,
    pub create: bool,          // 默认 false
    pub read_only: bool,       // 默认 false
    pub max_value_size: usize, // 默认 16 MiB
    pub cache_size: Option<usize>, // redb 页缓存，None = 引擎默认
}

impl OpenOptions {
    pub fn new(path: impl Into<PathBuf>) -> Self;
    pub fn create(self, yes: bool) -> Self;
    pub fn read_only(self, yes: bool) -> Self;
    pub fn max_value_size(self, bytes: usize) -> Self;
    pub fn cache_size(self, bytes: usize) -> Self;
}
```

`read_only=true` 时所有写操作返回 `Error::ReadOnly`，且仍需口令解密；同时指定 `create=true` 返回 `Error::InvalidArgument`。

### 4.2 `Credential`

```rust
pub enum Credential {
    Password(SecretString),
}

impl Credential {
    pub fn password(value: impl Into<SecretString>) -> Self;
}
```

枚举为未来恢复码、系统钥匙串等解锁方式留位；v0.1 只实现 `Password`。

### 4.3 `Kernel`

```rust
#[derive(Clone)]
pub struct Kernel { /* Arc<Inner> */ }

impl Kernel {
    pub async fn open(options: OpenOptions, credential: Credential) -> Result<Kernel>;

    pub fn namespace(&self, name: &str) -> Result<Namespace>;

    pub async fn flush(&self) -> Result<()>;

    pub async fn close(&self) -> Result<()>;

    /// 只重新包裹 DK，O(1)，不重写记录；轮换未完成（多行 key）时返回 Busy。
    pub async fn change_password(&self, new_password: SecretString) -> Result<()>;

    /// 生成新 DK，重加密全部记录，完成后淘汰旧 key。
    pub async fn rotate_data_key(&self) -> Result<()>;

    pub fn stats(&self) -> Result<Stats>;
}

pub struct Stats {
    pub format_version: u16,
    pub key_id: u16,
    pub db_bytes: u64,
}
```

`Kernel` 为 `Clone + Send + Sync + 'static`。`flush` 在默认持久化模式下是屏障语义；redb 每次提交即持久化，不依赖进程退出。

`Stats.key_id` 当前是 `format=1` 的 u16。同步库进入 `format=2` 后，计划改为 16 字节 `KeyId`（见第 12 节），这是 1.0 前允许的破坏性调整。

### 4.4 `Namespace`

```rust
#[derive(Clone)]
pub struct Namespace { /* Arc<Inner> + name */ }

impl Namespace {
    pub fn name(&self) -> &str;

    pub fn collection<T>(&self, name: &str) -> Result<Collection<T>>
    where
        T: Serialize + DeserializeOwned + Send + Sync + 'static;

    pub async fn collections(&self) -> Result<Vec<String>>;
}
```

- `collection` 是轻量构造，不访问磁盘；集合在首次写入时登记到 `catalog`。
- `collections` 返回按名称升序的集合名；列表只扫描 catalog，不触碰记录。
- 名称校验在构造时完成，非法名称返回 `Error::InvalidName`。

### 4.5 `Collection<T>`

```rust
#[derive(Clone)]
pub struct Collection<T> { /* Arc<Inner> + ns + coll + PhantomData<T> */ }

impl<T> Collection<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    pub fn namespace(&self) -> &str;
    pub fn name(&self) -> &str;

    /// 生成 UUIDv7 并插入。
    pub async fn insert(&self, value: &T) -> Result<(RecordId, RecordMeta)>;

    /// 指定 ID 的 upsert；墓碑记录会被复活，rev 递增。
    pub async fn put(&self, id: RecordId, value: &T) -> Result<RecordMeta>;

    pub async fn get(&self, id: &RecordId) -> Result<Option<Stored<T>>>;

    /// 写墓碑，返回是否存在。
    pub async fn delete(&self, id: &RecordId) -> Result<bool>;

    /// 物理删除，本地语义；多端同步启用后不得使用。
    pub async fn purge(&self, id: &RecordId) -> Result<bool>;

    pub async fn list(&self, options: ListOptions) -> Result<Page<RecordMeta>>;

    /// 原子批量写；任一失败则全部不生效。
    pub async fn batch(&self, ops: Vec<WriteOp<T>>) -> Result<Vec<RecordMeta>>;
}
```

`Collection<T>` 为 `Clone + Send + Sync`（当 `T: Send + Sync`）。`T` 不参与存储层寻址，可随时更换类型，但读取旧数据时反序列化失败会返回 `Error::Deserialize`。

### 4.6 记录类型

```rust
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct RecordId(Uuid);

impl RecordId {
    pub const fn new_v7() -> Self;
    pub const fn from_bytes(bytes: [u8; 16]) -> Self;
    pub const fn as_bytes(&self) -> &[u8; 16];
    pub const fn as_uuid(&self) -> &Uuid;
}

impl std::fmt::Display for RecordId;   // 标准 UUID 文本
impl std::str::FromStr for RecordId;   // 解析标准 UUID 文本

pub struct RecordMeta {
    pub id: RecordId,
    pub rev: u64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub data_version: u16,
    pub deleted: bool,
    pub ciphertext_len: u32,
}

pub struct Timestamp(i64); // Unix 毫秒，UTC

impl Timestamp {
    pub fn now() -> Self;
    pub const fn from_millis(millis: i64) -> Self;
    pub const fn as_millis(self) -> i64;
}

pub struct Stored<T> {
    pub meta: RecordMeta,
    pub value: T,
}
```

### 4.7 列表与批量

```rust
pub struct ListOptions {
    pub after: Option<RecordId>, // 游标：上一页最后一个 ID
    pub limit: usize,            // 默认 100，最大 1000
    pub include_deleted: bool,   // 默认 false
}

pub enum WriteOp<T> {
    Put { id: RecordId, value: T },
    Delete { id: RecordId },
}

pub struct Page<T> {
    pub items: Vec<T>,
    pub next: Option<RecordId>, // None 表示没有更多
}
```

`ListOptions` 实现 `Default`（`limit=100`）与链式构造：

```rust
impl ListOptions {
    pub const fn with_limit(self, limit: usize) -> Self;
    pub const fn after(self, id: RecordId) -> Self;
    pub const fn include_deleted(self, yes: bool) -> Self;
    pub fn validate(&self) -> Result<()>;
}
```

- v0.1 只保证按 `RecordId` 升序；UUIDv7 与写入时间近似同序。
- 按 `updated_at` 排序、前缀过滤、内容查询需要二级索引，属于后续变更。
- `limit=0` 或 `>1000` 返回 `Error::InvalidArgument`。

### 4.8 常量与名称校验

```rust
pub const MAX_NAME_LEN: usize = 64;
pub const DEFAULT_LIST_LIMIT: usize = 100;
pub const MAX_LIST_LIMIT: usize = 1000;
pub const DEFAULT_MAX_VALUE_SIZE: usize = 16 * 1024 * 1024;

pub fn validate_name(kind: NameKind, value: &str) -> Result<()>;

impl NameKind {
    pub const fn as_str(self) -> &'static str;
}
```

以上类型、常量与函数均由 `shio` 根模块重新导出，可直接 `use shio::{...}`。`NameKind` 同时实现 `Display`。

## 5. 操作语义

| 操作 | rev | created_at | updated_at | 备注 |
|---|---|---|---|---|
| `insert` | 1 | now | now | 生成 UUIDv7 |
| `put`（新） | 1 | now | now | 首次写入登记 catalog |
| `put`（覆盖） | +1 | 不变 | now | 整条替换，不做字段合并 |
| `delete`（存在） | +1 | 不变 | now | 写墓碑，保留密文 |
| `delete`（不存在） | — | — | — | 返回 `false`，不产生记录 |
| `delete`（已墓碑） | +1 | 不变 | now | 返回 `true`，刷新墓碑时间 |
| `purge` | — | — | — | 物理删除，返回是否存在 |

- 写入是整条替换：API 不做 patch/merge，避免读改写竞态。
- 写入成功后数据已持久化；进程崩溃不会留下半条记录。
- `batch` 的返回顺序与输入一致；`Delete` 项返回墓碑 meta。
- `get` 命中墓碑时返回 `Ok(None)`；管理/同步场景通过 `list(include_deleted=true)` 读取墓碑。
- 读操作不修改 `updated_at`。

## 6. 并发、取消与线程安全

- `Kernel`、`Namespace`、`Collection<T>` 均可跨任务克隆；内部共享同一引擎。
- 并发写由 redb 写锁串行化；并发读使用 MVCC 快照，不阻塞写。
- async 方法内部通过 `spawn_blocking` 调用 core；**取消 future 不会中止已开始的文件操作**，该操作可能仍会提交。调用方若需要“确定未执行”，应在取消后重新读取状态。
- `close` 与进行中的操作并发时：新操作立即 `Closed`，已开始的操作允许完成。
- `Collection<T>` 不缓存数据；不存在跨句柄的脏读。

### 6.1 稳定性保证

- core 的公开函数不以 panic 表达可恢复失败；内部不变式失败返回 `Error::Internal`。
- 落盘数据（header、键、长度、AAD、JSON）一律视为不可信，先校验后使用；损坏输入返回 `Corrupted` / `Deserialize`。
- 非测试代码不使用 `unwrap` / `expect` / `panic!`；CI 以 clippy 规则兜底。
- FFI 边界（未来）必须 `catch_unwind`，把 panic 转为错误码，禁止 panic 跨 C ABI。
- 发布构建不得依赖 `panic=abort` 来“避免”崩溃；正确性来自错误处理，而不是终止进程。

## 7. 错误

```rust
#[non_exhaustive]
pub enum Error {
    InvalidArgument(&'static str),
    InvalidName { kind: NameKind, value: String },
    InvalidKey,
    NotFound,
    Closed,
    ReadOnly,
    Busy,
    Locked,
    ValueTooLarge { size: usize, max: usize },
    Corrupted(&'static str),
    UnsupportedFormat { found: u16, supported: u16 },
    Serialize(serde_json::Error),
    Deserialize(serde_json::Error),
    Io(std::io::Error),
    Database(String), // redb 内部错误，保留文本，不暴露依赖类型
    Internal(&'static str),
}

pub enum NameKind { Namespace, Collection }
```

- `InvalidKey` 不区分口令错误与密钥损坏，避免口令预言机。
- `Busy` 表示密钥轮换未完成（存在多行 key），需先完成轮换；锁冲突仍用 `Locked`。
- `Corrupted` 表示结构或认证失败（AEAD tag 校验失败也归入此类，不泄露细节）。
- 错误实现 `std::error::Error` 与 `Display`；不实现 `Clone`。
- 可恢复错误不 panic；只有违反类型不变式（如内部状态机损坏）才允许 `debug_assert` 后返回 `Internal`。
- 所有来自落盘字节的输入先做长度/边界校验；失败返回 `Corrupted` 或 `Deserialize`，绝不 panic、越界或 abort。

## 8. C ABI 映射（设计，未实现）

C++ 与 Android 通过 `shio-ffi` 调用。原则：

- 句柄不透明：`typedef struct shio_kernel shio_kernel;`
- 所有字节串显式带长度；字符串参数为 UTF-8、非 NUL 结尾。
- 返回 `int32_t` 错误码；`0` 成功，负数对应稳定错误分类。
- 错误详情经 `shio_last_error(handle, buf, len)` 取回，线程局部。
- FFI 内部持有 tokio runtime，对外暴露**阻塞调用**；C++ 侧在 worker 线程调用，不阻塞 GUI 线程。
- 回调/取消令牌在后续版本追加，v0.1 不设计异步回调。

```c
int32_t shio_open(const char *path, const uint8_t *password, size_t password_len,
                  const char *options_json, shio_kernel **out);
void    shio_close(shio_kernel *kernel);

int32_t shio_put(shio_kernel *k, const char *ns, const char *coll,
                 const uint8_t id[16], const uint8_t *json, size_t json_len,
                 shio_meta *out);
int32_t shio_get(shio_kernel *k, const char *ns, const char *coll,
                 const uint8_t id[16], uint8_t **json, size_t *json_len);
int32_t shio_delete(shio_kernel *k, const char *ns, const char *coll,
                    const uint8_t id[16], int32_t *existed);

/* 释放由 shio_get / 列表接口返回的缓冲区 */
void    shio_free(uint8_t *ptr, size_t len);
```

> C ABI 需要 unsafe。当前 CONTRACT 禁止 unsafe，`shio-ffi` 的实施必须先取得显式例外；在此之前仅保留本节设计。

## 9. 兼容性

- 记录格式主版本 `format` 变化 = 不兼容；读取端拒绝未知版本。
- `format=1` 继续用于非同步库；同步库在 `enable_sync` 时升级到 `format=2`（见第 12 节），升级不可回退，旧二进制会拒绝打开。
- `data_version` 用于单个集合的载荷演进，由调用方解释，内核不强制。
- 新增字段只能追加在 header 尾部并提升 `format`；旧库由显式迁移升级，不静默改写。
- `key_id` 允许新旧密钥并存，是轮换与多设备密钥分发的基础；`format=1` 为 u16，`format=2` 为 16 字节 `KeyId`。
- `shio.changes`、`shio.sync`、`shio.keys2` 是同步库新增表，旧版本不可见；旧版本也不会写变更日志。

## 10. 示例

```rust
use serde::{Deserialize, Serialize};
use shio::{Credential, Kernel, ListOptions, OpenOptions};

#[derive(Serialize, Deserialize)]
struct Note {
    title: String,
    body: String,
    tags: Vec<String>,
}

#[tokio::main]
async fn main() -> shio::Result<()> {
    let kernel = Kernel::open(
        OpenOptions::new("./data/shio.redb").create(true),
        Credential::password(std::env::var("SHIO_PASSWORD").unwrap()),
    )
    .await?;

    let notes = kernel.namespace("notes")?.collection::<Note>("note")?;

    let (id, meta) = notes
        .insert(&Note {
            title: "第一篇".into(),
            body: "hello shio".into(),
            tags: vec!["demo".into()],
        })
        .await?;
    println!("inserted {id} rev={}", meta.rev);

    if let Some(stored) = notes.get(&id).await? {
        println!("{} / {}", stored.value.title, stored.value.body);
    }

    let page = notes.list(ListOptions::default()).await?;
    println!("{} notes, next={:?}", page.items.len(), page.next);

    notes.delete(&id).await?;
    assert!(notes.get(&id).await?.is_none());

    kernel.close().await?;
    Ok(())
}
```

## 11. `shio-core` 公开接口（当前有效）

`shio-core` 是同步内核，不依赖 tokio。应用通常经 `shio` 使用；需要自行控制阻塞线程，或使用同步能力时直接依赖本 crate。

### 11.1 常量与统计

```rust
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const FORMAT: u16 = 1; // 位于 shio_core::store::FORMAT，根未重导出

pub struct Stats {
    pub format_version: u16,
    pub key_id: u16,
    pub db_bytes: u64,
}
```

`VERSION` 是编译期包版本；`FORMAT` 是当前主格式版本；`Stats` 由 `Engine::stats` 返回。

### 11.2 `EngineOptions` 与 KDF 参数

```rust
pub struct EngineOptions {
    pub path: PathBuf,
    pub create: bool,          // 默认 false
    pub read_only: bool,       // 默认 false
    pub max_value_size: usize, // 默认 16 MiB
    pub app_id: String,        // 默认空
    pub kdf: KdfParams,        // 建库时使用；已存在的库以落盘参数为准
    pub cache_size: Option<usize>,
}

impl EngineOptions {
    pub fn new(path: impl Into<PathBuf>) -> Self;
    pub fn create(self, yes: bool) -> Self;
    pub fn read_only(self, yes: bool) -> Self;
    pub fn max_value_size(self, bytes: usize) -> Self;
    pub fn app_id(self, app_id: impl Into<String>) -> Self;
    pub fn kdf(self, kdf: KdfParams) -> Self;
    pub fn cache_size(self, bytes: usize) -> Self;
}

pub struct KdfParams {
    pub m_cost_kib: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

impl Default for KdfParams; // 65536 KiB / 3 / 1
impl KdfParams {
    pub const fn new(m_cost_kib: u32, t_cost: u32, p_cost: u32) -> Self;
    pub fn validate(&self) -> Result<()>;
}
```

`KdfParams` 位于 `shio_core::crypto`，用于建库与测试下调参数；非法参数返回 `Corrupted`。`create` 与 `read_only` 同时为真、或 `cache_size=Some(0)`，在打开前返回 `InvalidArgument`。

### 11.3 `Engine`

```rust
pub struct Engine { /* redb 句柄 + 会话密钥 */ } // Send + Sync，不实现 Clone

impl Engine {
    pub fn open(options: EngineOptions, password: &[u8]) -> Result<Engine>;

    pub fn insert<T: Serialize>(&self, ns: &str, coll: &str, value: &T)
        -> Result<(RecordId, RecordMeta)>;
    pub fn insert_raw(&self, ns: &str, coll: &str, plaintext: &[u8])
        -> Result<(RecordId, RecordMeta)>;

    pub fn put<T: Serialize>(&self, ns: &str, coll: &str, id: RecordId,
        value: &T, data_version: u16) -> Result<RecordMeta>;
    pub fn put_raw(&self, ns: &str, coll: &str, id: RecordId,
        plaintext: &[u8], data_version: u16) -> Result<RecordMeta>;

    pub fn get<T: DeserializeOwned>(&self, ns: &str, coll: &str, id: &RecordId)
        -> Result<Option<Stored<T>>>;
    pub fn get_raw(&self, ns: &str, coll: &str, id: &RecordId)
        -> Result<Option<(RecordMeta, Zeroizing<Vec<u8>>)>>;

    pub fn delete(&self, ns: &str, coll: &str, id: &RecordId) -> Result<bool>;
    pub fn purge(&self, ns: &str, coll: &str, id: &RecordId) -> Result<bool>;
    pub fn list(&self, ns: &str, coll: &str, options: ListOptions)
        -> Result<Page<RecordMeta>>;

    pub fn batch<T: Serialize>(&self, ns: &str, coll: &str, ops: &[WriteOp<T>])
        -> Result<Vec<RecordMeta>>;
    pub fn batch_raw(&self, ns: &str, coll: &str, ops: &[RawWriteOp<'_>])
        -> Result<Vec<RecordMeta>>;

    pub fn collections(&self, ns: &str) -> Result<Vec<String>>;
    pub fn stats(&self) -> Result<Stats>;
    pub fn flush(&self) -> Result<()>;

    pub fn change_password(&self, new_password: &[u8]) -> Result<()>;
    pub fn rotate_data_key(&self) -> Result<()>;
}

pub enum RawWriteOp<'a> {
    Put { id: RecordId, plaintext: &'a [u8] },
    Delete { id: RecordId },
}
```

- `*_raw` 接收已序列化的明文，供 facade 在调用线程序列化、阻塞线程加密；普通方法在内部做 serde JSON。
- `get_raw` 返回的明文包在 `Zeroizing` 中；墓碑与不存在的记录都返回 `None`。
- `purge` 是物理删除，多端同步启用后返回 `SyncEnabled`。
- core 不调用 `spawn_blocking`；调用方负责把阻塞操作移出 async executor。
- `codec` 与 `crypto` 模块虽然公开，但属于落盘/密码学内部契约，不承诺应用层稳定性；应用应使用本节与 `model` 的类型。

## 12. 多端同步（已定设计，未实现）

> 本节是 `20261022-sync-design` 的公开面，M0–M5 实现前不构成可用 API。

### 12.1 crate 与 feature

- `shio-sync` 只依赖 `shio-core`，同步、无 tokio、无网络；只生产/消费字节帧。
- `shio` 增加可选 `sync` feature；开启后依赖 `shio-sync`，关闭时默认 API 与依赖不变。
- 同步库使用 `format=2`：`key_id` 为 16 字节随机值，新增 `shio.keys2`、`shio.changes`、`shio.sync`。
- 导出面：`shio-core` 根重新导出 `KeyId` / `DeviceId` 与同步类型；`shio-sync` 重新导出其公开类型；`shio` 在 `sync` feature 下重新导出 `SyncInfo`、`ChangePage`、`ChangeEntry`、`RotationOffer`、`OfferPage`、`RotationInfo`、`ApplyReport`、`SnapshotPage`、`SnapshotRecord`、`PairingOffer`、`KeyId`、`DeviceId` 与 `SyncHandle`。

### 12.2 标识类型

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyId([u8; 16]);

impl KeyId {
    pub const fn from_bytes(bytes: [u8; 16]) -> Self;
    pub const fn as_bytes(&self) -> &[u8; 16];
    /// format=1 的 u16 映射：前 14 字节为 0，后 2 字节大端。
    pub const fn from_u16(value: u16) -> Self;
    pub const fn as_u16(&self) -> Option<u16>;
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceId([u8; 16]);

impl DeviceId {
    pub const fn from_bytes(bytes: [u8; 16]) -> Self;
    pub const fn as_bytes(&self) -> &[u8; 16];
}
```

M0 会把 `shio_core::crypto::KeyId`（u16）改名为 `LegacyKeyId`，并在 `model` 导出 16 字节的 `KeyId`；`Stats.key_id` 随之改为 `KeyId`。`KeyId` 与 `DeviceId` 的 `Debug` / `Display` 输出小写 hex，不包含秘密。

### 12.3 数据类型

```rust
pub struct SyncInfo {
    pub db_id: [u8; 16],
    pub device_id: DeviceId,
    pub current_key_id: KeyId,
    pub log_start: u64,
    pub log_end: u64,
    pub baseline_done: bool,
}

pub struct ChangePage {
    pub sender: DeviceId,
    pub after: u64,
    pub next: u64,
    pub has_more: bool,
    pub log_start: u64,
    pub log_end: u64,
    pub entries: Vec<ChangeEntry>,
}

#[non_exhaustive]
pub enum ChangeEntry {
    Record(RecordChange),
    RotationOffer(RotationOffer),
    RotationAck { to: KeyId, device: DeviceId, at: Timestamp },
    CurrentKey { key_id: KeyId, at: Timestamp },
    Migrated { key_id: KeyId, at: Timestamp },
}

pub struct RecordChange {
    pub namespace: String,
    pub collection: String,
    pub id: RecordId,
    pub key_id: KeyId,
    pub rev: u64,
    pub updated_at: Timestamp,
    pub deleted: bool,
    pub writer: DeviceId,
    /// 原样转发的 RecordBlob v2 字节；同步层不解密。
    pub blob: Vec<u8>,
}

pub struct RotationOffer {
    pub from: KeyId,
    pub to: KeyId,
    pub created_at: Timestamp,
    pub origin: DeviceId,
    pub wrap_nonce: [u8; 24],
    /// 用 from_DK 派生的 RK 包裹的 DK_to。
    pub wrapped_dk: Vec<u8>,
}

pub struct OfferPage {
    pub sender: DeviceId,
    pub offers: Vec<RotationOffer>,
}

pub struct RotationInfo {
    pub from: KeyId,
    pub to: KeyId,
    pub created_at: Timestamp,
    pub acked: Vec<DeviceId>,
}

pub struct ApplyReport {
    pub applied: usize,
    pub skipped: usize,
    pub controls: usize,
}

pub struct SnapshotPage {
    pub at_seq: u64,
    pub next_key: Option<Vec<u8>>,
    pub entries: Vec<SnapshotRecord>,
}

pub struct SnapshotRecord {
    pub namespace: String,
    pub collection: String,
    pub id: RecordId,
    pub blob: Vec<u8>,
}

pub struct PairingOffer {
    pub frame: Vec<u8>,
    pub secret: String,
    pub fingerprint: String,
}
```

`ChangeEntry` 的变体与 `data.md` 的 `kind=0..4` 一一对应；新变体通过 `#[non_exhaustive]` 追加。`PairingOffer.secret` 是 Base32 文本，调用方应移入 `Zeroizing` 并在使用后清零；facade 可直接接收 `Zeroizing<String>`。

### 12.4 `shio-core` 同步接口

```rust
impl Engine {
    pub fn enable_sync(&self, device_id: Option<DeviceId>) -> Result<SyncInfo>;
    pub fn sync_info(&self) -> Result<SyncInfo>;
    pub fn changes_since(&self, after: u64, limit: usize) -> Result<ChangePage>;

    pub fn snapshot_start(&self) -> Result<u64>;
    pub fn snapshot_page(&self, after: Option<&[u8]>, limit: usize) -> Result<SnapshotPage>;
    pub fn apply_page(&self, page: &ChangePage) -> Result<ApplyReport>;

    pub fn begin_rotation(&self) -> Result<RotationInfo>;
    pub fn rotation_state(&self) -> Result<Option<RotationInfo>>;
    pub fn rotation_offers(&self) -> Result<Vec<RotationOffer>>;
    pub fn acknowledge_rotation(&self, to: KeyId) -> Result<()>;
    pub fn migrate_all(&self, to: KeyId) -> Result<u64>;
    pub fn retire_key(&self, from: KeyId) -> Result<()>;
    pub fn prune_rotation_offers(&self, to: KeyId) -> Result<()>;

    pub fn compact_changes(&self, before: u64) -> Result<u64>;

    pub fn create_pairing_bundle(&self, ttl: Duration) -> Result<Vec<u8>>;
    pub fn open_with_pairing(
        options: EngineOptions,
        password: &[u8],
        bundle: &[u8],
        secret: &[u8],
    ) -> Result<Engine>;
}
```

- `enable_sync` 在 `format=1` 上执行不可回退升级；`format=2` 直接初始化。
- `apply_page` 整页一个写事务，推进 `peer/<sender>` 游标；缺 key 返回 `RotationRequired`。
- `begin_rotation` 任意设备可调用；并发轮换产生不同随机 `to`，写 key 按 `(created_at, key_id)` 收敛。
- `rotate_data_key` 在同步模式下等价于 `begin_rotation` + `migrate_all(to)`，不自动退役。
- 同步模式的 `change_password` 在同一事务内重新包裹所有 `shio.keys2` 行（同一新 salt/KEK、各自 nonce）；`format=1` 仍要求单行。
- `open_with_pairing` 只创建新库，目标必须不存在或为空；成功时直接是 `format=2`。
- `read_only` 句柄上的 `enable_sync`、`apply_page`、轮换/迁移/退役/压缩均返回 `ReadOnly`。

### 12.5 `shio-sync` 包装

```rust
pub struct Sync<'a> { /* &Engine */ }

impl<'a> Sync<'a> {
    pub fn new(engine: &'a Engine) -> Self;
    pub fn info(&self) -> Result<SyncInfo>;
    pub fn changes_since(&self, after: u64, limit: usize) -> Result<ChangePage>;
    pub fn encode(&self, page: &ChangePage) -> Result<Vec<u8>>;
    pub fn decode(&self, frame: &[u8]) -> Result<ChangePage>;
    pub fn apply(&self, frame: &[u8]) -> Result<ApplyReport>;

    pub fn offers(&self) -> Result<OfferPage>;
    pub fn encode_offers(&self, page: &OfferPage) -> Result<Vec<u8>>;
    pub fn apply_offers(&self, frame: &[u8]) -> Result<()>;

    pub fn snapshot(&self) -> Result<SnapshotIter<'_>>;

    pub fn begin_rotation(&self) -> Result<RotationInfo>;
    pub fn rotation_offers(&self) -> Result<Vec<RotationOffer>>;
    pub fn migrate_all(&self, to: KeyId) -> Result<u64>;
    pub fn retire(&self, from: KeyId) -> Result<()>;
    pub fn prune_offers(&self, to: KeyId) -> Result<()>;
    pub fn compact(&self, before: u64) -> Result<u64>;

    pub fn pairing_offer(&self, ttl: Duration) -> Result<PairingOffer>;
    pub fn accept_pairing(
        options: EngineOptions,
        password: &[u8],
        offer: &PairingOffer,
        secret: &[u8],
    ) -> Result<Engine>;
}
pub struct SyncHandle { /* Arc<Engine> */ }
```

`SyncHandle` 是 `Sync` 的拥有型版本（持有 `Arc<Engine>`），方法集与 `Sync` 相同；facade 的 `Kernel::sync()` 返回它。

帧格式（`SHY2`、变更页/offer 页/快照页）与字节布局见 `data.md` 第 15 节；同步层不引入网络、TLS 或后台线程。

### 12.6 facade（`sync` feature）

```rust
#[cfg(feature = "sync")]
impl Kernel {
    pub fn sync_info(&self) -> Result<SyncInfo>;
    pub fn sync(&self) -> SyncHandle; // 绑定同一 Arc<Engine>
    pub fn create_pairing_offer(&self, ttl: Duration) -> Result<PairingOffer>;
}

#[cfg(feature = "sync")]
pub async fn open_with_pairing(
    options: OpenOptions,
    credential: Credential,
    offer: &PairingOffer,
    secret: Zeroizing<String>,
) -> Result<Kernel>;
```

所有同步方法与现有 facade 一样经 `spawn_blocking`；口令与 pairing secret 用 `Zeroizing` 搬运，`Debug` 不输出内容。`SyncHandle` 拥有同一 `Arc<Engine>`，方法集与 `shio_sync::Sync` 相同，避免 facade 句柄生命周期外借。

### 12.7 新增错误变体

```rust
#[non_exhaustive]
pub enum Error {
    // ...现有变体...
    SyncDisabled,
    SyncEnabled,
    SyncResyncRequired { log_start: u64 },
    RotationRequired { key_id: KeyId },
    PairingExpired,
    PairingInvalid,
}
```

- `SyncDisabled`：未启用同步就拉取/应用日志。
- `SyncEnabled`：同步模式下调用 `purge`。
- `SyncResyncRequired`：游标早于 `log_start`，需走快照。
- `RotationRequired`：遇到本机缺少的记录 key，需先安装 offer。
- `PairingExpired` / `PairingInvalid`：配对包过期或认证失败；不泄露内部细节。

### 12.8 兼容与升级

- `enable_sync` 在单事务内写 `shio.keys2` 与 `format=2` meta，再分批把 `SHR1` 迁成 `SHR2`；升级不可回退。
- 旧二进制只认识 `format=1`，打开同步库时在 meta 阶段被拒绝。
- 升级期间 `change_password` / `rotate_data_key` 返回 `Busy`。
- 新设备配对包携带整条密钥链；旧 key 的退役需要所有已知设备公告迁移完成。
