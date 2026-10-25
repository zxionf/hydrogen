# 20261025-maintenance-mode

<!-- 维护模式：把长事务迁移拆成可让出写锁的步骤，应用在步骤之间继续读写。 -->

## 维护模式范围

当前 `migrate_all(_with_options)` 从取证到收尾全程持有会话写锁，迁移期间应用的读写都被阻塞；只有进度查询与取消走旁路。方案 B 第二步把「一次调用跑完全部记录」改成「调用方驱动、每次一批」，并显式声明一个维护窗口。

本次范围：

- 维护模式的进入/退出与期间读写策略
- 单一分步迁移入口 `migrate_step`，复用已持久化的迁移游标
- 对应的 facade 方法与增量 C ABI

不在本次范围：

- 完全合作式调度（自动让出、与后台任务协商配额、跨进程协调）
- 物理文件压缩、自动日志压缩
- 多设备备份/克隆、导入导出
- `format=1` 的轮换路径

不改记录格式、密钥层级与 `format=2` 布局；只增加旁路状态、错误语义与增量符号。

## 维护模式

### 进入与退出

- `Engine::begin_maintenance()` 置进程内标志；facade 提供同名 async 方法（`spawn_blocking`）。
- 进入与退出都会短暂获取会话写锁，与正在执行的迁移串行化：若连续迁移已在跑，进入会等它结束或取消后才生效；除此之外两者不做额外阻塞。
- 幂等：重复进入返回成功，不改变已有状态。
- 拒绝条件：只读句柄返回 `ReadOnly`；未启用同步返回 `SyncDisabled`；句柄已关闭返回 `Closed`。
- `Engine::end_maintenance()` 清除标志并返回 `MaintenanceOutcome { done }`：`done=false` 表示迁移未完成，游标保留，调用方决定继续步进还是放弃；重复退出同样成功。
- 标志只在进程内、不落盘。崩溃或重开后库回到普通模式，写操作不被拒绝，游标仍在，后续 `migrate_step` 从游标续跑。这就是失败恢复的语义：维护窗口只约束当前进程，不制造新的持久状态。
- 维护模式不跨进程：`.lock` 仍是唯一的多进程互斥手段，另一进程只读打开不受影响。

### 维护期间的读写策略

- 读操作照常：`get`、`list`、`stats`、`sync_info`、`snapshot_page`、`changes_since` 不受影响。
- 写操作返回 `Busy`（C ABI 为 `SHIO_BUSY`）：`insert`、`put`、`delete`、`batch`、`change_password`、`rotate_data_key`、`enable_sync`、`apply_page`、`begin_rotation`、`acknowledge_rotation`、`retire_key`、`compact_changes`。
- `purge` 是写操作，维护期间返回 `Busy`；同步启用时仍优先返回 `SyncEnabled`。
- 闸门放在 `Engine::require_writable` 之后新增的维护判断里，覆盖全部写入口；步进迁移本身用私有入口绕开闸门，不会自锁或挡掉自身的写入。
- 选择「拒绝」而不是「排队」的理由：排队会把调用线程按住，Android 侧等于把 UI 线程拖进长等待；返回值可预测，应用可以把编辑暂存到内存或提示稍后再试。

### 分步迁移

- `Engine::migrate_step(to, max_records) -> MigrationStep { scanned, migrated, done }`。
- 一次调用只处理一批：取写锁 → 读游标 → 重加密至多 `max_records` 条 → 同事务写入游标 → 提交 → 释放锁。
- `max_records` 为 0 时取默认 256；上限 1024；越界返回 `InvalidArgument`。
- 最后一批之后的收尾（扫描引用、为不再被引用的旧 key 追加 `Migrated` 公告、删除游标）在同一调用内完成，并返回 `done=true`。
- 收尾与最后一批不在同一事务：若收尾前中断，游标仍指向末尾，重跑一次 `migrate_step` 即完成收尾。
- 需要在 `to` 未安装时返回 `InvalidArgument`；`to` 等于当前 key 且无游标时直接完成收尾并返回 `done=true`。
- 连续模式 `migrate_all(_with_options)` 保持现状；两者共享游标，可以混用：连续迁移被取消后可用步进续跑，步进中途也可改用连续模式跑完。
- `cancel_migration` 在步进模式下的作用边界是「正在执行的这一批」：请求取消后当前批结束即返回 `Cancelled`，游标保留，之后仍可继续步进；批次内语义与现状一致。

### 进度与观测

- 进度 JSON 沿用 `active`、`scanned`、`migrated`、`done`；步进模式下 `active` 只在单次调用期间为真。
- 不新增持久化字段；游标布局沿用 `shio.sync` 的 `b"migration"`（`to16 | u16 cursor_len | cursor`）。

## C ABI 形态

```c
int32_t shio_sync_begin_maintenance(shio_kernel *handle);

int32_t shio_sync_end_maintenance(shio_kernel *handle, uint8_t *out_done);

int32_t shio_sync_migrate_step(shio_kernel *handle,
                               const uint8_t to[16],
                               uint32_t max_records,
                               uint8_t **out_json,
                               size_t *out_json_len);
```

- 三者都是阻塞调用；进入/退出在无并发迁移时立即返回，有连续迁移在执行时先等它到批次边界结束（进入前也可先调 `shio_sync_cancel_migration`）。
- `out_done` 可为 `NULL`；`migrate_step` 返回 JSON `{"scanned":N,"migrated":N,"done":bool}`。
- 维护期间普通写返回 `SHIO_BUSY`；未启用同步返回 `SHIO_SYNC_DISABLED`；只读句柄返回 `SHIO_READ_ONLY`。
- 均为增量符号，`SHIO_ABI_VERSION` 保持 1。

## 兼容性与版本

- 包版本与 `format` 不变；`format=1` 库没有同步状态，维护模式直接返回 `SyncDisabled`。
- 现有轮换接口语义不变：`shio_sync_migrate_all_ex` 仍是连续迁移入口，维护模式是额外的可选路径。
- 维护模式标志不跨句柄、不跨进程；同进程重复进入/退出安全。
- 错误面不扩张：维护期间的写操作复用现有 `Busy`，不新增错误变体；C 侧仍是 `SHIO_BUSY`。
- `migrate_step` 与 `migrate_all` 共用 `MigrationState` 的进度与取消原子量，因此进度查询、取消接口无需新增。
