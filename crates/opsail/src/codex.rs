use std::collections::BTreeMap;
use std::io::{self, Write as _};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clap::{Args, Subcommand, ValueEnum};
use miette::{IntoDiagnostic, Result, WrapErr, miette};
use opsail_refit_codex::{
    CodexRefit, CodexRefitConfig, CodexRefitError, CodexRefitStage, DEFAULT_CODEX_DEBUG_PORT,
    LaunchPolicy, ModelProviderRouting, RendererAssetUpdatePolicy, SessionMode,
    unlock_model_picker,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::activity::CliActivity;
use crate::config::OpsailConfig;
use crate::{with_trailing_newline, write_stdout};

const BACKGROUND_START_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_BACKGROUND_START_MESSAGE_BYTES: usize = 256 * 1024;
const BACKGROUND_STAGE_STABILITY_DELAY: Duration = Duration::from_millis(120);
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelProviderRoute {
    model: String,
    provider: String,
}

fn parse_model_provider_route(value: &str) -> std::result::Result<ModelProviderRoute, String> {
    let (model, provider) = value
        .split_once('=')
        .ok_or_else(|| "route must use MODEL=PROVIDER".to_owned())?;
    if [model, provider].iter().any(|part| {
        part.is_empty()
            || part.trim() != *part
            || part.chars().any(char::is_control)
            || part.len() > 512
    }) {
        return Err(
            "route model and provider must be non-empty, trimmed, control-free, and at most 512 bytes"
                .to_owned(),
        );
    }
    Ok(ModelProviderRoute {
        model: model.to_owned(),
        provider: provider.to_owned(),
    })
}

#[derive(Debug, Args)]
struct CodexUnlockArgs {
    /// Start a validated, stopped ChatGPT app with remote debugging before injecting.
    #[arg(short = 'l', long)]
    launch: bool,

    /// Provider id for each matching --model; retained as a compact single-provider form.
    #[arg(
        long,
        value_name = "PROVIDER",
        requires = "model",
        conflicts_with_all = ["route", "no_provider_routing"]
    )]
    model_provider: Option<String>,

    /// Model slug to route to --model-provider; repeat for multiple models.
    #[arg(
        long,
        value_name = "MODEL",
        requires = "model_provider",
        conflicts_with_all = ["route", "no_provider_routing"]
    )]
    model: Vec<String>,

    /// Route one model to one provider as MODEL=PROVIDER; repeat for multiple providers.
    #[arg(
        long,
        value_name = "MODEL=PROVIDER",
        value_parser = parse_model_provider_route,
        conflicts_with_all = ["model_provider", "model", "no_provider_routing"]
    )]
    route: Vec<ModelProviderRoute>,

    /// Provider used when a routed task switches back to an unmapped native model.
    #[arg(long, value_name = "PROVIDER")]
    default_provider: Option<String>,

    /// Ignore every provider route from config and inject only model visibility.
    #[arg(
        long,
        conflicts_with_all = ["model_provider", "model", "route", "default_provider"]
    )]
    no_provider_routing: bool,

    #[command(flatten)]
    endpoint: CodexPortArgs,
}

impl CodexUnlockArgs {
    fn routing(&self, config: &OpsailConfig) -> Result<ModelProviderRouting> {
        let routes = if self.no_provider_routing {
            BTreeMap::new()
        } else if !self.route.is_empty() {
            collect_routes(
                self.route
                    .iter()
                    .map(|route| (route.model.clone(), route.provider.clone())),
            )?
        } else if let Some(provider) = &self.model_provider {
            collect_routes(
                self.model
                    .iter()
                    .cloned()
                    .map(|model| (model, provider.clone())),
            )?
        } else {
            config.refit.codex.model_picker.routes.clone()
        };
        let default_provider = self
            .default_provider
            .as_deref()
            .unwrap_or_else(|| config.refit.codex.model_picker.default_provider());
        ModelProviderRouting::new(routes, default_provider).map_err(diagnostic)
    }
}

