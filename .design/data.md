# shio 数据与存储格式

<!-- 持久化格式的稳定契约。改字节布局、AAD 或密钥派生前，必须先追加 DECISIONS.log。 -->

本文描述 `format=1`（基础库）与 `format=2`（同步库，M0–M5 已实现）的落盘格式。API 契约见 `api.md`；定位与取舍见 `changes/20261020-encrypted-persistence-core/design.md` 与 `changes/20261022-sync-design/design.md`。

## 1. 文件与引擎

- 单文件 redb 数据库，默认文件名 `shio.redb`。
- 页大小、缓存等由 redb 管理；shio 只设置默认缓存与最大记录体，不自定义页格式。
- 单写多读：任意时刻一个写事务，读事务并发且看到一致快照。
- 进程崩溃后由 redb 保证已提交事务的原子性与持久性；shio 不引入额外 WAL。
- 文件权限由调用方设置；shio 不修改 umask。Android 上应位于应用私有目录。

## 2. 编码约定

- 整数一律**大端**：`u16`、`u32`、`u64`、`i64`。
- 名称（namespace / collection / label）编码为 `u8 len` + UTF-8 字节，`len ≤ 64`，不含 NUL。
- 记录 ID 固定 16 字节（UUIDv7 原始字节，非文本）。
- 未使用的保留位写 0；读取时忽略未知保留位，但不得据此跳过认证。
- 所有 `len` 在读取时必须与剩余字节数交叉校验；不一致即 `Corrupted`。

## 3. 表

`format=1` 使用四张表：

| 表名 | 键 | 值 | 用途 |
|---|---|---|---|
| `shio.meta` | 固定字符串 `meta` | `MetaBlob` | 格式版本、当前 key、key check |
| `shio.keys` | `key_id: u16` | `KeyBlob` | 被包裹的数据密钥 |
| `shio.catalog` | `CatalogKey` | `CatalogBlob` | 命名空间/集合清单 |
| `shio.records` | `RecordKey` | `RecordBlob` | 加密记录 |

`format=2` 新增 16 字节 key 表与两张同步表，现有表结构与键型不变：

| 表名 | 键 | 值 | 用途 |
|---|---|---|---|
| `shio.keys2` | `key_id: 16 字节` | `KeyBlob v2` | format 2 的数据密钥 |
| `shio.changes` | `u64` 大端序号 | `ChangeBlob` | 同步变更日志 |
| `shio.sync` | 字节串（前缀 + id） | 按字段 | 同步状态、游标、轮换与升级进度 |

- 升级期间 `shio.keys` 与 `shio.keys2` 并存；升级完成后清空 `shio.keys`。
- 同步表只在 `format=2` 使用；`format=1` 打开时不创建、不读取。
- 新增表不修改现有四张表的键型或值布局。
- S2 备份不引入新表或新字段：在一个 redb 读事务中把上述已有表原样复制到新文件，保留 `shio.sync` 中的 `db_id`、`device_id`、peer 游标、轮换/升级状态；恢复是文件级替换，不解密、不重加密，也不改变数据布局。

## 4. MetaBlob

```
偏移  长度  字段
0     4     magic = "SHIO"
4     2     format = 1
6     2     key_id（当前数据密钥）
8     8     created_at（Unix 毫秒，i64）
16    1     app_id_len（0..=64）
17    n     app_id（UTF-8，可为空）
17+n  24    key_check_nonce
41+n  32    key_check_ct
```

- `key_check_ct` 是 16 字节明文 `"shio-keycheck-v1"` 的 AEAD 密文（含 16 字节 tag）。
- MetaBlob 不含秘密；`app_id` 用于区分同一内核的不同应用实例，默认空。
- `format` 是**整个数据库**的主格式版本；未知版本拒绝打开。

读取校验：`magic`、`format`、长度、`key_id` 必须存在于 `shio.keys`。`format=2` 的布局与 `shio.keys2` 校验见 §15.1。

## 5. KeyBlob

`format=1` 的键：`key_id`（u16，大端）。`format=2` 的 16 字节键与布局见 §15.2。值：

```
偏移  长度  字段
0     2     key_id（与键一致）
2     1     kdf_id（1 = Argon2id v1.3）
3     1     flags（保留，0）
4     4     m_cost_kib（u32，默认 65536）
8     4     t_cost（u32，默认 3）
12    4     p_cost（u32，默认 1）
16    16    salt
32    24    wrap_nonce
56    48    wrapped_dk（32 字节 DK + 16 字节 tag）
104   8     created_at（Unix 毫秒）
112   1     label_len
113   n     label（UTF-8，例如 "primary" / "device:ab12"）
```

