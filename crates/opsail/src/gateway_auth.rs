use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Args;
use http::HeaderValue;
use miette::{IntoDiagnostic, Result, WrapErr, miette};
use opsail_gateway_model::{
    GatewayAuthorization, GatewayBearerFuture, GatewayBearerProvider, GatewayError,
};
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::Command;
use tokio::sync::Mutex;
use url::Url;

use crate::config::{UpstreamAuthFileConfig, user_home_dir};

const DEFAULT_AUTH_COMMAND_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_AUTH_REFRESH_INTERVAL_MS: u64 = 300_000;
const MAX_AUTH_COMMAND_TIMEOUT_MS: u64 = 60_000;
const MAX_AUTH_REFRESH_INTERVAL_MS: u64 = 86_400_000;
const MAX_AUTH_TOKEN_BYTES: usize = 8 * 1024;
const MAX_CODEX_CONFIG_BYTES: u64 = 1024 * 1024;
const AUTH_COMMAND_FAILURE_BACKOFF: Duration = Duration::from_secs(5);

#[derive(Default, Args)]
pub(crate) struct GatewayAuthArgs {
    /// Read the provider bearer token from this environment variable.
    #[arg(
        long,
        value_name = "ENV",
        conflicts_with_all = [
            "upstream_bearer_command",
            "upstream_auth_codex_provider",
            "no_upstream_authorization"
        ]
    )]
    upstream_bearer_env: Option<String>,

    /// Execute this program directly and read one provider bearer token from stdout.
    #[arg(
        long,
        value_name = "PROGRAM",
        conflicts_with_all = [
            "upstream_bearer_env",
            "upstream_auth_codex_provider",
            "no_upstream_authorization"
        ]
    )]
    upstream_bearer_command: Option<PathBuf>,

    /// Argument passed directly to --upstream-bearer-command; repeat as needed.
    #[arg(
        long,
        value_name = "ARG",
        requires = "upstream_bearer_command",
        action = clap::ArgAction::Append
    )]
    upstream_bearer_arg: Vec<String>,

    /// Working directory for --upstream-bearer-command.
    #[arg(long, value_name = "PATH", requires = "upstream_bearer_command")]
    upstream_bearer_command_cwd: Option<PathBuf>,

    /// Timeout for --upstream-bearer-command.
    #[arg(
        long,
        value_name = "MILLISECONDS",
        requires = "upstream_bearer_command"
    )]
    upstream_bearer_command_timeout_ms: Option<u64>,

    /// Maximum cached token age; 0 refreshes only after an upstream 401.
    #[arg(
        long,
        value_name = "MILLISECONDS",
        requires = "upstream_bearer_command"
    )]
    upstream_bearer_command_refresh_interval_ms: Option<u64>,

    /// Reuse command-auth from this requires_openai_auth=false Codex provider.
    #[arg(
        long,
        value_name = "PROVIDER",
        conflicts_with_all = [
            "upstream_bearer_env",
            "upstream_bearer_command",
            "no_upstream_authorization"
        ]
    )]
    upstream_auth_codex_provider: Option<String>,

    /// Codex TOML used with --upstream-auth-codex-provider.
    #[arg(long, value_name = "PATH", requires = "upstream_auth_codex_provider")]
    upstream_auth_codex_config: Option<PathBuf>,

    /// Override configured upstream authorization and send no credentials.
    #[arg(
        long,
        conflicts_with_all = [
            "upstream_bearer_env",
            "upstream_bearer_command",
            "upstream_auth_codex_provider"
        ]
    )]
    no_upstream_authorization: bool,
}

impl std::fmt::Debug for GatewayAuthArgs {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GatewayAuthArgs")
            .field("upstream_bearer_env", &self.upstream_bearer_env)
            .field(
                "upstream_bearer_command",
                &self
                    .upstream_bearer_command
                    .as_ref()
                    .map(|_| "[CONFIGURED]"),
            )
            .field(
                "upstream_bearer_arg",
                &(!self.upstream_bearer_arg.is_empty()).then_some("[REDACTED]"),
            )
            .field(
                "upstream_bearer_command_cwd",
                &self.upstream_bearer_command_cwd,
            )
            .field(
                "upstream_bearer_command_timeout_ms",
                &self.upstream_bearer_command_timeout_ms,
            )
            .field(
                "upstream_bearer_command_refresh_interval_ms",
                &self.upstream_bearer_command_refresh_interval_ms,
            )
            .field(
                "upstream_auth_codex_provider",
                &self.upstream_auth_codex_provider,
            )
            .field(
                "upstream_auth_codex_config",
                &self.upstream_auth_codex_config,
            )
            .field("no_upstream_authorization", &self.no_upstream_authorization)
            .finish()
    }
}

