use std::sync::OnceLock;

use serde_json::{Value, json};

use crate::error::{CodexRefitError, CodexRefitErrorCode};
use crate::model::SessionMode;

const MODEL_SOURCE: &str = include_str!("../assets/opsail-refit-codex-usage-model.js");
const DOM_ADAPTER_SOURCE: &str = include_str!("../assets/opsail-refit-codex-dom-adapter.js");
const RENDERER_PROBE_TEMPLATE: &str =
    include_str!("../assets/opsail-refit-codex-renderer-probe.js");
const EARLY_TEMPLATE: &str = include_str!("../assets/opsail-refit-codex-usage-early.js");
const RUNTIME_TEMPLATE: &str = include_str!("../assets/opsail-refit-codex-usage-runtime.js");
const STATUS_TEMPLATE: &str = include_str!("../assets/opsail-refit-codex-usage-status.js");
const DISABLE_SOURCE: &str = include_str!("../assets/opsail-refit-codex-usage-disable.js");
const CSS_SOURCE: &str = include_str!("../assets/opsail-refit-codex-usage.css");
const EN_LOCALE: &str = include_str!("../assets/locales/en.json");
const ZH_CN_LOCALE: &str = include_str!("../assets/locales/zh-CN.json");

const MODEL_PLACEHOLDER: &str = "__OPSAIL_REFIT_CODEX_MODEL_SOURCE__";
const DOM_ADAPTER_PLACEHOLDER: &str = "__OPSAIL_REFIT_CODEX_DOM_ADAPTER_SOURCE__";
const VERSION_PLACEHOLDER: &str = "__OPSAIL_REFIT_CODEX_VERSION_JSON__";
const REVISION_PLACEHOLDER: &str = "__OPSAIL_REFIT_CODEX_REVISION_JSON__";
const CSS_PLACEHOLDER: &str = "__OPSAIL_REFIT_CODEX_CSS_JSON__";
const LOCALES_PLACEHOLDER: &str = "__OPSAIL_REFIT_CODEX_LOCALES_JSON__";
const SESSION_MODE_PLACEHOLDER: &str = "__OPSAIL_REFIT_CODEX_SESSION_MODE_JSON__";
const MANAGER_TOKEN_PLACEHOLDER: &str = "__OPSAIL_REFIT_CODEX_MANAGER_TOKEN_JSON__";
const EARLY_REVISION_PLACEHOLDER: &str = "__OPSAIL_REFIT_CODEX_EARLY_REVISION_JSON__";
const CURRENT_PAYLOAD_PLACEHOLDER: &str = "__OPSAIL_REFIT_CODEX_CURRENT_PAYLOAD__";
const STATUS_REVISION_PLACEHOLDER: &str = "__OPSAIL_REFIT_CODEX_STATUS_REVISION_JSON__";

