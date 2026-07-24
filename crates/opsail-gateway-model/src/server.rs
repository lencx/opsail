use std::collections::VecDeque;
use std::convert::Infallible;
use std::error::Error;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::{Stream, StreamExt as _, stream};
use http::header::{
    ACCEPT, ALLOW, AUTHORIZATION, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_TYPE, HeaderValue,
};
use http::{HeaderMap, Method, Request, Response, StatusCode};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt as _, Full, LengthLimitError, Limited, StreamBody};
use hyper::body::{Body as _, Frame, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use reqwest::redirect::Policy;
use ring::digest::{Context as DigestContext, SHA256};
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use url::{Host, Url};

use crate::{
    EventMappingProfileV1, GatewayError, MappedSseProjector, ProjectionOutput, ReasoningDisplay,
    ResponsesProjector,
};

pub const DEFAULT_GATEWAY_LISTEN: &str = "127.0.0.1:55322";
pub const DEFAULT_GATEWAY_REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
pub const DEFAULT_GATEWAY_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
pub const DEFAULT_GATEWAY_MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;
pub const DEFAULT_GATEWAY_MAX_CONCURRENT_REQUESTS: usize = 8;
pub const MIN_GATEWAY_REQUEST_BYTES: usize = 1024;
pub const MAX_GATEWAY_REQUEST_BYTES: usize = 256 * 1024 * 1024;
pub const MAX_GATEWAY_CONCURRENT_REQUESTS: usize = 1024;
pub const MAX_GATEWAY_REQUEST_TIMEOUT: Duration = Duration::from_secs(3600);

const RESPONSES_PATH: &str = "/v1/responses";
const MAX_ERROR_MESSAGE_BYTES: usize = 512;
const MAX_PROMPT_CACHE_KEY_BYTES: usize = 4 * 1024;

type BoxError = Box<dyn Error + Send + Sync>;
type GatewayBody = UnsyncBoxBody<Bytes, BoxError>;
type UpstreamStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static>>;
pub type GatewayBearerFuture<'a> =
    Pin<Box<dyn Future<Output = Result<HeaderValue, GatewayError>> + Send + 'a>>;

/// Provider-scoped bearer source. Implementations own caching and refresh;
/// the gateway invokes refresh at most once after an upstream 401.
pub trait GatewayBearerProvider: Send + Sync {
    fn bearer(&self) -> GatewayBearerFuture<'_>;

    fn refresh_after_unauthorized<'a>(&'a self, failed: &'a HeaderValue)
    -> GatewayBearerFuture<'a>;

    fn report_name(&self) -> &'static str;
}

/// How a Codex prompt-cache routing key crosses the third-party boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PromptCacheRouting {
    /// Remove the key. This is safest for unknown Responses-compatible servers.
    #[default]
    Strip,
    /// Replace a valid client key with a stable, provider-scoped SHA-256 key.
    ProviderScoped,
}

/// Fully resolved runtime configuration for one loopback gateway.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub listen: SocketAddr,
    pub upstream: Url,
    pub reasoning_display: ReasoningDisplay,
    pub request_timeout: Duration,
    pub stream_idle_timeout: Duration,
    pub max_request_bytes: usize,
    pub max_concurrent_requests: usize,
    pub prompt_cache_routing: PromptCacheRouting,
    pub event_mapping: Option<EventMappingProfileV1>,
    pub authorization: GatewayAuthorization,
}

#[derive(Clone)]
pub enum GatewayAuthorization {
    /// Send no upstream authorization. Client authorization is always stripped.
    None,
    /// Inject a gateway-owned bearer value after stripping client headers.
    Bearer(HeaderValue),
    /// Resolve and refresh a gateway-owned bearer through a provider-scoped cache.
    BearerProvider(Arc<dyn GatewayBearerProvider>),
}

impl std::fmt::Debug for GatewayAuthorization {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::None => "GatewayAuthorization::None",
            Self::Bearer(_) => "GatewayAuthorization::Bearer([REDACTED])",
            Self::BearerProvider(_) => "GatewayAuthorization::BearerProvider([REDACTED])",
        })
    }
}

impl GatewayAuthorization {
    fn mark_sensitive(&mut self) {
        if let Self::Bearer(value) = self {
            value.set_sensitive(true);
        }
    }

    async fn bearer(&self) -> Result<Option<HeaderValue>, GatewayError> {
        match self {
            Self::None => Ok(None),
            Self::Bearer(value) => Ok(Some(value.clone())),
            Self::BearerProvider(provider) => {
                let mut value = provider.bearer().await?;
                value.set_sensitive(true);
                Ok(Some(value))
            }
        }
    }

    async fn refresh_after_unauthorized(
        &self,
        failed: &HeaderValue,
    ) -> Result<Option<HeaderValue>, GatewayError> {
        let Self::BearerProvider(provider) = self else {
            return Ok(None);
        };
        let mut value = provider.refresh_after_unauthorized(failed).await?;
        value.set_sensitive(true);
        Ok(Some(value))
    }

    fn report_name(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Bearer(_) => "bearer-env",
            Self::BearerProvider(provider) => provider.report_name(),
        }
    }
}

impl GatewayConfig {
    pub fn validate(&self) -> Result<(), GatewayError> {
        if !self.listen.ip().is_loopback() {
            return Err(GatewayError::invalid_config(
                "listen must use a loopback IP address",
            ));
        }
        validate_upstream_url(&self.upstream)?;
        if self.request_timeout.is_zero() || self.request_timeout > MAX_GATEWAY_REQUEST_TIMEOUT {
            return Err(GatewayError::invalid_config(format!(
                "request timeout must be between 1 and {} seconds",
                MAX_GATEWAY_REQUEST_TIMEOUT.as_secs()
            )));
        }
        if self.stream_idle_timeout.is_zero()
            || self.stream_idle_timeout > MAX_GATEWAY_REQUEST_TIMEOUT
        {
            return Err(GatewayError::invalid_config(format!(
                "stream idle timeout must be between 1 and {} seconds",
                MAX_GATEWAY_REQUEST_TIMEOUT.as_secs()
            )));
        }
        if !(MIN_GATEWAY_REQUEST_BYTES..=MAX_GATEWAY_REQUEST_BYTES)
            .contains(&self.max_request_bytes)
        {
            return Err(GatewayError::invalid_config(format!(
                "maximum request size must be between {MIN_GATEWAY_REQUEST_BYTES} and \
                 {MAX_GATEWAY_REQUEST_BYTES} bytes"
            )));
        }
        if self.max_concurrent_requests == 0
            || self.max_concurrent_requests > MAX_GATEWAY_CONCURRENT_REQUESTS
        {
            return Err(GatewayError::invalid_config(format!(
                "maximum concurrency must be between 1 and \
                 {MAX_GATEWAY_CONCURRENT_REQUESTS}"
            )));
        }
        if upstream_socket(&self.upstream).is_some_and(|upstream| {
            self.listen.port() != 0
                && upstream.ip() == self.listen.ip()
                && upstream.port() == self.listen.port()
        }) {
            return Err(GatewayError::invalid_config(
                "upstream resolves to the gateway listener and would create a request loop",
            ));
        }
        if let Some(profile) = &self.event_mapping {
            profile.validate()?;
        }
        Ok(())
    }
}

