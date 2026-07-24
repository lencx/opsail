# opsail-gateway-model

`opsail-gateway-model` is Opsail's bounded loopback boundary for third-party
model traffic. It keeps Codex login state, third-party credentials, provider
routing, semantic normalization, and Codex wire projection as separate
concerns.

```text
Codex task
  -> per-model provider route
  -> 127.0.0.1 Opsail gateway
  -> credential and header partition
  -> Responses-compatible upstream
  -> native Responses projector
       or JSON-SSE mapping -> OpsailEvent v1 -> Codex Responses projector
  -> Codex task stream
```

The crate accepts `POST /v1/responses`, forwards one bounded JSON request to
one configured upstream, and streams the result without buffering a complete
turn. `GET /healthz` is its only other route.

## Two response modes

Without an event mapping, the upstream must emit Responses API SSE:

- `strict` validates the stream and preserves every SSE byte exactly;
- `commentary` preserves normal Responses events but converts only
  provider-supplied reasoning summaries into complete assistant-message
  lifecycles with `phase: "commentary"`.

With an event mapping, every upstream SSE `data:` value must be a JSON object.
The configured mapper converts matching objects into ordered
`OpsailEventV1` values. A separate stateful projector then emits the exact
Responses lifecycle consumed by Codex, including generated IDs, output
indices, item completion, usage, tool arguments, and a terminal response
event.

This separation is deliberate:

- mappings describe bounded structural differences such as discriminator
  values and field locations;
- the canonical event model describes meaning;
- stateful code owns ordering, aggregation, lifecycle repair, target syntax,
  signatures, encrypted reasoning, and future non-JSON protocols.

Declarative mappings never execute scripts, templates, expressions, or
commands.

## Credential partition

Codex/ChatGPT credentials never cross the gateway boundary. One gateway
process represents exactly one upstream credential domain; it never selects
between unrelated provider credentials inside a request.

- Incoming `Authorization`, Cookie, account, organization, project,
  `x-api-key`, session, token, and arbitrary extension headers are not
  forwarded. Even incoming `Accept` and `Content-Type` values are not copied:
  Opsail generates fixed protocol values for the upstream request, preventing
  parameters on an otherwise allowed header from becoming a covert channel.
- There is no option to forward client authorization.
- A third-party bearer token may only come from a gateway-owned environment
  variable or a directly executed command-auth source. Command output is
  bounded, never interpreted by a shell, and cached only in memory. Token
  values are redacted from `Debug`, startup reports, errors, and logs.
- Top-level JSON credential channels such as `authorization`, `api_key`,
  `access_token`, `cookie`, `headers`, and `session_token` are rejected before
  contacting the upstream. Credential/header fields on a tool transport
  definition are rejected too. The outer request and control objects use a
  closed Responses field/type contract, so an unknown field cannot become a
  carrier. Prompt text, tool arguments/results, and nested function schemas
  are semantic model input, so they are not scanned or rewritten as if Opsail
  were a DLP system.
- Known provider-private Responses state is removed during a provider
  boundary crossing: `client_metadata` session/thread/trace identifiers,
  response/conversation IDs, raw or encrypted reasoning, signatures,
  encrypted compaction and agent-message state, internal chat metadata,
  prompt/cache identifiers, service tier, safety/user identifiers, and
  top-level metadata. Item IDs are removed, `store` is forced to `false`, and
  only safe reasoning summaries, messages, tool payloads, and ordinary history
  remain.
- Upstream response headers are not copied. Opsail emits a canonical content
  type and `Cache-Control: no-store`; cookies, authentication challenges,
  cache metadata, and provider-specific secret headers cannot flow back into
  Codex.
- Upstream URLs cannot contain credentials, query strings, or fragments.
  Plain HTTP is accepted only for a numeric loopback address; remote
  upstreams require HTTPS. Redirects and system proxies are disabled.

The signed-in native provider remains Codex's own `openai` provider and never
enters this gateway. Only models explicitly routed to a third-party provider
use the gateway.

## Configuration

Create the private Opsail config once:

```sh
opsail config init
```

