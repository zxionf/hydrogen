# 20261022-sync-design

## Context

`shio-core` 与 facade 已可用，`format=1` 预留了 `shio.changes`、墓碑与多 key 行。现在需要把「多端同步」从占位变成可评审的设计：设备如何配对、增量如何表达、冲突如何收敛、任意设备轮换如何不撞 key_id、旧二进制如何被强制拦下。

本变更只产出设计，不改实现。`shio-sync` 保持**传输无关**：只生产/消费字节，不引入 socket、TLS 或后台线程。

## 目标与非目标

目标：

1. 两台及以上设备共享同一逻辑数据库，离线写入后能增量收敛。
2. 删除通过墓碑传播，旧副本不会复活已删除记录。
3. 配对只转移一次密钥链，之后各设备用自己的口令解锁本地库。
4. **任意设备**都能发起轮换，不需要协调者、不需要重新配对。
5. 同步库使用 `format=2`；旧二进制因主格式未知而拒绝打开，避免静默掉队。
6. 任意中断点可重试，重复应用幂等。

非目标（本版）：

- 网络传输、发现、中继、账号体系。
- 多写者同时写同一数据库文件（仍是单写多读）。
- 撤销单台设备的读取能力：同步层没有访问控制，需要传输层拒绝 + 轮换 + 迁移退役。
- 字段级合并、向量时钟、OT/CRDT 文本合并。
- 服务端聚合或中心化权威。

## 总体模型

```
设备 A (db_id=D, device_id=A, keys={K1,K2a}, log=1..n)
  记录表 + 变更日志 ──changes_since(cursor)──▶ 设备 B (keys={K1,K2b}, log=1..m)
  记录表 + 变更日志 ◀──changes_since(cursor)── 记录表 + 变更日志
```

- **逻辑数据库**由 16 字节 `db_id` 标识，配对时从旧设备带到新设备。
- **设备**由 16 字节 `device_id` 标识，首次启用同步时随机生成。
- **密钥**由 16 字节随机 `key_id` 标识；轮换产生新的随机 id，因此任意设备并发发起也不会撞号。
- 每次本地写入在**同一 redb 写事务**内追加一条变更日志，保证「记录已变但日志没写」不会发生。
- 拉取方按游标读取对方日志；应用远端变更时认证后原样写入，并追加本地日志，支持 A→B→C 多跳。
- 冲突用全序 `(updated_at, rev, blob 字节序)`；写 key 的收敛用另一条全序 `(key.created_at, key_id)`。

## 格式 2

同步库把主格式升到 `format=2`。新二进制同时支持 1 与 2：

- `format=1`：现有布局（`key_id` 为 u16），不支持同步；`enable_sync` 会一次性升级。
- `format=2`：`key_id` 为 16 字节，新增同步表；所有新记录、新 key 行、新 meta 都使用 v2 布局。
- 旧二进制只认识 `format=1`，打开 `format=2` 时在 meta 校验阶段直接拒绝；这正是「旧写者强制掉队」的实现方式。

KDF 链保持不变（同一个 DK 派生同样的 `rk` / `ck`），变化只在**域分隔、blob 布局与 key_id 宽度**。升级期间新旧记录并存，靠每条记录的 `format` 字节区分。

### MetaBlob v2

```
偏移  长度  字段
0     4     magic = "SHIO"
4     2     format = 2
6     16    key_id（当前写 key）
22    8     created_at
30    1     app_id_len
31    n     app_id
31+n  24    key_check_nonce
55+n  32    key_check_ct
```

- key check 的明文改为 `"shio-keycheck-v2"`，AAD 为 `"shio/v2/keycheck"`。
- 未知 `format` 拒绝打开；`format=1` 走旧解码器。

### KeyBlob v2

v2 的 key 行存放在新表 `shio.keys2`（键为 16 字节 `key_id`）；`shio.keys` 保留给 v1，升级完成后清空。值：