/// Validate an upstream base URL without resolving DNS or opening a connection.
pub fn validate_upstream_url(upstream: &Url) -> Result<(), GatewayError> {
    if !matches!(upstream.scheme(), "http" | "https") {
        return Err(GatewayError::invalid_config(
            "upstream must use http or https",
        ));
    }
    if upstream.username() != "" || upstream.password().is_some() {
        return Err(GatewayError::invalid_config(
            "upstream must not contain URL credentials",
        ));
    }
    if upstream.query().is_some() || upstream.fragment().is_some() {
        return Err(GatewayError::invalid_config(
            "upstream must not contain a query or fragment",
        ));
    }
    if upstream.path().trim_end_matches('/') != "/v1" {
        return Err(GatewayError::invalid_config("upstream must end with /v1"));
    }
    if upstream.scheme() == "http"
        && !upstream
            .host()
            .and_then(numeric_host_ip)
            .is_some_and(|ip| ip.is_loopback())
    {
        return Err(GatewayError::invalid_config(
            "plaintext upstreams are allowed only on a numeric loopback IP address",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayReport {
    pub listen: SocketAddr,
    pub upstream: Url,
    pub reasoning_display: ReasoningDisplay,
    pub request_timeout_seconds: u64,
    pub stream_idle_timeout_seconds: u64,
    pub max_request_bytes: usize,
    pub max_concurrent_requests: usize,
    pub prompt_cache_routing: PromptCacheRouting,
    pub event_mapping: &'static str,
    pub authorization: &'static str,
}

/// A bound gateway. Binding is separate from running so callers can report the
/// actual listener address before accepting traffic.
pub struct GatewayServer {
    listener: TcpListener,
    state: Arc<GatewayState>,
    report: GatewayReport,
}

#[derive(Debug)]
struct GatewayState {
    client: reqwest::Client,
    responses_url: Url,
    reasoning_display: ReasoningDisplay,
    request_timeout: Duration,
    stream_idle_timeout: Duration,
    max_request_bytes: usize,
    permits: Arc<Semaphore>,
    prompt_cache_routing: PromptCacheRouting,
    prompt_cache_namespace: String,
    event_mapping: Option<EventMappingProfileV1>,
    authorization: GatewayAuthorization,
}

struct ResponseStreamState {
    upstream: UpstreamStream,
    projector: Option<StreamProjector>,
    pending: VecDeque<Bytes>,
    finished: bool,
    stream_idle_timeout: Duration,
    _permit: OwnedSemaphorePermit,
}

enum StreamProjector {
    Responses(ResponsesProjector),
    Mapped(MappedSseProjector),
}

impl StreamProjector {
    fn push(&mut self, chunk: &[u8]) -> Result<ProjectionOutput, GatewayError> {
        match self {
            Self::Responses(projector) => projector.push(chunk),
            Self::Mapped(projector) => projector.push(chunk),
        }
    }

    fn finish(&mut self) -> Result<(), GatewayError> {
        match self {
            Self::Responses(projector) => projector.finish(),
            Self::Mapped(projector) => projector.finish(),
        }
    }

    fn abort(
        &mut self,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<ProjectionOutput, GatewayError> {
        let code = code.into();
        let message = message.into();
        match self {
            Self::Responses(projector) => projector.abort(code, message),
            Self::Mapped(projector) => projector.abort(code, message),
        }
    }
}

impl GatewayServer {
    pub async fn bind(mut config: GatewayConfig) -> Result<Self, GatewayError> {
        config.validate()?;
        config.authorization.mark_sensitive();
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .connect_timeout(config.request_timeout.min(Duration::from_secs(30)))
            .build()
            .map_err(|_| GatewayError::invalid_config("could not create the upstream client"))?;
        let listener = TcpListener::bind(config.listen)
            .await
            .map_err(|error| GatewayError::Bind(bounded_error(&error)))?;
        let listen = listener
            .local_addr()
            .map_err(|error| GatewayError::Bind(bounded_error(&error)))?;
        let responses_url = responses_url(&config.upstream)?;
        let prompt_cache_namespace =
            format!("opsail-gateway-model-v1\0{listen}\0{}", config.upstream);
        let report = GatewayReport {
            listen,
            upstream: config.upstream,
            reasoning_display: config.reasoning_display,
            request_timeout_seconds: config.request_timeout.as_secs(),
            stream_idle_timeout_seconds: config.stream_idle_timeout.as_secs(),
            max_request_bytes: config.max_request_bytes,
            max_concurrent_requests: config.max_concurrent_requests,
            prompt_cache_routing: config.prompt_cache_routing,
            event_mapping: if config.event_mapping.is_some() {
                "custom-v1"
            } else {
                "builtin-responses"
            },
            authorization: config.authorization.report_name(),
        };
        let state = Arc::new(GatewayState {
            client,
            responses_url,
            reasoning_display: config.reasoning_display,
            request_timeout: config.request_timeout,
            stream_idle_timeout: config.stream_idle_timeout,
            max_request_bytes: config.max_request_bytes,
            permits: Arc::new(Semaphore::new(config.max_concurrent_requests)),
            prompt_cache_routing: config.prompt_cache_routing,
            prompt_cache_namespace,
            event_mapping: config.event_mapping,
            authorization: config.authorization,
        });
        Ok(Self {
            listener,
            state,
            report,
        })
    }

    pub fn report(&self) -> &GatewayReport {
        &self.report
    }

    pub async fn run(self) -> Result<(), GatewayError> {
        self.run_until(std::future::pending()).await
    }

    pub async fn run_until<F>(self, shutdown: F) -> Result<(), GatewayError>
    where
        F: Future<Output = ()> + Send,
    {
        let mut connections = JoinSet::new();
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                biased;
                () = &mut shutdown => break,
                accepted = self.listener.accept() => {
                    let (stream, _) = accepted
                        .map_err(|error| GatewayError::Listener(bounded_error(&error)))?;
                    spawn_connection(&mut connections, stream, Arc::clone(&self.state));
                }
                completed = connections.join_next(), if !connections.is_empty() => {
                    if let Some(Err(error)) = completed {
                        tracing::debug!(
                            error = %bounded_error(&error),
                            "model gateway connection task stopped"
                        );
                    }
                }
            }
        }
        connections.shutdown().await;
        Ok(())
    }
}

fn spawn_connection(connections: &mut JoinSet<()>, stream: TcpStream, state: Arc<GatewayState>) {
    connections.spawn(async move {
        let _ = stream.set_nodelay(true);
        let service = service_fn(move |request| {
            let state = Arc::clone(&state);
            async move { Ok::<_, Infallible>(route(request, state).await) }
        });
        if let Err(error) = http1::Builder::new()
            .serve_connection(TokioIo::new(stream), service)
            .await
        {
            tracing::debug!(
                error = %bounded_error(&error),
                "model gateway client connection closed"
            );
        }
    });
}

async fn route(request: Request<Incoming>, state: Arc<GatewayState>) -> Response<GatewayBody> {
    let path = request
        .uri()
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or_default();
    if path == "/healthz" {
        if request.method() == Method::GET {
            return json_response(
                StatusCode::OK,
                json!({
                    "status": "ok",
                    "service": "opsail-gateway-model",
                    "schemaVersion": 1,
                }),
            );
        }
        return method_not_allowed("GET");
    }
    if path != RESPONSES_PATH {
        return error_response(StatusCode::NOT_FOUND, "not_found", "route not found");
    }
    if request.method() != Method::POST {
        return method_not_allowed("POST");
    }

    let permit = match Arc::clone(&state.permits).try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "capacity_exceeded",
                "model gateway request capacity is exhausted",
            );
        }
    };
    proxy_responses(request, state, permit).await
}

