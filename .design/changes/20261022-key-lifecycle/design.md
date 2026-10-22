# 20261022-key-lifecycle

<!-- 本文件是本次变更合并进 SPEC 的终稿：小节标题与 `.design/SPEC.md` 的概念节一一对应。 -->

## 目标

- 可嵌入的加密持久化内核：命名空间 → 集合 → 记录，单文件 redb，记录级 AEAD。
- 内核同步、facade async；两者禁止 unsafe，可恢复失败一律以 `Result` 返回。
- 当前形态：core 与 facade 均已可用——Argon2id/HKDF/AEAD、四张 redb 表、同步 CRUD、异步句柄、改口令、数据密钥轮换、真正的只读打开；多端同步与 FFI 仍是占位。
- 记录格式 `format=1` 从首个实现起保持向后兼容；API 在 1.0 前允许调整。

## 加密与密钥

密钥层级：

```
口令 ──Argon2id(salt, 参数)──▶ KEK（32 字节）
KEK ──XChaCha20-Poly1305 解包──▶ DK（32 字节随机数据密钥）
DK ──HKDF-SHA256(info 区分用途)──▶ 记录密钥 / 元数据密钥 / 校验密钥
记录密钥 ──XChaCha20-Poly1305(random 24B nonce, AAD)──▶ 记录密文
```

- **口令不落盘**；落盘的是 Argon2id 参数、16 字节盐、被 KEK 包裹的 DK（48 = 32 + 16 tag），以及 key check（32 字节密文，明文固定为 `shio-keycheck-v1`）。
- 子密钥派生：HKDF-SHA256，盐固定 `shio/v1/hkdf-salt`，info 分别为 `shio/v1/record-key`、`shio/v1/meta-key`、`shio/v1/check-key`；三者在用途上隔离。
- **记录级 AEAD**：每条记录独立 nonce（24 字节随机），AAD 绑定命名空间、集合、记录 ID 与明文 header（域分隔符 `shio/v1/record`）。
- AAD 使记录无法在命名空间/集合/ID 之间搬运，header 篡改会导致解密失败；keywrap 与 keycheck 各自使用独立域分隔符。
- 解锁失败统一返回 `InvalidKey`，不区分“口令错误”和“数据损坏”，避免口令预言机。
- **多密钥会话**：打开时扫描 `keys` 表并用当前行的盐与参数派生 KEK，解包全部密钥行；记录按自身 header 的 `key_id` 选择记录密钥解密。这样轮换中途崩溃后，meta 仍指向旧密钥时已迁移记录也能读。
- **改口令**只重新包裹 DK：生成新盐与新 KEK，用新 nonce 重写该行的 salt/参数/nonce/包裹值，单行原子提交，代价 O(1)，不触碰记录；存在多行 key（轮换未完成）时返回 `Busy`。
- **轮换数据密钥**：`key_id` 递增（超上限拒绝），先用当前 KEK 包裹新 DK 并写入新 key 行，再按 256 条一批重加密记录（改写 header 的 `key_id` 与 nonce），最后单事务更新 meta 的 `key_id`/key check 并删除旧行；迁移期间持有会话写锁，崩溃后重跑可复用已写入的新行继续。
- 会话保留 KEK 与 DK 供改口令/轮换使用；所有密钥材料用 `zeroize` 在离开作用域时清零；错误信息、日志、panic 信息不得包含密钥或明文。
- Argon2id 默认参数：`m=65536 KiB, t=3, p=1`，盐 16 字节；参数随库存储，允许未来升级与按设备下调（包括测试用的小参数）。
- 只读打开同样需要口令：解包与 key check 不变，只是句柄不允许写。

完整字节布局与 AAD 编码见 `.design/data.md`；API 契约见 `.design/api.md`。

## API 风格

- 对外 async（tokio），内核同步；阻塞调用统一走 `spawn_blocking`，不占用 executor 线程。
- core 的同步入口是 `store::Engine`：`Engine::open(EngineOptions, password)`，选项含路径、是否创建、是否只读、明文上限、`app_id`、建库 KDF 参数与 redb 页缓存。
- `Engine` 提供 `insert`/`put`/`get`/`delete`/`purge`/`list`/`batch`/`collections`/`stats`/`flush`、密钥生命周期的 `change_password`/`rotate_data_key`，以及收发已序列化明文的 `insert_raw`/`put_raw`/`get_raw`/`batch_raw`；`Send + Sync`。
- facade 的 `Kernel::open(OpenOptions, Credential)` 完成参数校验、口令派生与解锁；`Credential::password` 接收 `Into<SecretString>`，`Debug` 输出脱敏。
- `Kernel` 是廉价可克隆句柄（内部 `Arc`），`Send + Sync`；提供 `namespace`、`flush`、`close`、`stats`、`change_password`、`rotate_data_key`。`close` 标记关闭并等待在途操作结束，之后新操作返回 `Closed`，重复调用返回 `Ok(())`。
- `Namespace` 与 `Collection<T>` 是从 `Kernel` 派生的轻量句柄，不复制连接；名称在构造时校验，不访问磁盘。`Namespace` 提供 `collections`，`Collection<T>` 提供 `insert/put/get/delete/purge/list/batch`。
- 线程边界：写路径在调用线程把 `T` 序列化为 `Zeroizing<Vec<u8>>`，再由阻塞线程加密落盘；读路径在阻塞线程解密并反序列化。`T` 只需 `Serialize + DeserializeOwned + Send + Sync + 'static`，不要求 `Clone`。
- `read_only=true` 时 core 的写入口直接返回 `ReadOnly`，底层以 redb 只读库打开、不持有写锁；facade 保留进入阻塞线程前的快速闸门，`create` 与 `read_only` 组合非法。
- `change_password` 与 `rotate_data_key` 都是写操作，经 `spawn_blocking` 执行；只读句柄返回 `ReadOnly`。
- 事务边界：v0.1 以 `batch` 作为唯一原子业务写单元，不暴露长生命周期事务对象；密钥轮换按批提交，但通过多密钥会话保证任意中断点都可读。
- 可恢复错误一律返回 `Result`，不以 panic 表达业务失败；公开签名与错误枚举见 `.design/api.md`。

