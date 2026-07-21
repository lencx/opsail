use std::sync::OnceLock;

use serde_json::{Value, json};

use crate::error::{CodexRefitError, CodexRefitErrorCode};
use crate::model::SessionMode;

const MODEL_SOURCE: &str = include_str!("../assets/opsail-refit-codex-usage-model.js");
const RUNTIME_TEMPLATE: &str = include_str!("../assets/opsail-refit-codex-usage-runtime.js");
const CSS_SOURCE: &str = include_str!("../assets/opsail-refit-codex-usage.css");
const EN_LOCALE: &str = include_str!("../assets/locales/en.json");
const ZH_CN_LOCALE: &str = include_str!("../assets/locales/zh-CN.json");

const MODEL_PLACEHOLDER: &str = "__OPSAIL_REFIT_CODEX_MODEL_SOURCE__";
const VERSION_PLACEHOLDER: &str = "__OPSAIL_REFIT_CODEX_VERSION_JSON__";
const REVISION_PLACEHOLDER: &str = "__OPSAIL_REFIT_CODEX_REVISION_JSON__";
const CSS_PLACEHOLDER: &str = "__OPSAIL_REFIT_CODEX_CSS_JSON__";
const LOCALES_PLACEHOLDER: &str = "__OPSAIL_REFIT_CODEX_LOCALES_JSON__";
const SESSION_MODE_PLACEHOLDER: &str = "__OPSAIL_REFIT_CODEX_SESSION_MODE_JSON__";
const MANAGER_TOKEN_PLACEHOLDER: &str = "__OPSAIL_REFIT_CODEX_MANAGER_TOKEN_JSON__";

#[derive(Clone)]
pub(crate) struct UsagePayload {
    current_template: String,
    pub revision: String,
}

impl UsagePayload {
    pub fn current(&self, mode: SessionMode, manager_token: &str) -> String {
        self.current_template
            .replace(
                SESSION_MODE_PLACEHOLDER,
                &serde_json::to_string(&mode)
                    .expect("serializing an in-memory session mode cannot fail"),
            )
            .replace(MANAGER_TOKEN_PLACEHOLDER, &json_string(manager_token))
    }

    pub fn early(&self, manager_token: &str) -> String {
        early_expression(
            &self.current(SessionMode::Persistent, manager_token),
            &self.revision,
        )
    }
}

pub(crate) fn usage_payload() -> Result<UsagePayload, CodexRefitError> {
    static PAYLOAD: OnceLock<Result<UsagePayload, String>> = OnceLock::new();
    PAYLOAD
        .get_or_init(build_payload)
        .as_ref()
        .cloned()
        .map_err(|message| CodexRefitError::new(CodexRefitErrorCode::InjectionFailed, message))
}

pub(crate) fn status_expression() -> String {
    let revision = json_string(&payload_revision());
    format!(
        r##"(() => {{
          const runtime = window.__OPSAIL_REFIT_CODEX_STATE__;
          let diagnostics = null;
          try {{ diagnostics = runtime?.diagnostics?.() ?? null; }} catch {{}}
          return {{
            installed: Boolean(runtime && runtime.mode === "usage"),
            revision: runtime?.revision ?? null,
            expectedRevision: {revision},
            diagnostics,
            hostCount: document.querySelectorAll("#opsail-refit-codex-usage").length,
            styleCount: document.querySelectorAll("#opsail-refit-codex-usage-style").length,
            detailsCount: document.querySelectorAll("#opsail-refit-codex-usage-details").length
          }};
        }})()"##
    )
}

pub(crate) fn renderer_probe_expression() -> &'static str {
    r#"(() => ({
      appProtocol: location.protocol === "app:",
      shell: Boolean(document.querySelector("main.main-surface")),
      sidebar: Boolean(document.querySelector(
        "aside.app-shell-left-panel, aside[data-testid='app-shell-floating-left-panel']"
      )),
      bridge: typeof window.electronBridge?.sendMessageFromView === "function"
    }))()"#
}

