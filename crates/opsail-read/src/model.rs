use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};
use url::Url;

pub const DEFAULT_MAX_BYTES: usize = 5 * 1024 * 1024;
pub const DEFAULT_MAX_ELEMENTS: usize = 50_000;
pub const DEFAULT_MAX_DEPTH: usize = 256;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The source to read.
#[derive(Debug, Clone)]
pub enum Input {
    Url(Url),
    File(PathBuf),
    Stdin(Vec<u8>),
}

/// Limits and request settings used while acquiring a document.
#[derive(Debug, Clone)]
pub struct ReadOptions {
    pub base_url: Option<Url>,
    pub timeout: Duration,
    pub connect_timeout: Duration,
    pub max_bytes: usize,
    pub user_agent: String,
    pub accept_language: Option<String>,
}

impl Default for ReadOptions {
    fn default() -> Self {
        Self {
            base_url: None,
            timeout: DEFAULT_TIMEOUT,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            max_bytes: DEFAULT_MAX_BYTES,
            user_agent: format!("opsail/{}", env!("CARGO_PKG_VERSION")),
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