async fn proxy_responses(
    request: Request<Incoming>,
    state: Arc<GatewayState>,
    permit: OwnedSemaphorePermit,
) -> Response<GatewayBody> {
    if request
        .headers()
        .get(CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| !value.eq_ignore_ascii_case("identity"))
    {
        return error_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "request_content_encoding_rejected",
            "encoded model request bodies are not accepted",
        );
    }
    if request
        .body()
        .size_hint()
        .upper()
        .is_some_and(|size| size > state.max_request_bytes as u64)
    {
        return error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request_too_large",
            "request body exceeds the configured limit",
        );
    }

    let (_, body) = request.into_parts();
    let body = match Limited::new(body, state.max_request_bytes).collect().await {
        Ok(body) => body.to_bytes(),
        Err(error) if error.downcast_ref::<LengthLimitError>().is_some() => {
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_too_large",
                "request body exceeds the configured limit",
            );
        }
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_body",
                "request body could not be read",
            );
        }
    };
    let body = match prepare_request_body_for_gateway(
        body,
        state.prompt_cache_routing,
        &state.prompt_cache_namespace,
    ) {
        Ok(body) => body,
        Err((code, message)) => {
            return error_response(StatusCode::BAD_REQUEST, code, message);
        }
    };

    let upstream = match tokio::time::timeout(
        state.request_timeout,
        send_with_authorization_retry(&state, body),
    )
    .await
    {
        Ok(Ok(response)) => response,
        Ok(Err(_)) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                "upstream_unavailable",
                "upstream model request failed",
            );
        }
        Err(_) => {
            return error_response(
                StatusCode::GATEWAY_TIMEOUT,
                "upstream_timeout",
                "upstream model request timed out before response headers",
            );
        }
    };

    let status = upstream.status();
    if !status.is_success() {
        return sanitized_upstream_error(status);
    }
    let is_sse = is_event_stream(upstream.headers());
    let headers = downstream_response_headers(is_sse);
    let projector = if is_sse {
        match &state.event_mapping {
            Some(profile) => {
                match MappedSseProjector::new(profile.clone(), state.reasoning_display) {
                    Ok(projector) => Some(StreamProjector::Mapped(projector)),
                    Err(_) => {
                        return error_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "invalid_event_mapping",
                            "configured event mapping could not be initialized",
                        );
                    }
                }
            }
            None => Some(StreamProjector::Responses(ResponsesProjector::new(
                state.reasoning_display,
            ))),
        }
    } else {
        None
    };
    let stream_state = ResponseStreamState {
        upstream: Box::pin(upstream.bytes_stream()),
        projector,
        pending: VecDeque::new(),
        finished: false,
        stream_idle_timeout: state.stream_idle_timeout,
        _permit: permit,
    };
    let stream = stream::unfold(stream_state, next_response_frame);
    let body = StreamBody::new(stream).boxed_unsync();
    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

async fn send_with_authorization_retry(
    state: &GatewayState,
    body: Bytes,
) -> Result<reqwest::Response, GatewayError> {
    let authorization = state.authorization.bearer().await?;
    let response = send_upstream(state, body.clone(), authorization.as_ref()).await?;
    if response.status() != StatusCode::UNAUTHORIZED {
        return Ok(response);
    }
    let Some(failed) = authorization.as_ref() else {
        return Ok(response);
    };
    let Some(refreshed) = state
        .authorization
        .refresh_after_unauthorized(failed)
        .await?
    else {
        return Ok(response);
    };
    drop(response);
    send_upstream(state, body, Some(&refreshed)).await
}

async fn send_upstream(
    state: &GatewayState,
    body: Bytes,
    authorization: Option<&HeaderValue>,
) -> Result<reqwest::Response, GatewayError> {
    let mut headers = outbound_request_headers();
    if let Some(value) = authorization {
        headers.insert(AUTHORIZATION, value.clone());
    }
    state
        .client
        .request(Method::POST, state.responses_url.clone())
        .headers(headers)
        .body(body)
        .send()
        .await
        .map_err(|_| GatewayError::Upstream("upstream model request failed".to_owned()))
}

fn sanitized_upstream_error(status: StatusCode) -> Response<GatewayBody> {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => error_response(
            StatusCode::BAD_GATEWAY,
            "upstream_authentication_failed",
            "upstream model authentication failed",
        ),
        StatusCode::TOO_MANY_REQUESTS => error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "upstream_rate_limited",
            "upstream model request was rate limited",
        ),
        status if status.is_client_error() => error_response(
            StatusCode::BAD_REQUEST,
            "upstream_rejected_request",
            "upstream model rejected the request",
        ),
        _ => error_response(
            StatusCode::BAD_GATEWAY,
            "upstream_failed",
            "upstream model request failed",
        ),
    }
}

async fn next_response_frame(
    mut state: ResponseStreamState,
) -> Option<(Result<Frame<Bytes>, BoxError>, ResponseStreamState)> {
    loop {
        if let Some(chunk) = state.pending.pop_front() {
            return Some((Ok(Frame::data(chunk)), state));
        }
        if state.finished {
            return None;
        }
        match tokio::time::timeout(state.stream_idle_timeout, state.upstream.next()).await {
            Ok(Some(Ok(chunk))) => {
                let Some(projector) = state.projector.as_mut() else {
                    return Some((Ok(Frame::data(chunk)), state));
                };
                match projector.push(&chunk) {
                    Ok(output) => {
                        if !output.events.is_empty() {
                            tracing::trace!(
                                count = output.events.len(),
                                "observed canonical model events"
                            );
                        }
                        state.pending.extend(output.chunks);
                    }
                    Err(error) => {
                        return terminal_stream_error(
                            state,
                            "invalid_upstream_stream",
                            "upstream returned an invalid Responses stream",
                            Some(error),
                        );
                    }
                }
            }
            Ok(Some(Err(_))) => {
                return terminal_stream_error(
                    state,
                    "upstream_stream_error",
                    "upstream response stream failed",
                    None,
                );
            }
            Ok(None) => {
                state.finished = true;
                if let Some(projector) = state.projector.as_mut()
                    && let Err(error) = projector.finish()
                {
                    return terminal_stream_error(
                        state,
                        "incomplete_upstream_stream",
                        "upstream response stream ended before completion",
                        Some(error),
                    );
                }
            }
            Err(_) => {
                return terminal_stream_error(
                    state,
                    "stream_idle_timeout",
                    "upstream response stream became idle",
                    None,
                );
            }
        }
    }
}

fn terminal_stream_error(
    mut state: ResponseStreamState,
    code: &'static str,
    message: &'static str,
    source: Option<GatewayError>,
) -> Option<(Result<Frame<Bytes>, BoxError>, ResponseStreamState)> {
    state.finished = true;
    let Some(projector) = state.projector.as_mut() else {
        let error: BoxError =
            Box::new(source.unwrap_or_else(|| GatewayError::Upstream(message.to_owned())));
        return Some((Err(error), state));
    };
    match projector.abort(code, message) {
        Ok(output) => {
            state.pending.extend(output.chunks);
            state
                .pending
                .pop_front()
                .map(|chunk| (Ok(Frame::data(chunk)), state))
        }
        Err(error) => Some((Err(Box::new(error)), state)),
    }
}

