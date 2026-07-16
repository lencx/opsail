use std::sync::Once;

use encoding_rs::{Encoding, UTF_8, WINDOWS_1252};
use futures_util::StreamExt;
use opsail_chrome::{
    CaptureOptions, CapturedPage, CdpSource, ChromeError, ChromeSource, RenderedPageEvidence,
    capture_cdp_with_probes, capture_chrome_with_probes,
};
use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, CONTENT_LENGTH, CONTENT_TYPE};
use tokio::io::AsyncReadExt;
use url::Url;

use crate::error::ReadError;
use crate::model::{
    CapturedDocument, DEFAULT_USER_AGENT, Input, ReadOptions, SourceInfo, SourceKind,
};
use crate::verification;

const ACCEPT_VALUE: &str = "text/html, application/xhtml+xml;q=0.9, */*;q=0.1";
const MAX_ERROR_HTML_PROBE_BYTES: usize = 512 * 1024;
const WECHAT_BROWSER_USER_AGENT: &str = concat!(
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) ",
    "AppleWebKit/537.36 (KHTML, like Gecko) ",
    "Chrome/138.0.0.0 Safari/537.36 opsail/",
    env!("CARGO_PKG_VERSION")
);
static INSTALL_TLS_PROVIDER: Once = Once::new();

pub(crate) struct LoadedDocument {
    pub html: String,
    pub base_url: Option<Url>,
    pub source: SourceInfo,
    pub warnings: Vec<String>,
    verification_context: VerificationContext,
}

enum VerificationContext {
    Static,
    Browser(Option<RenderedPageEvidence>),
}

pub(crate) async fn load(input: Input, options: &ReadOptions) -> Result<LoadedDocument, ReadError> {
    if let Some(base_url) = &options.base_url {
        validate_web_url(base_url)?;
    }

    let loaded = load_unchecked(input, options).await?;
    validate_loaded_document(&loaded)?;
    Ok(loaded)
}

async fn load_unchecked(input: Input, options: &ReadOptions) -> Result<LoadedDocument, ReadError> {
    match input {
        Input::Url(url) => load_url(url, options).await,
        Input::File(path) => {
            let path_metadata =
                tokio::fs::metadata(&path)
                    .await
                    .map_err(|source| ReadError::ReadFile {
                        path: path.clone(),
                        source,
                    })?;
            if !path_metadata.is_file() {
                return Err(ReadError::NotRegularFile { path });
            }
            if path_metadata.len() > options.max_bytes as u64 {
                return Err(ReadError::InputTooLarge {
                    limit: options.max_bytes,
                });
            }

            let file =
                tokio::fs::File::open(&path)
                    .await
                    .map_err(|source| ReadError::ReadFile {
                        path: path.clone(),
                        source,
                    })?;
            let metadata = file
                .metadata()
                .await
                .map_err(|source| ReadError::ReadFile {
                    path: path.clone(),
                    source,
                })?;
            if !metadata.is_file() {
                return Err(ReadError::NotRegularFile { path });
            }
            if metadata.len() > options.max_bytes as u64 {
                return Err(ReadError::InputTooLarge {
                    limit: options.max_bytes,
                });
            }

            let read_limit = u64::try_from(options.max_bytes)
                .unwrap_or(u64::MAX)
                .saturating_add(1);
            let mut bytes = Vec::new();
            file.take(read_limit)
                .read_to_end(&mut bytes)
                .await
                .map_err(|source| ReadError::ReadFile {
                    path: path.clone(),
                    source,
                })?;
            if bytes.len() > options.max_bytes {
                return Err(ReadError::InputTooLarge {
                    limit: options.max_bytes,
                });
            }
            let canonical =
                tokio::fs::canonicalize(&path)
                    .await
                    .map_err(|source| ReadError::ResolveFile {
                        path: path.clone(),
                        source,
                    })?;
            let file_url = Url::from_file_path(&canonical).ok();
            decode_loaded(
                bytes,
                SourceKind::File,
                path.display().to_string(),
                options.base_url.clone(),
                file_url,
                Some("text/html".to_owned()),
                options.max_bytes,
            )
        }
        Input::Stdin(bytes) => decode_loaded(
            bytes,
            SourceKind::Stdin,
            "-".to_owned(),
            options.base_url.clone(),
            options.base_url.clone(),
            Some("text/html".to_owned()),
            options.max_bytes,
        ),
        Input::Html(document) => load_captured(document, options),
        Input::Cdp(source) => load_cdp(source, options).await,
        Input::Chrome(source) => load_chrome(source, options).await,
        Input::Memory(html) => load_memory(html, options),
    }
}

