# Opsail for Node.js

The ESM-only `opsail` package is the single Node.js and command-line entry point
for Opsail. It exposes the native reader through a small asynchronous API and
installs the `opsail` command.

Install it with optional dependencies enabled so npm can select the native
binary for the current platform:

```sh
npm install opsail
```

The prebuilt release matrix targets macOS arm64/x64, Linux arm64/x64, and
Windows arm64/x64. It builds and stages `@opsail/win32-arm64` and
`@opsail/win32-x64` as optional native packages for publication; the public
package resolves the matching published target automatically. All `@opsail/*`
platform packages remain implementation details and have no separate API or
command.

```js
import { read } from "opsail";

const result = await read({
  source: {
    kind: "url",
    url: "https://example.com/article",
  },
});
```

For a `url` source, direct HTTP acquisition sends `opsail/<version>` by default;
WeChat article URLs retain Opsail's browser-compatible automatic HTTP profile.
Set `options.userAgent` to override either default explicitly.

The Node equivalent of `opsail read URL --launch` is a `chrome` source. Opsail
discovers Chrome, launches an isolated temporary profile, captures the rendered
page, and stops only the browser process it started:

```js
import { read } from "opsail";

const result = await read({
  source: {
    kind: "chrome",
    url: "https://example.com/app",
    waitUntil: "network-idle",
  },
  options: { timeoutMs: 20_000 },
});
```

Set `chromePath` on the source for an explicit Chrome or Chromium executable.
Executable resolution prefers `chromePath`, then the `OPSAIL_CHROME_PATH`
environment variable inherited by the native process, then supported system
locations and `PATH`.

Use a `cdp` source instead when Chrome is already running or its authenticated
session must be reused. This mode borrows a caller-managed endpoint: Opsail is a
short-lived CDP client and does not start or close Chrome:

```js
import { read } from "opsail";

const result = await read({
  source: {
    kind: "cdp",
    endpoint: "http://127.0.0.1:9222",
    url: "https://example.com/app",
    waitUntil: "network-idle",
  },
  options: { timeoutMs: 20_000 },
});
```

Both browser source modes accept `waitUntil` values of `none`,
`dom-content-loaded`, `load` (the default), or `network-idle`. `userAgent` and
`acceptLanguage` are applied before navigation. Without `userAgent`, an owned
`chrome` source derives the actual selected Chrome User-Agent and changes only
the `HeadlessChrome/<version>` product token to `Chrome/<version>`; a borrowed
`cdp` source preserves the caller-managed browser's User-Agent. An explicit
value is applied unchanged and always wins.

`endpoint` accepts a local Chrome debugging port as a string, an HTTP(S)
discovery URL, or a browser/page WebSocket URL. `targetId` captures an existing
page; when `url` is present without `targetId`, Opsail creates and later closes a
temporary target. Set `directPage: true` only when `endpoint` is already a
page-scoped WebSocket URL; `directPage` and `targetId` cannot be combined.
Without `url` or `targetId`, Opsail captures an existing page only when Chrome
has exactly one eligible page target. The final URL of any existing page must
use HTTP(S).
Opsail never closes borrowed Chrome or a caller-owned target. CDP endpoints can
expose authenticated browser state, so applications must treat them as
high-trust configuration and must not accept them from untrusted page content.

Opsail rejects high-confidence, full-page browser verification interstitials
as a structured native error instead of returning them as extracted content.
Cloudflare and AWS WAF are recognized from their official top-level response
contracts. WeChat, Cloudflare fallback pages, Google `/sorry/`, and top-level
DataDome pages require a strict conjunction of parsed DOM structure,
trusted resource or form URLs, final-page URL constraints where applicable,
and no substantive semantic content surface. Embedded reCAPTCHA, hCaptcha,
Turnstile, HUMAN/PerimeterX, or Arkose widgets and ordinary login pages are not
classified from widget presence alone. Chrome/CDP DOM fallbacks additionally
require live computed visibility and stability evidence bound to the same root
frame, loader, and final URL. Missing or inconsistent rendered evidence stays
unclassified. Coverage is conservative, not exhaustive; Opsail reports the gate
but does not solve or bypass it.

Applications that package the native binary themselves, including Electron
applications, can override automatic resolution with an absolute path:

```js
import { createOpsail } from "opsail";

const opsail = createOpsail({
  binaryPath: "/absolute/path/to/opsail",
});

const result = await opsail.read(
  {
    source: {
      kind: "html",
      html,
      baseUrl: "https://static.example.com/articles/",
      finalUrl: "https://example.com/article",
    },
  },
  { signal },
);
```

Resolution order is `binaryPath`, `OPSAIL_BINARY_PATH`, then the optional package
for the current `process.platform` and `process.arch`. The environment variable
also configures the package's `opsail` command.

Electron cannot execute a child process from inside an ASAR archive. Package
`node_modules/@opsail/**/bin/opsail*` as unpacked content; the resolver maps an
ASAR path to its corresponding `.asar.unpacked` path and fails clearly if that
file is absent.

The native process receives a versioned JSON request over stdin and returns a
versioned JSON response over stdout. Diagnostics stay on stderr. Abort signals,
hard timeouts, output limits, native errors, and protocol mismatches are exposed
as `OpsailError` instances. Process and protocol failures may include a bounded,
control-character-sanitized `diagnostic` string; structured native failures keep
their typed message authoritative instead of copying stderr.

`options.timeoutMs` controls native acquisition; extraction and bounded cleanup
can finish afterward. Unless
`createOpsail({ hardTimeoutMs })` explicitly sets the process deadline, the Node
wrapper uses `max(30_000, options.timeoutMs + 10_000)` milliseconds. The extra
time lets extraction finish and an owned Chrome process shut down after a native
timeout. An explicit
`hardTimeoutMs` remains authoritative, so callers that customize both deadlines
should leave enough cleanup time themselves.

The public `ReadSource` union supports `url`, `file`, `html`, `chrome`, and `cdp`
requests. Browser-independent callers should prefer `html` when they already
have both the rendered DOM and final URL; launching or attaching to Chrome again
adds no value.
Result provenance uses `source.kind = "html"` for supplied captures, `"chrome"`
for an Opsail-owned browser process, and `"cdp"` for a borrowed browser session.
For an `html` source, `baseUrl` is the resolution base for relative links in the
captured document, while `finalUrl` is the browser's final navigation URL and is
reported as `source.resolvedUrl`. When `baseUrl` is omitted, `finalUrl` also
serves as the link-resolution base.
