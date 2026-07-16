use std::io::{self, ErrorKind, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use opsail_read::{
    CapturedDocument, CdpSource, CdpWaitUntil, ChromeError, ChromeSource, ReadError, ReadOptions,
    ReadResult, ReadSource, read,
};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use url::Url;

const PROTOCOL_VERSION: u8 = 1;
const MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MachineRequest {
    protocol_version: u64,
    source: MachineSource,
    #[serde(default)]
    options: MachineOptions,
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum MachineSource {
    Url {
        url: String,
        #[serde(default)]
        user_agent: Option<String>,
        #[serde(default)]
        accept_language: Option<String>,
    },
    Html {
        html: String,
        #[serde(default)]
        base_url: Option<String>,
        #[serde(default)]
        final_url: Option<String>,
    },
    File {
        path: String,
        #[serde(default)]
        base_url: Option<String>,
    },
    Cdp {
        endpoint: String,
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        target_id: Option<String>,
        #[serde(default)]
        direct_page: bool,
        #[serde(default)]
        wait_until: CdpWaitUntil,
        #[serde(default)]
        user_agent: Option<String>,
        #[serde(default)]
        accept_language: Option<String>,
    },
    Chrome {
        url: String,
        #[serde(default)]
        chrome_path: Option<String>,
        #[serde(default)]
        wait_until: CdpWaitUntil,
        #[serde(default)]
        user_agent: Option<String>,
        #[serde(default)]
        accept_language: Option<String>,
    },
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MachineOptions {
    timeout_ms: Option<u64>,
    max_bytes: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum MachineResponse {
    Success(Box<SuccessResponse>),
    Failure(FailureResponse),
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SuccessResponse {
    protocol_version: u8,
    ok: bool,
    engine: EngineInfo,
    result: ReadResult,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FailureResponse {
    protocol_version: u8,
    ok: bool,
    engine: EngineInfo,
    error: MachineFailure,
}

#[derive(Debug, Serialize)]
struct EngineInfo {
    name: &'static str,
    version: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MachineFailure {
    code: &'static str,
    stage: FailureStage,
    message: String,
    retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery: Option<Recovery>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
enum FailureStage {
    Input,
    Acquire,
    Extract,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Recovery {
    RenderedHtml,
}

pub(crate) async fn run() -> ExitCode {
    let (response, exit_code) = match execute().await {
        Ok(result) => (
            MachineResponse::Success(Box::new(SuccessResponse {
                protocol_version: PROTOCOL_VERSION,
                ok: true,
                engine: engine_info(),
                result,
            })),
            ExitCode::SUCCESS,
        ),
        Err(error) => (
            MachineResponse::Failure(FailureResponse {
                protocol_version: PROTOCOL_VERSION,
                ok: false,
                engine: engine_info(),
                error,
            }),
            ExitCode::FAILURE,
        ),
    };

    match write_response(&response) {
        Ok(()) => exit_code,
        Err(error) if error.kind() == ErrorKind::BrokenPipe => exit_code,
        Err(error) => {
            let _ = writeln!(
                io::stderr().lock(),
                "failed to write machine response: {error}"
            );
            ExitCode::FAILURE
        }
    }
}

async fn execute() -> Result<ReadResult, MachineFailure> {
    let bytes = read_request().await?;
    let request: MachineRequest = serde_json::from_slice(&bytes).map_err(|_| {
        MachineFailure::new(
            "invalid-request",
            FailureStage::Input,
            "machine request is not valid protocol JSON",
        )
    })?;

    if request.protocol_version != u64::from(PROTOCOL_VERSION) {
        return Err(MachineFailure::new(
            "unsupported-protocol",
            FailureStage::Input,
            format!(
                "unsupported protocol version {}; expected {PROTOCOL_VERSION}",
                request.protocol_version
            ),
        ));
    }

    let (input, options) = request.into_read_request()?;
    read(input, &options)
        .await
        .map_err(MachineFailure::from_read_error)
}

async fn read_request() -> Result<Vec<u8>, MachineFailure> {
    let limit = u64::try_from(MAX_REQUEST_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut bytes = Vec::new();
    tokio::io::stdin()
        .take(limit)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| {
            MachineFailure::new(
                "input-read-failed",
                FailureStage::Input,
                "failed to read machine request from stdin",
            )
        })?;
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(MachineFailure::new(
            "request-too-large",
            FailureStage::Input,
            format!("machine request exceeds the {MAX_REQUEST_BYTES} byte limit"),
        ));
    }
    Ok(bytes)
}

impl MachineRequest {
    fn into_read_request(self) -> Result<(ReadSource, ReadOptions), MachineFailure> {
        let mut options = ReadOptions::default();
        if let Some(timeout_ms) = self.options.timeout_ms {
            if timeout_ms == 0 {
                return Err(MachineFailure::invalid_option(
                    "options.timeoutMs must be greater than zero",
                ));
            }
            options.timeout = Duration::from_millis(timeout_ms);
        }
        if let Some(max_bytes) = self.options.max_bytes {
            if max_bytes == 0 {
                return Err(MachineFailure::invalid_option(
                    "options.maxBytes must be greater than zero",
                ));
            }
            options.max_bytes = max_bytes;
        }

        let input = match self.source {
            MachineSource::Url {
                url,
                user_agent,
                accept_language,
            } => {
                let url = parse_web_url(&url, "source.url")?;
                options.user_agent = user_agent;
                options.accept_language = accept_language;
                ReadSource::Url(url)
            }
            MachineSource::Html {
                html,
                base_url,
                final_url,
            } => {
                let base_url = base_url
                    .as_deref()
                    .map(|value| parse_web_url(value, "source.baseUrl"))
                    .transpose()?;
                let final_url = final_url
                    .as_deref()
                    .map(|value| parse_web_url(value, "source.finalUrl"))
                    .transpose()?;
                ReadSource::Html(CapturedDocument::with_urls(html, base_url, final_url))
            }
            MachineSource::File { path, base_url } => {
                if path.is_empty() {
                    return Err(MachineFailure::new(
                        "invalid-path",
                        FailureStage::Input,
                        "source.path must not be empty",
                    ));
                }
                options.base_url = base_url
                    .as_deref()
                    .map(|value| parse_web_url(value, "source.baseUrl"))
                    .transpose()?;
                ReadSource::File(PathBuf::from(path))
            }
            MachineSource::Cdp {
                endpoint,
                url,
                target_id,
                direct_page,
                wait_until,
                user_agent,
                accept_language,
            } => {
                if endpoint.is_empty() {
                    return Err(MachineFailure::new(
                        "invalid-cdp-endpoint",
                        FailureStage::Input,
                        "source.endpoint must not be empty",
                    ));
                }
                if target_id.as_deref().is_some_and(str::is_empty) {
                    return Err(MachineFailure::new(
                        "invalid-cdp-target",
                        FailureStage::Input,
                        "source.targetId must not be empty",
                    ));
                }
                if direct_page && target_id.is_some() {
                    return Err(MachineFailure::new(
                        "invalid-cdp-target",
                        FailureStage::Input,
                        "source.targetId cannot be used with source.directPage",
                    ));
                }
                let url = url
                    .as_deref()
                    .map(|value| parse_web_url(value, "source.url"))
                    .transpose()?;
                options.user_agent = user_agent;
                options.accept_language = accept_language;
                ReadSource::Cdp(CdpSource {
                    endpoint,
                    url,
                    target_id,
                    direct_page,
                    wait_until,
                })
            }
            MachineSource::Chrome {
                url,
                chrome_path,
                wait_until,
                user_agent,
                accept_language,
            } => {
                let url = parse_web_url(&url, "source.url")?;
                if chrome_path.as_deref().is_some_and(str::is_empty) {
                    return Err(MachineFailure::new(
                        "invalid-chrome-path",
                        FailureStage::Input,
                        "source.chromePath must not be empty",
                    ));
                }
                options.user_agent = user_agent;
                options.accept_language = accept_language;
                ReadSource::Chrome(ChromeSource {
                    url,
                    executable_path: chrome_path.map(PathBuf::from),
                    wait_until,
                })
            }
        };

        Ok((input, options))
    }
}

fn parse_web_url(value: &str, field: &str) -> Result<Url, MachineFailure> {
    Url::parse(value).map_err(|_| {
        MachineFailure::new(
            "invalid-url",
            FailureStage::Input,
            format!("{field} must be a valid absolute HTTP or HTTPS URL"),
        )
    })
}

impl MachineFailure {
    fn new(code: &'static str, stage: FailureStage, message: impl Into<String>) -> Self {
        Self {
            code,
            stage,
            message: message.into(),
            retryable: false,
            recovery: None,
        }
    }

    fn invalid_option(message: impl Into<String>) -> Self {
        Self::new("invalid-option", FailureStage::Input, message)
    }

    fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }

    fn with_recovery(mut self, recovery: Recovery) -> Self {
        self.recovery = Some(recovery);
        self
    }

    fn from_read_error(error: ReadError) -> Self {
        match error {
            ReadError::UnsupportedScheme(_) => Self::new(
                "unsupported-scheme",
                FailureStage::Input,
                "source URL must use HTTP or HTTPS",
            ),
            ReadError::UrlContainsCredentials => Self::new(
                "url-contains-credentials",
                FailureStage::Input,
                "source URLs must not contain embedded credentials",
            ),
            ReadError::InputTooLarge { limit } => Self::new(
                "input-too-large",
                FailureStage::Input,
                format!("source exceeds the {limit} byte limit"),
            ),
            ReadError::TooManyElements { limit, .. } => Self::new(
                "document-too-complex",
                FailureStage::Extract,
                format!("document exceeds the {limit} element limit"),
            ),
            ReadError::DocumentTooDeep { limit } => Self::new(
                "document-too-complex",
                FailureStage::Extract,
                format!("document exceeds the {limit} level nesting limit"),
            ),
            ReadError::EmptyInput => {
                Self::new("empty-input", FailureStage::Input, "source HTML is empty")
            }
            ReadError::NotHtml => Self::new(
                "not-html",
                FailureStage::Input,
                "source does not appear to be HTML",
            ),
            ReadError::UnsupportedContentType(_) => Self::new(
                "unsupported-content-type",
                FailureStage::Acquire,
                "source returned an unsupported content type",
            ),
            ReadError::ReadFile { .. }
            | ReadError::NotRegularFile { .. }
            | ReadError::ResolveFile { .. } => Self::new(
                "source-unavailable",
                FailureStage::Input,
                "source could not be read",
            ),
            ReadError::BuildClient(_) => Self::new(
                "client-initialization-failed",
                FailureStage::Acquire,
                "HTTP client could not be initialized",
            ),
            ReadError::Request { source, .. } => {
                if source.is_timeout() {
                    Self::new(
                        "request-timeout",
                        FailureStage::Acquire,
                        "source request timed out",
                    )
                    .retryable()
                } else {
                    Self::new(
                        "request-failed",
                        FailureStage::Acquire,
                        "source request failed",
                    )
                }
            }
            ReadError::HttpStatus { status, .. } => {
                let failure = Self::new(
                    "http-status",
                    FailureStage::Acquire,
                    format!("source returned HTTP status {status}"),
                );
                if matches!(status, 408 | 425 | 429 | 500..=599) {
                    failure.retryable()
                } else {
                    failure
                }
            }
            ReadError::VerificationRequired { .. } => Self::new(
                "verification-required",
                FailureStage::Acquire,
                "source requires an interactive browser verification",
            )
            .with_recovery(Recovery::RenderedHtml),
            ReadError::Chrome(error) => Self::from_chrome_error(error),
            ReadError::ReadResponse { source, .. } => {
                if source.is_timeout() {
                    Self::new(
                        "request-timeout",
                        FailureStage::Acquire,
                        "source response timed out",
                    )
                    .retryable()
                } else {
                    Self::new(
                        "response-read-failed",
                        FailureStage::Acquire,
                        "source response could not be read",
                    )
                    .retryable()
                }
            }
            ReadError::Extraction(_) => Self::new(
                "extraction-failed",
                FailureStage::Extract,
                "readable content extraction failed",
            ),
            ReadError::NoContent => Self::new(
                "no-content",
                FailureStage::Extract,
                "no readable content was found",
            ),
        }
    }

    fn from_chrome_error(error: ChromeError) -> Self {
        match error {
            ChromeError::InvalidCdpEndpoint => Self::new(
                "invalid-cdp-endpoint",
                FailureStage::Input,
                "source.endpoint must identify a CDP discovery or WebSocket endpoint",
            ),
            ChromeError::CdpDiscovery | ChromeError::CdpConnection => Self::new(
                "cdp-unavailable",
                FailureStage::Acquire,
                "the CDP endpoint is unavailable",
            )
            .retryable(),
            ChromeError::CdpTargetNotFound => Self::new(
                "cdp-target-not-found",
                FailureStage::Acquire,
                "the requested browser page target was not found",
            ),
            ChromeError::CdpTargetAmbiguous => Self::new(
                "cdp-target-ambiguous",
                FailureStage::Acquire,
                "multiple browser page targets are available; source.targetId is required",
            ),
            ChromeError::CdpCommand { .. } => Self::new(
                "cdp-command-failed",
                FailureStage::Acquire,
                "the browser rejected a CDP capture command",
            ),
            ChromeError::CdpNavigation(_) => Self::new(
                "cdp-navigation-failed",
                FailureStage::Acquire,
                "the browser could not navigate to the source URL",
            )
            .retryable(),
            ChromeError::CdpTimeout | ChromeError::ChromeStartupTimeout => Self::new(
                "request-timeout",
                FailureStage::Acquire,
                "browser acquisition timed out",
            )
            .retryable(),
            ChromeError::InvalidCdpCapture => Self::new(
                "invalid-cdp-capture",
                FailureStage::Acquire,
                "the browser did not return a valid HTML document and final URL",
            )
            .with_recovery(Recovery::RenderedHtml),
            ChromeError::CaptureTooLarge { limit } => Self::new(
                "input-too-large",
                FailureStage::Input,
                format!("source exceeds the {limit} byte limit"),
            ),
            ChromeError::InvalidRenderedProbe => Self::new(
                "invalid-rendered-probe",
                FailureStage::Input,
                "browser render probes must use unique IDs and bounded CSS selectors",
            ),
            ChromeError::ChromeNotFound => Self::new(
                "chrome-not-found",
                FailureStage::Acquire,
                "Chrome or Chromium could not be found; configure source.chromePath",
            ),
            ChromeError::ChromeLaunch | ChromeError::ChromeExited => Self::new(
                "chrome-launch-failed",
                FailureStage::Acquire,
                "Chrome could not be started for capture",
            ),
            ChromeError::ChromeCleanup => Self::new(
                "chrome-cleanup-failed",
                FailureStage::Acquire,
                "Chrome capture completed but its owned resources could not be fully cleaned up",
            ),
        }
    }
}

fn engine_info() -> EngineInfo {
    EngineInfo {
        name: "opsail",
        version: env!("CARGO_PKG_VERSION"),
    }
}

fn write_response(response: &MachineResponse) -> io::Result<()> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    serde_json::to_writer(&mut stdout, response).map_err(io::Error::other)?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_http_statuses_are_explicit() {
        for status in [408, 425, 429, 500, 503, 599] {
            let failure = MachineFailure::from_read_error(ReadError::HttpStatus {
                url: "https://example.test/article".to_owned(),
                status,
            });
            assert!(failure.retryable, "HTTP {status} should be retryable");
        }

        let failure = MachineFailure::from_read_error(ReadError::HttpStatus {
            url: "https://example.test/article".to_owned(),
            status: 403,
        });
        assert!(!failure.retryable);
    }

    #[test]
    fn verification_failures_recommend_rendered_html() {
        let failure = MachineFailure::from_read_error(ReadError::VerificationRequired {
            url: "https://mp.weixin.qq.com/s/example".to_owned(),
        });

        assert_eq!(failure.code, "verification-required");
        assert!(matches!(failure.stage, FailureStage::Acquire));
        assert!(matches!(failure.recovery, Some(Recovery::RenderedHtml)));
        assert!(!failure.retryable);
    }
}
