# opsail-read

`opsail-read` is the Rust library behind
[`opsail read`](https://github.com/lencx/opsail#read-html). It acquires static HTML
or delegates rendered DOM capture to `opsail-chrome`, extracts the primary
document, sanitizes the result, and returns a versioned `ReadResult` suitable
for agents and other programmatic callers.

The extraction pipeline is browser-independent. `opsail-read` owns source
validation, non-browser acquisition, extraction, sanitization, and result
provenance. Browser executable discovery, owned process lifecycle, CDP target
management, waits, and DOM capture belong to `opsail-chrome`. Callers that
already have rendered HTML should provide it directly instead of using either
browser path.

## Capabilities

- Acquire HTML from HTTP(S), regular files, caller-provided stdin bytes, or an
  already-decoded captured document.
- Connect to caller-managed Chrome through an HTTP(S) discovery endpoint or
  browser/page WebSocket, optionally navigate, wait, and capture the current DOM.
- Launch a local Chrome or Chromium process with an isolated temporary profile,
  capture one page, and clean up the owned process and profile.
- Resolve relative links and assets against a validated HTTP(S) base URL.
- Extract readable Markdown and sanitized HTML with structured metadata.
- Report source, extraction method, quality signals, and warnings through one
  stable result model.
- Enforce byte, DOM element, nesting-depth, redirect, and timeout limits.
- Reject active content, unsafe resource URLs, embedded URL credentials, and
  high-confidence full-page browser verification interstitials instead of
  publishing them as content.

## Installation

```toml
[dependencies]
opsail-read = "0.2"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## Acquire and read a URL

```rust
use opsail_read::{ReadOptions, ReadSource, read};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = ReadSource::Url("https://example.com/article".parse()?);
    let result = read(source, &ReadOptions::default()).await?;

    println!("{}", result.metadata.title);
    println!("{}", result.content);
    Ok(())
}
```

`ReadOptions` controls the base URL, request and connection timeouts, maximum
input size, `User-Agent`, and `Accept-Language` header. For direct HTTP
acquisition, leaving `user_agent` as `None` sends `opsail/<version>`; WeChat
article URLs retain their browser-compatible automatic HTTP profile with an
`opsail/<version>` product token. An explicit value always wins.

## Process caller-captured HTML

Browser hosts should capture the rendered HTML and final page URL themselves,
then provide both to `opsail-read`:

```rust
use opsail_read::{CapturedDocument, ReadOptions, ReadSource, read};

