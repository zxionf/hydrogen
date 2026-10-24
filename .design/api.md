# shio API

<!-- 公共 API 的稳定契约。实现可演进；改变签名、语义或错误分类前，先追加 DECISIONS.log。 -->

本文描述**当前有效**的 API 契约；多端同步公开面见第 12 节，已随 `shio-sync` 与 `shio` 的 `sync` feature 落地。实现细节（表、字节、AAD）见 `data.md`；定位与取舍见 `changes/20261020-encrypted-persistence-core/design.md` 与 `changes/20261022-sync-design/design.md`。

## 1. 范围与稳定性

- 目标平台：Rust 应用（原生）、Android、C++/Vulkan GUI（经 C ABI）。
- 近期形态：嵌入式库。同一内核可被服务端直接链接，不引入 daemon 协议。
- 版本策略：1.0 前 API 允许调整；记录格式 `format=1` 从首版起保持向后兼容，同步库的 `format=2` 不可回退。
- 线程模型：对外 async（tokio），内核同步；所有阻塞工作离开 executor。

## 2. crate 与模块

| crate | 职责 | 允许依赖 |
|---|---|---|
| `shio-core` | 模型、ID、加密、redb 存储、事务 | redb、serde、serde_json、argon2、chacha20poly1305、hkdf、zeroize、uuid、getrandom、thiserror |
| `shio` | async facade、生命周期、句柄 | `shio-core`、tokio、secrecy、serde、serde_json、zeroize；可选 `shio-sync`（`sync` feature） |
| `shio-ffi` | C ABI v1 | `shio`、tokio、serde、serde_json、zeroize；唯一允许 unsafe 的 crate |
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
- 同一进程内可克隆 `Kernel` 句柄共享；跨进程由文件锁保证单写，冲突时返回 `Error::Locked`。非 Android 平台使用 redb 自带锁；Android 上 redb 4.4 的字节范围锁不可用，facade 改用数据库旁的 `<db>.lock` 文件加 `flock`：写句柄独占、只读句柄共享。`.lock` 文件在句柄关闭时释放，不要在持有期间删除。
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

`read_only=true` 时所有写操作返回 `Error::ReadOnly`，且仍需口令解密；同时指定 `create=true` 返回 `Error::InvalidArgument`。若上次进程异常退出、redb 要求修复，只读打开会返回 `Database` 错误；应先以写句柄打开一次完成修复，再使用只读句柄。

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

    /// 在线备份；目标必须不存在。
    pub async fn backup(&self, target: PathBuf) -> Result<BackupInfo>;

    /// 从备份恢复到 options.path；目标必须不存在或为空，create 被忽略。
    pub async fn restore(
        options: OpenOptions,
        credential: Credential,
        backup: PathBuf,
    ) -> Result<Kernel>;

    pub fn namespace(&self, name: &str) -> Result<Namespace>;

    pub async fn flush(&self) -> Result<()>;

    pub async fn close(&self) -> Result<()>;

    /// 只重新包裹 DK，O(1)，不重写记录；轮换未完成（多行 key）时返回 Busy。
    pub async fn change_password(&self, new_password: SecretString) -> Result<()>;

    /// 生成新 DK，重加密全部记录，完成后淘汰旧 key。
    pub async fn rotate_data_key(&self) -> Result<()>;

    pub fn stats(&self) -> Result<Stats>;
}

pub struct BackupInfo {
    pub format_version: u16,
    pub db_bytes: u64,
    pub table_count: u64,
    pub row_count: u64,
}

pub struct Stats {
    pub format_version: u16,
    pub key_id: KeyId,
    pub db_bytes: u64,
}
```

`Kernel` 为 `Clone + Send + Sync + 'static`。`flush` 在默认持久化模式下是屏障语义；redb 每次提交即持久化，不依赖进程退出。

`Stats.key_id` 在 `format=1` 下是 `KeyId::from_u16` 的映射，在 `format=2` 下是 16 字节随机值（见第 12 节）。

