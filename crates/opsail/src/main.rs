use std::ffi::OsStr;
use std::io::{self, ErrorKind, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use miette::{IntoDiagnostic, Result, WrapErr, miette};
use opsail_read::{CdpSource, CdpWaitUntil, ChromeSource, Input, ReadOptions, ReadResult, read};
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tracing_subscriber::{EnvFilter, util::SubscriberInitExt};
use url::Url;

mod activity;
mod codex;
mod machine;

const PROPERTY_NAMES: &str = "content, markdown, contentHtml, html, title, author, description, site, published, modified, image, favicon, language, direction, url, canonicalUrl, domain, wordCount, quality, source, extraction";

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
    /// Manage the verified Codex renderer in the macOS or Windows ChatGPT app.
    Codex(codex::CodexRefitArgs),
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
        RefitTarget::Codex(args) => codex::run(args).await,
    }
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
}
