# 20261025-record-transfer

<!-- 逻辑记录导出/导入：版本化 JSONL、LWW 合并、两遍校验；明文落盘标为危险操作。 -->

## 背景

现有整库能力都不是数据交换格式：

- `backup`/`restore` 复制整个 redb 文件（密文、带 key 链与身份），只能同库恢复；
- `clone_backup` 换设备身份，但仍要求同一 key 链与 `db_id`；
- `sync`/`snapshot` 需要配对并共享密钥。

用户需要把数据**带出去**或**带进来**：换安装、迁出到别的系统、从旧系统导入、把导出文件交给他人。
这些场景的共同点是：目标库与源库不共享密钥，因此只能在解密后按逻辑记录搬运，再用目标库
当前 key 重新加密。

另有一个实现约束：内核没有枚举命名空间的 API（只有 `Kernel::collections(ns)`）。导出放在
core 内部直接扫 `shio.records` 与 `shio.catalog`（编码键天然按 ns/col/id 排序），不为此新增
公开枚举接口。

## 本阶段范围

- 新增 `Engine::export_records` / `Engine::import_records` 与稳定、可人工检查的文件格式。
- 导出保留每条记录的全部元信息（`rev`、`created_at`、`updated_at`、`data_version`、墓碑）。
- 导入按 LWW 合并、保留源时间戳、重新加密、分批事务、可 dry-run、可重跑（幂等）。
- 格式自校验：头部版本、精确计数、SHA-256；导入先校验后写入。
- 暴露 facade 与 C ABI（`shio_export` / `shio_import`），并加 Android harness。
- 明文落盘明确标为危险操作：文档、CLI 提示、覆盖需显式开关。

不在本次范围：加密归档/口令派生、CSV、网络流式传输、清空后整体替换、跨库同步身份、
把导出文件当备份使用。独立 CLI `crates/shio-tool` 放到下一个变更。

## 文件格式 v1：`.shio.jsonl`

UTF-8、逐行 JSON、流式读写。首行 `head`，尾行 `end`，中间只有 `col` 与 `rec`。

```json
{"t":"head","v":1,"fmt":2,"at":1761350400000,"deleted":false}
{"t":"col","ns":"vault","col":"entries","at":1758758400000}
{"t":"rec","ns":"vault","col":"entries","id":"018f3c1e-9f0a-7c3e-8f5d-2b1a0c9d8e7f","rev":3,"created":1758758400000,"updated":1758758500123,"dv":1,"del":false,"v64":"eyJuYW1lIjoi5byg5LiJIn0="}
{"t":"rec","ns":"vault","col":"entries","id":"018f3c1e-9f0a-7c3e-8f5d-2b1a0c9d8e80","rev":4,"created":1758758400000,"updated":1758758600000,"dv":1,"del":true}
{"t":"end","rec":2,"col":1,"sha":"<64 位小写 hex>"}
```

规则：

- `head.v` 当前为 1；`head.fmt` 是源库格式版本（1 或 2），只作来源提示，导入不依赖。
- `head.at` 是导出时间，`head.deleted` 记录本次是否包含墓碑。
- 值二选一、逐行自描述：`v64`（标准 base64，默认）或 `v`（合法 UTF-8 字符串，工具可读模式）。
  非墓碑记录必须且只能有一个；`del=true` 不带值。
- `sha` 覆盖 `head` 之后、`end` 之前的每条体行字节加一个 `\n`（行内容不含换行），因此 CRLF
  文件也能校验通过；`rec`/`col` 是精确行数。允许空行（导入时跳过，不参与计数与哈希）。
- 不写 `db_id`、key、口令、日志、游标或密码学参数；`db_id` 是稳定指纹，写进明文文件会造成
  跨导出关联，默认排除。
- 记录按编码键序（namespace, collection, id）输出，同一库同样参数导出除 `head.at` 外字节级可复现。
- 未知字段忽略（向前兼容），未知 `t`、未知 `head.v` 直接拒绝。

## 导出

```rust
pub struct ExportOptions {
    pub namespace: Option<String>,
    pub collection: Option<String>,
    pub include_deleted: bool,
    pub overwrite: bool,
}
pub struct ExportInfo {
    pub format_version: u16,
    pub records: u64,
    pub deleted: u64,
    pub collections: u64,
    pub bytes: u64,
}
```

- `collection` 必须与 `namespace` 同时给出；否则 `InvalidArgument`。
- 整个导出在单个 redb 读事务里完成（与 `backup` 一致，快照一致；代价是延迟页回收）。
- 先写目标同目录临时文件，成功 `flush` 后原子改名；目标已存在且 `overwrite=false` 返回
  `InvalidArgument`，失败不留半成品。
- 只读句柄可导出（`read_only=true` 也可用）。
- `col` 行来自 `shio.catalog`，空集合也输出，保留原始 `created_at`。
- 墓碑默认不导出（`include_deleted=false`），此时 `ExportInfo.deleted = 0`。

## 导入

```rust
pub enum ImportMode { Merge, Replace }
pub struct ImportOptions { pub mode: ImportMode, pub dry_run: bool, pub batch_size: usize }
pub struct ImportReport {
    pub records: u64, pub applied: u64, pub skipped: u64, pub deleted: u64,
    pub collections: u64, pub bytes: u64, pub dry_run: bool,
}
```

**两遍处理**：第一遍只校验（`head`、逐行 JSON、名称、UUID、`rev`/时间戳、值编码、值长度、
精确计数、SHA-256、`end` 之后无内容），任何问题在写入前失败；第二遍按批导入。
`dry_run=true` 只跑第一遍，报告里的 `applied`/`skipped` 按校验结果推算，不写库。