fn responses_url(upstream: &Url) -> Result<Url, GatewayError> {
    let mut url = upstream.clone();
    url.set_path(RESPONSES_PATH);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn upstream_socket(upstream: &Url) -> Option<SocketAddr> {
    Some(SocketAddr::new(
        upstream.host().and_then(numeric_host_ip)?,
        upstream.port_or_known_default()?,
    ))
}

fn numeric_host_ip(host: Host<&str>) -> Option<IpAddr> {
    match host {
        Host::Ipv4(ip) => Some(IpAddr::V4(ip)),
        Host::Ipv6(ip) => Some(IpAddr::V6(ip)),
        Host::Domain(_) => None,
    }
}

fn is_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
}

fn outbound_request_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers
}

fn downstream_response_headers(is_sse: bool) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static(if is_sse {
            "text/event-stream"
        } else {
            "application/json"
        }),
    );
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers
}

fn prepare_request_body_for_gateway(
    body: Bytes,
    prompt_cache_routing: PromptCacheRouting,
    prompt_cache_namespace: &str,
) -> Result<Bytes, (&'static str, &'static str)> {
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return Err((
            "invalid_json_request",
            "model gateway requests must contain one JSON object",
        ));
    };
    let Some(object) = value.as_object() else {
        return Err((
            "invalid_json_request",
            "model gateway requests must contain one JSON object",
        ));
    };
    if object.keys().any(|field| is_credential_field(field)) {
        return Err((
            "credential_field_rejected",
            "top-level credential fields are not accepted by the model gateway",
        ));
    }
    if object
        .keys()
        .any(|field| !is_supported_responses_request_field(field))
    {
        return Err((
            "unsupported_request_field",
            "model gateway request contains an unsupported top-level field",
        ));
    }
    if has_invalid_request_shape(object) {
        return Err((
            "invalid_request_control",
            "model gateway request contains an invalid protocol field",
        ));
    }
    if has_tool_transport_credentials(object) {
        return Err((
            "credential_field_rejected",
            "tool transport credentials are not accepted by the model gateway",
        ));
    }
    if !sanitize_provider_private_fields(&mut value, prompt_cache_routing, prompt_cache_namespace) {
        return Ok(body);
    }
    serde_json::to_vec(&value).map(Bytes::from).map_err(|_| {
        (
            "invalid_json_request",
            "model gateway request could not be normalized",
        )
    })
}

#[cfg(test)]
fn prepare_request_body(body: Bytes) -> Result<Bytes, (&'static str, &'static str)> {
    prepare_request_body_for_gateway(body, PromptCacheRouting::Strip, "")
}

fn is_credential_field(field: &str) -> bool {
    if field.len() > 128 {
        return false;
    }
    let normalized = field
        .bytes()
        .filter(|byte| byte.is_ascii_alphanumeric())
        .map(|byte| byte.to_ascii_lowercase())
        .collect::<Vec<_>>();
    matches!(
        normalized.as_slice(),
        b"apikey"
            | b"accesstoken"
            | b"authorization"
            | b"auth"
            | b"authentication"
            | b"bearer"
            | b"bearertoken"
            | b"clientsecret"
            | b"cookie"
            | b"cookies"
            | b"credential"
            | b"credentials"
            | b"headers"
            | b"idtoken"
            | b"oauthtoken"
            | b"password"
            | b"proxyauthorization"
            | b"refreshtoken"
            | b"secret"
            | b"session"
            | b"sessionid"
            | b"sessiontoken"
            | b"setcookie"
            | b"token"
    )
}

fn is_supported_responses_request_field(field: &str) -> bool {
    matches!(
        field,
        "background"
            | "client_metadata"
            | "conversation"
            | "include"
            | "input"
            | "instructions"
            | "max_output_tokens"
            | "max_tool_calls"
            | "metadata"
            | "model"
            | "parallel_tool_calls"
            | "previous_response_id"
            | "prompt"
            | "prompt_cache_key"
            | "prompt_cache_retention"
            | "reasoning"
            | "safety_identifier"
            | "service_tier"
            | "store"
            | "stream"
            | "stream_options"
            | "temperature"
            | "text"
            | "tool_choice"
            | "tools"
            | "top_logprobs"
            | "top_p"
            | "truncation"
            | "user"
    )
}

fn has_invalid_request_shape(root: &serde_json::Map<String, serde_json::Value>) -> bool {
    value_has_wrong_type(root.get("model"), |value| value.is_string())
        || value_has_wrong_type(root.get("instructions"), |value| value.is_string())
        || value_has_wrong_type(root.get("input"), |value| {
            value.is_string() || value.is_array()
        })
        || value_has_wrong_type(root.get("tools"), serde_json::Value::is_array)
        || value_has_wrong_type(root.get("include"), |value| {
            value
                .as_array()
                .is_some_and(|items| items.iter().all(serde_json::Value::is_string))
        })
        || ["parallel_tool_calls", "store", "stream"]
            .iter()
            .any(|field| value_has_wrong_type(root.get(*field), serde_json::Value::is_boolean))
        || ["max_output_tokens", "max_tool_calls", "top_logprobs"]
            .iter()
            .any(|field| value_has_wrong_type(root.get(*field), serde_json::Value::is_u64))
        || ["temperature", "top_p"]
            .iter()
            .any(|field| value_has_wrong_type(root.get(*field), serde_json::Value::is_number))
        || value_has_wrong_type(root.get("truncation"), serde_json::Value::is_string)
        || object_has_disallowed_fields(root.get("reasoning"), &["context", "effort", "summary"])
        || object_has_disallowed_fields(
            root.get("stream_options"),
            &[
                "include_obfuscation",
                "include_usage",
                "reasoning_summary_delivery",
            ],
        )
        || object_has_disallowed_fields(root.get("text"), &["format", "verbosity"])
        || root
            .get("text")
            .and_then(|text| text.get("format"))
            .is_some_and(|format| {
                object_has_disallowed_fields(Some(format), &["name", "schema", "strict", "type"])
            })
        || root.get("tool_choice").is_some_and(|choice| match choice {
            serde_json::Value::Null | serde_json::Value::String(_) => false,
            serde_json::Value::Object(choice) => choice
                .keys()
                .any(|field| !matches!(field.as_str(), "mode" | "name" | "type")),
            _ => true,
        })
}

fn value_has_wrong_type(
    value: Option<&serde_json::Value>,
    predicate: impl FnOnce(&serde_json::Value) -> bool,
) -> bool {
    value.is_some_and(|value| !value.is_null() && !predicate(value))
}

fn object_has_disallowed_fields(value: Option<&serde_json::Value>, allowed: &[&str]) -> bool {
    value.is_some_and(|value| match value {
        serde_json::Value::Null => false,
        serde_json::Value::Object(object) => object
            .keys()
            .any(|field| !allowed.contains(&field.as_str())),
        _ => true,
    })
}

fn has_tool_transport_credentials(root: &serde_json::Map<String, serde_json::Value>) -> bool {
    root.get("tools")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|tools| {
            tools.iter().any(|tool| {
                tool.as_object()
                    .is_some_and(|tool| tool.keys().any(|field| is_credential_field(field)))
            })
        })
}

