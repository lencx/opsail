use ammonia::{Builder, UrlRelative};
use dom_query::{Document, Selection};
use dom_smoothie::{Article, CandidateSelectMode, Config, Metadata, Readability, TextMode};
use unicode_segmentation::UnicodeSegmentation;
use url::Url;

use crate::error::ReadError;
use crate::model::{DEFAULT_MAX_DEPTH, DEFAULT_MAX_ELEMENTS, DocumentMetadata, ExtractionMethod};
use crate::standardize::standardize_document;

const SEMANTIC_FALLBACK_THRESHOLD: usize = 120;
const SEMANTIC_FALLBACK_MIN_GAIN: usize = 120;
const HIDDEN_FALLBACK_WORD_THRESHOLD: usize = 50;
const HIDDEN_FALLBACK_MIN_WORDS: usize = 30;
const HIDDEN_FALLBACK_MAX_LINK_PERCENT: usize = 35;

pub(crate) struct Extracted {
    pub content: String,
    pub content_html: String,
    pub text: String,
    pub metadata: DocumentMetadata,
    pub method: ExtractionMethod,
    pub probably_readable: bool,
    pub warnings: Vec<String>,
}

struct Candidate {
    content_html: String,
    text: String,
    metadata: Metadata,
    method: ExtractionMethod,
    probably_readable: bool,
}

pub(crate) fn extract(html: &str, base_url: Option<&Url>) -> Result<Extracted, ReadError> {
    let prepared_html = prepare_for_extraction(html, base_url)?;
    let fallback_metadata = read_metadata(html, base_url)?;
    let primary = read_candidate(
        &prepared_html,
        base_url,
        CandidateSelectMode::Readability,
        ExtractionMethod::Readability,
    );
    let expanded = read_candidate(
        &prepared_html,
        base_url,
        CandidateSelectMode::DomSmoothie,
        ExtractionMethod::Expanded,
    );

    let first_error = primary.as_ref().err().map(ToString::to_string);
    let mut candidate = match (primary, expanded) {
        (Ok(primary), Ok(expanded)) => choose_richer(primary, expanded),
        (Ok(primary), Err(_)) => primary,
        (Err(_), Ok(expanded)) => expanded,
        (Err(error), Err(_)) => {
            return semantic_candidate(&prepared_html, fallback_metadata)
                .ok_or(error)
                .and_then(|candidate| finish_candidate(candidate, base_url));
        }
    };

    candidate.metadata = merge_metadata(fallback_metadata.clone(), candidate.metadata);
    let mut semantic_was_substantially_richer = false;
    let candidate_characters = visible_characters(&candidate.text);
    if let Some(semantic) = semantic_candidate(&prepared_html, fallback_metadata.clone()) {
        let semantic_characters = visible_characters(&semantic.text);
        let preserves_more_media = semantic_preserves_substantially_more_media(
            &candidate,
            candidate_characters,
            &semantic,
            semantic_characters,
        );
        if candidate_characters < SEMANTIC_FALLBACK_THRESHOLD
            && semantic_characters > candidate_characters
        {
            candidate = semantic;
        } else if semantic_is_substantially_richer(candidate_characters, semantic_characters)
            || preserves_more_media
        {
            candidate = semantic;
            semantic_was_substantially_richer = true;
        }
    }

    let mut extracted = finish_candidate(candidate, base_url)?;
    if extracted.method == ExtractionMethod::Semantic {
        let warning = if semantic_was_substantially_richer {
            "used semantic fallback because it preserved substantially more content"
        } else {
            "used semantic fallback because article scoring returned thin content"
        };
        extracted.warnings.push(warning.to_owned());
        if let Some(error) = first_error {
            tracing::debug!(error, "article scoring failed before semantic fallback");
        }
    }

    if should_try_hidden_fallback(&extracted)
        && let Some(hidden_candidate) =
            hidden_semantic_candidate(html, base_url, fallback_metadata)?
        && let Ok(mut recovered) = finish_candidate(hidden_candidate, base_url)
        && hidden_fallback_is_substantially_richer(&extracted, &recovered)
    {
        recovered.warnings.push(
            "used hidden-content fallback because visible extraction was unusually short"
                .to_owned(),
        );
        return Ok(recovered);
    }
    Ok(extracted)
}

