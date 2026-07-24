//! CDP-based Codex Desktop model-picker compatibility.
//!
//! The picker and provider routing are separate transitions:
//! - the embedded renderer script makes catalog models visible;
//! - an optional, fail-closed dispatcher patch selects a configured provider
//!   for matching task starts and resumes without changing global Codex config.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};

use crate::cdp::{CdpSession, RendererTarget, discover_targets};
use crate::error::{CodexRefitError, CodexRefitErrorCode};
use crate::platform::{self, LaunchedProcessIdentity, RuntimeIdentity, ValidatedAppIdentity};
use crate::run_blocking;

const MODEL_PICKER_SCRIPT: &str = include_str!("../assets/opsail-refit-codex-model-picker.js");
const CDP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const LAUNCH_WAIT_TIMEOUT: Duration = Duration::from_secs(45);
const LAUNCH_POLL_INTERVAL: Duration = Duration::from_millis(500);
const FALLBACK_PORTS: &[u16] = &[9222, 9223, 9229, 9230, 9231];
const PROVIDER_ROUTE_KEY: &str = "__opsailModelProviderRouteV3";
const PROVIDER_ROUTE_OBJECT_GROUP: &str = "opsail-model-provider-route";
const MAIN_RENDERER_URL: &str = "app://-/index.html";
const MAX_PROVIDER_ROUTES: usize = 128;
const MAX_ROUTE_VALUE_BYTES: usize = 512;
const MAX_REMOTE_OBJECT_ID_BYTES: usize = 1024;
const MIN_DEBUG_PORT: u16 = 1024;

/// Per-model provider selection applied only to Codex task starts and resumes.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderRouting {
    pub routes: BTreeMap<String, String>,
    pub default_provider: String,
}

impl ModelProviderRouting {
    pub fn new(
        routes: BTreeMap<String, String>,
        default_provider: impl Into<String>,
    ) -> Result<Self, CodexRefitError> {
        let routing = Self {
            routes,
            default_provider: default_provider.into(),
        };
        routing.validate()?;
        Ok(routing)
    }

    fn validate(&self) -> Result<(), CodexRefitError> {
        if self.routes.len() > MAX_PROVIDER_ROUTES {
            return Err(injection_error(format!(
                "at most {MAX_PROVIDER_ROUTES} model provider routes may be injected"
            )));
        }
        validate_route_value("default provider", &self.default_provider)?;
        for (model, provider) in &self.routes {
            validate_route_value("model", model)?;
            validate_route_value("provider", provider)?;
        }
        Ok(())
    }
}

/// Result of a model-picker compatibility operation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPickerUnlockResult {
    pub attempted_ports: Vec<u16>,
    pub debug_port: Option<u16>,
    pub target_id: Option<String>,
    pub target_title: Option<String>,
    pub target_url: Option<String>,
    pub injected: bool,
    pub provider_routing_injected: bool,
    pub provider_routing: ModelProviderRouting,
    pub launched: bool,
    pub message: String,
}