pub(crate) fn load_captured(
    document: CapturedDocument,
    options: &ReadOptions,
) -> Result<LoadedDocument, ReadError> {
    let final_url = document.final_url;
    let base_url = document
        .base_url
        .or_else(|| final_url.clone())
        .or_else(|| options.base_url.clone());
    let requested = final_url
        .as_ref()
        .or(base_url.as_ref())
        .map_or_else(|| "<html>".to_owned(), ToString::to_string);
    load_utf8_html(
        document.html,
        SourceKind::Html,
        requested,
        base_url,
        final_url,
        options.max_bytes,
    )
}

pub(crate) fn validate_loaded_document(loaded: &LoadedDocument) -> Result<(), ReadError> {
    let (rendered, allow_static_profile) = match &loaded.verification_context {
        VerificationContext::Static => (None, true),
        VerificationContext::Browser(rendered) => (rendered.as_ref(), false),
    };
    reject_verification_page_with_context(
        &loaded.html,
        loaded
            .source
            .resolved_url
            .as_ref()
            .or(loaded.base_url.as_ref()),
        &loaded.source.requested,
        rendered,
        allow_static_profile,
    )
}

async fn load_cdp(source: CdpSource, options: &ReadOptions) -> Result<LoadedDocument, ReadError> {
    if let Some(url) = source.url.as_ref() {
        validate_web_url(url)?;
    }
    let requested = source.url.as_ref().map(ToString::to_string);
    let probes = verification::rendered_probes().map_err(ReadError::Chrome)?;
    let captured = capture_cdp_with_probes(&source, &capture_options(options), &probes)
        .await
        .map_err(map_chrome_error)?;
    let requested = requested.unwrap_or_else(|| captured.final_url.to_string());
    load_browser_capture(captured, SourceKind::Cdp, requested, options.max_bytes)
}

async fn load_chrome(
    source: ChromeSource,
    options: &ReadOptions,
) -> Result<LoadedDocument, ReadError> {
    validate_web_url(&source.url)?;
    let requested = source.url.to_string();
    let probes = verification::rendered_probes().map_err(ReadError::Chrome)?;
    let captured = capture_chrome_with_probes(&source, &capture_options(options), &probes)
        .await
        .map_err(map_chrome_error)?;
    load_browser_capture(captured, SourceKind::Chrome, requested, options.max_bytes)
}

fn load_browser_capture(
    captured: CapturedPage,
    kind: SourceKind,
    requested: String,
    max_bytes: usize,
) -> Result<LoadedDocument, ReadError> {
    validate_web_url(&captured.final_url)?;
    if let Some(response) = captured.response() {
        reject_verification_response(
            response.status(),
            response.header("cf-mitigated"),
            response.header("x-amzn-waf-action"),
            &requested,
        )?;
    }
    let rendered_evidence = captured.rendered_evidence().cloned();
    let final_url = captured.final_url;
    let mut loaded = load_utf8_html(
        captured.html,
        kind,
        requested,
        Some(final_url.clone()),
        Some(final_url),
        max_bytes,
    )?;
    loaded.verification_context = VerificationContext::Browser(rendered_evidence);
    Ok(loaded)
}

fn capture_options(options: &ReadOptions) -> CaptureOptions {
    CaptureOptions {
        timeout: options.timeout,
        connect_timeout: options.connect_timeout,
        max_bytes: options.max_bytes,
        user_agent: options.user_agent.clone(),
        accept_language: options.accept_language.clone(),
    }
}

fn map_chrome_error(error: ChromeError) -> ReadError {
    match error {
        ChromeError::CaptureTooLarge { limit } => ReadError::InputTooLarge { limit },
        error => ReadError::Chrome(error),
    }
}

