use serde::{Deserialize, Serialize};

/// Lifetime policy for an enabled Codex renderer refit.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionMode {
    /// Inject only the current document and close the CDP connection.
    Once,
    /// Keep a foreground CDP manager attached and recover renderer reloads.
    #[default]
    Persistent,
}

impl SessionMode {
    pub const fn lifecycle_name(self) -> &'static str {
        match self {
            Self::Once => "ephemeral",
            Self::Persistent => "managed",
        }
    }
}

/// Whether enable may start a stopped, already-validated ChatGPT application.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LaunchPolicy {
    /// Attach only; never start the application.
    #[default]
    AttachOnly,
    /// Start once only when the application is confirmed stopped.
    LaunchIfStopped,
}

/// Current renderer state for the usage refit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodexRefitState {
    Disabled,
    Enabled,
    Stale,
}

/// Health information for one validated renderer target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexTargetHealth {
    pub target_id: String,
    pub state: CodexRefitState,
    pub healthy: bool,
    pub changed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_mode: Option<SessionMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl CodexTargetHealth {
    pub(crate) fn new(target_id: impl Into<String>, state: CodexRefitState, healthy: bool) -> Self {
        Self {
            target_id: target_id.into(),
            state,
            healthy,
            changed: false,
            session_mode: None,
            detail: None,
        }
    }

    pub(crate) fn with_session_mode(mut self, session_mode: SessionMode) -> Self {
        self.session_mode = Some(session_mode);
        self
    }

    pub(crate) fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// Lifecycle operation represented by a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodexRefitOperation {
    Enable,
    Disable,
    Status,
}

/// Aggregate result for a Codex refit lifecycle operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexRefitReport {
    pub operation: CodexRefitOperation,
    pub port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_mode: Option<SessionMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch_policy: Option<LaunchPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launched: Option<bool>,
    pub targets: Vec<CodexTargetHealth>,
}

/// State of one doctor check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DoctorCheckState {
    Pass,
    Fail,
    Warning,
}

/// One bounded doctor observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorCheck {
    pub name: &'static str,
    pub state: DoctorCheckState,
    pub message: String,
}

/// Read-only diagnostics for the supported Codex target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexDoctorReport {
    pub supported: bool,
    pub ready: bool,
    pub port: u16,
    pub default_session_mode: SessionMode,
    pub detected_session_modes: Vec<SessionMode>,
    pub checks: Vec<DoctorCheck>,
}