fn read_candidate(
    html: &str,
    base_url: Option<&Url>,
    mode: CandidateSelectMode,
    method: ExtractionMethod,
) -> Result<Candidate, ReadError> {
    let mut reader = Readability::new(html, base_url.map(Url::as_str), Some(config(mode)))
        .map_err(ReadError::Extraction)?;
    let probably_readable = reader.is_probably_readable();
    let article = reader.parse().map_err(ReadError::Extraction)?;
    Ok(candidate_from_article(article, method, probably_readable))
}

fn read_metadata(html: &str, base_url: Option<&Url>) -> Result<Metadata, ReadError> {
    let reader = Readability::new(
        html,
        base_url.map(Url::as_str),
        Some(config(CandidateSelectMode::Readability)),
    )
    .map_err(ReadError::Extraction)?;
    let json_ld = reader.parse_json_ld();
    Ok(reader.get_article_metadata(json_ld))
}

fn config(mode: CandidateSelectMode) -> Config {
    Config {
        keep_classes: true,
        max_elements_to_parse: DEFAULT_MAX_ELEMENTS,
        candidate_select_mode: mode,
        text_mode: TextMode::Raw,
        ..Config::default()
    }
}

fn prepare_for_extraction(html: &str, base_url: Option<&Url>) -> Result<String, ReadError> {
    let document = Document::from(html);
    validate_document(&document)?;
    standardize_document(&document, base_url);
    validate_document(&document)?;
    for selector in [
        "script",
        "style",
        "noscript",
        "template",
        "iframe",
        "form",
        "dialog",
        "[hidden]",
        ".advertisement",
        ".advert",
        ".promoted-content",
    ] {
        document.select(selector).remove();
    }
    remove_hidden_elements(&document);
    Ok(document.root().html().to_string())
}

fn validate_document(document: &Document) -> Result<(), ReadError> {
    let mut stack = vec![(document.root(), 0_usize)];
    let mut elements = 0_usize;
    while let Some((node, depth)) = stack.pop() {
        if depth > DEFAULT_MAX_DEPTH {
            return Err(ReadError::DocumentTooDeep {
                limit: DEFAULT_MAX_DEPTH,
            });
        }
        if node.is_element() {
            elements += 1;
            if elements > DEFAULT_MAX_ELEMENTS {
                return Err(ReadError::TooManyElements {
                    found: elements,
                    limit: DEFAULT_MAX_ELEMENTS,
                });
            }
        }
        stack.extend(node.children_it(false).map(|child| (child, depth + 1)));
    }
    Ok(())
}

fn remove_hidden_elements(document: &Document) {
    for selection in document.select("[aria-hidden]").iter() {
        if selection
            .attr("aria-hidden")
            .is_some_and(|value| value.as_ref().eq_ignore_ascii_case("true"))
        {
            selection.remove();
        }
    }
    for selection in document.select("[style]").iter() {
        let hidden = selection
            .attr("style")
            .is_some_and(|value| style_hides_element(value.as_ref()));
        if hidden {
            selection.remove();
        }
    }
    for selection in document.select("[class]").iter() {
        let hidden = selection.attr("class").is_some_and(|value| {
            value.split_ascii_whitespace().any(|class| {
                matches!(
                    class.to_ascii_lowercase().as_str(),
                    "hidden" | "concealed" | "visually-hidden" | "sr-only"
                )
            })
        });
        if hidden {
            selection.remove();
        }
    }
}

fn style_hides_element(style: &str) -> bool {
    style.split(';').any(|declaration| {
        let Some((property, value)) = declaration.split_once(':') else {
            return false;
        };
        let property = property.trim();
        let value = value
            .split_once('!')
            .map_or(value, |(value, _)| value)
            .trim();
        (property.eq_ignore_ascii_case("display") && value.eq_ignore_ascii_case("none"))
            || (property.eq_ignore_ascii_case("visibility") && value.eq_ignore_ascii_case("hidden"))
    })
}