An example `~/.opsail/config.toml`:

```toml
version = 1

[refit.codex]
debug_port = 55321

[refit.codex.model_picker]
default_provider = "openai"

[refit.codex.model_picker.routes]
"sf-deepseek-v3.2" = "opsail-gateway-model"

[gateway.model]
listen = "127.0.0.1:55322"
upstream = "http://127.0.0.1:8317/v1"
reasoning_display = "commentary"
request_timeout_seconds = 600
stream_idle_timeout_seconds = 120
max_request_bytes = 33554432
max_concurrent_requests = 8
# CLIProxyAPI/cc-switch accept prompt_cache_key. Keep "strip" for unknown
# Responses-compatible upstreams.
prompt_cache_routing = "provider-scoped"
event_mapping_file = "mappings/provider-events.toml"

[gateway.model.upstream_auth]
source = "codex-provider-command"
provider = "cliproxy"
```

Relative `event_mapping_file` paths are resolved from the directory containing
`config.toml`. An inline `[gateway.model.event_mapping]` profile is also
supported, but it is mutually exclusive with `event_mapping_file`. Every
setting has a corresponding `gateway model serve` option; command-line values
take precedence.

Register only the loopback gateway as the third-party provider in Codex:

```toml
[model_providers.opsail-gateway-model]
name = "Opsail Model Gateway"
base_url = "http://127.0.0.1:55322/v1"
wire_api = "responses"
requires_openai_auth = false
supports_websockets = false
```

Do not set a global `model_provider = "opsail-gateway-model"` and do not set
`requires_openai_auth = true`. The first would replace the signed-in default;
the second would ask Codex to attach an unrelated login credential. Keep
`model_catalog_json` as Codex's model catalog source and let Opsail route only
the configured model slugs.

If CLIProxyAPI or cc-switch is the loopback upstream and accepts requests
without a gateway credential, omit `[gateway.model.upstream_auth]`. When a
Codex provider already defines safe command-auth for that exact upstream,
Opsail can import only that command-auth block:

```toml
[gateway.model.upstream_auth]
source = "codex-provider-command"
provider = "cliproxy"
```

The imported provider must use `requires_openai_auth = false`, the Responses
wire API, and a `base_url` exactly matching `gateway.model.upstream`. This
prevents a ChatGPT/OpenAI credential or a token for a different endpoint from
crossing the boundary.

For a fixed raw provider token, name an environment variable without putting
the value in TOML:

```sh
export THIRD_PARTY_API_KEY='provider-owned-token'
opsail gateway model serve --upstream-bearer-env THIRD_PARTY_API_KEY
```

The equivalent persistent configuration is:

```toml
[gateway.model.upstream_auth]
source = "environment"
name = "THIRD_PARTY_API_KEY"
```

For rotating credentials, configure a direct executable. The program writes
one bearer token to stdout:

```toml
[gateway.model.upstream_auth]
source = "command"
command = "/absolute/path/to/provider-token"
args = ["account-a"]
timeout_ms = 5000
refresh_interval_ms = 300000
```

Run a separate gateway listener and config file for each independent provider
credential domain. For example, use
`opsail --config ~/.opsail/providers/vendor-a.toml gateway model serve`; do not
put multiple vendors' secrets into one environment variable or ask an
upstream proxy to infer a provider from a client credential.

## Token and prompt caching

Token caching and model prompt caching are separate layers:

- command-auth tokens are held only in process memory and reused for the
  configured `refresh_interval_ms` (five minutes by default, matching Codex's
  external bearer-command contract);
- `refresh_interval_ms = 0` disables proactive refresh and keeps the token
  until the upstream returns `401`;
- one async mutex provides singleflight on cache misses and refreshes;
- failed command refreshes enter a short in-memory backoff instead of executing
  again for every queued request;
- after `401`, Opsail compares the failed token with the current cached token,
  so concurrent requests reuse a refresh that already completed;
- the failed request is retried at most once, with the same normalized body.

