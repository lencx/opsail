use std::time::Duration;

use opsail_read::{Input, ReadError, ReadOptions, SourceKind, read};
use tempfile::tempdir;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const HTML: &str = "<!doctype html><html><head><title>Local note</title></head><body><main><p>A readable local note for acquisition tests.</p></main></body></html>";

#[tokio::test]
async fn reads_files_with_an_explicit_base_url() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("note.html");
    std::fs::write(&path, HTML).unwrap();
    let options = ReadOptions {
        base_url: Some(Url::parse("https://example.test/notes/local").unwrap()),
        ..ReadOptions::default()
    };

    let result = read(Input::File(path.clone()), &options).await.unwrap();

    assert_eq!(result.source.kind, SourceKind::File);
    assert_eq!(result.source.requested, path.display().to_string());
    assert_eq!(
        result.source.resolved_url.as_ref().map(Url::scheme),
        Some("file")
    );
    assert_eq!(
        result.metadata.canonical_url.as_deref(),
        Some("https://example.test/notes/local")
    );
    assert!(result.content.contains("A readable local note"));
}

#[tokio::test]
async fn keeps_file_links_relative_without_an_explicit_base_url() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("note.html");
    let html = "<!doctype html><html><head><title>Local note</title></head><body><main><p>Read the <a href=\"./guide.html\">local guide</a>.</p></main></body></html>";
    std::fs::write(&path, html).unwrap();

    let result = read(Input::File(path), &ReadOptions::default())
        .await
        .unwrap();

    assert!(result.content.contains("(./guide.html)"));
    assert!(!result.content.contains("file://"));
    assert_eq!(
        result.source.resolved_url.as_ref().map(Url::scheme),
        Some("file")
    );
}

#[tokio::test]
async fn rejects_directories_as_file_inputs() {
    let directory = tempdir().unwrap();
    let path = directory.path().to_path_buf();

    let error = read(Input::File(path.clone()), &ReadOptions::default())
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        ReadError::NotRegularFile { path: rejected } if rejected == path
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_special_files() {
    let path = std::path::PathBuf::from("/dev/null");

    let error = read(Input::File(path.clone()), &ReadOptions::default())
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        ReadError::NotRegularFile { path: rejected } if rejected == path
    ));
}

#[tokio::test]
async fn enforces_the_byte_limit_for_regular_files() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("large.html");
    std::fs::write(&path, HTML.repeat(2)).unwrap();
    let options = ReadOptions {
        max_bytes: HTML.len(),
        ..ReadOptions::default()
    };

    let error = read(Input::File(path), &options).await.unwrap_err();

    assert!(matches!(
        error,
        ReadError::InputTooLarge { limit } if limit == HTML.len()
    ));
}

#[tokio::test]
async fn enforces_the_byte_limit_for_stdin() {
    let options = ReadOptions {
        max_bytes: HTML.len() - 1,
        ..ReadOptions::default()
    };

    let error = read(Input::Stdin(HTML.as_bytes().to_vec()), &options)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        ReadError::InputTooLarge { limit } if limit == HTML.len() - 1
    ));
}

#[tokio::test]
async fn rejects_non_html_stdin() {
    let error = read(
        Input::Stdin(b"plain text without markup".to_vec()),
        &ReadOptions::default(),
    )
    .await
    .unwrap_err();

    assert!(matches!(error, ReadError::NotHtml));
}

#[tokio::test]
async fn follows_redirects_and_decodes_declared_charsets() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/start"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/article"))
        .mount(&server)
        .await;

    let body = b"<!doctype html><html><head><title>Caf\xe9 report</title></head><body><article><p>The Caf\xe9 report contains enough visible text for extraction.</p></article></body></html>";
    Mock::given(method("GET"))
        .and(path("/article"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/html; charset=windows-1252")
                .set_body_bytes(body),
        )
        .mount(&server)
        .await;

    let requested = Url::parse(&format!("{}/start", server.uri())).unwrap();
    let result = read(Input::Url(requested.clone()), &ReadOptions::default())
        .await
        .unwrap();

    assert_eq!(result.source.kind, SourceKind::Url);
    assert_eq!(result.source.requested, requested.as_str());
    assert_eq!(
        result.source.resolved_url.as_ref().map(Url::path),
        Some("/article")
    );
    assert_eq!(result.source.charset, "windows-1252");
    assert!(result.content.contains("Café report"));
}

#[tokio::test]
async fn rejects_non_html_content_types_before_extraction() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/image"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes([137, 80, 78, 71]),
        )
        .mount(&server)
        .await;

    let url = Url::parse(&format!("{}/image", server.uri())).unwrap();
    let error = read(Input::Url(url), &ReadOptions::default())
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        ReadError::UnsupportedContentType(content_type) if content_type == "image/png"
    ));
}

#[tokio::test]
async fn enforces_the_http_response_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/large"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/html")
                .set_body_string(HTML.repeat(4)),
        )
        .mount(&server)
        .await;
    let options = ReadOptions {
        max_bytes: HTML.len(),
        ..ReadOptions::default()
    };

    let url = Url::parse(&format!("{}/large", server.uri())).unwrap();
    let error = read(Input::Url(url), &options).await.unwrap_err();

    assert!(matches!(
        error,
        ReadError::InputTooLarge { limit } if limit == HTML.len()
    ));
}

#[tokio::test]
async fn applies_the_total_request_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/slow"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/html")
                .set_body_string(HTML)
                .set_delay(Duration::from_millis(100)),
        )
        .mount(&server)
        .await;
    let options = ReadOptions {
        timeout: Duration::from_millis(20),
        ..ReadOptions::default()
    };

    let url = Url::parse(&format!("{}/slow", server.uri())).unwrap();
    let error = read(Input::Url(url), &options).await.unwrap_err();

    assert!(matches!(error, ReadError::Request { .. }));
}

#[tokio::test]
async fn rejects_non_http_url_schemes() {
    let url = Url::parse("file:///tmp/page.html").unwrap();
    let error = read(Input::Url(url), &ReadOptions::default())
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        ReadError::UnsupportedScheme(scheme) if scheme == "file"
    ));
}

#[tokio::test]
async fn rejects_initial_urls_with_embedded_credentials() {
    let url = Url::parse("https://reader:secret@example.test/article").unwrap();
    let error = read(Input::Url(url), &ReadOptions::default())
        .await
        .unwrap_err();

    assert!(matches!(error, ReadError::UrlContainsCredentials));
    assert!(!format!("{error:?}").contains("secret"));
}

#[tokio::test]
async fn rejects_redirect_targets_with_embedded_credentials_without_leaking_them() {
    let server = MockServer::start().await;
    let target = format!("http://reader:redirect-secret@{}/article", server.address());
    Mock::given(method("GET"))
        .and(path("/start"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", target.as_str()))
        .mount(&server)
        .await;

    let url = Url::parse(&format!("{}/start", server.uri())).unwrap();
    let error = read(Input::Url(url), &ReadOptions::default())
        .await
        .unwrap_err();
    let diagnostic = format!("{error:?}");

    assert!(matches!(error, ReadError::Request { .. }));
    assert!(!diagnostic.contains("reader"));
    assert!(!diagnostic.contains("redirect-secret"));
}
