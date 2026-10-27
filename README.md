# shio

> [!NOTE]
> 此文件不允许ai修改

## 一句话

**shio** 是一个可嵌入的加密持久化内核：单文件 redb、命名空间 → 集合 → 记录三层寻址、记录级 AEAD，附带多端同步的字节层与稳定的 C ABI，可同时服务桌面 Rust 应用、C++/Vulkan GUI 与 Android。

## 项目现状

| 项 | 状态 |
|---|---|
| 包版本 | `0.3.0`（workspace 统一版本，`publish = false`） |
| 阶段 | 研发阶段，API 在 1.0 前允许调整 |
| 语言 / MSRV | Rust 2021，MSRV 1.90；工具链由 `rust-toolchain.toml` 固定为 `1.95.0` |
| 平台 | 桌面（Linux 已验证）+ Android（`aarch64-linux-android` 真机、`x86_64-linux-android` 模拟器） |
| 依赖取向 | 无服务端、无网络依赖；内核可完全离线运行 |
| 稳定性 | 非测试代码禁止 `unsafe` / `unwrap` / `expect` / `panic!`，由 workspace lints 机械保证 |

已完成的能力：加密存储与 CRUD、改口令、数据密钥轮换（可中断续跑）、真正的只读打开、原子批量、游标分页、多端同步（配对、变更日志、快照、轮换、ack/退役）、在线备份与恢复、同设备恢复与多设备克隆、离线物理压缩、自动日志裁剪、设备解绑、明文导出/导入、C ABI v1、独立 CLI `shio-tool`。

## 特性

- **加密不是可选项**：口令经 Argon2id 派生 KEK，解包随机数据密钥（DK），再由 HKDF 派生子密钥；每条记录独立 24 字节 nonce 的 XChaCha20-Poly1305 加密。
- **AAD 绑定位置**：记录无法在命名空间、集合或 ID 之间搬运；header 篡改即解密失败。
- **原子写**：每次公开写操作是一个 redb 写事务；`batch` 任一项失败则整批回滚。
- **墓碑与 LWW**：`delete` 写墓碑而非物理删除；同步冲突按 `(updated_at, rev, blob)` 收敛。
- **多密钥会话**：轮换中途崩溃，meta 仍指向旧 key 时已迁移记录也能读；重跑从断点继续。
- **可嵌入**：内核同步、facade async；core 不依赖 tokio，Android、服务端与守护进程复用同一实现。
- **明确的边界**：内核不提供网络、TLS、GUI、索引或插件运行时。

## 快速开始（Rust facade）

```toml
[dependencies]
shio = { path = "crates/shio" }
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

```rust
use serde::{Deserialize, Serialize};
use shio::{Credential, Kernel, ListOptions, OpenOptions};

#[derive(Debug, Serialize, Deserialize)]
struct Note {
    title: String,
    body: String,
}

#[tokio::main]
async fn main() -> shio::Result<()> {
    let kernel = Kernel::open(
        OpenOptions::new("shio.redb").create(true),
        Credential::password("correct horse battery staple"),
    )
    .await?;

    let notes = kernel.namespace("notes")?.collection::<Note>("note")?;

    let (id, meta) = notes
        .insert(&Note { title: "第一篇".into(), body: "hello shio".into() })
        .await?;
    println!("inserted {id} rev={}", meta.rev);

    if let Some(stored) = notes.get(&id).await? {
        println!("{} / {}", stored.value.title, stored.value.body);
    }

    for item in notes.list(ListOptions::default()).await?.items {
        println!("{} rev={}", item.id, item.rev);
    }

    kernel.close().await?;
    Ok(())
}
```

可直接运行的完整示例：

```sh
cargo run -p shio --example notes
cargo run -p shio --features sync --example sync   # 配对 + 变更拉取
```

要点：

- `Kernel` 是内部 `Arc` 的廉价可克隆句柄，`Send + Sync`；`namespace` / `collection` 只做名称校验，不访问磁盘，集合在首次写入时登记。
- `insert` 自动生成 UUIDv7，`put` 指定 ID 做 upsert；`delete` 写墓碑，`purge` 才物理删除（同步启用后永久禁止）。
- `close().await` 等待在途操作结束；之后新操作返回 `Closed`，重复调用返回 `Ok(())`。
- `read_only(true)` 以 redb 只读库打开，不持有写锁，可用于多进程只读并发。

## 直接使用内核

不需要 async 时可直接用 `shio-core`：

```rust
use shio_core::store::{Engine, EngineOptions};