fn load_memory(html: String, options: &ReadOptions) -> Result<LoadedDocument, ReadError> {
    let requested = options
        .base_url
        .as_ref()
        .map_or_else(|| "<memory>".to_owned(), ToString::to_string);
    load_utf8_html(
        html,
        SourceKind::Memory,
        requested,
        options.base_url.clone(),
        options.base_url.clone(),
        options.max_bytes,
    )
}

async fn load_url(url: Url, options: &ReadOptions) -> Result<LoadedDocument, ReadError> {
    validate_web_url(&url)?;

    INSTALL_TLS_PROVIDER.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    let client = reqwest::Client::builder()
        .user_agent(request_user_agent(&url, options.user_agent.as_deref()))
        .connect_timeout(options.connect_timeout)
        .timeout(options.timeout)
        .redirect(redirect_policy())
        .build()
        .map_err(ReadError::BuildClient)?;

    let mut request = client.get(url.clone()).header(ACCEPT, ACCEPT_VALUE);
    if let Some(language) = &options.accept_language {
        request = request.header(ACCEPT_LANGUAGE, language);
    }

    let response = request.send().await.map_err(|source| ReadError::Request {
        url: url.to_string(),
        source: source.without_url(),
    })?;
    let status = response.status();
    let final_url = response.url().clone();
    validate_web_url(&final_url)?;
    reject_verification_response(
        status.as_u16(),
        response
            .headers()
            .get("cf-mitigated")
            .and_then(|value| value.to_str().ok()),
        response
            .headers()
            .get("x-amzn-waf-action")
            .and_then(|value| value.to_str().ok()),
        url.as_str(),
    )?;
    let body_limit = if status.is_success() {
        options.max_bytes
    } else {
        options.max_bytes.min(MAX_ERROR_HTML_PROBE_BYTES)
    };
    let declared_too_large = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > body_limit);

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let unsupported_content_type = content_type.as_ref().is_some_and(|content_type| {
        !content_type_is_html(content_type) && !content_type_is_tolerated_generic(content_type)
    });
    if !status.is_success() && (declared_too_large || unsupported_content_type) {
        return Err(http_status_error(&final_url, status.as_u16()));
    }
    if declared_too_large {
        return Err(ReadError::InputTooLarge {
            limit: options.max_bytes,
        });
    }
    if unsupported_content_type {
        return Err(ReadError::UnsupportedContentType(
            content_type.expect("unsupported content type is present"),
        ));
    }

    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|source| ReadError::ReadResponse {
            url: final_url.to_string(),
            source: source.without_url(),
        })?;
        if bytes.len().saturating_add(chunk.len()) > body_limit {
            if !status.is_success() {
                return Err(http_status_error(&final_url, status.as_u16()));
            }
            return Err(ReadError::InputTooLarge {
                limit: options.max_bytes,
            });
        }
        bytes.extend_from_slice(&chunk);
    }

    let loaded = decode_loaded(
        bytes,
        SourceKind::Url,
        url.to_string(),
        Some(final_url.clone()),
        Some(final_url.clone()),
        content_type,
        body_limit,
    );
    if !status.is_success() {
        if let Ok(loaded) = loaded {
            reject_verification_page(
                &loaded.html,
                loaded.source.resolved_url.as_ref(),
                url.as_str(),
            )?;
        }
        return Err(http_status_error(&final_url, status.as_u16()));
    }
    loaded
}

fn http_status_error(url: &Url, status: u16) -> ReadError {
    ReadError::HttpStatus {
        url: url.to_string(),
        status,
    }
}

fn request_user_agent<'a>(url: &Url, configured: Option<&'a str>) -> &'a str {
    match configured {
        Some(user_agent) => user_agent,
        None if is_wechat_url(url) => WECHAT_BROWSER_USER_AGENT,
        None => DEFAULT_USER_AGENT,
    }
}

fn reject_verification_page(
    html: &str,
    resolved_url: Option<&Url>,
    requested_url: &str,
) -> Result<(), ReadError> {
    reject_verification_page_with_context(html, resolved_url, requested_url, None, true)
}

