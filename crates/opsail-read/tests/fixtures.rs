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

#[test]
fn recovers_lazy_and_noscript_images() {
    let result = fixture(
        "07-lazy-images.html",
        "https://example.test/guides/read/index.html",
    );

    for image in [
        "https://example.test/guides/read/images/pipeline-large.webp",
        "https://example.test/guides/media/deferred-map.png",
        "https://example.test/gallery/first.jpg",
        "https://example.test/gallery/second.jpg",
        "https://example.test/assets/static-fallback.svg",
    ] {
        assert!(
            result.content.contains(image),
            "missing recovered image: {image}"
        );
    }
    for loader_detail in ["data-src", "data:image", "<noscript"] {
        assert!(
            !result.content_html.contains(loader_detail),
            "loader detail survived: {loader_detail}"
        );
    }
    assert_markdown_matches_golden(&result, "07-lazy-images.md");
}

#[test]
fn normalizes_highlighted_code_without_line_numbers() {
    let result = fixture(
        "08-highlighted-code.html",
        "https://example.test/guides/code-layouts",
    );

    assert!(result.content.contains("```rust\nfn total"));
    assert!(result.content.contains("```python\ndef greet"));
    assert!(result.content.contains("values.iter().sum()"));
    assert!(result.content.contains("return f\"Hello, {name}\""));
    for gutter_text in ["10def greet", "11    return", "| ```"] {
        assert!(
            !result.content.contains(gutter_text),
            "code gutter survived: {gutter_text}"
        );
    }
    assert_markdown_matches_golden(&result, "08-highlighted-code.md");
}

#[test]
fn keeps_prose_around_multiple_highlighted_code_blocks() {
    let result = fixture(
        "09-multiple-code-blocks.html",
        "https://example.test/notes/two-compilers",
    );

    for value in [
        "First, inspect the source function",
        "auto process_values",
        "Next, compare the generated assembly",
        "vpandn xmm2",
        "same operation",
    ] {
        assert!(result.content.contains(value), "missing content: {value}");
    }
    assert_markdown_matches_golden(&result, "09-multiple-code-blocks.md");
}

#[test]
fn reveals_the_script_initialized_wechat_article_body() {
    let result = fixture(
        "10-wechat-hidden-content.html",
        "https://mp.weixin.qq.com/s/random-fixture",
    );

    for value in [
        "蓝色杯子、三张空白卡片",
        "纸风车转了两圈",
        "橘子、折尺、旧信封",
        "潮湿、方格、慢速和北边",
        "https://mp.weixin.qq.com/images/random-placeholder.png",
    ] {
        assert!(
            result.content.contains(value),
            "missing WeChat content: {value}"
        );
    }
    assert!(!result.content.contains("环境异常"));
    assert_markdown_matches_golden(&result, "10-wechat-hidden-content.md");

    let unrelated_origin = fixture(
        "10-wechat-hidden-content.html",
        "https://example.test/copied-markup",
    );
    assert!(
        !unrelated_origin.content.contains("蓝色杯子、三张空白卡片"),
        "script-hidden content must remain hidden outside the WeChat origin"
    );
}

#[test]
fn recovers_a_hidden_semantic_root_when_visible_extraction_is_thin() {
    let result = fixture(
        "11-hidden-semantic-root.html",
        "https://example.test/fragments/random-doorplate",
    );

    for value in [
        "桌角放着一枚绿色棋子",
        "十二号抽屉里依次装着玻璃珠",
        "used hidden-content fallback",
    ] {
        let haystack = if value.starts_with("used ") {
            result.warnings.join("\n")
        } else {
            result.content.clone()
        };
        assert!(haystack.contains(value), "missing recovered value: {value}");
    }
    for noise in [
        "FPS: --",
        "RANDOM FOOTER",
        "NESTED_HIDDEN_TEXT_MUST_NOT_APPEAR",
        "SCRIPT_TEXT_MUST_NOT_APPEAR",
        "FORM_CONTROL_MUST_NOT_APPEAR",
    ] {
        assert!(
            !result.content.contains(noise),
            "page noise survived: {noise}"
        );
    }
    assert_markdown_matches_golden(&result, "11-hidden-semantic-root.md");
}