`Kernel::backup` 在源库读事务中复制全部已知表；备份期间源库可继续写入，但旧页回收可能推迟。`Kernel::restore` 强制 `create=false`，在 Android 上先取得目标 `.lock` 独占锁，再用原口令校验临时副本；恢复后的库保留 `db_id`/`device_id`/key 链/同步游标，禁止把同一备份恢复到两台设备后同时启用同步。

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
- `format=2` 数据密钥迁移提供显式取消：取消在批次边界生效，已提交批次保留，旧 key 不删除；调用方应在后台线程执行迁移，在其它线程轮询进度或请求取消。
- `close` 与进行中的操作并发时：新操作立即 `Closed`，已开始的操作允许完成。
- `Collection<T>` 不缓存数据；不存在跨句柄的脏读。

### 6.1 稳定性保证

- core 的公开函数不以 panic 表达可恢复失败；内部不变式失败返回 `Error::Internal`。
- 落盘数据（header、键、长度、AAD、JSON）一律视为不可信，先校验后使用；损坏输入返回 `Corrupted` / `Deserialize`。
- 非测试代码不使用 `unwrap` / `expect` / `panic!`；CI 以 clippy 规则兜底。
- FFI 边界必须 `catch_unwind`，把 panic 转为错误码，禁止 panic 跨 C ABI。
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
    Cancelled,
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
- `Cancelled` 表示迁移在批次边界被调用方取消；已提交记录保留，可重新调用迁移继续。
- `Corrupted` 表示结构或认证失败（AEAD tag 校验失败也归入此类，不泄露细节）。
- 错误实现 `std::error::Error` 与 `Display`；不实现 `Clone`。
- 可恢复错误不 panic；只有违反类型不变式（如内部状态机损坏）才允许 `debug_assert` 后返回 `Internal`。
- 所有来自落盘字节的输入先做长度/边界校验；失败返回 `Corrupted` 或 `Deserialize`，绝不 panic、越界或 abort。

## 8. C ABI 映射（v1 + P4 同步扩展 + 存储维护 S1/S2）

C++ 与 Android 通过 `shio-ffi` 调用；契约头文件为 `crates/shio-ffi/include/shio.h`。用户已批准 `shio-ffi` 的 unsafe 例外，允许范围只在本 crate 的 FFI 边界内；`shio-core` 与 `shio` 继续 `forbid(unsafe_code)`。JNI/Kotlin 包装层由 `shio-android` 实现，内核不绑定具体 Java 包名或 UI 技术栈。

原则：

- 句柄不透明：`typedef struct shio_kernel shio_kernel;`
- 名称是 NUL 结尾的 UTF-8 字符串；载荷、口令和记录 ID 是显式长度的字节串，允许内嵌 NUL。口令字节必须是 UTF-8（facade 使用 `SecretString`）；载荷必须是合法 JSON。
- 记录 ID 固定为 16 字节原始 UUID；记录载荷是 JSON，读写都经过 `serde_json::Value`。
- 返回 `int32_t` 稳定错误码；`0` 成功，负数对应 `include/shio.h` 中的 `SHIO_*` 常量。未知错误映射为 `SHIO_INTERNAL`。
- 错误详情经 `shio_last_error(handle, buf, len)` 取回，线程局部；返回包含 NUL 的所需字节数，缓冲区不足时截断并补 NUL。
- FFI 内部为每个句柄持有 tokio multi-thread runtime，对外暴露**阻塞调用**；C++/Android 必须在 worker 线程调用，不阻塞 UI 线程。
- 所有导出函数都经过 `catch_unwind`；panic 转为 `SHIO_INTERNAL`，不跨 C ABI。
- 回调、取消令牌、异步 future 不在 v1。P4 以**增量符号**追加 sync/pairing；不改变现有函数签名或语义，因此 `SHIO_ABI_VERSION` 仍为 1，破坏性变更才升 2。
- 存储维护 S1 的轮换取消通过旁路状态与批次边界实现，不引入回调或取消令牌；轮换接口是兼容性例外，允许不兼容旧客户端，其他非轮换接口仍按增量方式演进。
- 存储维护 S2 的备份/恢复是新增非轮换符号；`shio_backup` 通过显式输出参数返回摘要 JSON，`shio_restore` 直接返回已打开的目标句柄。
- 同步业务数据只以不透明的 `SHY2` 帧进出；Kotlin/Java 不解析帧内部结构。同步状态、轮换信息和应用报告是小体积 JSON。

