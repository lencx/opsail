# opsail-refit-codex

`opsail-refit-codex` is Opsail's target-validated Codex renderer adapter. Its first feature adds a small remaining-usage capsule to the account row at the bottom of the Codex sidebar.

The crate owns the complete adapter boundary:

- an internal, reusable refit lifecycle with idempotent enable, disable, status, rollback, cleanup, and health checks;
- macOS application identity, signature, process ancestry, loopback listener, and renderer validation;
- bounded Chrome DevTools Protocol discovery and transport;
- Codex renderer bridge methods, a versioned DOM adapter, rate-limit normalization, partial-update merging, refresh coordination, and UI payloads;
- embedded locale JSON and theme-token-only CSS;
- explicit, versioned renderer JavaScript updates with fixed GitHub origin, SHA-256 verification, and atomic local activation.

The lifecycle remains an internal module while Codex is the only refit adapter. A shared crate should be extracted only after another adapter demonstrates a stable duplicated contract.

## Supported target

The only currently implemented target is the signed macOS application at `/Applications/ChatGPT.app`, with bundle identifier `com.openai.codex` and signing team `2DC432GLL2`. Other platforms return an explicit `unsupported` diagnostic.

Normal enable is attach-only. Explicit `--launch` may start a confirmed-stopped application once, using Rust's process API and the validated executable directly. Opsail never quits, kills, restarts, reloads, modifies, re-signs, or writes into the application. It accepts only a debugging endpoint bound to `127.0.0.1`, validates that the listener belongs to the expected signed application process tree, and requires an `app://` renderer with the expected application shell, sidebar, and local bridge.

## Launch and attach CLI

The supported entry point that does not require a manual application command is:

```sh
opsail refit codex enable usage --launch
opsail refit codex enable usage --launch --once
```

`--launch` maps to the crate's `LaunchPolicy::LaunchIfStopped`. Enable first attempts to attach to an existing validated endpoint. If none exists, Opsail validates the application path, bundle identifier, signing team, and code signature; confirms that ChatGPT is stopped and the selected port is free; and directly spawns exactly one process with:

```sh
/Applications/ChatGPT.app/Contents/MacOS/ChatGPT \
  --remote-debugging-address=127.0.0.1 \
  --remote-debugging-port=55321
```

There is no shell wrapper and no `open -a` call. Standard streams and the process group are detached from the Opsail session, and a once command or stopped persistent manager never takes ChatGPT down with it. Endpoint startup has a bounded timeout. After discovery, Opsail revalidates that the listener descends from the process it launched, then performs renderer and bridge validation before injection.

An already-valid endpoint is attached without starting another process. If ChatGPT is already running without the selected CDP endpoint, enable returns `restart-required` and never quits or restarts it. A conflicting listener returns `port-unavailable`; a spawn or endpoint timeout returns `launch-failed`. `doctor`, `status`, and `disable` are always attach-only and never start the application.

The public default is `55321`, and `--port PORT` or `-p PORT` explicitly overrides it. `--launch` also has `-l`, `--once` has `-o`, and `--foreground` has `-F`. Discovery, preflight, and launch always use `127.0.0.1`; `localhost`, IPv6 addresses, `0.0.0.0`, and non-loopback listeners are rejected. The current implementation does not automatically choose another port if `55321` is occupied. Selecting a free port from `49152..65535` and persisting that choice is a future consideration, not a current capability.

For a user-managed endpoint, start the application manually with the same address and selected port, then use attach-only enable:

```sh
/Applications/ChatGPT.app/Contents/MacOS/ChatGPT \
  --remote-debugging-address=127.0.0.1 \
  --remote-debugging-port=55321

opsail refit codex enable usage
opsail refit codex enable usage --once
```

The read-only diagnostic command reports why an existing target is or is not ready, but does not launch it:

```sh
opsail refit codex doctor
```

`persistent` (managed) is the default. Opsail starts a detached manager, waits for its validated health report, prints that JSON, and returns control to the terminal. The manager holds one WebSocket per validated renderer; it does not create an Opsail HTTP service, proxy, or additional listening port. A dedicated async reader drains responses, control frames, and unrelated CDP events while stable idle work blocks on the socket, and Rust does not duplicate the renderer's usage polling.

