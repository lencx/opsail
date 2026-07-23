//! CDP-based model picker unlock for Codex Desktop.
//!
//! Codex Desktop renderer filters the model menu through a Statsig dynamic config gate
//! (`107580212`) with a remote `available_models` allowlist. Third-party providers
//! configured via `config.toml` and `model-catalog.json` are invisible even though
//! the data reaches the renderer. This module injects a renderer patch through CDP
//! that expands the allowlist and forces `use_hidden_models = false`.

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::cdp::{CdpSession, discover_targets};
use crate::error::{CodexRefitError, CodexRefitErrorCode};
use crate::platform;

const CDP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long to wait for the renderer to appear after launching Codex.
const LAUNCH_WAIT_TIMEOUT: Duration = Duration::from_secs(45);
const LAUNCH_POLL_INTERVAL: Duration = Duration::from_millis(500);
/// CDP ports to try when the configured port does not respond.
const FALLBACK_PORTS: &[u16] = &[9222, 9223, 9229, 9230, 9231];
/// Key for the injected global state so repeated injections are idempotent.
const PATCH_KEY: &str = "__opsailCodexModelPickerUnlockV1";

/// Result of a model picker unlock operation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPickerUnlockResult {
    pub attempted_ports: Vec<u16>,
    pub debug_port: Option<u16>,
    pub target_id: Option<String>,
    pub target_title: Option<String>,
    pub target_url: Option<String>,
    pub injected: bool,
    pub launched: bool,
    pub message: String,
}

/// Try to unlock the model picker on a Codex Desktop renderer.
///
/// When `launch_if_stopped` is true and no renderer is reachable, the validated
/// ChatGPT application is launched with `--remote-debugging-port=<port>` and
/// we wait for its renderer to appear before injecting.
pub async fn unlock_model_picker(
    port: u16,
    launch_if_stopped: bool,
) -> Result<ModelPickerUnlockResult, CodexRefitError> {
    // First try to attach to an already-running Codex.
    if let Some(result) = try_unlock_on_port(port).await {
        return Ok(result);
    }

    if !launch_if_stopped {
        return Ok(no_renderer_found(port));
    }

    // Check if Codex is already running without CDP.
    if platform::is_supported() {
        let app = platform::validate_app()?;
        if platform::debug_listener_present(port)? {
            return Err(CodexRefitError::new(
                CodexRefitErrorCode::SessionUnavailable,
                "a debug listener exists but no injectable Codex renderer was found",
            ));
        }
        if platform::app_is_running(&app)? {
            return Err(CodexRefitError::new(
                CodexRefitErrorCode::RestartRequired,
                "Codex is already running without CDP. Quit Codex first, then retry with --launch.",
            ));
        }
        // Launch Codex with remote debugging.
        let launched = platform::launch_app(port, &app)?;
        let mut exit = launched.exit_receiver();

        // Wait for renderer to appear.
        let deadline = tokio::time::Instant::now() + LAUNCH_WAIT_TIMEOUT;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    return Err(CodexRefitError::new(
                        CodexRefitErrorCode::TargetNotFound,
                        "Codex was launched but the renderer did not appear within the timeout",
                    ));
                }
                _ = exit.changed() => {
                    if *exit.borrow() {
                        return Err(CodexRefitError::new(
                            CodexRefitErrorCode::TargetNotFound,
                            "Codex exited before its renderer became available",
                        ));
                    }
                }
                _ = tokio::time::sleep(LAUNCH_POLL_INTERVAL) => {
                    if let Some(result) = try_unlock_on_port(port).await {
                        return Ok(ModelPickerUnlockResult {
                            launched: true,
                            ..result
                        });
                    }
                }
            }
        }
    }

    Ok(ModelPickerUnlockResult {
        attempted_ports: vec![port],
        debug_port: None,
        target_id: None,
        target_title: None,
        target_url: None,
        injected: false,
        launched: true,
        message: "Codex was launched but the CDP endpoint never became available.".to_string(),
    })
}

