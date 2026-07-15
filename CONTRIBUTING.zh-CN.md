# 参与 Opsail 贡献

[English](CONTRIBUTING.md) | 简体中文

感谢您帮助改进 Opsail。每次变更应聚焦于一个可观察行为或一个内聚的行动模块。

## 环境要求

- Rust 1.97 或更高版本。
- 使用仓库提交的 `Cargo.lock`；开发和 CI 命令应锁定依赖。

## Workspace 边界

```text
crates/opsail       CLI 解析、输出路由、诊断和退出行为
crates/opsail-read  HTML 获取、正文提取、清洗和结果 schema
```

`opsail` package 是轻量的进程适配层。提取规则、网络、清洗和结果模型属于 `opsail-read`。未来行动在具备内聚的类型化 API 和独立测试后，应成为同级 `opsail-<action>` crate。在多个已实现模块证明有实际需要之前，不引入插件 ABI 或共享框架。

## 库入口

`opsail-read` 提供：

- `read(Input, &ReadOptions)`：异步获取 URL、文件或 stdin 输入。
- `extract_html(html, base_url)`：同步提取内存中的 HTML。

两者都返回 CLI JSON 输出所使用的带版本号 `ReadResult` 模型。

## 开发流程

提交变更前运行完整验证：

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo build --release --workspace --locked
```

## 变更要求

- 新增或更新能够证明行为变化的最小测试。
- 提取 fixture 必须自包含。完整 Markdown golden 的变化需要人工审阅，测试不得自动更新它们。
- 默认测试保持离线；HTTP 行为使用本地 mock server 测试。
- stdout 始终作为数据通道，stderr 始终作为诊断通道。
- 除非修改 `schemaVersion`，JSON schema 只做兼容性字段扩展。
- 获取的 HTML、元数据、链接和提取文本均视为不可信输入。
- 记录不支持的行为和新增的信任边界。
