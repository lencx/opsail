use serde::Serialize;
use thiserror::Error;

/// Stable diagnostic categories returned by the Codex refit adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodexRefitErrorCode {
    Unsupported,
    TargetNotFound,
    TargetValidationFailed,
    BridgeUnavailable,
    SessionUnavailable,
    RestartRequired,
    PortUnavailable,
    LaunchFailed,
    InjectionFailed,
    CleanupFailed,
    UpdateFailed,
    Stale,
    StateIo,
}

impl CodexRefitErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::TargetNotFound => "target-not-found",
            Self::TargetValidationFailed => "target-validation-failed",
            Self::BridgeUnavailable => "bridge-unavailable",
            Self::SessionUnavailable => "session-unavailable",
            Self::RestartRequired => "restart-required",
            Self::PortUnavailable => "port-unavailable",
            Self::LaunchFailed => "launch-failed",
            Self::InjectionFailed => "injection-failed",
            Self::CleanupFailed => "cleanup-failed",
            Self::UpdateFailed => "update-failed",
            Self::Stale => "stale",
            Self::StateIo => "state-io",
        }
    }
}

/// A bounded error that never retains renderer payloads or credentials.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct CodexRefitError {
    code: CodexRefitErrorCode,
    message: String,
}

impl CodexRefitError {
    pub(crate) fn new(code: CodexRefitErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> CodexRefitErrorCode {
        self.code
    }
}
