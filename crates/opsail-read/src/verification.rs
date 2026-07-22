use dom_query::Document;
use opsail_chrome::{ChromeError, RenderedPageEvidence, RenderedProbe};
use url::Url;

const ARTICLE_SURFACE_SELECTOR: &str = concat!(
    "article, [role='article'], [itemprop='articleBody'], [property='articleBody'], ",
    "#js_article, #js_content, .rich_media_content"
);
const MAIN_SURFACE_SELECTOR: &str = "main, [role='main']";
const RELATIVE_URL_BASE: &str = "https://opsail.invalid/";
const PROBE_WECHAT: u16 = 1;
const PROBE_CLOUDFLARE: u16 = 2;
const PROBE_GOOGLE: u16 = 3;
const PROBE_DATADOME: u16 = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VerificationProvider {
    WeChat,
    Cloudflare,
    AwsWaf,
    Google,
    DataDome,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VerificationSignal {
    pub(crate) provider: VerificationProvider,
}

struct DocumentEvidence<'a> {
    document: Document,
    resolved_url: Option<&'a Url>,
    has_substantive_content: bool,
    rendered: Option<&'a RenderedPageEvidence>,
    allow_static_profile: bool,
}

impl<'a> DocumentEvidence<'a> {
    fn new(
        html: &str,
        resolved_url: Option<&'a Url>,
        rendered: Option<&'a RenderedPageEvidence>,
        allow_static_profile: bool,
    ) -> Self {
        let document = Document::from(html);
        let has_substantive_content = has_substantive_content(&document);
        Self {
            document,
            resolved_url,
            has_substantive_content,
            rendered,
            allow_static_profile,
        }
    }

    fn profile_is_active(&self, probe_id: u16) -> bool {
        match self.rendered {
            Some(rendered) => rendered.result(probe_id).is_some_and(|result| {
                result.matches() > 0
                    && result
                        .marker()
                        .is_some_and(|marker| marker.visible() && marker.stable())
            }),
            None => self.allow_static_profile,
        }
    }
}

type Detector = for<'a> fn(&DocumentEvidence<'a>) -> bool;

const DETECTORS: &[(VerificationProvider, Detector)] = &[
    (VerificationProvider::WeChat, detects_wechat),
    (VerificationProvider::Cloudflare, detects_cloudflare),
    (VerificationProvider::Google, detects_google),
    (VerificationProvider::DataDome, detects_datadome),
];

/// Detects only full-page bot-verification interstitials backed by multiple
/// independent pieces of DOM and URL evidence.
///
/// Ordinary login pages and embedded CAPTCHA widgets are intentionally outside
/// this detector's scope.
pub(crate) fn detect_document(
    html: &str,
    resolved_url: Option<&Url>,
    rendered: Option<&RenderedPageEvidence>,
    allow_static_profile: bool,
) -> Option<VerificationSignal> {
    let evidence = DocumentEvidence::new(html, resolved_url, rendered, allow_static_profile);
    DETECTORS.iter().find_map(|(provider, detector)| {
        detector(&evidence).then_some(VerificationSignal {
            provider: *provider,
        })
    })
}

/// Neutral live-layout probes executed by `opsail-chrome`. Provider identity
/// and the decision rules remain private to this module.
pub(crate) fn rendered_probes() -> Result<Vec<RenderedProbe>, ChromeError> {
    [
        (PROBE_WECHAT, "#js_verify.weui-msg, #js_verify .weui-msg"),
        (
            PROBE_CLOUDFLARE,
            "form#challenge-form[action], #challenge-stage, #challenge-running",
        ),
        (
            PROBE_GOOGLE,
            "form#captcha-form, form[action] .g-recaptcha[data-sitekey]",
        ),
        (
            PROBE_DATADOME,
            "#captcha-container, #datadome-captcha, .captcha-container",
        ),
    ]
    .into_iter()
    .map(|(id, selector)| RenderedProbe::new(id, selector))
    .collect()
}