**Merge（默认，与同步 LWW 对齐）**：

1. 本地不存在 → 写入（墓碑行跳过并计入 `skipped`）。
2. 本地存在且 `(updated_at, rev)` 更大 → 跳过。
3. 相等 → 解密本地比较明文：相同则跳过（**重跑幂等**）；不同则按明文字节序决定，规则固定可复现。
4. 墓碑行且本地存在：按上面的 LWW 判断后写墓碑（保留源 `rev`/`updated_at`），明文沿用本地值；
   相等时墓碑优先。
5. 墓碑行且本地不存在：跳过（新库不需要墓碑）。

**Replace**：本地优先覆盖，但只保证压过本地副本：

- `rev = max(local.rev + 1, src.rev)`（本地不存在时 `max(1, src.rev)`），避免 rev 回退；
- `updated_at = max(src.updated_at, local.updated_at + 1)`（本地不存在时用源值）；
- `created_at` 沿用本地已有值（覆盖不改变首次创建时间）；
- 对端若存在更新的版本，仍按 LWW 胜出，不做“全局强制”。

写入细节：

- 用目标库当前 key 重新加密；format=1 用 v1 记录格式，format=2 用 v2，与 `stage_put` 同路径。
- 保留源 `created_at`/`updated_at`/`rev`/`data_version`/墓碑标志。
- 同步启用时按本机 `device_id` 追加 `shio.changes` 条目，对端能正常拉到；format=1 不写日志。
- 每批（默认 256，上限 4096，0 取默认）一个写事务，批内原子、整体非原子；失败后重跑靠幂等收敛。
- `col` 行只补登记缺失的集合，保留源 `created_at`。
- 普通写入路径：维护窗口返回 `Busy`，只读返回 `ReadOnly`，超 `max_value_size` 返回 `ValueTooLarge`。
- 不校验业务类型（内核不知道值的 schema），只校验名称、ID、长度与格式；应用需自行校验。

## 危险性与边界

- 导出文件是**明文**：文档、CLI、函数文档都要标注；不默认写当前目录，不自动上传。
- 导出不是备份：不含 key 链、`db_id`、日志、游标，不能恢复同步身份；跨设备仍用 `clone_backup` 或配对。
- 导入会在同步库中表现为一次大规模本机写入，日志会持续增长，之后由正常裁剪处理。
- 导入不清理任何既有数据；`Replace` 只覆盖同 ID 记录。

## API 面

```rust
impl Engine {
    pub fn export_records(&self, target: &Path, options: &ExportOptions) -> Result<ExportInfo>;
    pub fn import_records(&self, source: &Path, options: &ImportOptions) -> Result<ImportReport>;
}

// facade
impl Kernel {
    pub async fn export_records(&self, target: PathBuf, options: ExportOptions) -> Result<ExportInfo>;
    pub async fn import_records(&self, source: PathBuf, options: ImportOptions) -> Result<ImportReport>;
}
```

C ABI（增量符号，`SHIO_ABI_VERSION` 保持 1）：

```c
int32_t shio_export(shio_kernel *kernel, const char *target_path,
                    const uint8_t *options_json, size_t options_len,
                    uint8_t **out_json, size_t *out_json_len);
int32_t shio_import(shio_kernel *kernel, const char *source_path,
                    const uint8_t *options_json, size_t options_len,
                    uint8_t **out_json, size_t *out_json_len);
```

- export options：`{"namespace":"…","collection":"…","include_deleted":false,"overwrite":false}`；
  未知字段拒绝（与 `shio_open` 一致）。
- import options：`{"mode":"merge"|"replace","dry_run":false,"batch_size":256}`。
- 报告 JSON：export `{"format_version":N,"records":N,"deleted":N,"collections":N,"bytes":N}`；
  import `{"records":N,"applied":N,"skipped":N,"deleted":N,"collections":N,"bytes":N,"dry_run":bool}`。
- 格式错误映射 `SHIO_CORRUPTED`，参数错误 `SHIO_INVALID_ARGUMENT`，其余沿用既有映射。

## 实现落点

- 新子模块 `crates/shio-core/src/store/engine/engine_transfer.rs`：格式编解码、两遍导入、
  导出扫描；`TempPath` 从 `engine_backup.rs` 提升为 `pub(super)` 复用。
- 类型定义放 `engine.rs`，从 `store` 与核心根导出；facade 在 `kernel.rs`。
- 新增依赖：`base64`（仅标准字母表）；`sha2` 已在 workspace。
- C ABI 在 `ffi.rs` + `include/shio.h`；harness 增加 `transfer` 模式。

## 验收

- core：v1↔v2 往返、墓碑、Unicode 名称、空集合、二进制值、值长度边界；Merge 跳过与幂等；
  Replace 覆盖与 rev 不回退；`dry_run` 零写入；截断/哈希错/计数不符/坏行在写入前失败；
  只读、维护窗口、超大值、目标已存在；同步库导入后对端能拉到导入的记录。
- facade：async 包装、关闭后行为、只读库导入报错、导出的文件可被新库导入。
- FFI：符号往返、options JSON、报告 JSON、错误码；失败不写文件。
- Android harness：`transfer` 模式（源库含墓碑 → 导出 → 新库导入 → 读回；dry-run 不写；
  重复导出目标被拒）；x86_64 模拟器与 arm64 真机。
- `cargo test --workspace --all-features`、`fmt --check`、`clippy -D warnings`、`cc -fsyntax-only` 通过。
- 文档：`api.md` §3/§4/§8/§9/§12、`data.md`（导出不是备份、不含密钥）、`notes.md`（危险清单与
  格式）、`阶段计划.md`、`STATE.md`、决策 1 条。