The CDP socket is also the application-lifetime signal. When it closes, the manager checks whether the validated ChatGPT process still exists. If ChatGPT has exited, the manager removes its local target markers, releases its lock, and terminates. If ChatGPT is still running, the disconnect is treated as a renderer reload and target rediscovery uses bounded exponential backoff. Process checks occur only during this disconnected recovery window, at most once per second while waiting; there is no steady-state process polling. A successful renderer connection resets the backoff.

For diagnostics, keep the manager attached to the terminal explicitly:

```sh
opsail refit codex enable usage --foreground
opsail refit codex enable usage --launch --foreground
```

Foreground mode has the same managed lifecycle and accepts `Ctrl+C`, `Ctrl+Z`, `SIGTERM`, and `SIGHUP` as shutdown requests.

Disable validates and stops an active Opsail manager when necessary, reconnects temporarily, and removes the current DOM, styles, listeners, observers, timers, and managed marker. It never stops ChatGPT:

```sh
opsail refit codex status
opsail refit codex disable usage
```

For a current-document injection that exits immediately, use:

```sh
opsail refit codex enable usage --once
```

`once` (ephemeral) performs the same application, process, loopback listener, renderer URL, shell, sidebar, and bridge validation. It evaluates the payload, confirms current-document health, closes the CDP WebSocket, and stores no early-script identifier. It never calls `Page.addScriptToEvaluateOnNewDocument`. Once does not survive a hard reload, renderer reconstruction, or application restart; disappearance after any of those events is the documented trade-off, not a persistent-mode failure. Repeated once installation remains renderer-idempotent.

`status` and `doctor` report `once`/`ephemeral` separately from `persistent`/`managed`. A persistent renderer whose manager is gone is stale rather than healthy managed state. A once runtime that disappeared after reload is simply not installed. Disable after once performs only idempotent current-renderer cleanup and never tries to remove an identifier from the earlier CDP session.

Use the same explicit `--port PORT` on later `status`, `doctor`, and `disable` commands when the endpoint does not use `55321`.

## CDP cost and security boundaries

`--remote-debugging-port` exposes Chromium's DevTools HTTP/WebSocket entry point; it does not turn a release application into a debug build. An enabled loopback listener with no client usually has very low, but not zero, cost. Persistent mode adds one resident CDP session per validated renderer. Once removes that session cost after health confirmation, while the debug listener remains present for the lifetime of the ChatGPT process.

The renderer runtime is a separate cost boundary. Its longer-lived work is more important to constrain than an idle socket: mutation observations are filtered to relevant sidebar changes and coalesced with animation frames, geometry follows `ResizeObserver`, hidden pages pause fallback refreshes, and all listeners, observers, and timers have deterministic cleanup. Stable state performs no high-frequency polling.

The listener's security exposure matters more than its idle performance. Opsail accepts only the literal `127.0.0.1`, revalidates process and signed-application ownership around discovery, and validates the renderer URL and bridge before evaluation. Prefer an available randomized high port and pass that exact value with `--port`; never expose the endpoint on another interface. These controls bound the listener, CDP session, and renderer runtime separately rather than claiming zero overhead.

An Apple Events-based read-only probe is an unverified future consideration only. It is not implemented, enabled, or included in current capability and health decisions, and Opsail does not change application or system preferences for it.

## Usage data and refresh behavior

The renderer reads through the existing local account bridge using `account/rateLimits/read` and listens for `account/rateLimits/updated`. The same read response supplies optional `rateLimitResetCredits`; the refit never calls a consume method or a separate remote account endpoint. It does not call a model, so a refresh does not consume model tokens.

The UI derives window labels from each valid `windowDurationMins`, sorts shorter windows first, clamps finite `usedPercent` values to `0..100`, and displays rounded remaining percentages. It merges partial notifications by field presence and hides completely when no valid window exists. Window reset timestamps are Unix seconds and render as one compact localized date/time line, with the complete localized value available to assistive technology. Available reset credits with a future finite `expiresAt` are sorted by expiration and rendered in a compact two-column table: exact system-local time in `YYYY-MM-DD HH:mm:ss` form and a remaining-days/hours countdown. Subtle row separators preserve scanning without adding boxed cells. The full localized time-zone-aware expiration remains available to assistive technology. Opaque identifiers, titles, redeemed or expired entries, counts, and consume actions are not retained or rendered.

