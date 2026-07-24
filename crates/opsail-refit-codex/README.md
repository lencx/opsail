# opsail-refit-codex

`opsail-refit-codex` is Opsail's target-validated Codex renderer adapter. Its first feature adds a small remaining-usage capsule to the account row at the bottom of the Codex sidebar.

The crate owns the complete adapter boundary:

- an internal, reusable refit lifecycle with idempotent enable, disable, status, rollback, cleanup, and health checks;
- platform-specific application identity, process ownership, loopback listener, and renderer validation for macOS and Windows;
- bounded Chrome DevTools Protocol discovery and transport;
- Codex renderer bridge methods, a versioned DOM adapter, rate-limit normalization, partial-update merging, refresh coordination, and UI payloads;
- embedded locale JSON and theme-token-only CSS;
- explicit, versioned renderer JavaScript updates with fixed GitHub origin, SHA-256 validation, and atomic local activation.

The lifecycle remains an internal module while Codex is the only refit adapter. A shared crate should be extracted only after another adapter demonstrates a stable duplicated contract.

## Supported targets

| Platform | Validated application contract | Status |
| --- | --- | --- |
| macOS | `/Applications/ChatGPT.app`, bundle identifier `com.openai.codex`, signing team `2DC432GLL2`, and its signed executable/process tree | Implemented |
| Windows | The current user's Store-signed, non-development `OpenAI.Codex` package with exact PFN `OpenAI.Codex_2p2nqsd0c76g0` and AUMID `OpenAI.Codex_2p2nqsd0c76g0!App`; the executable is derived from the installed signed manifest (currently `app\ChatGPT.exe`) | Implemented for the x64 and ARM64 release targets; native compile, unit, and missing-application CI configured; installed-application end-to-end canary passed on Windows 11 ARM64; real x64 Store canary pending; no 32-bit x86/ia32 release target |
| Linux | No official application identity is defined | Unsupported |

The Windows backend and its native API boundary are implemented without depending on the reference PowerShell project. It queries the current user's packages by exact PFN and AUMID instead of matching a versioned `WindowsApps` directory, then reads the application executable from the installed signed `AppxManifest.xml`. The manifest path is accepted only when it is relative, resolves to a regular file, and remains canonically contained by the package root. Native Windows CI and npm packaging targets are configured for x64 and ARM64. A Windows 11 ARM64 canary against an installed Store application validates package activation, live listener ownership, renderer discovery, bridge injection, persistent mode, and cleanup. A real installed-application x64 canary remains pending; hosted CI covers the no-installed-package path.

Normal enable is attach-only. Explicit `--launch` may start a confirmed-stopped application once through the platform's validated launch mechanism. Opsail never quits, kills, restarts, reloads, modifies, re-signs, or writes into the application. It accepts only a debugging endpoint bound to `127.0.0.1`, validates that the listener belongs to the expected platform-validated application process, and requires an `app://` renderer with the expected application shell, sidebar, and local bridge.

## Launch and attach CLI

The supported entry point that does not require a manual application command is:

```sh
opsail refit codex enable usage --launch
opsail refit codex enable usage --launch --once
```

Interactive lifecycle commands show the current bounded milestone with a terminal spinner, including application validation, endpoint inspection, launch preflight, application startup, CDP readiness, renderer/bridge validation, injection, and health confirmation. Background startup changes the visible message only after a stage remains current for about 120ms; rapid milestones are coalesced instead of flashing completed work, without delaying the operation itself. The spinner writes only to `stderr`; structured results remain the only content on `stdout`. Redirected commands, automation without a terminal, and the background manager itself stay quiet. The foreground and background startup paths use the same structured stage model, so background startup does not fall back to an unexplained static wait.

`--launch` maps to the crate's `LaunchPolicy::LaunchIfStopped`. Enable first attempts to attach to an existing validated endpoint. If none exists, Opsail validates the platform application identity, confirms that ChatGPT is stopped and the selected port is free, and starts exactly one application instance.

On macOS, it validates the bundle identifier, signing team, code signature, and executable before spawning:

```sh
/Applications/ChatGPT.app/Contents/MacOS/ChatGPT \
  --remote-debugging-address=127.0.0.1 \
  --remote-debugging-port=55321
```

There is no shell wrapper and no `open -a` call. Standard streams and the process group are detached from the Opsail session.

