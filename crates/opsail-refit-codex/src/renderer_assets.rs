use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ring::digest;
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::error::{CodexRefitError, CodexRefitErrorCode};
use crate::model::{RendererAssetInfo, RendererAssetSource};

pub(crate) const RENDERER_ASSET_API_VERSION: u32 = 1;
pub(crate) const RENDERER_ASSET_FILES: [&str; 4] = [
    "opsail-refit-codex-dom-adapter.js",
    "opsail-refit-codex-renderer-control.js",
    "opsail-refit-codex-usage-model.js",
    "opsail-refit-codex-usage-runtime.js",
];

const MANIFEST_SCHEMA_VERSION: u32 = 1;
const CURRENT_POINTER_SCHEMA_VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
pub(crate) const MAX_RENDERER_ASSET_BYTES: usize = 512 * 1024;
pub(crate) const MAX_RENDERER_ASSET_TOTAL_BYTES: usize = 2 * 1024 * 1024;
const EMBEDDED_MANIFEST: &str = include_str!("../assets/opsail-refit-codex-update.json");

const EMBEDDED_SOURCES: [(&str, &str); 4] = [
    (
        "opsail-refit-codex-dom-adapter.js",
        include_str!("../assets/opsail-refit-codex-dom-adapter.js"),
    ),
    (
        "opsail-refit-codex-renderer-control.js",
        include_str!("../assets/opsail-refit-codex-renderer-control.js"),
    ),
    (
        "opsail-refit-codex-usage-model.js",
        include_str!("../assets/opsail-refit-codex-usage-model.js"),
    ),
    (
        "opsail-refit-codex-usage-runtime.js",
        include_str!("../assets/opsail-refit-codex-usage-runtime.js"),
    ),
];

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RendererAssetManifest {
    schema_version: u32,
    asset_version: String,
    api_version: u32,
    minimum_opsail_version: String,
    files: Vec<RendererAssetFile>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RendererAssetFile {
    name: String,
    sha256: String,
    bytes: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CurrentPointer {
    schema_version: u32,
    version: String,
    directory: String,
}

#[derive(Debug, Clone)]
pub(crate) struct RendererSources {
    files: BTreeMap<String, String>,
}

impl RendererSources {
    pub(crate) fn get(&self, name: &str) -> Result<&str, CodexRefitError> {
        self.files.get(name).map(String::as_str).ok_or_else(|| {
            update_error("renderer asset bundle is missing a required JavaScript file")
        })
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        RENDERER_ASSET_FILES.into_iter().map(|name| {
            (
                name,
                self.files
                    .get(name)
                    .expect("validated renderer sources contain every allowlisted file")
                    .as_str(),
            )
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RendererAssetBundle {
    manifest: RendererAssetManifest,
    sources: RendererSources,
}

impl RendererAssetBundle {
    pub(crate) fn from_parts(
        manifest_bytes: &[u8],
        files: BTreeMap<String, Vec<u8>>,
    ) -> Result<Self, CodexRefitError> {
        let manifest = RendererAssetManifest::parse(manifest_bytes)?;
        if files.len() != RENDERER_ASSET_FILES.len() {
            return Err(update_error(
                "renderer asset bundle does not contain the exact JavaScript allowlist",
            ));
        }
        let mut sources = BTreeMap::new();
        for expected in &manifest.files {
            let bytes = files.get(&expected.name).ok_or_else(|| {
                update_error("renderer asset bundle is missing a required JavaScript file")
            })?;
            if bytes.len() != expected.bytes || sha256(bytes) != expected.sha256 {
                return Err(update_error(
                    "renderer asset size or SHA-256 verification failed",
                ));
            }
            let source = std::str::from_utf8(bytes)
                .map_err(|_| update_error("renderer JavaScript is not valid UTF-8"))?;
            validate_javascript(source)?;
            sources.insert(expected.name.clone(), source.to_owned());
        }
        Ok(Self {
            manifest,
            sources: RendererSources { files: sources },
        })
    }

    #[cfg(test)]
    pub(crate) fn manifest(&self) -> &RendererAssetManifest {
        &self.manifest
    }

    pub(crate) fn sources(&self) -> &RendererSources {
        &self.sources
    }

    pub(crate) fn info(&self, source: RendererAssetSource) -> RendererAssetInfo {
        self.manifest.info(source)
    }

    fn version(&self) -> Version {
        self.manifest.version()
    }

    fn identity(&self) -> String {
        let bytes = serde_json::to_vec(&self.manifest)
            .expect("serializing a validated renderer asset manifest cannot fail");
        sha256(&bytes)
    }

    pub(crate) fn content_identity(&self) -> String {
        self.manifest.content_identity()
    }
}

impl RendererAssetManifest {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, CodexRefitError> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(update_error(
                "renderer asset manifest exceeds its size limit",
            ));
        }
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|_| update_error("renderer asset manifest is invalid"))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub(crate) fn file_names(&self) -> impl Iterator<Item = &str> {
        self.files.iter().map(|file| file.name.as_str())
    }

    pub(crate) fn info(&self, source: RendererAssetSource) -> RendererAssetInfo {
        RendererAssetInfo {
            version: self.asset_version.clone(),
            source,
        }
    }

    pub(crate) fn version(&self) -> Version {
        Version::parse(&self.asset_version).expect("validated renderer asset versions remain valid")
    }

    pub(crate) fn content_identity(&self) -> String {
        let mut hashes = String::with_capacity(self.files.len() * 64);
        for file in &self.files {
            hashes.push_str(&file.sha256);
        }
        sha256(hashes.as_bytes())
    }

    pub(crate) fn file_count(&self) -> usize {
        self.files.len()
    }

    fn validate(&self) -> Result<(), CodexRefitError> {
        if self.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(update_error(
                "renderer asset manifest uses an unsupported schema",
            ));
        }
        if self.api_version != RENDERER_ASSET_API_VERSION {
            return Err(update_error(
                "renderer asset bundle requires an unsupported payload API",
            ));
        }
        let asset_version = parse_version(&self.asset_version, "asset")?;
        if !asset_version.pre.is_empty() || !asset_version.build.is_empty() {
            return Err(update_error(
                "renderer asset version must be a stable semantic version",
            ));
        }
        let minimum = parse_version(&self.minimum_opsail_version, "minimum Opsail")?;
        let current = Version::parse(env!("CARGO_PKG_VERSION"))
            .expect("the package version is valid semantic versioning");
        if minimum > current {
            return Err(update_error(
                "renderer asset bundle requires a newer Opsail version",
            ));
        }
        if self.files.len() != RENDERER_ASSET_FILES.len() {
            return Err(update_error(
                "renderer asset manifest does not contain the exact JavaScript allowlist",
            ));
        }
        let mut total = 0usize;
        for (record, expected_name) in self.files.iter().zip(RENDERER_ASSET_FILES) {
            if record.name != expected_name {
                return Err(update_error(
                    "renderer asset manifest contains an unknown or reordered file",
                ));
            }
            if record.bytes == 0 || record.bytes > MAX_RENDERER_ASSET_BYTES {
                return Err(update_error(
                    "renderer asset manifest contains an invalid file size",
                ));
            }
            total = total
                .checked_add(record.bytes)
                .ok_or_else(|| update_error("renderer asset bundle size overflowed"))?;
            if record.sha256.len() != 64
                || !record
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            {
                return Err(update_error(
                    "renderer asset manifest contains an invalid SHA-256 value",
                ));
            }
        }
        if total > MAX_RENDERER_ASSET_TOTAL_BYTES {
            return Err(update_error("renderer asset bundle exceeds its size limit"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RendererAssetSelection {
    pub(crate) bundle: RendererAssetBundle,
    pub(crate) info: RendererAssetInfo,
    pub(crate) warning: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct RendererAssetInstall {
    pub(crate) installed: RendererAssetInfo,
    pub(crate) changed: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct RendererAssetStore {
    state_root: PathBuf,
    root: PathBuf,
}

impl RendererAssetStore {
    pub(crate) fn new(state_root: PathBuf) -> Self {
        Self {
            root: state_root.join("renderer-assets"),
            state_root,
        }
    }

    pub(crate) fn load_or_embedded(&self) -> Result<RendererAssetSelection, CodexRefitError> {
        let embedded = embedded_bundle()?;
        match self.load_installed() {
            Ok(Some(installed)) if installed.version() > embedded.version() => {
                Ok(RendererAssetSelection {
                    info: installed.info(RendererAssetSource::Github),
                    bundle: installed,
                    warning: None,
                })
            }
            Ok(Some(installed)) if installed.version() == embedded.version() => {
                if installed.identity() == embedded.identity() {
                    Ok(RendererAssetSelection {
                        info: installed.info(RendererAssetSource::Github),
                        bundle: installed,
                        warning: None,
                    })
                } else {
                    Ok(RendererAssetSelection {
                        info: embedded.info(RendererAssetSource::Embedded),
                        bundle: embedded,
                        warning: Some(
                            "ignored an installed renderer bundle that reused an embedded version"
                                .to_owned(),
                        ),
                    })
                }
            }
            Ok(Some(_)) => Ok(RendererAssetSelection {
                info: embedded.info(RendererAssetSource::Embedded),
                bundle: embedded,
                warning: Some(
                    "ignored an installed renderer bundle older than this binary".to_owned(),
                ),
            }),
            Ok(None) => Ok(RendererAssetSelection {
                info: embedded.info(RendererAssetSource::Embedded),
                bundle: embedded,
                warning: None,
            }),
            Err(error) => Ok(RendererAssetSelection {
                info: embedded.info(RendererAssetSource::Embedded),
                bundle: embedded,
                warning: Some(format!(
                    "{}: {error}; using embedded renderer assets",
                    error.code().as_str()
                )),
            }),
        }
    }

    pub(crate) fn install(
        &self,
        candidate: &RendererAssetBundle,
    ) -> Result<RendererAssetInstall, CodexRefitError> {
        let previous = self.load_or_embedded()?;
        let installed_state = self.load_installed();
        let ordering = candidate.version().cmp(&previous.bundle.version());
        if ordering.is_lt() {
            return Err(update_error(
                "renderer asset downgrade was rejected by version policy",
            ));
        }
        if ordering.is_eq() && candidate.identity() != previous.bundle.identity() {
            return Err(update_error(
                "renderer asset version was reused with different contents",
            ));
        }

        let installed_matches = matches!(
            &installed_state,
            Ok(Some(installed)) if installed.identity() == candidate.identity()
        );
        let embedded_without_pointer = matches!(&installed_state, Ok(None))
            && previous.info.source == RendererAssetSource::Embedded
            && ordering.is_eq();
        if installed_matches || embedded_without_pointer {
            return Ok(RendererAssetInstall {
                installed: previous.info,
                changed: false,
            });
        }

        self.write_bundle(candidate)?;
        Ok(RendererAssetInstall {
            installed: candidate.info(RendererAssetSource::Github),
            changed: true,
        })
    }

    fn load_installed(&self) -> Result<Option<RendererAssetBundle>, CodexRefitError> {
        if !existing_directory(&self.state_root)? || !existing_directory(&self.root)? {
            return Ok(None);
        }
        let pointer_path = self.root.join("current.json");
        let Some(pointer_bytes) = read_regular_file(&pointer_path, 16 * 1024)? else {
            return Ok(None);
        };
        let pointer: CurrentPointer = serde_json::from_slice(&pointer_bytes)
            .map_err(|_| update_error("renderer asset current pointer is invalid"))?;
        if pointer.schema_version != CURRENT_POINTER_SCHEMA_VERSION
            || !valid_directory_name(&pointer.directory)
            || parse_version(&pointer.version, "installed asset").is_err()
        {
            return Err(update_error(
                "renderer asset current pointer uses an unsupported schema",
            ));
        }
        let versions = self.root.join("versions");
        ensure_existing_directory(&versions)?;
        let directory = versions.join(&pointer.directory);
        ensure_existing_directory(&directory)?;
        let manifest_path = directory.join("manifest.json");
        let manifest_bytes = read_regular_file(&manifest_path, MAX_MANIFEST_BYTES)?
            .ok_or_else(|| update_error("installed renderer asset manifest is missing"))?;
        let manifest = RendererAssetManifest::parse(&manifest_bytes)?;
        if manifest.asset_version != pointer.version {
            return Err(update_error(
                "installed renderer asset version does not match its pointer",
            ));
        }
        let mut files = BTreeMap::new();
        for name in manifest.file_names() {
            let bytes = read_regular_file(&directory.join(name), MAX_RENDERER_ASSET_BYTES)?
                .ok_or_else(|| update_error("installed renderer JavaScript is missing"))?;
            files.insert(name.to_owned(), bytes);
        }
        RendererAssetBundle::from_parts(&manifest_bytes, files).map(Some)
    }

    fn write_bundle(&self, bundle: &RendererAssetBundle) -> Result<(), CodexRefitError> {
        prepare_private_directory(&self.state_root)?;
        prepare_private_directory(&self.root)?;
        let versions = self.root.join("versions");
        prepare_private_directory(&versions)?;
        let suffix = unique_suffix();
        let temporary = versions.join(format!(".install-{suffix}"));
        fs::create_dir(&temporary)
            .map_err(|_| update_error("could not create renderer asset staging directory"))?;
        set_directory_permissions(&temporary)?;
        let directory_name = format!(
            "{}-{}-{suffix}",
            bundle.manifest.asset_version,
            &bundle.identity()[..16]
        );
        let final_directory = versions.join(&directory_name);
        let result = (|| {
            for (name, source) in bundle.sources.iter() {
                write_new_private_file(&temporary.join(name), source.as_bytes())?;
            }
            let mut manifest_bytes = serde_json::to_vec_pretty(&bundle.manifest)
                .map_err(|_| update_error("could not serialize renderer asset manifest"))?;
            manifest_bytes.push(b'\n');
            write_new_private_file(&temporary.join("manifest.json"), &manifest_bytes)?;
            fs::rename(&temporary, &final_directory)
                .map_err(|_| update_error("could not commit renderer asset version"))?;
            let pointer = CurrentPointer {
                schema_version: CURRENT_POINTER_SCHEMA_VERSION,
                version: bundle.manifest.asset_version.clone(),
                directory: directory_name,
            };
            self.write_current_pointer(&pointer)
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&temporary);
        }
        result
    }

    fn write_current_pointer(&self, pointer: &CurrentPointer) -> Result<(), CodexRefitError> {
        let temporary = self.root.join(format!(".current-{}.tmp", unique_suffix()));
        let mut bytes = serde_json::to_vec_pretty(pointer)
            .map_err(|_| update_error("could not serialize renderer asset pointer"))?;
        bytes.push(b'\n');
        let result = (|| {
            write_new_private_file(&temporary, &bytes)?;
            fs::rename(&temporary, self.root.join("current.json"))
                .map_err(|_| update_error("could not activate renderer asset version"))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

pub(crate) fn embedded_bundle() -> Result<RendererAssetBundle, CodexRefitError> {
    let files = EMBEDDED_SOURCES
        .into_iter()
        .map(|(name, source)| (name.to_owned(), source.as_bytes().to_vec()))
        .collect();
    RendererAssetBundle::from_parts(EMBEDDED_MANIFEST.as_bytes(), files)
}

#[cfg(test)]
pub(crate) fn test_bundle_with_change(
    version: &str,
    changed_file: Option<&str>,
) -> RendererAssetBundle {
    let mut manifest: RendererAssetManifest =
        serde_json::from_str(EMBEDDED_MANIFEST).expect("embedded test manifest is valid");
    manifest.asset_version = version.to_owned();
    let mut files = EMBEDDED_SOURCES
        .into_iter()
        .map(|(name, source)| (name.to_owned(), source.as_bytes().to_vec()))
        .collect::<BTreeMap<_, _>>();
    if let Some(name) = changed_file {
        let bytes = files
            .get_mut(name)
            .expect("changed test file must be allowlisted");
        bytes.extend_from_slice(b"\n// verified update fixture\n");
        let record = manifest
            .files
            .iter_mut()
            .find(|record| record.name == name)
            .expect("changed test file must exist in the manifest");
        record.bytes = bytes.len();
        record.sha256 = sha256(bytes);
    }
    RendererAssetBundle::from_parts(
        &serde_json::to_vec(&manifest).expect("test manifest serializes"),
        files,
    )
    .expect("test renderer bundle is valid")
}

fn validate_javascript(source: &str) -> Result<(), CodexRefitError> {
    if source.contains('\0') {
        return Err(update_error("renderer JavaScript contains a null byte"));
    }
    for forbidden in [
        "fetch(",
        "XMLHttpRequest",
        "WebSocket(",
        "eval(",
        "new Function",
        "/v1/",
        "responses.create",
        "chat.completions",
    ] {
        if source.contains(forbidden) {
            return Err(update_error(
                "renderer JavaScript violates the local-only execution policy",
            ));
        }
    }
    Ok(())
}

fn parse_version(value: &str, kind: &str) -> Result<Version, CodexRefitError> {
    if value.len() > 64 || !value.is_ascii() {
        return Err(update_error(format!("renderer {kind} version is invalid")));
    }
    Version::parse(value).map_err(|_| update_error(format!("renderer {kind} version is invalid")))
}

fn sha256(bytes: &[u8]) -> String {
    let digest = digest::digest(&digest::SHA256, bytes);
    let mut output = String::with_capacity(64);
    for byte in digest.as_ref() {
        write!(&mut output, "{byte:02x}").expect("writing into a String cannot fail");
    }
    output
}

fn valid_directory_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 192
        && !value.starts_with('.')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn read_regular_file(path: &Path, limit: usize) -> Result<Option<Vec<u8>>, CodexRefitError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(update_error("could not inspect renderer asset storage")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(update_error(
            "renderer asset storage contains a non-regular file",
        ));
    }
    if metadata.len() > limit as u64 {
        return Err(update_error(
            "renderer asset storage exceeds its size limit",
        ));
    }
    fs::read(path)
        .map(Some)
        .map_err(|_| update_error("could not read renderer asset storage"))
}

fn prepare_private_directory(path: &Path) -> Result<(), CodexRefitError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(update_error(
                "renderer asset storage is not a regular directory",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {
            fs::create_dir_all(path)
                .map_err(|_| update_error("could not create renderer asset storage"))?;
            ensure_existing_directory(path)?;
        }
        Err(_) => return Err(update_error("could not inspect renderer asset storage")),
    }
    set_directory_permissions(path)
}

fn ensure_existing_directory(path: &Path) -> Result<(), CodexRefitError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| update_error("could not inspect renderer asset storage"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(update_error(
            "renderer asset storage is not a regular directory",
        ));
    }
    Ok(())
}

fn existing_directory(path: &Path) -> Result<bool, CodexRefitError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => Err(
            update_error("renderer asset storage is not a regular directory"),
        ),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(_) => Err(update_error("could not inspect renderer asset storage")),
    }
}

fn write_new_private_file(path: &Path, bytes: &[u8]) -> Result<(), CodexRefitError> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|_| update_error("could not create renderer asset file"))?;
    set_file_permissions(path)?;
    file.write_all(bytes)
        .map_err(|_| update_error("could not write renderer asset file"))?;
    file.sync_all()
        .map_err(|_| update_error("could not flush renderer asset file"))
}

fn unique_suffix() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{timestamp}-{sequence}", std::process::id())
}

#[cfg(unix)]
fn set_directory_permissions(path: &Path) -> Result<(), CodexRefitError> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| update_error("could not protect renderer asset storage"))
}

#[cfg(not(unix))]
fn set_directory_permissions(_path: &Path) -> Result<(), CodexRefitError> {
    Ok(())
}

#[cfg(unix)]
fn set_file_permissions(path: &Path) -> Result<(), CodexRefitError> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|_| update_error("could not protect renderer asset file"))
}

#[cfg(not(unix))]
fn set_file_permissions(_path: &Path) -> Result<(), CodexRefitError> {
    Ok(())
}

fn update_error(message: impl Into<String>) -> CodexRefitError {
    CodexRefitError::new(CodexRefitErrorCode::UpdateFailed, message)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn bundle_with_version(version: &str) -> RendererAssetBundle {
        let mut manifest: RendererAssetManifest = serde_json::from_str(EMBEDDED_MANIFEST).unwrap();
        manifest.asset_version = version.to_owned();
        let manifest = serde_json::to_vec(&manifest).unwrap();
        let files = EMBEDDED_SOURCES
            .into_iter()
            .map(|(name, source)| (name.to_owned(), source.as_bytes().to_vec()))
            .collect();
        RendererAssetBundle::from_parts(&manifest, files).unwrap()
    }

    #[test]
    fn embedded_manifest_matches_every_allowlisted_javascript_file() {
        let bundle = embedded_bundle().unwrap();
        assert_eq!(bundle.manifest.files.len(), RENDERER_ASSET_FILES.len());
        assert_eq!(bundle.info(RendererAssetSource::Embedded).version, "1.0.0");
    }

    #[test]
    fn bundle_rejects_missing_files_bad_hashes_and_network_code() {
        let manifest = EMBEDDED_MANIFEST.as_bytes();
        let mut files = EMBEDDED_SOURCES
            .into_iter()
            .map(|(name, source)| (name.to_owned(), source.as_bytes().to_vec()))
            .collect::<BTreeMap<_, _>>();
        files.remove(RENDERER_ASSET_FILES[0]);
        assert_eq!(
            RendererAssetBundle::from_parts(manifest, files)
                .unwrap_err()
                .code(),
            CodexRefitErrorCode::UpdateFailed
        );

        let mut manifest: RendererAssetManifest = serde_json::from_str(EMBEDDED_MANIFEST).unwrap();
        manifest.files[0].sha256 = "0".repeat(64);
        let manifest = serde_json::to_vec(&manifest).unwrap();
        let files = EMBEDDED_SOURCES
            .into_iter()
            .map(|(name, source)| (name.to_owned(), source.as_bytes().to_vec()))
            .collect();
        assert_eq!(
            RendererAssetBundle::from_parts(&manifest, files)
                .unwrap_err()
                .code(),
            CodexRefitErrorCode::UpdateFailed
        );

        let mut manifest: RendererAssetManifest = serde_json::from_str(EMBEDDED_MANIFEST).unwrap();
        let mut files = EMBEDDED_SOURCES
            .into_iter()
            .map(|(name, source)| (name.to_owned(), source.as_bytes().to_vec()))
            .collect::<BTreeMap<_, _>>();
        let name = RENDERER_ASSET_FILES[0];
        let bytes = files.get_mut(name).unwrap();
        bytes.extend_from_slice(b"\nfetch('https://example.invalid')\n");
        let record = manifest
            .files
            .iter_mut()
            .find(|record| record.name == name)
            .unwrap();
        record.bytes = bytes.len();
        record.sha256 = sha256(bytes);
        assert_eq!(
            RendererAssetBundle::from_parts(&serde_json::to_vec(&manifest).unwrap(), files)
                .unwrap_err()
                .code(),
            CodexRefitErrorCode::UpdateFailed
        );
    }

    #[test]
    fn manifest_rejects_incompatible_or_non_stable_versions() {
        for (field, value) in [
            ("apiVersion", serde_json::json!(2)),
            ("minimumOpsailVersion", serde_json::json!("99.0.0")),
            ("assetVersion", serde_json::json!("1.1.0-beta.1")),
        ] {
            let mut manifest: serde_json::Value = serde_json::from_str(EMBEDDED_MANIFEST).unwrap();
            manifest[field] = value;
            assert_eq!(
                RendererAssetManifest::parse(&serde_json::to_vec(&manifest).unwrap())
                    .unwrap_err()
                    .code(),
                CodexRefitErrorCode::UpdateFailed
            );
        }
    }

    #[test]
    fn version_store_installs_atomically_and_reloads_the_selected_bundle() {
        let directory = tempdir().unwrap();
        let store = RendererAssetStore::new(directory.path().to_owned());
        let candidate = bundle_with_version("1.1.0");
        let install = store.install(&candidate).unwrap();
        assert!(install.changed);
        assert_eq!(install.installed.version, "1.1.0");

        let loaded = store.load_or_embedded().unwrap();
        assert_eq!(loaded.info.source, RendererAssetSource::Github);
        assert_eq!(loaded.info.version, "1.1.0");
        assert!(loaded.warning.is_none());
        let root = directory.path().join("renderer-assets");
        assert!(root.join("current.json").is_file());
        assert!(fs::read_dir(root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp")
        }));
    }

    #[test]
    fn corrupted_pointer_falls_back_to_embedded_and_can_be_repaired() {
        let directory = tempdir().unwrap();
        let store = RendererAssetStore::new(directory.path().to_owned());
        fs::create_dir_all(directory.path().join("renderer-assets")).unwrap();
        fs::write(
            directory.path().join("renderer-assets/current.json"),
            b"not-json",
        )
        .unwrap();

        let fallback = store.load_or_embedded().unwrap();
        assert_eq!(fallback.info.source, RendererAssetSource::Embedded);
        assert!(fallback.warning.is_some());

        let install = store.install(&embedded_bundle().unwrap()).unwrap();
        assert!(install.changed);
        assert_eq!(install.installed.source, RendererAssetSource::Github);
        assert!(store.load_or_embedded().unwrap().warning.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_asset_storage_is_never_loaded_or_overwritten() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let outside = tempdir().unwrap();
        symlink(outside.path(), directory.path().join("renderer-assets")).unwrap();
        let store = RendererAssetStore::new(directory.path().to_owned());
        let selection = store.load_or_embedded().unwrap();
        assert_eq!(selection.info.source, RendererAssetSource::Embedded);
        assert!(selection.warning.is_some());
        assert_eq!(
            store
                .install(&bundle_with_version("1.1.0"))
                .unwrap_err()
                .code(),
            CodexRefitErrorCode::UpdateFailed
        );
        assert!(fs::read_dir(outside.path()).unwrap().next().is_none());
    }

    #[test]
    fn version_policy_rejects_downgrades_and_same_version_content_reuse() {
        let directory = tempdir().unwrap();
        let store = RendererAssetStore::new(directory.path().to_owned());
        store.install(&bundle_with_version("1.2.0")).unwrap();
        assert_eq!(
            store
                .install(&bundle_with_version("1.1.0"))
                .unwrap_err()
                .code(),
            CodexRefitErrorCode::UpdateFailed
        );

        let mut conflicting = bundle_with_version("1.2.0");
        conflicting.manifest.minimum_opsail_version = "0.0.1".to_owned();
        assert_eq!(
            store.install(&conflicting).unwrap_err().code(),
            CodexRefitErrorCode::UpdateFailed
        );
    }
}
