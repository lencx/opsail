//! Cross-platform Chrome lifecycle management and CDP page capture.

mod cdp;
mod launcher;
mod rendered;

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;
use tokio::time::Instant;
use url::Url;

pub use rendered::{RenderedPageEvidence, RenderedProbe, RenderedProbeResult, RenderedSurface};

pub const DEFAULT_MAX_BYTES: usize = 5 * 1024 * 1024;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const BROWSER_CLOSE_TIMEOUT: Duration = Duration::from_millis(750);

/// Browser lifecycle event to await after CDP navigation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CdpWaitUntil {
    None,
    DomContentLoaded,
    #[default]
    Load,
    NetworkIdle,
}

/// A caller-managed Chrome DevTools Protocol source.
#[derive(Clone)]
pub struct CdpSource {
    /// A Chrome discovery URL, browser/page WebSocket URL, or local port.
    pub endpoint: String,
    /// Navigate to this URL before capture. When omitted, capture an existing page.
    pub url: Option<Url>,
    /// Capture or navigate an existing target instead of selecting/creating one.
    pub target_id: Option<String>,
    /// Treat the endpoint as a page-scoped provider WebSocket.
    pub direct_page: bool,
    pub wait_until: CdpWaitUntil,
}

impl CdpSource {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            url: None,
            target_id: None,
            direct_page: false,
            wait_until: CdpWaitUntil::default(),
        }
    }
}

impl fmt::Debug for CdpSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CdpSource")
            .field("endpoint", &"<redacted>")
            .field("has_url", &self.url.is_some())
            .field("has_target_id", &self.target_id.is_some())
            .field("direct_page", &self.direct_page)
            .field("wait_until", &self.wait_until)
            .finish()
    }
}

/// A page acquired through an Opsail-owned local Chrome process.
#[derive(Clone)]
pub struct ChromeSource {
    pub url: Url,
    /// Explicit Chrome/Chromium executable. Automatic discovery is used when omitted.
    pub executable_path: Option<PathBuf>,
    pub wait_until: CdpWaitUntil,
}

impl ChromeSource {
    pub fn new(url: Url) -> Self {
        Self {
            url,
            executable_path: None,
            wait_until: CdpWaitUntil::default(),
        }
    }
}

impl fmt::Debug for ChromeSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChromeSource")
            .field("url", &"<redacted>")
            .field("has_executable_path", &self.executable_path.is_some())
            .field("wait_until", &self.wait_until)
            .finish()
    }
}

/// Limits and navigation profile shared by borrowed and launched Chrome capture.
#[derive(Debug, Clone)]
pub struct CaptureOptions {
    /// End-to-end acquisition deadline. Bounded target/browser cleanup may run afterward.
    pub timeout: Duration,
    /// Maximum time for discovery HTTP requests and each initial WebSocket connection.
    pub connect_timeout: Duration,
    /// Maximum captured HTML size in bytes.
    pub max_bytes: usize,
    /// Optional browser User-Agent override applied before navigation.
    pub user_agent: Option<String>,
    /// Optional browser Accept-Language override applied before navigation.
    pub accept_language: Option<String>,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            max_bytes: DEFAULT_MAX_BYTES,
            user_agent: None,
            accept_language: None,
        }
    }
}

/// Rendered page data captured from Chrome.
#[derive(Clone)]
pub struct CapturedPage {
    pub html: String,
    pub final_url: Url,
    response: Option<CapturedResponse>,
    rendered_evidence: Option<RenderedPageEvidence>,
}

impl CapturedPage {
    /// Metadata retained from the top-level document response, when Opsail
    /// navigated the page and the endpoint exposed the CDP Network domain.
    pub fn response(&self) -> Option<&CapturedResponse> {
        self.response.as_ref()
    }

    /// Compact live-layout evidence requested by a `*_with_probes` capture.
    pub fn rendered_evidence(&self) -> Option<&RenderedPageEvidence> {
        self.rendered_evidence.as_ref()
    }
}

impl fmt::Debug for CapturedPage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CapturedPage")
            .field("html_bytes", &self.html.len())
            .field("final_url", &"<redacted>")
            .field("has_response", &self.response.is_some())
            .field("has_rendered_evidence", &self.rendered_evidence.is_some())
            .finish()
    }
}

/// A privacy-bounded view of the top-level document response.
///
/// Opsail retains the status and normalized indicators derived from a small
/// response-header allowlist. Raw values, cookies, authorization data, and
/// arbitrary response headers are never stored here.
#[derive(Clone)]
pub struct CapturedResponse {
    status: u16,
    headers: CapturedResponseHeaders,
}

impl CapturedResponse {
    pub fn status(&self) -> u16 {
        self.status
    }

    /// Return a normalized indicator by its case-insensitive header name.
    ///
    /// Only exact recognized values for `cf-mitigated` and
    /// `x-amzn-waf-action` are represented; this never returns raw header data.
    pub fn header(&self, name: &str) -> Option<&str> {
        if name.eq_ignore_ascii_case("cf-mitigated") {
            self.headers.cf_mitigated_challenge.then_some("challenge")
        } else if name.eq_ignore_ascii_case("x-amzn-waf-action") {
            match self.headers.aws_waf_action {
                Some(CapturedAwsWafAction::Challenge) => Some("challenge"),
                Some(CapturedAwsWafAction::Captcha) => Some("captcha"),
                None => None,
            }
        } else {
            None
        }
    }

