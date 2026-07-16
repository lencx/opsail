//! Agent-ready HTML acquisition and readable content extraction.

mod error;
mod extract;
mod model;
mod source;
mod standardize;

use std::time::Instant;

use unicode_segmentation::UnicodeSegmentation;
use url::Url;

pub use error::ReadError;
pub use model::{
    DEFAULT_CONNECT_TIMEOUT, DEFAULT_MAX_BYTES, DEFAULT_MAX_DEPTH, DEFAULT_MAX_ELEMENTS,
    DEFAULT_TIMEOUT, DocumentMetadata, ExtractionInfo, ExtractionMethod, Input, QualityGrade,
    QualityInfo, ReadOptions, ReadResult, SourceInfo, SourceKind,
};

/// Acquire and extract one HTML document.
pub async fn read(input: Input, options: &ReadOptions) -> Result<ReadResult, ReadError> {
    let loaded = source::load(input, options).await?;
    build_result(
        &loaded.html,
        loaded.base_url.as_ref(),
        loaded.source,
        loaded.warnings,
    )
}

/// Extract an in-memory HTML document without performing I/O.
pub fn extract_html(html: &str, base_url: Option<&Url>) -> Result<ReadResult, ReadError> {
    if let Some(base_url) = base_url {
        source::validate_web_url(base_url)?;
    }
    if html.len() > DEFAULT_MAX_BYTES {
        return Err(ReadError::InputTooLarge {
            limit: DEFAULT_MAX_BYTES,
        });
    }
    let source = SourceInfo {
        kind: SourceKind::Memory,
        requested: base_url.map_or_else(|| "<memory>".to_owned(), ToString::to_string),
        resolved_url: base_url.cloned(),
        content_type: Some("text/html".to_owned()),
        charset: "utf-8".to_owned(),
        bytes: html.len(),
    };
    build_result(html, base_url, source, Vec::new())
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
        extracted.metadata.canonical_url = base_url
            .filter(|url| matches!(url.scheme(), "http" | "https"))
            .map(ToString::to_string)
            .or_else(|| {
                source
                    .resolved_url
                    .as_ref()
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
        assert_eq!(result.source.kind, SourceKind::Memory);
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