- `wrapped_dk` 的明文是 32 字节数据密钥 `DK`。
- 包裹算法：XChaCha20-Poly1305，密钥为 `KEK`，nonce 为 `wrap_nonce`，AAD 见 §8。
- `kdf_id` 未知 → `UnsupportedFormat`；参数非法 → `Corrupted`。
- 允许多行 key：轮换期间新旧并存；同步阶段可加入设备专属包裹。
- `label` 仅用于诊断，不参与密码学。

## 6. CatalogBlob

键 `CatalogKey`：

```
u8 ns_len | ns | u8 coll_len | coll
```

值 `CatalogBlob`：

```
偏移  长度  字段
0     8     created_at（Unix 毫秒）
8     2     catalog_version = 1
```

- 集合首次写入时登记；物理清空后条目保留（不维护计数，避免写放大）。
- 列表集合只扫描本表，不触碰记录表。

## 7. RecordBlob

`format=2` 的 `SHR2` 布局见 §15.3；以下为 `format=1` 的 `SHR1`。

键 `RecordKey`：

```
u8 ns_len | ns | u8 coll_len | coll | 16 字节 UUID
```

值 `RecordBlob`：

```
偏移  长度  字段
0     4     magic = "SHR1"
4     1     format = 1
5     1     flags（bit0 = deleted，bit1..7 保留）
6     2     key_id
8     8     rev（u64，从 1 开始）
16    8     created_at（Unix 毫秒）
24    8     updated_at（Unix 毫秒）
32    2     data_version（默认 1，由调用方解释）
34    24    nonce
58    4     ct_len（u32，密文字节数，含 16 字节 tag）
62    n     ciphertext
```

- **header** 指偏移 `0..34` 的 34 字节（magic 到 data_version）；`nonce` 与 `ct_len` 不属于 header。
- 认证覆盖 header、nonce（作为 AEAD 输入）与密文；`ct_len` 由结构校验保证。
- 读取时必须验证 `62 + ct_len == value.len()`，否则 `Corrupted`。
- 墓碑（`deleted=1`）保留密文，直到显式 `purge`。
- `updated_at` 使用调用进程的 UTC 毫秒；不保证单调，只用于展示与同步比较，排序以 `rev` 为准。

## 8. AAD 编码

所有 AAD 以固定域分隔符开头，字段用长度前缀拼接，避免歧义。

### 8.1 记录

```
"shio/v1/record"
|| u8 ns_len || ns
|| u8 coll_len || coll
|| 16 字节 UUID
|| header[0..34]
```

- 绑定命名空间、集合、ID、格式版本、flags、key_id、rev、时间戳与 data_version。
- 因此：跨命名空间/集合/ID 搬运记录、篡改 header、调换 rev 都会导致解密失败。
- `nonce` 不进入 AAD（它是 AEAD 的输入）；`ct_len` 不进入 AAD（结构校验）。

### 8.2 密钥包裹

```
"shio/v1/keywrap" || u16 key_id（大端）
```

绑定 key_id，防止把一行的 `wrapped_dk` 换到另一行。

### 8.3 密钥校验

```
"shio/v1/keycheck"
```

固定值；明文固定为 `"shio-keycheck-v1"`。

`format=2` 的域分隔与轮换 AAD 见 §15.4；升级期间的 `SHR1` 仍按本节验证。

## 9. 密钥派生与加密流程

### 9.1 层级

```
password (UTF-8 bytes)
  └─ Argon2id(salt, m, t, p, out=32) ──▶ KEK
       └─ XChaCha20-Poly1305 解包(wrapped_dk, AAD=keywrap) ──▶ DK (32B)
            └─ HKDF-SHA256(salt="shio/v1/hkdf-salt", info=...) ──▶ 子密钥
                 ├─ "shio/v1/record-key"  ▶ rk  (32B) 记录加密
                 ├─ "shio/v1/meta-key"    ▶ mk  (32B) 保留
                 └─ "shio/v1/check-key"   ▶ ck  (32B) key check
```

- `KEK` 在会话期间保留（用于改口令与轮换），关闭时清零；`DK`、`rk`、`ck` 同样驻留内存并清零。
- 口令按调用方提供的 UTF-8 字节使用，不做 Unicode 归一化；UI 层应先做 NFC。
- 所有随机数来自操作系统 CSPRNG（`getrandom`）。