#[derive(Clone)]
pub(crate) struct UsagePayload {
    current_template: String,
    early_template: String,
    renderer_probe: String,
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
        self.early_template
            .replace(EARLY_REVISION_PLACEHOLDER, &json_string(&self.revision))
            .replace(
                CURRENT_PAYLOAD_PLACEHOLDER,
                &self.current(SessionMode::Persistent, manager_token),
            )
    }

    pub fn renderer_probe(&self) -> &str {
        &self.renderer_probe
    }

    pub fn status(&self) -> String {
        STATUS_TEMPLATE.replace(STATUS_REVISION_PLACEHOLDER, &json_string(&self.revision))
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

pub(crate) fn disable_expression() -> &'static str {
    DISABLE_SOURCE
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
        .replace(DOM_ADAPTER_PLACEHOLDER, DOM_ADAPTER_SOURCE)
        .replace(VERSION_PLACEHOLDER, &json_string(env!("CARGO_PKG_VERSION")))
        .replace(REVISION_PLACEHOLDER, &json_string(&revision))
        .replace(CSS_PLACEHOLDER, &json_string(CSS_SOURCE))
        .replace(LOCALES_PLACEHOLDER, &locales.to_string());
    for placeholder in [
        MODEL_PLACEHOLDER,
        DOM_ADAPTER_PLACEHOLDER,
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
    let renderer_probe =
        RENDERER_PROBE_TEMPLATE.replace(DOM_ADAPTER_PLACEHOLDER, DOM_ADAPTER_SOURCE);
    if renderer_probe.contains(DOM_ADAPTER_PLACEHOLDER) {
        return Err("renderer probe contains an unresolved DOM adapter placeholder".to_owned());
    }
    let early_template = EARLY_TEMPLATE.replace(DOM_ADAPTER_PLACEHOLDER, DOM_ADAPTER_SOURCE);
    if early_template.contains(DOM_ADAPTER_PLACEHOLDER)
        || !early_template.contains(EARLY_REVISION_PLACEHOLDER)
        || !early_template.contains(CURRENT_PAYLOAD_PLACEHOLDER)
    {
        return Err("early renderer payload has invalid placeholders".to_owned());
    }
    if !STATUS_TEMPLATE.contains(STATUS_REVISION_PLACEHOLDER) {
        return Err("renderer status payload is missing its revision placeholder".to_owned());
    }
    Ok(UsagePayload {
        current_template,
        early_template,
        renderer_probe,
        revision,
    })
}

fn payload_revision() -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in MODEL_SOURCE
        .bytes()
        .chain(DOM_ADAPTER_SOURCE.bytes())
        .chain(RENDERER_PROBE_TEMPLATE.bytes())
        .chain(EARLY_TEMPLATE.bytes())
        .chain(RUNTIME_TEMPLATE.bytes())
        .chain(STATUS_TEMPLATE.bytes())
        .chain(DISABLE_SOURCE.bytes())
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
        assert!(current.contains("createOpsailRefitCodexDomAdapter"));
        assert!(current.contains("\"once\""));
        assert!(current.contains("\"test-token\""));
        assert!(!current.contains("__OPSAIL_REFIT_CODEX_MODEL_SOURCE__"));
        assert!(!current.contains(DOM_ADAPTER_PLACEHOLDER));
        let early = payload.early("managed-token");
        assert!(early.contains(&payload.revision));
        assert!(early.contains("\"persistent\""));
        assert!(early.contains("createOpsailRefitCodexDomAdapter"));
        assert!(!early.contains(EARLY_REVISION_PLACEHOLDER));
        assert!(!early.contains(CURRENT_PAYLOAD_PLACEHOLDER));
        assert!(
            payload
                .renderer_probe()
                .contains("createOpsailRefitCodexDomAdapter")
        );
        assert!(!payload.renderer_probe().contains(DOM_ADAPTER_PLACEHOLDER));
        let status = payload.status();
        assert!(status.contains(&payload.revision));
        assert!(!status.contains(STATUS_REVISION_PLACEHOLDER));
        assert!(disable_expression().contains("__OPSAIL_REFIT_CODEX_DISABLED__"));
    }

    #[test]
    fn native_dom_knowledge_is_centralized_in_the_adapter_asset() {
        assert!(DOM_ADAPTER_SOURCE.contains("const SELECTORS"));
        assert!(DOM_ADAPTER_SOURCE.contains("measureNativeLayout"));
        assert!(DOM_ADAPTER_SOURCE.contains("nodeMayAffectLayout"));
        assert!(DOM_ADAPTER_SOURCE.contains("const VERSION = 1"));
        for source in [
            MODEL_SOURCE,
            RUNTIME_TEMPLATE,
            RENDERER_PROBE_TEMPLATE,
            EARLY_TEMPLATE,
            STATUS_TEMPLATE,
            DISABLE_SOURCE,
        ] {
            for native_marker in [
                ["app", "shell", "left", "panel"].join("-"),
                ["main", "main-surface"].join("."),
            ] {
                assert!(!source.contains(&native_marker));
            }
        }
    }

    #[test]
    fn renderer_assets_do_not_define_network_or_model_calls() {
        let payload = usage_payload().unwrap();
        let sources = [
            payload.current(SessionMode::Once, "test-token"),
            payload.early("test-token"),
            payload.renderer_probe().to_owned(),
            payload.status(),
            disable_expression().to_owned(),
        ];
        for forbidden in [
            "fetch(",
            "XMLHttpRequest",
            "WebSocket(",
            "eval(",
            "new Function",
            "/v1/",
            "responses.create",
            "chat.completions",
        ] {
            for source in &sources {
                assert!(!source.contains(forbidden), "found {forbidden}");
            }
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
