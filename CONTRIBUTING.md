# Contributing to Opsail

English | [简体中文](CONTRIBUTING.zh-CN.md)

Thank you for helping improve Opsail. Keep changes focused on one observable behavior or one cohesive action module.

## Prerequisites

- Rust 1.97 or newer.
- The committed `Cargo.lock`; development and CI commands should use locked dependencies.

## Workspace boundaries

```text
crates/opsail       CLI parsing, output routing, diagnostics, and exit behavior
crates/opsail-read  HTML acquisition, extraction, sanitization, and result schema
```

The `opsail` package is a thin process adapter. Extraction heuristics, networking, sanitization, and result models belong in `opsail-read`. A future action should become a sibling `opsail-<action>` crate once it has a cohesive typed API and independent tests. Do not introduce a plugin ABI or shared framework before implemented modules demonstrate that need.

## Library entry points

`opsail-read` exposes:

- `read(Input, &ReadOptions)` for asynchronous URL, file, or stdin acquisition.
- `extract_html(html, base_url)` for synchronous in-memory extraction.

Both return the versioned `ReadResult` model used by CLI JSON output.

## Development workflow

Run the complete verification sequence before submitting a change:

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo build --release --workspace --locked
```

## Change expectations

- Add or update the narrowest test that proves a behavior change.
- Keep extraction fixtures self-contained. Review full Markdown golden changes manually; tests must never update them automatically.
- Keep default tests offline. HTTP behavior belongs in local mock-server tests.
- Preserve stdout as the data channel and stderr as the diagnostics channel.
- Keep JSON schema evolution additive unless `schemaVersion` changes.
- Treat acquired HTML, metadata, links, and extracted text as untrusted input.
- Document unsupported behavior and new trust boundaries.