```
0     16    key_id（与键一致）
16    1     kdf_id
17    1     flags
18    4     m_cost_kib
22    4     t_cost
26    4     p_cost
30    16    salt
46    24    wrap_nonce
70    48    wrapped_dk
118   8     created_at（Unix 毫秒）
126   1     label_len
127   n     label
```

- 包裹 AAD 为 `"shio/v2/keywrap" || key_id16`。
- 同一数据库的所有 key 行共用同一 KEK（同一 salt/参数），否则打开时无法统一解包。

### RecordBlob v2

键 `RecordKey` 不变：

```
u8 ns_len | ns | u8 coll_len | coll | 16 字节 UUID
```

值：

```
0     4     magic = "SHR2"
4     1     format = 2
5     1     flags（bit0 = deleted）
6     16    key_id
22    8     rev
30    8     created_at
38    8     updated_at
46    2     data_version
48    24    nonce
72    4     ct_len
76    n     ciphertext
```

- header 为 `0..48`；`76 + ct_len == value.len()`。
- AAD 为 `"shio/v2/record" || ns/coll/id || header[0..48]`。
- `format=1` 的 `SHR1` 记录仍按 v1 解码与 v1 AAD 验证，直到升级完成。

### 升级（format 1 → 2）

`enable_sync` 在 `format=1` 上执行一次可恢复升级：

1. 在**一个事务**里创建 `shio.keys2`、`shio.sync` 与同步表，写 `upgrade` 状态、旧 u16 key_id 到 16 字节新 id 的映射与 `shio.keys2` 行（同一 KEK 与 DK），并写 `MetaBlob v2`（format=2、新当前 key、v2 key check）。
2. 提交后旧二进制立即无法打开；新二进制在 meta=2 下混合解码 `SHR1` / `SHR2`。
3. 按记录键序分批重加密：解码 `SHR1`（v1 AAD）→ 写 `SHR2`（v2 AAD、新 key_id、新 nonce），每 256 条一个事务。
4. 全部完成后删除 `shio.keys` 旧行与映射，清空升级状态。
5. 崩溃恢复：meta 已是 2 时，新二进制按每条记录的 `format` 混合读取（`SHR1` 用旧 key 行、映射与 v1 AAD），并按 `upgrade` 进度续跑；升级不可回退。
6. 升级期间 `change_password` 与 `rotate_data_key` 返回 `Busy`，避免旧 key 行的 KEK 与迁移进度不一致。

## 新增表与字节格式

新增两张表，均为**追加式**，不影响旧表：

| 表名 | 键 | 值 | 用途 |
|---|---|---|---|
| `shio.changes` | `u64` 大端本地序号 | `ChangeBlob` | 本地变更日志 |
| `shio.sync` | UTF-8 字符串 | 按字段 | 同步状态与升级进度 |

### shio.sync

键为**字节串**（前缀 + 16 字节 id），值按字段：

| 键 | 值 | 说明 |
|---|---|---|
| `b"v"` | `u16` 大端 = 1 | 同步状态版本 |
| `b"db"` | 16 字节 | 逻辑数据库 ID |
| `b"dev"` | 16 字节 | 本设备 ID |
| `b"enabled_at"` | `i64` 大端 | 启用时间 |
| `b"baseline_done"` | `u8`（0/1） | 首次全量建日志是否完成 |
| `b"baseline_key"` | 记录键字节 | 基线扫描进度 |
| `b"log_start"` | `u64` 大端 | 当前保留的最早序号 |
| `b"peer/" + dev16` | `u64` 大端 | 已消费的对方日志序号 |
| `b"rot/" + to16` | `ChangeBlob`（kind=1） | 轮换 offer 持久副本 |
| `b"ack/" + to16 + dev16` | `u8` = 1 | 已收到该设备对 `to` 的 ack |
| `b"cur/" + dev16` | `key_id16` | 该设备最近公告的当前写 key |
| `b"mig/" + key16 + dev16` | `u8` = 1 | 该设备已无 `key` 记录 |
| `b"upgrade"` | 阶段 + 进度 | format 1→2 的可恢复状态 |
| `b"upgrade/key/" + u16` | `key_id16` | 旧 key_id 到新 id 的映射 |