```c
uint32_t shio_abi_version(void);
size_t   shio_last_error(const shio_kernel *k, char *buf, size_t len);

int32_t shio_open(const char *path, const uint8_t *password, size_t password_len,
                  const uint8_t *options_json, size_t options_len,
                  shio_kernel **out);
int32_t shio_backup(shio_kernel *k, const char *out_path,
                    uint8_t **out_json, size_t *out_json_len);
int32_t shio_restore(const char *backup_path, const char *target_path,
                     const uint8_t *password, size_t password_len,
                     const uint8_t *options_json, size_t options_len,
                     shio_kernel **out);
int32_t shio_close(shio_kernel *kernel);
void    shio_kernel_destroy(shio_kernel *kernel);

int32_t shio_insert(shio_kernel *k, const char *ns, const char *coll,
                    const uint8_t *json, size_t json_len, uint8_t out_id[16],
                    uint8_t **out_meta, size_t *out_meta_len);
int32_t shio_put(shio_kernel *k, const char *ns, const char *coll,
                 const uint8_t id[16], const uint8_t *json, size_t json_len,
                 uint8_t **out_meta, size_t *out_meta_len);
int32_t shio_get(shio_kernel *k, const char *ns, const char *coll,
                 const uint8_t id[16], uint8_t **out_json, size_t *out_json_len);
int32_t shio_delete(shio_kernel *k, const char *ns, const char *coll,
                    const uint8_t id[16], int32_t *out_existed);
int32_t shio_list(shio_kernel *k, const char *ns, const char *coll,
                  const uint8_t *after, size_t limit, uint8_t include_deleted,
                  uint8_t **out_json, size_t *out_json_len);
int32_t shio_batch(shio_kernel *k, const char *ns, const char *coll,
                   const uint8_t *ops_json, size_t ops_json_len,
                   uint8_t **out_meta, size_t *out_meta_len);

int32_t shio_change_password(shio_kernel *k, const uint8_t *new_password,
                             size_t new_password_len);
int32_t shio_rotate_data_key(shio_kernel *k);
int32_t shio_flush(shio_kernel *k);
int32_t shio_stats(shio_kernel *k, uint8_t **out_json, size_t *out_json_len);

/* 释放所有由 shio 分配的返回缓冲区；ptr/len 必须成对。 */
void    shio_free(void *ptr, size_t len);

/* ---- P4 同步扩展；以下函数均为阻塞调用 ---- */

/* 启用同步；device_id 为 NULL 时生成随机设备 ID。返回 sync_info JSON。 */
int32_t shio_sync_enable(shio_kernel *k, const uint8_t *device_id,
                         uint8_t **out_json, size_t *out_json_len);
int32_t shio_sync_info(shio_kernel *k,
                       uint8_t **out_json, size_t *out_json_len);

/*
 * 读取 after 之后的变更页，返回 SHY2 帧。
 * after=0 表示从头；limit=0 使用默认 100，最大 1000。
 * out_next 为下一页游标，out_has_more 表示是否还有更多；两者可为 NULL。
 */
int32_t shio_sync_changes(shio_kernel *k, uint64_t after, size_t limit,
                          uint8_t **out_frame, size_t *out_frame_len,
                          uint64_t *out_next, uint8_t *out_has_more);

/* 应用变更帧；返回 {"applied":N,"skipped":N,"controls":N}。 */
int32_t shio_sync_apply(shio_kernel *k, const uint8_t *frame, size_t frame_len,
                        uint8_t **out_report_json, size_t *out_report_len);

/* 本机待发送的轮换 offer 帧。 */
int32_t shio_sync_offers(shio_kernel *k,
                         uint8_t **out_frame, size_t *out_frame_len);
/* 应用对端的 offer 帧；成功后本机安装新 key 并自动 ack。 */
int32_t shio_sync_apply_offers(shio_kernel *k,
                               const uint8_t *frame, size_t frame_len);

/* 快照起点；用于日志已压缩时的重同步。 */
int32_t shio_sync_snapshot_start(shio_kernel *k, uint64_t *out_at_seq);

/*
 * 读取一页快照并编码为 SHY2 帧。
 * after=NULL/0 表示第一页；out_next 是下一页的不透明游标；out_done=1 表示结束。
 * limit=0 使用默认 100，最大 1000。
 */
int32_t shio_sync_snapshot_page(shio_kernel *k,
                                const uint8_t *after, size_t after_len,
                                size_t limit,
                                uint8_t **out_frame, size_t *out_frame_len,
                                uint8_t **out_next, size_t *out_next_len,
                                uint8_t *out_done);
int32_t shio_sync_apply_snapshot(shio_kernel *k,
                                 const uint8_t *frame, size_t frame_len,
                                 uint8_t **out_report_json, size_t *out_report_len);

/* 本机发起轮换；返回 rotation_info JSON。 */
int32_t shio_sync_begin_rotation(shio_kernel *k,
                                 uint8_t **out_json, size_t *out_json_len);
/* 当前轮换状态；没有进行中的轮换时返回 JSON null。 */
int32_t shio_sync_rotation_state(shio_kernel *k,
                                 uint8_t **out_json, size_t *out_json_len);
int32_t shio_sync_ack_rotation(shio_kernel *k, const uint8_t *to);
int32_t shio_sync_migrate_all(shio_kernel *k, const uint8_t *to,
                              uint64_t *out_count);
/* batch_size=0 使用默认 256；0<=pause_ms<=60000；取消在批次边界生效。 */
int32_t shio_sync_migrate_all_ex(shio_kernel *k, const uint8_t *to,
                                 uint32_t batch_size, uint32_t pause_ms);
/* 返回 {"active":bool,"scanned":N,"migrated":N,"done":bool}。 */
int32_t shio_sync_migration_progress(shio_kernel *k,
                                     uint8_t **out_json, size_t *out_json_len);
int32_t shio_sync_cancel_migration(shio_kernel *k);
int32_t shio_sync_retire(shio_kernel *k, const uint8_t *from);
int32_t shio_sync_prune_offers(shio_kernel *k, const uint8_t *to);
int32_t shio_sync_compact(shio_kernel *k, uint64_t before,
                          uint64_t *out_count);

/*
 * 生成配对包；ttl_seconds 仅用于调用方展示/过期控制，0 使用 600 秒；
 * 接收端固定按 10 分钟窗口校验 created_at。
 * out_frame 为 SHW2 外层帧，out_json 为 {"secret":"...","fingerprint":"..."}。
 */
int32_t shio_sync_create_pairing(shio_kernel *k, uint64_t ttl_seconds,
                                 uint8_t **out_frame, size_t *out_frame_len,
                                 uint8_t **out_json, size_t *out_json_len);

/*
 * 用配对包创建新库；目标必须不存在或为空。
 * options_json 与 shio_open 相同；secret 为 Base32 文本字节。
 */
int32_t shio_open_with_pairing(const char *path,
                               const uint8_t *password, size_t password_len,
                               const uint8_t *options_json, size_t options_len,
                               const uint8_t *frame, size_t frame_len,
                               const uint8_t *secret, size_t secret_len,
                               shio_kernel **out_handle);
```