async fn process(html: String) -> Result<(), Box<dyn std::error::Error>> {
    let document = CapturedDocument::new(
        html,
        Some("https://example.com/final-article-url".parse()?),
    );
    let result = read(ReadSource::Html(document), &ReadOptions::default()).await?;
    println!("{}", result.content);
    Ok(())
}
```

`CapturedDocument` accepts an already-decoded Rust `String`. Its bytes are
treated as UTF-8; a legacy `<meta charset>` inside the document does not
reinterpret the Unicode text supplied by the caller.

For synchronous extraction with the default input-size limit, use
`extract_html(html, base_url)` instead.

## Capture through `opsail-chrome`

`ReadSource::Chrome` is the owned mode. It discovers or uses an explicitly
configured executable, starts headless Chrome with an isolated temporary
profile and a dynamically assigned loopback debugging port, captures one URL,
then stops the process and removes the profile:

```rust
use opsail_read::{ChromeSource, ReadOptions, ReadSource, read};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let chrome = ChromeSource::new("https://example.com/app".parse()?);
    let result = read(ReadSource::Chrome(chrome), &ReadOptions::default()).await?;
    println!("{}", result.content);
    Ok(())
}
```

Executable resolution supports macOS, Linux, and Windows in this order: the
`ChromeSource::executable_path` value, `OPSAIL_CHROME_PATH`, then supported
platform locations and `PATH`. Owned launch never reuses the user's Chrome
profile and does not add `--no-sandbox` automatically.

With no explicit `ReadOptions::user_agent`, owned launch derives the actual
User-Agent from the selected Chrome process and changes only its
`HeadlessChrome/<version>` product token to `Chrome/<version>`. It does not
hard-code a Chrome version. An explicit User-Agent is applied unchanged and
always takes precedence.

`ReadSource::Cdp` is the borrowed mode. The caller starts Chrome, exposes its
debugging endpoint, and owns the browser lifecycle. Opsail connects as a
short-lived client; it does not run an adapter server or background daemon.
When a navigation URL is supplied without a target ID, Opsail creates a
temporary `about:blank` target inside that browser, applies any explicit
User-Agent and language before navigation, captures the rendered DOM, and
closes only that temporary target:

```rust
use opsail_read::{CdpSource, CdpWaitUntil, ReadOptions, ReadSource, read};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut chrome = CdpSource::new("http://127.0.0.1:9222");
    chrome.url = Some("https://example.com/app".parse()?);
    chrome.wait_until = CdpWaitUntil::NetworkIdle;

    let result = read(ReadSource::Cdp(chrome), &ReadOptions::default()).await?;
    println!("{}", result.content);
    Ok(())
}
```

CDP capture first uses one `Runtime.evaluate` call to obtain HTML and the final
URL atomically. If `Runtime.evaluate` fails, it falls back to
`DOM.getOuterHTML` plus page navigation history. The current DOM does not expose
closed shadow roots, canvas pixels, or inaccessible cross-origin frame
documents.

When a browser endpoint is used without a navigation URL or `target_id`, Opsail
attaches only if exactly one eligible page target exists. Multiple pages return
an error instead of selecting an arbitrary page; callers must set `target_id`
explicitly. `direct_page` is valid only for a page-scoped WebSocket endpoint and
cannot be combined with `target_id`; the final URL of any existing page must use
HTTP(S). Captured HTML is limited by `ReadOptions::max_bytes` and an absolute 16
MiB CDP capture ceiling.

When `ReadOptions::user_agent` is `None`, borrowed CDP preserves the
caller-managed browser's User-Agent. An explicit value is applied unchanged
before navigation. Opsail deliberately does not normalize the identity of a
browser it does not own.

Both paths return the same captured-page shape to `opsail-read`, but provenance
remains explicit: owned launch produces `SourceKind::Chrome`, while borrowed
CDP produces `SourceKind::Cdp`. Cleanup of borrowed attachments and temporary
targets is guaranteed on normal completion and attempted on bounded failures;
if the operation is abruptly cancelled or the process is terminated, cleanup
is best-effort and the borrowed browser remains the caller's responsibility.

## Browser verification

Before extraction, `opsail-read` rejects high-confidence, full-page browser
verification interstitials with `ReadError::VerificationRequired`. The detector
uses structured and conjunctive evidence rather than regexes or generic page
wording:

- Cloudflare and AWS WAF use their published top-level response contracts:
  `cf-mitigated: challenge`, or the documented status plus
  `x-amzn-waf-action` combinations.
- WeChat, Cloudflare fallback pages, Google `/sorry/`, and top-level DataDome
  pages require multiple matching facts from the parsed DOM, trusted resource
  or form URLs, final-page URL constraints where applicable, and the absence of
  a substantive semantic content surface.

For Chrome/CDP sources, those DOM profiles also require a stable, visible live
marker from `opsail-chrome`'s privacy-bounded rendered observer. It measures
computed visibility, viewport intersection, paint-hit ownership, and animation-
frame stability. The observation is retained only when the root frame, loader,
and final URL remain the same. Missing, timed-out, or inconsistent evidence is
never treated as a positive. Direct HTTP and supplied HTML use conservative
static profiles because no live layout is available.

An embedded reCAPTCHA, hCaptcha, Turnstile, HUMAN/PerimeterX, or Arkose widget
is not sufficient evidence, and ordinary login pages are outside this
classification. Without an authoritative response contract or provider-owned
top-level route, rendered visibility and page-takeover evidence is required;
ambiguous static markup remains unclassified. The vendor set is conservative
rather than exhaustive. Opsail reports that verification is required; it does
not solve CAPTCHAs, complete third-party authentication, or bypass access
controls.

For Chrome sources, the response detector consumes only the optional
privacy-bounded main-document metadata exposed by `opsail-chrome`: status plus
normalized indicators derived only from `cf-mitigated` and
`x-amzn-waf-action`. Raw header values, cookies, authorization data, and
arbitrary response headers never enter this detection path. Frame, loader, and
response URL must also match the captured final main document.

## Result contract

Every successful entry point returns `ReadResult`, which contains:

- `schema_version`: version of the serialized result contract.
- `content` and `content_html`: readable Markdown and sanitized HTML.
- `metadata`: title, author, dates, canonical URL, language, and related fields.
- `source`: input kind, requested and resolved locations, charset, media type,
  and byte count.
- `extraction`: selected extraction method and duration.
- `quality`: readability and content-size signals.
- `warnings`: non-fatal conditions such as unusually short extracted content.

Serialized fields use camel case, including `schemaVersion` and `contentHtml`.
Callers should branch on structured fields rather than warning or error text.

## Trust boundary

Treat source HTML and all extracted metadata as untrusted input. `opsail-read`
sanitizes its published HTML and filters unsafe URLs, but callers remain
responsible for safely rendering Markdown, escaping terminal output, and
applying any application-specific URL or content policy.

A borrowed CDP endpoint grants control over its Chrome session and may expose
cookies or authenticated pages. Accept it only from trusted caller
configuration. Endpoint URLs and query parameters are intentionally excluded
from `ReadResult` and public acquisition errors. Owned launch uses a fresh
temporary profile and therefore does not inherit the user's authenticated
browser state.

## License

Apache-2.0
