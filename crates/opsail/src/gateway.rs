use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Args, Subcommand, ValueEnum};
use miette::{IntoDiagnostic, Result, WrapErr, miette};
use opsail_gateway_model::{
    DEFAULT_GATEWAY_LISTEN, DEFAULT_GATEWAY_MAX_CONCURRENT_REQUESTS,
    DEFAULT_GATEWAY_MAX_REQUEST_BYTES, DEFAULT_GATEWAY_REQUEST_TIMEOUT,
    DEFAULT_GATEWAY_STREAM_IDLE_TIMEOUT, EventMappingProfileV1, GatewayConfig, GatewayServer,
    PromptCacheRouting, ReasoningDisplay,
};
use url::Url;

use crate::config::OpsailConfig;
use crate::{with_trailing_newline, write_stdout};

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub(crate) struct GatewayArgs {
    #[command(subcommand)]
    command: GatewayCommand,
}

#[derive(Debug, Subcommand)]
enum GatewayCommand {
    /// Translate explicitly routed third-party model traffic.
    Model(ModelGatewayArgs),
}

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
struct ModelGatewayArgs {
    #[command(subcommand)]
    command: ModelGatewayCommand,
}

#[derive(Debug, Subcommand)]
enum ModelGatewayCommand {
    /// Serve the configured Responses-compatible upstream on a loopback address.
    Serve(Box<GatewayServeArgs>),
    /// Validate a bounded declarative event-mapping profile without starting a server.
    ValidateMapping(GatewayValidateMappingArgs),
}

#[derive(Debug, Args)]
struct GatewayValidateMappingArgs {
    /// TOML event-mapping profile to validate.
    #[arg(value_name = "PATH")]
    path: PathBuf,
}

#[derive(Debug, Args)]
struct GatewayServeArgs {
    /// Loopback listener; overrides gateway.model.listen.
    #[arg(long, value_name = "IP:PORT")]
    listen: Option<SocketAddr>,

    /// Responses-compatible /v1 base URL; overrides gateway.model.upstream.
    #[arg(long, value_name = "URL")]
    upstream: Option<Url>,

    /// Preserve reasoning events or render provider summaries as commentary.
    #[arg(long, value_enum, value_name = "MODE")]
    reasoning_display: Option<ReasoningDisplayArg>,

    /// End-to-end upstream request timeout in seconds.
    #[arg(long, value_name = "SECONDS")]
    request_timeout_seconds: Option<u64>,

    /// Maximum idle interval between upstream response chunks.
    #[arg(long, value_name = "SECONDS")]
    stream_idle_timeout_seconds: Option<u64>,

    /// Maximum accepted Codex request body size.
    #[arg(long, value_name = "BYTES")]
    max_request_bytes: Option<usize>,

    /// Maximum number of in-flight upstream response streams.
    #[arg(long, value_name = "COUNT")]
    max_concurrent_requests: Option<usize>,

    /// Strip Codex cache keys or replace them with provider-scoped stable hashes.
    #[arg(long, value_enum, value_name = "MODE")]
    prompt_cache_routing: Option<PromptCacheRoutingArg>,

    /// TOML event-mapping profile; overrides the configured mapping.
    #[arg(long, value_name = "PATH")]
    event_mapping: Option<PathBuf>,