/// Unlock the model picker and optionally install per-model provider routing.
pub async fn unlock_model_picker(
    port: u16,
    launch_if_stopped: bool,
    provider_routing: ModelProviderRouting,
) -> Result<ModelPickerUnlockResult, CodexRefitError> {
    provider_routing.validate()?;
    if port < MIN_DEBUG_PORT {
        return Err(CodexRefitError::new(
            CodexRefitErrorCode::TargetValidationFailed,
            format!("debugging port must be between {MIN_DEBUG_PORT} and 65535"),
        ));
    }

    if !platform::is_supported() {
        return Err(CodexRefitError::new(
            CodexRefitErrorCode::Unsupported,
            "Codex model-picker compatibility is supported only on macOS and Windows",
        ));
    }

    let app = run_blocking(platform::validate_app).await?;
    if let Some(result) = try_unlock_on_ports(port, &provider_routing, &app, None).await? {
        return Ok(result);
    }
    if !launch_if_stopped {
        return Ok(no_renderer_found(port, provider_routing));
    }
    if run_blocking(move || platform::debug_listener_present(port)).await? {
        return Err(CodexRefitError::new(
            CodexRefitErrorCode::SessionUnavailable,
            "a debug listener exists but no injectable Codex renderer was found",
        ));
    }
    let running_app = app.clone();
    if run_blocking(move || platform::app_is_running(&running_app)).await? {
        return Err(CodexRefitError::new(
            CodexRefitErrorCode::RestartRequired,
            "Codex is already running without CDP. Quit Codex first, then retry with --launch.",
        ));
    }

    let launch_app = app.clone();
    let launched = run_blocking(move || platform::launch_app(port, &launch_app)).await?;
    let launched_identity = launched.identity();
    let mut exit = launched.exit_receiver();
    let deadline = tokio::time::Instant::now() + LAUNCH_WAIT_TIMEOUT;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                return Err(CodexRefitError::new(
                    CodexRefitErrorCode::TargetNotFound,
                    "Codex was launched but the renderer did not appear within the timeout",
                ));
            }
            changed = exit.changed() => {
                if changed.is_err() || *exit.borrow() {
                    return Err(CodexRefitError::new(
                        CodexRefitErrorCode::TargetNotFound,
                        "Codex exited before its renderer became available",
                    ));
                }
            }
            _ = tokio::time::sleep(LAUNCH_POLL_INTERVAL) => {
                if let Some(result) = try_unlock_on_ports(
                    port,
                    &provider_routing,
                    &app,
                    Some(launched_identity),
                ).await? {
                    return Ok(ModelPickerUnlockResult {
                        launched: true,
                        ..result
                    });
                }
            }
        }
    }
}