One read runs after injection. Notifications update immediately and schedule one debounced calibration read after 1.2 seconds. Focus refreshes are gated to 60 seconds, and a visible page receives a fallback refresh every 15 minutes. Requests are deduplicated and time out after 15 seconds. A failed refresh keeps the last successful snapshot with a quiet stale indicator.

## Localization and renderer behavior

All user-facing copy, compact summary labels, reset wording, duration labels, and the known Codex locale registry are loaded from the embedded `assets/locales.json` bundle. The Codex Language setting exposed through `document.documentElement.lang` is authoritative; browser languages are fallbacks. The bundle recognizes the 65 locales currently exposed by Codex, provides native copy for the major language families, and uses English copy with the requested locale's `Intl` date/number conventions for the remaining languages. This intentionally follows language families rather than duplicating every regional translation. Simplified and Traditional Chinese are distinct, and Chinese copy inserts typographic spacing between Han text and Latin letters or numbers.

Date wording follows the selected Codex locale while the time zone remains the system-local zone. A `lang` change redraws the installed UI without reinjection. Reset-credit countdowns conservatively floor the complete remaining hours or minutes, so they never promise more time than remains; the final partial minute displays as `0m`. Opening the details by pointer hover or keyboard focus immediately recalculates the countdown from the current local clock without reading quota data. One bounded timeout updates at the next displayed-unit boundary while the details remain installed. Hidden pages do not keep rescheduling the countdown, window focus recalibrates it, and cleanup releases the timeout.

The capsule prefers a real position in the native account-row layout. A measured, fixed-position fallback is used only when a reliable row cannot be identified. Resize and relevant sidebar mutations trigger coalesced remeasurement; insufficient space hides the capsule instead of covering native controls. Hover/focus details are portaled to `document.body`; they include used quota, one localized window-reset line, progress for each real window, and the optional reset-credit expiration list.

The details layer stays fully inside the sidebar with an approximately 16px horizontal inset whenever the sidebar is wide enough. Its horizontal range is also kept attached to the capsule: a capsule farther to the right pulls the layer toward it instead of leaving an unrelated panel at the sidebar edge. Only a sidebar narrower than the readable layer may cause right-side overflow. The final rectangle is always clamped inside the application viewport, so its left edge cannot be clipped by the window.

Renderer colors, borders, backgrounds, text, and progress styling use application theme tokens with semantic fallbacks. The feature supports keyboard focus, localized accessible labels, tooltip association, progressbar semantics, and reduced-motion preferences.

## Renderer asset updates and versioning

All Codex-native DOM selectors and geometry knowledge remain centralized in `assets/opsail-refit-codex-dom-adapter.js`. The renderer bundle has four allowlisted JavaScript files: that DOM adapter, one shared CDP control entry for probe/early bootstrap/status/cleanup, a testable usage data model, and the usage runtime. Rust assembles operation-specific payloads from those boundaries. CSS and locale JSON remain compile-time embedded. The executable also embeds the complete JavaScript bundle as the safe initial version and fallback.

Update the JavaScript bundle explicitly with:

```sh
opsail refit codex update
```

`update` is a separate execution path: it does not inspect a CDP port, discover a renderer, connect to a WebSocket, or start, stop, or restart ChatGPT. It first downloads only the version manifest from the fixed `raw.githubusercontent.com/lencx/opsail/refs/heads/main` repository path. An unchanged version returns without downloading JavaScript, and a default SHA-change rejection also stops at the manifest. Only an accepted installation downloads the four allowlisted JavaScript files concurrently from the same fixed path. It does not use the GitHub REST API. The manifest carries its schema version, renderer asset semantic version, payload API version, minimum compatible Opsail version, exact filenames, byte counts, and SHA-256 values. Unknown, missing, reordered, oversized, incompatible, non-UTF-8, network-capable, or hash-mismatched content is rejected.

The default command is deliberately conservative. If any JavaScript content SHA-256 differs from the active bundle, it performs no write and asks for explicit confirmation:

