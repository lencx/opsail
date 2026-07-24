use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{Read as _, Write as _};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use miette::{IntoDiagnostic, Result, WrapErr, miette};
use opsail_gateway_model::{
    EventMappingProfileV1, MAX_GATEWAY_CONCURRENT_REQUESTS, MAX_GATEWAY_REQUEST_BYTES,
    MAX_GATEWAY_REQUEST_TIMEOUT, MIN_GATEWAY_REQUEST_BYTES, PromptCacheRouting, ReasoningDisplay,
    validate_upstream_url,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use url::Url;

use crate::{with_trailing_newline, write_stdout};

const CONFIG_SCHEMA_VERSION: u32 = 1;
const MAX_CONFIG_BYTES: u64 = 256 * 1024;
const MIN_USER_PORT: u16 = 1024;
const MAX_ROUTE_COUNT: usize = 128;
const MAX_ROUTE_VALUE_BYTES: usize = 512;
const MAX_AUTH_REFRESH_INTERVAL_MS: u64 = 86_400_000;
pub(crate) const DEFAULT_CODEX_DEBUG_PORT: u16 = 55321;
pub(crate) const DEFAULT_SIGNED_IN_PROVIDER: &str = "openai";

const DEFAULT_CONFIG_TEMPLATE: &str = r#"version = 1

[refit.codex]
debug_port = 55321

[refit.codex.model_picker]
default_provider = "openai"

[refit.codex.model_picker.routes]
# "sf-deepseek-v3.2" = "opsail-gateway-model"

# Uncomment this section when using `opsail gateway model serve`.
# [gateway.model]
# listen = "127.0.0.1:55322"
# upstream = "http://127.0.0.1:8317/v1"
# reasoning_display = "commentary"
# request_timeout_seconds = 600
# stream_idle_timeout_seconds = 120
# max_request_bytes = 33554432
# max_concurrent_requests = 8
# prompt_cache_routing = "strip"
# event_mapping_file = "mappings/provider-events.toml"
#
# [gateway.model.upstream_auth]
# source = "codex-provider-command"
# provider = "cliproxy"
#
# For a standalone command instead:
# source = "command"
# command = "/absolute/path/to/provider-token"
# refresh_interval_ms = 300000 # 0 means refresh only after an upstream 401
"#;

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub(crate) struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Create a private starter config without overwriting an existing file.
    Init,
    /// Print the resolved config path.
    Path,
    /// Print the validated effective file configuration as TOML.
    Show,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OpsailConfig {
    version: u32,
    #[serde(default)]
    pub refit: RefitConfig,
    #[serde(default)]
    pub gateway: GatewayFileConfig,
}

impl Default for OpsailConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_SCHEMA_VERSION,
            refit: RefitConfig::default(),
            gateway: GatewayFileConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RefitConfig {
    #[serde(default)]
    pub codex: CodexConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodexConfig {
    pub debug_port: Option<u16>,
    #[serde(default)]
    pub model_picker: ModelPickerConfig,
}

impl CodexConfig {
    pub fn debug_port(&self) -> u16 {
        self.debug_port.unwrap_or(DEFAULT_CODEX_DEBUG_PORT)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelPickerConfig {
    pub default_provider: Option<String>,
    #[serde(default)]
    pub routes: BTreeMap<String, String>,
}

impl ModelPickerConfig {
    pub fn default_provider(&self) -> &str {
        self.default_provider
            .as_deref()
            .unwrap_or(DEFAULT_SIGNED_IN_PROVIDER)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GatewayFileConfig {
    #[serde(default)]
    pub model: ModelGatewayFileConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelGatewayFileConfig {
    pub listen: Option<SocketAddr>,
    pub upstream: Option<Url>,
    pub reasoning_display: Option<ReasoningDisplay>,
    pub request_timeout_seconds: Option<u64>,
    pub stream_idle_timeout_seconds: Option<u64>,
    pub max_request_bytes: Option<usize>,
    pub max_concurrent_requests: Option<usize>,
    pub prompt_cache_routing: Option<PromptCacheRouting>,
    pub event_mapping: Option<EventMappingProfileV1>,
    pub event_mapping_file: Option<PathBuf>,
    pub upstream_auth: Option<UpstreamAuthFileConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum UpstreamAuthFileConfig {
    Environment {
        name: String,
    },
    Command {
        command: PathBuf,
        #[serde(default)]
        args: Vec<String>,
        cwd: Option<PathBuf>,
        timeout_ms: Option<u64>,
        refresh_interval_ms: Option<u64>,
    },
    CodexProviderCommand {
        provider: String,
        config: Option<PathBuf>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct ConfigLocation {
    path: PathBuf,
    explicit: bool,
}

impl ConfigLocation {
    pub fn resolve(explicit: Option<PathBuf>) -> Result<Self> {
        match explicit {
            Some(path) => Ok(Self {
                path: absolute_path(path)?,
                explicit: true,
            }),
            None => {
                let home = user_home_dir().ok_or_else(|| {
                    miette!(
                        "could not resolve the user home directory; pass --config <PATH> explicitly"
                    )
                })?;
                Ok(Self {
                    path: home.join(".opsail").join("config.toml"),
                    explicit: false,
                })
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<OpsailConfig> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !self.explicit => {
                return Ok(OpsailConfig::default());
            }
            Err(error) => {
                return Err(error)
                    .into_diagnostic()
                    .wrap_err_with(|| format!("failed to inspect {}", self.path.display()));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(miette!(
                "Opsail config must be a regular, non-symlink file: {}",
                self.path.display()
            ));
        }
        if metadata.len() > MAX_CONFIG_BYTES {
            return Err(miette!(
                "Opsail config exceeds the {} byte limit: {}",
                MAX_CONFIG_BYTES,
                self.path.display()
            ));
        }

        let mut source = String::with_capacity(metadata.len() as usize);
        OpenOptions::new()
            .read(true)
            .open(&self.path)
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to open {}", self.path.display()))?
            .take(MAX_CONFIG_BYTES + 1)
            .read_to_string(&mut source)
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to read {}", self.path.display()))?;
        let mut config: OpsailConfig = toml::from_str(&source)
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to parse {}", self.path.display()))?;
        config.resolve_paths(
            self.path
                .parent()
                .ok_or_else(|| miette!("Opsail config path has no parent directory"))?,
        );
        config.validate()?;
        Ok(config)
    }

    pub fn initialize(&self) -> Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| miette!("Opsail config path has no parent directory"))?;
        fs::create_dir_all(parent)
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to create {}", parent.display()))?;
        let parent_metadata = fs::symlink_metadata(parent)
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to inspect {}", parent.display()))?;
        if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
            return Err(miette!(
                "Opsail config parent must be a regular, non-symlink directory: {}",
                parent.display()
            ));
        }
        set_private_directory_permissions(parent)?;

        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        set_private_file_creation_mode(&mut options);
        let mut file = options
            .open(&self.path)
            .into_diagnostic()
            .wrap_err_with(|| {
                format!(
                    "failed to create {}; the file is never overwritten",
                    self.path.display()
                )
            })?;
        file.write_all(DEFAULT_CONFIG_TEMPLATE.as_bytes())
            .and_then(|()| file.flush())
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to write {}", self.path.display()))
    }
}

pub(crate) fn run(args: ConfigArgs, explicit: Option<PathBuf>) -> Result<()> {
    let location = ConfigLocation::resolve(explicit)?;
    match args.command {
        ConfigCommand::Init => {
            location.initialize()?;
            let output = serde_json::to_string_pretty(&json!({
                "created": true,
                "path": location.path(),
                "version": CONFIG_SCHEMA_VERSION,
            }))
            .into_diagnostic()
            .wrap_err("failed to serialize the config initialization result")?;
            write_stdout(with_trailing_newline(output).as_bytes())
        }
        ConfigCommand::Path => {
            let output = location.path().to_string_lossy().into_owned();
            write_stdout(with_trailing_newline(output).as_bytes())
        }
        ConfigCommand::Show => {
            let config = location.load()?;
            let output = toml::to_string_pretty(&config)
                .into_diagnostic()
                .wrap_err("failed to serialize the effective Opsail config")?;
            write_stdout(with_trailing_newline(output).as_bytes())
        }
    }
}

impl OpsailConfig {
    fn resolve_paths(&mut self, config_directory: &Path) {
        if let Some(path) = self.gateway.model.event_mapping_file.as_mut()
            && path.is_relative()
        {
            *path = config_directory.join(&*path);
        }
        if let Some(auth) = self.gateway.model.upstream_auth.as_mut() {
            match auth {
                UpstreamAuthFileConfig::Command { command, cwd, .. } => {
                    if command.is_relative() && command.components().count() > 1 {
                        *command = config_directory.join(&*command);
                    }
                    if let Some(cwd) = cwd
                        && cwd.is_relative()
                    {
                        *cwd = config_directory.join(&*cwd);
                    }
                }
                UpstreamAuthFileConfig::CodexProviderCommand {
                    config: Some(path), ..
                } if path.is_relative() => {
                    *path = config_directory.join(&*path);
                }
                UpstreamAuthFileConfig::Environment { .. }
                | UpstreamAuthFileConfig::CodexProviderCommand { .. } => {}
            }
        }
    }

    fn validate(&self) -> Result<()> {
        if self.version != CONFIG_SCHEMA_VERSION {
            return Err(miette!(
                "unsupported Opsail config version {}; expected {}",
                self.version,
                CONFIG_SCHEMA_VERSION
            ));
        }
        if self
            .refit
            .codex
            .debug_port
            .is_some_and(|port| port < MIN_USER_PORT)
        {
            return Err(miette!(
                "refit.codex.debug_port must be between {MIN_USER_PORT} and 65535"
            ));
        }
        validate_route_value(
            "refit.codex.model_picker.default_provider",
            self.refit.codex.model_picker.default_provider.as_deref(),
        )?;
        if self.refit.codex.model_picker.routes.len() > MAX_ROUTE_COUNT {
            return Err(miette!(
                "refit.codex.model_picker.routes may contain at most {MAX_ROUTE_COUNT} entries"
            ));
        }
        for (model, provider) in &self.refit.codex.model_picker.routes {
            validate_route_value("model route key", Some(model))?;
            validate_route_value("model route provider", Some(provider))?;
        }
        self.gateway.model.validate()
    }
}

impl ModelGatewayFileConfig {
    fn validate(&self) -> Result<()> {
        if let Some(listen) = self.listen {
            if !listen.ip().is_loopback() {
                return Err(miette!(
                    "gateway.model.listen must use a loopback IP address"
                ));
            }
            if listen.port() < MIN_USER_PORT {
                return Err(miette!(
                    "gateway.model.listen port must be between {MIN_USER_PORT} and 65535"
                ));
            }
        }
        if let Some(upstream) = &self.upstream {
            validate_upstream_url(upstream)
                .into_diagnostic()
                .wrap_err("invalid gateway.model.upstream")?;
        }
        if self
            .request_timeout_seconds
            .is_some_and(|value| value == 0 || value > MAX_GATEWAY_REQUEST_TIMEOUT.as_secs())
        {
            return Err(miette!(
                "gateway.model.request_timeout_seconds must be between 1 and {}",
                MAX_GATEWAY_REQUEST_TIMEOUT.as_secs()
            ));
        }
        if self
            .stream_idle_timeout_seconds
            .is_some_and(|value| value == 0 || value > MAX_GATEWAY_REQUEST_TIMEOUT.as_secs())
        {
            return Err(miette!(
                "gateway.model.stream_idle_timeout_seconds must be between 1 and {}",
                MAX_GATEWAY_REQUEST_TIMEOUT.as_secs()
            ));
        }
        if self.max_request_bytes.is_some_and(|value| {
            !(MIN_GATEWAY_REQUEST_BYTES..=MAX_GATEWAY_REQUEST_BYTES).contains(&value)
        }) {
            return Err(miette!(
                "gateway.model.max_request_bytes must be between {MIN_GATEWAY_REQUEST_BYTES} and {MAX_GATEWAY_REQUEST_BYTES}"
            ));
        }
        if self
            .max_concurrent_requests
            .is_some_and(|value| value == 0 || value > MAX_GATEWAY_CONCURRENT_REQUESTS)
        {
            return Err(miette!(
                "gateway.model.max_concurrent_requests must be between 1 and {MAX_GATEWAY_CONCURRENT_REQUESTS}"
            ));
        }
        if let Some(profile) = &self.event_mapping {
            profile
                .validate()
                .into_diagnostic()
                .wrap_err("invalid gateway.model.event_mapping")?;
        }
        if self.event_mapping.is_some() && self.event_mapping_file.is_some() {
            return Err(miette!(
                "gateway.model.event_mapping and gateway.model.event_mapping_file are mutually exclusive"
            ));
        }
        if let Some(auth) = &self.upstream_auth {
            auth.validate()?;
        }
        Ok(())
    }
}

impl UpstreamAuthFileConfig {
    fn validate(&self) -> Result<()> {
        match self {
            Self::Environment { name } => {
                validate_env_name("gateway.model.upstream_auth.name", name)
            }
            Self::Command {
                command,
                args,
                cwd,
                timeout_ms,
                refresh_interval_ms,
            } => {
                validate_command_path("gateway.model.upstream_auth.command", command)?;
                validate_command_args("gateway.model.upstream_auth.args", args)?;
                if let Some(cwd) = cwd
                    && (!cwd.is_absolute() || cwd.as_os_str().is_empty())
                {
                    return Err(miette!(
                        "gateway.model.upstream_auth.cwd must resolve to a non-empty absolute path"
                    ));
                }
                if timeout_ms.is_some_and(|value| !(1..=60_000).contains(&value)) {
                    return Err(miette!(
                        "gateway.model.upstream_auth.timeout_ms must be between 1 and 60000"
                    ));
                }
                if refresh_interval_ms.is_some_and(|value| value > MAX_AUTH_REFRESH_INTERVAL_MS) {
                    return Err(miette!(
                        "gateway.model.upstream_auth.refresh_interval_ms must be 0 or at most \
                         {MAX_AUTH_REFRESH_INTERVAL_MS}"
                    ));
                }
                Ok(())
            }
            Self::CodexProviderCommand { provider, config } => {
                validate_route_value(
                    "gateway.model.upstream_auth.provider",
                    Some(provider.as_str()),
                )?;
                if config
                    .as_ref()
                    .is_some_and(|path| !path.is_absolute() || path.as_os_str().is_empty())
                {
                    return Err(miette!(
                        "gateway.model.upstream_auth.config must resolve to a non-empty absolute path"
                    ));
                }
                Ok(())
            }
        }
    }
}

fn validate_command_path(label: &str, value: &Path) -> Result<()> {
    let text = value.to_string_lossy();
    if text.is_empty()
        || text.len() > 1024
        || text.trim() != text
        || text.chars().any(char::is_control)
    {
        return Err(miette!(
            "{label} must be non-empty, trimmed, control-free, and at most 1024 bytes"
        ));
    }
    Ok(())
}

fn validate_command_args(label: &str, values: &[String]) -> Result<()> {
    if values.len() > 64 {
        return Err(miette!("{label} may contain at most 64 values"));
    }
    if values.iter().any(|value| {
        value.len() > 4096 || value.contains('\0') || value.chars().any(char::is_control)
    }) {
        return Err(miette!(
            "{label} values must be control-free and at most 4096 bytes"
        ));
    }
    Ok(())
}

fn validate_route_value(name: &str, value: Option<&str>) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.is_empty()
        || value.len() > MAX_ROUTE_VALUE_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(miette!(
            "{name} must be non-empty, trimmed, control-free, and at most {MAX_ROUTE_VALUE_BYTES} bytes"
        ));
    }
    Ok(())
}

fn validate_env_name(label: &str, value: &str) -> Result<()> {
    let mut chars = value.chars();
    if value.len() > 128
        || !chars
            .next()
            .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        || !chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
    {
        return Err(miette!(
            "{label} must be a portable environment variable name of at most 128 bytes"
        ));
    }
    Ok(())
}

fn absolute_path(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path);
    }
    Ok(std::env::current_dir()
        .into_diagnostic()
        .wrap_err("failed to resolve the current directory")?
        .join(path))
}

pub(crate) fn user_home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let value: Option<OsString> = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let value: Option<OsString> = std::env::var_os("HOME");
    value
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to secure {}", path.display()))
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_creation_mode(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt as _;

    options.mode(0o600);
}

#[cfg(not(unix))]
fn set_private_file_creation_mode(_options: &mut OpenOptions) {}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn explicit(path: PathBuf) -> ConfigLocation {
        ConfigLocation::resolve(Some(path)).unwrap()
    }

    #[test]
    fn absent_default_config_uses_bounded_defaults() {
        let location = ConfigLocation {
            path: PathBuf::from("/path/that/does/not/exist"),
            explicit: false,
        };
        let config = location.load().unwrap();
        assert_eq!(config.refit.codex.debug_port(), DEFAULT_CODEX_DEBUG_PORT);
        assert_eq!(
            config.refit.codex.model_picker.default_provider(),
            DEFAULT_SIGNED_IN_PROVIDER
        );
        assert_eq!(
            config.gateway.model.reasoning_display, None,
            "file defaults remain distinguishable from resolved defaults"
        );
    }

    #[test]
    fn explicit_missing_config_is_an_error() {
        let directory = tempdir().unwrap();
        let error = explicit(directory.path().join("missing.toml"))
            .load()
            .unwrap_err()
            .to_string();
        assert!(error.contains("failed to inspect"));
    }

    #[test]
    fn config_parses_routes_and_gateway_settings() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(
            &path,
            r#"
version = 1

[refit.codex]
debug_port = 55400

[refit.codex.model_picker]
default_provider = "openai"

[refit.codex.model_picker.routes]
"sf-deepseek-v3.2" = "opsail-gateway-model"

[gateway.model]
listen = "127.0.0.1:55401"
upstream = "http://127.0.0.1:8317/v1"
reasoning_display = "strict"
request_timeout_seconds = 90
max_request_bytes = 1048576
max_concurrent_requests = 4
prompt_cache_routing = "provider-scoped"
event_mapping_file = "mappings/provider.toml"

[gateway.model.upstream_auth]
source = "command"
command = "bin/provider-token"
args = ["account-a"]
cwd = "runtime"
timeout_ms = 4000
refresh_interval_ms = 60000
"#,
        )
        .unwrap();

        let config = explicit(path).load().unwrap();
        assert_eq!(config.refit.codex.debug_port(), 55400);
        assert_eq!(
            config.refit.codex.model_picker.routes["sf-deepseek-v3.2"],
            "opsail-gateway-model"
        );
        assert_eq!(
            config.gateway.model.listen,
            Some("127.0.0.1:55401".parse().unwrap())
        );
        assert_eq!(
            config.gateway.model.reasoning_display,
            Some(ReasoningDisplay::Strict)
        );
        assert_eq!(
            config.gateway.model.prompt_cache_routing,
            Some(PromptCacheRouting::ProviderScoped)
        );
        assert_eq!(
            config.gateway.model.event_mapping_file,
            Some(directory.path().join("mappings/provider.toml"))
        );
        match config.gateway.model.upstream_auth.unwrap() {
            UpstreamAuthFileConfig::Command {
                command,
                args,
                cwd,
                timeout_ms,
                refresh_interval_ms,
            } => {
                assert_eq!(command, directory.path().join("bin/provider-token"));
                assert_eq!(args, ["account-a"]);
                assert_eq!(cwd, Some(directory.path().join("runtime")));
                assert_eq!(timeout_ms, Some(4000));
                assert_eq!(refresh_interval_ms, Some(60000));
            }
            other => panic!("unexpected upstream auth: {other:?}"),
        }
    }

    #[test]
    fn config_rejects_unknown_fields_and_unsafe_gateway_endpoints() {
        for source in [
            "version = 1\nunknown = true\n",
            "version = 2\n",
            "version = 1\n[gateway.model]\nlisten = \"0.0.0.0:55322\"\n",
            "version = 1\n[gateway.model]\nupstream = \"http://example.com/v1\"\n",
            "version = 1\n[gateway.model]\nupstream = \"https://example.com/api\"\n",
            "version = 1\n[gateway.model]\nforward_client_authorization = true\n",
            "version = 1\n[gateway.model]\nupstream_bearer_env = \"NOT-PORTABLE\"\n",
            "version = 1\n[gateway.model.upstream_auth]\nsource = \"environment\"\nname = \"NOT-PORTABLE\"\n",
            "version = 1\n[gateway.model.upstream_auth]\nsource = \"command\"\ncommand = \"token\"\nrefresh_interval_ms = 86400001\n",
            "version = 1\n[gateway.model]\nevent_mapping_file = \"mapping.toml\"\n[gateway.model.event_mapping]\nversion = 1\ndiscriminator = \"/type\"\n[[gateway.model.event_mapping.rules]]\nmatch = \"done\"\nemit = \"run-completed\"\n",
        ] {
            let directory = tempdir().unwrap();
            let path = directory.path().join("config.toml");
            fs::write(&path, source).unwrap();
            assert!(explicit(path).load().is_err(), "{source}");
        }
    }

    #[test]
    fn initialize_is_private_and_never_overwrites() {
        let directory = tempdir().unwrap();
        let path = directory.path().join(".opsail").join("config.toml");
        let location = explicit(path.clone());
        location.initialize().unwrap();
        let source = fs::read_to_string(&path).unwrap();
        assert!(source.starts_with("version = 1\n"));
        assert!(source.contains("[refit.codex.model_picker.routes]"));
        assert!(location.initialize().is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            assert_eq!(
                fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn config_rejects_symlink_files() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let target = directory.path().join("target.toml");
        let link = directory.path().join("config.toml");
        fs::write(&target, "version = 1\n").unwrap();
        symlink(&target, &link).unwrap();
        assert!(explicit(link).load().is_err());
    }
}
