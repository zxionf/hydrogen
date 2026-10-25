# 20261025-cooperative-scheduling

<!-- 合作式调度：按时间片推进迁移，每次调用只占一小段写锁，应用在调用间隙正常读写。 -->

## 合作式调度范围

当前三个迁移入口的能力边界：

- `migrate_all(_with_options)`：从取证到收尾全程持有会话写锁，期间应用读写都被阻塞。
- `migrate_step(to, max_records)`：一批一锁，但批次大小由调用方按记录数控制，不感知耗时；应用要自己决定调多少次。
- 维护模式：把「写冻结」显式化，适合必须停写的窗口。

本次增加第四个入口 `migrate_tick`：一次调用在给定时间预算内连续跑若干个小批次，提交即释放；调用间隙应用正常读写，**不需要维护模式、不冻结写**。

本次范围：

- core 的 `migrate_tick(to, budget, max_records)`，复用已有游标、批次与收尾逻辑
- facade（`Kernel` / `SyncHandle`）与增量 C ABI
- 进度计数在「迁移序列」开始时的清零规则

不在本次范围：

- 内核隐藏线程、后台服务或 Android 前台服务绑定
- 自动感知前台/后台、电量或温度并调整配额
- 跨进程协调迁移节奏
- 百分比进度（仍不统计总记录数）
- 物理文件压缩、日志压缩、多设备克隆、导入导出

不改记录格式、密钥层级与 `format=2` 布局。

## 调度语义

### 时间片与批次

- 一次 tick 由两个配额共同约束：时间预算（默认 10 ms，上限 1 s）与单批记录上限（默认 32，上限 256）。
- 循环体：读一批 → 重加密并推进游标 → 提交 → 检查预算。预算耗尽立即返回 `done=false`，会话写锁随调用结束释放。
- 预算检查放在批次之间、不放第一批之前：每次 tick 至少推进一批，避免小预算下空转。
- 读到不足 `max_records` 的一批即视为扫描结束：同一次调用内执行收尾（扫引用、公告 `Migrated`、删游标）并返回 `done=true`。收尾不受预算约束。
- 默认批次上限比 `migrate_step` 的 1024 小一个量级，因为这里优化的是单事务时长，而不是调用次数。

### 写入与锁

- tick 不要求先进入维护模式；调用间隙普通读写照常。
- tick 只在单次调用期间持有会话写锁，时长约为「预算 + 最后一批耗时」；与 `migrate_all` / `migrate_step` 通过同一把锁自然串行化，三种入口可混用。
- 并发安全：轮换开始后会话当前 key 已是 `to`，应用新写入天然落在目标 key，扫描按 `key_id == to` 跳过；批内用 redb 写事务与应用写串行化，不会丢失更新。
- 维护窗口内也可以调用 tick（内部走放行路径），此时普通写仍按维护模式返回 `Busy`。
- `retire_key` 自带「仍有记录引用该 key 则返回 `Busy`」的检查，tick 途中误调用不会删掉仍在使用的旧 key。

### 取消与失败恢复

- `cancel_migration()` 在批次之间检查：正在执行的 tick 返回 `Cancelled`，已提交批次与游标保留。
- 每次 tick 开始时清除旧的取消标志，与 `migrate_step` 一致：取消只对正在执行的这一次调用有效。
- 停止调度就是调用方不再调用 tick，不需要额外状态或退出流程。
- 崩溃或重开后游标仍在，下一次 tick 从游标继续；维护标志本来就不落盘，与此互不影响。

### 进度

- `migration_progress` 的 `scanned` / `migrated` 在一个迁移序列内累计；序列开始时（目标 key 没有对应游标）清零。
- 单次 tick 的返回值只统计本次调用；`active` 只在调用期间为真。
- 仍不承诺百分比，也不统计剩余条数。

## 常量

```rust
pub const DEFAULT_TICK_BUDGET_MS: u64 = 10;
pub const MAX_TICK_BUDGET_MS: u64 = 1_000;
pub const DEFAULT_TICK_BATCH_SIZE: usize = 32;
pub const MAX_TICK_BATCH_SIZE: usize = 256;
```

- 时间预算在 facade 侧用 `Duration` 表达，C ABI 用毫秒整数；`0` 一律取默认值，超上限返回 `InvalidArgument`。
- `max_records` 为 `0` 取默认值，超上限返回 `InvalidArgument`。

## C ABI 形态

```c
int32_t shio_sync_migrate_tick(shio_kernel *handle,
                               const uint8_t *to,
                               uint32_t budget_ms,
                               uint32_t max_records,
                               uint8_t **out_json,
                               size_t *out_json_len);
```

- 返回 `{"scanned":N,"migrated":N,"done":bool}`；`done=1` 表示已收尾。
- `budget_ms=0` 取默认 10，上限 1000；`max_records=0` 取默认 32，上限 256。
- 只读句柄返回 `SHIO_READ_ONLY`；未启用同步返回 `SHIO_SYNC_DISABLED`；目标 key 未安装或配额越界返回 `SHIO_INVALID_ARGUMENT`；取消返回 `SHIO_CANCELLED`。
- 增量符号，`SHIO_ABI_VERSION` 保持 1。

## 与维护模式的关系

- 维护模式是**强窗口**：普通写冻结，应用自己循环 `migrate_step` / `migrate_tick`，退出时清除标志。
- 合作式调度是**弱窗口**：不冻结写，应用在自己方便的时候调用 tick；不调用就等于暂停。
- 两者可组合（进入维护 → tick 循环 → 退出），也可以完全不用维护模式。
- 决策 #51（维护窗口内写返回 `Busy`）保持不变：tick 没有引入新的写策略，只是在没有维护窗口时也能推进迁移。

## 兼容性与版本

- 包版本与 `format` 不变；`format=1` 库没有同步状态，tick 返回 `SyncDisabled`。
- `migrate_all`、`migrate_step`、`migrate_tick` 共用持久化游标与 `MigrationState` 原子量，可任意混用。
- 现有 C ABI 符号签名与语义都不变，只追加 `shio_sync_migrate_tick`。