- 二进制键避免 hex 膨胀，也避开名称长度限制；表只被同步代码读取。

### ChangeBlob v2

所有条目共用 8 字节头：

```
0     4     magic = "SHC2"
4     1     format = 2
5     1     kind（0 记录，1 轮换 offer，2 acks，3 当前 key 公告，4 迁移完成）
6     1     flags（仅 kind=0 使用：bit0 = deleted）
7     1     reserved = 0
```

`kind=0` 记录 body：

```
0     16    key_id
16    8     rev
24    8     updated_at
32    16    writer（最初写这条记录的 device_id）
48    2     key_len
50    n     record_key
50+n  4     blob_len
54+n  m     RecordBlob 原始字节
```

`kind=1` 轮换 offer body：

```
0     16    from_key_id
16    16    to_key_id（随机，不与现有 key 重复）
32    8     created_at
40    16    origin（发起设备）
56    24    wrap_nonce
80    4     ct_len
84    m     RK 包裹的 DK_to（32B + 16B tag）
```

`kind=2` ack body：

```
0     16    to_key_id
16    16    device_id
32    8     at
```

`kind=3` 当前 key 公告 body：

```
0     16    key_id
16    8     at
```

`kind=4` 迁移完成 body：

```
0     16    key_id（本机已无该 key 的记录）
16    8     at
```

- `kind=0` 的 `key_id` / `rev` / `updated_at` / `flags.deleted` 必须与解出的 `RecordBlob.header` 一致，否则 `Corrupted`；`key_id` 必须存在于本机 `shio.keys2`（升级期间也可能是 `shio.keys`）。
- 日志条目携带完整密文，保证「按游标拉到的内容」与「当时提交的记录」一致；代价是写放大，由压缩回收。
- 控制条目（kind=1/2/3/4）只改同步状态与密钥行，不写记录表；它们与记录共用日志序号，因此 offer 一定排在用新 key 写入的记录之前。

### 帧（wire）

```
偏移  长度  字段
0     4     frame_len（不含这 4 字节）
4     4     magic = "SHY2"
8     1     format = 2
9     1     frame_kind（0 变更页，1 offer 页，2 快照页）
10    1     flags（保留，0）
11    16    db_id
27    16    sender
43    8     after（变更页为日志游标；offer 页为 0）
51    8     log_start
59    8     log_end
67    8     next
75    2     count
77    ...   count ×（u32 len + ChangeBlob 或快照条目）
```

- `frame_kind=0`：条目为日志条目（记录或控制），序号语义同 `after/next/log_start/log_end`。
- `frame_kind=1`：条目为 `kind=1` offer，按 `created_at` 升序；`after` 保留为 0。
- `frame_kind=2`：条目为 `record_key + RecordBlob`；具体布局在 M2 实现时补齐。
- 解码先校验 `frame_len`、`magic`、`format`、`frame_kind`、`db_id` 与 `count`，再逐条校验长度；不分配未验证的巨大缓冲。
- 单帧上限默认 4 MiB、`count ≤ 1000`；调用方可调小。
- 帧格式与数据库 `format` 独立演进；未知 `format` 返回 `UnsupportedFormat`。

### 配对包

明文（仅在内存中短暂存在）：

```
"SHB2" | format=2 | db_id 16 | current_key_id 16
       | app_id_len 1 | app_id | created_at 8
       | key_count 1 | key_count ×（key_id 16 + DK 32）
```

外层：

```
"SHW2" | kdf_id 1 | m_cost 4 | t_cost 4 | p_cost 4
       | salt 16 | nonce 24 | ct_len 4 | ciphertext
```

