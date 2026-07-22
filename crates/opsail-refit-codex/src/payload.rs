#[cfg(test)]
use std::sync::OnceLock;

use serde_json::Value;

use crate::error::{CodexRefitError, CodexRefitErrorCode};
#[cfg(test)]
use crate::model::RendererAssetSource;
use crate::model::{RendererAssetInfo, SessionMode};
#[cfg(test)]
use crate::renderer_assets::embedded_bundle;
use crate::renderer_assets::{
    CONTROL_FILE, DOM_ADAPTER_FILE, MODEL_FILE, RUNTIME_FILE, RendererSources,
};

const CSS_SOURCE: &str = include_str!("../assets/opsail-refit-codex-usage.css");
const LOCALES_SOURCE: &str = include_str!("../assets/locales.json");

const MODEL_PLACEHOLDER: &str = "__OPSAIL_REFIT_CODEX_MODEL_SOURCE__";
const DOM_ADAPTER_PLACEHOLDER: &str = "__OPSAIL_REFIT_CODEX_DOM_ADAPTER_SOURCE__";
const OPERATION_PLACEHOLDER: &str = "__OPSAIL_REFIT_CODEX_OPERATION_JSON__";
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
    status_template: String,
    disable_source: String,
    launch_notice_source: String,
    pub asset_info: RendererAssetInfo,
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
        self.status_template
            .replace(STATUS_REVISION_PLACEHOLDER, &json_string(&self.revision))
    }

    pub fn disable(&self) -> &str {
        &self.disable_source
    }

    pub fn launch_notice(&self) -> &str {
        &self.launch_notice_source
    }
}

#[cfg(test)]
pub(crate) fn usage_payload() -> Result<UsagePayload, CodexRefitError> {
    static PAYLOAD: OnceLock<Result<UsagePayload, String>> = OnceLock::new();
    PAYLOAD
        .get_or_init(|| {
            let bundle = embedded_bundle().map_err(|error| error.to_string())?;
            build_payload(bundle.sources(), bundle.info(RendererAssetSource::Embedded))
        })
        .as_ref()
        .cloned()
        .map_err(|message| CodexRefitError::new(CodexRefitErrorCode::InjectionFailed, message))
}

pub(crate) fn build_usage_payload(
    sources: &RendererSources,
    asset_info: RendererAssetInfo,
) -> Result<UsagePayload, CodexRefitError> {
    build_payload(sources, asset_info)
        .map_err(|message| CodexRefitError::new(CodexRefitErrorCode::InjectionFailed, message))
}

fn build_payload(
    sources: &RendererSources,
    asset_info: RendererAssetInfo,
) -> Result<UsagePayload, String> {
    let model_source = source(sources, MODEL_FILE)?;
    let dom_adapter_source = source(sources, DOM_ADAPTER_FILE)?;
    let control_template = source(sources, CONTROL_FILE)?;
    let runtime_template = source(sources, RUNTIME_FILE)?;
    let locales: Value = serde_json::from_str(LOCALES_SOURCE)
        .map_err(|_| "embedded locale bundle JSON is invalid".to_owned())?;
    validate_locale_bundle(&locales)?;
    let revision = payload_revision(sources, &asset_info.version);
    let current_template = runtime_template
        .replace(MODEL_PLACEHOLDER, model_source)
        .replace(DOM_ADAPTER_PLACEHOLDER, dom_adapter_source)
        .replace(VERSION_PLACEHOLDER, &json_string(&asset_info.version))
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
    for required in [
        "__OPSAIL_REFIT_CODEX_STATE__",
        "account/rateLimits/read",
        "account/rateLimits/updated",
        "rateLimitResetCredits",
        "createOpsailRefitCodexDomAdapter",
    ] {
        if !current_template.contains(required) {
            return Err("renderer payload is missing a required local contract".to_owned());
        }
    }
    if !dom_adapter_source.contains("const VERSION = 1") {
        return Err("renderer DOM adapter uses an unsupported API version".to_owned());
    }
    let renderer_probe = render_control(
        control_template,
        "probe",
        dom_adapter_source,
        "null",
        "void 0",
        "null",
    )?;
    let early_template = render_control(
        control_template,
        "early",
        dom_adapter_source,
        EARLY_REVISION_PLACEHOLDER,
        CURRENT_PAYLOAD_PLACEHOLDER,
        "null",
    )?;
    if !early_template.contains(EARLY_REVISION_PLACEHOLDER)
        || !early_template.contains(CURRENT_PAYLOAD_PLACEHOLDER)
    {
        return Err("early renderer payload has invalid placeholders".to_owned());
    }
    let status_template = render_control(
        control_template,
        "status",
        "",
        "null",
        "void 0",
        STATUS_REVISION_PLACEHOLDER,
    )?;
    if !status_template.contains(STATUS_REVISION_PLACEHOLDER) {
        return Err("renderer status payload is missing its revision placeholder".to_owned());
    }
    let disable_source = render_control(control_template, "disable", "", "null", "void 0", "null")?;
    let launch_notice_source = render_control(
        control_template,
        "launch-notice",
        "",
        "null",
        "void 0",
        "null",
    )?;
    if !disable_source.contains("__OPSAIL_REFIT_CODEX_DISABLED__") {
        return Err("renderer cleanup payload is missing its cleanup marker".to_owned());
    }
    for marker in [
        "__OPSAIL_REFIT_CODEX_STATE__",
        "opsail-refit-codex-usage",
        "opsail-refit-codex-usage-details",
        "opsail-refit-codex-usage-style",
    ] {
        if !current_template.contains(marker)
            || !status_template.contains(marker)
            || !disable_source.contains(marker)
        {
            return Err("renderer assets disagree about their cleanup contract".to_owned());
        }
    }
    Ok(UsagePayload {
        current_template,
        early_template,
        renderer_probe,
        status_template,
        disable_source,
        launch_notice_source,
        asset_info,
        revision,
    })
}