JSON 形状：

- `shio_open` 的 `options_json` 可为空；字段为 `create`、`read_only`、`max_value_size`、`cache_size`，未知字段拒绝。上次异常退出后若 redb 要求修复，`read_only` 打开会失败；先用写句柄打开一次修复。
- `shio_get` 返回 `{"meta":{...},"value":...}`；墓碑视为 `SHIO_NOT_FOUND`。
- `shio_insert` / `shio_put` / `shio_batch` 返回记录元信息，字段为 `id`、`rev`、`created_at`、`updated_at`、`data_version`、`deleted`、`ciphertext_len`。
- `shio_list` 返回 `{"items":[...],"next":"uuid"|null}`；`after` 为 `NULL` 表示从头，`limit == 0` 使用默认值。
- `shio_batch` 输入为 `[{"id":"uuid","value":...},{"id":"uuid","delete":true}]`，最多 1000 条，单次原子提交。
- `shio_stats` 返回 `format_version`、`key_id`（32 位十六进制）和 `db_bytes`。
- `shio_backup` 返回 `{"format_version":N,"db_bytes":N,"table_count":N,"row_count":N}`；备份文件使用原库口令打开，不携带明文或密钥。目标已存在返回 `SHIO_INVALID_ARGUMENT`；失败不会留下目标或临时文件。
- `shio_restore` 的目标必须不存在或为 0 字节；`options_json` 的 `create` 被忽略。恢复失败返回对应错误并删除临时文件/未完成的目标；成功返回已打开的目标句柄。目标非空返回 `SHIO_INVALID_ARGUMENT`。
- `shio_sync_info` / `shio_sync_enable` 返回 `{"db_id":"32hex","device_id":"32hex","current_key_id":"32hex","log_start":N,"log_end":N,"baseline_done":bool}`。
- `shio_sync_begin_rotation` / `shio_sync_rotation_state` 返回 `{"from":"32hex","to":"32hex","created_at":millis,"acked":["32hex",...]}`；没有进行中的轮换时 `rotation_state` 返回 `null`。
- `shio_sync_migration_progress` 返回 `{"active":bool,"scanned":N,"migrated":N,"done":bool}`；`scanned` 含已迁移记录，`done` 只表示最近一次迁移完整成功，不承诺百分比。
- `shio_sync_migrate_all_ex` 在取消时返回 `SHIO_CANCELLED`；已提交批次保留，旧 key 不删除。恢复中断轮换先取 `shio_sync_rotation_state` 的 `to`，再迁移。
- `shio_sync_apply` / `shio_sync_apply_snapshot` 返回 `{"applied":N,"skipped":N,"controls":N}`。
- `shio_sync_create_pairing` 返回 `{"secret":"<Base32>","fingerprint":"<6 位十六进制>"}`；`secret` 只用于配对展示，不得写日志。
- `shio_sync_changes`、`shio_sync_offers`、`shio_sync_snapshot_page` 的 `out_frame` 是 `SHY2` 帧；调用方不得解析或改写，只负责传输与 `shio_free`。
- `shio_sync_snapshot_page` 的 `out_next` 是不透明游标，必须与 `shio_free` 成对释放；`out_done=1` 时 `out_next` 为 `NULL`/`0`。

