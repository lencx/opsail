# Opsail

English | [简体中文](https://github.com/lencx/opsail/blob/main/README.zh-CN.md)

Opsail is a modular Rust CLI for small, composable actions used by software agents. Its first action, `read`, turns static HTML from an HTTP(S) URL, a local file, or standard input into readable Markdown, sanitized HTML, or versioned JSON.

Opsail extracts the HTML it receives; it does not execute JavaScript, maintain a browser session, authenticate to sites, crawl links, or interact with pages.

| Crate | Version | Description |
| --- | --- | --- |
| [`opsail`](https://crates.io/crates/opsail) | [![crates.io version](https://img.shields.io/crates/v/opsail)](https://crates.io/crates/opsail) | Agent action CLI and unified command entry point |
| [`opsail-read`](https://crates.io/crates/opsail-read) | [![crates.io version](https://img.shields.io/crates/v/opsail-read)](https://crates.io/crates/opsail-read) | Extracts clean Markdown, sanitized HTML, and structured JSON from static HTML |

## Installation

Opsail requires Rust 1.97 or newer. Install the latest release from crates.io:

```sh
cargo install opsail
```

Verify the installation:

```sh
opsail --version
```

## Read HTML

Markdown is the default output:

```sh
opsail read https://example.com/article
opsail read ./article.html
opsail read - < article.html
```

Choose another representation, resolve relative links for non-URL input, project one field, or write the result to a file:

```sh
opsail read ./article.html --format html --output cleaned.html
opsail read - --base-url https://example.com/articles/ < article.html
opsail read ./article.html --format json
opsail read ./article.html --property title
```

`extract` is a visible alias for `read`. Run `opsail read --help` for request headers, timeout, byte-limit, and output options.

### Output contract

Data is written to stdout, or to `--output PATH`. Diagnostics and extraction warnings are written to stderr, so stdout remains safe to pipe. Every successful representation ends with a newline. A downstream closed pipe is treated as a successful termination.

| Exit code | Meaning |
| --- | --- |
| `0` | Successful command, help, or version output |
| `1` | Acquisition, extraction, serialization, or write failure |
| `2` | Invalid command-line usage |

`--format json` emits schema version `1` with these top-level fields:

```text
schemaVersion
content
contentHtml
metadata
source
extraction
quality
warnings
```

`content` is Markdown and `contentHtml` is sanitized HTML. Metadata includes the title and, when available, author, description, site, publication timestamps, image, favicon, language, direction, canonical URL, and domain. Source, extraction, and quality objects record provenance and useful confidence signals.

`--property` accepts:

```text
content, markdown, contentHtml, html, title, author, description, site,
published, modified, image, favicon, language, direction, url, canonicalUrl, domain,
wordCount, quality, source, extraction
```

With `--format json`, a projected property is valid JSON. With Markdown or HTML format, scalar properties are plain text and structured properties are pretty-printed JSON.

### Defaults and limits

- Maximum input: 5 MiB; override with a positive `--max-bytes` value.
- Maximum parsed DOM: 50,000 elements and 256 nesting levels.
- HTTP(S) timeouts: 5 seconds to connect and 15 seconds overall; `--timeout` overrides the overall timeout.
- Redirect limit: 10.
- URL input and `--base-url` must use HTTP(S) and cannot contain embedded username/password credentials.
- Character decoding considers a BOM, HTTP charset, HTML metadata, UTF-8 validity, then a Windows-1252 fallback.
- A fetched body must look like HTML. If a media type is declared, it must be HTML or a tolerated generic text/binary type.
- File input must be a regular file. Its links remain relative unless `--base-url` is supplied. URL input resolves links against the final response URL.

The byte and DOM limits bound common resource-exhaustion paths; they are not a security sandbox. URL fetching can reach destinations allowed by the host network and honors the system proxy. Treat extracted text and links as untrusted, and enforce network, filesystem, and downstream execution policy in the embedding agent.

## Contributing

Development setup, module boundaries, testing rules, and verification commands are documented in [CONTRIBUTING.md](https://github.com/lencx/opsail/blob/main/CONTRIBUTING.md).

## License

Apache License 2.0
