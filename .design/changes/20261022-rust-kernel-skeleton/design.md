# 20261022-rust-kernel-skeleton

<!-- 本文件是本次变更合并进 SPEC 的终稿：小节标题与 `.design/SPEC.md` 的概念节一一对应。 -->

## 技术栈

- 语言：Rust（edition 2021，MSRV 1.90；工具链由 `rust-toolchain.toml` 固定为 1.95.0，带 `rustfmt` 与 `clippy`）
- 工作区：纯 workspace（无根包），成员为 `crates/*` 与 `tests`；依赖集中在根 `workspace.dependencies`，版本锁定于 `Cargo.lock`
- 存储引擎：redb 4.4（单文件、嵌入式、MVCC、单写多读）
- 序列化：serde 1 + serde_json 1（记录体编码，落盘前整体加密）
- 异步：tokio 1.54（仅外层 facade；core 保持同步）
- 加密：Argon2id（口令派生，argon2 0.6）、HKDF-SHA256（子密钥，hkdf 0.13 + sha2 0.11）、XChaCha20-Poly1305（AEAD，chacha20poly1305 0.11）
- 错误：thiserror 2；标识：UUIDv7（uuid 1.28）；随机数：getrandom 0.4；密钥清零：zeroize 1.9；口令封装：secrecy 0.10
- 稳定性：`shio-core` 与 `shio` 使用 `#![forbid(unsafe_code)]`；可恢复失败只返回错误，不以 panic 表达

## 目标

- 可嵌入的加密持久化内核：命名空间 → 集合 → 记录，单文件 redb，记录级 AEAD。
- 内核同步、facade async；两者禁止 unsafe，可恢复失败一律以 `Result` 返回。
- 当前形态：workspace、类型契约与纯逻辑层（模型、错误、字节编码）已落地；加密运算、redb 事务与 facade 的 `open`/读写尚未接线。
- 记录格式 `format=1` 从首个实现起保持向后兼容；API 在 1.0 前允许调整。

## 约束

- 稳定性优先：内核不得因未定义行为、越界、未处理错误或 panic 而崩溃；可恢复失败一律以 `Result` 返回。
- 非测试代码禁用 `unwrap` / `expect` / `panic!`；所有落盘输入视为不可信，先校验后使用。
- 静态护栏由 workspace lints 机械执行：`unsafe_code = "forbid"`，clippy 的 `unwrap_used` / `expect_used` / `panic` / 占位宏 / `dbg_macro` 均为 deny；测试代码局部放宽。
- `shio-core` 与 `shio` 禁止 unsafe；FFI 若引入 unsafe，必须隔离、审计并证明无 UB，且需用户批准。
- 密钥、口令、明文不得出现在日志、错误信息或 panic 信息中。
- 单写多读；每次公开写操作必须原子提交。
- `format=1` 的记录布局与 AAD 是兼容性契约；变更需提升版本并提供迁移。
- 不引入服务端或网络依赖；内核可离线运行。

## 分层与目录结构

依赖方向严格单向，core 不依赖 tokio，保证 Android、服务端与未来守护进程复用同一内核。

```
shio/
├── Cargo.toml                  # 纯 workspace：members = crates/*, tests
├── rust-toolchain.toml         # 固定 1.95.0，要求 rustfmt/clippy
├── rustfmt.toml
├── crates/
│   ├── shio-core/              # 同步内核（forbid unsafe）
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── error.rs        # 错误分类，全量
│   │       ├── model.rs        # 名称校验、RecordId、Timestamp、meta/分页类型，全量
│   │       ├── codec.rs        # 复合键、34 字节 header、RecordBlob 视图、三类 AAD，全量
│   │       ├── crypto/         # kdf.rs / aead.rs / keys.rs：类型已定型，运算未接线
│   │       └── store/          # meta / keys / catalog / records 四张表定义，事务未接线
│   ├── shio/                   # async facade（forbid unsafe）
│   │   └── src/{lib,kernel,collection}.rs
│   ├── shio-ffi/               # C ABI：文档占位，受 unsafe 约束未实现
│   └── shio-sync/              # 多端同步：文档占位
└── tests/                      # shio-tests：跨 crate 集成测试包
    ├── src/lib.rs
    └── smoke.rs
```

- `shio-core`：无 async、无 tokio；持有 redb 与密钥材料，所有操作为同步函数。当前 `model`/`error`/`codec` 完整可用，`crypto` 与 `store` 只有类型与表定义。
- `shio`：把 core 的阻塞调用经 `spawn_blocking` 暴露为 async；当前只提供参数、凭据与句柄类型，名称校验在构造句柄时完成，触及存储的方法尚不存在。
- `shio-ffi`：仅为 C++/Vulkan GUI 与 Android 提供 C ABI，不承载业务逻辑；未取得 unsafe 例外前不导出任何符号。
- `shio-sync`：后续接入，只依赖 core 的模型与存储接口，不修改记录格式。
- `tests`：`autotests = false` 的成员包，通过显式 `[[test]]` 承载跨 crate 用例。

## 稳定性与错误处理

内核稳定性是硬约束，不是优化项：

- 所有可恢复失败通过 `Result` 返回，不用 panic/abort 表达业务失败。
- 非测试代码禁止 `unwrap` / `expect` / `panic!`；仅当不变式已由类型或前置校验保证、失败即代码缺陷时，才允许 `debug_assert` 后返回 `Internal`。
- 落盘数据一律视为不可信：解析 header、键、长度、AAD、JSON 前先校验；损坏或恶意输入只返回 `Corrupted` / `Deserialize`，不得越界或崩溃。
- 长度与偏移计算使用 `checked_*`，防止整数溢出；不靠 unsafe 做性能捷径。
- 内存分配失败、文件锁冲突、磁盘只读等环境错误都返回错误，不 abort。
- FFI 边界（未来）必须用 `catch_unwind` 把 panic 转为错误码，绝不让 panic 跨 C ABI 传播。
- 测试覆盖截断值、随机字节、错误口令、并发关闭与磁盘只读；关键编解码做 fuzz/property 测试。
- 静态护栏：workspace lints 拒绝 `unsafe`、`unwrap` / `expect` / `panic!` 与未实现的占位宏，新增 crate 默认继承。
- 编解码用例必须覆盖损坏输入：魔数不符、未知格式版本、`key_id` 越界、截断、`ct_len` 与总长不符、尾随字节。

## 构建与验证

- 工具链由 `rust-toolchain.toml` 固定（1.95.0），MSRV 1.90（redb 4.4 的要求）；低于 MSRV 时 cargo 直接拒绝。
- `cargo build --workspace` 构建全部 crate，产物落 `target/`（不入库）。
- `cargo test --workspace`：当前 29 个用例（core 23、facade 3、跨 crate 集成 3）。
- `cargo clippy --workspace --all-targets -- -D warnings` 与 `cargo fmt --all -- --check` 是合并前的必过项。
- 依赖版本由 `Cargo.lock` 锁定并随仓库提交；升级依赖需重新跑上述三项检查。