- 携带**整条密钥链**：新设备必须能解密快照里所有历史 key 的记录。
- `key_count` 上限 32；达到上限时 `begin_rotation` 拒绝，要求先 `migrate_all` + `retire`。
- `pairing_secret` 为 32 字节 CSPRNG，Base32（Crockford）显示；二维码编码外层包，另显示 6 位指纹（外层包 SHA-256 前 3 字节）供人工比对。
- 有效期默认 10 分钟；接收方拒绝 `created_at` 超出 ±10 分钟的包。包是一次性的（发送方不强制）。
- 新设备用新 salt + 新口令派生 KEK，逐行包裹所有 DK，写 `shio.keys2`；meta 写 `format=2`、`key_id = current_key_id`；`shio.sync` 写 `db_id`、新 `device_id`、`baseline_done=1`，并追加 `kind=3` 公告与 `cur/<dev>`。

## 本地写入与变更日志

- `enable_sync(device_id?)`：
  1. `format=1` 或存在未清空的 `upgrade` 状态时，先执行/续跑升级；已有同步状态则直接返回。
  2. 生成/沿用 `db_id`，生成 `device_id`。
  3. 对已有记录做基线扫描，每 256 条一个事务，记录进度；完成后写 `baseline_done=1`。
  4. 追加 `kind=3` 公告当前 key，写 `cur/<dev>`；这样其他设备能把它计入退役判断。
  5. 基线期间持有会话写锁，阻塞业务写入。
- 启用后，`insert`/`put`/`delete`/`batch` 在提交记录的同时追加 `ChangeBlob(kind=0)`；序号 `log_end+1`，同事务。
- 当前写 key 变化时（本地轮换或采纳更高 key）追加 `kind=3` 公告，并更新 `shio.sync` 的 `cur/<dev>`。
- `purge` 物理删除会丢墓碑，启用同步后返回 `Error::SyncEnabled`。
- `change_password` 在同步模式下重包裹**所有** key 行（同一新 salt 与 KEK、各自 nonce，单事务）；独立 `format=1` 仍要求单行。
- 旧二进制无法打开 `format=2`；不存在「旧写者静默掉队」。

## 拉取、快照与游标

- `changes_since(after, limit)`：
  - `after == 0` 表示从头拉。
  - `after + 1 < log_start` → `ResyncRequired`，附带 `log_start` 与 `log_end`。
  - `after > log_end` → `InvalidArgument`。
  - 返回 `next`、`has_more`、`log_start`、`log_end`。
- 游标按**发送方**日志序号解释，不跨设备比较。
- 应用页在**一个写事务**内：校验 → 逐条比较 → 写入 → 追加本地日志 → 更新 `peer/<sender>`；控制条目只更新状态，不转发；失败整页回滚。
- 新设备或游标过期走快照：
  - `snapshot_start()` 只读事务取 `at_seq = log_end`；
  - `snapshot_page(after_key, limit)` 按键序返回 `record_key + RecordBlob`；
  - 应用完后从 `at_seq+1` 续拉；快照期间的新更新也在日志里，重复由全序吸收。
- 快照条目没有 `writer`，统一全零；应用后若需多跳再以本机 `device_id` 记日志。
- 压缩：`compact_changes(before)` 删除 `< before` 的条目并前移 `log_start`；本版只做手动压缩。

## 冲突解决

同一 `RecordKey` 的两份状态用全序：

```
(updated_at, rev, RecordBlob 字节字典序)
```

- 本地新写入总是生效；比较只发生在应用远端变更时。
- 远端严格更大才覆盖；相等或更小跳过（不追加日志，避免回声）。
- 完全相同 blobs 视为相等，天然幂等。
- 删除是 `flags.deleted=1` 的普通状态，参与同一比较；旧更新若 `updated_at` 更大仍会复活，这是 LWW 的已知语义。
- 同步模式下本地写入把 `updated_at` 取 `max(now, 旧记录 updated_at + 1)`，避免本机回拨；跨设备时钟偏差仍可能让快钟占优，部署应启用 NTP。
- 未采用 HLC/向量时钟，留到后续版本。

## 密钥模型与任意设备轮换

### 密钥图