#[derive(Clone)]
struct BearerCommand {
    program: PathBuf,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    timeout: Duration,
    refresh_interval: Option<Duration>,
}

impl std::fmt::Debug for BearerCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BearerCommand")
            .field("program", &"[CONFIGURED]")
            .field("args", &"[REDACTED]")
            .field("cwd", &self.cwd)
            .field("timeout", &self.timeout)
            .field("refresh_interval", &self.refresh_interval)
            .finish()
    }
}

struct CachedBearer {
    value: HeaderValue,
    fetched_at: Instant,
}

#[derive(Default)]
struct CommandBearerState {
    cached: Option<CachedBearer>,
    retry_after: Option<Instant>,
}

struct CommandBearerProvider {
    command: BearerCommand,
    state: Mutex<CommandBearerState>,
}

#[derive(Debug, Deserialize)]
struct CodexConfigView {
    #[serde(default)]
    model_providers: BTreeMap<String, CodexProviderView>,
}

#[derive(Debug, Deserialize)]
struct CodexProviderView {
    base_url: Option<Url>,
    wire_api: Option<String>,
    #[serde(default)]
    requires_openai_auth: bool,
    auth: Option<CodexCommandAuthView>,
    env_key: Option<String>,
    experimental_bearer_token: Option<serde::de::IgnoredAny>,
    aws: Option<serde::de::IgnoredAny>,
    query_params: Option<serde::de::IgnoredAny>,
    http_headers: Option<serde::de::IgnoredAny>,
    env_http_headers: Option<serde::de::IgnoredAny>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexCommandAuthView {
    command: PathBuf,
    #[serde(default)]
    args: Vec<String>,
    timeout_ms: Option<u64>,
    #[serde(default = "default_auth_refresh_interval_ms")]
    refresh_interval_ms: u64,
    cwd: Option<PathBuf>,
}

impl GatewayAuthArgs {
    pub(crate) fn resolve(
        &self,
        configured: Option<&UpstreamAuthFileConfig>,
        upstream: &Url,
    ) -> Result<GatewayAuthorization> {
        if self.no_upstream_authorization {
            return Ok(GatewayAuthorization::None);
        }
        if let Some(name) = &self.upstream_bearer_env {
            return bearer_from_env(name);
        }
        if let Some(program) = &self.upstream_bearer_command {
            let command = BearerCommand {
                program: resolve_cli_path(program)?,
                args: self.upstream_bearer_arg.clone(),
                cwd: self
                    .upstream_bearer_command_cwd
                    .as_deref()
                    .map(resolve_cli_path)
                    .transpose()?,
                timeout: command_timeout(self.upstream_bearer_command_timeout_ms)?,
                refresh_interval: command_refresh_interval(
                    self.upstream_bearer_command_refresh_interval_ms,
                )?,
            };
            return bearer_from_command(command);
        }
        if let Some(provider) = &self.upstream_auth_codex_provider {
            let config = self
                .upstream_auth_codex_config
                .as_deref()
                .map(resolve_cli_path)
                .transpose()?;
            let command = command_from_codex_provider(provider, config.as_deref(), upstream)?;
            return bearer_from_command(command);
        }

        match configured {
            None => Ok(GatewayAuthorization::None),
            Some(UpstreamAuthFileConfig::Environment { name }) => bearer_from_env(name),
            Some(UpstreamAuthFileConfig::Command {
                command,
                args,
                cwd,
                timeout_ms,
                refresh_interval_ms,
            }) => bearer_from_command(BearerCommand {
                program: command.clone(),
                args: args.clone(),
                cwd: cwd.clone(),
                timeout: command_timeout(*timeout_ms)?,
                refresh_interval: command_refresh_interval(*refresh_interval_ms)?,
            }),
            Some(UpstreamAuthFileConfig::CodexProviderCommand { provider, config }) => {
                let command = command_from_codex_provider(provider, config.as_deref(), upstream)?;
                bearer_from_command(command)
            }
        }
    }
}

fn bearer_from_env(name: &str) -> Result<GatewayAuthorization> {
    let value = std::env::var(name)
        .into_diagnostic()
        .wrap_err_with(|| format!("upstream bearer environment variable {name} is unavailable"))?;
    authorization_from_token(&value)
        .wrap_err_with(|| format!("upstream bearer environment variable {name} is invalid"))
}

fn bearer_from_command(command: BearerCommand) -> Result<GatewayAuthorization> {
    validate_command(&command)?;
    Ok(GatewayAuthorization::BearerProvider(Arc::new(
        CommandBearerProvider {
            command,
            state: Mutex::new(CommandBearerState::default()),
        },
    )))
}

impl CommandBearerProvider {
    async fn resolve(&self) -> Result<HeaderValue, GatewayError> {
        let mut state = self.state.lock().await;
        if let Some(cached) = state.cached.as_ref()
            && self
                .command
                .refresh_interval
                .is_none_or(|interval| cached.fetched_at.elapsed() < interval)
        {
            return Ok(cached.value.clone());
        }
        if refresh_is_backing_off(&state) {
            return Err(auth_refresh_backoff_error());
        }
        match run_bearer_command(&self.command).await {
            Ok(value) => {
                state.cached = Some(CachedBearer {
                    value: value.clone(),
                    fetched_at: Instant::now(),
                });
                state.retry_after = None;
                Ok(value)
            }
            Err(error) => {
                state.retry_after = Some(Instant::now() + AUTH_COMMAND_FAILURE_BACKOFF);
                Err(error)
            }
        }
    }

