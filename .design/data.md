# shio 数据与存储格式

<!-- 持久化格式的稳定契约。改字节布局、AAD 或密钥派生前，必须先追加 DECISIONS.log。 -->

本文描述 `format=1` 的落盘格式。API 契约见 `api.md`；定位与取舍见 `changes/20261020-encrypted-persistence-core/design.md`。

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

redb 表名固定为以下四个：

| 表名 | 键 | 值 | 用途 |
|---|---|---|---|
| `shio.meta` | 固定字符串 `meta` | `MetaBlob` | 格式版本、当前 key、key check |
| `shio.keys` | `key_id: u16` | `KeyBlob` | 被包裹的数据密钥 |
| `shio.catalog` | `CatalogKey` | `CatalogBlob` | 命名空间/集合清单 |
| `shio.records` | `RecordKey` | `RecordBlob` | 加密记录 |

多设备同步启用后新增 `shio.changes` 表；不修改现有表的结构。

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

读取校验：`magic`、`format`、长度、`key_id` 必须存在于 `shio.keys`。

## 5. KeyBlob

键：`key_id`（u16，大端）。值：

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
- 崩溃后不会出现“header 已更新、密文未更新”的半条记录：二者在同一值内原子写入。
- 落盘字节一律不可信：所有长度、偏移、格式先校验；任何不一致返回 `Corrupted`，不 panic、不越界。
- 文件锁冲突（另一进程持写锁）→ `Locked`；`read_only` 打开可绕过写锁（由 redb 能力决定，若不可用则返回 `Locked`）。

## 11. 限制

| 项 | 值 |
|---|---|
| 名称长度 | 1–64 字节 UTF-8 |
| 名称字符集 | `[a-z0-9._-]`，不得以 `.` 开头 |
| 记录 ID | 16 字节 UUIDv7 |
| 单条明文 | 默认 16 MiB（可配置） |
| 单条密文 | 明文 + 16 字节 tag |
| `rev` | u64，从 1 开始 |
| `key_id` | u16，1..=65534（0 与 65535 保留） |
| 列表页 | 1..=1000，默认 100 |
| `format` | u16，当前 1 |

## 12. 不变量

1. `RecordBlob.ct_len + 62 == value.len()`。
2. `RecordBlob.key_id` 必须存在于 `shio.keys`；轮换期间新旧均可。
3. 每个有记录的 `(ns, coll)` 必须在 `shio.catalog` 中有条目。
4. 同一 `(rk, nonce)` 不得重复；nonce 由 CSPRNG 生成，24 字节。
5. 墓碑记录保留密文；只有 `purge` 移除键。
6. header 的任一字段被修改都会导致 AAD 不匹配、解密失败。
7. `format` 只增不减；未知主版本拒绝读写。

## 13. 泄露面

落盘可见的元数据：命名空间、集合、记录 ID、`rev`、创建/更新时间、墓碑标志、`key_id`、密文长度（近似明文大小）。

不可见：记录内容、字段名、标签、索引词、口令、DK、KEK。

访问模式（读写了哪些键、何时读写）不被隐藏；需要更强隐藏时属于后续独立设计（填充、桶化、ORAM），不在 `format=1` 内承诺。

## 14. 预留

- `flags` 的 bit1..7：压缩、外部存储、优先级等。
- `data_version`：集合级载荷演进，由调用方解释；内核只负责存取。
- `shio.changes`：同步变更日志，后续新增。
- 多 key 行：设备配对后每台设备一个包裹行，DK 相同，`key_id` 可共享或独立。
- 二级索引：以独立表实现，不改变 `RecordBlob`。