- 每个 key 有 16 字节随机 `key_id` 与 `created_at`。
- 轮换是从 `from` 到 `to` 的有向边；`DK_to` 用 `RK = HKDF(from_DK, salt=db_id, info="shio/v2/rotation-key")` 包裹，AAD 绑定 `db_id || from || to`。
- 任意设备可基于自己的当前 key 发起；并发发起产生不同随机 `to`，因此**不会撞号**，密钥结构是有根 DAG 而不是单链。
- 所有设备最终学会所有 key；记录按自身 `key_id` 解密，因此分支上的记录仍可读。

### 发起（任意设备）

1. 前置：本机无 pending 轮换；key 行数 < 32；`from = meta.key_id` 存在。
2. 生成 16 字节随机 `to`（非零、不与现有 key 重复）与 32 字节 `DK_to`。
3. 包裹 `DK_to`，写 `kind=1` offer。
4. 同一事务：写本地 KeyBlob `to`（本地 KEK）、追加 offer、更新 `meta.key_id = to` 与 key check、写 `shio.sync` 的 `cur/<dev>`、追加 `kind=3` 公告。
5. 提交后才允许用 `DK_to` 写记录。

### 安装（任意 peer）

应用日志页或 offer 页遇到 `kind=1` 时，在同一事务内：

1. 校验 `from` 已安装、`to` 不与现有 key 冲突、AAD 与 `db_id` 匹配；缺 `from` → `RotationRequired { from }`。
2. 用 `from_DK` 派生 `RK`，解包 `DK_to`（认证失败 → `Corrupted`）。
3. 用本机 KEK（沿用现有 salt/参数）包裹 `DK_to`，写 KeyBlob；加入会话 key map。
4. 按 `(created_at, key_id)` 比较：若 `to` 更大，更新 `meta.key_id = to` 与 key check，并追加 `kind=3` 公告；否则只安装、不切写 key。
5. 追加 `kind=2` ack；同事务。

要点：

- 安装不重写记录；多 key 会话按 header 解密。
- 并发轮换产生两个 `to`，两个都会安装；当前写 key 收敛到全序最大者，旧分支的记录仍可读。
- 若先遇到新 key 的记录而本机缺 key，返回 `RotationRequired { key_id }`，补齐 offer 后重试；不把未知 key 误判为 `Corrupted`。
- 分支深度不受限制，但 key 行数受 32 上限约束。

### 收敛与公告

- 每次 `meta.key_id` 变化都追加 `kind=3`，并写 `cur/<dev>`；对端拉取后更新本地 `cur/<sender>`。
- 采纳规则只依赖 `(created_at, key_id)`，不依赖到达顺序；时钟偏差只影响「谁当写 key」，不影响可读性。
- 由于任意设备可独立轮换，不会出现「必须某台设备在线才能轮换」。

### 迁移、退役与清理

- `migrate_all(to)`：把本机所有非 `to` 记录批量重加密到 `to`，每 256 条一个事务、可中断续跑；完成后对不再被任何记录引用的 key 追加 `kind=4` 迁移完成公告。
- `retire(from)` 删除本机 `from` 行，前置条件：
  1. 本机无 `from` 记录；
  2. 已知的 `cur/<dev>` 中没有设备仍以 `from` 为当前 key；
  3. 每个已知设备都有 `mig/<from>/<dev>`（即都已迁移完成）；
  4. 没有未安装的 offer 以 `from` 为起点。
- 「已知设备」由 `cur/` 条目确定；任何写过记录或发起过轮换的设备都会公告 `kind=3`，因此不会漏掉活跃设备。若某设备长期离线、既未公告也未迁移，退役会被拒绝，这是有意的保守行为。
- `retire` 是全局安全操作：只应在确认所有设备在线、都已执行 `migrate_all` 后调用。新配对设备应在首次同步后执行一次空 `migrate_all`，让其 `mig/` 公告补齐；否则退役会被拒绝。
- `prune_rotation_offers(to)`：删除 `rot/<to>`；需所有已知设备已 ack。
- 默认**软轮换**：切换写 key、保留旧行；退役与清理都显式调用。这样旧密文与离线设备不会被自动破坏。

### 失败与并发

