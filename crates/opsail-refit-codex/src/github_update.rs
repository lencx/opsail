use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Once, OnceLock};
use std::time::Duration;

use futures_util::StreamExt as _;
use reqwest::redirect::Policy;
use url::Url;

use crate::error::{CodexRefitError, CodexRefitErrorCode};
use crate::renderer_assets::{
    MAX_RENDERER_ASSET_BYTES, RendererAssetBundle, RendererAssetManifest,
};

const RAW_REPOSITORY_ROOT: &str = "https://raw.githubusercontent.com/lencx/opsail/refs/heads/main";
const RENDERER_ASSET_PATH: &str = "crates/opsail-refit-codex/assets";
const UPDATE_MANIFEST_NAME: &str = "opsail-refit-codex-update.json";
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_MANIFEST_RESPONSE_BYTES: usize = 64 * 1024;

pub(crate) type ManifestFuture<'a> =
    Pin<Box<dyn Future<Output = Result<RendererAssetManifest, CodexRefitError>> + Send + 'a>>;
pub(crate) type BundleFuture<'a> =
    Pin<Box<dyn Future<Output = Result<RendererAssetBundle, CodexRefitError>> + Send + 'a>>;

pub(crate) trait RendererAssetUpdateClient: Send + Sync {
    fn fetch_latest_manifest(&self) -> ManifestFuture<'_>;
    fn fetch_bundle(&self, manifest: RendererAssetManifest) -> BundleFuture<'_>;
}

#[derive(Debug, Default)]
pub(crate) struct GithubRendererAssetClient;

impl RendererAssetUpdateClient for GithubRendererAssetClient {
    fn fetch_latest_manifest(&self) -> ManifestFuture<'_> {
        Box::pin(fetch_latest_manifest())
    }

    fn fetch_bundle(&self, manifest: RendererAssetManifest) -> BundleFuture<'_> {
        Box::pin(fetch_bundle(manifest))
    }
}

async fn fetch_latest_manifest() -> Result<RendererAssetManifest, CodexRefitError> {
    install_tls_provider();
    let client = http_client()?;
    let manifest_url = raw_asset_url(UPDATE_MANIFEST_NAME)?;
    let manifest_bytes = fetch_bounded(
        client,
        &manifest_url,
        MAX_MANIFEST_RESPONSE_BYTES,
        "renderer asset manifest",
    )
    .await?;
    RendererAssetManifest::parse(&manifest_bytes)
}

async fn fetch_bundle(
    manifest: RendererAssetManifest,
) -> Result<RendererAssetBundle, CodexRefitError> {
    install_tls_provider();
    let client = http_client()?;
    let mut pending = futures_util::stream::FuturesUnordered::new();
    for name in manifest.file_names().map(str::to_owned) {
        let url = raw_asset_url(&name)?;
        pending.push(async move {
            let bytes = fetch_bounded(
                client,
                &url,
                MAX_RENDERER_ASSET_BYTES,
                "renderer JavaScript",
            )
            .await?;
            Ok::<_, CodexRefitError>((name, bytes))
        });
    }
    let mut files = BTreeMap::new();
    while let Some(result) = pending.next().await {
        let (name, bytes) = result?;
        files.insert(name, bytes);
    }
    let manifest_bytes = serde_json::to_vec(&manifest)
        .map_err(|_| update_error("could not serialize the validated renderer manifest"))?;
    RendererAssetBundle::from_parts(&manifest_bytes, files)
}

async fn fetch_bounded(
    client: &reqwest::Client,
    url: &Url,
    limit: usize,
    kind: &str,
) -> Result<Vec<u8>, CodexRefitError> {
    let response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|_| update_error(format!("could not download {kind} from GitHub")))?;
    if response.url() != url {
        return Err(update_error("GitHub update request changed its fixed URL"));
    }
    if !response.status().is_success() {
        return Err(update_error(format!(
            "GitHub {kind} request failed with status {}",
            response.status().as_u16()
        )));
    }
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(update_error(format!(
            "GitHub {kind} exceeds its size limit"
        )));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| update_error(format!("could not read GitHub {kind}")))?;
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(update_error(format!(
                "GitHub {kind} exceeds its size limit"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn raw_asset_url(name: &str) -> Result<Url, CodexRefitError> {
    let allowed_name = name == UPDATE_MANIFEST_NAME
        || crate::renderer_assets::RENDERER_ASSET_FILES.contains(&name);
    if !allowed_name {
        return Err(update_error(
            "GitHub renderer asset name is not allowlisted",
        ));
    }
    Url::parse(&format!(
        "{RAW_REPOSITORY_ROOT}/{RENDERER_ASSET_PATH}/{name}"
    ))
    .map_err(|_| update_error("could not construct the fixed GitHub renderer asset URL"))
}

fn http_client() -> Result<&'static reqwest::Client, CodexRefitError> {
    static CLIENT: OnceLock<Result<reqwest::Client, ()>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(HTTP_TIMEOUT)
                .timeout(HTTP_TIMEOUT)
                .redirect(Policy::none())
                .user_agent(format!("opsail-refit-codex/{}", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|_| ())
        })
        .as_ref()
        .map_err(|()| update_error("could not initialize the GitHub update client"))
}

fn install_tls_provider() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn update_error(message: impl Into<String>) -> CodexRefitError {
    CodexRefitError::new(CodexRefitErrorCode::UpdateFailed, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_urls_are_pinned_to_the_official_repository_main_branch() {
        let url = raw_asset_url(UPDATE_MANIFEST_NAME).unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("raw.githubusercontent.com"));
        assert_eq!(
            url.path(),
            format!("/lencx/opsail/refs/heads/main/{RENDERER_ASSET_PATH}/{UPDATE_MANIFEST_NAME}")
        );
        assert!(raw_asset_url("unexpected.js").is_err());
    }
}
