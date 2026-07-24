//! Errors exposed by the model gateway.

use thiserror::Error;

/// Bounded gateway failures. Messages never include request bodies or headers.
#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("invalid gateway configuration: {0}")]
    InvalidConfig(String),
    #[error("failed to bind the model gateway: {0}")]
    Bind(String),
    #[error("model gateway listener failed: {0}")]
    Listener(String),
    #[error("upstream model request failed: {0}")]
    Upstream(String),
    #[error("invalid event mapping: {0}")]
    InvalidMapping(String),
    #[error("invalid Responses SSE stream: {0}")]
    Protocol(String),
}

impl GatewayError {
    pub(crate) fn invalid_config(message: impl Into<String>) -> Self {
        Self::InvalidConfig(message.into())
    }

    pub(crate) fn protocol(message: impl Into<String>) -> Self {
        Self::Protocol(message.into())
    }

    pub(crate) fn invalid_mapping(message: impl Into<String>) -> Self {
        Self::InvalidMapping(message.into())
    }
}
