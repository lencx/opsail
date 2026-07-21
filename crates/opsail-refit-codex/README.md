# opsail-refit-codex

`opsail-refit-codex` is Opsail's target-validated Codex renderer adapter. Its first feature adds a small remaining-usage capsule to the account row at the bottom of the Codex sidebar.

The crate owns the complete adapter boundary:

- an internal, reusable refit lifecycle with idempotent enable, disable, status, rollback, cleanup, and health checks;
- macOS application identity, signature, process ancestry, loopback listener, and renderer validation;
- bounded Chrome DevTools Protocol discovery and transport;
- Codex renderer bridge methods, selectors, rate-limit normalization, partial-update merging, refresh coordination, and UI payloads;
- embedded locale JSON and theme-token-only CSS.

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

The public default is `55321`, and `--port PORT` explicitly overrides it. Discovery, preflight, and launch always use `127.0.0.1`; `localhost`, IPv6 addresses, `0.0.0.0`, and non-loopback listeners are rejected. The current implementation does not automatically choose another port if `55321` is occupied. Selecting a free port from `49152..65535` and persisting that choice is a future consideration, not a current capability.

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

`persistent` (managed) is the default. The command writes its initial JSON report, then remains in the foreground with one WebSocket per validated renderer. A dedicated async reader drains responses, control frames, and unrelated CDP events while idle work blocks on the socket. A closed socket triggers target rediscovery with bounded exponential backoff; a successful connection resets that backoff. No Opsail HTTP service, proxy, or additional listening port is created, and Rust does not duplicate the renderer's usage polling.

Disable validates and stops an active foreground Opsail manager when necessary, reconnects temporarily, and removes the current DOM, styles, listeners, observers, timers, and managed marker. It never stops ChatGPT:

```sh
opsail refit codex status
opsail refit codex disable usage
```

For a current-document injection that exits immediately, use:

```sh
opsail refit codex enable usage --once
```

`once` (ephemeral) performs the same application, process, loopback listener, renderer URL, shell, sidebar, and bridge validation. It evaluates the payload, confirms current-document health, closes the CDP WebSocket, and stores no early-script identifier. It never calls `Page.addScriptToEvaluateOnNewDocument`. Once does not survive a hard reload, renderer reconstruction, or application restart; disappearance after any of those events is the documented trade-off, not a persistent-mode failure. Repeated once installation remains renderer-idempotent.

`status` and `doctor` report `once`/`ephemeral` separately from `persistent`/`managed`. A persistent renderer whose foreground manager is gone is stale rather than healthy managed state. A once runtime that disappeared after reload is simply not installed. Disable after once performs only idempotent current-renderer cleanup and never tries to remove an identifier from the earlier CDP session.

Use the same explicit `--port PORT` on later `status`, `doctor`, and `disable` commands when the endpoint does not use `55321`.

## CDP cost and security boundaries

`--remote-debugging-port` exposes Chromium's DevTools HTTP/WebSocket entry point; it does not turn a release application into a debug build. An enabled loopback listener with no client usually has very low, but not zero, cost. Persistent mode adds one resident CDP session per validated renderer. Once removes that session cost after health confirmation, while the debug listener remains present for the lifetime of the ChatGPT process.

The renderer runtime is a separate cost boundary. Its longer-lived work is more important to constrain than an idle socket: mutation observations are filtered to relevant sidebar changes and coalesced with animation frames, geometry follows `ResizeObserver`, hidden pages pause fallback refreshes, and all listeners, observers, and timers have deterministic cleanup. Stable state performs no high-frequency polling.

The listener's security exposure matters more than its idle performance. Opsail accepts only the literal `127.0.0.1`, revalidates process and signed-application ownership around discovery, and validates the renderer URL and bridge before evaluation. Prefer an available randomized high port and pass that exact value with `--port`; never expose the endpoint on another interface. These controls bound the listener, CDP session, and renderer runtime separately rather than claiming zero overhead.

An Apple Events-based read-only probe is an unverified future consideration only. It is not implemented, enabled, or included in current capability and health decisions, and Opsail does not change application or system preferences for it.

## Usage data and refresh behavior

The renderer reads through the existing local account bridge using `account/rateLimits/read` and listens for `account/rateLimits/updated`. It does not call a model or an external account endpoint, so a refresh does not consume model tokens.

The UI derives window labels from each valid `windowDurationMins`, sorts shorter windows first, clamps finite `usedPercent` values to `0..100`, and displays rounded remaining percentages. It merges partial notifications by field presence and hides completely when no valid window exists. Reset timestamps are Unix seconds and use the system/browser locale's full date and time format independently of UI-copy fallback.

One read runs after injection. Notifications update immediately and schedule one debounced calibration read after 1.2 seconds. Focus refreshes are gated to 60 seconds, and a visible page receives a fallback refresh every 15 minutes. Requests are deduplicated and time out after 15 seconds. A failed refresh keeps the last successful snapshot with a quiet stale indicator.

## Localization and renderer behavior

All user-facing copy and duration labels are loaded from embedded JSON files in `assets/locales`. Locale selection considers `document.documentElement.lang`, then `navigator.language`, matching exact locale, language family, and finally English. English and Simplified Chinese are included and checked for matching message keys at payload construction time.

The capsule prefers a real position in the native account-row layout. A measured, fixed-position fallback is used only when a reliable row cannot be identified. Resize and relevant sidebar mutations trigger coalesced remeasurement; insufficient space hides the capsule instead of covering native controls. Details are portaled to `document.body`, positioned from measured rectangles, and clamped to both the sidebar and viewport.

Renderer colors, borders, backgrounds, text, and progress styling use application theme tokens with semantic fallbacks. The feature supports keyboard focus, localized accessible labels, tooltip association, progressbar semantics, and reduced-motion preferences.

## Library API

Embedders can construct `CodexRefit` with `CodexRefitConfig` and call `enable_usage(SessionMode, LaunchPolicy)`, `disable_usage`, `status`, or the read-only `doctor`. `LaunchPolicy::AttachOnly` is the default behavior; `LaunchPolicy::LaunchIfStopped` is the explicit launch capability. `enable_usage` returns a `CodexUsageSession`; inspect its initial report, then await `run()` to keep a persistent session managed. `run()` returns immediately for once mode. Reports include the actual port, session mode, launch policy, and whether this invocation launched ChatGPT. Diagnostics distinguish unsupported targets, target validation failures, bridge unavailability, restart requirements, port conflicts, launch failures, injection and cleanup failures, stale state, and local state errors without including account payloads or credentials.

A `stale` target with `healthy: true` means the installed runtime is structurally healthy but its local account snapshot is unavailable or stale; enable remains idempotent and does not reinject in that case. A `stale` target with `healthy: false` means renderer artifacts and the session lifecycle marker require reconciliation.

Local managed markers and advisory locks use owner-only permissions under `~/Library/Application Support/opsail/refit/codex` by default. Markers contain only bounded target identifiers, payload revisions, debugging ports, the persistent mode, non-secret installation tokens, and the owning Opsail manager PID. CDP early-script identifiers remain only in the live session that owns them and are never treated as cross-connection receipts. JavaScript, CSS, and locale JSON are embedded at compile time and are not written into a Codex configuration directory or the application bundle.