On Windows, Opsail validates the current user's registered Store package, Store signature kind, non-development status, exact PFN and AUMID, signed-manifest-derived executable, and user SID. The executable is currently `app\ChatGPT.exe`, but its versioned installation path and filename are not discovered with a prefix or regular-expression scan. Opsail then passes the same two CDP arguments through the Windows application activation API. It does not execute the protected WindowsApps executable directly or invoke PowerShell. The activated PID, creation time, package identity, executable file identity, and user SID are checked again around listener discovery.

A once command or stopped persistent manager never takes ChatGPT down with it. Endpoint startup has a bounded timeout. After discovery, Opsail revalidates the platform process and listener identity, then performs renderer and bridge validation before injection.

When `--launch` actually starts ChatGPT and the injected runtime passes its health check, the renderer shows one short, localized notice confirming that Opsail mode is active. It is horizontally centered near 30% of the viewport height, above the visual midpoint without hugging the window edge. Its background and high-contrast foreground use Codex's paired activity-badge theme tokens, with the themed progress accent and foreground as semantic fallbacks. Attaching to an already-running validated endpoint, reconnecting after a renderer reload, and repeated idempotent enable do not produce that launch notice. Notice failure is diagnostic-only and never replaces or rolls back a healthy usage capsule.

An already-valid endpoint is attached without starting another process. If ChatGPT is already running without the selected CDP endpoint, enable returns `restart-required` and never quits or restarts it. A conflicting listener returns `port-unavailable`; a spawn or endpoint timeout returns `launch-failed`. `doctor`, `status`, and `disable` are always attach-only and never start the application.

The public default is `55321`, and `--port PORT` or `-p PORT` explicitly overrides it. `--launch` also has `-l`, `--once` has `-o`, and `--foreground` has `-F`. Discovery, preflight, and launch always use `127.0.0.1`; `localhost`, IPv6 addresses, `0.0.0.0`, and non-loopback listeners are rejected. The current implementation does not automatically choose another port if `55321` is occupied. Selecting a free port from `49152..65535` and persisting that choice is a future consideration, not a current capability.

On macOS, a user-managed endpoint can start the application manually with the same address and selected port before attach-only enable:

```sh
/Applications/ChatGPT.app/Contents/MacOS/ChatGPT \
  --remote-debugging-address=127.0.0.1 \
  --remote-debugging-port=55321

opsail refit codex enable usage
opsail refit codex enable usage --once
```

On Windows, do not run the executable inside WindowsApps directly. `--launch` uses the implemented AUMID package-activation route; attach-only commands may reuse an endpoint that is already running and passes the full Windows identity checks. The executable path is used for identity validation, not direct launch.

The read-only diagnostic command reports why an existing target is or is not ready, but does not launch it:

```sh
opsail refit codex doctor
```

`persistent` (managed) is the default. Opsail starts a background manager, waits for its validated health report, prints that JSON, and returns control to the terminal. The manager holds one WebSocket per validated renderer; it does not create an Opsail HTTP service, proxy, or additional listening port. A dedicated async reader drains responses, control frames, and unrelated CDP events while stable idle work blocks on the socket, and Rust does not duplicate the renderer's usage polling.

The CDP socket is also the primary application-lifetime signal. For an application started by Opsail, the background manager additionally owns a process-exit receiver backed by the child wait primitive; either the socket close or the process-exit event wakes the blocked supervisor. For an application attached after external startup, socket close triggers one process-identity check. If ChatGPT has exited, the manager removes its local target markers, releases its lock, and terminates. If ChatGPT is still running, the disconnect is treated as a renderer reload and target rediscovery uses exponential backoff from 250ms up to 30 seconds. Process checks occur only before and after those disconnected recovery waits; there is no timer or process polling while the socket is healthy. A successful renderer connection resets the backoff.

For diagnostics, keep the manager attached to the terminal explicitly:

```sh
opsail refit codex enable usage --foreground
opsail refit codex enable usage --launch --foreground
```

Foreground mode has the same managed lifecycle. macOS accepts `Ctrl+C`, `Ctrl+Z`, `SIGTERM`, and `SIGHUP` as shutdown requests; Windows currently handles `Ctrl+C`.

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

The listener's security exposure matters more than its idle performance. Opsail accepts only the literal `127.0.0.1` and revalidates process and platform application ownership around discovery. Discovery admits only a bounded, credential-free local `app:` renderer candidate on that verified endpoint; Opsail then separately probes the expected shell, sidebar, and bridge before any injection. This layered check avoids treating one private packaged URL such as `app://-/index.html` as a permanent product contract while still failing closed when the renderer identity changes. Prefer an available randomized high port and pass that exact value with `--port`; never expose the endpoint on another interface. These controls bound the listener, CDP session, and renderer runtime separately rather than claiming zero overhead.