所有权与生命周期：

- 所有 `uint8_t **` 输出由调用方持有，必须用 `shio_free(ptr, len)` 释放；失败时输出为 `NULL`/`0`。
- `shio_close` 关闭并等待在途操作，但不释放句柄；可重复调用，之后的操作返回 `SHIO_CLOSED`。
- `shio_kernel_destroy` 释放句柄，调用前必须确保没有其它线程继续使用；不要重复 destroy。
- 口令临时缓冲区在边界内用 `zeroize` 清零；返回的记录明文由调用方在释放前自行处理。
- 内核单写多读：Android 多进程只能有一个可写 owner；跨进程只读受同一旁路 `.lock` 的共享锁约束。Android 的锁文件为 `<db>.lock`，由 facade 持有到 `close`。
- P4 首次建库：新库先 `shio_open`，随后立即 `shio_sync_enable`；只有拿到成功返回的 `sync_info` 后才写入业务记录。若启用失败，调用方应关闭并删除这个尚未使用的库，不要留下 `format=1` 的半成品。
- 同步帧、配对 secret、口令和明文都不得写入日志。`shio_sync_create_pairing` 返回的 `secret` 使用后应尽快覆盖或释放；`shio_free` 会在释放前清零输出缓冲区。
- 网络/TLS、二维码、重试、后台调度和冲突提示属于 `shio-android`；内核只提供帧、配对文本和状态查询。

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
    pub key_id: KeyId,
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