let engine = Engine::open(EngineOptions::new("shio.redb").create(true), b"password")?;
let (id, meta) = engine.insert("notes", "note", &note)?;
```

（片段示意：`note` 为实现 `Serialize` 的结构体，与 facade 示例中的 `Note` 相同。）

- `insert` / `put` / `get` / `delete` / `purge` / `list` / `batch` / `collections` / `stats` / `flush` 均为同步函数。
- `*_raw` 接口收发已序列化的明文，供 facade 跨线程使用。
- 同步面提供 `enable_sync` / `changes_since` / `apply_page` / 快照 / 轮换 / 配对；维护面提供 `backup` / `restore` / `migrate_all_with_options` / `migrate_step` / `migrate_tick`。
- `EngineOptions::kdf` 只在**创建时**生效；已存在的库以落盘参数为准。

## 多端同步

同步只定义**字节层**，不持有连接、不带后台线程，网络与重试由应用负责。

```sh
cargo run -p shio --features sync --example sync
```

流程概览：

1. A 设备 `enable_sync`（库升级到不可回退的 `format=2`），生成配对包；
2. B 设备 `open_with_pairing` 拿到整条密钥链与新 `device_id`，成为 `format=2` 库；
3. 本地写在同一事务内追加 `kind=0` 变更日志，`changes_since` 拉页、`apply_page` 整页原子应用；
4. 日志被裁剪或新设备基线时走 `snapshot_start` / `snapshot_page` / `apply_snapshot`；
5. 冲突按 LWW `(updated_at, rev, blob)` 收敛，墓碑保留删除事实。

配套能力：

- `shio-sync` 提供 `SHY2` 帧与 Crockford Base32 配对文本；
- 自动日志裁剪：拉取方上报 `kind=3` 进度帧写入 `heard/<device>`，全部已知对端都报告后才裁剪，且永远保留最新一条；
- `forget_device` 显式解绑对端（删除 `peer/` / `cur/` / `heard/` / `ack/` / `mig/`），**不是**远程撤销，也不删记录；
- `clone_backup` 用于「第二台设备」，保留 `db_id` / 密钥链 / 记录，换新 `device_id` 并重建完整日志。

## 备份、压缩与维护

| 场景 | 入口（facade） | 说明 |
|---|---|---|
| 同设备备份/恢复 | `Kernel::backup` / `Kernel::restore` | 在线一致性快照，保留 `db_id` / `device_id` / 游标 |
| 新设备克隆 | `shio::clone_backup` | 换 `device_id`，可按原口令打开 |
| 物理文件压缩 | `shio::compact_file` | 离线入口，要求所有句柄已关闭，取 `.lock` 独占 |
| 维护窗口 | `begin_maintenance` / `migrate_step` / `end_maintenance` | 窗口内普通写返回 `Busy`，读放行 |
| 合作式调度 | `migrate_tick(to, budget, max_records)` | 预算内多批推进，不冻结写，可与日常读写交错 |

迁移使用持久化游标：取消、崩溃或重开后，同一目标 key 从未扫描位置继续，不重扫已提交范围。

## 明文导出 / 导入（危险操作）

导出是**明文 JSONL**，用于迁移与排障，**不能当备份用**（不含 `db_id`、密钥链、日志与游标）。

```sh
# 库侧接口
# Kernel::export_records(target, ExportOptions)
# Kernel::import_records(source, ImportOptions)

# 独立 CLI（仅桌面，不进 Android 构建）
cargo run -p shio-tool -- export <db> --out <file>          # 覆盖需 --force
cargo run -p shio-tool -- import <db> <file> --dry-run      # 预览
cargo run -p shio-tool -- import <db> <file> --yes          # 真正写入
cargo run -p shio-tool -- verify <file>                     # 无需库与口令
cargo run -p shio-tool -- stats  <file>
```

- 文件尾有精确行数与 SHA-256，任何一行被改动都会校验失败。
- 导入是「两遍处理」：先完整校验，再按批写入；`--dry-run` 只跑第一遍。
- 口令只从 `--password-stdin`、`SHIO_PASSWORD` 或终端不回显读取，**永不进 argv**。
- 退出码：`0` 成功、`1` 运行失败、`2` 用法错误。

## C ABI（v1）

面向 C++/Vulkan GUI 与 Android，头文件为 `crates/shio-ffi/include/shio.h`。

```c
#include "shio.h"

shio_kernel *k = NULL;
uint8_t *out = NULL;
size_t out_len = 0;

static const uint8_t PW[] = "correct horse battery staple";
static const uint8_t OPT[] = "{\"create\":true}";

