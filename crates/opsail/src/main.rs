use std::ffi::OsStr;
use std::io::{self, ErrorKind, Write as _};
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::Duration;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use miette::{IntoDiagnostic, Result, WrapErr, miette};
use opsail_read::{CdpSource, CdpWaitUntil, ChromeSource, Input, ReadOptions, ReadResult, read};
use opsail_refit_codex::{
    CodexRefit, CodexRefitConfig, CodexRefitError, DEFAULT_CODEX_DEBUG_PORT, LaunchPolicy,
    RendererAssetUpdatePolicy, SessionMode,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tracing_subscriber::{EnvFilter, util::SubscriberInitExt};
use url::Url;

mod machine;

const PROPERTY_NAMES: &str = "content, markdown, contentHtml, html, title, author, description, site, published, modified, image, favicon, language, direction, url, canonicalUrl, domain, wordCount, quality, source, extraction";
const BACKGROUND_START_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_BACKGROUND_START_MESSAGE_BYTES: usize = 256 * 1024;

#[derive(Debug, Parser)]
#[command(
    name = "opsail",
    version,
    about = "Native tools that agents can rely on",
    before_help = "GitHub repository: https://github.com/lencx/opsail",
    subcommand_required = true,
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Increase diagnostic verbosity. Repeat for more detail.
    #[arg(short = 'v', long, action = ArgAction::Count, global = true)]
    verbose: u8,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Read a URL or HTML input and extract its primary content.
    #[command(visible_alias = "extract")]
    Read(Box<ReadArgs>),
    /// Apply a reversible, target-validated application refit.
    Refit(RefitArgs),
}

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
struct RefitArgs {
    #[command(subcommand)]
    target: RefitTarget,
}

#[derive(Debug, Subcommand)]
enum RefitTarget {
    /// Manage the verified Codex renderer in the macOS ChatGPT app.
    Codex(CodexRefitArgs),
}

#[derive(Debug, Args)]
#[command(
    arg_required_else_help = true,
    after_help = "CDP lifecycle commands accept -p, --port <PORT> for 127.0.0.1 (default: 55321). The update command does not use CDP."
)]
struct CodexRefitArgs {
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
    /// Check and install the latest verified renderer JavaScript from GitHub.
    Update(CodexUpdateArgs),
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

    #[arg(
        long,
        hide = true,
        conflicts_with_all = ["once", "foreground"]
    )]
    background_child: bool,

    #[command(flatten)]
    endpoint: CodexPortArgs,
}

#[derive(Debug, Args)]
struct CodexPortArgs {
    /// 127.0.0.1 CDP port; defaults to 55321 and may be explicitly overridden.
    #[arg(short = 'p', long, default_value_t = DEFAULT_CODEX_DEBUG_PORT)]
    port: u16,
}

#[derive(Debug, Args)]
struct CodexUpdateArgs {
    /// Install verified JavaScript even when its SHA-256 differs from the active bundle.
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

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
struct ReadArgs {
    /// Read one versioned JSON request from stdin and write one JSON response to stdout.
    #[arg(
        long,
        conflicts_with_all = [
            "source",
            "format",
            "property",
            "output",
            "base_url",
            "timeout",
            "max_bytes",
            "user_agent",
            "accept_language",
            "cdp",
            "launch",
            "chrome_path",
            "target_id",
            "cdp_direct",
            "wait_until"
        ]
    )]
    machine: bool,

    /// URL, HTML file, or '-' for stdin. Browser modes accept an HTTP(S) URL.
    #[arg(value_name = "SOURCE")]
    source: Option<PathBuf>,

    /// Output representation.
    #[arg(long, value_enum, default_value_t = OutputFormat::Markdown)]
    format: OutputFormat,

    /// Emit one named result property instead of the full representation.
    #[arg(long, value_name = "NAME", value_parser = [
        "content", "markdown", "contentHtml", "html", "title", "author",
        "description", "site", "published", "modified", "image", "favicon",
        "language", "direction", "url", "canonicalUrl", "domain", "wordCount",
        "quality", "source", "extraction"
    ])]
    property: Option<String>,

    /// Write data to a file instead of stdout. Use '-' for stdout.
    #[arg(long, value_name = "PATH")]
    output: Option<PathBuf>,

    /// HTTP(S) base URL used to resolve relative links in file or stdin input.
    #[arg(
        long,
        value_name = "URL",
        conflicts_with_all = ["cdp", "launch", "chrome_path"]
    )]
    base_url: Option<Url>,

    /// Capture through a caller-managed Chrome DevTools Protocol endpoint.
    #[arg(long, value_name = "ENDPOINT", conflicts_with = "launch")]
    cdp: Option<String>,

    /// Discover, launch, and stop an isolated local Chrome process.
    #[arg(long, conflicts_with = "cdp")]
    launch: bool,

    /// Chrome or Chromium executable used by --launch.
    #[arg(long, value_name = "PATH", requires = "launch", conflicts_with = "cdp")]
    chrome_path: Option<PathBuf>,

    /// Capture or navigate this existing CDP page target.
    #[arg(
        long,
        value_name = "ID",
        requires = "cdp",
        conflicts_with = "cdp_direct"
    )]
    target_id: Option<String>,

    /// Treat --cdp as a page-scoped provider WebSocket.
    #[arg(long, requires = "cdp")]
    cdp_direct: bool,

    /// Browser lifecycle event to await after CDP navigation.
    #[arg(long, value_enum, value_name = "STATE")]
    wait_until: Option<CdpWaitArg>,

    /// Overall HTTP or Chrome CDP acquisition timeout in seconds.
    #[arg(long, value_name = "SECONDS", value_parser = parse_positive_u64)]
    timeout: Option<u64>,

    /// Maximum number of input bytes to read.
    #[arg(long, value_name = "BYTES", value_parser = parse_positive_usize)]
    max_bytes: Option<usize>,

    /// User-Agent used for HTTP requests or Chrome CDP navigation.
    #[arg(long, value_name = "VALUE")]
    user_agent: Option<String>,

    /// Accept-Language used for HTTP requests or Chrome CDP navigation.
    #[arg(long, value_name = "VALUE")]
    accept_language: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    Markdown,
    Html,
    Json,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CdpWaitArg {
    None,
    DomContentLoaded,
    Load,
    NetworkIdle,
}