An Apple Events-based read-only probe is an unverified future consideration only. It is not implemented, enabled, or included in current capability and health decisions, and Opsail does not change application or system preferences for it.

## Usage data and refresh behavior

The renderer reads through the existing local account bridge using `account/rateLimits/read` and listens for `account/rateLimits/updated`. Those local payloads can optionally include `rateLimitResetCredits`; the field is not guaranteed to be present. The native application owns its separate reset-credit loading path. The refit never calls that private remote endpoint, a consume method, or a model, so a refresh does not consume model tokens.

The UI derives window labels from each valid `windowDurationMins`, sorts shorter windows first, clamps finite `usedPercent` values to `0..100`, and displays rounded remaining percentages. It merges partial notifications by field presence and hides completely when no valid window exists. Each valid window reset renders a conservative remaining-time countdown plus an exact system-local timestamp in `YYYY-MM-DD HH:mm:ss` form. Available reset credits with a future finite `expiresAt` are sorted by expiration and rendered in a compact two-column table using the same exact timestamp and countdown. A localized footer states that every displayed timestamp uses local time and a 24-hour clock, avoiding duplicated AM/PM or time-zone labels on each row. Subtle row separators preserve scanning without adding boxed cells. Full localized time-zone-aware values remain available to assistive technology. Opaque identifiers, titles, redeemed or expired entries, counts, and consume actions are not retained or rendered.

One read runs after injection. Persistent early bootstrap waits for both the document root and Electron preload bridge before installing observers and issuing local reads; the window `load` transition performs an immediate retry instead of leaving a partially installed runtime. Rate-limit windows and reset credits are merged independently from the same local bridge payload, so a response or notification that contains only one field cannot erase or block the other. A structurally present response is not considered ready unless it contains at least one displayable rate-limit window. If startup returns no such window, Opsail performs one bounded calibration read after 1.2 seconds before remaining quietly hidden.

Reset-credit availability has three conservative observation states. `not-observed` means no structurally valid list has arrived and makes no claim about loading, failure, or account entitlement. `empty` means a valid list explicitly contains no currently usable future credit. `available` means at least one usable future credit is present. Only `available` renders the reset-credit section; both other states leave it hidden. Missing, `null`, and malformed fields preserve the last confirmed observation, while an explicitly empty or no-longer-usable valid list clears the rendered rows. Opsail performs no dedicated reset-credit retry or polling. Status exposes the observation state and usable count as structured fields without adding a warning or user-facing status sentence.

Notifications update both fields immediately and schedule one debounced rate-limit calibration read after 1.2 seconds. Focus refreshes are gated to 60 seconds, and a visible page receives a fallback refresh every 15 minutes. Requests are deduplicated and time out after 15 seconds. A failed rate-limit refresh keeps the last successful snapshot with a quiet stale indicator.

## Localization and renderer behavior

All user-facing copy, compact summary labels, reset wording, duration labels, and the known Codex locale registry are loaded from the embedded `assets/locales.json` bundle. The runtime reads Codex's local `config.desktop.localeOverride` value through the existing renderer `config/read` bridge and treats it as authoritative. `document.documentElement.lang` and browser languages are fallbacks only when that override is absent or invalid. The locale is read once after injection, recalibrated when the account row returns after Settings or a route transition, and checked on window focus with in-flight deduplication and a short gate. There is no configuration polling, filesystem access, remote request, or model call; only the validated locale string is retained, while the rest of the configuration response is neither stored nor logged. The bundle recognizes the 65 locales currently exposed by Codex, provides native copy for the major language families, and uses English copy with the requested locale's `Intl` date/number conventions for the remaining languages. This intentionally follows language families rather than duplicating every regional translation. Simplified and Traditional Chinese are distinct, and Chinese copy inserts typographic spacing between Han text and Latin letters or numbers.

Date wording follows the selected Codex locale while the time zone remains the system-local zone. A fallback `lang` change redraws the installed UI without reinjection when no valid configured override is present. Reset-credit countdowns conservatively floor the complete remaining hours or minutes, so they never promise more time than remains; the final partial minute displays as `0m`. Opening the details by pointer hover or keyboard focus immediately recalculates the countdown from the current local clock without reading quota data. One bounded timeout updates at the next displayed-unit boundary while the details remain installed. Hidden pages do not keep rescheduling the countdown, window focus recalibrates it, and cleanup releases the timeout.

