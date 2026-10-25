# 20261025-auto-log-compaction

<!-- 按对端进度安全裁剪 shio.changes：新增 SHY2 进度帧、heard/<device> 与 compact_changes_auto。 -->

## 背景

`Engine::compact_changes(before)` 只能由应用手动调用。自动裁剪需要回答一个问题：
**删除 `before` 之前的日志，是否会伤害某个还没读到这些条目的对端？**

现有信息不足以回答：

- `peer/<device>` 是本机消费对端日志的游标（`apply_page` 写入），方向相反；
- `cur/<device>` 只说明「我们应用过该设备的变更」，不说明它读到了我们的哪一条；
- 应用可以直接调 `compact_changes`，但没有任何机制检查对端进度。

因此自动裁剪的前提是**跨设备进度上报**：让每个拉取方告诉日志持有方「我读到哪了」。

## 目标

- 新增进度上报帧（`SHY2` kind=3），把「拉取方在持有方日志中的游标」送回持有方。
- 持有方把进度落在 `shio.sync` 的 `heard/<device>`，单调推进。
- 新增 `compact_changes_auto()`：所有已知对端都报告过进度后，裁剪到
  `min(heard) + 1`；任一已知对端缺报告则不裁剪。
- 保留 `compact_changes(before)` 手动入口不变。

不在本次范围：自动定时触发、后台线程、日志保留条数策略、跨设备进度压缩成摘要。

## 帧格式

沿用 `SHY2` 头（77 字节），新增 `KIND_PROGRESS = 3`：

| 字段 | 含义 |
|---|---|
| `kind` | `3` |
| `db_id` | 逻辑库 ID（收发双方必须一致） |
| `sender` | 报告方设备 ID |
| `next` | 报告方在**接收方**日志中的已消费游标 |
| `after` / `log_start` / `log_end` | 固定 0 |
| `count` | 固定 0，不带条目 |

- 编码：`frame::encode_progress(db_id, device, cursor)`。
- 解码：`frame::decode_progress(expected_db, frame) -> (DeviceId, u64)`；校验 kind、
  `db_id`、无条目。`parse_frame` 的 kind 白名单加入 3。
- 兼容性：新 kind 是增量扩展；旧版本的 `parse_frame` 会以
  `InvalidArgument("unknown frame kind")` 拒绝，但旧版本不会主动收到该帧。
  `SHIO_ABI_VERSION` 保持 1。

方向示例：B 从 A 拉取后，用 `Sync::progress_frame(A)` 生成帧发给 A；A 调用
`Sync::apply_progress(frame)` 记录 `heard/B = 游标`。

## 存储

`shio.sync` 新增 `heard/<device>`（16 字节设备 ID 后缀），值为 8 字节大端 `u64`，
表示该设备已消费到本机日志的哪一条（含）。

- 单调：同一设备的新值小于旧值时保留旧值。
- 防御性截断：上报值大于本机当前 `log_end` 时按 `log_end` 落盘。
- 与 `peer/<device>` 互不影响：前者是「对方读我们」，后者是「我们读对方」。

## 裁剪规则

`compact_changes_auto()`：

1. 要求可写、已启用同步；维护模式返回 `Busy`（与普通写一致）。
2. 已知对端集合 = `cur/` 前缀设备 ∪ `heard/` 前缀设备。
3. 集合为空 → 返回 0。
4. `cur/` 中任一设备没有 `heard/` → 返回 0（不裁剪）。
5. 取 `min(heard)`；删除键 `<= min(heard)` 的条目，即调用
   `compact_changes(min_heard + 1)`。
6. 裁剪点最多删到 `log_end` 之前，**保留最新一条**：否则空表会让 `log_end` 回到 0，
   序列号被重置，已有游标的对端将永远看不到新变更。
7. 返回删除条数；不需要裁剪时返回 0。

安全兜底已经存在：`changes_since(after)` 在 `after + 1 < log_start` 时返回
`Error::SyncResyncRequired { log_start }`，落后于裁剪点的对端会明确收到「需走快照」
而不是静默丢数据。快照流程（`snapshot_start`/`snapshot_page`/`apply_snapshot`）已在
存储维护 S1 中交付。

## API

```rust
// shio-core
impl Engine {
    /// 本机在 `peer` 日志中的已消费游标；未消费过返回 None。
    pub fn peer_cursor(&self, peer: DeviceId) -> Result<Option<u64>>;

    /// 记录对端已消费到本机日志的游标；单调推进，返回落盘值。
    pub fn apply_peer_progress(&self, device: DeviceId, cursor: u64) -> Result<u64>;

    /// 按对端进度裁剪变更日志；返回删除条数。
    pub fn compact_changes_auto(&self) -> Result<u64>;
}

// shio-sync（Sync 与 SyncHandle 同名方法）
pub fn progress_frame(&self, peer: DeviceId) -> Result<Vec<u8>>;
pub fn apply_progress(&self, frame: &[u8]) -> Result<u64>;
pub fn compact_changes_auto(&self) -> Result<u64>;

// facade
impl Kernel {
    pub async fn sync_progress_frame(&self, peer: DeviceId) -> Result<Vec<u8>>;
    pub async fn apply_sync_progress(&self, frame: Vec<u8>) -> Result<u64>;
    pub async fn compact_changes_auto(&self) -> Result<u64>;
}
```

```c
/* 生成本机上报给 peer 的进度帧。 */
int32_t shio_sync_progress_frame(shio_kernel *handle, const uint8_t *peer,
                                 uint8_t **out_frame, size_t *out_frame_len);

/* 应用对端进度帧；out_cursor 返回落盘后的游标（可为 NULL）。 */
int32_t shio_sync_apply_progress(shio_kernel *handle,
                                 const uint8_t *frame, size_t frame_len,
                                 uint64_t *out_cursor);

/* 按对端进度自动裁剪；out_count 可为 NULL。 */
int32_t shio_sync_compact_auto(shio_kernel *handle, uint64_t *out_count);
```

命名与既有 `shio_sync_compact`（手动裁剪）区分：自动入口用 `shio_sync_compact_auto`。

## 验收

- core：
  - 无已知对端 → `compact_changes_auto` 返回 0；
  - 有 `cur/` 但无 `heard/` → 返回 0；
  - 上报进度后按 `min(heard)+1` 裁剪，`changes_since(min_heard)` 仍可读；
  - 同一设备上报更小游标不回退；超过 `log_end` 的值被截断；
  - 裁剪后落后游标返回 `SyncResyncRequired`；
  - 未启用同步 / 只读 / 维护模式错误符合既有约定。
- shio-sync：进度帧编码→解码往返；`db_id` 不匹配、kind 不符、带条目被拒。
- FFI：`shio_sync_progress_frame`/`shio_sync_apply_progress`/`shio_sync_compact_auto`
  往返；两设备帧交换后自动裁剪生效。
- Android harness：新增 `auto-compact` 模式（本地模拟对端上报 → 裁剪 → 落后游标收到
  `SHIO_SYNC_RESYNC_REQUIRED`），双 ABI 上机；同轮回归既有模式。