    #[command(flatten)]
    auth: crate::gateway_auth::GatewayAuthArgs,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ReasoningDisplayArg {
    Strict,
    Commentary,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum PromptCacheRoutingArg {
    Strip,
    ProviderScoped,
}

impl From<ReasoningDisplayArg> for ReasoningDisplay {
    fn from(value: ReasoningDisplayArg) -> Self {
        match value {
            ReasoningDisplayArg::Strict => Self::Strict,
            ReasoningDisplayArg::Commentary => Self::Commentary,
        }
    }
}

impl From<PromptCacheRoutingArg> for PromptCacheRouting {
    fn from(value: PromptCacheRoutingArg) -> Self {
        match value {
            PromptCacheRoutingArg::Strip => Self::Strip,
            PromptCacheRoutingArg::ProviderScoped => Self::ProviderScoped,
        }
    }
}

impl GatewayServeArgs {
    fn resolve(&self, config: &OpsailConfig) -> Result<GatewayConfig> {
        let file = &config.gateway.model;
        let listen = self.listen.or(file.listen).unwrap_or_else(|| {
            DEFAULT_GATEWAY_LISTEN
                .parse()
                .expect("the built-in gateway listener is valid")
        });
        let upstream = self
            .upstream
            .clone()
            .or_else(|| file.upstream.clone())
            .ok_or_else(|| {
                miette!(
                    "model gateway upstream is required; set gateway.model.upstream in \
                     ~/.opsail/config.toml or pass --upstream <URL>"
                )
            })?;
        let reasoning_display = self
            .reasoning_display
            .map(Into::into)
            .or(file.reasoning_display)
            .unwrap_or_default();
        let request_timeout = Duration::from_secs(
            self.request_timeout_seconds
                .or(file.request_timeout_seconds)
                .unwrap_or(DEFAULT_GATEWAY_REQUEST_TIMEOUT.as_secs()),
        );
        let max_request_bytes = self
            .max_request_bytes
            .or(file.max_request_bytes)
            .unwrap_or(DEFAULT_GATEWAY_MAX_REQUEST_BYTES);
        let stream_idle_timeout = Duration::from_secs(
            self.stream_idle_timeout_seconds
                .or(file.stream_idle_timeout_seconds)
                .unwrap_or(DEFAULT_GATEWAY_STREAM_IDLE_TIMEOUT.as_secs()),
        );
        let max_concurrent_requests = self
            .max_concurrent_requests
            .or(file.max_concurrent_requests)
            .unwrap_or(DEFAULT_GATEWAY_MAX_CONCURRENT_REQUESTS);
        let prompt_cache_routing = self
            .prompt_cache_routing
            .map(Into::into)
            .or(file.prompt_cache_routing)
            .unwrap_or_default();
        let event_mapping = if let Some(path) = self.event_mapping.as_deref() {
            Some(read_event_mapping(path)?)
        } else if let Some(profile) = &file.event_mapping {
            Some(profile.clone())
        } else {
            file.event_mapping_file
                .as_deref()
                .map(read_event_mapping)
                .transpose()?
        };
        let authorization = self.auth.resolve(file.upstream_auth.as_ref(), &upstream)?;
        let resolved = GatewayConfig {
            listen,
            upstream,
            reasoning_display,
            request_timeout,
            stream_idle_timeout,
            max_request_bytes,
            max_concurrent_requests,
            prompt_cache_routing,
            event_mapping,
            authorization,
        };
        resolved
            .validate()
            .into_diagnostic()
            .wrap_err("invalid model gateway configuration")?;
        Ok(resolved)
    }
}

fn read_event_mapping(path: &Path) -> Result<EventMappingProfileV1> {
    const MAX_MAPPING_FILE_BYTES: u64 = 256 * 1024;

    let metadata = fs::symlink_metadata(path)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to inspect event mapping {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(miette!(
            "event mapping must be a regular, non-symlink file: {}",
            path.display()
        ));
    }
    if metadata.len() > MAX_MAPPING_FILE_BYTES {
        return Err(miette!(
            "event mapping exceeds the {MAX_MAPPING_FILE_BYTES} byte limit: {}",
            path.display()
        ));
    }
    let source = fs::read_to_string(path)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read event mapping {}", path.display()))?;
    EventMappingProfileV1::from_toml(&source)
        .into_diagnostic()
        .wrap_err_with(|| format!("invalid event mapping {}", path.display()))
}

async fn run_model(args: ModelGatewayArgs, config: &OpsailConfig) -> Result<()> {
    match args.command {
        ModelGatewayCommand::Serve(args) => {
            let server = GatewayServer::bind(args.resolve(config)?)
                .await
                .into_diagnostic()
                .wrap_err("failed to start the model gateway")?;
            let output = serde_json::to_string_pretty(server.report())
                .into_diagnostic()
                .wrap_err("failed to serialize the model gateway report")?;
            write_stdout(with_trailing_newline(output).as_bytes())?;
            server
                .run()
                .await
                .into_diagnostic()
                .wrap_err("model gateway stopped")
        }
        ModelGatewayCommand::ValidateMapping(args) => {
            let profile = read_event_mapping(&args.path)?;
            let output = serde_json::to_string_pretty(&serde_json::json!({
                "valid": true,
                "schemaVersion": profile.version,
                "rules": profile.rules.len(),
                "path": args.path,
            }))
            .into_diagnostic()
            .wrap_err("failed to serialize the event mapping validation result")?;
            write_stdout(with_trailing_newline(output).as_bytes())
        }
    }
}

pub(crate) async fn run(args: GatewayArgs, config: &OpsailConfig) -> Result<()> {
    match args.command {
        GatewayCommand::Model(args) => run_model(args, config).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn args() -> GatewayServeArgs {
        GatewayServeArgs {
            listen: None,
            upstream: None,
            reasoning_display: None,
            request_timeout_seconds: None,
            stream_idle_timeout_seconds: None,
            max_request_bytes: None,
            max_concurrent_requests: None,
            prompt_cache_routing: None,
            event_mapping: None,
            auth: crate::gateway_auth::GatewayAuthArgs::default(),
        }
    }

    #[test]
    fn command_line_values_override_file_values() {
        let mut config = OpsailConfig::default();
        config.gateway.model.listen = Some("127.0.0.1:55401".parse().unwrap());
        config.gateway.model.upstream = Some(Url::parse("http://127.0.0.1:8317/v1").unwrap());
        config.gateway.model.reasoning_display = Some(ReasoningDisplay::Strict);
        config.gateway.model.max_concurrent_requests = Some(2);
        config.gateway.model.prompt_cache_routing = Some(PromptCacheRouting::Strip);

        let mut cli = args();
        cli.listen = Some("127.0.0.1:55402".parse().unwrap());
        cli.upstream = Some(Url::parse("http://127.0.0.1:8318/v1").unwrap());
        cli.reasoning_display = Some(ReasoningDisplayArg::Commentary);
        cli.max_concurrent_requests = Some(4);
        cli.prompt_cache_routing = Some(PromptCacheRoutingArg::ProviderScoped);
        let resolved = cli.resolve(&config).unwrap();

        assert_eq!(resolved.listen.port(), 55402);
        assert_eq!(resolved.upstream.port(), Some(8318));
        assert_eq!(resolved.reasoning_display, ReasoningDisplay::Commentary);
        assert_eq!(resolved.max_concurrent_requests, 4);
        assert_eq!(
            resolved.prompt_cache_routing,
            PromptCacheRouting::ProviderScoped
        );
        assert!(resolved.event_mapping.is_none());
        assert!(matches!(
            resolved.authorization,
            opsail_gateway_model::GatewayAuthorization::None
        ));
    }

    #[test]
    fn upstream_is_the_only_required_setting() {
        let config = OpsailConfig::default();
        assert!(args().resolve(&config).is_err());
    }

    #[test]
    fn configured_mapping_file_is_loaded_and_cli_mapping_has_precedence() {
        let directory = tempdir().unwrap();
        let configured = directory.path().join("configured.toml");
        let command_line = directory.path().join("command-line.toml");
        fs::write(
            &configured,
            "version = 1\ndiscriminator = \"/kind\"\n[[rules]]\nmatch = \"configured\"\nemit = \"run-completed\"\n",
        )
        .unwrap();
        fs::write(
            &command_line,
            "version = 1\ndiscriminator = \"/kind\"\n[[rules]]\nmatch = \"command-line\"\nemit = \"run-completed\"\n",
        )
        .unwrap();

        let mut config = OpsailConfig::default();
        config.gateway.model.upstream = Some(Url::parse("http://127.0.0.1:8317/v1").unwrap());
        config.gateway.model.event_mapping_file = Some(configured);
        let configured = args().resolve(&config).unwrap();
        assert_eq!(
            configured.event_mapping.unwrap().rules[0].match_value,
            serde_json::json!("configured")
        );

        let mut cli = args();
        cli.event_mapping = Some(command_line);
        let overridden = cli.resolve(&config).unwrap();
        assert_eq!(
            overridden.event_mapping.unwrap().rules[0].match_value,
            serde_json::json!("command-line")
        );
    }
}