    async fn refresh(&self, failed: &HeaderValue) -> Result<HeaderValue, GatewayError> {
        let mut state = self.state.lock().await;
        if let Some(cached) = state.cached.as_ref()
            && cached.value != *failed
        {
            return Ok(cached.value.clone());
        }
        if refresh_is_backing_off(&state) {
            return Err(auth_refresh_backoff_error());
        }
        match run_bearer_command(&self.command).await {
            Ok(value) => {
                state.cached = Some(CachedBearer {
                    value: value.clone(),
                    fetched_at: Instant::now(),
                });
                state.retry_after = None;
                Ok(value)
            }
            Err(error) => {
                state.cached = None;
                state.retry_after = Some(Instant::now() + AUTH_COMMAND_FAILURE_BACKOFF);
                Err(error)
            }
        }
    }
}

fn refresh_is_backing_off(state: &CommandBearerState) -> bool {
    state
        .retry_after
        .is_some_and(|retry_after| Instant::now() < retry_after)
}

fn auth_refresh_backoff_error() -> GatewayError {
    GatewayError::Upstream("provider bearer command is temporarily unavailable".to_owned())
}

impl GatewayBearerProvider for CommandBearerProvider {
    fn bearer(&self) -> GatewayBearerFuture<'_> {
        Box::pin(self.resolve())
    }

    fn refresh_after_unauthorized<'a>(
        &'a self,
        failed: &'a HeaderValue,
    ) -> GatewayBearerFuture<'a> {
        Box::pin(self.refresh(failed))
    }

    fn report_name(&self) -> &'static str {
        "bearer-command-cache"
    }
}

async fn run_bearer_command(command: &BearerCommand) -> Result<HeaderValue, GatewayError> {
    run_bearer_command_inner(command).await.map_err(|error| {
        tracing::debug!(
            target: "opsail_gateway_model",
            error = %error,
            "provider bearer command failed"
        );
        GatewayError::Upstream("provider bearer command failed".to_owned())
    })
}