    pub(crate) fn new(
        status: u16,
        cf_mitigated: Option<String>,
        aws_waf_action: Option<String>,
    ) -> Self {
        Self {
            status,
            headers: CapturedResponseHeaders {
                cf_mitigated_challenge: header_value_is(cf_mitigated.as_deref(), "challenge"),
                aws_waf_action: if header_value_is(aws_waf_action.as_deref(), "challenge") {
                    Some(CapturedAwsWafAction::Challenge)
                } else if header_value_is(aws_waf_action.as_deref(), "captcha") {
                    Some(CapturedAwsWafAction::Captcha)
                } else {
                    None
                },
            },
        }
    }
}

impl fmt::Debug for CapturedResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CapturedResponse")
            .field("status", &self.status)
            .field("has_cf_mitigated", &self.headers.cf_mitigated_challenge)
            .field(
                "has_x_amzn_waf_action",
                &self.headers.aws_waf_action.is_some(),
            )
            .finish()
    }
}

#[derive(Clone, Default)]
struct CapturedResponseHeaders {
    cf_mitigated_challenge: bool,
    aws_waf_action: Option<CapturedAwsWafAction>,
}

#[derive(Clone, Copy)]
enum CapturedAwsWafAction {
    Challenge,
    Captcha,
}

fn header_value_is(actual: Option<&str>, expected: &str) -> bool {
    actual.is_some_and(|value| value.trim().eq_ignore_ascii_case(expected))
}

#[derive(Debug, Error)]
pub enum ChromeError {
    #[error(
        "invalid CDP endpoint; expected an HTTP(S) discovery URL, a WebSocket URL, or a local port"
    )]
    InvalidCdpEndpoint,

    #[error("failed to discover a Chrome DevTools Protocol endpoint")]
    CdpDiscovery,

    #[error("failed to connect to the Chrome DevTools Protocol endpoint")]
    CdpConnection,

    #[error("the requested Chrome DevTools Protocol page target was not found")]
    CdpTargetNotFound,

    #[error("multiple Chrome page targets are available; select one explicitly")]
    CdpTargetAmbiguous,

    #[error("Chrome DevTools Protocol command `{method}` failed: {message}")]
    CdpCommand {
        method: &'static str,
        message: String,
    },

    #[error("Chrome DevTools Protocol navigation failed: {0}")]
    CdpNavigation(String),

    #[error("Chrome DevTools Protocol acquisition timed out")]
    CdpTimeout,

    #[error("Chrome DevTools Protocol returned an invalid page capture")]
    InvalidCdpCapture,

    #[error("Chrome capture exceeds the {limit} byte limit")]
    CaptureTooLarge { limit: usize },

    #[error("invalid rendered-page probe; selectors must be bounded and IDs must be unique")]
    InvalidRenderedProbe,

    #[error("Chrome or Chromium could not be found; set an explicit executable path")]
    ChromeNotFound,

    #[error("failed to launch Chrome")]
    ChromeLaunch,

    #[error("Chrome exited before exposing its DevTools endpoint")]
    ChromeExited,

    #[error("Chrome did not expose its DevTools endpoint before the startup timeout")]
    ChromeStartupTimeout,

    #[error("failed to fully stop Chrome or remove its temporary profile")]
    ChromeCleanup,
}

/// Capture through a caller-managed CDP endpoint.
pub async fn capture_cdp(
    source: &CdpSource,
    options: &CaptureOptions,
) -> Result<CapturedPage, ChromeError> {
    cdp::capture(source, options, cdp::UserAgentPolicy::Preserve, &[]).await
}

/// Capture through caller-managed CDP and request privacy-bounded live layout
/// evidence for the supplied CSS selectors.
pub async fn capture_cdp_with_probes(
    source: &CdpSource,
    options: &CaptureOptions,
    probes: &[RenderedProbe],
) -> Result<CapturedPage, ChromeError> {
    rendered::validate_probes(probes)?;
    cdp::capture(source, options, cdp::UserAgentPolicy::Preserve, probes).await
}

/// Discover, launch, capture through, and stop an Opsail-owned Chrome process.
pub async fn capture_chrome(
    source: &ChromeSource,
    options: &CaptureOptions,
) -> Result<CapturedPage, ChromeError> {
    capture_chrome_impl(source, options, &[]).await
}

/// Launch an Opsail-owned Chrome process and request privacy-bounded live
/// layout evidence for the supplied CSS selectors.
pub async fn capture_chrome_with_probes(
    source: &ChromeSource,
    options: &CaptureOptions,
    probes: &[RenderedProbe],
) -> Result<CapturedPage, ChromeError> {
    rendered::validate_probes(probes)?;
    capture_chrome_impl(source, options, probes).await
}

async fn capture_chrome_impl(
    source: &ChromeSource,
    options: &CaptureOptions,
    probes: &[RenderedProbe],
) -> Result<CapturedPage, ChromeError> {
    let started = Instant::now();
    let chrome = launcher::launch(source.executable_path.as_deref(), options.timeout).await?;
    let elapsed = started.elapsed();
    let Some(remaining) = options.timeout.checked_sub(elapsed) else {
        let _ = chrome.shutdown().await;
        return Err(ChromeError::CdpTimeout);
    };

    let cdp_source = CdpSource {
        endpoint: chrome.endpoint().to_owned(),
        url: Some(source.url.clone()),
        target_id: None,
        direct_page: false,
        wait_until: source.wait_until,
    };
    let capture_options = CaptureOptions {
        timeout: remaining,
        ..options.clone()
    };
    let result = cdp::capture(
        &cdp_source,
        &capture_options,
        cdp::UserAgentPolicy::BrowserCompatible,
        probes,
    )
    .await;
    cdp::close_browser(chrome.endpoint(), BROWSER_CLOSE_TIMEOUT).await;
    let cleanup = chrome.shutdown().await;
    match result {
        Ok(captured) => cleanup.map(|()| captured),
        Err(error) => Err(error),
    }
}