fn should_try_hidden_fallback(extracted: &Extracted) -> bool {
    visible_characters(&extracted.text) < SEMANTIC_FALLBACK_THRESHOLD
        || extracted.text.unicode_words().count() < HIDDEN_FALLBACK_WORD_THRESHOLD
}

fn hidden_semantic_candidate(
    html: &str,
    base_url: Option<&Url>,
    metadata: Metadata,
) -> Result<Option<Candidate>, ReadError> {
    let document = Document::from(html);
    validate_document(&document)?;
    standardize_document(&document, base_url);
    validate_document(&document)?;

    let best = document
        .select("[hidden], [aria-hidden], [class]")
        .iter()
        .filter(is_hidden_fallback_root)
        .filter(|candidate| hidden_candidate_has_article_structure(candidate))
        .filter(|candidate| !hidden_candidate_has_clutter_context(candidate))
        .filter(|candidate| {
            hidden_candidate_link_density(candidate) <= HIDDEN_FALLBACK_MAX_LINK_PERCENT
        })
        .map(|candidate| {
            let score = visible_characters(candidate.text().as_ref());
            (score, candidate)
        })
        .filter(|(characters, candidate)| {
            *characters >= SEMANTIC_FALLBACK_THRESHOLD
                && candidate.text().unicode_words().count() >= HIDDEN_FALLBACK_MIN_WORDS
        })
        .max_by_key(|(score, _)| *score)
        .map(|(_, candidate)| candidate);

    let Some(candidate) = best else {
        return Ok(None);
    };
    let prepared = prepare_for_extraction(candidate.inner_html().as_ref(), base_url)?;
    Ok(semantic_candidate(&prepared, metadata))
}

fn is_hidden_fallback_root(candidate: &Selection<'_>) -> bool {
    candidate.has_attr("hidden")
        || candidate
            .attr("aria-hidden")
            .is_some_and(|value| value.as_ref().eq_ignore_ascii_case("true"))
        || candidate.attr("class").is_some_and(|classes| {
            classes
                .split_ascii_whitespace()
                .any(|class| matches!(class.to_ascii_lowercase().as_str(), "hidden" | "invisible"))
        })
}

fn hidden_candidate_has_article_structure(candidate: &Selection<'_>) -> bool {
    let has_semantic_root = candidate.is("article, main, [role='main']")
        || candidate.select("article, main, [role='main']").exists();
    let has_heading = candidate
        .select("h1, h2, h3")
        .iter()
        .any(|heading| visible_characters(heading.text().as_ref()) > 1);
    let meaningful_blocks = candidate
        .select("p, li, pre, blockquote, td")
        .iter()
        .filter(|block| visible_characters(block.text().as_ref()) >= 20)
        .take(2)
        .count();
    has_semantic_root && has_heading && meaningful_blocks >= 2
}

fn hidden_candidate_has_clutter_context(candidate: &Selection<'_>) -> bool {
    std::iter::once(candidate.clone())
        .chain(candidate.ancestors(Some(32)).iter())
        .any(|node| {
            node.is(
                "nav, aside, footer, header, form, template, svg, math, mjx-container, \
                 .katex-html, .MathJax, [role='menu'], [role='navigation'], \
                 [role='tooltip'], [role='status'], [role='alert']",
            ) || ["class", "id"]
                .iter()
                .filter_map(|attribute| node.attr(attribute))
                .flat_map(|value| {
                    value
                        .split(|character: char| !character.is_ascii_alphanumeric())
                        .map(str::to_ascii_lowercase)
                        .collect::<Vec<_>>()
                })
                .any(|token| {
                    matches!(
                        token.as_str(),
                        "ad" | "ads"
                            | "advert"
                            | "advertisement"
                            | "cookie"
                            | "modal"
                            | "newsletter"
                            | "popover"
                            | "promo"
                            | "promoted"
                            | "sidebar"
                            | "social"
                    )
                })
        })
}

fn hidden_candidate_link_density(candidate: &Selection<'_>) -> usize {
    let characters = visible_characters(candidate.text().as_ref()).max(1);
    let link_characters = candidate
        .select("a")
        .iter()
        .map(|link| visible_characters(link.text().as_ref()))
        .sum::<usize>();
    link_characters.saturating_mul(100) / characters
}