fn reject_verification_page_with_context(
    html: &str,
    resolved_url: Option<&Url>,
    requested_url: &str,
    rendered: Option<&RenderedPageEvidence>,
    allow_static_profile: bool,
) -> Result<(), ReadError> {
    if verification::detect_document(html, resolved_url, rendered, allow_static_profile).is_some() {
        return Err(ReadError::VerificationRequired {
            url: verification::redacted_url(requested_url),
        });
    }
    Ok(())
}

fn reject_verification_response(
    status: u16,
    cf_mitigated: Option<&str>,
    aws_waf_action: Option<&str>,
    requested_url: &str,
) -> Result<(), ReadError> {
    if verification::detect_response(status, cf_mitigated, aws_waf_action).is_some() {
        return Err(ReadError::VerificationRequired {
            url: verification::redacted_url(requested_url),
        });
    }
    Ok(())
}

fn is_wechat_url(url: &Url) -> bool {
    url.host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("mp.weixin.qq.com"))
}

pub(crate) fn validate_web_url(url: &Url) -> Result<(), ReadError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ReadError::UnsupportedScheme(url.scheme().to_owned()));
    }
    if url_has_credentials(url) {
        return Err(ReadError::UrlContainsCredentials);
    }
    Ok(())
}

fn url_has_credentials(url: &Url) -> bool {
    !url.username().is_empty() || url.password().is_some()
}

fn redirect_policy() -> reqwest::redirect::Policy {
    let limit = reqwest::redirect::Policy::limited(10);
    reqwest::redirect::Policy::custom(move |attempt| {
        let rejection = {
            let next = attempt.url();
            if !matches!(next.scheme(), "http" | "https") {
                Some("redirect target scheme is not allowed")
            } else if url_has_credentials(next) {
                Some("redirect target credentials are not allowed")
            } else {
                None
            }
        };

        match rejection {
            Some(reason) => attempt.error(reason),
            None => limit.redirect(attempt),
        }
    })
}

fn decode_loaded(
    bytes: Vec<u8>,
    kind: SourceKind,
    requested: String,
    base_url: Option<Url>,
    resolved_url: Option<Url>,
    content_type: Option<String>,
    max_bytes: usize,
) -> Result<LoadedDocument, ReadError> {
    if bytes.len() > max_bytes {
        return Err(ReadError::InputTooLarge { limit: max_bytes });
    }
    if bytes.is_empty() {
        return Err(ReadError::EmptyInput);
    }
    if bytes.iter().take(4096).any(|byte| *byte == 0) {
        return Err(ReadError::NotHtml);
    }

    let encoding = detect_encoding(content_type.as_deref(), &bytes);
    let (decoded, actual_encoding, had_errors) = encoding.decode(&bytes);
    if !looks_like_html(&decoded) {
        return Err(ReadError::NotHtml);
    }

    let mut warnings = Vec::new();
    if had_errors {
        warnings.push(format!(
            "the input contained invalid {} byte sequences and was decoded with replacements",
            actual_encoding.name()
        ));
    }

    Ok(LoadedDocument {
        html: decoded.into_owned(),
        base_url: base_url.clone(),
        source: SourceInfo {
            kind,
            requested,
            resolved_url,
            content_type,
            charset: actual_encoding.name().to_ascii_lowercase(),
            bytes: bytes.len(),
        },
        warnings,
        verification_context: VerificationContext::Static,
    })
}

fn load_utf8_html(
    html: String,
    kind: SourceKind,
    requested: String,
    base_url: Option<Url>,
    resolved_url: Option<Url>,
    max_bytes: usize,
) -> Result<LoadedDocument, ReadError> {
    if let Some(base_url) = base_url.as_ref() {
        validate_web_url(base_url)?;
    }
    if let Some(resolved_url) = resolved_url.as_ref() {
        validate_web_url(resolved_url)?;
    }

    let bytes = html.len();
    if bytes > max_bytes {
        return Err(ReadError::InputTooLarge { limit: max_bytes });
    }
    if html.is_empty() {
        return Err(ReadError::EmptyInput);
    }
    if html.as_bytes().iter().take(4096).any(|byte| *byte == 0) || !looks_like_html(&html) {
        return Err(ReadError::NotHtml);
    }

    Ok(LoadedDocument {
        html,
        base_url,
        source: SourceInfo {
            kind,
            requested,
            resolved_url,
            content_type: Some("text/html".to_owned()),
            charset: "utf-8".to_owned(),
            bytes,
        },
        warnings: Vec::new(),
        verification_context: VerificationContext::Static,
    })
}