fn sanitize_provider_private_fields(
    value: &mut serde_json::Value,
    prompt_cache_routing: PromptCacheRouting,
    prompt_cache_namespace: &str,
) -> bool {
    let Some(root) = value.as_object_mut() else {
        return false;
    };
    let provider_cache_key = match prompt_cache_routing {
        PromptCacheRouting::Strip => None,
        PromptCacheRouting::ProviderScoped => root
            .get("prompt_cache_key")
            .and_then(serde_json::Value::as_str)
            .filter(|key| {
                !key.is_empty()
                    && key.len() <= MAX_PROMPT_CACHE_KEY_BYTES
                    && key.trim() == *key
                    && !key.chars().any(char::is_control)
            })
            .map(|key| provider_scoped_prompt_cache_key(prompt_cache_namespace, key)),
    };
    let mut changed = false;
    for field in [
        "background",
        "client_metadata",
        "conversation",
        "metadata",
        "previous_response_id",
        "prompt",
        "prompt_cache_key",
        "prompt_cache_retention",
        "safety_identifier",
        "service_tier",
        "user",
    ] {
        changed |= root.remove(field).is_some();
    }
    if let Some(cache_key) = provider_cache_key {
        root.insert(
            "prompt_cache_key".to_owned(),
            serde_json::Value::String(cache_key),
        );
        changed = true;
    }
    if root.get("store").and_then(serde_json::Value::as_bool) != Some(false) {
        root.insert("store".to_owned(), serde_json::Value::Bool(false));
        changed = true;
    }
    if let Some(include) = root
        .get_mut("include")
        .and_then(serde_json::Value::as_array_mut)
    {
        let previous_len = include.len();
        include.retain(|value| value.as_str() != Some("reasoning.encrypted_content"));
        changed |= include.len() != previous_len;
    }
    if let Some(input) = root
        .get_mut("input")
        .and_then(serde_json::Value::as_array_mut)
    {
        input.retain_mut(|item| {
            let Some(item) = item.as_object_mut() else {
                return true;
            };
            let item_type = item
                .get("type")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            if matches!(
                item_type.as_deref(),
                Some(
                    "additional_tools"
                        | "compaction"
                        | "compaction_summary"
                        | "compaction_trigger"
                        | "context_compaction"
                        | "tool_search_call"
                        | "tool_search_output"
                )
            ) {
                changed = true;
                return false;
            }
            changed |= item.remove("id").is_some();
            changed |= item.remove("encrypted_content").is_some();
            changed |= item.remove("signature").is_some();
            changed |= item
                .remove("internal_chat_message_metadata_passthrough")
                .is_some();
            changed |= item.remove("provider_metadata").is_some();
            match item_type.as_deref() {
                Some("reasoning") => {
                    changed |= item.remove("content").is_some();
                }
                Some("agent_message") => {
                    if let Some(content) = item
                        .get_mut("content")
                        .and_then(serde_json::Value::as_array_mut)
                    {
                        let previous_len = content.len();
                        content.retain(|part| {
                            part.get("type").and_then(serde_json::Value::as_str)
                                != Some("encrypted_content")
                        });
                        changed |= content.len() != previous_len;
                    }
                }
                _ => {}
            }
            true
        });
    }
    changed
}