`format=2` 沿用本节的 KDF 链与 HKDF info；新增的轮换包裹密钥见 §15.4。

### 9.2 初始化（库不存在且 `create=true`）

单个写事务内完成：

1. 生成 16 字节 salt、32 字节 DK。
2. `KEK = Argon2id(password, salt, m=65536, t=3, p=1)`。
3. 生成 `wrap_nonce`（24B），计算 `wrapped_dk`。
4. 由 DK 派生 `ck`，生成 `key_check_nonce`，加密固定明文。
5. 写入 `shio.keys[key_id=1]` 与 `shio.meta`。
6. 提交。

并发初始化由 redb 写锁串行化；进入写事务后重新检查 meta，已存在则转为普通打开。

### 9.3 打开

1. 读 `shio.meta`；缺失且 `create=false` → `NotFound`。
2. 校验 magic / format / 长度。
3. 读当前 `key_id` 对应的 `KeyBlob`；未知 `kdf_id` → `UnsupportedFormat`。
4. 派生 `KEK`，解包 `wrapped_dk`。tag 校验失败 → `InvalidKey`（不区分口令错误与数据损坏）。
5. 由 DK 派生 `rk` / `mk` / `ck`，验证 key check；失败 → `InvalidKey`（与解包失败同一错误，避免区分口令错误与密钥损坏）。
6. 清零 KEK 之外的临时材料；KEK、DK、子密钥保留至关闭。

### 9.4 写入

1. 序列化 `T` 为 JSON 字节；超过 `max_value_size` → `ValueTooLarge`。
2. 读旧记录 header（同一写事务）：存在则 `rev+1`、保留 `created_at`；不存在则 `rev=1`、`created_at=now`。
3. `updated_at=now`，`flags.deleted=0`。
4. 生成 24 字节随机 `nonce`，构造 header 与 AAD。
5. 用 `rk` 加密 JSON，写入 `RecordKey → RecordBlob`。
6. 若 `(ns, coll)` 不在 catalog，写入 `CatalogBlob`。
7. 提交事务。

明文 JSON 缓冲在加密后立即清零（best-effort）。

### 9.5 读取

1. 读值，校验 `magic/format/ct_len`。
2. 用 header 的 `key_id` 选择对应密钥（轮换期间可能是旧 key）。
3. 重建 AAD，解密。tag 失败 → `Corrupted`。
4. `serde_json` 反序列化；失败 → `Deserialize`。

### 9.6 删除与清除

- `delete`：写入 `flags.deleted=1`、`rev+1`、`updated_at=now`，用同样载荷重新加密（因 header 在 AAD 内）；不存在则返回 `false`。
- `purge`：直接移除 `RecordKey`，不保留墓碑。多端同步启用后禁止使用。
- 重复 `delete` 已墓碑记录：`rev+1` 并刷新时间，返回 `true`。

### 9.7 改口令

1. 生成新 salt 与新 `KEK`。
2. 用会话内的 DK 重新包裹，更新该 key 行的 salt / 参数 / nonce / wrapped_dk。
3. 替换内存中的 KEK；不触碰记录。
4. 崩溃安全：单行更新，原子提交。
5. 若存在多行 key（轮换未完成），返回 `Busy`，要求先完成轮换。

### 9.8 数据密钥轮换

1. `new_key_id = current + 1`（溢出则拒绝）。
2. 生成 `DK'`，用当前 KEK 包裹，写入新 key 行；生成新 `rk` / `ck`。
3. 分批遍历 `shio.records`：用记录 header 的 key_id 选择旧密钥解密，改用 `DK'` 重加密，更新 `key_id` 与 `nonce`。每批一个写事务。
4. 全部迁移后用新 `ck` 重写 meta 的 key check（nonce + ct），并更新 `shio.meta.key_id`。
5. 确认无记录引用旧 key_id 后删除旧 key 行。

任一步崩溃都可恢复：

- 记录按自身 `key_id` 解密，新旧密钥并存期间都能读。
- meta 未更新时继续使用旧 key；更新后新记录使用新 key。
- 迁移可重入：已迁移记录（key_id 已是新值）跳过。

## 10. 事务、并发与崩溃

