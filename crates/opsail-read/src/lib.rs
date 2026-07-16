//! Agent-ready HTML acquisition and readable content extraction.

mod error;
mod extract;
mod model;
mod source;
mod standardize;
mod verification;

use std::time::Instant;

use unicode_segmentation::UnicodeSegmentation;
use url::Url;

pub use error::ReadError;
pub use model::{
    CapturedDocument, DEFAULT_CONNECT_TIMEOUT, DEFAULT_MAX_BYTES, DEFAULT_MAX_DEPTH,
    DEFAULT_MAX_ELEMENTS, DEFAULT_TIMEOUT, DocumentMetadata, ExtractionInfo, ExtractionMethod,
    Input, QualityGrade, QualityInfo, ReadOptions, ReadResult, ReadSource, SourceInfo, SourceKind,
};
pub use opsail_chrome::{CdpSource, CdpWaitUntil, ChromeError, ChromeSource};

/// Acquire and extract one HTML document.
pub async fn read(source: ReadSource, options: &ReadOptions) -> Result<ReadResult, ReadError> {
    let loaded = source::load(source, options).await?;
    build_result(
        &loaded.html,
        loaded.base_url.as_ref(),
        loaded.source,
        loaded.warnings,
    )
}

/// Extract an in-memory HTML document without performing I/O.
pub fn extract_html(html: &str, base_url: Option<&Url>) -> Result<ReadResult, ReadError> {
    let options = ReadOptions::default();
    let loaded = source::load_captured(
        CapturedDocument::with_urls(html, base_url.cloned(), None),
        &options,
    )?;
    source::validate_loaded_document(&loaded)?;
    build_result(
        &loaded.html,
        loaded.base_url.as_ref(),
        loaded.source,
        loaded.warnings,
    )
}

fn build_result(
    source_html: &str,
    base_url: Option<&Url>,
    source: SourceInfo,
    mut warnings: Vec<String>,
) -> Result<ReadResult, ReadError> {
    let started = Instant::now();
    let mut extracted = extract::extract(source_html, base_url)?;
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    if extracted.metadata.canonical_url.is_none() {
        extracted.metadata.canonical_url = source
            .resolved_url
            .as_ref()
            .filter(|url| matches!(url.scheme(), "http" | "https"))
            .map(ToString::to_string)
            .or_else(|| {
                base_url
                    .filter(|url| matches!(url.scheme(), "http" | "https"))
                    .map(ToString::to_string)
            });
    }
    extracted.metadata.domain = extracted
        .metadata
        .canonical_url
        .as_deref()
        .and_then(|value| Url::parse(value).ok())
        .and_then(|url| url.host_str().map(str::to_owned))
        .or_else(|| {
            source
                .resolved_url
                .as_ref()
                .and_then(Url::host_str)
                .map(str::to_owned)
        });

    let content_characters = extracted.text.chars().count();
    let word_count = extracted.text.unicode_words().count();
    let source_characters = source_html.chars().count().max(1);
    let extraction_ratio = (content_characters as f64 / source_characters as f64).clamp(0.0, 1.0);
    let grade = quality_grade(content_characters, word_count);
    if grade == QualityGrade::Thin {
        warnings.push("the extracted document is unusually short".to_owned());
    }
    warnings.append(&mut extracted.warnings);

    Ok(ReadResult {
        schema_version: 1,
        content: extracted.content,
        content_html: extracted.content_html,
        metadata: extracted.metadata,
        source,
        extraction: ExtractionInfo {
            method: extracted.method,
            duration_ms,
        },
        quality: QualityInfo {
            grade,
            content_characters,
            word_count,
            extraction_ratio,
            probably_readable: extracted.probably_readable,
        },
        warnings,
    })
}