async fn run_bearer_command_inner(command: &BearerCommand) -> Result<HeaderValue> {
    let mut process = Command::new(resolve_program(&command.program, command.cwd.as_deref())?);
    process
        .args(&command.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(cwd) = &command.cwd {
        process.current_dir(cwd);
    }
    apply_bounded_command_environment(&mut process);

    let mut child = process
        .spawn()
        .into_diagnostic()
        .wrap_err("upstream bearer command failed to start")?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| miette!("upstream bearer command stdout was unavailable"))?;
    let mut reader = tokio::spawn(read_bounded_output(stdout));
    let deadline = tokio::time::Instant::now() + command.timeout;
    let status = match tokio::time::timeout_at(deadline, child.wait()).await {
        Ok(status) => status
            .into_diagnostic()
            .wrap_err("upstream bearer command could not be awaited")?,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            reader.abort();
            let _ = reader.await;
            return Err(miette!("upstream bearer command timed out"));
        }
    };
    let reader_result = match tokio::time::timeout_at(deadline, &mut reader).await {
        Ok(result) => result,
        Err(_) => {
            reader.abort();
            let _ = reader.await;
            return Err(miette!("upstream bearer command timed out"));
        }
    };
    let (stdout, exceeded) = reader_result
        .into_diagnostic()
        .wrap_err("upstream bearer command output reader stopped")?
        .into_diagnostic()
        .wrap_err("upstream bearer command stdout could not be read")?;
    if !status.success() {
        return Err(miette!("upstream bearer command exited unsuccessfully"));
    }
    if exceeded {
        return Err(miette!(
            "upstream bearer command token exceeds the {MAX_AUTH_TOKEN_BYTES} byte limit"
        ));
    }
    let token = std::str::from_utf8(&stdout)
        .into_diagnostic()
        .wrap_err("upstream bearer command token is not UTF-8")?
        .trim();
    header_from_token(token).wrap_err("upstream bearer command token is invalid")
}

fn command_from_codex_provider(
    provider_id: &str,
    explicit_config: Option<&Path>,
    upstream: &Url,
) -> Result<BearerCommand> {
    let config_path = match explicit_config {
        Some(path) => path.to_path_buf(),
        None => user_home_dir()
            .ok_or_else(|| {
                miette!(
                    "could not resolve the user home directory; set \
                     gateway.model.upstream_auth.config explicitly"
                )
            })?
            .join(".codex")
            .join("config.toml"),
    };
    let source = read_bounded_regular_file(
        &config_path,
        MAX_CODEX_CONFIG_BYTES,
        "Codex provider config",
    )?;
    let config: CodexConfigView = toml::from_str(&source).map_err(|_| {
        miette!(
            "failed to parse Codex provider config {}; parser details were suppressed because \
             that file may contain credentials",
            config_path.display()
        )
    })?;
    let provider = config
        .model_providers
        .get(provider_id)
        .ok_or_else(|| miette!("Codex provider {provider_id} is not configured"))?;
    if provider.requires_openai_auth {
        return Err(miette!(
            "Codex provider {provider_id} requires OpenAI or ChatGPT auth and cannot supply \
             third-party gateway credentials"
        ));
    }
    if provider.env_key.is_some()
        || provider.experimental_bearer_token.is_some()
        || provider.aws.is_some()
        || provider.query_params.is_some()
        || provider.http_headers.is_some()
        || provider.env_http_headers.is_some()
    {
        return Err(miette!(
            "Codex provider {provider_id} mixes command-auth with another credential or transport \
             source that the gateway cannot safely import"
        ));
    }
    let provider_base = provider.base_url.as_ref().ok_or_else(|| {
        miette!("Codex provider {provider_id} has no base_url to bind its credential")
    })?;
    if !same_upstream(provider_base, upstream) {
        return Err(miette!(
            "Codex provider {provider_id} base_url does not match the configured gateway upstream"
        ));
    }
    if provider
        .wire_api
        .as_deref()
        .is_some_and(|wire_api| wire_api != "responses")
    {
        return Err(miette!(
            "Codex provider {provider_id} does not use the Responses wire API"
        ));
    }
    let auth = provider
        .auth
        .as_ref()
        .ok_or_else(|| miette!("Codex provider {provider_id} has no command-auth configuration"))?;
    let cwd = auth.cwd.clone().map(resolve_against_current).transpose()?;
    let current_directory = std::env::current_dir()
        .into_diagnostic()
        .wrap_err("failed to resolve the current directory")?;
    let program = if auth.command.is_relative() && auth.command.components().count() > 1 {
        cwd.as_deref()
            .unwrap_or(current_directory.as_path())
            .join(&auth.command)
    } else {
        auth.command.clone()
    };
    Ok(BearerCommand {
        program,
        args: auth.args.clone(),
        cwd,
        timeout: command_timeout(auth.timeout_ms)?,
        refresh_interval: command_refresh_interval(Some(auth.refresh_interval_ms))?,
    })
}