## 12. 多端同步（当前有效）

> 本节是 `20261022-sync-design` 的公开面；M0–M5 已实现，`shio-sync` 与 `shio` 的 `sync` feature 均已可用。

### 12.1 crate 与 feature

- `shio-sync` 只依赖 `shio-core`，同步、无 tokio、无网络；只生产/消费字节帧。
- `shio` 增加可选 `sync` feature；开启后依赖 `shio-sync`，关闭时默认 API 与依赖不变。
- 同步库使用 `format=2`：`key_id` 为 16 字节随机值，新增 `shio.keys2`、`shio.changes`、`shio.sync`。
- 导出面：`shio-core` 根重新导出 `KeyId` / `DeviceId` 与同步类型；`shio-sync` 重新导出其公开类型；`shio` 在 `sync` feature 下重新导出 `SyncInfo`、`ChangePage`、`ChangeEntry`、`RotationOffer`、`OfferPage`、`RotationInfo`、`ApplyReport`、`SnapshotPage`、`SnapshotRecord`、`SnapshotIter`、`PairingOffer`、`KeyId`、`DeviceId` 与 `SyncHandle`。

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

`shio_core::crypto::KeyId`（u16）保留为 `LegacyKeyId`；`model` 导出 16 字节的 `KeyId`，`Stats.key_id` 为 `KeyId`。`KeyId` 与 `DeviceId` 的 `Debug` / `Display` 输出小写 hex，不包含秘密。

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

`ChangeEntry` 的变体与 `data.md` 的 `kind=0..4` 一一对应；新变体通过 `#[non_exhaustive]` 追加。`PairingOffer.secret` 是 Base32 文本，调用方应移入 `Zeroizing` 并在使用后清零；`PairingOffer` 的 `Debug` 只输出帧长度、指纹与 `<redacted>`，不输出 secret；facade 可直接接收 `Zeroizing<String>`。

### 12.4 `shio-core` 同步接口

```rust
pub struct MigrationOptions {
    pub batch_size: usize, // 1..=1024，默认 256
    pub pause: Duration,   // 0..=60s，默认 0
}

pub struct MigrationProgress {
    pub active: bool,
    pub scanned: u64,
    pub migrated: u64,
    pub done: bool,
}

pub struct BackupInfo {
    pub format_version: u16,
    pub db_bytes: u64,
    pub table_count: u64,
    pub row_count: u64,
}
```

```rust
impl Engine {
    pub fn backup(&self, target: &Path) -> Result<BackupInfo>;
    pub fn restore(
        backup: &Path,
        options: EngineOptions,
        password: &[u8],
    ) -> Result<Engine>;

    pub fn enable_sync(&self, device_id: Option<DeviceId>) -> Result<SyncInfo>;
    pub fn sync_info(&self) -> Result<SyncInfo>;
    pub fn changes_since(&self, after: u64, limit: usize) -> Result<ChangePage>;

    pub fn snapshot_start(&self) -> Result<u64>;
    pub fn snapshot_page(&self, after: Option<&[u8]>, limit: usize) -> Result<SnapshotPage>;
    pub fn apply_page(&self, page: &ChangePage) -> Result<ApplyReport>;
    pub fn apply_snapshot_page(&self, page: &SnapshotPage) -> Result<ApplyReport>;

    pub fn begin_rotation(&self) -> Result<RotationInfo>;
    pub fn rotation_state(&self) -> Result<Option<RotationInfo>>;
    pub fn rotation_offers(&self) -> Result<Vec<RotationOffer>>;
    pub fn acknowledge_rotation(&self, to: KeyId) -> Result<()>;
    pub fn migrate_all(&self, to: KeyId) -> Result<u64>;
    pub fn migrate_all_with_options(
        &self,
        to: KeyId,
        options: MigrationOptions,
    ) -> Result<u64>;
    pub fn migration_progress(&self) -> MigrationProgress;
    pub fn cancel_migration(&self);
    pub fn retire_key(&self, from: KeyId) -> Result<()>;
    pub fn prune_rotation_offers(&self, to: KeyId) -> Result<()>;

    pub fn compact_changes(&self, before: u64) -> Result<u64>;

    pub fn create_pairing_bundle(&self, ttl: Duration) -> Result<Vec<u8>>;
    pub fn create_pairing_bundle_with_secret(
        &self,
        ttl: Duration,
    ) -> Result<(Vec<u8>, [u8; 32])>;
    pub fn open_with_pairing(
        options: EngineOptions,
        password: &[u8],
        bundle: &[u8],
        secret: &[u8],
    ) -> Result<Engine>;
}
```