- 每个公开写操作 = 一个 redb 写事务；`batch` 的所有操作在同一事务内。
- `batch` 中任一操作失败 → 整个事务不提交，返回首个错误。
- 读事务看到提交快照；长列表使用游标分页，不持有跨调用快照。
- 崩溃后不会出现“header 已更新、密文未更新”的半条记录：二者在同一值内原子写入。若进程被强杀后 redb 需要修复，只读打开不能修复，应先以写句柄打开一次。
- 落盘字节一律不可信：所有长度、偏移、格式先校验；任何不一致返回 `Corrupted`，不 panic、不越界。
- 文件锁冲突（另一句柄或进程持写锁）→ `Locked`；`read_only` 打开在非 Android 平台由 redb 能力决定，Android 上持 `<db>.lock` 的共享锁，与写者互斥。

## 11. 限制

| 项 | 值 |
|---|---|
| 名称长度 | 1–64 字节 UTF-8 |
| 名称字符集 | `[a-z0-9._-]`，不得以 `.` 开头 |
| 记录 ID | 16 字节 UUIDv7 |
| 单条明文 | 默认 16 MiB（可配置） |
| 单条密文 | 明文 + 16 字节 tag |
| `rev` | u64，从 1 开始 |
| `key_id` | `format=1`：u16，1..=65534（0 与 65535 保留）；`format=2`：16 字节随机值 |
| 列表页 | 1..=1000，默认 100 |
| `format` | u16，当前 1；同步库为 2 |

## 12. 不变量

1. `format=1`：`RecordBlob.ct_len + 62 == value.len()`；`format=2`：`ct_len + 76 == value.len()`。
2. `RecordBlob.key_id` 必须存在于对应 key 表（`shio.keys` 或 `shio.keys2`）；轮换期间新旧均可。
3. 每个有记录的 `(ns, coll)` 必须在 `shio.catalog` 中有条目。
4. 同一 `(rk, nonce)` 不得重复；nonce 由 CSPRNG 生成，24 字节。
5. 墓碑记录保留密文；只有 `purge` 移除键；同步启用后禁止 `purge`。
6. header 的任一字段被修改都会导致 AAD 不匹配、解密失败。
7. `format` 只增不减；未知主版本拒绝读写。
8. `format=2` 的 `ChangeBlob(kind=0)` 中 `key_id` / `rev` / `updated_at` / `flags.deleted` 必须与它携带的 `RecordBlob v2` header 一致。
9. `shio.sync` 的 `db`、`dev` 一旦写入不得更换；`peer/` 游标只能单调增加。

## 13. 泄露面

落盘可见的元数据：命名空间、集合、记录 ID、`rev`、创建/更新时间、墓碑标志、`key_id`、密文长度（近似明文大小）。

不可见：记录内容、字段名、标签、索引词、口令、DK、KEK。

访问模式（读写了哪些键、何时读写）不被隐藏；需要更强隐藏时属于后续独立设计（填充、桶化、ORAM），不在 `format=1` 内承诺。`format=2` 的额外泄露面见 §15.13。

## 14. 预留

- `flags` 的 bit1..7：压缩、外部存储、优先级等。
- `data_version`：集合级载荷演进，由调用方解释；内核只负责存取。
- 多 key 行：`format=1` 用于轮换；`format=2` 用于任意设备轮换形成的 key 图，所有行共用同一 KEK。
- 二级索引：以独立表实现，不改变 `RecordBlob`。
- 只读降级/导出工具：让旧版本读取 `format=2` 备份；本版不做，后续实现。

## 15. format 2（同步库）

> 本节是 `20261022-sync-design` 的字节契约，M0–M5 已实现；新库仍先写 `format=1`，`enable_sync` 后升级为 `format=2`。

`format=2` 保留 `format=1` 的四张表与 `RecordKey` / `CatalogKey` 布局，只改变 meta、key、record 的字段宽度与 AAD 域，并新增 `shio.keys2`、`shio.changes`、`shio.sync`。升级期间新旧记录并存，按每条记录的 `format` 字节选择解码器。

### 15.1 MetaBlob v2

```
偏移  长度  字段
0     4     magic = "SHIO"
4     2     format = 2
6     16    key_id（当前写 key）
22    8     created_at（Unix 毫秒，i64）
30    1     app_id_len（0..=64）
31    n     app_id（UTF-8，可为空）
31+n  24    key_check_nonce
55+n  32    key_check_ct
```

- key check 明文为 `"shio-keycheck-v2"`，AAD 为 `"shio/v2/keycheck"`。
- `key_id` 必须存在于 `shio.keys2`；读取时不接受 v1 的 u16 字段宽度。
- `format=2` 的 meta 一旦提交，旧二进制在 meta 校验阶段直接拒绝；升级不可回退。

### 15.2 KeyBlob v2