fn same_upstream(provider: &Url, configured: &Url) -> bool {
    let normalize = |url: &Url| {
        (
            url.scheme().to_owned(),
            url.host_str().map(str::to_ascii_lowercase),
            url.port_or_known_default(),
            url.path().trim_end_matches('/').to_owned(),
            url.username().to_owned(),
            url.password().map(str::to_owned),
            url.query().map(str::to_owned),
            url.fragment().map(str::to_owned),
        )
    };
    normalize(provider) == normalize(configured)
}

fn resolve_program(program: &Path, cwd: Option<&Path>) -> Result<PathBuf> {
    if program.is_absolute() || program.components().count() == 1 {
        return Ok(program.to_path_buf());
    }
    Ok(cwd
        .map(Path::to_path_buf)
        .unwrap_or(std::env::current_dir().into_diagnostic()?)
        .join(program))
}

fn resolve_cli_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() || path.components().count() == 1 {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()
        .into_diagnostic()
        .wrap_err("failed to resolve the current directory")?
        .join(path))
}

fn resolve_against_current(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path);
    }
    Ok(std::env::current_dir()
        .into_diagnostic()
        .wrap_err("failed to resolve the current directory")?
        .join(path))
}

fn command_timeout(value: Option<u64>) -> Result<Duration> {
    let value = value.unwrap_or(DEFAULT_AUTH_COMMAND_TIMEOUT_MS);
    if !(1..=MAX_AUTH_COMMAND_TIMEOUT_MS).contains(&value) {
        return Err(miette!(
            "upstream bearer command timeout must be between 1 and \
             {MAX_AUTH_COMMAND_TIMEOUT_MS} milliseconds"
        ));
    }
    Ok(Duration::from_millis(value))
}

fn default_auth_refresh_interval_ms() -> u64 {
    DEFAULT_AUTH_REFRESH_INTERVAL_MS
}

fn command_refresh_interval(value: Option<u64>) -> Result<Option<Duration>> {
    let value = value.unwrap_or(DEFAULT_AUTH_REFRESH_INTERVAL_MS);
    if value > MAX_AUTH_REFRESH_INTERVAL_MS {
        return Err(miette!(
            "upstream bearer command refresh interval must be 0 or at most \
             {MAX_AUTH_REFRESH_INTERVAL_MS} milliseconds"
        ));
    }
    Ok((value != 0).then(|| Duration::from_millis(value)))
}

fn validate_command(command: &BearerCommand) -> Result<()> {
    validate_bounded_text("upstream bearer command", command.program.as_os_str(), 1024)?;
    if command.args.len() > 64 {
        return Err(miette!(
            "upstream bearer command may receive at most 64 arguments"
        ));
    }
    for argument in &command.args {
        validate_bounded_text(
            "upstream bearer command argument",
            OsStr::new(argument),
            4096,
        )?;
    }
    if let Some(cwd) = &command.cwd {
        validate_bounded_text("upstream bearer command cwd", cwd.as_os_str(), 4096)?;
        if !cwd.is_absolute() {
            return Err(miette!(
                "upstream bearer command cwd must be an absolute path"
            ));
        }
    }
    Ok(())
}

fn validate_bounded_text(label: &str, value: &OsStr, limit: usize) -> Result<()> {
    let value = value.to_string_lossy();
    if value.is_empty()
        || value.len() > limit
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(miette!(
            "{label} must be non-empty, trimmed, control-free, and at most {limit} bytes"
        ));
    }
    Ok(())
}

fn authorization_from_token(token: &str) -> Result<GatewayAuthorization> {
    Ok(GatewayAuthorization::Bearer(header_from_token(token)?))
}

fn header_from_token(token: &str) -> Result<HeaderValue> {
    if token.is_empty() || token.len() > MAX_AUTH_TOKEN_BYTES || token.chars().any(char::is_control)
    {
        return Err(miette!(
            "provider bearer token must be one non-empty control-free value of at most \
             {MAX_AUTH_TOKEN_BYTES} bytes"
        ));
    }
    let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
        .into_diagnostic()
        .wrap_err("provider bearer token is not a valid HTTP header value")?;
    value.set_sensitive(true);
    Ok(value)
}