pub(crate) fn disable_expression() -> &'static str {
    r#"(() => {
      window.__OPSAIL_REFIT_CODEX_DISABLED__ = true;
      window.__OPSAIL_REFIT_CODEX_EARLY_GENERATION__ = `disabled:${Date.now()}`;
      try { window.__OPSAIL_REFIT_CODEX_EARLY_STATE__?.cleanup?.(); } catch {}
      try { window.__OPSAIL_REFIT_CODEX_STATE__?.cleanup?.(); } catch {}
      document.getElementById("opsail-refit-codex-usage")?.remove();
      document.getElementById("opsail-refit-codex-usage-details")?.remove();
      document.getElementById("opsail-refit-codex-usage-style")?.remove();
      document.documentElement?.classList.remove("opsail-refit-codex-usage-enabled");
      delete window.__OPSAIL_REFIT_CODEX_STATE__;
      delete window.__OPSAIL_REFIT_CODEX_EARLY_STATE__;
      return {
        clean: !document.getElementById("opsail-refit-codex-usage")
          && !document.getElementById("opsail-refit-codex-usage-details")
          && !document.getElementById("opsail-refit-codex-usage-style")
          && !window.__OPSAIL_REFIT_CODEX_STATE__
      };
    })()"#
}

fn build_payload() -> Result<UsagePayload, String> {
    let en: Value = serde_json::from_str(EN_LOCALE)
        .map_err(|_| "embedded English locale JSON is invalid".to_owned())?;
    let zh_cn: Value = serde_json::from_str(ZH_CN_LOCALE)
        .map_err(|_| "embedded Chinese locale JSON is invalid".to_owned())?;
    validate_locale_pair(&en, &zh_cn)?;
    let locales = json!({
        "defaultLocale": "en",
        "locales": {
            "en": en,
            "zh-CN": zh_cn
        }
    });
    let revision = payload_revision();
    let current_template = RUNTIME_TEMPLATE
        .replace(MODEL_PLACEHOLDER, MODEL_SOURCE)
        .replace(VERSION_PLACEHOLDER, &json_string(env!("CARGO_PKG_VERSION")))
        .replace(REVISION_PLACEHOLDER, &json_string(&revision))
        .replace(CSS_PLACEHOLDER, &json_string(CSS_SOURCE))
        .replace(LOCALES_PLACEHOLDER, &locales.to_string());
    for placeholder in [
        MODEL_PLACEHOLDER,
        VERSION_PLACEHOLDER,
        REVISION_PLACEHOLDER,
        CSS_PLACEHOLDER,
        LOCALES_PLACEHOLDER,
    ] {
        if current_template.contains(placeholder) {
            return Err("renderer payload contains an unresolved placeholder".to_owned());
        }
    }
    if !current_template.contains(SESSION_MODE_PLACEHOLDER)
        || !current_template.contains(MANAGER_TOKEN_PLACEHOLDER)
    {
        return Err("renderer payload is missing its session metadata placeholders".to_owned());
    }
    Ok(UsagePayload {
        current_template,
        revision,
    })
}

fn early_expression(payload: &str, revision: &str) -> String {
    let revision = json_string(revision);
    format!(
        r#"(() => {{
          const STATE_KEY = "__OPSAIL_REFIT_CODEX_EARLY_STATE__";
          const GENERATION_KEY = "__OPSAIL_REFIT_CODEX_EARLY_GENERATION__";
          const generation = {revision};
          const installToken = {{}};
          try {{ window[STATE_KEY]?.cleanup?.(); }} catch {{}}
          window[GENERATION_KEY] = generation;
          let observer = null;
          let timeout = null;
          const cleanup = () => {{
            if (window[STATE_KEY]?.installToken !== installToken) return false;
            observer?.disconnect();
            observer = null;
            if (timeout !== null) clearTimeout(timeout);
            timeout = null;
            delete window[STATE_KEY];
            return true;
          }};
          const install = () => {{
            if (window[GENERATION_KEY] !== generation) {{ cleanup(); return true; }}
            if (!document.documentElement) return false;
            if (location.protocol !== "app:") {{ cleanup(); return true; }}
            const bridge = typeof window.electronBridge?.sendMessageFromView === "function";
            const shell = Boolean(document.querySelector("main.main-surface"));
            const sidebar = Boolean(document.querySelector(
              "aside.app-shell-left-panel, aside[data-testid='app-shell-floating-left-panel']"
            ));
            if (!bridge || !shell || !sidebar) return false;
            cleanup();
            {payload};
            return true;
          }};
          window[STATE_KEY] = {{ cleanup, installToken }};
          if (!install()) {{
            if (typeof MutationObserver === "function" && document.documentElement) {{
              observer = new MutationObserver(install);
              observer.observe(document.documentElement, {{ childList: true, subtree: true }});
            }}
            timeout = setTimeout(cleanup, 30000);
          }}
        }})()"#
    )
}