fn collect_routes(
    routes: impl IntoIterator<Item = (String, String)>,
) -> Result<BTreeMap<String, String>> {
    let mut collected = BTreeMap::new();
    for (model, provider) in routes {
        if let Some(previous) = collected.insert(model.clone(), provider.clone())
            && previous != provider
        {
            return Err(miette!(
                "model {model:?} has conflicting provider routes {previous:?} and {provider:?}"
            ));
        }
    }
    Ok(collected)
}

#[derive(Debug, Default)]
struct PendingBackgroundStage {
    value: Option<(CodexRefitStage, tokio::time::Instant)>,
}

impl PendingBackgroundStage {
    fn replace(&mut self, stage: CodexRefitStage, now: tokio::time::Instant) {
        if self.value.is_some_and(|(current, _)| current == stage) {
            return;
        }
        self.value = Some((stage, now + BACKGROUND_STAGE_STABILITY_DELAY));
    }

    fn deadline(&self) -> Option<tokio::time::Instant> {
        self.value.map(|(_, deadline)| deadline)
    }

    fn is_pending(&self) -> bool {
        self.value.is_some()
    }

    fn take_ready(&mut self, now: tokio::time::Instant) -> Option<CodexRefitStage> {
        if self.deadline().is_some_and(|deadline| deadline <= now) {
            self.value.take().map(|(stage, _)| stage)
        } else {
            None
        }
    }
}

#[derive(Debug, Args)]
#[command(
    arg_required_else_help = true,
    after_help = "CDP lifecycle commands accept -p, --port <PORT> for 127.0.0.1 (default: 55321). The update command does not use CDP."
)]
pub(crate) struct CodexRefitArgs {
    #[command(subcommand)]
    command: CodexRefitCommand,
}

#[derive(Debug, Subcommand)]
enum CodexRefitCommand {
    /// Enable a refit feature idempotently.
    Enable(CodexEnableArgs),
    /// Disable a refit feature and remove all of its renderer artifacts.
    Disable(CodexFeatureArgs),
    /// Inspect the current renderer refit state.
    Status(CodexPortArgs),
    /// Run read-only target, bridge, and state diagnostics.
    Doctor(CodexPortArgs),
    /// Check and install the latest validated renderer JavaScript from GitHub.
    Update(CodexUpdateArgs),
    /// Expose catalog models and optionally route them per task without changing global Codex auth.
    UnlockModelPicker(CodexUnlockArgs),
}

#[derive(Debug, Args)]
struct CodexFeatureArgs {
    #[arg(value_enum)]
    feature: CodexRefitFeature,

    #[command(flatten)]
    endpoint: CodexPortArgs,
}

#[derive(Debug, Args)]
struct CodexEnableArgs {
    #[arg(value_enum)]
    feature: CodexRefitFeature,

    /// Inject only the current document, confirm health, close CDP, and exit; persistent managed mode is the default.
    #[arg(short = 'o', long)]
    once: bool,

    /// Start a validated, stopped ChatGPT app once; otherwise enable is attach-only.
    #[arg(short = 'l', long)]
    launch: bool,

    /// Keep the persistent manager attached to this terminal for diagnostics; background managed mode is the default.
    #[arg(short = 'F', long, conflicts_with = "once")]
    foreground: bool,

    #[arg(long, hide = true, conflicts_with_all = ["once", "foreground"])]
    background_child: bool,

    #[command(flatten)]
    endpoint: CodexPortArgs,
}

#[derive(Debug, Args)]
struct CodexPortArgs {
    /// 127.0.0.1 CDP port; overrides refit.codex.debug_port and the 55321 fallback.
    #[arg(short = 'p', long)]
    port: Option<u16>,
}

impl CodexPortArgs {
    fn resolve(&self, config: &OpsailConfig) -> u16 {
        self.port.unwrap_or_else(|| config.refit.codex.debug_port())
    }
}

#[derive(Debug, Args)]
struct CodexUpdateArgs {
    /// Install validated JavaScript even when its SHA-256 differs from the active bundle.
    #[arg(short = 'f', long)]
    force: bool,
}

impl CodexUpdateArgs {
    fn policy(&self) -> RendererAssetUpdatePolicy {
        if self.force {
            RendererAssetUpdatePolicy::Force
        } else {
            RendererAssetUpdatePolicy::RequireUnchanged
        }
    }
}

