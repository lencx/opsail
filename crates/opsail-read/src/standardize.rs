use dom_query::{Document, Selection};
use url::Url;

/// Normalize browser-oriented markup before article scoring sees it.
pub(crate) fn standardize_document(document: &Document, base_url: Option<&Url>) {
    reveal_wechat_article(document, base_url);
    recover_noscript_images(document);
    normalize_images(document);
    normalize_code_blocks(document);
}

fn reveal_wechat_article(document: &Document, base_url: Option<&Url>) {
    let is_wechat = base_url
        .and_then(Url::host_str)
        .is_some_and(|host| host.eq_ignore_ascii_case("mp.weixin.qq.com"));
    if !is_wechat || !document.select("#js_article").exists() {
        return;
    }

    // WeChat ships the complete article in this node, initially hides it,
    // then removes the style from page JavaScript. Static readers must apply
    // that one initialization step before generic hidden-content filtering.
    document
        .select("#js_content.rich_media_content")
        .remove_attrs(&["hidden", "aria-hidden", "style"]);
}

fn recover_noscript_images(document: &Document) {
    for noscript in document.select("noscript").iter() {
        let scope = noscript.clone();
        let mut images = scope
            .select("img")
            .iter()
            .filter(has_image_source)
            .map(|image| image.html().to_string())
            .collect::<Vec<_>>();

        // With scripting enabled, HTML parsers represent a noscript body as a
        // text node. Parse only that inert text and copy image elements out.
        if images.is_empty() {
            let fallback = Document::fragment(noscript.text());
            images = fallback
                .select("img")
                .iter()
                .filter(has_image_source)
                .map(|image| image.html().to_string())
                .collect();
        }

        if !images.is_empty() {
            noscript.replace_with_html(images.join(""));
        }
    }
}

fn has_image_source(image: &Selection<'_>) -> bool {
    [
        "src",
        "srcset",
        "data-src",
        "data-srcset",
        "data-original",
        "data-lazy-src",
    ]
    .iter()
    .any(|attribute| {
        image
            .attr(attribute)
            .is_some_and(|value| !value.trim().is_empty())
    })
}

fn normalize_images(document: &Document) {
    for picture in document.select("picture").iter() {
        let scope = picture.clone();
        let Some(image) = scope.select("img").iter().next() else {
            continue;
        };
        let current = image.attr("src").map(|value| value.to_string());
        if current.as_deref().is_some_and(is_usable_image_source) {
            continue;
        }

        let candidate = scope.select("source[srcset]").iter().find_map(|source| {
            source
                .attr("srcset")
                .and_then(|srcset| best_srcset_candidate(srcset.as_ref()))
        });
        if let Some(candidate) = candidate {
            image.set_attr("src", &candidate);
        }
    }

    for image in document.select("img").iter() {
        let srcset_candidate = ["data-srcset", "srcset"].iter().find_map(|attribute| {
            image
                .attr(attribute)
                .and_then(|srcset| best_srcset_candidate(srcset.as_ref()))
        });
        let direct_candidate = [
            "data-src",
            "data-original",
            "data-lazy-src",
            "data-original-src",
            "data-url",
        ]
        .iter()
        .find_map(|attribute| {
            image
                .attr(attribute)
                .map(|value| value.trim().to_owned())
                .filter(|value| is_safe_resource_reference(value))
        });
        let current = image.attr("src").map(|value| value.to_string());

        if let Some(candidate) = srcset_candidate.or(direct_candidate)
            && (current
                .as_deref()
                .is_none_or(|value| !is_usable_image_source(value))
                || image.has_attr("srcset")
                || image.has_attr("data-srcset"))
        {
            image.set_attr("src", &candidate);
        }

        image.remove_attrs(&[
            "data-src",
            "data-srcset",
            "data-original",
            "data-lazy-src",
            "data-original-src",
            "data-url",
            "data-ll-status",
        ]);
    }
}

fn best_srcset_candidate(srcset: &str) -> Option<String> {
    split_srcset_candidates(srcset)
        .into_iter()
        .filter_map(|candidate| {
            let mut parts = candidate.split_ascii_whitespace();
            let url = parts.next()?.trim();
            if !is_safe_resource_reference(url) {
                return None;
            }
            let score = parts
                .next()
                .and_then(srcset_descriptor_score)
                .unwrap_or(1.0);
            Some((score, url.to_owned()))
        })
        .max_by(|left, right| left.0.total_cmp(&right.0))
        .map(|(_, url)| url)
}

fn split_srcset_candidates(srcset: &str) -> Vec<&str> {
    let protect_data_url_commas = srcset.to_ascii_lowercase().contains("data:");
    let mut candidates = Vec::new();
    let mut start = 0;
    for (index, character) in srcset.char_indices() {
        if character != ',' {
            continue;
        }
        let next_is_whitespace = srcset
            .get(index + 1..)
            .and_then(|rest| rest.chars().next())
            .is_some_and(char::is_whitespace);
        if !protect_data_url_commas || next_is_whitespace {
            candidates.push(&srcset[start..index]);
            start = index + 1;
        }
    }
    candidates.push(&srcset[start..]);
    candidates
}

fn srcset_descriptor_score(descriptor: &str) -> Option<f64> {
    let descriptor = descriptor.trim();
    if let Some(value) = descriptor.strip_suffix('w') {
        return value.parse::<f64>().ok();
    }
    if let Some(value) = descriptor.strip_suffix('x') {
        return value.parse::<f64>().ok().map(|density| density * 1_000.0);
    }
    None
}