- `backup` 在一个 redb 读事务中复制全部已知表到临时文件，校验表行数与 `meta` 原始字节后原子改名；目标已存在返回 `InvalidArgument`。备份不压缩同步日志，也不解密/重加密。
- `restore` 接受不存在的目标或 0 字节文件；先复制到同目录临时文件，用原口令打开校验，再替换空目标。校验失败删除临时文件；目标非空返回 `InvalidArgument`。`options.create` 被忽略，恢复出的库保留源库身份与同步状态。
- `enable_sync` 在 `format=1` 上执行不可回退升级；`format=2` 直接初始化。
- `apply_page` 整页一个写事务，推进 `peer/<sender>` 游标；缺 key 返回 `RotationRequired`。
- `apply_snapshot_page` 同样整页原子，写入的记录日志以本机 `device_id` 为 `writer`；缺 key 返回 `RotationRequired`。
- `begin_rotation` 任意设备可调用；并发轮换产生不同随机 `to`，写 key 按 `(created_at, key_id)` 收敛。
- `migrate_all` 重加密记录时推进 `updated_at`（至少 +1），避免旧密钥的同一逻辑版本在 LWW 的 blob 次序里把已迁移记录翻回旧密钥。
- `migrate_all_with_options` 保持迁移期间 session 写锁；`migration_progress` 与 `cancel_migration` 走旁路状态，不获取该锁。取消在批次提交后的边界生效。
- 取消后再次 `migrate_all(to)` 会从起点扫描并跳过已迁移记录；恢复中断轮换使用 `rotation_state()` 取 `to`，不要用 `rotate_data_key`。
- `rotate_data_key` 在同步模式下等价于 `begin_rotation` + `migrate_all(to)`，不自动退役；`format=1` 路径不提供进度或取消。
- 同步模式的 `change_password` 在同一事务内重新包裹所有 `shio.keys2` 行（同一新 salt/KEK、各自 nonce）；`format=1` 仍要求单行。
- `open_with_pairing` 只创建新库，目标必须不存在或为空；成功时直接是 `format=2`，失败不会留下半成品文件。
- 配对包不携带 TTL；接收端固定按 `DEFAULT_PAIRING_TTL`（10 分钟）窗口校验 `created_at`，`ttl` 由调用方用于展示与过期控制。
- `read_only` 句柄上的 `enable_sync`、`apply_page`、轮换/迁移/退役/压缩均返回 `ReadOnly`。

### 12.5 `shio-sync` 包装