fn quality_grade(content_characters: usize, word_count: usize) -> QualityGrade {
    if content_characters >= 500 || word_count >= 100 {
        QualityGrade::Good
    } else if content_characters >= 120 || word_count >= 25 {
        QualityGrade::Fair
    } else {
        QualityGrade::Thin
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_an_in_memory_document() {
        let result = extract_html(
            "<html><head><title>Example</title></head><body><main><p>Readable text.</p></main></body></html>",
            Some(&Url::parse("https://example.test/post").unwrap()),
        )
        .unwrap();

        assert_eq!(result.metadata.title, "Example");
        assert!(result.content.contains("Readable text."));
        assert_eq!(result.source.kind, SourceKind::Html);
    }

    #[tokio::test]
    async fn preserves_unicode_in_already_decoded_memory_input() {
        let html = "<!doctype html><html><head><meta charset=\"windows-1252\"><title>Café</title></head><body><main><p>Café déjà vu.</p></main></body></html>";
        let result = read(Input::Memory(html.to_owned()), &ReadOptions::default())
            .await
            .unwrap();

        assert_eq!(result.metadata.title, "Café");
        assert!(result.content.contains("Café déjà vu."));
        assert_eq!(result.source.charset, "utf-8");
        assert_eq!(result.source.bytes, html.len());
    }

    #[tokio::test]
    async fn records_caller_captured_html_as_an_html_source() {
        let final_url = Url::parse("https://example.test/rendered/article").unwrap();
        let document = CapturedDocument::new(
            "<html><head><title>Captured</title></head><body><main><p>Rendered article text.</p></main></body></html>",
            Some(final_url.clone()),
        );
        let result = read(ReadSource::Html(document), &ReadOptions::default())
            .await
            .unwrap();

        assert_eq!(result.source.kind, SourceKind::Html);
        assert_eq!(result.source.resolved_url, Some(final_url));
        assert_eq!(result.metadata.title, "Captured");
    }

    #[tokio::test]
    async fn separates_captured_link_resolution_from_final_url_provenance() {
        let base_url = Url::parse("https://static.example.test/articles/current/").unwrap();
        let final_url = Url::parse("https://reader.example.test/final/article").unwrap();
        let document = CapturedDocument::with_urls(
            "<html><head><title>Captured</title></head><body><main><p>Rendered article text.</p><a href=\"../related\">Related</a></main></body></html>",
            Some(base_url),
            Some(final_url.clone()),
        );
        let result = read(ReadSource::Html(document), &ReadOptions::default())
            .await
            .unwrap();

        assert_eq!(result.source.resolved_url, Some(final_url.clone()));
        assert_eq!(
            result.metadata.canonical_url.as_deref(),
            Some(final_url.as_str())
        );
        assert!(
            result
                .content_html
                .contains("https://static.example.test/articles/related")
        );
    }

    #[test]
    fn debug_output_redacts_browser_endpoints_urls_paths_and_html() {
        let mut cdp =
            CdpSource::new("wss://provider.example.test/devtools/browser/id?token=secret");
        cdp.url = Some(Url::parse("https://private.example.test/account?key=secret").unwrap());
        let cdp_debug = format!("{:?}", ReadSource::Cdp(cdp));
        assert!(!cdp_debug.contains("provider.example.test"));
        assert!(!cdp_debug.contains("private.example.test"));
        assert!(!cdp_debug.contains("secret"));

        let mut chrome = ChromeSource::new(
            Url::parse("https://private.example.test/launch?key=secret").unwrap(),
        );
        chrome.executable_path = Some("/private/path/to/chrome".into());
        let chrome_debug = format!("{:?}", ReadSource::Chrome(chrome));
        assert!(!chrome_debug.contains("private.example.test"));
        assert!(!chrome_debug.contains("/private/path/to/chrome"));
        assert!(!chrome_debug.contains("secret"));

        let html_debug = format!(
            "{:?}",
            ReadSource::Html(CapturedDocument::new(
                "<html><body>sensitive document text</body></html>",
                None,
            ))
        );
        assert!(!html_debug.contains("sensitive document text"));
        assert!(html_debug.contains("html_bytes"));
    }

    #[tokio::test]
    async fn rejects_a_verification_page_from_a_browser_capture() {
        let document = CapturedDocument::new(
            r#"<!doctype html><html><head>
              <script>var PAGE_MID = 'mmbizwap:secitptpage/verify.html';</script>
              <link rel="stylesheet" href="/secitptpage/verify.css">
            </head><body><main id="js_verify" class="weui-msg">
              <h1>环境异常</h1><p>完成验证后即可继续访问。</p>
            </main></body></html>"#,
            Some(Url::parse("https://mp.weixin.qq.com/s/example").unwrap()),
        );

        assert!(matches!(
            read(ReadSource::Html(document), &ReadOptions::default()).await,
            Err(ReadError::VerificationRequired { .. })
        ));
    }

    #[tokio::test]
    async fn validates_the_base_url_for_memory_input() {
        let html = "<html><body><main><p>Readable text.</p></main></body></html>";
        let mut options = ReadOptions {
            base_url: Some(Url::parse("file:///tmp/article.html").unwrap()),
            ..ReadOptions::default()
        };

        assert!(matches!(
            read(Input::Memory(html.to_owned()), &options).await,
            Err(ReadError::UnsupportedScheme(scheme)) if scheme == "file"
        ));

        options.base_url = Some(Url::parse("https://user:secret@example.test/article").unwrap());
        assert!(matches!(
            read(Input::Memory(html.to_owned()), &options).await,
            Err(ReadError::UrlContainsCredentials)
        ));
    }

    #[test]
    fn rejects_oversized_in_memory_documents() {
        let html = "x".repeat(DEFAULT_MAX_BYTES + 1);
        assert!(matches!(
            extract_html(&html, None),
            Err(ReadError::InputTooLarge { limit }) if limit == DEFAULT_MAX_BYTES
        ));
    }

    #[test]
    fn rejects_documents_with_too_many_elements() {
        let html = format!(
            "<html><body>{}</body></html>",
            "<div></div>".repeat(DEFAULT_MAX_ELEMENTS)
        );
        assert!(matches!(
            extract_html(&html, None),
            Err(ReadError::TooManyElements { limit, .. }) if limit == DEFAULT_MAX_ELEMENTS
        ));
    }

    #[test]
    fn rejects_documents_nested_beyond_the_depth_budget() {
        let html = format!(
            "<html><body>{}text{}</body></html>",
            "<div>".repeat(DEFAULT_MAX_DEPTH + 1),
            "</div>".repeat(DEFAULT_MAX_DEPTH + 1)
        );
        assert!(matches!(
            extract_html(&html, None),
            Err(ReadError::DocumentTooDeep { limit }) if limit == DEFAULT_MAX_DEPTH
        ));
    }

    #[test]
    fn metadata_cannot_publish_active_urls_or_inject_a_heading() {
        let base = Url::parse("https://example.test/article").unwrap();
        let result = extract_html(
            r#"<html><head>
                <meta property="og:title" content="Safe [link](javascript:alert(1)) &#35; forged">
                <meta property="og:url" content="javascript:alert(1)">
                <meta property="og:image" content="data:text/html,bad">
                </head><body><main><p>Visible article text.</p></main></body></html>"#,
            Some(&base),
        )
        .unwrap();

        assert_eq!(
            result
                .content
                .lines()
                .filter(|line| line.starts_with("# "))
                .count(),
            1
        );
        assert!(result.content.starts_with("# Safe \\[link\\]"));
        assert_eq!(
            result.metadata.canonical_url.as_deref(),
            Some("https://example.test/article")
        );
        assert_eq!(result.metadata.image, None);
    }

    #[test]
    fn rejects_non_web_or_credentialed_base_urls() {
        let file_url = Url::parse("file:///tmp/article.html").unwrap();
        assert!(matches!(
            extract_html("<main><p>Readable text.</p></main>", Some(&file_url)),
            Err(ReadError::UnsupportedScheme(scheme)) if scheme == "file"
        ));

        let credentialed = Url::parse("https://reader:secret@example.test/article").unwrap();
        assert!(matches!(
            extract_html("<main><p>Readable text.</p></main>", Some(&credentialed)),
            Err(ReadError::UrlContainsCredentials)
        ));
    }
}
