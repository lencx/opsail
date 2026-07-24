//! Opsail's model gateway for bounded loopback protocol translation.
//!
//! Provider traffic follows either a validated native Responses path or:
//! provider JSON-SSE -> OpsailEvent v1 -> Codex Responses projection.
//! User config and secret lookup remain outside this crate; resolved upstream
//! authorization is gateway-owned and client credentials are never forwarded.

mod error;
mod event;
mod mapped;
mod mapping;
mod responses;
mod server;
mod sse;

pub use error::GatewayError;
pub use event::{MessagePhase, OPSAIL_EVENT_SCHEMA_VERSION, OpsailEventKind, OpsailEventV1};
pub use mapped::{CanonicalResponsesProjector, MappedSseProjector};
pub use mapping::{
    EVENT_MAPPING_SCHEMA_VERSION, EventMapper, EventMappingProfileV1, EventMappingRuleV1,
    MappedEventFieldV1, MappedEventTypeV1, MappedValueSourceV1, MappingInputV1,
};
pub use responses::{ProjectionOutput, ReasoningDisplay, ResponsesProjector};
pub use server::{
    DEFAULT_GATEWAY_LISTEN, DEFAULT_GATEWAY_MAX_CONCURRENT_REQUESTS,
    DEFAULT_GATEWAY_MAX_REQUEST_BYTES, DEFAULT_GATEWAY_REQUEST_TIMEOUT,
    DEFAULT_GATEWAY_STREAM_IDLE_TIMEOUT, GatewayAuthorization, GatewayBearerFuture,
    GatewayBearerProvider, GatewayConfig, GatewayReport, GatewayServer,
    MAX_GATEWAY_CONCURRENT_REQUESTS, MAX_GATEWAY_REQUEST_BYTES, MAX_GATEWAY_REQUEST_TIMEOUT,
    MIN_GATEWAY_REQUEST_BYTES, PromptCacheRouting, validate_upstream_url,
};