fn hidden_fallback_is_substantially_richer(current: &Extracted, alternative: &Extracted) -> bool {
    let current_characters = visible_characters(&current.text);
    let alternative_characters = visible_characters(&alternative.text);
    let current_words = current.text.unicode_words().count();
    let alternative_words = alternative.text.unicode_words().count();
    alternative_characters >= SEMANTIC_FALLBACK_THRESHOLD
        && alternative_words >= HIDDEN_FALLBACK_MIN_WORDS
        && alternative_characters > current_characters.saturating_mul(2)
        && alternative_words > current_words.saturating_mul(2)
}

fn candidate_from_article(
    article: Article,
    method: ExtractionMethod,
    probably_readable: bool,
) -> Candidate {
    let metadata = Metadata {
        title: article.title,
        byline: article.byline,
        excerpt: article.excerpt,
        site_name: article.site_name,
        published_time: article.published_time,
        modified_time: article.modified_time,
        image: article.image,
        favicon: article.favicon,
        lang: article.lang,
        url: article.url,
        dir: article.dir,
    };
    Candidate {
        content_html: article.content.to_string(),
        text: article.text_content.to_string(),
        metadata,
        method,
        probably_readable,
    }
}

fn choose_richer(primary: Candidate, expanded: Candidate) -> Candidate {
    let primary_chars = visible_characters(&primary.text);
    let expanded_chars = visible_characters(&expanded.text);
    if expanded_chars > primary_chars.saturating_add(primary_chars / 4) {
        expanded
    } else {
        primary
    }
}

fn semantic_candidate(html: &str, metadata: Metadata) -> Option<Candidate> {
    let document = Document::from(html);
    for selector in [
        "script",
        "style",
        "noscript",
        "template",
        "nav",
        "aside",
        "footer",
        "header",
        "form",
        "dialog",
        "[hidden]",
        "[aria-hidden=\"true\"]",
        ".advertisement",
        ".advert",
        ".sidebar",
        ".cookie",
        ".newsletter",
        ".social",
    ] {
        document.select(selector).remove();
    }

    let mut best: Option<(usize, String, String)> = None;
    for selector in ["article", "main", "[role=\"main\"]"] {
        for selection in document.select(selector).iter() {
            let text = selection.text().trim().to_owned();
            let score = visible_characters(&text);
            if score > best.as_ref().map_or(0, |candidate| candidate.0) {
                best = Some((score, selection.html().to_string(), text));
            }
        }
    }

    if best.is_none() {
        let body = document.select("body");
        let text = body.text().trim().to_owned();
        if visible_characters(&text) > 0 {
            best = Some((visible_characters(&text), body.html().to_string(), text));
        }
    }

    best.map(|(_, content_html, text)| Candidate {
        content_html,
        text,
        metadata,
        method: ExtractionMethod::Semantic,
        probably_readable: false,
    })
}

fn finish_candidate(candidate: Candidate, base_url: Option<&Url>) -> Result<Extracted, ReadError> {
    let metadata = document_metadata(candidate.metadata, base_url);
    let (content_html, markdown) = sanitize_and_convert(&candidate.content_html, base_url);
    let content = ensure_document_title(normalize_markdown(&markdown), &metadata.title);
    if content.trim().is_empty() {
        return Err(ReadError::NoContent);
    }
    Ok(Extracted {
        content,
        text: Document::from(content_html.as_str())
            .root()
            .formatted_text()
            .to_string(),
        content_html,
        metadata,
        method: candidate.method,
        probably_readable: candidate.probably_readable,
        warnings: Vec::new(),
    })
}