fn payload_revision() -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in MODEL_SOURCE
        .bytes()
        .chain(RUNTIME_TEMPLATE.bytes())
        .chain(CSS_SOURCE.bytes())
        .chain(EN_LOCALE.bytes())
        .chain(ZH_CN_LOCALE.bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{}-{hash:016x}", env!("CARGO_PKG_VERSION"))
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("serializing an in-memory string cannot fail")
}

fn validate_locale_pair(en: &Value, zh_cn: &Value) -> Result<(), String> {
    let mut en_messages = Vec::new();
    let mut zh_messages = Vec::new();
    collect_messages(en, "", &mut en_messages)?;
    collect_messages(zh_cn, "", &mut zh_messages)?;
    en_messages.sort();
    zh_messages.sort();
    if en_messages != zh_messages {
        return Err(
            "embedded locale JSON files do not expose matching message keys and variables"
                .to_owned(),
        );
    }
    Ok(())
}

fn collect_messages(
    value: &Value,
    prefix: &str,
    messages: &mut Vec<(String, Vec<String>)>,
) -> Result<(), String> {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                collect_messages(value, &path, messages)?;
            }
            Ok(())
        }
        Value::String(value) => {
            messages.push((prefix.to_owned(), message_variables(value)));
            Ok(())
        }
        _ => Err("embedded locale JSON values must be strings or objects".to_owned()),
    }
}

fn message_variables(value: &str) -> Vec<String> {
    let mut variables = Vec::new();
    let mut remaining = value;
    while let Some(start) = remaining.find('{') {
        remaining = &remaining[start + 1..];
        let Some(end) = remaining.find('}') else {
            break;
        };
        let candidate = &remaining[..end];
        let valid = candidate.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphabetic() || (index > 0 && byte.is_ascii_digit())
        });
        if valid && !candidate.is_empty() {
            variables.push(candidate.to_owned());
        }
        remaining = &remaining[end + 1..];
    }
    variables.sort();
    variables.dedup();
    variables
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_locales_match_and_payload_resolves_all_placeholders() {
        let payload = usage_payload().unwrap();
        let current = payload.current(SessionMode::Once, "test-token");
        assert!(current.contains("account/rateLimits/read"));
        assert!(current.contains("account/rateLimits/updated"));
        assert!(current.contains("__OPSAIL_REFIT_CODEX_STATE__"));
        assert!(current.contains("\"once\""));
        assert!(current.contains("\"test-token\""));
        assert!(!current.contains("__OPSAIL_REFIT_CODEX_MODEL_SOURCE__"));
        let early = payload.early("managed-token");
        assert!(early.contains(&payload.revision));
        assert!(early.contains("\"persistent\""));
    }

    #[test]
    fn renderer_assets_do_not_define_network_or_model_calls() {
        let payload = usage_payload().unwrap();
        let current = payload.current(SessionMode::Once, "test-token");
        for forbidden in [
            "fetch(",
            "XMLHttpRequest",
            "WebSocket(",
            "/v1/",
            "responses.create",
            "chat.completions",
        ] {
            assert!(!current.contains(forbidden), "found {forbidden}");
        }
    }

    #[test]
    fn css_uses_theme_tokens_and_required_compact_affordances() {
        assert!(CSS_SOURCE.contains("font-size: 10px"));
        assert!(CSS_SOURCE.contains("cursor: default"));
        assert!(CSS_SOURCE.contains("prefers-reduced-motion"));
        assert!(CSS_SOURCE.contains("var(--color-token-"));
        for fixed_color in ["rgb(", "rgba(", "hsl(", "hsla("] {
            assert!(!CSS_SOURCE.contains(fixed_color));
        }
    }
}