async fn try_unlock_on_port(port: u16) -> Option<ModelPickerUnlockResult> {
    let mut attempted = vec![port];
    attempted.extend(FALLBACK_PORTS.iter().filter(|&&p| p != port).copied());
    attempted.sort_unstable();
    attempted.dedup();

    for &candidate_port in &attempted {
        let targets = match discover_targets(candidate_port).await {
            Ok(targets) => targets,
            Err(_) => continue,
        };

        for target in &targets {
            if let Ok(injected) = inject_model_picker_patch(target).await {
                return Some(ModelPickerUnlockResult {
                    attempted_ports: attempted,
                    debug_port: Some(candidate_port),
                    target_id: Some(target.id.clone()),
                    target_title: Some(injected.title),
                    target_url: Some(injected.url),
                    injected: true,
                    launched: false,
                    message: "Codex model picker whitelist was patched successfully.".to_string(),
                });
            }
        }
    }
    None
}

fn no_renderer_found(port: u16) -> ModelPickerUnlockResult {
    ModelPickerUnlockResult {
        attempted_ports: vec![port],
        debug_port: None,
        target_id: None,
        target_title: None,
        target_url: None,
        injected: false,
        launched: false,
        message: "No Codex Desktop renderer with an open CDP port was found. Use --launch to start Codex with remote debugging, or launch Codex manually with --remote-debugging-port=<PORT> first.".to_string(),
    }
}

#[derive(Debug, Clone)]
struct InjectionTarget {
    title: String,
    url: String,
}

async fn inject_model_picker_patch(
    target: &crate::cdp::RendererTarget,
) -> Result<InjectionTarget, CodexRefitError> {
    let mut session = tokio::time::timeout(CDP_CONNECT_TIMEOUT, CdpSession::connect(target))
        .await
        .map_err(|_| {
            CodexRefitError::new(
                CodexRefitErrorCode::SessionUnavailable,
                "timed out connecting to the renderer",
            )
        })??;

    let result = inject_script(&mut session).await;
    session.close().await;
    result
}

async fn inject_script(session: &mut CdpSession) -> Result<InjectionTarget, CodexRefitError> {
    let script = build_model_picker_unlock_script();

    // Register the script for new document navigations.
    let _identifier = session.add_script(&script).await.map_err(|error| {
        CodexRefitError::new(
            CodexRefitErrorCode::InjectionFailed,
            format!("failed to register model picker patch: {error}"),
        )
    })?;

    // Evaluate immediately to patch the current page.
    let result = session.evaluate(&script).await.map_err(|error| {
        CodexRefitError::new(
            CodexRefitErrorCode::InjectionFailed,
            format!("model picker patch evaluation failed: {error}"),
        )
    })?;

    let title = result
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Codex")
        .to_string();
    let url = result
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    Ok(InjectionTarget { title, url })
}

/// Inject the model picker unlock script into an existing CDP session.
///
/// This is used when model-picker-unlock is requested alongside other features
/// (e.g. `enable usage unlock-model-picker`) so the same session is reused.
pub(crate) async fn inject_model_picker_into_session(
    session: &mut CdpSession,
) -> Result<(), CodexRefitError> {
    let script = build_model_picker_unlock_script();
    let _identifier = session.add_script(&script).await.map_err(|error| {
        CodexRefitError::new(
            CodexRefitErrorCode::InjectionFailed,
            format!("failed to register model picker patch: {error}"),
        )
    })?;
    session.evaluate(&script).await.map_err(|error| {
        CodexRefitError::new(
            CodexRefitErrorCode::InjectionFailed,
            format!("model picker patch evaluation failed: {error}"),
        )
    })?;
    Ok(())
}

