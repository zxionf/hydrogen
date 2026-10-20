# 20261020-encrypted-persistence-core 任务清单

## 设计（本变更，已完成）

- [x] 读取 `.agent/` 契约与 `.design/` 现有设计
- [x] 确认定位、密钥来源、数据组织、API 风格
- [x] 用 `sele new` 创建变更
- [x] 编写 `design.md`（定位、分层、模型、加密、存储、API、平台、同步预留）
- [x] 编写 `.design/api.md`（公共类型、签名、语义、错误、FFI 映射）
- [x] 编写 `.design/data.md`（表、字节布局、AAD、密钥流程、不变量）
- [x] 在 `.design/INDEX.md` 登记新文档
- [x] 追加关键决策到 `DECISIONS.log`
- [x] 用户评审设计与文档（已确认，可折叠关闭）

## 稳定性约束（已纳入设计）

- [x] 明确目标：内核不得因未定义行为、越界、未处理错误或 panic 崩溃
- [x] `shio-core` / `shio` 禁止 unsafe；落盘输入一律视为不可信
- [x] 可恢复失败一律返回 `Result`，非测试代码不用 `unwrap` / `expect` / `panic!`
- [x] FFI 暂缓：若引入 unsafe，需隔离、审计、`catch_unwind` 并经用户批准
- [x] 写入 SPEC 的 `约束` 节

## 后续变更：实现（另开变更承接）

### 准备

- [ ] 安装 Rust 工具链并写 `rust-toolchain.toml`（当前环境无 `rustc`/`cargo`）
- [ ] 初始化 workspace 与 `shio-core`、`shio` 骨架（`shio-ffi`、`shio-sync` 暂不建）
- [ ] 固定依赖版本并生成 `Cargo.lock`

### shio-core

- [ ] `error.rs`：`Error` 枚举、`Display`、错误分类
- [ ] `model.rs`：`RecordId`、`RecordMeta`、`Timestamp`、名称校验
- [ ] `codec.rs`：复合键、header、AAD 的编解码与长度校验
- [ ] `crypto`：Argon2id、HKDF 子密钥、XChaCha20-Poly1305 封装、zeroize
- [ ] `store::meta`：MetaBlob / KeyBlob 读写与初始化
- [ ] `store::records`：RecordBlob 编解码、CRUD、墓碑、purge
- [ ] `store::catalog`：集合登记与枚举
- [ ] `Kernel` 生命周期：open / 解锁 / close / 文件锁
- [ ] 批量写与原子性
- [ ] 改口令与数据密钥轮换
- [ ] 格式版本校验与拒绝未知版本

### shio（async facade）

- [ ] `Kernel` / `Namespace` / `Collection<T>` 句柄
- [ ] `spawn_blocking` 桥接与取消语义
- [ ] `close` 与并发操作的协调

### 测试与稳定性

- [ ] 字节格式黄金测试（固定向量）
- [ ] AAD 篡改、跨命名空间搬运、header 篡改必须失败
- [ ] 错误口令、损坏 key check、未知格式版本
- [ ] CRUD / 墓碑 / purge / 批量原子性
- [ ] 并发读写与关闭竞态
- [ ] 轮换中断后的可恢复性
- [ ] 截断、随机、损坏输入不得 panic 或越界
- [ ] clippy 兜底：非测试代码禁止 `unwrap` / `expect` / `panic!`
- [ ] 关键编解码与解析的 fuzz/property 测试
- [ ] 磁盘只读、分配失败等环境错误返回错误而非 abort
- [ ] Android 目标交叉编译与文件锁验证

### 决策与待定

- [ ] FFI 边界方案（若需要 C ABI）：隔离 unsafe、逐条审计、`catch_unwind`、用户批准
- [ ] 依赖版本与最低 Rust 版本（MSRV）标定
- [ ] Argon2id 参数在低端 Android 设备上的实测与下调策略