| 中断点 | 结果与恢复 |
|---|---|
| 发起事务前崩溃 | 无变化，重试即可 |
| 发起事务后崩溃 | 本地已切 key；offer 在日志，peer 拉取后安装 |
| peer 安装事务中崩溃 | 单事务全有或全无；重试幂等 |
| peer 已安装未 ack | 重拉 offer 时检测已安装，只补 ack |
| 两台设备并发轮换 | 两个随机 `to` 都有效；安装后按全序收敛写 key |
| 分支上已有记录 | 两把 key 都在，记录都可读；`migrate_all` 可合并到当前 key |
| 缺祖先 key | `RotationRequired`，先装祖先 offer |
| 旧二进制打开 | meta `format=2` 直接拒绝 |

## 公开接口

公开面用 16 字节 `KeyId` / `DeviceId` 新类型表达 id，不再暴露裸数组；这是 `DECISIONS.log` #34 的类型层细化。下面用 `[u8;16]` 表示等价的字节语义。

### shio-core

```rust
pub struct SyncInfo {
    pub db_id: [u8; 16],
    pub device_id: [u8; 16],
    pub current_key_id: [u8; 16],
    pub log_start: u64,
    pub log_end: u64,
    pub baseline_done: bool,
}
pub struct ChangePage { /* sender, entries, after, next, has_more, log_start, log_end */ }
pub struct OfferPage { /* sender, offers */ }
pub struct RotationInfo { /* from, to, created_at, acked devices */ }
pub struct ApplyReport { /* applied, skipped, controls */ }
pub struct SnapshotPage { /* entries, next_key, at_seq */ }

impl Engine {
    pub fn enable_sync(&self, device_id: Option<[u8; 16]>) -> Result<SyncInfo>;
    pub fn sync_info(&self) -> Result<SyncInfo>;
    pub fn changes_since(&self, after: u64, limit: usize) -> Result<ChangePage>;
    pub fn snapshot_start(&self) -> Result<u64>;
    pub fn snapshot_page(&self, after: Option<&[u8]>, limit: usize) -> Result<SnapshotPage>;
    pub fn apply_page(&self, page: &ChangePage) -> Result<ApplyReport>;
    pub fn begin_rotation(&self) -> Result<RotationInfo>;
    pub fn rotation_state(&self) -> Result<Option<RotationInfo>>;
    pub fn rotation_offers(&self) -> Result<Vec<RotationOffer>>;
    pub fn migrate_all(&self, to: [u8; 16]) -> Result<u64>;
    pub fn retire_key(&self, from: [u8; 16]) -> Result<()>;
    pub fn prune_rotation_offers(&self, to: [u8; 16]) -> Result<()>;
    pub fn acknowledge_rotation(&self, to: [u8; 16]) -> Result<()>;
    pub fn compact_changes(&self, before: u64) -> Result<u64>;
    pub fn create_pairing_bundle(&self, ttl: Duration) -> Result<Vec<u8>>;
    pub fn open_with_pairing(options: EngineOptions, password: &[u8], bundle: &[u8], secret: &[u8]) -> Result<Engine>;
}
```

- `Engine` 仍是同步类型；所有方法由 facade 的 `spawn_blocking` 调用。
- 同步模式下 `rotate_data_key` = `begin_rotation` + `migrate_all(to)`，不自动退役；独立 `format=1` 仍走现有全量轮换。
- 新增错误变体：`SyncDisabled`、`SyncEnabled`（禁用 `purge`）、`SyncResyncRequired { log_start }`、`RotationRequired { key_id: [u8;16] }`、`PairingExpired`、`PairingInvalid`。
- key id 对外统一用 16 字节；显示层用 hex 或 UUID。

### shio-sync

```rust
pub struct Sync<'a> { engine: &'a shio_core::Engine }

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
    pub fn migrate_all(&self, to: [u8; 16]) -> Result<u64>;
    pub fn retire(&self, from: [u8; 16]) -> Result<()>;
    pub fn prune_offers(&self, to: [u8; 16]) -> Result<()>;
    pub fn compact(&self, before: u64) -> Result<u64>;
    pub fn pairing_offer(&self, ttl: Duration) -> Result<PairingOffer>;
    pub fn accept_pairing(/* path, password, offer, secret */) -> Result<Engine>;
}
```