/// Build the renderer injection script that patches the Statsig model whitelist gate.
///
/// This script:
/// 1. Patches `StatsigClient.getDynamicConfig` for gate `107580212` to expand `available_models`
///    and force `use_hidden_models = false`.
/// 2. Patches `Response.prototype.json` to intercept model list responses.
/// 3. Patches the app-server `sendRequest` to intercept `list-models-for-host` responses.
/// 4. Patches the React fiber state graph to fix memoized model lists.
/// 5. Runs on a 1.5s interval to catch late-initializing state.
fn build_model_picker_unlock_script() -> String {
    let patch_key_json = serde_json::to_string(PATCH_KEY).unwrap();
    format!(
        r#"
(async () => {{
  const PATCH_KEY = {patch_key_json};
  const state = window[PATCH_KEY] || {{ failures: [] }};
  window[PATCH_KEY] = state;
  if (state.installed) return {{ title: document.title, url: location.href, status: "already-installed" }};
  state.installed = true;

  // ---- helpers ----
  const isModelDescriptor = (value) => value && typeof value === "object" && typeof value.model === "string";
  const isModelArray = (value) => Array.isArray(value) && value.every(isModelDescriptor);

  const patchModelArray = (models) => {{
    if (!isModelArray(models)) return false;
    let changed = false;
    for (const model of models) {{
      if (model.hidden !== false) {{ model.hidden = false; changed = true; }}
    }}
    return changed;
  }};

  const patchModelContainer = (value) => {{
    if (!value || typeof value !== "object") return false;
    let changed = false;
    if (patchModelArray(value.models)) changed = true;
    if (patchModelArray(value.data)) changed = true;
    if (patchModelArray(value.result)) changed = true;
    if (patchModelArray(value.result?.data)) changed = true;
    if (patchModelArray(value.result?.models)) changed = true;
    if (patchModelArray(value.message?.result?.data)) changed = true;
    if (patchModelArray(value.message?.result?.models)) changed = true;
    // Force hidden models off for statsig-like configs.
    if (("availableModels" in value || "available_models" in value || "useHiddenModels" in value || "use_hidden_models" in value) && value.useHiddenModels !== false) {{
      value.useHiddenModels = false; changed = true;
    }}
    if (("availableModels" in value || "available_models" in value || "useHiddenModels" in value || "use_hidden_models" in value) && value.use_hidden_models !== false) {{
      value.use_hidden_models = false; changed = true;
    }}
    return changed;
  }};

  const patchObjectGraph = (root, visited = new WeakSet(), depth = 0) => {{
    if (!root || typeof root !== "object" || visited.has(root) || depth > 6) return false;
    visited.add(root);
    let changed = patchModelContainer(root);
    if (root instanceof Element || root === window || root === document || root === document.body) return changed;
    for (const key of Object.keys(root)) {{
      if (["ownerDocument", "parentElement", "parentNode", "children", "childNodes"].includes(key)) continue;
      try {{ if (patchObjectGraph(root[key], visited, depth + 1)) changed = true; }} catch (_) {{}}
    }}
    return changed;
  }};

  // ---- patch Statsig dynamic config gate 107580212 ----
  const patchStatsigConfig = (config) => {{
    if (!config?.value || typeof config.value !== "object") return config;
    const value = config.value;
    const next = {{ ...value, use_hidden_models: false }};
    try {{ config.value = next; }} catch (_) {{ return {{ ...config, value: next }}; }}
    return config;
  }};

  const statsigRoot = () => window.__STATSIG__ || globalThis.__STATSIG__;
  const statsigClients = () => {{
    const root = statsigRoot();
    if (!root || typeof root !== "object") return [];
    const clients = [root.firstInstance, typeof root.instance === "function" ? root.instance() : null];
    if (root.instances && typeof root.instances === "object") clients.push(...Object.values(root.instances));
    return clients.filter((c, i, arr) => c && typeof c === "object" && arr.indexOf(c) === i);
  }};

  const patchStatsig = () => {{
    for (const client of statsigClients()) {{
      if (typeof client.getDynamicConfig !== "function") continue;
      if (!client.__opsailModelWhitelistPatched) {{
        const original = client.getDynamicConfig.bind(client);
        client.getDynamicConfig = (name, options) => patchStatsigConfig(original(name, options));
        client.__opsailModelWhitelistPatched = true;
      }}
      try {{ patchStatsigConfig(client.getDynamicConfig("107580212", {{ disableExposureLog: true }})); }} catch (_) {{}}
    }}
  }};

  // ---- patch Response.json() ----
  const installResponsePatch = () => {{
    if (state.responsePatchInstalled || typeof Response === "undefined") return;
    state.responsePatchInstalled = true;
    const originalJson = Response.prototype.json;
    Response.prototype.json = async function patchedJson(...args) {{
      const data = await originalJson.apply(this, args);
      try {{ patchModelContainer(data); patchObjectGraph(data); }} catch (_) {{}}
      return data;
    }};
  }};

  // ---- patch app-server sendRequest for list-models-for-host ----
  const installAppServerPatch = () => {{
    if (state.appServerPatchAttempted) return;
    state.appServerPatchAttempted = true;
    // Find the app-server module from loaded assets.
    const urls = [
      ...Array.from(document.scripts || []).map(s => s.src),
      ...Array.from(document.querySelectorAll("link[href]") || []).map(l => l.href),
      ...performance.getEntriesByType("resource").map(e => e.name),
    ].filter(Boolean);
    const assetUrl = urls.find(u => u.includes("/assets/") && u.includes("app-server") && u.endsWith(".js"));
    if (!assetUrl) return;
    import(assetUrl).then(module => {{
      for (const candidate of Object.values(module)) {{
        if (!candidate || typeof candidate !== "object") continue;
        if (typeof candidate.sendRequest !== "function") continue;
        if (candidate.__opsailModelRequestPatch) continue;
        const original = candidate.sendRequest.bind(candidate);
        candidate.sendRequest = async function(method, params, options) {{
          const result = await original(method, params, options);
          const methodName = method === "send-cli-request-for-host" && params?.method ? String(params.method) : String(method || "");
          if (methodName === "list-models-for-host" || methodName === "model/list") {{
            try {{
              patchModelContainer(result);
              patchObjectGraph(result);
            }} catch (_) {{}}
          }}
          return result;
        }};
        candidate.__opsailModelRequestPatch = true;
      }}
    }}).catch(() => {{}});
  }};

  // ---- patch React fiber state ----
  const reactFiberKeys = (el) => Object.keys(el || {{}}).filter(k => k.startsWith("__reactFiber") || k.startsWith("__reactInternalInstance") || k.startsWith("__reactProps"));

  const patchReactState = () => {{
    const visited = new WeakSet();
    const nodes = [document.body, ...document.querySelectorAll("button, [role='menu'], [role='dialog'], [data-radix-popper-content-wrapper]")].filter(Boolean);
    for (const node of nodes.slice(0, 200)) {{
      for (const key of reactFiberKeys(node)) patchObjectGraph(node[key], visited);
    }}
  }};

  // ---- run ----
  installResponsePatch();
  installAppServerPatch();
  patchStatsig();
  patchReactState();

  if (!state.interval) {{
    state.interval = setInterval(() => {{
      patchStatsig();
      patchReactState();
    }}, 1500);
  }}

  return {{ title: document.title, url: location.href, status: "ok", patchKey: PATCH_KEY }};
}})()
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlock_script_contains_required_patch_targets() {
        let script = build_model_picker_unlock_script();
        assert!(script.contains("107580212"));
        assert!(script.contains("use_hidden_models"));
        assert!(script.contains("useHiddenModels"));
        assert!(script.contains("getDynamicConfig"));
        assert!(script.contains("list-models-for-host"));
        assert!(script.contains("model/list"));
        assert!(script.contains("Response.prototype.json"));
        assert!(script.contains(PATCH_KEY));
        assert!(script.contains("__reactFiber"));
        assert!(script.contains("setInterval"));
    }

    #[test]
    fn unlock_script_is_idempotent() {
        let script = build_model_picker_unlock_script();
        assert!(script.contains("state.installed"));
        assert!(script.contains(r#""already-installed""#));
    }

    #[test]
    fn unlock_script_does_not_make_network_calls() {
        let script = build_model_picker_unlock_script();
        for forbidden in ["fetch(", "XMLHttpRequest", "WebSocket("] {
            assert!(!script.contains(forbidden), "found {forbidden}");
        }
    }

    #[test]
    fn result_serializes_with_all_required_fields() {
        let result = ModelPickerUnlockResult {
            attempted_ports: vec![55321],
            debug_port: Some(55321),
            target_id: Some("renderer-1".to_string()),
            target_title: Some("Codex".to_string()),
            target_url: Some("app://-/index.html".to_string()),
            injected: true,
            launched: false,
            message: "patched".to_string(),
        };
        let json = serde_json::to_string_pretty(&result).unwrap();
        assert!(json.contains("attemptedPorts"));
        assert!(json.contains("debugPort"));
        assert!(json.contains("targetId"));
        assert!(json.contains("injected"));
        assert!(json.contains("message"));
    }
}