## 稳定性与错误处理

内核稳定性是硬约束，不是优化项：

- 所有可恢复失败通过 `Result` 返回，不用 panic/abort 表达业务失败。
- 非测试代码禁止 `unwrap` / `expect` / `panic!`；仅当不变式已由类型或前置校验保证、失败即代码缺陷时，才允许 `debug_assert` 后返回 `Internal`。
- 落盘数据一律视为不可信：解析 header、键、长度、AAD、JSON 前先校验；损坏或恶意输入只返回 `Corrupted` / `Deserialize`，不得越界或崩溃。
- 由于所有写操作都会读取并复用已有 header，写入路径同样执行完整解码与校验。
- 长度与偏移计算使用 `checked_*`，防止整数溢出；不靠 unsafe 做性能捷径。
- 内存分配失败、文件锁冲突、磁盘只读等环境错误都返回错误，不 abort；redb 的 `DatabaseAlreadyOpen` 映射为 `Locked`，其余存储错误归入 `Database`。
- DK 解包与 key check 失败映射为 `InvalidKey`；记录结构或认证失败映射为 `Corrupted`；库文件缺失映射为 `NotFound`；轮换未完成导致的工作状态冲突映射为 `Busy`；只读句柄的写操作映射为 `ReadOnly`。
- 密钥轮换的每个中断点都保持可读：新 key 行先落盘，记录按自身 `key_id` 解密，meta 最后切换；重跑轮换会复用已存在的新行并跳过已迁移记录。
- facade 不把 panic 带过线程边界：`spawn_blocking` 的 `JoinError` 统一转为 `Internal`；句柄状态锁与密钥会话锁中毒同样返回 `Internal`。
- 关闭语义：取引擎时登记在途计数，`close` 置空引擎并等待计数归零；已开始的操作持有 `Arc<Engine>` 完成，之后数据库才释放文件锁。
- FFI 边界（未来）必须用 `catch_unwind` 把 panic 转为错误码，绝不让 panic 跨 C ABI 传播。
- 测试覆盖截断值、随机字节、错误口令、并发关闭与磁盘只读；关键编解码做 fuzz/property 测试。当前已覆盖错误口令、截断/篡改的 blob 与记录、分页、批量回滚、句柄关闭、只读拒写、改口令、全量轮换与轮换中断后重跑。
- 静态护栏：workspace lints 拒绝 `unsafe`、`unwrap` / `expect` / `panic!` 与未实现的占位宏，新增 crate 默认继承。
- 编解码用例必须覆盖损坏输入：魔数不符、未知格式版本、`key_id` 越界、截断、`ct_len` 与总长不符、尾随字节。

## 影响与兼容性

- 不改变 `format=1`、AAD 与 blob 布局；已有数据库无需迁移。
- `change_password`、`rotate_data_key` 与 `read_only` 已按 `api.md` 落地；facade 签名不变。
- `read_only` 现在真正以 redb 只读库打开，不再持有写锁，可用于多进程只读并发。
- 轮换中途崩溃会留下两行 key：库仍可读，但 `change_password` 返回 `Busy`；重新执行轮换即可完成并删除旧行。
- API 在 1.0 前允许调整，但任何影响记录格式或密钥层级的变化都必须追加 `DECISIONS.log`。

## 构建与验证

- 工具链由 `rust-toolchain.toml` 固定（1.95.0），MSRV 1.90（redb 4.4 的要求）；低于 MSRV 时 cargo 直接拒绝。
- `cargo build --workspace` 构建全部 crate，产物落 `target/`（不入库）。
- `cargo test --workspace`：当前 66 个用例（core 库内 34、引擎端到端 21、facade 库内 3、facade 端到端 5、跨 crate 集成 3）。
- 引擎端到端使用 `tempfile` 与 8 MiB 的 Argon2id 参数，含 300 条记录的跨批轮换与「篡改记录 → 轮换中断 → 还原 → 续跑」；facade 端到端用默认参数（64 MiB、t=3），5 个用例约 13 秒。
- `cargo clippy --workspace --all-targets -- -D warnings` 与 `cargo fmt --all -- --check` 是合并前的必过项。
- 依赖版本由 `Cargo.lock` 锁定并随仓库提交；升级依赖需重新跑上述三项检查。
