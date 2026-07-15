use std::path::PathBuf;

use opsail_read::{ExtractionMethod, ReadResult, extract_html};
use url::Url;

fn fixture(name: &str, base_url: &str) -> ReadResult {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    let html = std::fs::read_to_string(path).expect("fixture should be readable");
    let base_url = Url::parse(base_url).expect("fixture URL should be valid");
    extract_html(&html, Some(&base_url)).expect("fixture should extract")
}

fn assert_markdown_matches_golden(result: &ReadResult, name: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("expected")
        .join(name);
    let expected = std::fs::read_to_string(path).expect("golden file should be readable");

    assert_eq!(
        format!("{}\n", result.content),
        expected,
        "extracted Markdown should match the reviewed golden file"
    );
}

#[test]
fn extracts_article_and_discards_page_furniture() {
    let result = fixture(
        "01-article-noise.html",
        "https://example.test/journal/field-notes/quiet-tools",
    );

    assert!(result.content.starts_with("# Quiet Tools for Careful Work"));
    assert_eq!(result.metadata.author.as_deref(), Some("Mira Chen"));
    assert!(result.content.contains("## A stable handoff"));
    assert!(result.content.contains("Keep content order predictable."));
    assert!(!result.content.contains("This recommendation panel"));
    assert!(!result.content.contains("Copyright notice"));
    assert!(!result.content.contains("Accept"));
    assert_markdown_matches_golden(&result, "01-article-noise.md");
}

#[test]
fn prefers_structured_article_metadata() {
    let result = fixture(
        "02-metadata-jsonld-og.html",
        "https://example.test/reports/river-observatory-reopens",
    );

    assert_eq!(
        result.metadata.title,
        "River Observatory Reopens After Spring Repairs"
    );
    assert_eq!(
        result.content.lines().next(),
        Some("# River Observatory Reopens After Spring Repairs")
    );
    assert_eq!(result.metadata.author.as_deref(), Some("Mira Chen"));
    assert_eq!(
        result.metadata.site.as_deref(),
        Some("Example Test Journal")
    );
    assert_eq!(
        result.metadata.published.as_deref(),
        Some("2026-07-11T17:40:00+08:00")
    );
    assert_eq!(
        result.metadata.modified.as_deref(),
        Some("2026-07-12T07:05:00+08:00")
    );
    assert_eq!(
        result.metadata.canonical_url.as_deref(),
        Some("https://example.test/reports/river-observatory-reopens")
    );
    assert_eq!(result.metadata.domain.as_deref(), Some("example.test"));
    assert_markdown_matches_golden(&result, "02-metadata-jsonld-og.md");
}

#[test]
fn preserves_code_and_resolves_relative_assets() {
    let result = fixture(
        "03-code-relative-assets.html",
        "https://example.test/guides/cli/index.html",
    );

    assert!(result.content.contains("use std::io::{self, Read};"));
    assert!(result.content.contains("fn main()"));
    assert!(result.content_html.contains("language-rust"));
    assert!(
        result
            .content
            .contains("https://example.test/guides/cli/images/input-to-text.png")
    );
    assert!(
        result
            .content
            .contains("https://example.test/guides/reference/output-contract.html")
    );
    assert!(
        result
            .content
            .contains("https://example.test/policies/untrusted-input")
    );
    assert!(
        result
            .content
            .contains("https://example.test/guides/cli/index.html#verification")
    );
    assert_markdown_matches_golden(&result, "03-code-relative-assets.md");
}

#[test]
fn converts_regular_and_irregular_tables_without_losing_values() {
    let result = fixture(
        "04-tables-regular-irregular.html",
        "https://example.test/reference/seasonal-readings",
    );

    for value in [
        "Regular monthly readings",
        "April",
        "82 mm",
        "June",
        "Irregular maintenance record",
        "Work windows and shared notes",
        "Morning",
        "14.2",
        "Afternoon",
        "Evening reading postponed",
        "Path closed",
    ] {
        assert!(
            result.content.contains(value),
            "missing table value: {value}"
        );
    }
    for glued in ["BeforeAfter", "Morning14.2", "postponedPath"] {
        assert!(
            !result.content.contains(glued),
            "glued table cells: {glued}"
        );
    }
    assert!(result.content.contains("Morning · 14.2 · 14.6"));
    assert!(
        result
            .content
            .contains("Evening reading postponed · Path closed")
    );
    assert_markdown_matches_golden(&result, "04-tables-regular-irregular.md");
}

#[test]
fn produces_inert_output_from_untrusted_markup() {
    let result = fixture(
        "05-unsafe-hidden-content.html",
        "https://example.test/security/untrusted-page",
    );

    assert!(result.content.contains("The visible article explains"));
    assert!(result.content.contains("meaningful inline text"));
    assert!(
        result
            .content
            .contains("https://example.test/safety/allowed")
    );
    for unwanted in [
        "SCRIPT_TEXT_MUST_NOT_APPEAR",
        "HIDDEN_ATTRIBUTE_TEXT_MUST_NOT_APPEAR",
        "ARIA_HIDDEN_TEXT_MUST_NOT_APPEAR",
        "DISPLAY_NONE_TEXT_MUST_NOT_APPEAR",
        "VISIBILITY_HIDDEN_TEXT_MUST_NOT_APPEAR",
        "HIDDEN_CLASS_TEXT_MUST_NOT_APPEAR",
        "UPPERCASE_ARIA_HIDDEN_MUST_NOT_APPEAR",
        "UPPERCASE_DISPLAY_NONE_MUST_NOT_APPEAR",
        "UPPERCASE_HIDDEN_CLASS_MUST_NOT_APPEAR",
        "ADVERTISEMENT_TEXT_MUST_NOT_APPEAR",
        "SECOND_ADVERTISEMENT_MUST_NOT_APPEAR",
        "javascript:",
        "onclick",
        "onmouseover",
        "onerror",
    ] {
        assert!(
            !result.content.contains(unwanted) && !result.content_html.contains(unwanted),
            "unsafe or hidden value survived: {unwanted}"
        );
    }
    assert_markdown_matches_golden(&result, "05-unsafe-hidden-content.md");
}

#[test]
fn keeps_short_cjk_content_via_semantic_fallback() {
    let result = fixture(
        "06-short-cjk-main.html",
        "https://example.test/zh/notes/valley-rain",
    );

    assert!(result.content.contains("# 山谷雨讯"));
    assert!(result.content.contains("清晨有小雨，石桥湿滑。"));
    assert!(
        result
            .content
            .contains("https://example.test/zh/maps/trail")
    );
    assert!(!result.content.contains("热门链接"));
    assert!(!result.content.contains("隐私说明"));
    assert_eq!(result.metadata.language.as_deref(), Some("zh-CN"));
    assert_eq!(result.extraction.method, ExtractionMethod::Semantic);
    assert_eq!(
        result
            .content
            .lines()
            .filter(|line| line.starts_with("# "))
            .count(),
        1
    );
    assert_eq!(result.content.lines().next(), Some("# 山谷雨讯"));
    assert_markdown_matches_golden(&result, "06-short-cjk-main.md");
}
