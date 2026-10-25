# 20261025-multi-device-clone

<!-- 从备份克隆出同库的新设备：换 device_id、清理设备级同步状态、按新身份重建完整日志。 -->

## 背景

S2 的 `backup`/`restore` 保留 `db_id`、`device_id`、key 链与全部同步游标，语义是
**同一台设备的灾难恢复**。把同一个备份恢复到第二台设备并同时启用同步会冲突：

- 两台设备共享 `device_id`，`cur/`、`heard/`、`peer/` 与日志条目里的 `writer` 全部重叠；
- 对端会根据 `heard/<device>` 判断同一设备的进度，两个实体互相覆盖；
- 自动日志裁剪与轮换 ack 都会把两台机器当成一台。

多设备克隆要解决的是另一个问题：**同一个逻辑库，另一台设备**。所以它不能复用
`restore` 的身份，必须在副本上生成新身份并重建日志。

## 本阶段范围

- 新增 `Engine::clone_backup`：从备份文件恢复出一个新设备身份的库。
- 保留：`db_id`、key 链、记录、目录、`meta`（含口令）。
- 新建：随机非零 `device_id`、`enabled_at`、`log_start = 1`。
- 清理：`peer/*`、`cur/*`、`heard/*`、`migration`、`baseline_key`。
- 保留：`rot/*`、`ack/*`、`mig/*`、`upgrade`（key 事实，不是本设备的消费进度）。
- 以新身份重建**完整**日志（全部记录 + `CurrentKey`），使克隆本身成为完整的同步源。
- 提供 C ABI `shio_clone_backup` 与 facade `clone_backup`。

不在本次范围：换口令/重新加密、在线跨进程复制、设备解绑或两库合并、明文导出。

## 语义

输入：备份文件、目标路径（不存在或 0 字节）、口令、`EngineOptions`。

1. 校验目标，复制到目标同目录的临时文件。
2. 用口令打开临时库（走完整校验：MAC、meta、key check）。
3. 若已启用同步（`shio.sync` 含 `db`/`dev`）：
   - 生成新的非零 `device_id`，写 `dev`、`enabled_at = now`、`log_start = 1`、
     `baseline_done = 0`；
   - 删除 `peer/*`、`cur/*`、`heard/*`、`migration`、`baseline_key`；
   - 清空 `shio.changes`；
   - 复用 `enable_sync` 的基线流程：按记录键序分批追加全部记录（`writer` = 新设备，
     blob 原样），最后追加 `CurrentKey` 与 `cur/<新设备>`，`baseline_done = 1`。
4. 未启用同步（`format=1`）：没有 `db_id`/`device_id` 可重置，只做普通复制；
   报告里同步字段为空，之后 `enable_sync` 会照常生成新身份。
5. 原子改名到目标，重新打开并返回 `(Engine, CloneInfo)`。

失败路径与 `restore` 一致：临时文件由 `TempPath` 在 `Drop` 时删除，不留下目标或半成品。

## 为什么保留 key 相关状态

`rot/` 是仍未剪掉的轮换 offer，`ack/` 是「某设备已安装某个 key」，`mig/` 是
「某设备已无某个 key 的记录」。这些描述的是**库与 key 的事实**，不是本设备的同步进度：

- 清掉 `rot/` 后，缺新 key 的对端会收到 `RotationRequired`，却无法再从这里取 offer；
- 清掉 `ack/` 会让 `prune_rotation_offers` 失去依据；
- `mig/` 影响 `retire_key` 的安全判断。

`upgrade`（format 1→2 的数据库级迁移游标）同理保留。

相反，`cur/` 虽然是「某设备公告的当前 key」，但它同时是 `known_devices` 的来源，
会参与自动日志裁剪的门控。克隆清掉后只剩自己，等对端重新上报，符合「新设备刚开始
同步」的事实。

## 为什么重建完整日志

配对包只携带身份与 key，不携带记录；新设备拿到数据靠拉日志或快照。如果克隆的日志
只有一条 `CurrentKey`，新配对设备拉完日志仍没有记录，会静默得到空库。沿用
`run_baseline` 的全部记录条目可以保证：

- 新设备只靠日志就能拿到全量数据；
- 老设备拉克隆日志时，相同记录按 LWW `(updated_at, rev, blob)` 被判为旧版本而跳过。

记录 blob 的 header 不含 `writer`，日志条目的 `writer` 只表示发布者，因此重基线
不会改变任何冲突结果。

代价是克隆会写 `记录数 + 1` 条日志，大库有一次性 O(记录数) 写入；这与首次
`enable_sync` 的成本相同。

## 阶段 API

```rust
// shio-core
pub struct CloneInfo {
    /// 源库格式版本。
    pub format_version: u16,
    /// 克隆文件字节数。
    pub db_bytes: u64,
    /// 逻辑库 ID；未启用同步时为 None。
    pub db_id: Option<[u8; 16]>,
    /// 源设备 ID；未启用同步时为 None。
    pub previous_device_id: Option<DeviceId>,
    /// 克隆的新设备 ID；未启用同步时为 None。
    pub device_id: Option<DeviceId>,
    /// 被清理的 `peer/` 数量。
    pub peer_count: u64,
    /// 重建后的日志条数；未重置日志时为 0。
    pub log_entries: u64,
}

impl Engine {
    /// 从备份克隆到新设备；目标必须不存在或为空。
    pub fn clone_backup(
        backup: &Path,
        options: EngineOptions,
        password: &[u8],
    ) -> Result<(Engine, CloneInfo)>;
}

// facade
pub async fn clone_backup(
    options: OpenOptions,
    credential: Credential,
    backup: PathBuf,
) -> Result<(Kernel, CloneInfo)>;
```

```c
int32_t shio_clone_backup(const char *backup_path,
                          const char *target_path,
                          const uint8_t *password,
                          size_t password_len,
                          const uint8_t *options_json,
                          size_t options_len,
                          shio_kernel **out_handle,
                          uint8_t **out_json,
                          size_t *out_json_len);
```

`out_json` 为
`{"format_version":N,"db_bytes":N,"db_id":"…","previous_device_id":"…","device_id":"…","peer_count":N,"log_entries":N}`；
未启用同步时后四项为 `null` / 0。

新增符号是增量扩展，`SHIO_ABI_VERSION` 保持 1。`shio_restore` 语义不变。

## 验收

- core：
  - 克隆后 `device_id` 与源不同、`db_id` 相同；
  - `log_start = 1`、`baseline_done`、`peer_cursor(旧对端) = None`、`known_devices` 只含自己；
  - 记录数与源一致且可读；
  - 新配对设备只拉克隆日志即可拿到全部记录；
  - 旧设备用原有 `peer` 游标拉克隆日志不报错（记录被 LWW 跳过），克隆新写的记录能被拉到；
  - 目标非空、备份缺失、口令错误返回既有错误且不留临时文件；
  - 未启用同步的库克隆后仍是 `SyncDisabled`，记录可读。
- FFI：符号往返、JSON 字段、错误码；facade：`Kernel` 返回与关闭。
- Android harness：`clone` 模式（源库启用同步并写记录 → 备份 → 克隆 → 校验身份/日志/记录），
  双 ABI 上机；同轮回归既有模式。
