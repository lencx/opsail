<p align="center">
  <img src="https://raw.githubusercontent.com/lencx/opsail/main/assets/opsail-logo.png" alt="Opsail logo" width="160">
</p>

<h1 align="center">Opsail</h1>

<p align="center"><strong>Native tools that agents can rely on.</strong></p>

<p align="center">
  English | <a href="https://github.com/lencx/opsail/blob/main/README.zh-CN.md">简体中文</a>
</p>

<a href="https://www.buymeacoffee.com/lencx" target="_blank"><img height="40" width="145" src="https://cdn.buymeacoffee.com/buttons/v2/default-blue.png" alt="Buy Me A Coffee"></a>

Opsail is a modular native toolkit that gives software agents small, composable, and reliable capabilities through one command-line entry point. Its Rust crates keep acquisition, browser control, content extraction, and application-specific refits behind explicit boundaries, while the Node.js package makes the same native runtime easy to embed.

## Core characteristics

- **Native and predictable.** Long-running work, process ownership, transport, validation, and cleanup are implemented in Rust rather than shell scripts or proxy services.
- **Small composable capabilities.** Each package owns one clear boundary and can be used independently or through the `opsail` CLI.
- **Agent-ready contracts.** Commands expose stable output, structured diagnostics, bounded resource use, and quiet failure modes suitable for automation.
- **Explicit trust boundaries.** Borrowed browsers, owned processes, remote content, and application refits are validated according to their actual ownership and security model.
- **Reversible by design.** Refit features are target-validated, idempotent, and removable without modifying the target application bundle.

## Core capabilities

### Read HTML

`opsail read` turns static HTML or a browser-rendered DOM into readable Markdown, sanitized HTML, or versioned JSON. It accepts URLs, files, stdin, an Opsail-owned isolated Chrome process, or an explicitly borrowed CDP endpoint.

```sh
opsail read https://example.com/article
opsail read https://example.com/app --launch
```

See [`opsail-read`](https://github.com/lencx/opsail/blob/main/crates/opsail-read/README.md) for acquisition, extraction, result contracts, and Rust APIs. See [`opsail-chrome`](https://github.com/lencx/opsail/blob/main/crates/opsail-chrome/README.md) for Chrome discovery, owned launch, borrowed CDP, navigation, and rendered DOM capture.

### Refit Codex

`opsail refit codex` provides a reversible, target-validated Codex adapter. Its usage feature adds localized remaining-usage information to the Codex sidebar using the renderer's existing local bridge, without model calls or changes to the application bundle. Its model-picker compatibility exposes entries from Codex's effective model catalog and can route selected models to task-local providers without replacing global signed-in authentication.

![refit-codex](https://raw.githubusercontent.com/lencx/opsail/main/assets/refit-codex.png)

The refit target is implemented for the signed macOS application and the current-user Microsoft Store application on Windows; Linux is not supported. Windows release targets are x64 and ARM64; no 32-bit x86/ia32 artifact is provided. Opsail resolves the exact package family and AUMID, derives the application executable from the installed signed manifest (currently `app\ChatGPT.exe`), and protects its Local AppData state with an explicit current-user-and-SYSTEM DACL. Native CI and npm packaging targets are configured for both Windows architectures. A Windows 11 ARM64 canary against the installed Store application validates package activation, listener ownership, renderer discovery, bridge injection, persistence, and cleanup; a real installed-application x64 canary remains pending, while hosted CI covers the no-installed-package path.

```sh
opsail refit codex enable usage --launch
```

Persistent mode starts a validated background manager and returns after its health report; `--once` remains ephemeral and `--foreground` is available for diagnostics.

Interactive waits show their current validated lifecycle stage on `stderr`, while the final machine-readable JSON remains isolated on `stdout`.

See [`opsail-refit-codex`](https://github.com/lencx/opsail/blob/main/crates/opsail-refit-codex/README.md) for supported targets, attach and launch modes, lifecycle semantics, renderer updates, localization, security checks, and library APIs.

### Gateway model and configuration

`opsail gateway model` is a loopback-only boundary for explicitly routed third-party models. It preserves native Responses streams or maps bounded JSON-SSE shapes through `OpsailEvent v1` into Codex Responses events.

```sh
opsail config init
opsail gateway model serve
```

Settings live in a private `~/.opsail/config.toml`, with CLI overrides. Client ChatGPT credentials cannot be forwarded; each gateway instance owns one third-party credential domain. Provider tokens can come from a named environment variable or a bounded Rust-managed command-auth cache. See [`opsail-gateway-model`](https://github.com/lencx/opsail/blob/main/crates/opsail-gateway-model/README.md) for routing, schemas, security boundaries, and protocol scope.

## Packages

| Package | Responsibility | Documentation |
| --- | --- | --- |
| [`opsail`](https://crates.io/crates/opsail) | Native CLI and unified command entry point | Run `opsail --help` |
| [`opsail-read`](https://crates.io/crates/opsail-read) | Content acquisition, extraction, sanitization, and result contracts | [README](https://github.com/lencx/opsail/blob/main/crates/opsail-read/README.md) |
| [`opsail-chrome`](https://crates.io/crates/opsail-chrome) | Cross-platform Chrome lifecycle, CDP transport, and rendered capture | [README](https://github.com/lencx/opsail/blob/main/crates/opsail-chrome/README.md) |
| [`opsail-refit-codex`](https://crates.io/crates/opsail-refit-codex) | Validated Codex refit lifecycle, usage semantics, localization, and UI payload | [README](https://github.com/lencx/opsail/blob/main/crates/opsail-refit-codex/README.md) |
| [`opsail-gateway-model`](https://crates.io/crates/opsail-gateway-model) | Loopback model routing, canonical event normalization, and Codex Responses projection | [README](https://github.com/lencx/opsail/blob/main/crates/opsail-gateway-model/README.md) |
| [`opsail`](https://www.npmjs.com/package/opsail) for Node.js | ESM API and native binary distribution | [README](https://github.com/lencx/opsail/blob/main/packages/node/README.md) |

## Install

Install the CLI from crates.io:

```sh
cargo install opsail
```

Install the Node.js API and CLI from npm:

```sh
npm install opsail
```

Prebuilt native binaries are available from [GitHub Releases](https://github.com/lencx/opsail/releases/latest). Agent hosts can use the reviewed [`bootstrap-opsail` Skill](https://github.com/lencx/opsail/blob/main/skills/bootstrap-opsail/SKILL.md) to reconcile the CLI and runtime Skill with explicit approval.

## Project documentation

- [Content extraction and result model](https://github.com/lencx/opsail/blob/main/crates/opsail-read/README.md)
- [Chrome and CDP integration](https://github.com/lencx/opsail/blob/main/crates/opsail-chrome/README.md)
- [Codex refit lifecycle and model picker](https://github.com/lencx/opsail/blob/main/crates/opsail-refit-codex/README.md)
- [Model gateway and canonical event mapping](https://github.com/lencx/opsail/blob/main/crates/opsail-gateway-model/README.md)
- [Node.js API and packaging](https://github.com/lencx/opsail/blob/main/packages/node/README.md)
- [Development and contribution guide](https://github.com/lencx/opsail/blob/main/CONTRIBUTING.md)

## License

Apache License 2.0