Changing a bearer token does not itself change the request body or its stable
prompt prefix, so it does not intentionally invalidate an upstream prompt/KV
cache. A well-behaved provider partitions cache data by provider account or
tenant rather than by the literal access-token bytes. If a token command mints
a different still-valid identity on every execution and its upstream keys
cache entries by raw token, use `refresh_interval_ms = 0` or fix the command to
reuse tokens until expiry; Opsail cannot guarantee an opaque third-party cache
implementation.

Opsail never forwards Codex's original `prompt_cache_key`, because it may
identify an official account or session. The default
`prompt_cache_routing = "strip"` also avoids HTTP 400 responses from incomplete
Responses implementations. For CLIProxyAPI, cc-switch, or another upstream
that explicitly supports this field, select:

```toml
[gateway.model]
prompt_cache_routing = "provider-scoped"
```

Opsail then replaces a valid incoming key with a deterministic SHA-256 key
scoped by the gateway listener and upstream URL. The value stays stable across
token refreshes and gateway restarts with the same configuration, while the
original Codex key never crosses the boundary. Missing or invalid keys remain
absent; request IDs and `previous_response_id` are never promoted into cache
keys.

The token command remains responsible for its provider's OAuth lifecycle. It
should cache an access token until its real expiry, serialize refreshes across
processes if refresh tokens are one-time-use, and persist only the minimum
refresh material with private file permissions. Opsail intentionally does not
persist access tokens.

Then install model visibility and task-local routing into a validated Codex
renderer:

```sh
opsail refit codex unlock-model-picker \
  --launch \
  --route sf-deepseek-v3.2=opsail-gateway-model
```

## Event mapping schema v1

Validate a mapping without starting the gateway:

```sh
opsail gateway model validate-mapping \
  crates/opsail-gateway-model/examples/model-event-mapping.toml
```

The checked-in
[`model-event-mapping.toml`](examples/model-event-mapping.toml)
shows run, reasoning summary, assistant text, tool-call arguments, usage,
completion, and failure events.

A profile contains:

- an input view: decoded `json-data` (default) or an `sse-envelope` containing
  both `{ "event": <SSE event name>, "data": <decoded JSON> }`;
- one RFC 6901 JSON Pointer selecting the discriminator;
- at most 128 exact-match rules;
- at most 16 exact JSON-Pointer conditions per rule;
- typed canonical event names and typed fields;
- for each field, exactly one JSON Pointer or bounded literal.

Pointers are limited to 512 bytes and 32 segments. Identifiers are limited to
512 bytes, mapped text to 8 MiB per SSE frame, and aggregated projected output
to 32 MiB. Unknown events are ignored. After a rule matches, missing,
mistyped, inconsistent, out-of-order, oversized, or unterminated data fails
closed as a Responses-shaped error event.

Schema v1 covers:

- run start, completion, and failure;
- reasoning-summary delta and completion;
- assistant text delta with `commentary` or `final_answer` phase;
- function-call start, argument delta, and completion;
- input, output, and total token usage.

## Current boundary

The request side remains the Responses API in this version. The declarative
mapper handles structurally different JSON-SSE responses; it is not a general
Chat Completions or Anthropic request translator. CLIProxyAPI and cc-switch
can be used as the upstream conversion layer today.

Future stateful adapters should emit the same `OpsailEventV1` stream and reuse
the same Codex projector. Protocols that require request rewriting, tool-call
assembly across heterogeneous frames, provider signatures, encrypted
reasoning continuity, or non-SSE transport belong in compiled adapters rather
than a more powerful configuration language.

Opaque reasoning/signature continuity is intentionally fail-closed in schema
v1: unowned state is stripped even if that means a provider must reason again.
A future state vault may replay an opaque value only after Opsail can prove it
was issued by the same configured provider domain. Recognizing a third-party
prefix or forwarding every `encrypted_content` value is not an ownership
proof.

## Runtime bounds

The gateway binds only a numeric loopback address, denies redirects and system
proxy use, limits request bytes and concurrent response streams, applies
separate response-header and stream-idle timeouts, and cancels upstream work
when the downstream body is dropped. It does not persist prompts, responses,
canonical events, credentials, or transcripts.
