# 20261025-file-compaction

<!-- 物理文件压缩：离线入口，整理 redb 页并释放空洞；自动日志压缩留给后续变更。 -->

## 范围

当前两个待办：

- 物理文件压缩：redb 是 copy-on-write，删除、覆盖、日志裁剪都只把页标成空闲，文件只增不减；`Stats.db_bytes` 只反映文件大小，没有整理入口。
- 自动日志压缩：`compact_changes(before)` 只能由应用手动调用；按对端进度安全触发需要跨设备协议扩展。

本次交付**物理文件压缩的离线入口**（core + facade + C ABI）。自动日志压缩只给出设计结论与前置条件，单开变更实施。

不在本次范围：在线压缩、后台压缩线程、定时触发、文件重建式压缩、日志自动裁剪。

不改记录格式、密钥层级、`format` 布局与 `db_id`/`device_id`。

## 为什么先做离线压缩

redb 4.4 的压缩是就地整理：`Database::compact(&mut self)` 需要**唯一可变借用**，并且要求调用时没有进行中的事务、读引用或 savepoint，否则返回 `CompactionError::TransactionInProgress` / `*SavepointExists`。

- `Engine` 被 `Arc` 共享，`Database` 直接持有在 `Handle` 里；在线压缩要把 `handle` 改成 `RwLock<Handle>` 并让所有读写入口先取锁，改动面覆盖 `engine.rs` / `engine_sync.rs` / `engine_backup.rs` / `pairing.rs`。
- 即便改了锁，任何正在进行的读事务都会让压缩失败。应用侧可见行为会是「偶发 `Busy`，重试」，实际很难保证一次成功。
- 离线入口满足计划里的「关闭句柄或提供独占维护接口」，实现小、可跨进程，失败语义干净。

因此本次采用离线入口；在线压缩留作后续独立变更（若需要，再评估 `RwLock<Handle>` 与失败重试策略）。

## 离线物理压缩语义

### 前置条件

- 调用方必须关闭所有 shio 句柄（同进程的 `Kernel`/`SyncHandle`，以及其它进程）。
- facade 入口先取数据库旁路 `.lock` 的独占锁：拿不到返回 `Locked`，不触碰数据库文件。redb 自身的文件锁作为第二道保护，同一文件被其它句柄打开时打开失败也映射为 `Locked`。
- 目标文件不存在返回 `NotFound`；不隐式创建空库。

### 执行

1. 取独占 `.lock`。
2. 记录 `before_bytes = metadata(path).len()`。
3. 以读写方式打开 redb（`Database::open`，不创建；空文件会被 redb 初始化，因此要求文件非 0 字节，否则 `InvalidArgument`）。
4. `db.compact()`，`compacted=false` 表示已经无可整理空间。
5. `db.check_integrity()` 校验压缩结果；失败返回 `Corrupted` 或底层 `Database` 错误。
6. 关闭句柄，记录 `after_bytes`。
7. 返回 `CompactReport { before_bytes, after_bytes, compacted }`。

压缩是纯页整理：不解密、不需要口令、不改 key 行、不改同步状态与迁移游标；format=1 与 format=2 都可压缩。

### 错误映射

| 情况 | 错误 |
|---|---|
| `.lock` 被占用（有 shio 句柄打开） | `Locked` |
| redb 报告 `DatabaseAlreadyOpen` | `Locked` |
| 文件不存在 | `NotFound` |
| 文件为 0 字节 | `InvalidArgument` |
| `TransactionInProgress` / `*SavepointExists` | `Busy` |
| 底层 I/O、校验失败 | `Io` / `Database` / `Corrupted` |

不新增错误变体；C ABI 复用既有 `SHIO_LOCKED` / `SHIO_NOT_FOUND` / `SHIO_BUSY` / `SHIO_INVALID_ARGUMENT` / `SHIO_CORRUPTED`。

### 崩溃语义

redb 4 的压缩在内部事务中搬页并提交，中断时按提交槽恢复，不需要临时文件或重命名。压缩前后文件都可被下一次 `open` 打开；最坏情况是压缩未生效（`compacted=false` 或文件大小不变），不会产生逻辑损坏。设计上**不**引入「压缩到临时文件再改名」的路径，避免与 S2 备份/恢复的原子发布逻辑重复。

## 自动日志压缩（本次不实现，记录结论）

目标：按「所有已知对端都已消费到的最小游标」裁剪 changes 日志，保留手动 `compact_changes(before)` 作为兜底。

结论与前置条件：

- 需要新增跨设备进度上报：用 `SHY2` 帧新增 kind=3（`KIND_PROGRESS`，空条目），帧头 `sender` 为报告方、`next` 为报告方在接收方日志中的已消费游标；接收方校验 `db_id` 后写入 `shio.sync` 的 `heard/<device>`，同时把该设备登记为已知对端。
- 裁剪点取 `min(heard/<device>)`；`known_devices()`（现有 `cur/` 前缀）中只要有一台设备没有报告，就**不裁剪**（返回 0）。新配对设备走基线快照，不依赖旧日志，因此不需要为「新设备」保留日志。
- 触发方式保持显式：`compact_changes_auto()`（+ C ABI），不放进 `flush`，避免后台任务与写入路径耦合。
- 这项工作要动帧格式与对端状态，需要双设备与兼容性测试，因此单开变更，不与本次物理压缩混在一起。

## API 形态

```rust
// shio-core
pub struct CompactReport {
    pub before_bytes: u64,
    pub after_bytes: u64,
    pub compacted: bool,
}

impl Engine {
    /// 离线压缩数据库文件；要求没有其它打开的句柄。
    pub fn compact_file(path: &Path) -> Result<CompactReport>;
}

// facade（`shio`）
pub async fn compact_file(path: &Path) -> Result<CompactReport>;
```

```c
/* 离线压缩：返回 {"before_bytes":N,"after_bytes":N,"compacted":bool}。 */
int32_t shio_compact_file(const char *path,
                          uint8_t **out_json, size_t *out_json_len);
```

- facade 版本与 `Kernel::restore` 一致走 `spawn_blocking`，不阻塞调用线程；C ABI 版本阻塞在 FFI worker 线程内。
- 与既有 `shio_sync_compact`（日志裁剪）区分命名：物理文件压缩用 `shio_compact_file`。
- 新增符号为增量，`SHIO_ABI_VERSION` 保持 1。

## 验收

- core：临时库写入并删除大量记录后压缩，文件变小、记录与同步状态不变；重复压缩 `compacted=false`；文件不存在 `NotFound`；已打开句柄时 `Locked`；0 字节文件 `InvalidArgument`。
- facade：`compact_file` 在关闭 Kernel 后成功；Kernel 未关闭时 `Locked`；压缩后重新打开记录可读。
- C ABI：`shio_compact_file` 返回摘要 JSON；句柄打开时 `SHIO_LOCKED`；不存在路径 `SHIO_NOT_FOUND`；压缩后可重新 `shio_open`。
- Android harness：新增 `compact` 模式（写入/删除 → 关闭 → 压缩 → 重开校验 → 再压缩 `compacted=0`），双 ABI 上机验证；同轮回归既有模式。
