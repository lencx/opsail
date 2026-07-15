use std::sync::Once;

use encoding_rs::{Encoding, UTF_8, WINDOWS_1252};
use futures_util::StreamExt;
use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, CONTENT_LENGTH, CONTENT_TYPE};
use tokio::io::AsyncReadExt;
use url::Url;

use crate::error::ReadError;
use crate::model::{Input, ReadOptions, SourceInfo, SourceKind};

const ACCEPT_VALUE: &str = "text/html, application/xhtml+xml;q=0.9, */*;q=0.1";
static INSTALL_TLS_PROVIDER: Once = Once::new();

pub(crate) struct LoadedDocument {
    pub html: String,
    pub base_url: Option<Url>,
    pub source: SourceInfo,
    pub warnings: Vec<String>,
}

pub(crate) async fn load(input: Input, options: &ReadOptions) -> Result<LoadedDocument, ReadError> {
    if let Some(base_url) = &options.base_url {
        validate_web_url(base_url)?;
    }

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
    }
}

async fn load_url(url: Url, options: &ReadOptions) -> Result<LoadedDocument, ReadError> {
    validate_web_url(&url)?;

    INSTALL_TLS_PROVIDER.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    let client = reqwest::Client::builder()
        .user_agent(&options.user_agent)
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
    if !status.is_success() {
        return Err(ReadError::HttpStatus {
            url: final_url.to_string(),
            status: status.as_u16(),
        });
    }

    if response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > options.max_bytes)
    {
        return Err(ReadError::InputTooLarge {
            limit: options.max_bytes,
        });
    }

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    if let Some(content_type) = &content_type
        && !content_type_is_html(content_type)
        && !content_type_is_tolerated_generic(content_type)
    {
        return Err(ReadError::UnsupportedContentType(content_type.clone()));
    }

    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|source| ReadError::ReadResponse {
            url: final_url.to_string(),
            source: source.without_url(),
        })?;
        if bytes.len().saturating_add(chunk.len()) > options.max_bytes {
            return Err(ReadError::InputTooLarge {
                limit: options.max_bytes,
            });
        }
        bytes.extend_from_slice(&chunk);
    }

    decode_loaded(
        bytes,
        SourceKind::Url,
        url.to_string(),
        Some(final_url.clone()),
        Some(final_url),
        content_type,
        options.max_bytes,
    )
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
}