fn content_type_is_html(content_type: &str) -> bool {
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    matches!(media_type.as_str(), "text/html" | "application/xhtml+xml")
}

fn content_type_is_tolerated_generic(content_type: &str) -> bool {
    let media_type = content_type.split(';').next().unwrap_or_default().trim();
    media_type.eq_ignore_ascii_case("text/plain")
        || media_type.eq_ignore_ascii_case("application/octet-stream")
}

fn looks_like_html(input: &str) -> bool {
    let sample = input
        .trim_start_matches('\u{feff}')
        .trim_start()
        .chars()
        .take(8192)
        .collect::<String>()
        .to_ascii_lowercase();
    sample.starts_with('<')
        && [
            "<!doctype",
            "<html",
            "<head",
            "<body",
            "<main",
            "<article",
            "<section",
            "<div",
            "<p",
            "<h1",
            "<h2",
            "<table",
            "<pre",
            "<ul",
            "<ol",
            "<figure",
        ]
        .iter()
        .any(|marker| sample.contains(marker))
}

fn detect_encoding(content_type: Option<&str>, bytes: &[u8]) -> &'static Encoding {
    if let Some((encoding, _)) = Encoding::for_bom(bytes) {
        return encoding;
    }
    if let Some(label) = content_type.and_then(charset_from_content_type)
        && let Some(encoding) = Encoding::for_label(label.as_bytes())
    {
        return encoding;
    }
    if let Some(label) = charset_from_meta(bytes)
        && let Some(encoding) = Encoding::for_label(label.as_bytes())
    {
        return encoding;
    }
    if std::str::from_utf8(bytes).is_ok() {
        UTF_8
    } else {
        WINDOWS_1252
    }
}

fn charset_from_content_type(content_type: &str) -> Option<String> {
    content_type.split(';').skip(1).find_map(|parameter| {
        let (name, value) = parameter.trim().split_once('=')?;
        name.eq_ignore_ascii_case("charset")
            .then(|| value.trim().trim_matches(['\'', '"']).to_owned())
    })
}

