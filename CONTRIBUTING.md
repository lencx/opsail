# Contributing to Opsail

English | [简体中文](CONTRIBUTING.zh-CN.md)

Thank you for helping improve Opsail. Keep changes focused on one observable behavior or one cohesive action module.

## Prerequisites

- Rust 1.97 or newer.
- The committed `Cargo.lock`; development and CI commands should use locked dependencies.

## Workspace boundaries

```text
crates/opsail          Native CLI parsing, protocol routing, diagnostics, and exit behavior
crates/opsail-chrome   Chrome executable discovery, owned lifecycle, CDP, and DOM capture
crates/opsail-read     Source orchestration, HTML acquisition, extraction, sanitization, and result schema
crates/opsail-refit-codex Codex refit lifecycle, target safety, and renderer integration
packages/node          Public `opsail` npm facade and native binary resolution
skills/bootstrap-opsail Transient agent-facing installation control plane
skills/opsail          Unified Agent Skill for runtime Opsail capabilities
```

The native `opsail` crate owns the unified command entry point, while the public `opsail` npm package is a thin process adapter. Generated `@opsail/<platform>-<arch>` packages are implementation-only binary carriers, not additional APIs. `opsail-chrome` owns all Chrome-specific mechanics: cross-platform executable discovery, isolated process launch and cleanup, borrowed CDP connections, target lifecycle, navigation waits, and rendered DOM capture. It does not extract or sanitize content. `opsail-read` selects and validates sources, acquires non-browser HTML, delegates browser capture to `opsail-chrome`, and owns extraction, sanitization, and `ReadResult`. `opsail-refit-codex` owns the Codex-specific application identity, process and loopback CDP validation, renderer bridge, selectors, quota semantics, localization assets, and UI payload. Its refit lifecycle stays internal until a second adapter demonstrates a stable shared contract. A future action should become a sibling `opsail-<action>` crate once it has a cohesive typed API and independent tests, then be exposed through the existing CLI, npm facade, and unified runtime skill. Do not introduce a plugin ABI or shared framework before implemented modules demonstrate that need.

## Library entry points

`opsail-chrome` exposes two ownership-specific entry points:

- `capture_chrome(&ChromeSource, &CaptureOptions)` discovers or uses a configured executable, launches an isolated temporary profile, captures one page, and stops the owned browser.
- `capture_cdp(&CdpSource, &CaptureOptions)` borrows a caller-managed endpoint and never owns that browser or its existing targets.

Executable resolution must remain explicit path, then `OPSAIL_CHROME_PATH`, then platform candidates and `PATH`. Owned launch supports macOS, Linux, and Windows, uses a dynamically assigned loopback debugging port, never reuses a user profile, and must not silently add `--no-sandbox`.

Borrowed CDP cleanup must close only Opsail-created targets. Detach and target cleanup are expected on normal completion, but remain best-effort when a capture future is abruptly cancelled or the process is terminated; the caller always retains ownership of that browser.

`opsail-read` exposes:

- `read(ReadSource, &ReadOptions)` for asynchronous URL, file, stdin, captured HTML, borrowed CDP, or owned Chrome acquisition.
- `extract_html(html, base_url)` for synchronous in-memory extraction.

Both `opsail-read` entry points return the versioned `ReadResult` model used by CLI JSON output. Browser captures retain distinct provenance: `SourceKind::Chrome` for owned launch and `SourceKind::Cdp` for a borrowed endpoint.

`opsail-refit-codex` exposes `CodexRefit`, configured through `CodexRefitConfig`, with asynchronous `enable_usage`, `disable_usage`, `status`, and read-only `doctor` operations. The adapter currently supports only the validated macOS application at `/Applications/ChatGPT.app`. Enable is attach-only unless its typed launch policy is explicitly `LaunchIfStopped`; that policy may spawn the validated executable once but must never quit, kill, restart, reload, modify, or re-sign the application. `doctor`, `status`, and `disable` never launch. Connections must use only `127.0.0.1` and fail closed unless the application signature, process ancestry, renderer URL and shell, sidebar, and expected local bridge all validate. Codex protocol names, selectors, quota semantics, localization JSON, and UI copy belong in this crate, not in a shared module.

## Development workflow

Run the complete verification sequence before submitting a change:

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo build --release --workspace --locked
npm test --prefix packages/node
npm run pack:check --prefix packages/node
```

## Change expectations

- Add or update the narrowest test that proves a behavior change.
- Keep extraction fixtures self-contained. Review full Markdown golden changes manually; tests must never update them automatically.
- Keep default tests offline. HTTP behavior belongs in local mock-server tests.
- Keep an explicit version in each Rust crate. Crates version independently; bump only a crate whose release contract changes, then update dependent version requirements deliberately.
- Preserve stdout as the data channel and stderr as the diagnostics channel.
- Keep JSON schema evolution additive unless `schemaVersion` changes.
- Treat acquired HTML, metadata, links, and extracted text as untrusted input.
- Document unsupported behavior and new trust boundaries.
- Version the transient `bootstrap-opsail` procedure independently of the CLI and npm package; bump its `metadata.version` when bootstrap behavior changes.
- Update the pinned Opsail version in `skills/opsail/SKILL.md` (`compatibility` and `metadata`) on `main` before tagging a CLI release; `bootstrap-opsail` installs the CLI from the latest release but the runtime Skill from `main`.
- Treat `metadata.openclaw` and `metadata.hermes` as intentional host extensions. Strict Agent Skills metadata portability requires generated host projections; do not stringify or remove these objects without replacing their gating, installer, and discovery behavior.