#[test]
fn does_not_recover_hidden_content_when_visible_article_is_substantial() {
    let base_url = Url::parse("https://example.test/visible-article").unwrap();
    let html = r#"<!doctype html><html><head><title>Visible article</title></head><body>
        <main><article>
          <h1>Visible article</h1>
          <p>The visible article contains enough ordinary prose to be selected without consulting hidden alternatives. It describes a wooden tray, two paper clips, a folded map, and a numbered card on a quiet desk.</p>
          <p>A second visible paragraph adds enough structure for a normal article while remaining unrelated to the concealed test material beside it.</p>
        </article></main>
        <section aria-hidden="true"><article><h1>HIDDEN_PROMPT_MUST_NOT_APPEAR</h1>
          <p>HIDDEN_PROMPT_MUST_NOT_APPEAR repeated inside a longer concealed article-shaped block that must never replace an already substantial visible result.</p>
          <p>HIDDEN_PROMPT_MUST_NOT_APPEAR remains excluded even when this alternative contains more raw characters than the ordinary visible article.</p>
        </article></section>
      </body></html>"#;

    let result = extract_html(html, Some(&base_url)).unwrap();

    assert!(result.content.contains("wooden tray"));
    assert!(!result.content.contains("HIDDEN_PROMPT_MUST_NOT_APPEAR"));
}

#[test]
fn does_not_recover_hidden_promotional_content_from_a_thin_page() {
    let base_url = Url::parse("https://example.test/thin-note").unwrap();
    let html = r#"<!doctype html><html><head><title>Thin note</title></head><body>
        <main><p>A short visible note.</p></main>
        <section class="advertisement" aria-hidden="true">
          <h1>HIDDEN_ADVERTISEMENT_MUST_NOT_APPEAR</h1>
          <p>This concealed promotional block deliberately contains long article-shaped prose, several ordinary sentences, and enough characters to look richer than the visible note.</p>
          <p>It must remain excluded because an advertisement marker is stronger evidence than its length or paragraph structure.</p>
        </section>
        <section class="modal" aria-hidden="true"><article>
          <h1>HIDDEN_MODAL_MUST_NOT_APPEAR</h1>
          <p>This concealed modal also contains long article-shaped prose and multiple meaningful blocks, but its user-interface role makes it an unsafe recovery candidate.</p>
          <p>It must remain excluded even when the ordinary visible result is short and no advertisement candidate is eligible.</p>
        </article></section>
      </body></html>"#;

    let result = extract_html(html, Some(&base_url)).unwrap();

    assert!(result.content.contains("A short visible note"));
    assert!(
        !result
            .content
            .contains("HIDDEN_ADVERTISEMENT_MUST_NOT_APPEAR")
    );
    assert!(!result.content.contains("HIDDEN_MODAL_MUST_NOT_APPEAR"));
}

#[test]
fn css_custom_properties_do_not_hide_visible_content() {
    let base_url = Url::parse("https://example.test/custom-properties").unwrap();
    let html = r#"<!doctype html><html><head><title>Custom properties</title></head><body>
        <main style="--footer-display: none; --panel-visibility: hidden">
          <h1>Custom properties</h1>
          <p>VISIBLE_CUSTOM_PROPERTY_CONTENT remains readable because CSS custom property names are not the display or visibility properties themselves.</p>
        </main>
      </body></html>"#;

    let result = extract_html(html, Some(&base_url)).unwrap();

    assert!(
        result
            .content
            .contains("remains readable because CSS custom property"),
        "unexpected content: {}",
        result.content
    );
}