The capsule prefers a real position in the native account-row layout. A measured, fixed-position fallback is used only when a reliable row cannot be identified. Resize and relevant account-row mutations trigger coalesced remeasurement; after initial discovery, remeasurement queries only the cached account row and falls back to a sidebar-wide discovery scan only when that cache becomes invalid. Insufficient space hides the capsule instead of covering native controls. Stable observation is limited to the account row plus direct child-list changes along its structural ancestor path. When Settings removes only that row, observation temporarily falls back to the nearest still-connected structural ancestors without observing their subtrees, so session loading and search-result churn stay outside the callback. Before initial discovery or while a route has removed the entire sidebar, observation temporarily covers the document body subtree but filters mutations to newly added or removed nodes that match or contain the sidebar shape. This catches a sidebar mounted later inside a new route wrapper without scheduling layout for unrelated page churn. A returned account row is rediscovered and immediately becomes the narrow observation scope again. Hover/focus details are portaled to `document.body`; they include used quota, one localized window-reset line, progress for each real window, and the optional reset-credit expiration list.

The details layer stays fully inside the sidebar with an approximately 16px horizontal inset whenever the sidebar is wide enough. Its horizontal range is also kept attached to the capsule: a capsule farther to the right pulls the layer toward it instead of leaving an unrelated panel at the sidebar edge. Only a sidebar narrower than the readable layer may cause right-side overflow. The final rectangle is always clamped inside the application viewport, so its left edge cannot be clipped by the window.

Renderer colors, borders, backgrounds, text, and progress styling use application theme tokens with semantic fallbacks. The feature supports keyboard focus, localized accessible labels, tooltip association, progressbar semantics, and reduced-motion preferences.

## Renderer asset updates and versioning

All Codex-native DOM selectors and geometry knowledge remain centralized in `assets/opsail-refit-codex-dom-adapter.js`. The renderer bundle has four allowlisted JavaScript files: that DOM adapter, one shared CDP control entry for probe/early bootstrap/status/cleanup, a testable usage data model, and the usage runtime. Rust assembles operation-specific payloads from those boundaries. CSS and locale JSON remain compile-time embedded. The executable also embeds the complete JavaScript bundle as the safe initial version and fallback.

Update the JavaScript bundle explicitly with:

```sh
opsail refit codex update
```

`update` is a separate execution path: it does not inspect a CDP port, discover a renderer, connect to a WebSocket, or start, stop, or restart ChatGPT. It first downloads only the version manifest from the fixed `raw.githubusercontent.com/lencx/opsail/refs/heads/main` repository path. An unchanged version returns without downloading JavaScript, and a default SHA-change rejection also stops at the manifest. Only an accepted installation downloads the four allowlisted JavaScript files concurrently from the same fixed path. It does not use the GitHub REST API. The manifest carries its schema version, renderer asset semantic version, payload API version, exact filenames, byte counts, and SHA-256 values. Payload compatibility is owned solely by `apiVersion`; it is not duplicated through a minimum CLI package version. Unknown, missing, reordered, oversized, incompatible, non-UTF-8, network-capable, or hash-mismatched content is rejected.

The default command is deliberately conservative. If any JavaScript content SHA-256 differs from the active bundle, it performs no write and asks for explicit confirmation:

```sh
opsail refit codex update --force
opsail refit codex update -f
```

When every JavaScript SHA-256 is unchanged, the normal `update` command succeeds without `--force`. This includes a manifest-only version advance; `--force` is required only to accept an actual JavaScript content change.

Force confirms the validated content change only. It cannot override the fixed GitHub origin, file allowlist, size limits, manifest/file hashes, payload compatibility, or downgrade protection. Repository control is the update trust root; SHA-256 and byte counts provide content integrity and consistency, not an independent publisher signature. Fetching the mutable `main` manifest and files can race with a repository update; those checks make that race fail safely instead of mixing versions. Retrying later obtains one coherent publication.

Validated versions are staged under Opsail's platform state directory, then activated through a small `current.json` pointer. Version directories are immutable and retained; symlinks, Windows reparse points, and non-regular files are rejected where applicable. Nothing is written to a Codex configuration directory, `ChatGPT.app`, `app.asar`, the Windows Store package directory, or any other application installation tree. `status`, lifecycle reports, and `doctor` expose the selected renderer asset version and whether it came from `embedded` or `github`. A malformed installed pointer or bundle falls back to the embedded version and produces a bounded doctor warning.

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