键为 16 字节 `key_id`，存放在 `shio.keys2`；`shio.keys` 只保留给 v1 行，升级完成后清空。值：

```
偏移  长度  字段
0     16    key_id（与键一致）
16    1     kdf_id（1 = Argon2id v1.3）
17    1     flags（保留，0）
18    4     m_cost_kib
22    4     t_cost
26    4     p_cost
30    16    salt
46    24    wrap_nonce
70    48    wrapped_dk（32 字节 DK + 16 字节 tag）
118   8     created_at（Unix 毫秒）
126   1     label_len
127   n     label（UTF-8）
```

- 包裹 AAD 为 `"shio/v2/keywrap" || key_id16`。
- 同一数据库的所有 key 行共用同一 KEK（同一 salt/参数）；每行用独立 `wrap_nonce`。
- `key_id` 为 CSPRNG 生成的 16 字节非零值，不与现有 key 重复；`created_at` 用于写 key 收敛。
- 单库 key 行上限 32（配对包与轮换共用此上限）。
- `label` 仅用于诊断，不参与密码学。

### 15.3 RecordBlob v2

键 `RecordKey` 与 §7 相同。值：

```
偏移  长度  字段
0     4     magic = "SHR2"
4     1     format = 2
5     1     flags（bit0 = deleted，bit1..7 保留）
6     16    key_id
22    8     rev（u64，从 1 开始）
30    8     created_at（Unix 毫秒）
38    8     updated_at（Unix 毫秒）
46    2     data_version
48    24    nonce
72    4     ct_len（u32，含 16 字节 tag）
76    n     ciphertext
```

- **header** 为偏移 `0..48`；`76 + ct_len == value.len()`。
- AAD 为 `"shio/v2/record" || ns/coll/id || header[0..48]`。
- 记录体仍是 serde JSON 明文加密；`data_version` 语义不变。
- 墓碑、`rev`、时间戳与 v1 一致；同步模式写入时 `updated_at = max(now, 旧值+1)`。
- `format=1` 的 `SHR1` 记录在升级完成前仍按 v1 AAD 与 `shio.keys` 解码。

### 15.4 AAD 与派生 v2

```
记录：    "shio/v2/record" || u8 ns_len || ns || u8 coll_len || coll
          || 16 字节 UUID || header[0..48]
包裹：    "shio/v2/keywrap" || key_id16
校验：    "shio/v2/keycheck"；明文 "shio-keycheck-v2"
轮换 key：RK = HKDF-SHA256(salt=db_id, ikm=from_DK, info="shio/v2/rotation-key")
轮换 AAD："shio/v2/rotation" || db_id16 || from16 || to16
```

- 从 DK 派生子密钥的 HKDF info 仍是 v1 字符串（`shio/v1/record-key` 等），因为 KDF 链不变；变化只在 key_id 宽度、blob 布局与域分隔。
- 轮换只包裹 `DK_to`，不重新包裹整条链；`db_id` 防止跨库重放 offer。

### 15.5 `shio.sync`

键是字节串（前缀 + 16 字节 id），值为定长整数或 `ChangeBlob`：

| 键 | 值 | 说明 |
|---|---|---|
| `b"v"` | `u16` 大端 = 1 | 同步状态版本 |
| `b"db"` | 16 字节 | 逻辑数据库 ID，创建后不可更换 |
| `b"dev"` | 16 字节 | 本设备 ID，创建后不可更换 |
| `b"enabled_at"` | `i64` 大端 | 启用时间 |
| `b"baseline_done"` | `u8`（0/1） | 首次全量建日志是否完成 |
| `b"baseline_key"` | 记录键字节 | 基线扫描的最后一个记录键；缺失表示从起点开始 |
| `b"log_start"` | `u64` 大端 | 当前保留的最早日志序号；空日志为 1 |
| `b"peer/" + dev16` | `u64` 大端 | 已消费的对方日志序号 |
| `b"heard/" + dev16` | `u64` 大端 | 该设备已消费到本机日志的序号；单调，自动裁剪的依据 |
| `b"rot/" + to16` | `ChangeBlob`（kind=1） | 轮换 offer 的持久副本 |
| `b"ack/" + to16 + dev16` | `u8` = 1 | 已收到该设备对 `to` 的 ack |
| `b"cur/" + dev16` | `key_id16` | 该设备最近公告的当前写 key |
| `b"mig/" + key16 + dev16` | `u8` = 1 | 该设备已无 `key` 记录 |
| `b"upgrade"` | `u8 phase` + `u16 cursor_len` + `cursor` | format 1→2 的可恢复状态 |
| `b"upgrade/key/" + u16` | `key_id16` | 旧 u16 key_id 到新 id 的映射 |
| `b"migration"` | `to16` + `u16 cursor_len` + `cursor` | 进行中的 format=2 迁移持久游标；完成、目标 key 变化或应用远端记录后重写/删除 |

