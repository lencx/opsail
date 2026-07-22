<p align="center">
  <img src="https://raw.githubusercontent.com/lencx/opsail/main/assets/opsail-logo.png" alt="Opsail 标志" width="160">
</p>

<h1 align="center">Opsail</h1>

<p align="center"><strong>让 Agent 可以信赖的原生工具。</strong></p>

<p align="center">
  <a href="https://github.com/lencx/opsail/blob/main/README.md">English</a> | 简体中文
</p>

<a href="https://www.buymeacoffee.com/lencx" target="_blank"><img src="https://cdn.buymeacoffee.com/buttons/v2/default-blue.png" alt="Buy Me A Coffee" style="height: 40px !important;width: 145px !important;" ></a>

Opsail 是一个模块化原生工具集，通过统一的命令行入口，为软件 Agent 提供小而可组合、行为可靠的能力。它使用职责清晰的 Rust crate 隔离内容获取、浏览器控制、正文提取和应用适配，并通过 Node.js 包方便地嵌入同一套原生运行时。

## 核心特色

- **原生且可预测**：长期运行、进程归属、传输、校验和清理均由 Rust 实现，不依赖 shell 脚本或代理服务作为主引擎。
- **能力小而可组合**：每个包只负责一个清晰边界，既可以独立使用，也可以通过 `opsail` CLI 统一调用。
- **面向 Agent 的契约**：命令提供稳定输出、结构化诊断、受控资源占用和适合自动化的安静失败模式。
- **明确的信任边界**：对借用的浏览器、自有进程、远程内容和应用适配，按照各自真实的所有权与安全模型进行校验。
- **默认可逆**：Refit 功能会验证目标，支持幂等执行和完整移除，不修改目标应用包。

## 核心能力

### 读取 HTML

`opsail read` 可以将静态 HTML 或浏览器渲染后的 DOM 转换为易读的 Markdown、经过清理的 HTML 或带版本的 JSON。输入可以来自 URL、文件、stdin、由 Opsail 启动的隔离 Chrome，或显式借用的 CDP 端点。

```sh
opsail read https://example.com/article
opsail read https://example.com/app --launch
```

内容获取、正文提取、结果契约和 Rust API 请参阅 [`opsail-read`](crates/opsail-read/README.md)；Chrome 发现、自有启动、借用 CDP、页面导航和渲染 DOM 捕获请参阅 [`opsail-chrome`](crates/opsail-chrome/README.md)。

### Codex Refit

`opsail refit codex` 提供可逆且经过目标校验的 Codex 适配器。它的首个功能通过 renderer 已有的本地 bridge，在 Codex 左侧栏显示本地化的剩余额度信息，不调用模型，也不修改应用包。

```sh
opsail refit codex enable usage --launch
```

默认 persistent 模式会启动经过校验的后台 manager，并在输出健康报告后返回；`--once` 仍是单次注入，诊断时可显式使用 `--foreground`。

交互式等待会在 `stderr` 中显示当前经过校验的生命周期阶段，最终供程序读取的 JSON 仍只写入 `stdout`。

支持目标、附加与启动模式、生命周期语义、renderer 更新、多语言、安全校验和库 API 请参阅 [`opsail-refit-codex`](crates/opsail-refit-codex/README.md)。

## 包结构

| 包 | 职责 | 文档 |
| --- | --- | --- |
| [`opsail`](https://crates.io/crates/opsail) | 原生 CLI 与统一命令入口 | 运行 `opsail --help` |
| [`opsail-read`](https://crates.io/crates/opsail-read) | 内容获取、正文提取、清理和结果契约 | [README](crates/opsail-read/README.md) |
| [`opsail-chrome`](https://crates.io/crates/opsail-chrome) | 跨平台 Chrome 生命周期、CDP 传输和渲染捕获 | [README](crates/opsail-chrome/README.md) |
| [`opsail-refit-codex`](https://crates.io/crates/opsail-refit-codex) | Codex 适配生命周期、额度语义、多语言和 UI payload | [README](crates/opsail-refit-codex/README.md) |
| Node.js [`opsail`](https://www.npmjs.com/package/opsail) | ESM API 与原生二进制分发 | [README](packages/node/README.md) |

## 安装

从 crates.io 安装 CLI：

```sh
cargo install opsail
```

从 npm 安装 Node.js API 和 CLI：

```sh
npm install opsail
```

预编译原生二进制可从 [GitHub Releases](https://github.com/lencx/opsail/releases/latest) 下载。Agent 宿主可以在明确授权后，使用经过审阅的 [`bootstrap-opsail` Skill](skills/bootstrap-opsail/SKILL.md) 同步 CLI 和运行时 Skill。

## 项目文档

- [内容提取与结果模型](crates/opsail-read/README.md)
- [Chrome 与 CDP 集成](crates/opsail-chrome/README.md)
- [Codex 左侧栏 Refit](crates/opsail-refit-codex/README.md)
- [Node.js API 与打包](packages/node/README.md)
- [开发与贡献指南](CONTRIBUTING.md)

## 许可证

Apache License 2.0