/// Detect provider-declared verification from a top-level document response.
///
/// These are exact transport contracts published by the providers. Status and
/// header values are evaluated together where the provider requires it; no
/// body text or generic error status participates in this decision.
pub(crate) fn detect_response(
    status: u16,
    cf_mitigated: Option<&str>,
    aws_waf_action: Option<&str>,
) -> Option<VerificationSignal> {
    if header_value_is(cf_mitigated, "challenge") {
        return Some(VerificationSignal {
            provider: VerificationProvider::Cloudflare,
        });
    }

    let is_aws_gate = (status == 202 && header_value_is(aws_waf_action, "challenge"))
        || (status == 405 && header_value_is(aws_waf_action, "captcha"));
    is_aws_gate.then_some(VerificationSignal {
        provider: VerificationProvider::AwsWaf,
    })
}

pub(crate) fn redacted_url(value: &str) -> String {
    let Ok(mut url) = Url::parse(value) else {
        return "<source>".to_owned();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

fn detects_wechat(evidence: &DocumentEvidence<'_>) -> bool {
    evidence
        .resolved_url
        .is_some_and(|url| host_is_or_subdomain(url, "mp.weixin.qq.com"))
        && !evidence.has_substantive_content
        && evidence
            .document
            .select("#js_verify.weui-msg, #js_verify .weui-msg")
            .exists()
        && has_wechat_verification_resource(evidence)
        && evidence.profile_is_active(PROBE_WECHAT)
}

fn has_wechat_verification_resource(evidence: &DocumentEvidence<'_>) -> bool {
    has_inline_script_token(&evidence.document, "secitptpage/verify")
        || evidence
            .document
            .select("link[href], script[src]")
            .iter()
            .any(|resource| {
                let value = resource.attr("href").or_else(|| resource.attr("src"));
                value
                    .as_deref()
                    .and_then(|value| parse_dom_url(value, evidence.resolved_url))
                    .is_some_and(|url| url.path().contains("/secitptpage/verify"))
            })
}

fn detects_cloudflare(evidence: &DocumentEvidence<'_>) -> bool {
    if evidence.has_substantive_content {
        return false;
    }

    let has_challenge_form = evidence
        .document
        .select("form#challenge-form[action]")
        .iter()
        .any(|form| {
            form.attr("action")
                .as_deref()
                .and_then(|action| parse_dom_url(action, evidence.resolved_url))
                .is_some_and(|url| {
                    same_origin_or_unknown(&url, evidence.resolved_url)
                        && is_cloudflare_challenge_endpoint(&url)
                })
        });
    if !has_challenge_form {
        return false;
    }

    let has_runtime = has_inline_script_token(&evidence.document, "_cf_chl_opt")
        || evidence
            .document
            .select("script[src]")
            .iter()
            .any(|script| {
                script
                    .attr("src")
                    .as_deref()
                    .and_then(|source| parse_dom_url(source, evidence.resolved_url))
                    .is_some_and(|url| {
                        same_origin_or_unknown(&url, evidence.resolved_url)
                            && is_cloudflare_challenge_resource(&url)
                    })
            });
    has_runtime && evidence.profile_is_active(PROBE_CLOUDFLARE)
}

fn is_cloudflare_challenge_endpoint(url: &Url) -> bool {
    is_cloudflare_challenge_resource(url)
        || url
            .path_segments()
            .is_some_and(|mut segments| segments.any(|segment| segment.starts_with("__cf_chl_")))
        || url
            .query_pairs()
            .any(|(name, _)| name.starts_with("__cf_chl_"))
}

fn is_cloudflare_challenge_resource(url: &Url) -> bool {
    url.path().starts_with("/cdn-cgi/challenge-platform/")
}

fn detects_google(evidence: &DocumentEvidence<'_>) -> bool {
    let Some(resolved_url) = evidence.resolved_url else {
        return false;
    };
    if evidence.has_substantive_content
        || !host_is_or_subdomain(resolved_url, "google.com")
        || !is_google_sorry_path(resolved_url.path())
        || !has_google_recaptcha_runtime(evidence)
    {
        return false;
    }

    let has_gate = evidence.document.select("form[action]").iter().any(|form| {
        let recognized_form = form
            .attr("id")
            .is_some_and(|id| id.eq_ignore_ascii_case("captcha-form"))
            || form
                .attr("action")
                .as_deref()
                .and_then(|action| parse_dom_url(action, Some(resolved_url)))
                .is_some_and(|url| {
                    host_is_or_subdomain(&url, "google.com") && is_google_sorry_path(url.path())
                });
        let has_continuation = form
            .select("input[name='q'], input[name='continue']")
            .exists();
        let has_widget = form.select(".g-recaptcha[data-sitekey]").exists()
            || form.select("iframe[src]").iter().any(|iframe| {
                iframe
                    .attr("src")
                    .as_deref()
                    .and_then(|source| parse_dom_url(source, Some(resolved_url)))
                    .is_some_and(|url| is_google_recaptcha_url(&url))
            });
        recognized_form && has_continuation && has_widget
    });
    has_gate && evidence.profile_is_active(PROBE_GOOGLE)
}

fn is_google_sorry_path(path: &str) -> bool {
    path.eq_ignore_ascii_case("/sorry")
        || path
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("/sorry/"))
}

fn has_google_recaptcha_runtime(evidence: &DocumentEvidence<'_>) -> bool {
    evidence
        .document
        .select("script[src]")
        .iter()
        .any(|script| {
            script
                .attr("src")
                .as_deref()
                .and_then(|source| parse_dom_url(source, evidence.resolved_url))
                .is_some_and(|url| is_google_recaptcha_url(&url))
        })
}

fn is_google_recaptcha_url(url: &Url) -> bool {
    (host_is_or_subdomain(url, "google.com") || host_is_or_subdomain(url, "recaptcha.net"))
        && url.path().starts_with("/recaptcha/")
}

fn detects_datadome(evidence: &DocumentEvidence<'_>) -> bool {
    let Some(resolved_url) = evidence.resolved_url else {
        return false;
    };
    if evidence.has_substantive_content
        || !host_is_or_subdomain(resolved_url, "captcha-delivery.com")
        || !is_captcha_path(resolved_url.path())
        || !evidence
            .document
            .select("#captcha-container, #datadome-captcha, .captcha-container")
            .exists()
    {
        return false;
    }

    let has_provider_resource = evidence
        .document
        .select("iframe[src], script[src], form[action]")
        .iter()
        .any(|resource| {
            let value = resource.attr("src").or_else(|| resource.attr("action"));
            value
                .as_deref()
                .and_then(|value| parse_dom_url(value, Some(resolved_url)))
                .is_some_and(|url| host_is_or_subdomain(&url, "captcha-delivery.com"))
        });
    has_provider_resource && evidence.profile_is_active(PROBE_DATADOME)
}

fn is_captcha_path(path: &str) -> bool {
    path.eq_ignore_ascii_case("/captcha")
        || path
            .get(..9)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("/captcha/"))
}

