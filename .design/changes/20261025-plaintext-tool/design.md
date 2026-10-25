# 20261025-plaintext-tool

<!-- 独立 CLI shio-tool：export / import / verify / stats；明文落盘标为危险操作，不进入内核默认 API。 -->

## 背景

`Engine::export_records` / `import_records` 已经提供明文 JSONL 的读写与校验，但只有 Rust 调用方
能用。运维与普通用户需要不写代码就能：

- 把库导出成可读文件、检查文件是否完整；
- 在导入前预览影响（不写库）；
- 在没有 shio 应用的环境里核对导出内容与统计。

这属于危险能力：一旦落盘就是明文。因此做成**独立二进制**，不链接 `shio-ffi`，不进入 Android
构建，也不扩大内核的默认 API 面。

## 本阶段范围

- 新 crate `crates/shio-tool`，二进制名 `shio-tool`。
- 子命令：`export`、`import`、`verify`、`stats`。
- 口令只从 stdin / 环境变量 / 交互提示读取，**不接受命令行参数**（避免 `ps` 泄露）。
- 导入默认拒绝：必须 `--yes`；`--dry-run` 不需要。
- 导出覆盖必须 `--force`；stderr 始终打印明文警告。
- `verify` / `stats` 不打开数据库，复用内核的校验逻辑。

## 命令

```
shio-tool export <db> --out <file> [--namespace N] [--collection C] [--with-deleted] [--force]
shio-tool import <db> <file> [--dry-run] [--replace] [--batch N] [--yes]
shio-tool verify <file>
shio-tool stats  <file>
```

- `export` 以只读方式打开库（`read_only=true`），输出走内核的临时文件 + 原子改名；目标已存在
  且没有 `--force` 时返回错误，不覆盖。
- `import` 以读写方式打开库；`--batch` 映射到 `ImportOptions::batch_size`；`--replace` 映射到
  `ImportMode::Replace`，默认 `Merge`。
- `verify` 输出 `format=<n> records=<n> deleted=<n> collections=<n> bytes=<n>`；失败时退出码 1。
- `stats` 先完整校验，再按 `(namespace, collection)` 汇总记录数与墓碑数。
- 退出码：`0` 成功，`1` 运行失败（错误/校验失败/口令错误），`2` 用法错误。
- `-h/--help` 与 `-V/--version` 顶层可用；内核版本与工具版本一起打印。

## 口令

按优先级：

1. `--password-stdin`：从标准输入读一行，去掉行尾换行；空口令拒绝。
2. `SHIO_PASSWORD` 环境变量。
3. 终端交互：不回显输入（工具级依赖 `rpassword`）。
4. 都没有（stdin 是管道且未加 `--password-stdin`）→ 用法错误，提示可用的两种方式。

口令在进程内用 `Zeroizing<String>` 搬运，读完即清零副本；不写日志、不回显。

## 危险闸门

- 导出：stderr 先打印「导出文件是明文，包含全部记录值；用完请自行清理」，再写文件。
- 导入：没有 `--yes` 时退出码 2，提示先 `--dry-run` 预览；`--dry-run` 不写库。
- 不提供「删除文件」「上传」「自动清理」等隐式行为；工具只做用户明确要求的那一步。

## 新增内核 API

`verify` / `stats` 需要在不打开数据库的情况下校验文件。为此把导出文件扫描从 `Engine` 方法提炼为
free function，并复用同一个校验实现：

```rust
// shio-core::store（shio 根重导出）
pub fn verify_export(source: &Path, max_value_size: usize) -> Result<ExportInfo>;
```

- 返回的 `ExportInfo` 含 `format_version`、`records`、`deleted`、`collections`、`bytes`。
- 校验规则与 `import_records` 第一遍完全一致（结构、名称、UUID、值编码与长度、计数、SHA-256、
  尾后数据）。
- 不引入新类型、新错误码；`max_value_size` 传 `DEFAULT_MAX_VALUE_SIZE` 即可。

## 依赖

- `shio`（默认 feature，不含 `sync`；format=2 的读写由 core 支持，不需要同步 feature）。
- `serde_json`（`stats` 汇总）、`tokio`（复用 facade 的异步入口）、`zeroize`（口令）。
- `rpassword`（仅工具用于不回显输入；不进入库与 Android 构建）。

## 验收

- `cargo run -p shio-tool -- export db --out file`：退出码 0，文件通过 `verify`。
- `verify` 对合法文件输出正确计数；截断/篡改文件退出码 1。
- `import` 不带 `--yes` 退出码 2 且不写库；`--dry-run` 退出码 0 且库不变；带 `--yes` 后记录可读。
- `export` 目标已存在且无 `--force` 退出码 1；加 `--force` 成功。
- 错误口令退出码 1。
- 未知命令/缺参数退出码 2。
- 集成测试用 `CARGO_BIN_EXE_shio-tool` 跑完整往返；`cargo test --workspace --all-features`、
  `fmt --check`、`clippy -D warnings` 通过。
- 文档：`api.md` §4/§12（`verify_export`）、`notes.md`（工具用法与危险清单）、`阶段计划.md`、
  `STATE.md`、决策 1 条。