impl From<CdpWaitArg> for CdpWaitUntil {
    fn from(value: CdpWaitArg) -> Self {
        match value {
            CdpWaitArg::None => Self::None,
            CdpWaitArg::DomContentLoaded => Self::DomContentLoaded,
            CdpWaitArg::Load => Self::Load,
            CdpWaitArg::NetworkIdle => Self::NetworkIdle,
        }
    }
}

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = writeln!(
                io::stderr().lock(),
                "failed to initialize async runtime: {error}"
            );
            return ExitCode::FAILURE;
        }
    };
    let exit_code = runtime.block_on(async_main());
    runtime.shutdown_timeout(Duration::from_millis(500));
    exit_code
}

async fn async_main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    tokio::select! {
        exit_code = execute(cli.command) => exit_code,
        () = shutdown_signal() => ExitCode::from(130),
    }
}

async fn execute(command: Command) -> ExitCode {
    match command {
        Command::Read(args) if args.machine => machine::run().await,
        command => match run(command).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let _ = writeln!(io::stderr().lock(), "{error:?}");
                ExitCode::FAILURE
            }
        },
    }
}

fn init_tracing(verbosity: u8) {
    let level = match verbosity {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::new(format!(
        "error,opsail={level},opsail_read={level},opsail_chrome={level},opsail_refit_codex={level}"
    ));

    tracing_subscriber::fmt()
        .compact()
        .without_time()
        .with_target(false)
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .finish()
        .init();
}

#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut interrupt = signal(SignalKind::interrupt()).ok();
    let mut terminate = signal(SignalKind::terminate()).ok();
    let mut hangup = signal(SignalKind::hangup()).ok();
    let mut terminal_stop = signal(SignalKind::from_raw(libc::SIGTSTP)).ok();
    tokio::select! {
        () = receive_optional_signal(&mut interrupt) => {}
        () = receive_optional_signal(&mut terminate) => {}
        () = receive_optional_signal(&mut hangup) => {}
        () = receive_optional_signal(&mut terminal_stop) => {}
    }
}

#[cfg(unix)]
async fn receive_optional_signal(signal: &mut Option<tokio::signal::unix::Signal>) {
    match signal {
        Some(signal) => {
            let _ = signal.recv().await;
        }
        None => std::future::pending::<()>().await,
    }
}

#[cfg(windows)]
async fn shutdown_signal() {
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}

#[cfg(not(any(unix, windows)))]
async fn shutdown_signal() {
    std::future::pending::<()>().await;
}

fn parse_positive_u64(value: &str) -> std::result::Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|error| error.to_string())
        .and_then(|value| {
            (value > 0)
                .then_some(value)
                .ok_or_else(|| "value must be greater than zero".to_owned())
        })
}