Embedders can construct `CodexRefit` with `CodexRefitConfig` and call `enable_usage(SessionMode, LaunchPolicy)`, `disable_usage`, `status`, the read-only `doctor`, or `update_renderer_assets(RendererAssetUpdatePolicy)`. `CodexRefitConfig::with_progress_handler` exposes the same infrequent `CodexRefitStage` milestones used by the CLI without coupling the adapter crate to terminal rendering. Handlers are synchronous and should return promptly. `RendererAssetUpdatePolicy::RequireUnchanged` is the safe update default; `Force` explicitly accepts validated JavaScript hash changes. `LaunchPolicy::AttachOnly` is the default behavior; `LaunchPolicy::LaunchIfStopped` is the explicit launch capability. `enable_usage` returns a `CodexUsageSession`; inspect its initial report, then await `run()` to keep a persistent session managed. `run()` returns immediately for once mode. Lifecycle reports include the actual port, session mode, launch policy, renderer asset version/source, and whether this invocation launched ChatGPT. Update reports include the previous and installed versions, whether content was forced, activation timing, and file count. Diagnostics distinguish unsupported targets, target validation failures, bridge unavailability, restart requirements, port conflicts, launch failures, injection, cleanup, update, stale-state, and local-state errors without including account payloads or credentials.

A `stale` target with `healthy: true` means the installed runtime is structurally healthy but its local account snapshot is unavailable or stale; enable remains idempotent and does not reinject in that case. A `stale` target with `healthy: false` means renderer artifacts and the session lifecycle marker require reconciliation.

Local managed markers, versioned renderer JavaScript, and advisory locks live under `~/Library/Application Support/opsail/refit/codex` on macOS and `%LOCALAPPDATA%\opsail\refit\codex` on Windows by default. macOS applies owner-only permission bits. Windows rejects symlinks and reparse points and replaces inherited access with a protected DACL granting full control only to the current user and SYSTEM; directory entries inherit that policy to descendants. Markers contain only bounded target identifiers, payload revisions, debugging ports, the persistent mode, non-secret installation tokens, and the owning Opsail manager PID plus its creation-time identity on Windows. CDP early-script identifiers remain only in the live session that owns them and are never treated as cross-connection receipts. CSS and locale JSON stay embedded; updated JavaScript is stored only in Opsail's state tree and never in a Codex configuration directory or the application installation tree.

## Model picker and task-local providers

`unlock-model-picker` is a separate compatibility operation from the usage
refit:

```sh
opsail refit codex unlock-model-picker \
  --launch \
  --route sf-deepseek-v3.2=opsail-gateway-model
```

It does not create model descriptors. Models still come from Codex's effective
catalog, including a configured `model_catalog_json`. The renderer patch only
makes catalog entries that Codex marked hidden eligible for display and
disables the current hidden-model gate. If a model is absent from the catalog,
this command cannot invent it.

Provider routing is explicit and task-local. For a configured model slug,
Opsail patches `thread/start`, `thread/resume`, and `thread/fork` with that
model's `modelProvider`. When an existing task changes between a native and a
routed model, the dispatcher performs the Codex-required unsubscribe/resume
transition before `turn/start`; concurrent switches are deduplicated and a
failed switch attempts to restore the last known provider. It never writes a
global `model_provider` value.

Multiple providers can be routed independently:

```sh
opsail refit codex unlock-model-picker \
  --route model-a=provider-a \
  --route model-b=provider-b \
  --default-provider openai
```

The compact single-provider form remains available:

```sh
opsail refit codex unlock-model-picker \
  --model-provider opsail-gateway-model \
  --model model-a \
  --model model-b
```

Routes may instead be stored under
`[refit.codex.model_picker.routes]` in `~/.opsail/config.toml`. Command-line
routes replace configured routes for that invocation.
`--no-provider-routing` installs only model visibility.

The native signed-in provider should remain `openai`. A third-party provider
must use its own authentication and should set `requires_openai_auth = false`;
see [`opsail-gateway-model`](../opsail-gateway-model/README.md) for the
loopback provider and credential-partition contract.

The operation validates the same signed application, numeric loopback CDP
listener, primary `app://-/index.html` renderer, and bounded route values
before injection. The model-visibility bootstrap is registered for later
documents in the current renderer target. The request-dispatcher route patch
belongs to the current renderer instance; rerun `unlock-model-picker` after a
renderer reconstruction or application restart.
