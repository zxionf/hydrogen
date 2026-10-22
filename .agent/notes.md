# 工具

sele CLI用于管理 `.design/` 与 `.agent/` 目录的 C++20 命令行工具，支撑「创建变更 → 追加决策 → 查询状态 → 规则检查 → 归档预览」这一最小闭环。

## 构建与测试

- `cargo build --workspace` 构建全部 crate；产物在 `target/`，已加入 `.gitignore`。
- `cargo test --workspace`：29 个用例（shio-core 23、shio 3、跨 crate 集成 3）。
- 提交前必过：`cargo fmt --all -- --check` 与 `cargo clippy --workspace --all-targets -- -D warnings`。
- 工具链由 `rust-toolchain.toml` 固定为 1.95.0（rustfmt + clippy）；首次在该目录执行 cargo 时 rustup 会自动补齐。MSRV 1.90，因为 redb 4.4 要求。
- 升级任何依赖后重跑三项检查；`Cargo.lock` 随仓库提交。

## 踩坑

- workspace lints 把 `clippy::unwrap_used` 等设为 deny：测试代码必须放宽，库内用 `#![cfg_attr(test, allow(...))]`，集成测试文件用 `#![allow(...)]`，否则 `-D warnings` 直接失败。
- `sele close` 按小节标题折叠，而本仓库 `SPEC.md` 是「一个概念一节」：合并时把 `design.md` 的小节标题写成与 SPEC 概念节同名，close 才会原地替换；否则会把模板四节当新小节追加。
- 禁词检查按子串匹配，`todo!` 这类写法会被当成 TODO 命中；在 `.design/` 里描述 lint 名单时改成「占位宏」之类措辞。