fn has_inline_script_token(document: &Document, token: &str) -> bool {
    document.select("script").iter().any(|script| {
        script.attr("src").is_none()
            && script_is_executable(&script)
            && script.inner_html().as_ref().contains(token)
    })
}

fn script_is_executable(script: &dom_query::Selection<'_>) -> bool {
    let Some(script_type) = script.attr("type") else {
        return true;
    };
    let script_type = script_type.split(';').next().unwrap_or_default().trim();
    script_type.is_empty()
        || [
            "module",
            "text/javascript",
            "application/javascript",
            "text/ecmascript",
            "application/ecmascript",
        ]
        .iter()
        .any(|executable| script_type.eq_ignore_ascii_case(executable))
}

fn has_substantive_content(document: &Document) -> bool {
    document
        .select(ARTICLE_SURFACE_SELECTOR)
        .iter()
        .any(|surface| {
            !is_statically_hidden(&surface)
                && (surface.text().trim().chars().count() >= 1
                    || surface
                        .select("h1, h2, h3, p, li, pre, blockquote")
                        .exists())
        })
        || document
            .select(MAIN_SURFACE_SELECTOR)
            .iter()
            .any(|surface| {
                if is_statically_hidden(&surface) {
                    return false;
                }
                let text_chars = surface.text().trim().chars().count();
                let blocks = surface.select("p, li, pre, blockquote, td").length();
                text_chars >= 48
                    && ((surface.select("h1, h2, h3").exists() && blocks >= 1) || blocks >= 2)
            })
}