fn apply_bounded_command_environment(command: &mut Command) {
    const SAFE_ENVIRONMENT: &[&str] = &[
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "TMPDIR",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "SystemRoot",
        "WINDIR",
        "ComSpec",
        "PATHEXT",
        "TEMP",
        "TMP",
        "USERPROFILE",
    ];
    let values = SAFE_ENVIRONMENT
        .iter()
        .filter_map(|name| std::env::var_os(name).map(|value| (*name, value)))
        .collect::<Vec<_>>();
    command.env_clear();
    for (name, value) in values {
        command.env(name, value);
    }
}

async fn read_bounded_output(
    mut output: impl AsyncRead + Unpin,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut stored = Vec::with_capacity(MAX_AUTH_TOKEN_BYTES.min(1024));
    let mut exceeded = false;
    let mut buffer = [0_u8; 1024];
    loop {
        let count = output.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let remaining = MAX_AUTH_TOKEN_BYTES.saturating_sub(stored.len());
        stored.extend_from_slice(&buffer[..count.min(remaining)]);
        exceeded |= count > remaining;
    }
    Ok((stored, exceeded))
}

fn read_bounded_regular_file(path: &Path, limit: u64, label: &str) -> Result<String> {
    let metadata = fs::symlink_metadata(path)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to inspect {label} {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(miette!(
            "{label} must be a regular, non-symlink file: {}",
            path.display()
        ));
    }
    if metadata.len() > limit {
        return Err(miette!(
            "{label} exceeds the {limit} byte limit: {}",
            path.display()
        ));
    }
    let mut source = String::with_capacity(metadata.len() as usize);
    OpenOptions::new()
        .read(true)
        .open(path)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to open {label} {}", path.display()))?
        .take(limit + 1)
        .read_to_string(&mut source)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read {label} {}", path.display()))?;
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn provider_binding_compares_normalized_base_urls() {
        assert!(same_upstream(
            &Url::parse("http://127.0.0.1:8317/v1/").unwrap(),
            &Url::parse("http://127.0.0.1:8317/v1").unwrap()
        ));
        assert!(!same_upstream(
            &Url::parse("http://127.0.0.1:8317/v1").unwrap(),
            &Url::parse("http://127.0.0.1:8318/v1").unwrap()
        ));
    }

    #[test]
    fn codex_provider_auth_is_bound_to_a_non_openai_responses_provider() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(
            &path,
            r#"
[model_providers.unsafe]
base_url = "http://127.0.0.1:8317/v1"
wire_api = "responses"
requires_openai_auth = true
[model_providers.unsafe.auth]
command = "/usr/bin/printf"
args = ["token"]

[model_providers.wrong]
base_url = "http://127.0.0.1:8318/v1"
wire_api = "responses"
requires_openai_auth = false
[model_providers.wrong.auth]
command = "/usr/bin/printf"
args = ["token"]

[model_providers.valid]
base_url = "http://127.0.0.1:8317/v1/"
wire_api = "responses"
requires_openai_auth = false
[model_providers.valid.auth]
command = "token-helper"

[model_providers.mixed]
base_url = "http://127.0.0.1:8317/v1"
wire_api = "responses"
requires_openai_auth = false
env_key = "PROVIDER_TOKEN"
[model_providers.mixed.auth]
command = "token-helper"
"#,
        )
        .unwrap();
        let upstream = Url::parse("http://127.0.0.1:8317/v1").unwrap();
        assert!(command_from_codex_provider("unsafe", Some(&path), &upstream).is_err());
        assert!(command_from_codex_provider("wrong", Some(&path), &upstream).is_err());
        assert!(command_from_codex_provider("mixed", Some(&path), &upstream).is_err());
        let valid = command_from_codex_provider("valid", Some(&path), &upstream).unwrap();
        assert_eq!(valid.program, PathBuf::from("token-helper"));
        assert_eq!(
            valid.refresh_interval,
            Some(Duration::from_millis(DEFAULT_AUTH_REFRESH_INTERVAL_MS))
        );
    }

    #[test]
    fn codex_config_parser_errors_never_echo_possible_secrets() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(
            &path,
            "experimental_bearer_token = \"must-not-leak\"\nthis is invalid toml\n",
        )
        .unwrap();
        let error = command_from_codex_provider(
            "provider",
            Some(&path),
            &Url::parse("http://127.0.0.1:8317/v1").unwrap(),
        )
        .unwrap_err()
        .to_string();
        assert!(!error.contains("must-not-leak"));
        assert!(!error.contains("this is invalid toml"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_auth_is_cached_singleflight_and_keeps_tokens_out_of_debug() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempdir().unwrap();
        let program = directory.path().join("token-command");
        fs::write(
            &program,
            concat!(
                "#!/bin/sh\n",
                "count_file=\"$PWD/count\"\n",
                "count=0\n",
                "if [ -f \"$count_file\" ]; then count=$(cat \"$count_file\"); fi\n",
                "count=$((count + 1))\n",
                "printf '%s' \"$count\" > \"$count_file\"\n",
                "sleep 0.05\n",
                "printf 'provider-test-token-%s' \"$count\"\n",
            ),
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        let authorization = bearer_from_command(BearerCommand {
            program,
            args: Vec::new(),
            cwd: Some(directory.path().to_path_buf()),
            timeout: Duration::from_secs(2),
            refresh_interval: None,
        })
        .unwrap();
        assert!(!format!("{authorization:?}").contains("provider-test-token"));

        let GatewayAuthorization::BearerProvider(provider) = authorization else {
            panic!("command auth must use a dynamic bearer provider");
        };
        let (first, second, third) =
            tokio::join!(provider.bearer(), provider.bearer(), provider.bearer());
        let first = first.unwrap();
        assert_eq!(second.unwrap(), first);
        assert_eq!(third.unwrap(), first);
        assert_eq!(
            fs::read_to_string(directory.path().join("count")).unwrap(),
            "1"
        );

        let (refreshed, reused) = tokio::join!(
            provider.refresh_after_unauthorized(&first),
            provider.refresh_after_unauthorized(&first)
        );
        let refreshed = refreshed.unwrap();
        let reused = reused.unwrap();
        assert_ne!(refreshed, first);
        assert_eq!(reused, refreshed);
        assert_eq!(
            fs::read_to_string(directory.path().join("count")).unwrap(),
            "2",
            "concurrent 401s for the old token must share one refresh"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn positive_refresh_interval_proactively_reloads_the_token() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempdir().unwrap();
        let program = directory.path().join("token-command");
        fs::write(
            &program,
            concat!(
                "#!/bin/sh\n",
                "count_file=\"$PWD/count\"\n",
                "count=0\n",
                "if [ -f \"$count_file\" ]; then count=$(cat \"$count_file\"); fi\n",
                "count=$((count + 1))\n",
                "printf '%s' \"$count\" > \"$count_file\"\n",
                "printf 'provider-test-token-%s' \"$count\"\n",
            ),
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        let GatewayAuthorization::BearerProvider(provider) = bearer_from_command(BearerCommand {
            program,
            args: Vec::new(),
            cwd: Some(directory.path().to_path_buf()),
            timeout: Duration::from_secs(2),
            refresh_interval: Some(Duration::from_millis(1)),
        })
        .unwrap() else {
            panic!("command auth must use a dynamic bearer provider");
        };

        let first = provider.bearer().await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        let second = provider.bearer().await.unwrap();
        assert_ne!(first, second);
        assert_eq!(
            fs::read_to_string(directory.path().join("count")).unwrap(),
            "2"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_command_refresh_is_singleflight_and_backed_off() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempdir().unwrap();
        let program = directory.path().join("failing-token-command");
        fs::write(
            &program,
            concat!(
                "#!/bin/sh\n",
                "count_file=\"$PWD/count\"\n",
                "count=0\n",
                "if [ -f \"$count_file\" ]; then count=$(cat \"$count_file\"); fi\n",
                "count=$((count + 1))\n",
                "printf '%s' \"$count\" > \"$count_file\"\n",
                "exit 1\n",
            ),
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        let GatewayAuthorization::BearerProvider(provider) = bearer_from_command(BearerCommand {
            program,
            args: Vec::new(),
            cwd: Some(directory.path().to_path_buf()),
            timeout: Duration::from_secs(2),
            refresh_interval: None,
        })
        .unwrap() else {
            panic!("command auth must use a dynamic bearer provider");
        };

        let (first, second) = tokio::join!(provider.bearer(), provider.bearer());
        assert!(first.is_err());
        assert!(second.is_err());
        assert!(provider.bearer().await.is_err());
        assert_eq!(
            fs::read_to_string(directory.path().join("count")).unwrap(),
            "1",
            "requests during the failure backoff must not rerun the command"
        );
    }
}