- `shio-sync` 只依赖 `shio-core`；不依赖 tokio。
- `PairingOffer { pub frame: Vec<u8>, pub secret: String, pub fingerprint: String }`；`secret` 是 Base32 文本，调用方应移入 `Zeroizing` 并在使用后清零；facade 可直接接收 `Zeroizing<String>`。
- 传输层拿 `frame` 自行发；对端 `apply` 前校验 `db_id`。

### shio facade（可选 feature）

- 新增 `sync` feature，开启后 `shio` 依赖 `shio-sync`：
  - `Kernel::sync_info()`、`Kernel::sync()`（绑定同一 `Arc<Engine>`）；
  - `Kernel::create_pairing_offer(ttl)`；
  - `shio::open_with_pairing(options, credential, offer, secret)`。
- 全部经 `spawn_blocking`；口令与 pairing secret 用 `Zeroizing` 搬运。
- 未开启 feature 时默认依赖与现有 API 不变。

## 失败恢复

| 中断点 | 结果与恢复 |
|---|---|
| 升级 1→2 中崩溃 | 原子切换前无变化；切换后 meta=2、混合读取并续跑；旧二进制已无法打开 |
| 升级写完 meta 后崩溃 | 已是 format=2，升级完成；清理旧行可重入 |
| 基线扫描中崩溃 | `baseline_done=0` + 进度键续跑；重复由全序吸收 |
| 记录提交后崩溃 | 日志同事务，不会缺条 |
| 应用整页前崩溃 | 页未提交、游标未动，重试 |
| 应用页提交后崩溃 | 游标同事务，重复也幂等 |
| 配对中途崩溃 | 目标库未创建或 meta 未提交，不产生半个库 |
| 日志压缩、游标过期 | `ResyncRequired` → 快照 + 从 `at_seq+1` 续拉 |
| 缺 key | `RotationRequired` → 本地/对端补齐 offer 后重试 |
| blob 认证失败 | 整页回滚，返回 `Corrupted` |

## 安全与泄露

- 变更日志与记录表泄露面相同：命名空间、集合、ID、`rev`、时间戳、墓碑、`key_id`、密文长度；不新增明文。
- 帧里是密文，但窃听者可看到「哪条记录、何时、多大」；更强隐藏需另做填充/桶化。
- 配对包含明文 DK（解包后），必须在 `Zeroizing` 缓冲中处理，禁止日志与错误消息。
- 没有传输认证：应用必须用 TLS、二维码或其他可信通道；`db_id` 只防串库，不防伪造。
- 恶意对端可发超大/畸形帧：解码先限额再分配，认证失败一律 `Corrupted`。
- 任一配对设备都持有全部 key，属于完全信任域：可解密全库、伪造记录、伪造 offer；协议不做设备间签名。需要防恶意对端时另做签名/审计链。
- 软轮换下旧 key 保留，旧密文仍可被旧 key 解密；只有 `migrate_all` + `retire` 后旧 key 泄露的影响才收敛。
- 设备撤销需要传输层拒绝 + 轮换 + 迁移 + 退役；本版不提供一键撤销。

## 压缩与容量

- 每条写入产生一条日志，含完整密文，写放大约为 2 倍；压缩后回落。
- `compact_changes(before)` 只删日志，不触碰记录；建议所有对端游标都超过 `before` 后调用。
- 轮换 offer 存于 `shio.sync`，不随日志压缩消失；清理走 `prune_rotation_offers`。
- 本版不自动压缩：自动策略需要知道离线对端游标，容易触发意外全量重同步。

## 测试计划