async fn try_unlock_on_ports(
    port: u16,
    provider_routing: &ModelProviderRouting,
    app: &ValidatedAppIdentity,
    launched_process: Option<LaunchedProcessIdentity>,
) -> Result<Option<ModelPickerUnlockResult>, CodexRefitError> {
    let attempted_ports = candidate_ports(port);
    for candidate_port in &attempted_ports {
        let candidate_port = *candidate_port;
        let runtime_app = app.clone();
        let identity =
            match run_blocking(move || platform::validate_runtime(candidate_port, &runtime_app))
                .await
            {
                Ok(identity) => identity,
                Err(error) if skip_candidate_runtime_error(port, candidate_port, &error) => {
                    continue;
                }
                Err(error) => return Err(error),
            };
        validate_launched_runtime(&identity, app, launched_process).await?;

        let targets = match discover_targets(candidate_port).await {
            Ok(targets) => targets,
            Err(_) => continue,
        };
        for target in &targets {
            match inject_model_picker_patch(target, provider_routing).await {
                Ok(injected) => {
                    revalidate_runtime(candidate_port, app, &identity).await?;
                    validate_launched_runtime(&identity, app, launched_process).await?;
                    let provider_routing_injected = !provider_routing.routes.is_empty();
                    return Ok(Some(ModelPickerUnlockResult {
                        attempted_ports: attempted_ports.clone(),
                        debug_port: Some(candidate_port),
                        target_id: Some(target.id.clone()),
                        target_title: Some(injected.title),
                        target_url: Some(injected.url),
                        injected: true,
                        provider_routing_injected,
                        provider_routing: provider_routing.clone(),
                        launched: false,
                        message: if provider_routing_injected {
                            "Codex model picker and per-task model provider routing were installed."
                                .to_owned()
                        } else {
                            "Codex model picker compatibility was installed.".to_owned()
                        },
                    }));
                }
                Err(error) if error.code() == CodexRefitErrorCode::TargetValidationFailed => {
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
    }
    Ok(None)
}

fn skip_candidate_runtime_error(
    requested_port: u16,
    candidate_port: u16,
    error: &CodexRefitError,
) -> bool {
    error.code() == CodexRefitErrorCode::SessionUnavailable
        || (candidate_port != requested_port
            && error.code() == CodexRefitErrorCode::TargetValidationFailed)
}

async fn revalidate_runtime(
    port: u16,
    app: &ValidatedAppIdentity,
    identity: &RuntimeIdentity,
) -> Result<(), CodexRefitError> {
    let app = app.clone();
    let identity = identity.clone();
    run_blocking(move || platform::revalidate_runtime(port, &app, &identity)).await
}

async fn validate_launched_runtime(
    identity: &RuntimeIdentity,
    app: &ValidatedAppIdentity,
    launched_process: Option<LaunchedProcessIdentity>,
) -> Result<(), CodexRefitError> {
    let Some(launched_process) = launched_process else {
        return Ok(());
    };
    let identity = identity.clone();
    let app = app.clone();
    run_blocking(move || platform::validate_launched_runtime(&identity, &app, launched_process))
        .await
}

fn candidate_ports(port: u16) -> Vec<u16> {
    let mut ports = vec![port];
    ports.extend(
        FALLBACK_PORTS
            .iter()
            .copied()
            .filter(|value| *value != port),
    );
    ports
}

fn no_renderer_found(port: u16, provider_routing: ModelProviderRouting) -> ModelPickerUnlockResult {
    ModelPickerUnlockResult {
        attempted_ports: candidate_ports(port),
        debug_port: None,
        target_id: None,
        target_title: None,
        target_url: None,
        injected: false,
        provider_routing_injected: false,
        provider_routing,
        launched: false,
        message: "No primary Codex Desktop renderer with an open CDP port was found. Use --launch after quitting Codex, or start the validated app with remote debugging first.".to_owned(),
    }
}

#[derive(Debug, Clone)]
struct InjectionTarget {
    title: String,
    url: String,
}

async fn inject_model_picker_patch(
    target: &RendererTarget,
    provider_routing: &ModelProviderRouting,
) -> Result<InjectionTarget, CodexRefitError> {
    let mut session = tokio::time::timeout(CDP_CONNECT_TIMEOUT, CdpSession::connect(target))
        .await
        .map_err(|_| {
            CodexRefitError::new(
                CodexRefitErrorCode::SessionUnavailable,
                "timed out connecting to the renderer",
            )
        })??;
    let result = inject_into_session(&mut session, provider_routing).await;
    session.close().await;
    result
}

async fn inject_into_session(
    session: &mut CdpSession,
    provider_routing: &ModelProviderRouting,
) -> Result<InjectionTarget, CodexRefitError> {
    validate_main_renderer(session).await?;
    session
        .add_script(MODEL_PICKER_SCRIPT)
        .await
        .map_err(|error| {
            injection_error(format!(
                "failed to register the model-picker compatibility script: {error}"
            ))
        })?;
    let result = session
        .evaluate(MODEL_PICKER_SCRIPT)
        .await
        .map_err(|error| {
            injection_error(format!(
                "model-picker compatibility evaluation failed: {error}"
            ))
        })?;
    let status = result.get("status").and_then(Value::as_str);
    if !matches!(status, Some("installed" | "already-installed")) {
        return Err(injection_error(
            "the primary renderer did not confirm the model-picker patch",
        ));
    }
    if !provider_routing.routes.is_empty() {
        install_model_provider_routes(session, provider_routing).await?;
    }

    Ok(InjectionTarget {
        title: result
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("Codex")
            .to_owned(),
        url: result
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or(MAIN_RENDERER_URL)
            .to_owned(),
    })
}

async fn validate_main_renderer(session: &mut CdpSession) -> Result<(), CodexRefitError> {
    let identity = session
        .evaluate("({ title: document.title, url: location.href })")
        .await?;
    if identity.get("url").and_then(Value::as_str) != Some(MAIN_RENDERER_URL) {
        return Err(CodexRefitError::new(
            CodexRefitErrorCode::TargetValidationFailed,
            "the renderer is not the primary Codex window",
        ));
    }
    Ok(())
}

const FIND_REQUEST_DISPATCHER_EXPORT: &str = r#"
(async () => {
  for (let attempt = 0; attempt < 30; attempt += 1) {
    const urls = [
      ...Array.from(document.querySelectorAll("link[href]")).map((link) => link.href),
      ...performance.getEntriesByType("resource").map((entry) => entry.name),
    ];
    const assetUrl = urls.find((href) =>
      href.includes("/assets/app-initial-") && href.endsWith(".js")
    );
    if (assetUrl) {
      const module = await import(assetUrl);
      const candidate = Object.values(module).find((value) => {
        if (typeof value !== "function") return false;
        const source = Function.prototype.toString.call(value).replace(/\s+/g, "");
        return /^function[\w$]*\(e,t\)\{return[\w$]+\.sendRequest\(e,t\)\}$/.test(source);
      });
      if (candidate) return candidate;
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  throw new Error("Codex request dispatcher export was not found");
})()
"#;

const INSTALL_PROVIDER_ROUTE_FUNCTION: &str = r#"
function (routes, key, defaultProvider) {
  const existing = this[key];
  if (existing) {
    existing.routes = routes;
    existing.defaultProvider = defaultProvider;
    return { status: "updated", routes, defaultProvider };
  }

  const state = {
    routes,
    defaultProvider,
    original: this.sendRequest.bind(this),
    routedCount: 0,
    providerSwitchCount: 0,
    compensationCount: 0,
    lastRoute: null,
    lastProviderSwitch: null,
    threadProviders: new Map(),
    providerSwitches: new Map(),
  };

  const routeForModel = (model) =>
    typeof model === "string" &&
    Object.prototype.hasOwnProperty.call(state.routes, model)
      ? state.routes[model]
      : null;

  const desiredProvider = (params) => {
    if (!params || typeof params !== "object") return null;
    const model = typeof params.model === "string" ? params.model : null;
    const routedProvider = routeForModel(model);
    if (routedProvider != null) return routedProvider;
    const threadId = typeof params.threadId === "string" ? params.threadId : null;
    if (threadId == null || !state.threadProviders.has(threadId)) return null;
    return model == null ? state.threadProviders.get(threadId) : state.defaultProvider;
  };

  const patchThreadParams = (params) => {
    if (!params || typeof params !== "object") return params;
    const model = typeof params.model === "string" ? params.model : null;
    const provider = desiredProvider(params);
    if (provider == null || params.modelProvider === provider) return params;
    state.routedCount += 1;
    state.lastRoute = { model, provider };
    return { ...params, modelProvider: provider };
  };

  const patchEnvelope = (method, params) => {
    if (
      method === "send-cli-request-for-host" &&
      ["thread/start", "thread/resume", "thread/fork"].includes(params?.method)
    ) {
      return { ...params, params: patchThreadParams(params.params) };
    }
    if (method === "start-thread-for-host") return patchThreadParams(params);
    if (method === "prewarm-thread-start-for-host") {
      return { ...params, params: patchThreadParams(params?.params) };
    }
    return params;
  };

  const rememberProvider = (response, requestedProvider) => {
    const threadId =
      typeof response?.thread?.id === "string" ? response.thread.id : null;
    const provider =
      typeof response?.modelProvider === "string"
        ? response.modelProvider
        : typeof response?.thread?.modelProvider === "string"
          ? response.thread.modelProvider
          : requestedProvider;
    if (threadId != null && typeof provider === "string") {
      state.threadProviders.set(threadId, provider);
    }
  };

  const resumeThread = (hostId, threadId, model, provider) =>
    state.original("send-cli-request-for-host", {
      hostId,
      method: "thread/resume",
      params: {
        threadId,
        model,
        modelProvider: provider,
        excludeTurns: true,
      },
    });

  const ensureTurnProvider = async (envelope) => {
    if (
      envelope?.hostId !== "local" ||
      envelope?.method !== "turn/start" ||
      !envelope.params ||
      typeof envelope.params !== "object"
    ) {
      return;
    }
    const { threadId, model } = envelope.params;
    if (typeof threadId !== "string" || typeof model !== "string") return;
    const routedProvider = routeForModel(model);
    const knownProvider = state.threadProviders.get(threadId);
    const provider =
      routedProvider ?? (knownProvider == null ? null : state.defaultProvider);
    if (provider == null || knownProvider === provider) return;

    let switching = state.providerSwitches.get(threadId);
    if (!switching) {
      switching = (async () => {
        await state.original("send-cli-request-for-host", {
          hostId: envelope.hostId,
          method: "thread/unsubscribe",
          params: { threadId },
        });
        try {
          const resumed = await resumeThread(
            envelope.hostId,
            threadId,
            model,
            provider,
          );
          if (resumed?.modelProvider !== provider) {
            throw new Error(
              `Codex kept provider ${String(resumed?.modelProvider)} instead of ${provider}`,
            );
          }
          state.threadProviders.set(threadId, provider);
          state.providerSwitchCount += 1;
          state.lastProviderSwitch = { threadId, model, provider };
        } catch (error) {
          const fallbackProvider = knownProvider ?? state.defaultProvider;
          try {
            const restored = await resumeThread(
              envelope.hostId,
              threadId,
              model,
              fallbackProvider,
            );
            rememberProvider(restored, fallbackProvider);
            state.compensationCount += 1;
          } catch (_) {}
          throw error;
        }
      })().finally(() => {
        state.providerSwitches.delete(threadId);
      });
      state.providerSwitches.set(threadId, switching);
    }
    await switching;
  };

  this.sendRequest = async (method, params, ...rest) => {
    if (method === "send-cli-request-for-host") {
      await ensureTurnProvider(params);
    }
    const patchedParams = patchEnvelope(method, params);
    const response = await state.original(method, patchedParams, ...rest);
    if (
      method === "send-cli-request-for-host" &&
      ["thread/start", "thread/resume", "thread/fork"].includes(patchedParams?.method)
    ) {
      rememberProvider(response, patchedParams?.params?.modelProvider);
    }
    return response;
  };
  this[key] = state;
  window[key] = state;
  return { status: "installed", routes, defaultProvider };
}
"#;

async fn install_model_provider_routes(
    session: &mut CdpSession,
    provider_routing: &ModelProviderRouting,
) -> Result<(), CodexRefitError> {
    let result = install_model_provider_routes_inner(session, provider_routing).await;
    let _ = session
        .release_object_group(PROVIDER_ROUTE_OBJECT_GROUP)
        .await;
    result
}

async fn install_model_provider_routes_inner(
    session: &mut CdpSession,
    provider_routing: &ModelProviderRouting,
) -> Result<(), CodexRefitError> {
    let dispatcher_export = session
        .evaluate_remote_object(FIND_REQUEST_DISPATCHER_EXPORT, PROVIDER_ROUTE_OBJECT_GROUP)
        .await?;
    let dispatcher_function_id = remote_object_id(&dispatcher_export)?;
    let dispatcher_binding = dispatcher_binding(
        dispatcher_export
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )
    .ok_or_else(|| injection_error("the Codex request dispatcher has an unsupported shape"))?;

    let function_properties = session.get_properties(dispatcher_function_id).await?;
    let scopes_id = function_properties
        .get("internalProperties")
        .and_then(Value::as_array)
        .and_then(|properties| {
            properties
                .iter()
                .find(|property| property.get("name").and_then(Value::as_str) == Some("[[Scopes]]"))
        })
        .and_then(|property| property.get("value"))
        .ok_or_else(|| injection_error("the Codex request dispatcher scope was not exposed"))
        .and_then(remote_object_id)?;

    let scope_list = session.get_properties(scopes_id).await?;
    let scope_values = scope_list
        .get("result")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            injection_error("the Codex request dispatcher returned an invalid scope list")
        })?;
    let scope_ids = scope_values
        .iter()
        .filter(|property| {
            property
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| name.bytes().all(|byte| byte.is_ascii_digit()))
        })
        .filter_map(|property| property.get("value"))
        .filter_map(|value| remote_object_id(value).ok().map(str::to_owned))
        .take(8)
        .collect::<Vec<_>>();

    let mut request_dispatcher_id = None;
    for scope_id in scope_ids {
        let scope_properties = session.get_properties(&scope_id).await?;
        request_dispatcher_id = scope_properties
            .get("result")
            .and_then(Value::as_array)
            .and_then(|properties| {
                properties.iter().find(|property| {
                    property.get("name").and_then(Value::as_str)
                        == Some(dispatcher_binding.as_str())
                })
            })
            .and_then(|property| property.get("value"))
            .and_then(|value| remote_object_id(value).ok().map(str::to_owned));
        if request_dispatcher_id.is_some() {
            break;
        }
    }
    let request_dispatcher_id = request_dispatcher_id
        .ok_or_else(|| injection_error("the Codex request dispatcher instance was not found"))?;

    let installed = session
        .call_function_on(
            &request_dispatcher_id,
            INSTALL_PROVIDER_ROUTE_FUNCTION,
            json!([
                { "value": provider_routing.routes },
                { "value": PROVIDER_ROUTE_KEY },
                { "value": provider_routing.default_provider }
            ]),
        )
        .await?;
    if !matches!(
        installed
            .get("value")
            .and_then(|value| value.get("status"))
            .and_then(Value::as_str),
        Some("installed" | "updated")
    ) {
        return Err(injection_error(
            "the Codex request dispatcher did not confirm provider routing",
        ));
    }
    Ok(())
}

fn remote_object_id(value: &Value) -> Result<&str, CodexRefitError> {
    value
        .get("objectId")
        .and_then(Value::as_str)
        .filter(|object_id| !object_id.is_empty() && object_id.len() <= MAX_REMOTE_OBJECT_ID_BYTES)
        .ok_or_else(|| {
            injection_error("the renderer did not return a valid remote object identifier")
        })
}

fn dispatcher_binding(description: &str) -> Option<String> {
    let compact = description
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    let start = compact.find("{return")? + "{return".len();
    let end = compact[start..].find(".sendRequest(")? + start;
    let binding = &compact[start..end];
    (!binding.is_empty()
        && binding.len() <= 128
        && binding
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')))
    .then(|| binding.to_owned())
}

fn validate_route_value(kind: &str, value: &str) -> Result<(), CodexRefitError> {
    if value.is_empty()
        || value.len() > MAX_ROUTE_VALUE_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(injection_error(format!(
            "{kind} route values must be non-empty, trimmed, control-free, and at most {MAX_ROUTE_VALUE_BYTES} bytes"
        )));
    }
    Ok(())
}

fn injection_error(message: impl Into<String>) -> CodexRefitError {
    CodexRefitError::new(CodexRefitErrorCode::InjectionFailed, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn routing() -> ModelProviderRouting {
        ModelProviderRouting::new(
            BTreeMap::from([(
                "sf-deepseek-v3.2".to_owned(),
                "opsail-gateway-model".to_owned(),
            )]),
            "openai",
        )
        .unwrap()
    }

    #[test]
    fn model_picker_script_has_bounded_local_patch_targets() {
        for required in [
            "107580212",
            "use_hidden_models",
            "getDynamicConfig",
            "Response.prototype.json",
            "__reactFiber",
            "name === MODEL_CONFIG_KEY",
            "MAX_GRAPH_NODES",
            "MAX_MODEL_COUNT",
            MAIN_RENDERER_URL,
        ] {
            assert!(MODEL_PICKER_SCRIPT.contains(required), "{required}");
        }
        for forbidden in ["fetch(", "XMLHttpRequest", "WebSocket("] {
            assert!(!MODEL_PICKER_SCRIPT.contains(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn explicit_debug_port_precedes_bounded_fallbacks() {
        let ports = candidate_ports(55321);
        assert_eq!(ports[0], 55321);
        assert_eq!(&ports[1..], FALLBACK_PORTS);

        let ports = candidate_ports(9222);
        assert_eq!(ports[0], 9222);
        assert_eq!(ports.iter().filter(|port| **port == 9222).count(), 1);
    }

    #[test]
    fn runtime_validation_only_skips_absent_or_untrusted_fallback_ports() {
        let unavailable =
            CodexRefitError::new(CodexRefitErrorCode::SessionUnavailable, "not listening");
        assert!(skip_candidate_runtime_error(55321, 55321, &unavailable));

        let untrusted =
            CodexRefitError::new(CodexRefitErrorCode::TargetValidationFailed, "wrong owner");
        assert!(!skip_candidate_runtime_error(55321, 55321, &untrusted));
        assert!(skip_candidate_runtime_error(55321, 9222, &untrusted));
    }

    #[tokio::test]
    async fn privileged_ports_fail_before_platform_or_target_discovery() {
        let error = unlock_model_picker(80, false, routing()).await.unwrap_err();
        assert_eq!(error.code(), CodexRefitErrorCode::TargetValidationFailed);
        assert!(error.to_string().contains("between 1024 and 65535"));
    }

    #[test]
    fn provider_route_patch_carries_switch_and_compensation_contracts() {
        for required in [
            "thread/unsubscribe",
            "thread/resume",
            "turn/start",
            "modelProvider",
            "compensationCount",
            "fallbackProvider",
            "hasOwnProperty.call",
        ] {
            assert!(
                INSTALL_PROVIDER_ROUTE_FUNCTION.contains(required),
                "{required}"
            );
        }
    }

    #[test]
    fn routing_validation_is_bounded() {
        assert!(
            ModelProviderRouting::new(BTreeMap::new(), " provider")
                .unwrap_err()
                .to_string()
                .contains("default provider")
        );
        let routes = (0..=MAX_PROVIDER_ROUTES)
            .map(|index| (format!("model-{index}"), "provider".to_owned()))
            .collect();
        assert!(ModelProviderRouting::new(routes, "openai").is_err());
    }

    #[test]
    fn dispatcher_binding_accepts_only_the_verified_wrapper_shape() {
        assert_eq!(
            dispatcher_binding("function Z(e,t){return Q.sendRequest(e,t)}"),
            Some("Q".to_owned())
        );
        assert_eq!(
            dispatcher_binding("function Z(e,t){return Q.value(e,t)}"),
            None
        );
        assert_eq!(
            dispatcher_binding("function Z(e,t){return bad-name.sendRequest(e,t)}"),
            None
        );
    }

    #[test]
    fn result_serialization_exposes_routing_receipts() {
        let result = ModelPickerUnlockResult {
            attempted_ports: vec![55321],
            debug_port: Some(55321),
            target_id: Some("renderer-1".to_owned()),
            target_title: Some("Codex".to_owned()),
            target_url: Some(MAIN_RENDERER_URL.to_owned()),
            injected: true,
            provider_routing_injected: true,
            provider_routing: routing(),
            launched: false,
            message: "installed".to_owned(),
        };
        let value = serde_json::to_value(result).unwrap();
        assert_eq!(value["providerRoutingInjected"], true);
        assert_eq!(
            value["providerRouting"]["routes"]["sf-deepseek-v3.2"],
            "opsail-gateway-model"
        );
        assert_eq!(value["providerRouting"]["defaultProvider"], "openai");
    }
}
