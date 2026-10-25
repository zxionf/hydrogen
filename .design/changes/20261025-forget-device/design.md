# 20261025-forget-device

<!-- 设备解绑：清掉某个对端的全部设备级同步状态，解除对自动裁剪与退 key 的阻塞。 -->

## 背景

自动日志裁剪的「已知对端」是 `cur/` 与 `heard/` 的并集（排除本机）；只要其中一个设备没有
上报 `heard/`，`compact_changes_auto` 就返回 0。轮换退役与 offer 清理也依赖「所有已知设备
都有 `ack/` / `mig/`」。

已经不再同步的设备（换机、卸载、退场）不会自己消失：

- `cur/<device>`、`heard/<device>` 永久占住裁剪门控；
- `ack/<to>/<device>`、`mig/<key>/<device>` 让 `retire_key` / `prune_rotation_offers` 一直等待；
- `peer/<device>` 保留着一个可能早已过期、甚至低于 `log_start` 的游标。

决策 #57 明确记录了这一点（「当前没有解绑 API，需要时可另开变更」）。本次补上显式解绑。

## 本阶段范围

- `Engine::forget_device(device)`：在一个写事务里删除该设备的全部设备级同步状态。
- 拒绝解绑本机；对未知设备幂等（返回全零报告）。
- 不影响记录、密钥、`rot/`、`upgrade`、变更日志。
- 解绑后自动裁剪与轮换退役不再等待该设备。
- facade + C ABI + Android harness。

不在本次范围：设备黑名单/撤销、强制删除该设备写入的记录、解绑时的网络通知、自动判断
「设备已退场」。

## 语义

删除的键（全部幂等）：

| 键 | 含义 |
|---|---|
| `peer/<device>` | 本机在该设备日志中的消费游标 |
| `cur/<device>` | 该设备公告的当前写 key |
| `heard/<device>` | 该设备上报的「已读到本机日志哪一条」 |
| `ack/<to>/<device>` | 该设备对某个轮换 key 的确认（扫描全部 `to`） |
| `mig/<key>/<device>` | 该设备已完成某个 key 的迁移（扫描全部 `key`） |

保留：记录与墓碑、`shio.keys2`、`meta`、`rot/`、`ack/*/<其它设备>`、`mig/*/<其它设备>`、
`upgrade`、`shio.changes`。

错误与顺序：

- 只读句柄 → `ReadOnly`；维护窗口 → `Busy`；未启用同步 → `SyncDisabled`；
- `device == 本机` → `InvalidArgument`（不能把自己从自己的裁剪门控里删掉）；
- 未知设备 → 成功，报告全为 `false` / 0（便于重试）。

## 设备回来后

没有黑名单，也不阻止重新配对：该设备的变更页会重新写入 `cur/`，进度帧会重新写入 `heard/`。
因为 `peer/` 已删除，本机会从 0 拉取；如果本机日志已经裁剪过，就会收到
`SyncResyncRequired`，走 `snapshot_start`/`snapshot_page`/`apply_snapshot` 重同步。

也就是说：解绑是「本机不再关心这个对端」的显式声明，不是永久拉黑；重新上线会按新对端
（或需要快照的老对端）处理。文档必须写清这一点，避免被当成远程撤销。

## 阶段 API

```rust
pub struct ForgetReport {
    pub device: DeviceId,
    pub removed_peer_cursor: bool,
    pub removed_current_key: bool,
    pub removed_progress: bool,
    pub removed_acks: u64,
    pub removed_migrations: u64,
}

impl Engine {
    pub fn forget_device(&self, device: DeviceId) -> Result<ForgetReport>;
}

// facade（`sync` feature）
impl Kernel {
    pub async fn forget_device(&self, device: DeviceId) -> Result<ForgetReport>;
}
```

```c
int32_t shio_sync_forget_device(shio_kernel *handle,
                                const uint8_t *device_id,
                                uint8_t **out_json,
                                size_t *out_json_len);
```

摘要 JSON：

```json
{"device":"32hex","peer_cursor":true,"current_key":true,"progress":true,"acks":1,"migrations":2}
```

增量符号，`SHIO_ABI_VERSION` 保持 1。

## 实现落点

- 类型 `ForgetReport` 放 `shio-core/src/sync.rs`（与 `ApplyReport` 同处），核心根重导出。
- 实现放 `engine_sync.rs`，紧邻 `apply_peer_progress` / `compact_changes_auto`；单个写事务，
  `ack/` 与 `mig/` 按键前缀扫描后按尾部 16 字节匹配设备 ID。
- facade 在 `kernel.rs`（`#[cfg(feature = "sync")]`），C ABI 在 `ffi.rs` + `shio.h`。
- harness 增加 `forget` 模式。

## 验收

- 三设备（A、B、C）：B 上报进度、C 不上报时 `compact_changes_auto() == 0`；`forget_device(C)`
  后返回报告包含 `current_key`/`progress`，再次裁剪返回非 0。
- 解绑本机返回 `InvalidArgument`；未知设备返回全零报告；连续解绑幂等。
- 轮换场景：B 安装 offer 并 ack 后，A 的 `ack/<to>/B` 存在；`forget_device(B)` 清掉它，
  `removed_acks >= 1`；迁移公告同理清掉 `mig/`。
- 未启用同步返回 `SyncDisabled`；只读返回 `ReadOnly`；维护窗口返回 `Busy`。
- FFI：符号往返、摘要 JSON、错误码；facade：async 包装与 `compact_changes_auto` 联动。
- Android harness：`forget` 模式（A/B/C 三库，裁剪先 0 后非 0，报告字段正确），
  x86_64 模拟器与 arm64 真机；同轮回归既有模式。
- `cargo test --workspace --all-features`、`fmt --check`、`clippy -D warnings`、`cc -fsyntax-only` 通过。
- 文档：`api.md` §4/§8/§9/§12、`data.md` §15.5、`notes.md`（用法与「不是拉黑」的边界）、
  `阶段计划.md`、`STATE.md`、决策 1 条。