fn render_control(
    template: &str,
    operation: &str,
    dom_adapter: &str,
    early_revision: &str,
    current_payload: &str,
    status_revision: &str,
) -> Result<String, String> {
    let rendered = template
        .replace(OPERATION_PLACEHOLDER, &json_string(operation))
        .replace(DOM_ADAPTER_PLACEHOLDER, dom_adapter)
        .replace(EARLY_REVISION_PLACEHOLDER, early_revision)
        .replace(CURRENT_PAYLOAD_PLACEHOLDER, current_payload)
        .replace(STATUS_REVISION_PLACEHOLDER, status_revision);
    for placeholder in [
        OPERATION_PLACEHOLDER,
        DOM_ADAPTER_PLACEHOLDER,
        EARLY_REVISION_PLACEHOLDER,
        CURRENT_PAYLOAD_PLACEHOLDER,
        STATUS_REVISION_PLACEHOLDER,
    ] {
        if rendered.contains(placeholder)
            && placeholder != early_revision
            && placeholder != current_payload
            && placeholder != status_revision
        {
            return Err("renderer control payload contains an unresolved placeholder".to_owned());
        }
    }
    Ok(rendered)
}

fn source<'a>(sources: &'a RendererSources, name: &str) -> Result<&'a str, String> {
    sources.get(name).map_err(|error| error.to_string())
}

fn payload_revision(sources: &RendererSources, asset_version: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    let bytes = sources
        .iter()
        .flat_map(|(_, source)| source.bytes())
        .chain(CSS_SOURCE.bytes())
        .chain(LOCALES_SOURCE.bytes());
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{asset_version}-{hash:016x}")
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("serializing an in-memory string cannot fail")
}

fn validate_locale_bundle(bundle: &Value) -> Result<(), String> {
    let root = bundle
        .as_object()
        .ok_or_else(|| "embedded locale bundle must be an object".to_owned())?;
    let default_locale = root
        .get("defaultLocale")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "embedded locale bundle has no default locale".to_owned())?;
    let supported = root
        .get("supportedLocales")
        .and_then(Value::as_array)
        .ok_or_else(|| "embedded locale bundle has no supported locale list".to_owned())?;
    let mut supported_names = Vec::with_capacity(supported.len());
    for locale in supported {
        let locale = locale
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "embedded supported locale is invalid".to_owned())?;
        if supported_names.contains(&locale) {
            return Err("embedded supported locale list contains a duplicate".to_owned());
        }
        supported_names.push(locale);
    }
    let locales = root
        .get("locales")
        .and_then(Value::as_object)
        .ok_or_else(|| "embedded locale bundle has no locale messages".to_owned())?;
    let default_messages = locales
        .get(default_locale)
        .ok_or_else(|| "embedded default locale messages are missing".to_owned())?;
    let mut messages = Vec::new();
    collect_messages(default_messages, "", &mut messages)?;
    for (locale, override_messages) in locales {
        if !supported_names.contains(&locale.as_str()) {
            return Err("embedded locale messages use an unsupported locale".to_owned());
        }
        validate_locale_override(default_messages, override_messages, "")?;
    }
    Ok(())
}

fn validate_locale_override(
    default: &Value,
    override_value: &Value,
    path: &str,
) -> Result<(), String> {
    match (default, override_value) {
        (Value::Object(default), Value::Object(override_object)) => {
            for (key, value) in override_object {
                let default_value = default
                    .get(key)
                    .ok_or_else(|| "embedded locale override contains an unknown key".to_owned())?;
                let next_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                validate_locale_override(default_value, value, &next_path)?;
            }
            Ok(())
        }
        (Value::String(default), Value::String(override_value)) => {
            if message_variables(default) != message_variables(override_value) {
                return Err(format!(
                    "embedded locale override changes message variables at {path}"
                ));
            }
            Ok(())
        }
        _ => Err("embedded locale override changes a message value type".to_owned()),
    }
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
        assert!(
            payload
                .disable()
                .contains("__OPSAIL_REFIT_CODEX_DISABLED__")
        );
        assert!(payload.launch_notice().contains("launch-notice"));
        assert!(payload.launch_notice().contains("showLaunchNotice"));
    }

    #[test]
    fn native_dom_knowledge_is_centralized_in_the_adapter_asset() {
        let bundle = embedded_bundle().unwrap();
        let sources = bundle.sources();
        let dom_adapter = sources.get(DOM_ADAPTER_FILE).unwrap();
        assert!(dom_adapter.contains("const SELECTORS"));
        assert!(dom_adapter.contains("measureNativeLayout"));
        assert!(dom_adapter.contains("nodeMayAffectLayout"));
        assert!(dom_adapter.contains("const VERSION = 1"));
        for source in [
            sources.get(MODEL_FILE).unwrap(),
            sources.get(RUNTIME_FILE).unwrap(),
            sources.get(CONTROL_FILE).unwrap(),
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
            payload.disable().to_owned(),
            payload.launch_notice().to_owned(),
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
