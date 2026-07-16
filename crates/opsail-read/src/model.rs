use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use opsail_chrome::{CdpSource, ChromeSource};
use serde::Serialize;
use serde_json::{Value, json};
use url::Url;

pub const DEFAULT_MAX_BYTES: usize = 5 * 1024 * 1024;
pub const DEFAULT_MAX_ELEMENTS: usize = 50_000;
pub const DEFAULT_MAX_DEPTH: usize = 256;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const DEFAULT_USER_AGENT: &str = concat!("opsail/", env!("CARGO_PKG_VERSION"));

/// A caller-managed document captured from a browser or another rendered host.
#[derive(Clone)]
pub struct CapturedDocument {
    pub html: String,
    pub base_url: Option<Url>,
    pub final_url: Option<Url>,
}

impl CapturedDocument {
    pub fn new(html: impl Into<String>, final_url: Option<Url>) -> Self {
        Self {
            html: html.into(),
            base_url: final_url.clone(),
            final_url,
        }
    }

    pub fn with_urls(
        html: impl Into<String>,
        base_url: Option<Url>,
        final_url: Option<Url>,
    ) -> Self {
        Self {
            html: html.into(),
            base_url,
            final_url,
        }
    }
}

impl fmt::Debug for CapturedDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CapturedDocument")
            .field("html_bytes", &self.html.len())
            .field("has_base_url", &self.base_url.is_some())
            .field("has_final_url", &self.final_url.is_some())
            .finish()
    }
}

/// The source to acquire and read.
#[derive(Clone)]
pub enum ReadSource {
    Url(Url),
    File(PathBuf),
    Stdin(Vec<u8>),
    Html(CapturedDocument),
    Cdp(CdpSource),
    Chrome(ChromeSource),
    /// Compatibility input for callers that supplied HTML through `ReadOptions::base_url`.
    Memory(String),
}

impl fmt::Debug for ReadSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Url(_) => formatter.debug_tuple("Url").field(&"<redacted>").finish(),
            Self::File(_) => formatter.debug_tuple("File").field(&"<redacted>").finish(),
            Self::Stdin(bytes) => formatter
                .debug_struct("Stdin")
                .field("bytes", &bytes.len())
                .finish(),
            Self::Html(document) => formatter.debug_tuple("Html").field(document).finish(),
            Self::Cdp(source) => formatter.debug_tuple("Cdp").field(source).finish(),
            Self::Chrome(source) => formatter.debug_tuple("Chrome").field(source).finish(),
            Self::Memory(html) => formatter
                .debug_struct("Memory")
                .field("html_bytes", &html.len())
                .finish(),
        }
    }
}

/// Backwards-compatible name for [`ReadSource`].
pub type Input = ReadSource;

/// Limits and request settings used while acquiring a document.
#[derive(Debug, Clone)]
pub struct ReadOptions {
    pub base_url: Option<Url>,
    pub timeout: Duration,
    pub connect_timeout: Duration,
    pub max_bytes: usize,
    /// An exact User-Agent value, or `None` to select the automatic profile.
    pub user_agent: Option<String>,
    pub accept_language: Option<String>,
}

impl Default for ReadOptions {
    fn default() -> Self {
        Self {
            base_url: None,
            timeout: DEFAULT_TIMEOUT,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            max_bytes: DEFAULT_MAX_BYTES,
            user_agent: None,
            accept_language: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    Url,
    File,
    Stdin,
    Html,
    Cdp,
    Chrome,
    Memory,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
    pub kind: SourceKind,
    pub requested: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_url: Option<Url>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    pub charset: String,
    pub bytes: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentMetadata {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub favicon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExtractionMethod {
    Readability,
    Expanded,
    Semantic,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionInfo {
    pub method: ExtractionMethod,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum QualityGrade {
    Good,
    Fair,
    Thin,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QualityInfo {
    pub grade: QualityGrade,
    pub content_characters: usize,
    pub word_count: usize,
    pub extraction_ratio: f64,
    pub probably_readable: bool,
}

/// A stable, agent-readable representation of an extracted document.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadResult {
    pub schema_version: u8,
    pub content: String,
    pub content_html: String,
    pub metadata: DocumentMetadata,
    pub source: SourceInfo,
    pub extraction: ExtractionInfo,
    pub quality: QualityInfo,
    pub warnings: Vec<String>,
}

impl ReadResult {
    /// Return a named projection used by the CLI's `--property` option.
    pub fn property(&self, name: &str) -> Option<Value> {
        let value = match name {
            "content" | "markdown" => json!(self.content),
            "contentHtml" | "html" => json!(self.content_html),
            "title" => json!(self.metadata.title),
            "author" => json!(self.metadata.author),
            "description" => json!(self.metadata.description),
            "site" => json!(self.metadata.site),
            "published" => json!(self.metadata.published),
            "modified" => json!(self.metadata.modified),
            "image" => json!(self.metadata.image),
            "favicon" => json!(self.metadata.favicon),
            "language" => json!(self.metadata.language),
            "direction" => json!(self.metadata.direction),
            "url" | "canonicalUrl" => json!(self.metadata.canonical_url),
            "domain" => json!(self.metadata.domain),
            "wordCount" => json!(self.quality.word_count),
            "quality" => json!(self.quality),
            "source" => json!(self.source),
            "extraction" => json!(self.extraction),
            _ => return None,
        };
        Some(value)
    }
}
