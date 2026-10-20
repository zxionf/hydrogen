# shio API

<!-- 公共 API 的稳定契约。实现可演进；改变签名、语义或错误分类前，先追加 DECISIONS.log。 -->

本文只描述**当前有效**的 API 契约。实现细节（表、字节、AAD）见 `data.md`；定位与取舍见 `changes/20261020-encrypted-persistence-core/design.md`。

## 1. 范围与稳定性

- 目标平台：Rust 应用（原生）、Android、C++/Vulkan GUI（经 C ABI）。
- 近期形态：嵌入式库。同一内核可被服务端直接链接，不引入 daemon 协议。
- 版本策略：1.0 前 API 允许调整；记录格式 `format=1` 从首版起保持向后兼容。
- 线程模型：对外 async（tokio），内核同步；所有阻塞工作离开 executor。

## 2. crate 与模块

| crate | 职责 | 允许依赖 |
|---|---|---|
| `shio-core` | 模型、ID、加密、redb 存储、事务 | redb、serde、argon2、chacha20poly1305、hkdf、zeroize、uuid、thiserror |
| `shio` | async facade、生命周期、句柄 | `shio-core`、tokio、secrecy |
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
    pub fn new_v7() -> Self;
    pub fn from_bytes(bytes: [u8; 16]) -> Self;
    pub fn as_bytes(&self) -> &[u8; 16];
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
    pub fn as_millis(&self) -> i64;
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

- v0.1 只保证按 `RecordId` 升序；UUIDv7 与写入时间近似同序。
- 按 `updated_at` 排序、前缀过滤、内容查询需要二级索引，属于后续变更。
- `limit=0` 或 `>1000` 返回 `Error::InvalidArgument`。

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
- `data_version` 用于单个集合的载荷演进，由调用方解释，内核不强制。
- 新增字段只能追加在 header 尾部并提升 `format`；旧库由显式迁移升级，不静默改写。
- `key_id` 允许新旧密钥并存，是轮换与多设备密钥分发的基础。

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