fn parse_positive_usize(value: &str) -> std::result::Result<usize, String> {
    value
        .parse::<usize>()
        .map_err(|error| error.to_string())
        .and_then(|value| {
            (value > 0)
                .then_some(value)
                .ok_or_else(|| "value must be greater than zero".to_owned())
        })
}

async fn run(command: Command) -> Result<()> {
    match command {
        Command::Read(args) => run_read(*args).await,
        Command::Refit(args) => run_refit(args).await,
    }
}

async fn run_refit(args: RefitArgs) -> Result<()> {
    match args.target {
        RefitTarget::Codex(args) => run_codex_refit(args).await,
    }
}

async fn run_codex_refit(args: CodexRefitArgs) -> Result<()> {
    match args.command {
        CodexRefitCommand::Enable(enable) => {
            if enable.should_spawn_background() {
                return spawn_background_codex_manager(&enable).await;
            }
            if enable.background_child {
                return run_background_codex_manager(enable).await;
            }
            let adapter = CodexRefit::new(CodexRefitConfig::new(enable.endpoint.port))
                .map_err(codex_diagnostic)?;
            let mode = enable.session_mode();
            let launch_policy = enable.launch_policy();
            let session = match enable.feature {
                CodexRefitFeature::Usage => adapter
                    .enable_usage(mode, launch_policy)
                    .await
                    .map_err(codex_diagnostic)?,
            };
            write_codex_json(session.report())?;
            session.run().await.map_err(codex_diagnostic)
        }
        CodexRefitCommand::Update(update) => {
            let adapter = CodexRefit::new(CodexRefitConfig::default()).map_err(codex_diagnostic)?;
            let report = adapter
                .update_renderer_assets(update.policy())
                .await
                .map_err(codex_diagnostic)?;
            write_codex_json(&report)
        }
        CodexRefitCommand::Disable(CodexFeatureArgs {
            feature: CodexRefitFeature::Usage,
            endpoint,
        }) => {
            let adapter =
                CodexRefit::new(CodexRefitConfig::new(endpoint.port)).map_err(codex_diagnostic)?;
            write_codex_json(&adapter.disable_usage().await.map_err(codex_diagnostic)?)
        }
        CodexRefitCommand::Status(endpoint) => {
            let adapter =
                CodexRefit::new(CodexRefitConfig::new(endpoint.port)).map_err(codex_diagnostic)?;
            write_codex_json(&adapter.status().await.map_err(codex_diagnostic)?)
        }
        CodexRefitCommand::Doctor(endpoint) => {
            let adapter =
                CodexRefit::new(CodexRefitConfig::new(endpoint.port)).map_err(codex_diagnostic)?;
            write_codex_json(&adapter.doctor().await)
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
enum BackgroundStartupMessage {
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

fn background_codex_command(enable: &CodexEnableArgs) -> Result<tokio::process::Command> {
    let executable = std::env::current_exe()
        .into_diagnostic()
        .wrap_err("failed to resolve the current Opsail executable")?;
    let mut command = tokio::process::Command::new(executable);
    let port = enable.endpoint.port.to_string();
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
    Ok(command)
}

async fn spawn_background_codex_manager(enable: &CodexEnableArgs) -> Result<()> {
    let child = background_codex_command(enable)?
        .spawn()
        .into_diagnostic()
        .wrap_err("failed to start the background Codex refit manager")?;
    let mut child = BackgroundChildGuard::new(child);
    let stdout = child
        .child_mut()
        .stdout
        .take()
        .ok_or_else(|| miette!("background Codex refit manager has no startup channel"))?;
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let read = tokio::time::timeout(BACKGROUND_START_TIMEOUT, reader.read_line(&mut line)).await;
    let bytes = match read {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error)) => {
            child.terminate().await;
            return Err(error)
                .into_diagnostic()
                .wrap_err("failed to read the background manager startup report");
        }
        Err(_) => {
            child.terminate().await;
            return Err(miette!(
                "background Codex refit manager did not report readiness within {} seconds",
                BACKGROUND_START_TIMEOUT.as_secs()
            ));
        }
    };
    if bytes == 0 {
        let status = child.child_mut().wait().await.into_diagnostic()?;
        child.armed = false;
        return Err(miette!(
            "background Codex refit manager exited before reporting readiness ({status})"
        ));
    }
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
        BackgroundStartupMessage::Ready { report } => {
            validate_background_report(&report, enable.endpoint.port)?;
            write_codex_json(&report)?;
            child.disarm();
            Ok(())
        }
        BackgroundStartupMessage::Error { code, message } => {
            child.terminate().await;
            Err(miette!("[opsail-refit-codex:{code}] {message}"))
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

async fn run_background_codex_manager(enable: CodexEnableArgs) -> Result<()> {
    let result = async {
        let adapter = CodexRefit::new(CodexRefitConfig::new(enable.endpoint.port))?;
        adapter
            .enable_usage(SessionMode::Persistent, enable.launch_policy())
            .await
    }
    .await;
    let session = match result {
        Ok(session) => session,
        Err(error) => {
            write_background_startup(&json!({
                "status": "error",
                "code": error.code().as_str(),
                "message": error.to_string(),
            }))?;
            return Err(codex_diagnostic(error));
        }
    };
    write_background_startup(&json!({
        "status": "ready",
        "report": session.report(),
    }))?;
    session.run().await.map_err(codex_diagnostic)
}

fn write_background_startup(value: &Value) -> Result<()> {
    let output = with_trailing_newline(
        serde_json::to_string(value)
            .into_diagnostic()
            .wrap_err("failed to serialize background manager startup report")?,
    );
    write_stdout(output.as_bytes())
}

fn write_codex_json(value: &impl serde::Serialize) -> Result<()> {
    let output = with_trailing_newline(
        serde_json::to_string_pretty(value)
            .into_diagnostic()
            .wrap_err("failed to serialize Codex refit result")?,
    );
    write_stdout(output.as_bytes())
}

fn codex_diagnostic(error: CodexRefitError) -> miette::Report {
    miette!("[opsail-refit-codex:{}] {error}", error.code().as_str())
}

async fn run_read(args: ReadArgs) -> Result<()> {
    let ReadArgs {
        machine: _,
        source,
        format,
        property,
        output,
        base_url,
        cdp,
        launch,
        chrome_path,
        target_id,
        cdp_direct,
        wait_until,
        timeout,
        max_bytes,
        user_agent,
        accept_language,
    } = args;

    if let Some(base_url) = &base_url {
        validate_web_url(base_url, "base URL")?;
    }

    let mut options = ReadOptions {
        base_url,
        ..ReadOptions::default()
    };
    if let Some(timeout) = timeout {
        options.timeout = Duration::from_secs(timeout);
    }
    if let Some(max_bytes) = max_bytes {
        options.max_bytes = max_bytes;
    }
    options.user_agent = user_agent;
    if let Some(accept_language) = accept_language {
        options.accept_language = Some(accept_language);
    }

    let requested_browser_wait = wait_until.is_some();
    let wait_until = wait_until.map_or_else(CdpWaitUntil::default, Into::into);
    let input = match (cdp, launch) {
        (Some(endpoint), false) => {
            resolve_cdp_input(source, endpoint, target_id, cdp_direct, wait_until)?
        }
        (None, true) => resolve_chrome_input(source, chrome_path, wait_until)?,
        (None, false) => {
            if requested_browser_wait {
                return Err(miette!("--wait-until requires --cdp or --launch"));
            }
            resolve_input(source, options.max_bytes).await?
        }
        (Some(_), true) => unreachable!("clap rejects --cdp with --launch"),
    };
    let result = read(input, &options)
        .await
        .into_diagnostic()
        .wrap_err("failed to read source")?;

    for warning in &result.warnings {
        tracing::warn!(warning = %warning, "read warning");
    }

    let data = render_result(&result, format, property.as_deref())?;
    write_output(output.as_deref(), &data).await
}

fn resolve_chrome_input(
    source: Option<PathBuf>,
    executable_path: Option<PathBuf>,
    wait_until: CdpWaitUntil,
) -> Result<Input> {
    let source = source.ok_or_else(|| miette!("--launch requires an HTTP(S) SOURCE URL"))?;
    let value = source
        .to_str()
        .ok_or_else(|| miette!("Chrome source URL must be valid Unicode"))?;
    let url = Url::parse(value)
        .into_diagnostic()
        .wrap_err("invalid Chrome source URL")?;
    validate_web_url(&url, "Chrome source URL")?;

    Ok(Input::Chrome(ChromeSource {
        url,
        executable_path,
        wait_until,
    }))
}

fn resolve_cdp_input(
    source: Option<PathBuf>,
    endpoint: String,
    target_id: Option<String>,
    direct_page: bool,
    wait_until: CdpWaitUntil,
) -> Result<Input> {
    if endpoint.is_empty() {
        return Err(miette!("CDP endpoint must not be empty"));
    }
    if target_id.as_deref().is_some_and(str::is_empty) {
        return Err(miette!("CDP target ID must not be empty"));
    }

    let url = source
        .map(|source| {
            let value = source
                .to_str()
                .ok_or_else(|| miette!("CDP source URL must be valid Unicode"))?;
            if value == "-" || !value.contains("://") {
                return Err(miette!(
                    "when --cdp is used, SOURCE must be an HTTP(S) URL or omitted"
                ));
            }
            let url = Url::parse(value)
                .into_diagnostic()
                .wrap_err("invalid CDP source URL")?;
            validate_web_url(&url, "CDP source URL")?;
            Ok(url)
        })
        .transpose()?;

    Ok(Input::Cdp(CdpSource {
        endpoint,
        url,
        target_id,
        direct_page,
        wait_until,
    }))
}

async fn resolve_input(source: Option<PathBuf>, max_bytes: usize) -> Result<Input> {
    let Some(source) = source else {
        return read_stdin(max_bytes).await.map(Input::Stdin);
    };

    if source.as_os_str() == OsStr::new("-") {
        return read_stdin(max_bytes).await.map(Input::Stdin);
    }

    if let Some(value) = source.to_str()
        && value.contains("://")
    {
        let url = Url::parse(value)
            .into_diagnostic()
            .wrap_err_with(|| format!("invalid source URL `{value}`"))?;
        validate_web_url(&url, "source URL")?;
        return Ok(Input::Url(url));
    }

    Ok(Input::File(source))
}

fn validate_web_url(url: &Url, label: &str) -> Result<()> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(miette!("{label} must use HTTP or HTTPS"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(miette!("{label} must not contain embedded credentials"));
    }
    Ok(())
}

async fn read_stdin(max_bytes: usize) -> Result<Vec<u8>> {
    let limit = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut input = Vec::new();
    tokio::io::stdin()
        .take(limit)
        .read_to_end(&mut input)
        .await
        .into_diagnostic()
        .wrap_err("failed to read stdin")?;
    Ok(input)
}

fn render_result(
    result: &ReadResult,
    format: OutputFormat,
    property: Option<&str>,
) -> Result<Vec<u8>> {
    let rendered = match property {
        Some(name) => {
            let value = result.property(name).ok_or_else(|| {
                miette!("unknown property `{name}`; expected one of: {PROPERTY_NAMES}")
            })?;
            render_property(value, format)?
        }
        None => match format {
            OutputFormat::Markdown => result.content.clone(),
            OutputFormat::Html => result.content_html.clone(),
            OutputFormat::Json => serde_json::to_string_pretty(result)
                .into_diagnostic()
                .wrap_err("failed to serialize result")?,
        },
    };

    Ok(with_trailing_newline(rendered).into_bytes())
}

fn render_property(value: Value, format: OutputFormat) -> Result<String> {
    if matches!(format, OutputFormat::Json) {
        return serde_json::to_string_pretty(&value)
            .into_diagnostic()
            .wrap_err("failed to serialize property");
    }

    match value {
        Value::Null => Ok(String::new()),
        Value::String(value) => Ok(value),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Number(value) => Ok(value.to_string()),
        value @ (Value::Array(_) | Value::Object(_)) => serde_json::to_string_pretty(&value)
            .into_diagnostic()
            .wrap_err("failed to serialize property"),
    }
}

fn with_trailing_newline(mut value: String) -> String {
    if !value.ends_with('\n') {
        value.push('\n');
    }
    value
}

async fn write_output(path: Option<&Path>, data: &[u8]) -> Result<()> {
    match path {
        None => write_stdout(data),
        Some(path) if path.as_os_str() == OsStr::new("-") => write_stdout(data),
        Some(path) => tokio::fs::write(path, data)
            .await
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to write output `{}`", path.display())),
    }
}

fn write_stdout(data: &[u8]) -> Result<()> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    finish_stdout_write((|| {
        stdout.write_all(data)?;
        stdout.flush()
    })())
}

fn finish_stdout_write(result: io::Result<()>) -> Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::BrokenPipe => Ok(()),
        Err(error) => {
            let failure: io::Result<()> = Err(error);
            failure.into_diagnostic().wrap_err("failed to write stdout")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broken_pipe_is_a_successful_write_termination() {
        let error = io::Error::new(ErrorKind::BrokenPipe, "consumer closed the pipe");
        assert!(finish_stdout_write(Err(error)).is_ok());
    }

    #[test]
    fn codex_enable_parses_modes_launch_policy_and_port_defaults() {
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
            assert_eq!(enable.endpoint.port, expected_port);
            assert_eq!(
                enable.should_spawn_background(),
                expected_mode == SessionMode::Persistent
            );
        }
    }

    #[test]
    fn codex_foreground_mode_is_explicit_and_background_child_command_is_bounded() {
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
        let command = background_codex_command(&enable).unwrap();
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
    }

    #[test]
    fn codex_update_parses_as_an_independent_operation() {
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
}