```sh
opsail refit codex update --force
opsail refit codex update -f
```

When every JavaScript SHA-256 is unchanged, the normal `update` command succeeds without `--force`. This includes a manifest-only version advance; `--force` is required only to accept an actual JavaScript content change.

Force confirms the verified content change only. It cannot override the fixed GitHub origin, file allowlist, size limits, manifest/file hashes, payload compatibility, or downgrade protection. Fetching the mutable `main` manifest and files can race with a repository update; byte counts and SHA-256 make that race fail safely instead of mixing versions. Retrying later obtains one coherent publication.

Validated versions are staged under the owner-only Opsail application-support directory, then a small `current.json` pointer is replaced atomically. Version directories are immutable and retained; symlinks and non-regular files are rejected. Nothing is written to a Codex configuration directory, ChatGPT.app, app.asar, or the application installation tree. `status`, lifecycle reports, and `doctor` expose the selected renderer asset version and whether it came from `embedded` or `github`. A malformed installed pointer or bundle falls back to the embedded version and produces a bounded doctor warning.

An installed update activates when the next `CodexRefit` session is constructed. It does not mutate a currently running persistent manager or renderer in place. Run `opsail refit codex disable usage`, update, then enable again when immediate activation is desired; the update command itself never stops anything.

A published renderer update must use an `assetVersion` higher than the previously published bundle whenever JavaScript content changes. Before the first release, development changes keep the initial `1.0.0` asset version and refresh only byte counts and SHA-256 values; the version is not incremented for each local edit. The embedded-manifest test makes stale metadata fail the build. Run the focused publication checks after changing renderer assets:

```sh
node --test packages/node/test/opsail-refit-codex-usage.test.js
cargo test --locked -p opsail-refit-codex
cargo clippy --locked -p opsail-refit-codex --all-targets -- -D warnings
```

Build the executable that carries the embedded fallback and updater with:

```sh
cargo build --locked --release -p opsail
```

The resulting CLI is `target/release/opsail`. To verify the standalone library crate package, including the manifest and every embedded fallback asset, use `cargo package --locked -p opsail-refit-codex --list` and then `cargo package --locked -p opsail-refit-codex`. The checked-in manifest must reach the fixed GitHub path before remote update clients can see that version.

## Library API

Embedders can construct `CodexRefit` with `CodexRefitConfig` and call `enable_usage(SessionMode, LaunchPolicy)`, `disable_usage`, `status`, the read-only `doctor`, or `update_renderer_assets(RendererAssetUpdatePolicy)`. `RendererAssetUpdatePolicy::RequireUnchanged` is the safe update default; `Force` explicitly accepts verified JavaScript hash changes. `LaunchPolicy::AttachOnly` is the default behavior; `LaunchPolicy::LaunchIfStopped` is the explicit launch capability. `enable_usage` returns a `CodexUsageSession`; inspect its initial report, then await `run()` to keep a persistent session managed. `run()` returns immediately for once mode. Lifecycle reports include the actual port, session mode, launch policy, renderer asset version/source, and whether this invocation launched ChatGPT. Update reports include the previous and installed versions, whether content was forced, activation timing, and file count. Diagnostics distinguish unsupported targets, target validation failures, bridge unavailability, restart requirements, port conflicts, launch failures, injection, cleanup, update, stale-state, and local-state errors without including account payloads or credentials.

A `stale` target with `healthy: true` means the installed runtime is structurally healthy but its local account snapshot is unavailable or stale; enable remains idempotent and does not reinject in that case. A `stale` target with `healthy: false` means renderer artifacts and the session lifecycle marker require reconciliation.

Local managed markers, versioned renderer JavaScript, and advisory locks use owner-only permissions under `~/Library/Application Support/opsail/refit/codex` by default. Markers contain only bounded target identifiers, payload revisions, debugging ports, the persistent mode, non-secret installation tokens, and the owning Opsail manager PID. CDP early-script identifiers remain only in the live session that owns them and are never treated as cross-connection receipts. CSS and locale JSON stay embedded; updated JavaScript is stored only in Opsail's application-support tree and never in a Codex configuration directory or the application bundle.