- `log_end` 不单独存储：等于 `shio.changes` 的最大键，空表为 0。
- 克隆（`clone_backup`）保留 `db`、`rot/`、`ack/`、`mig/`、`upgrade` 与 `upgrade/key/`、全部记录与密钥；重写 `dev`（新随机设备 ID）、`enabled_at`、`baseline_done`、`log_start=1`，删除 `baseline_key`、`peer/`、`cur/`、`heard/`、`migration` 与全部 `shio.changes`，随后由基线流程按记录键序重写记录条目与 `CurrentKey`。
- `upgrade.phase`：1 = meta 已切到 2；2 = 正在迁移；3 = 清理旧表。`cursor` 是最后迁移的记录键，空表示从起点开始。
- `rot/` 不随日志压缩消失；清理只走 `prune_rotation_offers`。
- `ack/` 与 `mig/` 是幂等标记；重复写入同值不改变语义。
- 表只被同步代码读取；业务代码不直接依赖这些键。

### 15.6 ChangeBlob v2

所有条目共用 8 字节头：

```
偏移  长度  字段
0     4     magic = "SHC2"
4     1     format = 2
5     1     kind（0 记录，1 轮换 offer，2 ack，3 当前 key 公告，4 迁移完成）
6     1     flags（仅 kind=0：bit0 = deleted）
7     1     reserved = 0
```

`kind=0` 记录：

```
0     16    key_id
16    8     rev
24    8     updated_at
32    16    writer（最初写这条记录的 device_id）
48    2     key_len
50    n     record_key
50+n  4     blob_len
54+n  m     RecordBlob v2 原始字节
```

`kind=1` 轮换 offer：

```
0     16    from_key_id
16    16    to_key_id
32    8     created_at
40    16    origin（发起设备）
56    24    wrap_nonce
80    4     ct_len
84    m     RK 包裹的 DK_to（32 字节 + 16 字节 tag）
```

`kind=2` ack：

```
0     16    to_key_id
16    16    device_id
32    8     at
```

`kind=3` 当前 key 公告：

```
0     16    key_id
16    8     at
```

`kind=4` 迁移完成：

```
0     16    key_id（本机已无该 key 的记录）
16    8     at
```

- `kind=0` 的 `key_id` / `rev` / `updated_at` / `flags.deleted` 必须与 `RecordBlob` header 一致，否则 `Corrupted`；`key_id` 必须存在于 `shio.keys2`（升级期间也可能是 `shio.keys`）。
- `writer` 保留最初写入设备；本机应用远端条目后追加本地日志时沿用该 `writer`，本机新写入用 `device_id`。快照条目没有 `writer`，应用后以本机 `device_id` 记日志。
- 控制条目（1/2/3/4）只改同步状态与密钥行，不写记录表；它们与记录共用日志序号，保证 offer 排在用新 key 写入的记录之前。
- `blob_len` 必须等于剩余字节数；`key_len` 必须与记录键结构自洽。

### 15.7 `shio.changes` 与本地写入

- 表键为 `u64` 大端本地序号，值 `ChangeBlob`；只追加，不原地改写。
- `insert` / `put` / `delete` / `batch` 在**同一写事务**内写记录、catalog（如需）和 `kind=0` 日志；序号为 `log_end + 1`。
- `batch` 的每条成功操作各追加一条日志；任一操作失败则整个事务回滚，日志也不写。
- `purge` 在同步模式下返回 `SyncEnabled`，避免墓碑丢失。
- catalog 不单独记日志；应用远端记录时按 `(ns, coll)` 补登记。
- 同步模式下本地写入的 `updated_at = max(now, 旧记录 updated_at + 1)`；跨设备仍按 LWW。
- 基线扫描为已有记录补 `kind=0` 日志，每 256 条一个事务，同时推进 `baseline_key`；完成后写 `baseline_done=1` 和 `kind=3`。基线期间持有会话写锁，业务写入等待。

### 15.8 应用、快照与压缩

应用页在**一个写事务**内完成：