impl CodexEnableArgs {
    fn session_mode(&self) -> SessionMode {
        if self.once {
            SessionMode::Once
        } else {
            SessionMode::Persistent
        }
    }

    fn launch_policy(&self) -> LaunchPolicy {
        if self.launch {
            LaunchPolicy::LaunchIfStopped
        } else {
            LaunchPolicy::AttachOnly
        }
    }

    fn should_spawn_background(&self) -> bool {
        self.session_mode() == SessionMode::Persistent && !self.foreground && !self.background_child
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CodexRefitFeature {
    /// Show remaining account rate-limit windows in the sidebar account row.
    Usage,
}

pub(crate) async fn run(
    args: CodexRefitArgs,
    config: &OpsailConfig,
    explicit_config_path: Option<&Path>,
) -> Result<()> {
    match args.command {
        CodexRefitCommand::Enable(enable) => {
            let port = enable.endpoint.resolve(config);
            if enable.should_spawn_background() {
                return spawn_background_manager(&enable, port, explicit_config_path).await;
            }
            if enable.background_child {
                return run_background_manager(enable, port).await;
            }
            let activity = CliActivity::start(if enable.launch {
                "Starting and enabling the Codex usage refit…"
            } else {
                "Enabling the Codex usage refit…"
            });
            let adapter =
                CodexRefit::new(config_with_activity(port, &activity)).map_err(diagnostic)?;
            let mode = enable.session_mode();
            let launch_policy = enable.launch_policy();
            let session = match enable.feature {
                CodexRefitFeature::Usage => adapter
                    .enable_usage(mode, launch_policy)
                    .await
                    .map_err(diagnostic)?,
            };
            drop(adapter);
            activity.finish();
            write_json(session.report())?;
            session.run().await.map_err(diagnostic)
        }
        CodexRefitCommand::Update(update) => {
            let activity = CliActivity::start("Checking Codex renderer updates…");
            let adapter =
                CodexRefit::new(config_with_activity(DEFAULT_CODEX_DEBUG_PORT, &activity))
                    .map_err(diagnostic)?;
            let report = adapter
                .update_renderer_assets(update.policy())
                .await
                .map_err(diagnostic)?;
            activity.finish();
            write_json(&report)
        }
        CodexRefitCommand::Disable(CodexFeatureArgs {
            feature: CodexRefitFeature::Usage,
            endpoint,
        }) => {
            let port = endpoint.resolve(config);
            let activity = CliActivity::start("Removing the Codex usage refit…");
            let adapter =
                CodexRefit::new(config_with_activity(port, &activity)).map_err(diagnostic)?;
            let report = adapter.disable_usage().await.map_err(diagnostic)?;
            activity.finish();
            write_json(&report)
        }
        CodexRefitCommand::Status(endpoint) => {
            let port = endpoint.resolve(config);
            let activity = CliActivity::start("Inspecting the Codex usage refit…");
            let adapter =
                CodexRefit::new(config_with_activity(port, &activity)).map_err(diagnostic)?;
            let report = adapter.status().await.map_err(diagnostic)?;
            activity.finish();
            write_json(&report)
        }
        CodexRefitCommand::Doctor(endpoint) => {
            let port = endpoint.resolve(config);
            let activity = CliActivity::start("Running Codex refit diagnostics…");
            let adapter =
                CodexRefit::new(config_with_activity(port, &activity)).map_err(diagnostic)?;
            let report = adapter.doctor().await;
            activity.finish();
            write_json(&report)
        }
        CodexRefitCommand::UnlockModelPicker(args) => {
            let port = args.endpoint.resolve(config);
            let provider_routing = args.routing(config)?;
            let activity = CliActivity::start(if args.launch {
                "Starting Codex and installing model-picker compatibility…"
            } else {
                "Installing Codex model-picker compatibility…"
            });
            let result = unlock_model_picker(port, args.launch, provider_routing)
                .await
                .map_err(diagnostic)?;
            activity.finish();
            write_json(&result)
        }
    }
}

fn stage_message(stage: CodexRefitStage) -> &'static str {
    match stage {
        CodexRefitStage::LoadRendererAssets => "Loading validated renderer assets…",
        CodexRefitStage::FetchUpdateManifest => "Checking the renderer update manifest…",
        CodexRefitStage::DownloadRendererAssets => "Downloading renderer assets…",
        CodexRefitStage::InstallRendererAssets => "Installing validated renderer assets…",
        CodexRefitStage::ValidateApplication => "Validating the signed ChatGPT app…",
        CodexRefitStage::InspectEndpoint => "Checking for an existing CDP endpoint…",
        CodexRefitStage::ValidateListener => "Validating the loopback CDP listener…",
        CodexRefitStage::DiscoverRenderer => "Discovering the Codex renderer…",
        CodexRefitStage::ValidateRenderer => "Validating the renderer and account bridge…",
        CodexRefitStage::CheckLaunchReadiness => "Checking the launch port and process state…",
        CodexRefitStage::LaunchApplication => "Starting ChatGPT in Opsail mode…",
        CodexRefitStage::WaitForEndpoint => "Waiting for ChatGPT's CDP endpoint…",
        CodexRefitStage::InspectUsage => "Inspecting the current usage refit…",
        CodexRefitStage::InjectUsage => "Injecting the usage capsule…",
        CodexRefitStage::ConfirmHealth => "Confirming renderer health…",
        CodexRefitStage::StartManager => "Starting the managed renderer session…",
        CodexRefitStage::StopManager => "Stopping the managed renderer session…",
        CodexRefitStage::CleanupUsage => "Removing renderer artifacts…",
        CodexRefitStage::RunDiagnostics => "Running Codex refit diagnostics…",
        _ => "Waiting for the Codex refit operation…",
    }
}

fn config_with_activity(port: u16, activity: &CliActivity) -> CodexRefitConfig {
    let handle = activity.handle();
    CodexRefitConfig::new(port).with_progress_handler(move |stage| {
        handle.set_message(stage_message(stage));
    })
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
enum BackgroundStartupMessage {
    Progress { stage: CodexRefitStage },
    Ready { report: Value },
    Error { code: String, message: String },
}

struct BackgroundChildGuard {
    child: Option<tokio::process::Child>,
    armed: bool,
}

impl BackgroundChildGuard {
    fn new(child: tokio::process::Child) -> Self {
        Self {
            child: Some(child),
            armed: true,
        }
    }

    fn child_mut(&mut self) -> &mut tokio::process::Child {
        self.child
            .as_mut()
            .expect("background child remains available during startup")
    }

    fn disarm(mut self) {
        self.armed = false;
    }

    async fn terminate(mut self) {
        self.armed = false;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }
}

impl Drop for BackgroundChildGuard {
    fn drop(&mut self) {
        if self.armed
            && let Some(child) = &mut self.child
        {
            let _ = child.start_kill();
        }
    }
}

fn background_command(
    enable: &CodexEnableArgs,
    port: u16,
    explicit_config_path: Option<&Path>,
) -> Result<tokio::process::Command> {
    let executable = std::env::current_exe()
        .into_diagnostic()
        .wrap_err("failed to resolve the current Opsail executable")?;
    let mut command = tokio::process::Command::new(executable);
    let port = port.to_string();
    if let Some(path) = explicit_config_path {
        command.arg("--config").arg(path);
    }
    command.args([
        "refit",
        "codex",
        "enable",
        "usage",
        "--background-child",
        "--port",
        &port,
    ]);
    if enable.launch {
        command.arg("--launch");
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    Ok(command)
}

async fn spawn_background_manager(
    enable: &CodexEnableArgs,
    port: u16,
    explicit_config_path: Option<&Path>,
) -> Result<()> {
    let activity = CliActivity::start(if enable.launch {
        "Starting ChatGPT and the Codex usage manager…"
    } else {
        "Starting the Codex usage manager…"
    });
    let child = background_command(enable, port, explicit_config_path)?
        .spawn()
        .into_diagnostic()
        .wrap_err("failed to start the background Codex refit manager")?;
    let mut child = BackgroundChildGuard::new(child);
    let stdout = child
        .child_mut()
        .stdout
        .take()
        .ok_or_else(|| miette!("background Codex refit manager has no startup channel"))?;
    let mut lines = BufReader::new(stdout).lines();
    let deadline = tokio::time::Instant::now() + BACKGROUND_START_TIMEOUT;
    let mut pending_stage = PendingBackgroundStage::default();
    loop {
        tokio::select! {
            biased;
            read = lines.next_line() => {
                let line = match read {
                    Ok(Some(line)) => line,
                    Ok(None) => {
                        let status = child.child_mut().wait().await.into_diagnostic()?;
                        child.armed = false;
                        return Err(miette!(
                            "background Codex refit manager exited before reporting readiness ({status})"
                        ));
                    }
                    Err(error) => {
                        child.terminate().await;
                        return Err(error)
                            .into_diagnostic()
                            .wrap_err("failed to read the background manager startup report");
                    }
                };
                if line.len() > MAX_BACKGROUND_START_MESSAGE_BYTES {
                    child.terminate().await;
                    return Err(miette!(
                        "background manager startup report exceeded its size limit"
                    ));
                }
                let message: BackgroundStartupMessage = match serde_json::from_str(&line) {
                    Ok(message) => message,
                    Err(error) => {
                        child.terminate().await;
                        return Err(error)
                            .into_diagnostic()
                            .wrap_err("background manager returned an invalid startup report");
                    }
                };
                match message {
                    BackgroundStartupMessage::Progress { stage } => {
                        pending_stage.replace(stage, tokio::time::Instant::now());
                    }
                    BackgroundStartupMessage::Ready { report } => {
                        validate_background_report(&report, port)?;
                        activity.finish();
                        write_json(&report)?;
                        child.disarm();
                        return Ok(());
                    }
                    BackgroundStartupMessage::Error { code, message } => {
                        child.terminate().await;
                        return Err(miette!("[opsail-refit-codex:{code}] {message}"));
                    }
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                child.terminate().await;
                return Err(miette!(
                    "background Codex refit manager did not report readiness within {} seconds",
                    BACKGROUND_START_TIMEOUT.as_secs()
                ));
            }
            _ = tokio::time::sleep_until(pending_stage.deadline().unwrap_or(deadline)), if pending_stage.is_pending() => {
                if let Some(stage) = pending_stage.take_ready(tokio::time::Instant::now()) {
                    activity.set_message(stage_message(stage));
                }
            }
        }
    }
}

fn validate_background_report(report: &Value, port: u16) -> Result<()> {
    let valid = report.get("operation").and_then(Value::as_str) == Some("enable")
        && report.get("port").and_then(Value::as_u64) == Some(u64::from(port))
        && report.get("sessionMode").and_then(Value::as_str) == Some("persistent")
        && report.get("targets").is_some_and(Value::is_array);
    if valid {
        Ok(())
    } else {
        Err(miette!(
            "background manager returned a mismatched startup report"
        ))
    }
}

async fn run_background_manager(enable: CodexEnableArgs, port: u16) -> Result<()> {
    let startup_open = Arc::new(AtomicBool::new(true));
    let progress_open = Arc::clone(&startup_open);
    let result = async {
        let config = CodexRefitConfig::new(port).with_progress_handler(move |stage| {
            if progress_open.load(Ordering::Acquire) {
                let _ = write_background_startup(&json!({
                    "status": "progress",
                    "stage": stage,
                }));
            }
        });
        let adapter = CodexRefit::new(config)?;
        adapter
            .enable_usage(SessionMode::Persistent, enable.launch_policy())
            .await
    }
    .await;
    let session = match result {
        Ok(session) => session,
        Err(error) => {
            startup_open.store(false, Ordering::Release);
            write_background_startup(&json!({
                "status": "error",
                "code": error.code().as_str(),
                "message": error.to_string(),
            }))?;
            return Err(diagnostic(error));
        }
    };
    startup_open.store(false, Ordering::Release);
    write_background_startup(&json!({
        "status": "ready",
        "report": session.report(),
    }))?;
    session.run().await.map_err(diagnostic)
}

fn write_background_startup(value: &Value) -> Result<()> {
    let output = with_trailing_newline(
        serde_json::to_string(value)
            .into_diagnostic()
            .wrap_err("failed to serialize background manager startup report")?,
    );
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    stdout
        .write_all(output.as_bytes())
        .and_then(|()| stdout.flush())
        .into_diagnostic()
        .wrap_err("failed to write the background manager startup report")
}

fn write_json(value: &impl serde::Serialize) -> Result<()> {
    let output = with_trailing_newline(
        serde_json::to_string_pretty(value)
            .into_diagnostic()
            .wrap_err("failed to serialize Codex refit result")?,
    );
    write_stdout(output.as_bytes())
}

fn diagnostic(error: CodexRefitError) -> miette::Report {
    miette!("[opsail-refit-codex:{}] {error}", error.code().as_str())
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;
    use crate::{Cli, Command, RefitArgs, RefitTarget};

    #[test]
    fn enable_parses_modes_launch_policy_and_port_defaults() {
        for (arguments, expected_mode, expected_launch, expected_port) in [
            (
                vec![
                    "opsail", "refit", "codex", "enable", "usage", "-o", "-l", "-p", "55400",
                ],
                SessionMode::Once,
                LaunchPolicy::LaunchIfStopped,
                55400,
            ),
            (
                vec!["opsail", "refit", "codex", "enable", "usage"],
                SessionMode::Persistent,
                LaunchPolicy::AttachOnly,
                DEFAULT_CODEX_DEBUG_PORT,
            ),
        ] {
            let cli = Cli::try_parse_from(arguments).unwrap();
            let Command::Refit(RefitArgs {
                target:
                    RefitTarget::Codex(CodexRefitArgs {
                        command: CodexRefitCommand::Enable(enable),
                    }),
            }) = cli.command
            else {
                panic!("expected Codex enable arguments");
            };
            assert_eq!(enable.session_mode(), expected_mode);
            assert_eq!(enable.launch_policy(), expected_launch);
            assert_eq!(
                enable.endpoint.resolve(&OpsailConfig::default()),
                expected_port
            );
            assert_eq!(
                enable.should_spawn_background(),
                expected_mode == SessionMode::Persistent
            );
        }
    }

    #[test]
    fn foreground_mode_is_explicit_and_background_child_command_is_bounded() {
        let cli = Cli::try_parse_from([
            "opsail",
            "refit",
            "codex",
            "enable",
            "usage",
            "--launch",
            "--foreground",
            "--port",
            "55400",
        ])
        .unwrap();
        let Command::Refit(RefitArgs {
            target:
                RefitTarget::Codex(CodexRefitArgs {
                    command: CodexRefitCommand::Enable(mut enable),
                }),
        }) = cli.command
        else {
            panic!("expected Codex enable arguments");
        };
        assert!(!enable.should_spawn_background());

        enable.foreground = false;
        let command = background_command(&enable, 55400, None).unwrap();
        let arguments = command
            .as_std()
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            arguments,
            [
                "refit",
                "codex",
                "enable",
                "usage",
                "--background-child",
                "--port",
                "55400",
                "--launch",
            ]
        );

        let command =
            background_command(&enable, 55400, Some(Path::new("/tmp/custom-opsail.toml"))).unwrap();
        let arguments = command
            .as_std()
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            &arguments[..2],
            ["--config", "/tmp/custom-opsail.toml"],
            "the background child must load the same explicit config"
        );
    }

    #[test]
    fn update_parses_as_an_independent_operation() {
        let cli = Cli::try_parse_from(["opsail", "refit", "codex", "update"]).unwrap();
        let Command::Refit(RefitArgs {
            target:
                RefitTarget::Codex(CodexRefitArgs {
                    command: CodexRefitCommand::Update(update),
                }),
        }) = cli.command
        else {
            panic!("expected Codex update arguments");
        };
        assert_eq!(update.policy(), RendererAssetUpdatePolicy::RequireUnchanged);

        let cli = Cli::try_parse_from(["opsail", "refit", "codex", "update", "-f"]).unwrap();
        let Command::Refit(RefitArgs {
            target:
                RefitTarget::Codex(CodexRefitArgs {
                    command: CodexRefitCommand::Update(update),
                    ..
                }),
        }) = cli.command
        else {
            panic!("expected forced Codex update arguments");
        };
        assert_eq!(update.policy(), RendererAssetUpdatePolicy::Force);
    }

    #[test]
    fn model_picker_routes_use_cli_over_config_and_support_multiple_providers() {
        let mut config = OpsailConfig::default();
        config.refit.codex.model_picker.routes.insert(
            "configured-model".to_owned(),
            "configured-provider".to_owned(),
        );
        let cli = Cli::try_parse_from([
            "opsail",
            "refit",
            "codex",
            "unlock-model-picker",
            "--route",
            "model-a=provider-a",
            "--route",
            "model-b=provider-b",
            "--default-provider",
            "native",
        ])
        .unwrap();
        let Command::Refit(RefitArgs {
            target:
                RefitTarget::Codex(CodexRefitArgs {
                    command: CodexRefitCommand::UnlockModelPicker(args),
                }),
        }) = cli.command
        else {
            panic!("expected model-picker arguments");
        };
        let routing = args.routing(&config).unwrap();
        assert_eq!(routing.routes.len(), 2);
        assert_eq!(routing.routes["model-a"], "provider-a");
        assert_eq!(routing.routes["model-b"], "provider-b");
        assert!(!routing.routes.contains_key("configured-model"));
        assert_eq!(routing.default_provider, "native");
    }

    #[test]
    fn model_picker_can_explicitly_ignore_configured_routes() {
        let mut config = OpsailConfig::default();
        config
            .refit
            .codex
            .model_picker
            .routes
            .insert("model".to_owned(), "provider".to_owned());
        let cli = Cli::try_parse_from([
            "opsail",
            "refit",
            "codex",
            "unlock-model-picker",
            "--no-provider-routing",
        ])
        .unwrap();
        let Command::Refit(RefitArgs {
            target:
                RefitTarget::Codex(CodexRefitArgs {
                    command: CodexRefitCommand::UnlockModelPicker(args),
                }),
        }) = cli.command
        else {
            panic!("expected model-picker arguments");
        };
        assert!(args.routing(&config).unwrap().routes.is_empty());
    }

    #[test]
    fn background_progress_messages_use_structured_stages() {
        let message: BackgroundStartupMessage =
            serde_json::from_str(r#"{"status":"progress","stage":"validate-application"}"#)
                .unwrap();
        let BackgroundStartupMessage::Progress { stage } = message else {
            panic!("expected a background progress message");
        };
        assert_eq!(stage, CodexRefitStage::ValidateApplication);
        assert_eq!(stage_message(stage), "Validating the signed ChatGPT app…");
    }

    #[test]
    fn background_progress_displays_only_the_latest_stage_after_it_stabilizes() {
        let started_at = tokio::time::Instant::now();
        let mut pending = PendingBackgroundStage::default();
        pending.replace(CodexRefitStage::ValidateApplication, started_at);
        assert_eq!(
            pending.take_ready(started_at + BACKGROUND_STAGE_STABILITY_DELAY),
            Some(CodexRefitStage::ValidateApplication),
        );

        pending.replace(CodexRefitStage::LaunchApplication, started_at);
        pending.replace(
            CodexRefitStage::WaitForEndpoint,
            started_at + Duration::from_millis(40),
        );
        let wait_deadline = pending.deadline();
        pending.replace(
            CodexRefitStage::WaitForEndpoint,
            started_at + Duration::from_millis(80),
        );
        assert_eq!(pending.deadline(), wait_deadline);
        assert_eq!(
            pending.take_ready(started_at + BACKGROUND_STAGE_STABILITY_DELAY),
            None,
        );
        assert_eq!(
            pending.take_ready(
                started_at + Duration::from_millis(40) + BACKGROUND_STAGE_STABILITY_DELAY,
            ),
            Some(CodexRefitStage::WaitForEndpoint),
        );
        assert!(!pending.is_pending());
    }
}
