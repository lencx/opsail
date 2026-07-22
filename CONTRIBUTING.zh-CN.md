# 参与 Opsail 贡献

[English](CONTRIBUTING.md) | 简体中文

感谢您帮助改进 Opsail。每次变更应聚焦于一个可观察行为或一个内聚的行动模块。

## 环境要求

- Rust 1.97 或更高版本。
- 使用仓库提交的 `Cargo.lock`；开发和 CI 命令应锁定依赖。

## Workspace 边界

```text
crates/opsail          原生 CLI 解析、协议路由、诊断和退出行为
crates/opsail-chrome   Chrome 可执行文件查找、自有生命周期、CDP 与 DOM 捕获
crates/opsail-read     来源编排、HTML 获取、正文提取、清洗和结果 schema
crates/opsail-refit-codex Codex refit 生命周期、目标安全校验与 renderer 集成
packages/node          公开 `opsail` npm facade 与原生二进制解析
skills/bootstrap-opsail 面向 Agent 的临时安装控制面
skills/opsail          统一的 Opsail 运行时 Agent Skill
```

原生 `opsail` crate 负责统一命令入口，公开的 `opsail` npm package 是轻量进程适配层。生成的 `@opsail/<platform>-<arch>` package 仅承载二进制实现，不是额外 API。`opsail-chrome` 负责所有 Chrome 专属机制：跨平台可执行文件查找、隔离进程的启动与清理、借用 CDP 连接、target 生命周期、导航等待和渲染后 DOM 捕获；它不负责正文提取或清洗。`opsail-read` 选择并校验来源，获取非浏览器 HTML，将浏览器捕获委托给 `opsail-chrome`，并负责提取、清洗和 `ReadResult`。`opsail-refit-codex` 负责 Codex 专属的应用身份、进程与 loopback CDP 校验、renderer bridge、选择器、额度语义、本地化资源和 UI payload；在第二个适配器证明存在稳定共享契约前，refit 生命周期保持为其内部模块。未来行动在具备内聚的类型化 API 和独立测试后，应成为同级 `opsail-<action>` crate，再通过现有 CLI、npm facade 与统一运行时 Skill 暴露。在多个已实现模块证明有实际需要之前，不引入插件 ABI 或共享框架。

## 库入口

`opsail-chrome` 提供两个所有权明确的入口：

- `capture_chrome(&ChromeSource, &CaptureOptions)`：查找或使用已配置的可执行文件，以隔离的临时 profile 启动 Chrome，捕获一个页面，再停止自有浏览器。
- `capture_cdp(&CdpSource, &CaptureOptions)`：借用调用方管理的 endpoint，不拥有该浏览器或其现有 target。

可执行文件解析顺序必须保持为：显式路径、`OPSAIL_CHROME_PATH`、平台候选位置与 `PATH`。自有启动支持 macOS、Linux 与 Windows，使用 loopback 上动态分配的调试端口，不复用用户 profile，也不得静默添加 `--no-sandbox`。

借用 CDP 的清理只能关闭 Opsail 自己创建的 target。正常完成时应 detach 并清理 target；若捕获 future 被突然取消或进程被终止，清理只能 best-effort，调用方始终保留该浏览器的所有权。

`opsail-read` 提供：

- `read(ReadSource, &ReadOptions)`：异步获取 URL、文件、stdin、已捕获 HTML、借用 CDP 或自有 Chrome 输入。
- `extract_html(html, base_url)`：同步提取内存中的 HTML。

`opsail-read` 的两个入口都返回 CLI JSON 输出所使用的带版本号 `ReadResult` 模型。浏览器捕获保留不同的来源信息：自有启动使用 `SourceKind::Chrome`，借用 endpoint 使用 `SourceKind::Cdp`。

`opsail-refit-codex` 暴露通过 `CodexRefitConfig` 配置的 `CodexRefit`，提供异步的 `enable_usage`、`disable_usage`、`status` 与只读 `doctor` 操作。适配器目前只支持经过校验的 macOS 应用 `/Applications/ChatGPT.app`。Enable 默认为只附加；只有显式使用类型化 `LaunchIfStopped` 策略时才可启动一次已校验的可执行文件，但不得退出、kill、重启、重载、修改或重新签名该应用。`doctor`、`status` 与 `disable` 绝不启动应用。连接必须只使用 `127.0.0.1`，并且只有应用签名、进程祖先链、renderer URL 与 shell、侧栏以及预期本机 bridge 全部通过校验后才能继续。Codex 协议名、选择器、额度语义、本地化 JSON 和 UI 文案均属于此 crate，不进入共享模块。

## 开发流程

提交变更前运行完整验证：

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo build --release --workspace --locked
npm test --prefix packages/node
npm run pack:check --prefix packages/node
```

## 变更要求

- 新增或更新能够证明行为变化的最小测试。
- 提取 fixture 必须自包含。完整 Markdown golden 的变化需要人工审阅，测试不得自动更新它们。
- 默认测试保持离线；HTTP 行为使用本地 mock server 测试。
- 每个 Rust crate 都应显式声明版本并独立管理；仅在该 crate 的发布契约发生变化时升级，同时明确更新依赖方的版本约束。
- stdout 始终作为数据通道，stderr 始终作为诊断通道。
- 除非修改 `schemaVersion`，JSON schema 只做兼容性字段扩展。
- 获取的 HTML、元数据、链接和提取文本均视为不可信输入。
- 记录不支持的行为和新增的信任边界。
- `bootstrap-opsail` 临时安装流程独立于 CLI 与 npm 包进行版本管理；bootstrap 行为发生变化时更新其 `metadata.version`。
- 为 CLI 打 release tag 之前，先在 `main` 上更新 `skills/opsail/SKILL.md` 中固定的 Opsail 版本（`compatibility` 与 `metadata`）；`bootstrap-opsail` 从最新 Release 安装 CLI，而 runtime Skill 取自 `main`。
- 将 `metadata.openclaw` 与 `metadata.hermes` 视为有意保留的宿主扩展。严格的 Agent Skills 元数据兼容需要生成宿主投影；在替代其 gating、安装器与发现行为之前，不要将这些对象字符串化或移除。