1. 校验每条 `ChangeBlob` 与记录 blob；帧层的 `db_id` 与格式已在解码时校验。
2. `kind=0` 按 `(updated_at, rev, RecordBlob 字节序)` 与本地比较；远端严格更大才覆盖，否则跳过。
3. 覆盖时写记录、补 catalog、追加本地 `kind=0` 日志（沿用 `writer`）；不覆盖时不追加日志，避免回声。
4. 控制条目更新 `shio.sync`、安装 key、生成 ack；不转发。缺记录 key 时整页回滚并返回 `RotationRequired`。
5. 更新 `peer/<sender> = next`，提交。任一步失败整页回滚。

- 重复页、同页重复条目、相同 blob 都是幂等的；相等或更小直接跳过。
- `migrate_all` 重加密时推进 `updated_at`，因此旧密钥的旧日志不会在 LWW 里覆盖已迁移副本。
- `after + 1 < log_start` → `SyncResyncRequired { log_start }`；`after > log_end` → `InvalidArgument`。
- `snapshot_start()` 在只读事务取 `at_seq = log_end`；`snapshot_page(after_key, limit)` 按键序返回 `record_key + RecordBlob`，它返回的 `at_seq` 是该次读取时的 `log_end`，续拉锚点应使用 `shio-sync` 迭代器保存的起始 `at_seq`。应用完快照后从起始 `at_seq + 1` 续拉；快照期间的新更新仍在日志里。
- `compact_changes(before)` 删除序号 `< before` 的条目并前移 `log_start`，返回删除条数；`before` 超出 `1..=log_end+1` 返回 `InvalidArgument`。本版只手动压缩。

### 15.9 帧 `SHY2`

```
偏移  长度  字段
0     4     frame_len（不含这 4 字节）
4     4     magic = "SHY2"
8     1     format = 2
9     1     frame_kind（0 变更页，1 offer 页，2 快照页）
10    1     flags（保留，0）
11    16    db_id
27    16    sender
43    8     after
51    8     log_start
59    8     log_end
67    8     next
75    2     count
77    ...   count ×（u32 entry_len + entry）
```

- `frame_kind=0`：entry 为 `ChangeBlob`；`after/next/log_start/log_end` 是发送方日志语义。
- `frame_kind=1`：entry 为 `kind=1` offer，按 `created_at` 升序；`after=0`。
- `frame_kind=2`：entry 为 `u16 key_len | record_key | u32 blob_len | RecordBlob`；`sender` 为全 0，`after=0`，`log_start=log_end=at_seq`，`next=1` 表示还有下一页、`0` 表示结束。快照游标不放进帧头：接收方用本页最后一条记录的 `record_key` 作为下一页的 `after_key`；这与 `snapshot_page` 的排他游标一致。
- 解码顺序：`frame_len` → magic/format/kind/db_id → `count` → 逐条 `entry_len`；先限额再分配。单帧默认上限 4 MiB、`count ≤ 1000`，调用方可调小。
- 帧 `format` 与数据库 `format` 独立演进；未知版本返回 `UnsupportedFormat`。

### 15.10 配对包 `SHB2` / `SHW2`

明文（仅内存）：

```
"SHB2" | u8 format=2 | db_id16 | current_key_id16
       | u8 app_id_len | app_id
       | i64 created_at
       | u8 key_count | key_count ×（key_id16 + DK32）
```

外层：

```
"SHW2" | u8 kdf_id | u32 m_cost | u32 t_cost | u32 p_cost
       | salt16 | nonce24 | u32 ct_len | ciphertext
```

- `ct_len` 含 16 字节 tag；明文用 XChaCha20-Poly1305 加密。
- 外层密钥由 32 字节 `pairing_secret` 经 Argon2id 派生；AAD 为 `"shio/v2/pairing" || 外层 header[0..33]`（magic 到 salt）。
- `key_count` 为 1..=32，且必须包含 `current_key_id`；新设备拿到整条链，才能解密所有历史记录。
- 指纹为外层帧 SHA-256 前 3 字节的 6 位十六进制；默认有效期 10 分钟，接收方拒绝 `created_at` 超出 ±10 分钟的包。
- 新设备用新 salt + 新 KEK 逐行包裹所有 DK，写 `shio.keys2`；meta 写 `format=2`、`key_id = current_key_id`；`shio.sync` 写 `db`、新 `dev`、`log_start=1`、`baseline_done=1`，并追加 `kind=3` 与 `cur/<dev>`。
- 配对只创建新库；目标必须不存在或为空；失败不产生半个库。

### 15.11 升级 1 → 2