fn charset_from_meta(bytes: &[u8]) -> Option<String> {
    let sample = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).to_ascii_lowercase();
    let start = sample.find("charset")?;
    let after = sample.get(start + "charset".len()..)?.trim_start();
    let after = after.strip_prefix('=')?.trim_start();
    let after = after.trim_start_matches(['\'', '"']);
    let label: String = after
        .chars()
        .take_while(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | ':')
        })
        .collect();
    (!label.is_empty()).then_some(label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_meta_charset() {
        assert_eq!(
            charset_from_meta(br#"<meta charset="windows-1252">"#).as_deref(),
            Some("windows-1252")
        );
    }

    #[test]
    fn rejects_plain_text() {
        assert!(!looks_like_html("This is only text."));
    }

    #[test]
    fn accepts_html_fragments() {
        assert!(looks_like_html("  <main><p>Readable</p></main>"));
    }

    #[test]
    fn uses_a_browser_compatible_default_user_agent_for_wechat() {
        let url = Url::parse("https://mp.weixin.qq.com/s/example").unwrap();
        let user_agent = request_user_agent(&url, None);

        assert!(user_agent.starts_with("Mozilla/5.0 "));
        assert!(user_agent.contains("Safari/537.36"));
        assert!(user_agent.contains("opsail/"));
    }

    #[test]
    fn preserves_an_explicit_user_agent_for_wechat() {
        let url = Url::parse("https://mp.weixin.qq.com/s/example").unwrap();
        assert_eq!(
            request_user_agent(&url, Some("research-reader/42")),
            "research-reader/42"
        );
        assert_eq!(
            request_user_agent(&url, Some(DEFAULT_USER_AGENT)),
            DEFAULT_USER_AGENT
        );

        let unrelated = Url::parse("https://example.test/article").unwrap();
        assert_eq!(request_user_agent(&unrelated, None), DEFAULT_USER_AGENT);
    }

    #[test]
    fn rejects_a_high_confidence_wechat_verification_page() {
        let requested_url =
            "https://mp.weixin.qq.com/s/challenge-fixture?poc_token=request-secret#fragment";
        let resolved_url = Url::parse(
            "https://mp.weixin.qq.com/mp/wappoc_appmsgcaptcha?poc_token=redirect-secret",
        )
        .unwrap();
        let html = r#"<!doctype html>
            <html><head>
              <script>var PAGE_MID = 'mmbizwap:secitptpage/verify.html';</script>
              <link rel="stylesheet" href="/secitptpage/verify.css">
            </head><body>
              <main id="js_verify" class="weui-msg">
                <h1>当前环境异常</h1><a href="/mp/verify">去验证</a>
              </main>
            </body></html>"#;

        assert!(matches!(
            reject_verification_page(html, Some(&resolved_url), requested_url),
            Err(ReadError::VerificationRequired { url: rejected })
                if rejected == "https://mp.weixin.qq.com/s/challenge-fixture"
                    && !rejected.contains("request-secret")
        ));
    }

    #[test]
    fn does_not_reject_articles_or_non_wechat_pages_with_verification_markers() {
        let wechat = Url::parse("https://mp.weixin.qq.com/s/article").unwrap();
        let unrelated = Url::parse("https://example.test/copied-page").unwrap();
        let article = r#"<!doctype html><html><body>
            <script>var example = 'secitptpage/verify';</script>
            <article id="js_article"><div id="js_content">
              <div id="js_verify" class="weui-msg">Quoted interface markup.</div>
            </div></article>
        </body></html>"#;
        let copied_challenge = r#"<!doctype html><html><body>
            <script>var PAGE_MID = 'mmbizwap:secitptpage/verify.html';</script>
            <main id="js_verify" class="weui-msg">Copied verification page.</main>
        </body></html>"#;

        assert!(reject_verification_page(article, Some(&wechat), wechat.as_str()).is_ok());
        assert!(
            reject_verification_page(copied_challenge, Some(&unrelated), unrelated.as_str())
                .is_ok()
        );
    }

    #[test]
    fn rejects_a_high_confidence_cloudflare_challenge_page() {
        let requested_url = "https://www.npmjs.com/package/opsail";
        let resolved_url = Url::parse(requested_url).unwrap();
        let html = r#"<!doctype html>
            <html lang="en"><head>
              <title>Just a moment...</title>
              <meta name="robots" content="noindex,nofollow">
            </head><body>
              <main class="main-wrapper" role="main">
                <noscript><span id="challenge-error-text">
                  Enable JavaScript and cookies to continue
                </span></noscript>
                <form id="challenge-form" action="/__cf_chl_f_tk=challenge-secret"></form>
              </main>
              <script>
                window._cf_chl_opt = {
                  cZone: 'www.npmjs.com',
                  cType: 'managed',
                  cRay: '0123456789abcdef'
                };
                var challenge = document.createElement('script');
                challenge.src = '/cdn-cgi/challenge-platform/h/g/orchestrate/chl_page/v1';
              </script>
            </body></html>"#;

        assert!(matches!(
            reject_verification_page(html, Some(&resolved_url), requested_url),
            Err(ReadError::VerificationRequired { .. })
        ));
    }

    #[test]
    fn does_not_reject_an_article_discussing_cloudflare_challenges() {
        let url = Url::parse("https://example.test/cloudflare-challenge-guide").unwrap();
        let article = r#"<!doctype html><html><head>
          <title>Understanding Cloudflare challenge pages</title>
        </head><body><article>
          <h1>Diagnosing a Cloudflare challenge</h1>
          <p>A visitor may briefly see “Just a moment...” while checks run.</p>
          <p>Diagnostic terms include <code>_cf_chl_opt</code>, <code>cf-ray</code>,
             <code>challenge-form</code>, and <code>/cdn-cgi/challenge-platform</code>.</p>
          <p>This article explains those indicators and is not itself a verification page.</p>
        </article></body></html>"#;

        assert!(reject_verification_page(article, Some(&url), url.as_str()).is_ok());
    }
}