fn is_statically_hidden(element: &dom_query::Selection<'_>) -> bool {
    element.has_attr("hidden")
        || element
            .attr("aria-hidden")
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
        || element.attr("style").is_some_and(|style| {
            let compact = style
                .chars()
                .filter(|character| !character.is_ascii_whitespace())
                .flat_map(char::to_lowercase)
                .collect::<String>();
            compact
                .split(';')
                .any(|declaration| matches!(declaration, "display:none" | "visibility:hidden"))
        })
}

fn parse_dom_url(value: &str, base_url: Option<&Url>) -> Option<Url> {
    if let Ok(url) = Url::parse(value) {
        return Some(url);
    }
    if let Some(base_url) = base_url
        && let Ok(url) = base_url.join(value)
    {
        return Some(url);
    }
    Url::parse(RELATIVE_URL_BASE).ok()?.join(value).ok()
}

fn same_origin_or_unknown(url: &Url, base_url: Option<&Url>) -> bool {
    base_url.is_none_or(|base_url| url.origin() == base_url.origin())
}

fn host_is_or_subdomain(url: &Url, domain: &str) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case(domain)
            || host
                .strip_suffix(domain)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

fn header_value_is(actual: Option<&str>, expected: &str) -> bool {
    actual.is_some_and(|value| value.trim().eq_ignore_ascii_case(expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detected_provider(html: &str, url: &str) -> Option<VerificationProvider> {
        let url = Url::parse(url).unwrap();
        detect_document(html, Some(&url), None, true).map(|signal| signal.provider)
    }

    #[test]
    fn detects_a_structural_wechat_verification_gate() {
        let html = r#"<!doctype html><html><head>
          <link rel="stylesheet" href="/secitptpage/verify.css">
        </head><body>
          <main id="js_verify" class="weui-msg"><a href="/mp/verify">verify</a></main>
        </body></html>"#;

        assert_eq!(
            detected_provider(
                html,
                "https://mp.weixin.qq.com/mp/wappoc_appmsgcaptcha?token=secret"
            ),
            Some(VerificationProvider::WeChat)
        );
    }

    #[test]
    fn wechat_markers_need_the_provider_origin_and_no_article() {
        let copied_gate = r#"<html><head>
          <script>var page = 'secitptpage/verify';</script>
        </head><body><main id="js_verify" class="weui-msg"></main></body></html>"#;
        let article = r#"<html><head>
          <script>var page = 'secitptpage/verify';</script>
        </head><body><article id="js_article"><div id="js_content">
          <div id="js_verify" class="weui-msg">Quoted gate</div>
        </div></article></body></html>"#;

        assert_eq!(
            detected_provider(copied_gate, "https://example.test/copied"),
            None
        );
        assert_eq!(
            detected_provider(article, "https://mp.weixin.qq.com/s/article"),
            None
        );
    }

    #[test]
    fn detects_cloudflare_from_a_challenge_form_and_runtime_evidence() {
        let html = r#"<!doctype html><html><head><title>Unrelated localized title</title></head>
        <body><main>
          <form id="challenge-form" action="/?__cf_chl_rt_tk=secret"></form>
        </main><script>window._cf_chl_opt = { cType: 'managed' };</script></body></html>"#;

        assert_eq!(
            detected_provider(html, "https://www.npmjs.com/package/opsail"),
            Some(VerificationProvider::Cloudflare)
        );
    }

    #[test]
    fn detects_cloudflare_from_a_provider_resource_without_copy_matching_text() {
        let html = r#"<html><body>
          <form id="challenge-form" action="/__cf_chl_f_tk=secret"></form>
          <script src="/cdn-cgi/challenge-platform/h/g/orchestrate/chl_page/v1"></script>
        </body></html>"#;

        assert_eq!(
            detected_provider(html, "https://protected.example.test/path"),
            Some(VerificationProvider::Cloudflare)
        );
    }

    #[test]
    fn cloudflare_detection_requires_all_structural_evidence() {
        let form_only = r#"<html><body>
          <form id="challenge-form" action="/?__cf_chl_rt_tk=secret"></form>
        </body></html>"#;
        let runtime_only = r#"<html><body>
          <script>window._cf_chl_opt = {};</script>
        </body></html>"#;
        let article_with_every_marker = r#"<html><body><article>
          <h1>Cloudflare challenge integration notes</h1>
          <form id="challenge-form" action="/?__cf_chl_rt_tk=example"></form>
          <script>window._cf_chl_opt = {};</script>
        </article></body></html>"#;

        for html in [form_only, runtime_only, article_with_every_marker] {
            assert_eq!(detected_provider(html, "https://example.test/guide"), None);
        }
    }

    #[test]
    fn cloudflare_ignores_inert_example_data_and_substantive_main_content() {
        let integration_guide = r#"<!doctype html><html><body><main>
          <h1>Perimeter integration guide</h1>
          <p>This page documents how a challenge form and its runtime configuration work.</p>
          <form id="challenge-form" action="/?__cf_chl_rt_tk=example"></form>
          <script type="application/json">{"example":"window._cf_chl_opt"}</script>
        </main></body></html>"#;

        assert_eq!(
            detected_provider(integration_guide, "https://example.test/integration-docs"),
            None
        );
    }

    #[test]
    fn hidden_or_empty_article_shell_does_not_mask_a_cloudflare_gate() {
        let html = r#"<!doctype html><html><body>
          <article hidden></article>
          <main><form id="challenge-form" action="/?__cf_chl_rt_tk=secret"></form></main>
          <script>window._cf_chl_opt = { cType: 'managed' };</script>
        </body></html>"#;

        assert_eq!(
            detected_provider(html, "https://protected.example.test/path"),
            Some(VerificationProvider::Cloudflare)
        );
    }

    #[test]
    fn detects_google_unusual_traffic_from_origin_route_form_and_recaptcha() {
        let html = r#"<!doctype html><html><body><main>
          <form id="captcha-form" action="index" method="post">
            <input type="hidden" name="q" value="opaque">
            <div class="g-recaptcha" data-sitekey="public-key"></div>
          </form>
          <script src="https://www.google.com/recaptcha/api.js" async></script>
        </main></body></html>"#;

        assert_eq!(
            detected_provider(
                html,
                "https://www.google.com/sorry/index?continue=https%3A%2F%2Fgoogle.com"
            ),
            Some(VerificationProvider::Google)
        );
    }

    #[test]
    fn google_detection_rejects_lookalike_origins_and_non_sorry_routes() {
        let html = r#"<html><body>
          <form id="captcha-form" action="/sorry/index">
            <input name="continue"><div class="g-recaptcha" data-sitekey="key"></div>
          </form>
          <script src="https://www.google.com/recaptcha/api.js"></script>
        </body></html>"#;

        assert_eq!(
            detected_provider(html, "https://google.com.evil.example/sorry/index"),
            None
        );
        assert_eq!(
            detected_provider(html, "https://www.google.com/account/security"),
            None
        );
    }

    #[test]
    fn detects_a_top_level_datadome_captcha_delivery_gate() {
        let html = r#"<html><body>
          <div id="captcha-container">
            <iframe src="https://geo.captcha-delivery.com/captcha/frame"></iframe>
          </div>
        </body></html>"#;

        assert_eq!(
            detected_provider(
                html,
                "https://geo.captcha-delivery.com/captcha/?initialCid=secret"
            ),
            Some(VerificationProvider::DataDome)
        );
    }

    #[test]
    fn datadome_widgets_are_not_top_level_gate_evidence() {
        let embedded = r#"<html><body><article><h1>Shop</h1>
          <div id="captcha-container">
            <iframe src="https://geo.captcha-delivery.com/captcha/frame"></iframe>
          </div>
        </article></body></html>"#;
        let provider_article = r#"<html><body><article>
          <h1>DataDome documentation</h1><div id="captcha-container"></div>
          <script src="/captcha/runtime.js"></script>
        </article></body></html>"#;

        assert_eq!(
            detected_provider(embedded, "https://shop.example.test/product"),
            None
        );
        assert_eq!(
            detected_provider(
                provider_article,
                "https://geo.captcha-delivery.com/captcha/docs"
            ),
            None
        );
    }

    #[test]
    fn human_markup_without_rendered_takeover_evidence_is_not_classified() {
        let root_only = r#"<html><body><div id="px-captcha"></div></body></html>"#;
        let root_and_runtime = r#"<html><body>
          <div id="px-captcha"></div>
          <script>window._pxBlockedUrl = '/blocked';</script>
        </body></html>"#;
        let normal_spa = r#"<html><body><main>
          <h1>Account dashboard</h1><p>The requested account content is available.</p>
          <div id="px-captcha"></div>
          <script src="https://client.px-cloud.net/init.js"></script>
        </main></body></html>"#;
        let inert_configuration = r#"<html><body><main>
          <h1>Integration guide</h1><p>This page contains inert configuration examples.</p>
          <div id="px-captcha"></div>
          <script type="application/json">{"example":"window._pxBlockedUrl"}</script>
        </main></body></html>"#;

        for html in [root_only, root_and_runtime, normal_spa, inert_configuration] {
            assert_eq!(
                detected_provider(html, "https://example.test/article"),
                None
            );
        }
    }

    #[test]
    fn embedded_captcha_widgets_are_not_verification_pages() {
        let html = r#"<!doctype html><html><body><main>
          <form action="/contact">
            <div class="g-recaptcha" data-sitekey="recaptcha-key"></div>
            <div class="h-captcha" data-sitekey="hcaptcha-key"></div>
            <div class="cf-turnstile" data-sitekey="turnstile-key"></div>
          </form>
          <script src="https://www.google.com/recaptcha/api.js"></script>
          <script src="https://js.hcaptcha.com/1/api.js"></script>
          <script src="https://challenges.cloudflare.com/turnstile/v0/api.js"></script>
        </main></body></html>"#;

        assert_eq!(
            detected_provider(html, "https://example.test/contact"),
            None
        );
    }

    #[test]
    fn redacts_query_fragment_and_credentials_from_reported_urls() {
        assert_eq!(
            redacted_url("https://reader:secret@example.test/path?token=secret#fragment"),
            "https://example.test/path"
        );
        assert_eq!(redacted_url("<memory>"), "<source>");
    }

    #[test]
    fn detects_only_provider_declared_main_response_challenges() {
        assert_eq!(
            detect_response(403, Some(" challenge "), None).map(|signal| signal.provider),
            Some(VerificationProvider::Cloudflare)
        );
        assert_eq!(
            detect_response(202, None, Some("Challenge")).map(|signal| signal.provider),
            Some(VerificationProvider::AwsWaf)
        );
        assert_eq!(
            detect_response(405, None, Some("CAPTCHA")).map(|signal| signal.provider),
            Some(VerificationProvider::AwsWaf)
        );

        for (status, cf_mitigated, aws_waf_action) in [
            (403, None, None),
            (403, Some("managed"), None),
            (403, None, Some("challenge")),
            (202, None, Some("captcha")),
            (405, None, Some("challenge")),
        ] {
            assert!(detect_response(status, cf_mitigated, aws_waf_action).is_none());
        }
    }
}
