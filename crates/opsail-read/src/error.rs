use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ReadError {
    #[error("unsupported URL scheme `{0}`; expected http or https")]
    UnsupportedScheme(String),

    #[error("URL must not contain embedded credentials")]
    UrlContainsCredentials,

    #[error("input exceeds the {limit} byte limit")]
    InputTooLarge { limit: usize },

    #[error("document contains {found} elements, exceeding the {limit} element limit")]
    TooManyElements { found: usize, limit: usize },

    #[error("document nesting exceeds the {limit} level limit")]
    DocumentTooDeep { limit: usize },

    #[error("input is empty")]
    EmptyInput,

    #[error("input does not appear to be an HTML document")]
    NotHtml,

    #[error("unsupported response content type `{0}`")]
    UnsupportedContentType(String),

    #[error("failed to read `{path}`")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("`{path}` is not a regular file")]
    NotRegularFile { path: PathBuf },

    #[error("failed to resolve `{path}`")]
    ResolveFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to create the HTTP client")]
    BuildClient(#[source] reqwest::Error),

    #[error("request failed for `{url}`")]
    Request {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("request for `{url}` returned HTTP {status}")]
    HttpStatus { url: String, status: u16 },

    #[error("request for `{url}` returned an interactive verification page")]
    VerificationRequired { url: String },

    #[error(transparent)]
    Chrome(#[from] opsail_chrome::ChromeError),

    #[error("failed while reading the response from `{url}`")]
    ReadResponse {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("failed to extract readable content")]
    Extraction(#[source] dom_smoothie::ReadabilityError),

    #[error("no readable content was found")]
    NoContent,
}