1. 升级：format 1 库（300 条、含墓碑）升级到 2；每个阶段崩溃后重跑；升级后旧记录可读、可写。
2. 两副本随机插入/更新/删除/批量 → 双向同步 → 一致；再跑一轮无变化。
3. 三副本 A→B→C 与 A→C 双路径，验证多跳与去重。
4. 乱序/重复/同页重复：幂等且收敛。
5. 冲突：同 `updated_at` 不同 `rev`、同 `updated_at/rev` 不同 blob、删除对更新。
6. 游标：`after=0`、空页、`has_more`、`after > log_end`、压缩后 `ResyncRequired`、快照续拉。
7. 基线与崩溃：扫描中断续跑、基线期间禁止写入。
8. 配对：往返、多 key 链、错误 secret、过期、篡改、目标库已存在、重复使用。
9. 轮换：任意设备发起、两台并发发起不同 `to`、分支记录可读、按全序收敛写 key、缺祖先、重复安装、ack 幂等。
10. 轮换迁移与退役：`migrate_all` 跨批续跑、空迁移公告、`retire` 四个前置条件、`prune` 的 ack 约束。
11. 旧二进制拦截：`format=2` 被 v1 解码器拒绝（用格式门测试）。
12. 恶意输入：截断帧、超大 `frame_len`、`count` 不符、blob 认证失败、未知 magic/format。
13. 只读库：`read_only=true` 上启用同步/升级/应用页返回 `ReadOnly`。

## 兼容性与迁移

- `format=1` 继续支持读写（不同步）；`enable_sync` 升级到 `format=2` 后不可回退。
- 升级开始时就写 `format=2` 的 meta，旧二进制立即被拦；新二进制在 meta=2 下混合解码 `SHR1` / `SHR2`，直到迁移完成。`shio.keys` 与 `shio.keys2` 在升级期间并存。
- `shio.changes` / `shio.sync` 对旧版本不可见；旧版本也不会写日志。
- 帧 `format=2`、配对包 `SHW2` 与数据库格式独立；未知版本拒绝。
- 配对创建的新库直接是 `format=2`；`created_at` 用本机时间，`app_id` 沿用，`db_id` 防串库。
- 现有 `api.md` / `data.md` 的 v1 契约保留；实现时新增 format 2 章节与升级说明。

## 里程碑

1. **M0 format 2 与升级**：v2 的 meta/key/record 编解码、AAD、混合读取、`enable_sync` 升级状态机、旧格式拦截；升级崩溃测试。
2. **M1 日志与收敛**：`shio.changes` / `shio.sync`、写入追加、`changes_since`、`apply_page`、记录全序、两副本测试。
3. **M2 快照与压缩**：`snapshot_start/page`、`compact_changes`、`ResyncRequired`、三副本测试。
4. **M3 任意设备轮换**：随机 key id、offer/ack/公告、DAG 安装、写 key 收敛、`migrate_all`、`retire`、`prune`；并发与中断测试。
5. **M4 配对**：`create_pairing_bundle`、`open_with_pairing`、密钥链、二维码/指纹、配对测试。
6. **M5 facade 与文档**：`sync` feature、`Kernel` 透传、示例、SPEC/api/data 折入。

## 已确认与后续

已确认：

- 记录冲突用 LWW 全序 `(updated_at, rev, blob)`。
- facade 以可选 `sync` feature 暴露。
- 配对用二维码 + 6 位指纹；日志只做手动压缩。
- 任意设备可发起轮换，`key_id` 升为 128 位随机值，同步库使用 `format=2`。
- `format=1 → 2` 升级不可回退，旧二进制被强制拒绝。
- 当前写 key 按 `(created_at, key_id)` 收敛；时钟偏差只影响选谁当写 key，不影响可读性。
- 默认软轮换 + 显式退役；迁移完成用 `kind=4` 公告。

后续（不在本版）：

- **只读降级/导出工具**：让旧版本能读取 `format=2` 的备份；本版先不做，但是在路线图上，实现时需同时定义导出格式与只读打开路径。
- HLC/向量时钟，替代 `(created_at, key_id)` 的写 key 收敛。
- 设备签名与撤销，防恶意对端、支持最小权限。
- 自动压缩与自动轮换策略。