`enable_sync` 在 `format=1` 上执行一次可恢复升级：

1. 取会话写锁，确认没有升级状态；`read_only` 返回 `ReadOnly`。
2. **单个事务**：创建 `shio.keys2`、`shio.changes`、`shio.sync`；为每个旧 key 行生成 16 字节新 id 并写 `shio.keys2`（同一 KEK/DK）；写 `upgrade/key/<u16>` 映射；写 `MetaBlob v2`（format=2、新当前 key、v2 key check）；写 `upgrade.phase=1`。
3. 提交后旧二进制立即无法打开；新二进制在 meta=2 下混合读取 `SHR1`/`SHR2`。
4. 按记录键序每 256 条一个事务：`SHR1` 用 `shio.keys` 与 v1 AAD 解码，改用 16 字节 key_id、v2 AAD 与新 nonce 重加密为 `SHR2`；更新 `upgrade` 的 cursor。可中断续跑。
5. 全部迁移后进入 phase=3：删除 `shio.keys` 旧行、`upgrade/key/` 映射与 `upgrade` 状态；`shio.keys2` 保留。清理可重入。
6. 升级期间 `change_password` 与 `rotate_data_key` 返回 `Busy`；完成后 `enable_sync` 继续生成 `db_id`/`device_id` 并做基线。

### 15.12 format 2 限制与不变量

轮换与退役的持久状态：

- `begin_rotation`：生成 `to`，写 `shio.keys2` 行与 `rot/<to>`，追加 `kind=1`，更新 `meta.key_id` 与 key check，追加 `kind=3`，写 `cur/<dev>`；全部同事务。
- 安装 offer：校验 `db_id` 与祖先 key，解包 `DK_to`，用本机 KEK 重包裹，写 `shio.keys2`，追加 `kind=2` ack；若 `to` 更大则同时切 `meta.key_id` 并追加 `kind=3`。
- `migrate_all(to)`：每 256 条重加密，重加密时把 `updated_at` 推进到 `max(now, 旧值 + 1)`（避免旧密钥的同一逻辑版本在 LWW 的 blob 次序里翻回旧密钥）；完成后对不再被引用的 key 追加 `kind=4` 并写 `mig/<key>/<dev>`。
- `retire(from)`：仅在本机无 `from` 记录、无设备以 `from` 为 `cur`、所有已知设备都有 `mig/from/<dev>`、且无未安装的 `from` offer 时删除 key 行。
- `prune_rotation_offers(to)`：所有已知设备都有 `ack/to/<dev>` 后删除 `rot/<to>`；不触碰记录表。
- 同步模式的 `change_password` 重新包裹所有 `shio.keys2` 行（同一新 salt/KEK、各自 nonce，单事务）；`format=1` 仍要求单行。

| 项 | 值 |
|---|---|
| key 行数 | ≤ 32 |
| key_id | 16 字节 CSPRNG，非零且不重复 |
| 记录密文偏移 | 76 |
| 单条日志 | 记录 blob + 记录键 + 62 字节开销；受 `max_value_size` 约束 |
| 帧 | ≤ 4 MiB，`count ≤ 1000` |
| 配对包 | `key_count ≤ 32`，默认 TTL 10 分钟 |
| 当前写 key | 本机已安装 key 中按 `(created_at, key_id)` 最大者 |

1. `format=2` 的 `RecordBlob`：`ct_len + 76 == value.len()`；`key_id` 必须存在于 `shio.keys2`。
2. `meta.key_id` 必须存在于 `shio.keys2`，且是本机当前写 key。
3. `kind=0` 日志与记录 header 必须一致；日志中的 blob 必须是 `SHR2`（升级期间同步尚未启用，因此日志里不出现 `SHR1`）。
4. `shio.sync` 的 `db`、`dev` 不可更换；`peer/`、`heard/` 游标单调增加；`log_start` 只前移，自动裁剪至少保留最新一条日志。
5. `rot/`、`ack/`、`cur/`、`mig/` 的 id 必须是 16 字节；`ack/` 与 `mig/` 只写 1。
6. 升级完成后不得再写 `shio.keys`；旧二进制不得打开 meta=2 的库。
7. 同步模式禁止 `purge`；删除只写墓碑。

### 15.13 泄露面增补

同步库额外泄露：变更日志的序号与时间、`writer`/`sender` 设备 id、`key_id`、轮换关系、各设备当前 key 与迁移状态；配对帧泄露 `db_id`、key 数量与密文长度。记录内容、DK、KEK、pairing secret 仍不可见。