fn provider_scoped_prompt_cache_key(namespace: &str, client_key: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut context = DigestContext::new(&SHA256);
    context.update(namespace.as_bytes());
    context.update(b"\0");
    context.update(client_key.as_bytes());
    let digest = context.finish();
    let mut encoded = String::with_capacity("opsail-v1-".len() + digest.as_ref().len() * 2);
    encoded.push_str("opsail-v1-");
    for byte in digest.as_ref() {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn method_not_allowed(allowed: &'static str) -> Response<GatewayBody> {
    let mut response = error_response(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "HTTP method is not allowed for this route",
    );
    response
        .headers_mut()
        .insert(ALLOW, allowed.parse().expect("static Allow value is valid"));
    response
}

fn error_response(
    status: StatusCode,
    code: &'static str,
    message: &'static str,
) -> Response<GatewayBody> {
    json_response(
        status,
        json!({
            "error": {
                "type": "opsail_gateway_error",
                "code": code,
                "message": message,
            }
        }),
    )
}

fn json_response(status: StatusCode, value: serde_json::Value) -> Response<GatewayBody> {
    let bytes = serde_json::to_vec(&value)
        .map(Bytes::from)
        .unwrap_or_else(|_| Bytes::from_static(b"{\"error\":{\"type\":\"opsail_gateway_error\"}}"));
    let body = Full::new(bytes)
        .map_err(|error: Infallible| -> BoxError { match error {} })
        .boxed_unsync();
    let mut response = Response::new(body);
    *response.status_mut() = status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        "application/json".parse().expect("valid MIME"),
    );
    response.headers_mut().insert(
        CACHE_CONTROL,
        "no-store".parse().expect("valid cache policy"),
    );
    response
}

fn bounded_error(error: &impl std::fmt::Display) -> String {
    error
        .to_string()
        .chars()
        .take(MAX_ERROR_MESSAGE_BYTES)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct RotatingBearer {
        refreshed: AtomicBool,
        refresh_count: AtomicUsize,
    }

    impl GatewayBearerProvider for RotatingBearer {
        fn bearer(&self) -> GatewayBearerFuture<'_> {
            Box::pin(async move {
                Ok(if self.refreshed.load(Ordering::SeqCst) {
                    HeaderValue::from_static("Bearer refreshed-key")
                } else {
                    HeaderValue::from_static("Bearer expired-key")
                })
            })
        }

        fn refresh_after_unauthorized<'a>(
            &'a self,
            _failed: &'a HeaderValue,
        ) -> GatewayBearerFuture<'a> {
            Box::pin(async move {
                self.refresh_count.fetch_add(1, Ordering::SeqCst);
                self.refreshed.store(true, Ordering::SeqCst);
                Ok(HeaderValue::from_static("Bearer refreshed-key"))
            })
        }

        fn report_name(&self) -> &'static str {
            "test-rotating-bearer"
        }
    }

    fn config(upstream: Url, reasoning_display: ReasoningDisplay) -> GatewayConfig {
        GatewayConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            upstream,
            reasoning_display,
            request_timeout: Duration::from_secs(5),
            stream_idle_timeout: Duration::from_secs(5),
            max_request_bytes: MIN_GATEWAY_REQUEST_BYTES,
            max_concurrent_requests: 2,
            prompt_cache_routing: PromptCacheRouting::Strip,
            event_mapping: None,
            authorization: GatewayAuthorization::None,
        }
    }

    #[test]
    fn config_rejects_remote_plaintext_and_self_loops() {
        let remote = config(
            Url::parse("http://example.com/v1").unwrap(),
            ReasoningDisplay::Strict,
        );
        assert!(remote.validate().is_err());

        let mut looping = config(
            Url::parse("http://127.0.0.1:55322/v1").unwrap(),
            ReasoningDisplay::Strict,
        );
        looping.listen = "127.0.0.1:55322".parse().unwrap();
        assert!(looping.validate().is_err());
    }

    #[test]
    fn request_envelope_rejects_transport_credential_channels() {
        assert!(
            prepare_request_body(Bytes::from_static(
                br#"{"model":"m","authorization":"secret"}"#
            ))
            .is_err()
        );
        assert!(
            prepare_request_body(Bytes::from_static(br#"{"model":"m","API-KEY":"secret"}"#))
                .is_err()
        );
        assert!(
            prepare_request_body(Bytes::from_static(
                br#"{"model":"m","headers":{"x-api-key":"secret"}}"#
            ))
            .is_err()
        );
        assert!(
            prepare_request_body(Bytes::from_static(
                br#"{"model":"m","oauth_token":"secret"}"#
            ))
            .is_err()
        );
        assert!(
            prepare_request_body(Bytes::from_static(
                br#"{"model":"m","tools":[{"type":"mcp","authorization":"secret"}]}"#
            ))
            .is_err()
        );
        assert!(
            prepare_request_body(Bytes::from_static(
                br#"{"model":"m","extra":{"access_token":"secret"}}"#
            ))
            .is_err(),
            "unknown request control fields cannot become credential carriers"
        );
        assert!(
            prepare_request_body(Bytes::from_static(
                br#"{"model":"m","reasoning":{"effort":"high","token":"secret"}}"#
            ))
            .is_err(),
            "known control objects reject unknown nested fields"
        );
        assert!(
            prepare_request_body(Bytes::from_static(
                br#"{"model":"m","temperature":{"token":"secret"}}"#
            ))
            .is_err(),
            "typed protocol fields cannot become arbitrary credential carriers"
        );
        assert!(prepare_request_body(Bytes::from_static(br#"not-json"#)).is_err());
        assert!(
            prepare_request_body(Bytes::from_static(
                br#"{"model":"m","input":[{"role":"user","content":"explain api_key fields"}]}"#
            ))
            .is_ok(),
            "ordinary prompt content must not be inspected or rewritten"
        );
        assert!(
            prepare_request_body(Bytes::from_static(
                br#"{
                    "model":"m",
                    "max_output_tokens":100,
                    "reasoning":{"effort":"high","summary":"auto"},
                    "text":{
                        "format":{
                            "type":"json_schema",
                            "name":"result",
                            "strict":true,
                            "schema":{
                                "type":"object",
                                "properties":{"access_token":{"type":"string"}}
                            }
                        }
                    },
                    "tools":[{
                        "type":"function",
                        "name":"inspect",
                        "parameters":{
                            "type":"object",
                            "properties":{"authorization":{"type":"string"}}
                        }
                    }]
                }"#
            ))
            .is_ok(),
            "function schemas are semantic model input, not transport credential channels"
        );
    }

    #[test]
    fn request_normalization_removes_only_provider_private_state() {
        let prepared = prepare_request_body(Bytes::from_static(
            br#"{
                "model":"third-party",
                "background":true,
                "client_metadata":{"thread_id":"official-thread"},
                "conversation":"official-conversation",
                "metadata":{"account":"private"},
                "previous_response_id":"official-response",
                "prompt":{"id":"official-prompt"},
                "prompt_cache_key":"official-cache",
                "prompt_cache_retention":"24h",
                "safety_identifier":"official-safety",
                "service_tier":"official-tier",
                "store":true,
                "user":"official-user",
                "include":["reasoning.encrypted_content","other"],
                "input":[
                    {
                        "id":"official-reasoning-id",
                        "type":"reasoning",
                        "summary":[{"type":"summary_text","text":"safe summary"}],
                        "content":[{"type":"reasoning_text","text":"private raw reasoning"}],
                        "encrypted_content":"opaque-official-state",
                        "signature":"official-signature",
                        "internal_chat_message_metadata_passthrough":{"turn_id":"private"}
                    },
                    {
                        "id":"official-message-id",
                        "type":"message",
                        "role":"user",
                        "content":[{"type":"input_text","text":"keep this"}],
                        "internal_chat_message_metadata_passthrough":{"turn_id":"private"}
                    },
                    {
                        "type":"agent_message",
                        "content":[
                            {"type":"encrypted_content","encrypted_content":"private"},
                            {"type":"input_text","text":"visible agent text"}
                        ]
                    },
                    {
                        "id":"official-compaction-id",
                        "type":"compaction",
                        "encrypted_content":"official-compaction-state"
                    },
                    {
                        "id":"official-tool-search-id",
                        "type":"tool_search_output",
                        "tools":[{"name":"official-dynamic-tool"}]
                    }
                ]
            }"#,
        ))
        .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&prepared).unwrap();
        for field in [
            "client_metadata",
            "conversation",
            "background",
            "metadata",
            "previous_response_id",
            "prompt",
            "prompt_cache_key",
            "prompt_cache_retention",
            "safety_identifier",
            "service_tier",
            "user",
        ] {
            assert!(value.get(field).is_none(), "{field}");
        }
        assert_eq!(value["store"], false);
        assert_eq!(value["include"], json!(["other"]));
        assert!(
            value["input"][0].get("encrypted_content").is_none()
                && value["input"][0].get("signature").is_none()
                && value["input"][0].get("content").is_none()
                && value["input"][0].get("id").is_none()
                && value["input"][0]
                    .get("internal_chat_message_metadata_passthrough")
                    .is_none()
        );
        assert_eq!(value["input"][0]["summary"][0]["text"], "safe summary");
        assert!(
            value["input"][1]
                .get("internal_chat_message_metadata_passthrough")
                .is_none()
        );
        assert!(value["input"][1].get("id").is_none());
        assert_eq!(value["input"][1]["content"][0]["text"], "keep this");
        assert_eq!(value["input"][2]["content"].as_array().unwrap().len(), 1);
        assert_eq!(
            value["input"][2]["content"][0]["text"],
            "visible agent text"
        );
        assert_eq!(value["input"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn provider_scoped_cache_keys_are_stable_but_never_forwarded_raw() {
        let body = Bytes::from_static(
            br#"{
                "model":"third-party",
                "prompt_cache_key":"official-session-key"
            }"#,
        );
        let first = prepare_request_body_for_gateway(
            body.clone(),
            PromptCacheRouting::ProviderScoped,
            "gateway-a",
        )
        .unwrap();
        let repeated = prepare_request_body_for_gateway(
            body.clone(),
            PromptCacheRouting::ProviderScoped,
            "gateway-a",
        )
        .unwrap();
        let other_provider =
            prepare_request_body_for_gateway(body, PromptCacheRouting::ProviderScoped, "gateway-b")
                .unwrap();

        let key = |body: &Bytes| {
            serde_json::from_slice::<serde_json::Value>(body).unwrap()["prompt_cache_key"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        assert_eq!(key(&first), key(&repeated));
        assert_ne!(key(&first), key(&other_provider));
        assert!(!String::from_utf8_lossy(&first).contains("official-session-key"));
    }

    #[tokio::test]
    async fn gateway_injects_own_auth_and_projects_reasoning_summary() {
        let upstream = MockServer::start().await;
        let sse = concat!(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-1\",\"model\":\"third-party\"}}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"reasoning-1\",\"type\":\"reasoning\",\"summary\":[]}}\n\n",
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"reasoning-1\",\"output_index\":0,\"summary_index\":0,\"delta\":\"Visible summary\"}\n\n",
            "data: {\"type\":\"response.reasoning_summary_text.done\",\"item_id\":\"reasoning-1\",\"output_index\":0,\"summary_index\":0,\"text\":\"Visible summary\"}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"reasoning-1\",\"type\":\"reasoning\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"Visible summary\"}]}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-1\",\"model\":\"third-party\"}}\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_raw(sse, "text/event-stream"),
            )
            .mount(&upstream)
            .await;

        let upstream_url = Url::parse(&format!("{}/v1", upstream.uri())).unwrap();
        let mut gateway_config = config(upstream_url, ReasoningDisplay::Commentary);
        gateway_config.authorization =
            GatewayAuthorization::Bearer(HeaderValue::from_static("Bearer test-key"));
        let server = GatewayServer::bind(gateway_config).await.unwrap();
        let listen = server.report().listen;
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(server.run_until(async move {
            let _ = shutdown_rx.await;
        }));

        let response = reqwest::Client::new()
            .post(format!("http://{listen}{RESPONSES_PATH}"))
            .header("authorization", "Bearer test-key")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.text().await.unwrap();
        assert!(body.contains(r#""phase":"commentary""#));
        assert!(body.contains("Visible summary"));
        assert!(!body.contains(r#""type":"reasoning""#));

        let _ = shutdown_tx.send(());
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn gateway_strips_client_identity_headers_by_default() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream; token=upstream-secret")
                    .insert_header("cache-control", "private, x-secret=upstream-secret")
                    .insert_header("set-cookie", "upstream=session")
                    .insert_header("www-authenticate", "Bearer secret-metadata")
                    .insert_header("x-api-key", "upstream-secret")
                    .set_body_raw(
                        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-1\"}}\n\n",
                        "text/event-stream",
                    ),
            )
            .mount(&upstream)
            .await;

        let upstream_url = Url::parse(&format!("{}/v1", upstream.uri())).unwrap();
        let mut gateway_config = config(upstream_url, ReasoningDisplay::Strict);
        gateway_config.authorization = GatewayAuthorization::None;
        gateway_config.max_request_bytes = 8 * 1024;
        let server = GatewayServer::bind(gateway_config).await.unwrap();
        let listen = server.report().listen;
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(server.run_until(async move {
            let _ = shutdown_rx.await;
        }));

        let response = reqwest::Client::new()
            .post(format!("http://{listen}{RESPONSES_PATH}"))
            .header("accept", "text/event-stream; token=client-secret")
            .header("content-type", "application/json; token=client-secret")
            .header("authorization", "Bearer chatgpt-oauth")
            .header("cookie", "session=chatgpt")
            .header("chatgpt-account-id", "acct-user")
            .header("openai-organization", "org-user")
            .header("openai-project", "project-user")
            .header("x-api-key", "provider-key-from-client")
            .header("x-auth-token", "client-session-token")
            .header("x-unrelated-secret", "must-not-cross-boundary")
            .body(
                r#"{
                    "model":"third-party",
                    "client_metadata":{"thread_id":"official-thread"},
                    "prompt_cache_key":"official-cache",
                    "service_tier":"official-tier",
                    "store":true,
                    "include":["reasoning.encrypted_content"],
                    "input":[
                        {
                            "id":"official-reasoning-id",
                            "type":"reasoning",
                            "summary":[{"type":"summary_text","text":"safe summary"}],
                            "content":[{"type":"reasoning_text","text":"private raw reasoning"}],
                            "encrypted_content":"opaque-official-state"
                        },
                        {
                            "id":"safe-message-id",
                            "type":"message",
                            "role":"user",
                            "content":[{"type":"input_text","text":"keep this"}]
                        },
                        {
                            "type":"compaction",
                            "encrypted_content":"official-compaction-state"
                        }
                    ]
                }"#,
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );
        assert_eq!(
            response
                .headers()
                .get(CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        for name in ["set-cookie", "www-authenticate", "x-api-key"] {
            assert!(
                !response.headers().contains_key(name),
                "{name} leaked from the upstream response"
            );
        }
        let _ = response.text().await.unwrap();

        let requests = upstream.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]
                .headers
                .get("accept")
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );
        assert_eq!(
            requests[0]
                .headers
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        for name in [
            "authorization",
            "cookie",
            "chatgpt-account-id",
            "openai-organization",
            "openai-project",
            "x-api-key",
            "x-auth-token",
            "x-unrelated-secret",
        ] {
            assert!(
                !requests[0].headers.contains_key(name),
                "{name} leaked to the upstream"
            );
        }
        let outbound: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(outbound["model"], "third-party");
        assert_eq!(outbound["store"], false);
        assert_eq!(outbound["input"][0]["summary"][0]["text"], "safe summary");
        assert_eq!(outbound["input"][1]["content"][0]["text"], "keep this");
        assert!(outbound["include"].as_array().unwrap().is_empty());
        assert_eq!(outbound["input"].as_array().unwrap().len(), 2);
        let outbound_bytes = String::from_utf8_lossy(&requests[0].body);
        for private_value in [
            "official-thread",
            "official-cache",
            "official-tier",
            "official-reasoning-id",
            "private raw reasoning",
            "opaque-official-state",
            "safe-message-id",
            "official-compaction-state",
        ] {
            assert!(
                !outbound_bytes.contains(private_value),
                "{private_value} leaked in the upstream request body"
            );
        }

        let _ = shutdown_tx.send(());
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn gateway_replaces_client_auth_with_gateway_owned_bearer() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(header("authorization", "Bearer gateway-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_raw(
                        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-1\"}}\n\n",
                        "text/event-stream",
                    ),
            )
            .mount(&upstream)
            .await;

        let upstream_url = Url::parse(&format!("{}/v1", upstream.uri())).unwrap();
        let mut gateway_config = config(upstream_url, ReasoningDisplay::Strict);
        gateway_config.authorization =
            GatewayAuthorization::Bearer(HeaderValue::from_static("Bearer gateway-key"));
        let server = GatewayServer::bind(gateway_config).await.unwrap();
        assert_eq!(server.report().authorization, "bearer-env");
        assert!(!format!("{:?}", server.state.authorization).contains("gateway-key"));
        assert!(
            matches!(
                &server.state.authorization,
                GatewayAuthorization::Bearer(value) if value.is_sensitive()
            ),
            "the HTTP authorization value must remain marked sensitive"
        );
        let listen = server.report().listen;
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(server.run_until(async move {
            let _ = shutdown_rx.await;
        }));

        let response = reqwest::Client::new()
            .post(format!("http://{listen}{RESPONSES_PATH}"))
            .header("authorization", "Bearer chatgpt-oauth")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let _ = response.text().await.unwrap();

        let _ = shutdown_tx.send(());
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn dynamic_bearer_refreshes_once_after_401_and_retries_the_same_body() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(header("authorization", "Bearer expired-key"))
            .respond_with(
                ResponseTemplate::new(401)
                    .insert_header("www-authenticate", "Bearer upstream-secret")
                    .set_body_string("expired upstream-secret"),
            )
            .expect(1)
            .mount(&upstream)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(header("authorization", "Bearer refreshed-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_raw(
                        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-1\"}}\n\n",
                        "text/event-stream",
                    ),
            )
            .expect(1)
            .mount(&upstream)
            .await;

        let provider = Arc::new(RotatingBearer {
            refreshed: AtomicBool::new(false),
            refresh_count: AtomicUsize::new(0),
        });
        let upstream_url = Url::parse(&format!("{}/v1", upstream.uri())).unwrap();
        let mut gateway_config = config(upstream_url, ReasoningDisplay::Strict);
        gateway_config.authorization = GatewayAuthorization::BearerProvider(provider.clone());
        gateway_config.prompt_cache_routing = PromptCacheRouting::ProviderScoped;
        let server = GatewayServer::bind(gateway_config).await.unwrap();
        assert_eq!(server.report().authorization, "test-rotating-bearer");
        let listen = server.report().listen;
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(server.run_until(async move {
            let _ = shutdown_rx.await;
        }));

        let request_body = r#"{
            "model":"third-party",
            "input":"stable-prefix",
            "prompt_cache_key":"official-session-key"
        }"#;
        let response = reqwest::Client::new()
            .post(format!("http://{listen}{RESPONSES_PATH}"))
            .header("authorization", "Bearer chatgpt-oauth")
            .body(request_body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!response.text().await.unwrap().contains("upstream-secret"));
        assert_eq!(provider.refresh_count.load(Ordering::SeqCst), 1);

        let requests = upstream.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].body, requests[1].body);
        let retried_body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(retried_body["input"], "stable-prefix");
        assert_eq!(retried_body["model"], "third-party");
        assert_eq!(retried_body["store"], false);
        let cache_key = retried_body["prompt_cache_key"].as_str().unwrap();
        assert!(cache_key.starts_with("opsail-v1-"));
        assert_eq!(cache_key.len(), "opsail-v1-".len() + 64);
        assert!(!cache_key.contains("official-session-key"));
        assert!(
            requests
                .iter()
                .all(|request| !String::from_utf8_lossy(&request.body).contains("chatgpt-oauth"))
        );

        let _ = shutdown_tx.send(());
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn static_bearer_does_not_retry_or_expose_upstream_401_details() {
        let upstream = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(header("authorization", "Bearer static-key"))
            .respond_with(
                ResponseTemplate::new(401)
                    .insert_header("www-authenticate", "Bearer private-realm")
                    .set_body_string("provider-token-private-detail"),
            )
            .expect(1)
            .mount(&upstream)
            .await;

        let upstream_url = Url::parse(&format!("{}/v1", upstream.uri())).unwrap();
        let mut gateway_config = config(upstream_url, ReasoningDisplay::Strict);
        gateway_config.authorization =
            GatewayAuthorization::Bearer(HeaderValue::from_static("Bearer static-key"));
        let server = GatewayServer::bind(gateway_config).await.unwrap();
        let listen = server.report().listen;
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(server.run_until(async move {
            let _ = shutdown_rx.await;
        }));

        let response = reqwest::Client::new()
            .post(format!("http://{listen}{RESPONSES_PATH}"))
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(!response.headers().contains_key("www-authenticate"));
        let body = response.text().await.unwrap();
        assert!(body.contains("upstream_authentication_failed"));
        assert!(!body.contains("provider-token-private-detail"));
        assert!(!body.contains("private-realm"));

        let _ = shutdown_tx.send(());
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn gateway_rejects_body_credential_channels_before_upstream() {
        let upstream = MockServer::start().await;
        let upstream_url = Url::parse(&format!("{}/v1", upstream.uri())).unwrap();
        let server = GatewayServer::bind(config(upstream_url, ReasoningDisplay::Strict))
            .await
            .unwrap();
        let listen = server.report().listen;
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(server.run_until(async move {
            let _ = shutdown_rx.await;
        }));

        let client = reqwest::Client::new();
        for (payload, expected_code) in [
            (
                r#"{"model":"third-party","access_token":"must-not-cross-boundary"}"#,
                "credential_field_rejected",
            ),
            (
                r#"{"model":"third-party","extra":{"token":"must-not-cross-boundary"}}"#,
                "unsupported_request_field",
            ),
            (
                r#"{"model":"third-party","tools":[{"type":"mcp","headers":{"authorization":"must-not-cross-boundary"}}]}"#,
                "credential_field_rejected",
            ),
            (
                r#"{"model":"third-party","temperature":{"token":"must-not-cross-boundary"}}"#,
                "invalid_request_control",
            ),
        ] {
            let response = client
                .post(format!("http://{listen}{RESPONSES_PATH}"))
                .header(CONTENT_TYPE, "application/json")
                .body(payload)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let body = response.text().await.unwrap();
            assert!(body.contains(expected_code));
            assert!(!body.contains("must-not-cross-boundary"));
        }
        assert!(upstream.received_requests().await.unwrap().is_empty());

        let _ = shutdown_tx.send(());
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn gateway_maps_unknown_json_sse_into_codex_responses_events() {
        let upstream = MockServer::start().await;
        let sse = concat!(
            "data: {\"kind\":\"start\",\"run\":\"resp-custom\",\"model\":\"future-model\"}\n\n",
            "data: {\"kind\":\"thought\",\"item\":\"reason-1\",\"index\":0,\"text\":\"Checking\"}\n\n",
            "data: {\"kind\":\"thought-done\",\"item\":\"reason-1\",\"index\":0,\"text\":\"Checking inputs\"}\n\n",
            "data: {\"kind\":\"answer\",\"item\":\"msg-1\",\"text\":\"Done\"}\n\n",
            "data: {\"kind\":\"usage\",\"input\":4,\"output\":2,\"total\":6}\n\n",
            "data: {\"kind\":\"done\",\"run\":\"resp-custom\",\"model\":\"future-model\"}\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_raw(sse, "text/event-stream"),
            )
            .mount(&upstream)
            .await;

        let profile = EventMappingProfileV1::from_toml(
            r#"
version = 1
discriminator = "/kind"

[[rules]]
match = "start"
emit = "run-started"
[rules.fields.run_id]
pointer = "/run"
[rules.fields.model]
pointer = "/model"

[[rules]]
match = "thought"
emit = "reasoning-summary-delta"
[rules.fields.item_id]
pointer = "/item"
[rules.fields.summary_index]
pointer = "/index"
[rules.fields.delta]
pointer = "/text"

[[rules]]
match = "thought-done"
emit = "reasoning-summary-completed"
[rules.fields.item_id]
pointer = "/item"
[rules.fields.summary_index]
pointer = "/index"
[rules.fields.text]
pointer = "/text"

[[rules]]
match = "answer"
emit = "assistant-text-delta"
[rules.fields.item_id]
pointer = "/item"
[rules.fields.phase]
value = "final_answer"
[rules.fields.delta]
pointer = "/text"

[[rules]]
match = "usage"
emit = "usage-updated"
[rules.fields.input_tokens]
pointer = "/input"
[rules.fields.output_tokens]
pointer = "/output"
[rules.fields.total_tokens]
pointer = "/total"

[[rules]]
match = "done"
emit = "run-completed"
[rules.fields.response_id]
pointer = "/run"
[rules.fields.model]
pointer = "/model"
"#,
        )
        .unwrap();
        let upstream_url = Url::parse(&format!("{}/v1", upstream.uri())).unwrap();
        let mut gateway_config = config(upstream_url, ReasoningDisplay::Commentary);
        gateway_config.event_mapping = Some(profile);
        gateway_config.authorization = GatewayAuthorization::None;
        let server = GatewayServer::bind(gateway_config).await.unwrap();
        let listen = server.report().listen;
        assert_eq!(server.report().event_mapping, "custom-v1");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(server.run_until(async move {
            let _ = shutdown_rx.await;
        }));

        let response = reqwest::Client::new()
            .post(format!("http://{listen}{RESPONSES_PATH}"))
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_ENCODING),
            None,
            "a reserialized stream must not retain upstream content encoding"
        );
        let body = response.text().await.unwrap();
        assert!(body.contains("response.created"));
        assert!(body.contains(r#""phase":"commentary""#));
        assert!(body.contains("Checking inputs"));
        assert!(body.contains(r#""phase":"final_answer""#));
        assert!(body.contains(r#""total_tokens":6"#));
        assert!(body.contains("response.completed"));
        assert!(!body.contains(r#""kind":"answer""#));

        let _ = shutdown_tx.send(());
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn gateway_rejects_oversized_request_before_upstream() {
        let upstream = MockServer::start().await;
        let upstream_url = Url::parse(&format!("{}/v1", upstream.uri())).unwrap();
        let server = GatewayServer::bind(config(upstream_url, ReasoningDisplay::Strict))
            .await
            .unwrap();
        let listen = server.report().listen;
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(server.run_until(async move {
            let _ = shutdown_rx.await;
        }));

        let response = reqwest::Client::new()
            .post(format!("http://{listen}{RESPONSES_PATH}"))
            .body(vec![0_u8; MIN_GATEWAY_REQUEST_BYTES + 1])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let requests = upstream.received_requests().await.unwrap();
        assert!(requests.is_empty());

        let _ = shutdown_tx.send(());
        task.await.unwrap().unwrap();
    }
}
