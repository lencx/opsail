# Opsail

English | [简体中文](https://github.com/lencx/opsail/blob/main/README.zh-CN.md)

Opsail is a modular Rust CLI for small, composable actions used by software agents. Its first action, `read`, turns static HTML from an HTTP(S) URL, a local file, or standard input into readable Markdown, sanitized HTML, or versioned JSON.

Opsail extracts the HTML it receives; it does not execute JavaScript, maintain a browser session, authenticate to sites, crawl links, or interact with pages.

<table>
  <thead>
    <tr>
      <th width="180">Crate</th>
      <th width="180">Version</th>
      <th>Description</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="180"><a href="https://crates.io/crates/opsail"><code>opsail</code></a></td>
      <td width="180"><a href="https://crates.io/crates/opsail"><img src="https://img.shields.io/crates/v/opsail" alt="crates.io version"></a></td>
      <td>Agent action CLI and unified command entry point</td>
    </tr>
    <tr>
      <td width="180"><a href="https://crates.io/crates/opsail-read"><code>opsail-read</code></a></td>
      <td width="180"><a href="https://crates.io/crates/opsail-read"><img src="https://img.shields.io/crates/v/opsail-read" alt="crates.io version"></a></td>
      <td>Extracts clean Markdown, sanitized HTML, and structured JSON from static HTML</td>
    </tr>
  </tbody>
</table>

## Installation

### Prebuilt binaries

Download the archive for your platform, extract it, and place `opsail` (`opsail.exe` on Windows) somewhere on your `PATH`:

- macOS: [Apple Silicon](https://github.com/lencx/opsail/releases/latest/download/opsail-aarch64-apple-darwin.tar.gz) · [Intel](https://github.com/lencx/opsail/releases/latest/download/opsail-x86_64-apple-darwin.tar.gz)
- Linux: [x86_64](https://github.com/lencx/opsail/releases/latest/download/opsail-x86_64-unknown-linux-musl.tar.gz) · [ARM64](https://github.com/lencx/opsail/releases/latest/download/opsail-aarch64-unknown-linux-musl.tar.gz)
- Windows: [x86_64](https://github.com/lencx/opsail/releases/latest/download/opsail-x86_64-pc-windows-msvc.zip)
- [SHA-256 checksums](https://github.com/lencx/opsail/releases/latest/download/SHA256SUMS)

On macOS or Linux, set `TARGET` to the value for your platform and run:

```sh
TARGET=aarch64-apple-darwin
curl -fL "https://github.com/lencx/opsail/releases/latest/download/opsail-${TARGET}.tar.gz" -o "opsail-${TARGET}.tar.gz"
tar -xzf "opsail-${TARGET}.tar.gz"
sudo install -d /usr/local/bin
sudo install -m 755 "opsail-${TARGET}/opsail" /usr/local/bin/opsail
opsail --version
```

The example installs the Apple Silicon build. Use `x86_64-apple-darwin`, `x86_64-unknown-linux-musl`, or `aarch64-unknown-linux-musl` for the other supported platforms.

On Windows, run in PowerShell:

```powershell
$target = "x86_64-pc-windows-msvc"
$archive = "opsail-$target.zip"
$bin = Join-Path $HOME "bin"

Invoke-WebRequest -UseBasicParsing -Uri "https://github.com/lencx/opsail/releases/latest/download/$archive" -OutFile $archive
Expand-Archive $archive -DestinationPath . -Force
New-Item -ItemType Directory -Force $bin | Out-Null
Copy-Item ".\opsail-$target\opsail.exe" "$bin\opsail.exe" -Force

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (($userPath -split ";") -notcontains $bin) {
    $newPath = if ($userPath) { "$userPath;$bin" } else { $bin }
    [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
}
$env:Path = "$bin;$env:Path"
opsail --version
```

### Cargo

Opsail requires Rust 1.97 or newer when installed from crates.io:

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