fn sanitize_and_convert(html: &str, base_url: Option<&Url>) -> (String, String) {
    let mut builder = Builder::default();
    builder
        .add_tags(&["tfoot"])
        .add_tag_attributes("code", &["class", "data-lang"])
        .add_tag_attributes("pre", &["class"]);
    if let Some(base_url) = base_url {
        builder.url_relative(UrlRelative::RewriteWithBase(base_url.clone()));
    }
    let content_html = builder.clean(html).to_string();
    let document = Document::from(content_html.as_str());
    remove_credentialed_urls(&document);
    for image in document.select("img").iter() {
        if image
            .attr("src")
            .is_none_or(|source| source.trim().is_empty())
        {
            image.remove();
        }
    }
    for table in document.select("table").iter() {
        if !table.select("td").exists() && !table.select("th").exists() {
            table.remove();
        }
    }
    let content_html = strip_unsafe_controls(document.root().html().as_ref());
    let markdown_document = Document::from(content_html.as_str());
    for table in markdown_document.select("table").iter() {
        let caption = table.select("caption");
        let caption_text = caption.text().trim().to_owned();
        let is_irregular = table.select("[rowspan], [colspan]").exists();
        let mut readable_rows = String::from("<div>");
        if is_irregular {
            for row in table.select("tr").iter() {
                let is_header = row.select("td").is_empty();
                let cells: Vec<String> = row
                    .select("th, td")
                    .iter()
                    .map(|cell| {
                        cell.formatted_text()
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .filter(|cell| !cell.is_empty())
                    .collect();
                if cells.is_empty() {
                    continue;
                }
                let row_text = escape_html(&cells.join(" · "));
                if is_header {
                    readable_rows.push_str(&format!("<p><strong>{row_text}</strong></p>"));
                } else {
                    readable_rows.push_str(&format!("<p>{row_text}</p>"));
                }
            }
        }
        readable_rows.push_str("</div>");
        if !caption_text.is_empty() {
            table.before_html(format!(
                "<p><strong>{}</strong></p>",
                escape_html(&caption_text)
            ));
        }
        caption.remove();
        if is_irregular {
            table.replace_with_html(readable_rows);
        }
    }
    let markdown = strip_unsafe_controls(markdown_document.md(None).as_ref());
    (content_html, markdown)
}

fn remove_credentialed_urls(document: &Document) {
    for (selector, attribute) in [("[href]", "href"), ("[src]", "src"), ("[cite]", "cite")] {
        for selection in document.select(selector).iter() {
            let has_credentials = selection
                .attr(attribute)
                .and_then(|value| Url::parse(value.as_ref()).ok())
                .is_some_and(|url| url_has_credentials(&url));
            if has_credentials {
                selection.remove_attr(attribute);
            }
        }
    }
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn document_metadata(metadata: Metadata, base_url: Option<&Url>) -> DocumentMetadata {
    DocumentMetadata {
        title: clean_text(&metadata.title),
        author: clean_author(metadata.byline),
        description: clean_option(metadata.excerpt),
        site: clean_option(metadata.site_name),
        published: clean_option(metadata.published_time),
        modified: clean_option(metadata.modified_time),
        image: clean_web_url(metadata.image, base_url),
        favicon: clean_web_url(metadata.favicon, base_url),
        language: clean_option(metadata.lang),
        direction: clean_option(metadata.dir),
        canonical_url: clean_web_url(metadata.url, base_url),
        domain: None,
    }
}

fn clean_author(value: Option<String>) -> Option<String> {
    clean_option(value).map(|value| {
        for prefix in ["By ", "by ", "Written by ", "written by "] {
            if let Some(author) = value.strip_prefix(prefix) {
                return author.trim().to_owned();
            }
        }
        value
    })
}

fn merge_metadata(mut preferred: Metadata, fallback: Metadata) -> Metadata {
    if clean_text(&preferred.title).is_empty() {
        preferred.title = fallback.title;
    }
    preferred.byline = prefer_nonempty(preferred.byline, fallback.byline);
    preferred.excerpt = prefer_nonempty(preferred.excerpt, fallback.excerpt);
    preferred.site_name = prefer_nonempty(preferred.site_name, fallback.site_name);
    preferred.published_time = prefer_nonempty(preferred.published_time, fallback.published_time);
    preferred.modified_time = prefer_nonempty(preferred.modified_time, fallback.modified_time);
    preferred.image = prefer_nonempty(preferred.image, fallback.image);
    preferred.favicon = prefer_nonempty(preferred.favicon, fallback.favicon);
    preferred.lang = prefer_nonempty(preferred.lang, fallback.lang);
    preferred.url = prefer_nonempty(preferred.url, fallback.url);
    preferred.dir = prefer_nonempty(preferred.dir, fallback.dir);
    preferred
}

fn clean_option(value: Option<String>) -> Option<String> {
    value
        .map(|value| clean_text(&value))
        .filter(|value| !value.is_empty())
}

fn clean_text(value: &str) -> String {
    strip_unsafe_controls(value)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn strip_unsafe_controls(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .collect()
}

fn prefer_nonempty(preferred: Option<String>, fallback: Option<String>) -> Option<String> {
    clean_option(preferred).or_else(|| clean_option(fallback))
}

fn clean_web_url(value: Option<String>, base_url: Option<&Url>) -> Option<String> {
    let value = clean_option(value)?;
    let url = match Url::parse(&value) {
        Ok(url) => url,
        Err(url::ParseError::RelativeUrlWithoutBase) => base_url?.join(&value).ok()?,
        Err(_) => return None,
    };
    (matches!(url.scheme(), "http" | "https") && !url_has_credentials(&url))
        .then(|| url.to_string())
}

fn url_has_credentials(url: &Url) -> bool {
    !url.username().is_empty() || url.password().is_some()
}

fn ensure_document_title(markdown: String, title: &str) -> String {
    let title = title.trim();
    let safe_title = escape_markdown_title(title);
    let mut lines: Vec<String> = markdown.trim().lines().map(ToOwned::to_owned).collect();
    if lines.is_empty() {
        return if title.is_empty() {
            String::new()
        } else {
            format!("# {safe_title}")
        };
    }

    let first_content = lines.iter().position(|line| !line.trim().is_empty());
    let mut title_line = first_content.and_then(|index| {
        let (level, heading) = atx_heading(&lines[index])?;
        if level == 1 {
            Some(index)
        } else if !title.is_empty()
            && (heading.eq_ignore_ascii_case(title) || heading.eq_ignore_ascii_case(&safe_title))
        {
            lines[index] = format!("# {heading}");
            Some(index)
        } else {
            None
        }
    });

    if title_line.is_none() {
        title_line = first_h1_line(&lines);
    }
    if title_line.is_none() && !title.is_empty() {
        lines.insert(0, String::new());
        lines.insert(0, format!("# {safe_title}"));
        title_line = Some(0);
    }

    let mut in_fence = false;
    for (index, line) in lines.iter_mut().enumerate() {
        if is_fence(line) {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || Some(index) == title_line {
            continue;
        }
        if let Some((1, heading)) = atx_heading(line) {
            *line = format!("## {heading}");
        }
    }
    lines.join("\n").trim().to_owned()
}

fn first_h1_line(lines: &[String]) -> Option<usize> {
    let mut in_fence = false;
    for (index, line) in lines.iter().enumerate() {
        if is_fence(line) {
            in_fence = !in_fence;
        } else if !in_fence && atx_heading(line).is_some_and(|heading| heading.0 == 1) {
            return Some(index);
        }
    }
    None
}

fn escape_markdown_title(title: &str) -> String {
    let mut output = String::with_capacity(title.len());
    for character in title.chars() {
        if character as u32 == 96
            || matches!(
                character,
                '\\' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '!' | '|'
            )
        {
            output.push('\\');
        }
        output.push(character);
    }
    output
}

fn is_fence(line: &str) -> bool {
    let line = line.trim_start();
    line.as_bytes().starts_with(&[96, 96, 96]) || line.starts_with("~~~")
}

fn atx_heading(line: &str) -> Option<(usize, String)> {
    let line = line.trim_start();
    let level = line
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let heading = line.get(level..)?.strip_prefix(' ')?.trim();
    (!heading.is_empty()).then(|| (level, heading.to_owned()))
}

fn normalize_markdown(markdown: &str) -> String {
    let mut in_fence = false;
    let mut previous_blank = false;
    let mut output = String::with_capacity(markdown.len());
    for line in markdown.lines() {
        if is_fence(line) {
            in_fence = !in_fence;
            previous_blank = false;
            output.push_str(line);
        } else if in_fence {
            previous_blank = false;
            output.push_str(line);
        } else if line.trim().is_empty() {
            if previous_blank {
                continue;
            }
            previous_blank = true;
        } else {
            previous_blank = false;
            output.push_str(&unescape_prose_line(line));
        }
        output.push('\n');
    }
    output.trim().to_owned()
}

fn unescape_prose_line(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut output = String::with_capacity(line.len());
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '\\'
            && let Some(next) = chars.get(index + 1).copied()
        {
            let previous = chars.get(index.wrapping_sub(1)).copied();
            let after = chars.get(index + 2).copied();
            let ordered_list_marker = next == '.'
                && previous.is_some_and(|character| character.is_ascii_digit())
                && chars[..index]
                    .iter()
                    .copied()
                    .skip_while(|character| character.is_whitespace())
                    .all(|character| character.is_ascii_digit());
            let image_marker = next == '!' && after == Some('[');
            if matches!(next, '.' | '!' | '(' | ')' | '{' | '}')
                && !ordered_list_marker
                && !image_marker
            {
                output.push(next);
                index += 2;
                continue;
            }
        }
        output.push(chars[index]);
        index += 1;
    }
    output
}

fn visible_characters(value: &str) -> usize {
    value
        .chars()
        .filter(|character| !character.is_whitespace())
        .count()
}

fn semantic_is_substantially_richer(current: usize, alternative: usize) -> bool {
    let required_gain = SEMANTIC_FALLBACK_MIN_GAIN.max(current.saturating_mul(3) / 4);
    alternative > current.saturating_add(required_gain)
}

fn semantic_preserves_substantially_more_media(
    current: &Candidate,
    current_characters: usize,
    alternative: &Candidate,
    alternative_characters: usize,
) -> bool {
    alternative_characters >= current_characters
        && meaningful_image_count(&alternative.content_html)
            >= meaningful_image_count(&current.content_html).saturating_add(2)
}

fn meaningful_image_count(html: &str) -> usize {
    Document::from(html)
        .select("img[src][alt]")
        .iter()
        .filter(|image| {
            image.attr("alt").is_some_and(|alt| !alt.trim().is_empty())
                && image
                    .attr("src")
                    .is_some_and(|source| is_safe_image_source(source.as_ref()))
        })
        .count()
}

fn is_safe_image_source(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_control) {
        return false;
    }
    if value.starts_with("//") {
        return Url::parse(&format!("https:{value}")).is_ok_and(|url| !url_has_credentials(&url));
    }
    match Url::parse(value) {
        Ok(url) => matches!(url.scheme(), "http" | "https") && !url_has_credentials(&url),
        Err(url::ParseError::RelativeUrlWithoutBase) => true,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prose_punctuation_is_not_over_escaped() {
        assert_eq!(
            normalize_markdown(
                r"Version 2\.0 \(stable\)\!

1\. Keep this marker.

\![Keep this image](image.png)"
            ),
            "Version 2.0 (stable)!\n\n1\\. Keep this marker.\n\n\\![Keep this image](image.png)"
        );
    }

    #[test]
    fn adds_a_missing_document_title() {
        assert_eq!(
            ensure_document_title("Paragraph.".to_owned(), "A title"),
            "# A title\n\nParagraph."
        );
    }

    #[test]
    fn promotes_a_matching_lower_level_title() {
        assert_eq!(
            ensure_document_title("## A title\n\nParagraph.".to_owned(), "A title"),
            "# A title\n\nParagraph."
        );
    }

    #[test]
    fn keeps_the_visible_h1_when_metadata_has_a_suffix() {
        assert_eq!(
            ensure_document_title("# A title\n\nParagraph.".to_owned(), "A title | Site"),
            "# A title\n\nParagraph."
        );
    }

    #[test]
    fn demotes_additional_h1_headings() {
        assert_eq!(
            ensure_document_title("# A title\n\n# Section".to_owned(), "A title"),
            "# A title\n\n## Section"
        );
    }

    #[test]
    fn sanitizer_removes_active_content() {
        let (html, markdown) =
            sanitize_and_convert("<main><script>alert(1)</script><p>Safe</p></main>", None);
        assert!(!html.contains("script"));
        assert!(!markdown.contains("alert"));
        assert!(markdown.contains("Safe"));
    }

    #[test]
    fn strips_terminal_control_characters_from_published_content() {
        let (html, markdown) = sanitize_and_convert(
            "<main><p>Safe&#x1b;]2;forged title&#x7; text.</p></main>",
            None,
        );

        for output in [&html, &markdown] {
            assert!(!output.contains('\u{1b}'));
            assert!(!output.contains('\u{7}'));
            assert!(output.contains("Safe"));
            assert!(output.contains("forged title"));
        }
        assert_eq!(
            clean_text("Safe\u{1b}]2;title\u{7} text"),
            "Safe]2;title text"
        );
    }

    #[test]
    fn title_metadata_cannot_inject_markdown_structure() {
        assert_eq!(
            ensure_document_title(
                "Paragraph.".to_owned(),
                "Safe [link](javascript:alert(1)) # forged"
            ),
            "# Safe \\[link\\](javascript:alert(1)) \\# forged\n\nParagraph."
        );
    }

    #[test]
    fn unsafe_metadata_urls_are_removed() {
        let base = Url::parse("https://example.test/article").unwrap();
        assert_eq!(
            clean_web_url(Some("/image.png".to_owned()), Some(&base)).as_deref(),
            Some("https://example.test/image.png")
        );
        assert_eq!(
            clean_web_url(Some("javascript:alert(1)".to_owned()), Some(&base)),
            None
        );
        assert_eq!(
            clean_web_url(Some("data:text/html,bad".to_owned()), Some(&base)),
            None
        );
        assert_eq!(
            clean_web_url(
                Some("https://reader:secret@example.test/image.png".to_owned()),
                Some(&base)
            ),
            None
        );
    }

    #[test]
    fn sanitizer_removes_embedded_credentials_from_resource_urls() {
        let (html, markdown) = sanitize_and_convert(
            r#"<main>
                <p><a href="https://reader:secret@example.test/article">Readable label</a></p>
                <img src="https://reader:secret@example.test/image.png" alt="Private image">
            </main>"#,
            None,
        );

        for output in [&html, &markdown] {
            assert!(!output.contains("reader"));
            assert!(!output.contains("secret"));
            assert!(output.contains("Readable label"));
        }
        assert!(!html.contains("<img"));
    }

    #[test]
    fn keeps_an_existing_later_h1_when_metadata_title_is_empty() {
        assert_eq!(
            ensure_document_title("Lead.\n\n# Existing title".to_owned(), ""),
            "Lead.\n\n# Existing title"
        );
    }

    #[test]
    fn requires_a_large_gain_before_replacing_a_scored_candidate() {
        assert!(!semantic_is_substantially_richer(200, 320));
        assert!(!semantic_is_substantially_richer(200, 350));
        assert!(semantic_is_substantially_richer(200, 351));
        assert!(semantic_is_substantially_richer(1_000, 1_751));
    }

    #[test]
    fn counts_only_publishable_images_with_meaningful_alt_text() {
        assert_eq!(
            meaningful_image_count(
                r#"<div>
                    <img src="/one.png" alt="Diagram one">
                    <img src="https://example.test/two.png" alt="Diagram two">
                    <img src="data:image/gif;base64,bad" alt="Placeholder">
                    <img src="javascript:alert(1)" alt="Unsafe">
                    <img src="/decorative.png" alt="">
                </div>"#
            ),
            2
        );
    }

    #[test]
    fn hidden_style_detection_requires_exact_css_declarations() {
        for hidden in [
            "display: none",
            "DISPLAY : none !important",
            "color: red; visibility: HIDDEN",
        ] {
            assert!(style_hides_element(hidden), "should be hidden: {hidden}");
        }
        for visible in [
            "--footer-display: none",
            "--panel-visibility: hidden",
            "display: block",
            "content: 'display:none'",
        ] {
            assert!(
                !style_hides_element(visible),
                "should be visible: {visible}"
            );
        }
    }
}