```rust
pub struct Sync<'a> { /* &Engine */ }

impl<'a> Sync<'a> {
    pub fn new(engine: &'a Engine) -> Self;
    pub fn enable_sync(&self, device_id: Option<DeviceId>) -> Result<SyncInfo>;
    pub fn info(&self) -> Result<SyncInfo>;
    pub fn changes_since(&self, after: u64, limit: usize) -> Result<ChangePage>;
    pub fn encode(&self, page: &ChangePage) -> Result<Vec<u8>>;
    pub fn decode(&self, frame: &[u8]) -> Result<ChangePage>;
    pub fn apply(&self, frame: &[u8]) -> Result<ApplyReport>;

    pub fn offers(&self) -> Result<OfferPage>;
    pub fn encode_offers(&self, page: &OfferPage) -> Result<Vec<u8>>;
    pub fn decode_offers(&self, frame: &[u8]) -> Result<OfferPage>;
    pub fn apply_offers(&self, frame: &[u8]) -> Result<()>;

    pub fn snapshot(&self) -> Result<SnapshotIter<'a>>;
    pub fn snapshot_start(&self) -> Result<u64>;
    pub fn snapshot_page(
        &self,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<SnapshotPage>;
    pub fn encode_snapshot(&self, page: &SnapshotPage) -> Result<Vec<u8>>;
    pub fn decode_snapshot(&self, frame: &[u8]) -> Result<SnapshotPage>;
    pub fn apply_snapshot(&self, page: &SnapshotPage) -> Result<ApplyReport>;
    pub fn apply_snapshot_frame(&self, frame: &[u8]) -> Result<ApplyReport>;

    pub fn begin_rotation(&self) -> Result<RotationInfo>;
    pub fn rotation_offers(&self) -> Result<Vec<RotationOffer>>;
    pub fn rotation_state(&self) -> Result<Option<RotationInfo>>;
    pub fn acknowledge_rotation(&self, to: KeyId) -> Result<()>;
    pub fn migrate_all(&self, to: KeyId) -> Result<u64>;
    pub fn migrate_all_with_options(
        &self,
        to: KeyId,
        options: MigrationOptions,
    ) -> Result<u64>;
    pub fn migration_progress(&self) -> MigrationProgress;
    pub fn cancel_migration(&self);
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
pub struct SnapshotIter<'a> { /* Engine 或 Arc<Engine> + 游标 */ }

impl<'a> SnapshotIter<'a> {
    pub fn at_seq(&self) -> u64;
    pub fn next_page(&mut self, limit: usize) -> Result<Option<SnapshotPage>>;
}

pub struct SyncHandle { /* Arc<Engine> */ }
```

`SyncHandle` 是 `Sync` 的拥有型版本（持有 `Arc<Engine>`），方法集与 `Sync` 相同；`snapshot()` 返回拥有型迭代器，可脱离句柄借用生命周期；facade 的 `Kernel::sync()` 返回它。快照续拉锚点用 `SnapshotIter::at_seq()`（取自 `snapshot_start`），不要把后续 `SnapshotPage::at_seq` 当作固定起点。

帧格式（`SHY2`、变更页/offer 页/快照页）与字节布局见 `data.md` 第 15 节；同步层不引入网络、TLS 或后台线程。`decode*` 均校验 `db_id` 与本库一致，变更页还会校验 `ChangeEntry` 与 `RecordBlob` header 一致。

### 12.6 facade（`sync` feature）

```rust
#[cfg(feature = "sync")]
impl Kernel {
    pub fn sync_info(&self) -> Result<SyncInfo>;
    pub fn sync(&self) -> Result<SyncHandle>; // 绑定同一 Arc<Engine>；关闭后返回 Closed
    pub async fn create_pairing_offer(&self, ttl: Duration) -> Result<PairingOffer>;
}

#[cfg(feature = "sync")]
pub async fn open_with_pairing(
    options: OpenOptions,
    credential: Credential,
    offer: &PairingOffer,
    secret: Zeroizing<String>,
) -> Result<Kernel>;
```

`sync_info` 与 `sync` 是廉价的同步调用（与 `stats` 一致）；`create_pairing_offer` 与 `open_with_pairing` 经 `spawn_blocking`。口令与 pairing secret 用 `Zeroizing` 搬运，`Debug` 不输出内容。`SyncHandle` 拥有同一 `Arc<Engine>`，方法集与 `shio_sync::Sync` 相同，避免 facade 句柄生命周期外借。

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
    Cancelled,
}
```

- `Cancelled`：`format=2` 迁移在批次边界被取消；已迁移记录保留，可重新迁移。
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