int32_t rc = shio_open("shio.redb", PW, sizeof(PW) - 1, OPT, sizeof(OPT) - 1, &k);
if (rc != SHIO_OK) {
    /* 错误详情是线程局部的，必须在发起调用的同一个线程读取 */
    char err[256];
    shio_last_error(k, err, sizeof err);
    /* ... */
}

/* ... 使用 k ... */
shio_close(k);              /* 等待在途操作，但不释放句柄 */
shio_kernel_destroy(k);     /* 释放；此前确保没有其它线程在用 */
shio_free(out, out_len);
```

约定：

- 全部是**阻塞**调用；Android 必须从 worker 线程调用，`open` / `close` / 改口令 / 轮换会等待磁盘与 KDF。
- 句柄不透明；字符串 NUL 结尾 UTF-8，载荷/口令/ID 显式带长度。
- 返回缓冲区由调用方 `shio_free` 释放，释放前清零。
- `SHIO_ABI_VERSION` 为 `1`；新增能力只加符号，不改已有签名。
- 错误码覆盖 `SHIO_INVALID_KEY`、`SHIO_LOCKED`、`SHIO_BUSY`、`SHIO_READ_ONLY`、`SHIO_SYNC_RESYNC_REQUIRED`、`SHIO_CANCELLED` 等。
- `shio-ffi` 是全仓唯一的 `unsafe` 边界，仅限裸指针校验、句柄转换、缓冲区所有权与 `catch_unwind`。

## 架构与 crate 划分

依赖方向严格单向：

```
shio (async facade, tokio)
 ├─ shio-core (同步内核: 模型/编码/加密/redb)  ← 也可被直接使用
 └─ shio-sync (SHY2 帧/配对文本, 可选 feature)
shio-ffi (C ABI v1) ── shio (sync feature)
shio-tool (独立 CLI) ── shio
tests (跨 crate 集成测试包)
```

| crate | 职责 |
|---|---|
| `shio-core` | 同步内核：`model`、`error`、`codec`/`codec2`、`crypto`（KDF/HKDF/AEAD）、`store`（meta/keys/catalog/records/changes/sync_state/engine） |
| `shio` | async facade：`Kernel` / `Namespace` / `Collection<T>`，`spawn_blocking`，关闭等待，`sync` feature |
| `shio-sync` | `SHY2` 帧、配对 secret、拥有型 `SyncHandle`；不引入网络/TLS/后台线程 |
| `shio-ffi` | 阻塞 C ABI v1，不承载业务逻辑 |
| `shio-tool` | 明文导出/导入/校验/统计 CLI，仅桌面 |
| `tests` | `autotests = false` 的成员包，显式 `[[test]]` 承载跨 crate 用例 |

## 数据模型与存储

```
Namespace（notes）→ Collection（note）→ Record（UUIDv7 + JSON 载荷）
```

- 记录 = **明文 header**（版本、标志、`rev`、时间戳、`key_id`）+ **加密载荷**。
- 名称 1–64 字节，字符集 `[a-z0-9._-]`，不得以 `.` 开头。
- 记录体默认上限 16 MiB，可配置；超限报错，不截断。
- 单文件 redb；format 1 用 `meta` / `keys` / `catalog` / `records`，format 2 追加 `meta2` / `keys2` / `changes` / `sync`。
- 复合键长度前缀编码，`a/b` 与 `a`+`/b` 不会歧义；列表只读 header，不解密。
- 存储层不感知业务类型，`T` 只存在于 API 层。

字节布局、AAD 编码与密钥流程见 [`.design/data.md`](../.design/data.md)；单文件导出格式 v1 见 [`.design/SPEC.md`](../.design/SPEC.md)。

## 安全与稳定性

- 口令、密钥、明文不得出现在日志、错误信息或 panic 信息中；解锁失败统一 `InvalidKey`，不给口令预言机。
- 落盘数据一律视为不可信：解析前校验魔数、版本、长度、AAD、JSON；长度计算用 `checked_*`。
- 可恢复失败一律 `Result`；FFI 边界用 `catch_unwind` 把 panic 转为 `SHIO_INTERNAL`。
- 静态护栏：`unsafe_code = "forbid"`（`shio-ffi` 为受控例外），clippy 的 `unwrap_used` / `expect_used` / `panic` / 占位宏 / `dbg_macro` 均为 `deny`。
- 文件锁冲突映射 `Locked`；Android 用旁路 `<db>.lock` + `rustix::fs::flock`（写独占、只读共享）。

## 构建与测试

```sh
cargo build --workspace
cargo test --workspace                       # 101 个用例
cargo test -p shio --features sync           # 追加 facade 配对/同步用例
cargo run -p shio --example notes

