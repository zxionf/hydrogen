# 20261020-encrypted-persistence-core 交接

## 已完成

- 读取契约、状态、设计规范与阶段计划。
- 与用户确认：先做持久化；记录级 AEAD；serde_json；async tokio；多端/Android/C++ 长期目标；口令 + Argon2id；命名空间 + 集合 + 记录。
- 创建并完成变更 `20261020-encrypted-persistence-core`。
- 写 `design.md`、`.design/api.md`、`.design/data.md`，并登记 `.design/INDEX.md`。
- 追加 8 条决策到 `.design/DECISIONS.log`。
- 用户确认设计，`sele close` 折叠进 `SPEC.md`（技术栈、约束替换，其余追加）。
- `sele lint` 通过。

## 未完成

- 实现尚未开始。
- `.agent/work/20261020-encrypted-persistence-core/tasks.md` 中保留了实现待办，需新开变更承接。
- FFI 未落地：C ABI 需要 unsafe，需先确定隔离与审计方案。

## 关键选择

- 记录级 AEAD：XChaCha20-Poly1305；DK 由 KEK 包裹，HKDF 派生记录/元数据/校验子密钥。
- 口令经 Argon2id 派生 KEK；会话内保留 KEK 以支持改口令与轮换。
- redb 单文件四表：meta / keys / catalog / records。
- 记录 = 明文 header + nonce + 密文；AAD 绑定命名空间、集合、ID、header。
- UUIDv7 + rev + 墓碑，为同步预留；purge 仅本地使用。
- core 同步，`shio` 用 tokio + spawn_blocking。
- 稳定性硬约束：core/facade 禁止 unsafe；可恢复失败返回 `Result`；落盘输入先校验；非测试代码禁用 `unwrap`/`expect`/`panic!`。

## 下一步

1. 安装 Rust 工具链，写 `rust-toolchain.toml`。
2. 用 `sele new --slug core-implementation` 新开实现变更。
3. 先做 `shio-core` 的 codec + crypto + meta/keys，再做 records 与生命周期。
4. 实现前决定 FFI 边界（是否需要 C ABI、unsafe 如何隔离）。

## 阻塞

- 本机缺少 `rustc` / `cargo`。
- `shio-ffi` 受安全约束限制，需要用户批准的隔离方案。