fn is_usable_image_source(value: &str) -> bool {
    !value
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("data:image/")
        && is_safe_resource_reference(value)
}

fn is_safe_resource_reference(value: &str) -> bool {
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

fn url_has_credentials(url: &Url) -> bool {
    !url.username().is_empty() || url.password().is_some()
}

fn normalize_code_blocks(document: &Document) {
    for selector in [
        "pre .lnt",
        "pre .lineno",
        "pre .react-syntax-highlighter-line-number",
        "pre [data-line-number]",
        "pre button",
        "pre [class*='codeblock-button']",
        "pre [class*='toolbar']",
        "pre [class*='code__header']",
    ] {
        let chrome = document.select(selector);
        for element in chrome.iter() {
            mark_enclosing_pre(&element);
        }
        chrome.remove();
    }

    for span in document.select("pre span[style]").iter() {
        let style = span
            .attr("style")
            .map(|value| {
                value
                    .chars()
                    .filter(|character| !character.is_ascii_whitespace())
                    .flat_map(char::to_lowercase)
                    .collect::<String>()
            })
            .unwrap_or_default();
        let text = span.text();
        if style.contains("user-select:none")
            && !text.trim().is_empty()
            && text
                .trim()
                .chars()
                .all(|character| character.is_ascii_digit())
        {
            mark_enclosing_pre(&span);
            span.remove();
        }
    }

    for table in document
        .select("table.lntable, table.rouge-table, table.highlighttable")
        .iter()
    {
        let scope = table.clone();
        let Some(code) = find_table_code(&scope) else {
            continue;
        };
        let text = trim_code_boundaries(code.text().as_ref());
        if text.trim().is_empty() {
            continue;
        }

        let language = code_language(&code).or_else(|| code_language(&table));
        let language_attributes = language.map_or_else(String::new, |language| {
            format!(" class=\"language-{language}\" data-lang=\"{language}\"")
        });
        let replacement_target = table
            .ancestors(Some(4))
            .filter("code")
            .iter()
            .find(|ancestor| {
                ancestor.select("table").length() == 1
                    && ancestor.text().trim() == table.text().trim()
            })
            .unwrap_or_else(|| table.clone());
        replacement_target.replace_with_html(format!(
            "<pre><code{language_attributes}>{}</code></pre>",
            escape_html(&text)
        ));
    }

    for pre in document.select("pre[data-opsail-normalize-code]").iter() {
        if let Some(code) = pre.select("code").iter().next() {
            code.set_text(&trim_code_boundaries(code.text().as_ref()));
        }
        pre.remove_attr("data-opsail-normalize-code");
    }
}

fn mark_enclosing_pre(element: &Selection<'_>) {
    if let Some(pre) = element.ancestors(Some(12)).filter("pre").iter().next() {
        pre.set_attr("data-opsail-normalize-code", "true");
    }
}

fn find_table_code<'a>(scope: &Selection<'a>) -> Option<Selection<'a>> {
    for selector in [
        "td.rouge-code pre",
        "td.code pre",
        "code[data-lang]",
        "code[data-language]",
        "code[class*='language-']",
    ] {
        if let Some(code) = scope
            .select(selector)
            .iter()
            .find(|node| meaningful_code_score(node.text().as_ref()) > 0)
        {
            return Some(code);
        }
    }

    scope
        .select("pre")
        .iter()
        .max_by_key(|node| meaningful_code_score(node.text().as_ref()))
        .filter(|node| meaningful_code_score(node.text().as_ref()) > 0)
}

fn meaningful_code_score(value: &str) -> usize {
    let visible = value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    if !visible.is_empty() && !visible.chars().all(|character| character.is_ascii_digit()) {
        visible.chars().count()
    } else {
        0
    }
}

fn code_language(node: &Selection<'_>) -> Option<String> {
    std::iter::once(node.clone())
        .chain(node.ancestors(Some(10)).iter())
        .find_map(|element| {
            for attribute in ["data-lang", "data-language", "lang"] {
                if let Some(language) = element
                    .attr(attribute)
                    .and_then(|value| sanitize_language(value.as_ref()))
                {
                    return Some(language);
                }
            }

            element.attr("class").and_then(|classes| {
                classes.split_ascii_whitespace().find_map(|class| {
                    ["language-", "lang-"]
                        .iter()
                        .find_map(|prefix| class.strip_prefix(prefix))
                        .and_then(sanitize_language)
                })
            })
        })
}

fn sanitize_language(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    (!value.is_empty()
        && value.len() <= 32
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '#' | '_')
        }))
    .then_some(value)
}

fn trim_code_boundaries(value: &str) -> String {
    value
        .trim_matches(|character| matches!(character, '\n' | '\r'))
        .to_owned()
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_the_largest_safe_srcset_candidate() {
        assert_eq!(
            best_srcset_candidate("small.png 320w, large.png 1280w").as_deref(),
            Some("large.png")
        );
        assert_eq!(
            best_srcset_candidate("javascript:alert(1) 2x, safe.png 1x").as_deref(),
            Some("safe.png")
        );
        assert_eq!(
            best_srcset_candidate("data:image/gif;base64,AAAA 2x, safe.png 1x").as_deref(),
            Some("safe.png")
        );
    }

    #[test]
    fn rejects_active_or_credentialed_image_references() {
        assert!(!is_safe_resource_reference("data:text/html,bad"));
        assert!(!is_safe_resource_reference("javascript:alert(1)"));
        assert!(!is_safe_resource_reference(
            "https://reader:secret@example.test/image.png"
        ));
        assert!(is_safe_resource_reference("../images/diagram.png"));
    }
}