# 合并前必过
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p shio --features sync --all-targets -- -D warnings
```

- `cargo test -p shio-tool` 用真实二进制跑端到端，约 20 秒（每库默认 Argon2id 参数）。
- facade 端到端使用默认 Argon2id（64 MiB、t=3）；core 测试用小参数，全部用例 1 秒内跑完。
- 依赖版本由 `Cargo.lock` 锁定并随仓库提交；升级依赖后重跑上述检查。

## Android

- 应用 minSdk **29**；内核 `.so` 按 API 24 构建基线链接，`LOAD` 对齐 16 KB。
- 目标：`aarch64-linux-android`（真机）与 `x86_64-linux-android`（模拟器），NDK r29；`armv7` 暂不支持。
- 交付物是 `libshio_ffi.so` + `include/shio.h`；JNI/Kotlin 包装由应用侧（如 `oxygen`）维护。
- 已有 C harness（`crates/shio-ffi/examples/android_smoke.c`）覆盖 CRUD、跨进程锁、同步配对、迁移取消/续跑、维护模式、压缩、克隆、导出/导入、设备解绑与备份恢复。
- 构建示例（PowerShell）：

```powershell
$bin = Join-Path $env:LOCALAPPDATA 'Android\Sdk\ndk\29.0.14206865\toolchains\llvm\prebuilt\windows-x86_64\bin'
$env:CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER = Join-Path $bin 'aarch64-linux-android24-clang.cmd'
$env:RUSTFLAGS = '-C link-arg=--target=aarch64-linux-android24'
cargo build -p shio-ffi --release --target aarch64-linux-android
```

CI（`.github/workflows/android.yml`）当前只做两个 Android target 的 `cargo check` 与头文件 `-fsyntax-only` 检查；桌面测试尚未进入 CI。（待确认是否补一条 Linux 桌面流水线。）

## 明确不做

- 插件运行时、权限模型、插件间通信
- 网络传输、TLS、后台同步线程、服务端部署
- 全文检索、二级索引、压缩（记录体）
- GUI/渲染、剪贴板监听、Markdown 渲染等应用功能
- 多写进程（单写多读）

## 版本与兼容性

- `format=1` 的记录布局与 AAD 是兼容性契约；不启用同步的旧库无需迁移。
- `enable_sync` 把库升级为 `format=2` 后**不可回退**，旧二进制在 meta 阶段拒绝打开。
- 升级、轮换的每个中断点都保持可读；中途崩溃后相关写操作返回 `Busy`，重跑即可完成。
- `read_only` 真正以 redb 只读库打开，不再持有写锁。
- `sync` 是可选 feature：关闭时默认 API 与依赖不变，开启时新增 `Kernel::sync()`、`create_pairing_offer` 与 `open_with_pairing`。
- 任何影响记录格式或密钥层级的变化都必须追加 `.design/DECISIONS.log`。

## 目录结构

```
.
├── crates/
│   ├── shio-core/     # 同步内核
│   ├── shio/          # async facade + examples
│   ├── shio-sync/     # SHY2 帧与配对
│   ├── shio-ffi/      # C ABI v1（include/shio.h, examples/android_smoke.c）
│   └── shio-tool/     # 明文导出/导入 CLI
├── tests/             # 跨 crate 集成测试
├── .design/           # 设计真相源：SPEC / api / data / 阶段计划 / DECISIONS
├── .agent/            # agent 工作区：契约、状态与踩坑笔记
└── .github/workflows/ # Android 交叉检查
```

## 文档索引

| 文件 | 内容 |
|---|---|
| [`.design/SPEC.md`](../.design/SPEC.md) | 当前设计真相源（稳定层） |
| [`.design/api.md`](../.design/api.md) | 公共 API 契约：句柄、错误、并发语义 |
| [`.design/data.md`](../.design/data.md) | 持久化与加密的字节布局、AAD、密钥流程 |
| [`.design/阶段计划.md`](../.design/阶段计划.md) | 整体路线与已完成阶段 |
| [`.design/DECISIONS.log`](../.design/DECISIONS.log) | 决策记录与取舍理由 |
| [`.design/同步.md`](../.design/同步.md) | 多设备同步应用层方案 |
| [`.agent/notes.md`](../.agent/notes.md) | 命令、用法与踩坑（贡献者必读） |

## License

Workspace 在 `Cargo.toml` 中声明 `MIT OR Apache-2.0`。（待确认：仓库目前尚未包含 `LICENSE-MIT` / `LICENSE-APACHE` 文件，发布前需补齐。）
