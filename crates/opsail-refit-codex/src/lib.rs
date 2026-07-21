//! Safe Codex renderer refits for Opsail.
//!
//! The adapter is intentionally narrow: it connects only to a `127.0.0.1` CDP
//! endpoint owned by the signed macOS ChatGPT process tree. Enable is
//! attach-only unless the caller explicitly selects [`LaunchPolicy::LaunchIfStopped`].
//! Opsail never quits, restarts, modifies, or signs the application.

mod cdp;
mod error;
mod launch;
mod lifecycle;
mod model;
mod payload;
mod platform;
mod state;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::stream::{FuturesUnordered, StreamExt as _};
use serde::Deserialize;
use tokio::time::sleep;

use cdp::{CdpSession, discover_targets, wait_for_termination};
pub use error::{CodexRefitError, CodexRefitErrorCode};
use launch::{LaunchBackend, SystemLaunchBackend};
use lifecycle::{ManagedSession, SessionFuture};
pub use model::{
    CodexDoctorReport, CodexRefitOperation, CodexRefitReport, CodexRefitState, CodexTargetHealth,
    DoctorCheck, DoctorCheckState, LaunchPolicy, SessionMode,
};
use payload::{
    UsagePayload, disable_expression, renderer_probe_expression, status_expression, usage_payload,
};
use state::{StateManagedSessionLock, StateStore, TargetRecord};

pub const DEFAULT_CODEX_DEBUG_PORT: u16 = 55321;
const USAGE_RUNTIME_LISTENER_COUNT: usize = 10;
const APP_LAUNCH_TIMEOUT: Duration = Duration::from_secs(15);
const MANAGER_STOP_TIMEOUT: Duration = Duration::from_secs(3);

/// Configuration for the verified Codex renderer adapter.
#[derive(Debug, Clone)]
pub struct CodexRefitConfig {
    port: u16,
    state_dir: Option<PathBuf>,
}

impl Default for CodexRefitConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_CODEX_DEBUG_PORT,
            state_dir: None,
        }
    }
}

impl CodexRefitConfig {
    pub fn new(port: u16) -> Self {
        Self {
            port,
            state_dir: None,
        }
    }

    /// Override the state directory for an embedding application or test.
    pub fn with_state_dir(mut self, state_dir: PathBuf) -> Self {
        self.state_dir = Some(state_dir);
        self
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

/// Entry point for the Codex usage refit lifecycle.
#[derive(Clone)]
pub struct CodexRefit {
    port: u16,
    state: StateStore,
    payload: Arc<UsagePayload>,
    launch_backend: Arc<dyn LaunchBackend>,
}

/// An enabled usage session and its initial health report.
pub struct CodexUsageSession {
    mode: SessionMode,
    report: CodexRefitReport,
    supervisor: Option<PersistentSupervisor>,
}

impl CodexUsageSession {
    pub fn mode(&self) -> SessionMode {
        self.mode
    }

    pub fn report(&self) -> &CodexRefitReport {
        &self.report
    }

    /// Keep a persistent session attached. A once session returns immediately.
    pub async fn run(self) -> Result<(), CodexRefitError> {
        match self.supervisor {
            Some(supervisor) => supervisor.run().await,
            None => Ok(()),
        }
    }
}

struct PersistentSupervisor {
    adapter: CodexRefit,
    sessions: Vec<CodexSession>,
    manager_token: String,
    _managed_lock: StateManagedSessionLock,
}

#[derive(Debug)]
struct ReconnectBackoff {
    next: Duration,
    maximum: Duration,
}

impl Default for ReconnectBackoff {
    fn default() -> Self {
        Self {
            next: Duration::from_millis(250),
            maximum: Duration::from_secs(30),
        }
    }
}

impl ReconnectBackoff {
    fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = self.next.saturating_mul(2).min(self.maximum);
        delay
    }

    fn reset(&mut self) {
        self.next = Duration::from_millis(250);
    }
}

impl CodexRefit {
    pub fn new(config: CodexRefitConfig) -> Result<Self, CodexRefitError> {
        if config.port < 1024 {
            return Err(CodexRefitError::new(
                CodexRefitErrorCode::TargetValidationFailed,
                "the Codex debug port must be between 1024 and 65535",
            ));
        }
        let state_dir = match config.state_dir {
            Some(path) => path,
            None => platform::default_state_dir()?,
        };
        Ok(Self {
            port: config.port,
            state: StateStore::new(state_dir),
            payload: Arc::new(usage_payload()?),
            launch_backend: Arc::new(SystemLaunchBackend),
        })
    }

    pub async fn enable_usage(
        &self,
        mode: SessionMode,
        launch_policy: LaunchPolicy,
    ) -> Result<CodexUsageSession, CodexRefitError> {
        let _operation_lock = self.state.try_operation_lock()?;
        match mode {
            SessionMode::Once => {
                if self.state.managed_session_active()? {
                    return Err(CodexRefitError::new(
                        CodexRefitErrorCode::SessionUnavailable,
                        "a persistent foreground manager is active; stop it before using once mode",
                    ));
                }
                let manager_token = new_manager_token();
                let (mut sessions, result) = self
                    .connect_and_enable_with_policy(
                        SessionMode::Once,
                        launch_policy,
                        &manager_token,
                    )
                    .await?;
                close_sessions(&mut sessions).await;
                Ok(CodexUsageSession {
                    mode,
                    report: result,
                    supervisor: None,
                })
            }
            SessionMode::Persistent => {
                let Some(managed_lock) = self.state.try_managed_session_lock()? else {
                    return self.existing_persistent_session(launch_policy).await;
                };
                let manager_token = new_manager_token();
                let (sessions, report) = self
                    .connect_and_enable_with_policy(
                        SessionMode::Persistent,
                        launch_policy,
                        &manager_token,
                    )
                    .await?;
                Ok(CodexUsageSession {
                    mode,
                    report,
                    supervisor: Some(PersistentSupervisor {
                        adapter: self.clone(),
                        sessions,
                        manager_token,
                        _managed_lock: managed_lock,
                    }),
                })
            }
        }
    }

    pub async fn disable_usage(&self) -> Result<CodexRefitReport, CodexRefitError> {
        let _operation_lock = self.state.try_operation_lock()?;
        if self.state.managed_session_active()? {
            self.stop_managed_session().await?;
        }
        let mut sessions = self.connect_sessions(&new_manager_token(), None).await?;
        if let Err(error) = self.prune_absent_sessions(&sessions) {
            close_sessions(&mut sessions).await;
            return Err(error);
        }
        let result = lifecycle::disable(&mut sessions)
            .await
            .map(|report| self.report_with_port(report));
        close_sessions(&mut sessions).await;
        result
    }

    pub async fn status(&self) -> Result<CodexRefitReport, CodexRefitError> {
        let mut sessions = self.connect_sessions(&new_manager_token(), None).await?;
        let result = lifecycle::status(&mut sessions)
            .await
            .map(|report| self.report_with_port(report));
        close_sessions(&mut sessions).await;
        result
    }

    /// Run read-only checks. This never injects, launches, stops, or reloads the app.
    pub async fn doctor(&self) -> CodexDoctorReport {
        let mut checks = Vec::new();
        let mut detected_session_modes = Vec::new();
        if !platform::is_supported() {
            checks.push(DoctorCheck {
                name: "platform",
                state: DoctorCheckState::Fail,
                message: "only the verified macOS ChatGPT target is currently supported".to_owned(),
            });
            return CodexDoctorReport {
                supported: false,
                ready: false,
                port: self.port,
                default_session_mode: SessionMode::Persistent,
                detected_session_modes,
                checks,
            };
        }
        checks.push(DoctorCheck {
            name: "platform",
            state: DoctorCheckState::Pass,
            message: "macOS is supported".to_owned(),
        });

        if let Err(error) = run_blocking(platform::validate_app).await {
            checks.push(failed_check("application", &error));
            return doctor_report(self.port, checks, detected_session_modes);
        }
        checks.push(DoctorCheck {
            name: "application",
            state: DoctorCheckState::Pass,
            message: "the signed ChatGPT application identity is valid".to_owned(),
        });

        match self.state.validate() {
            Ok(()) => checks.push(DoctorCheck {
                name: "state",
                state: DoctorCheckState::Pass,
                message: "local refit state is valid".to_owned(),
            }),
            Err(error) => checks.push(failed_check("state", &error)),
        }

        let port = self.port;
        let identity = match run_blocking(move || platform::validate_runtime(port)).await {
            Ok(identity) => {
                checks.push(DoctorCheck {
                    name: "listener",
                    state: DoctorCheckState::Pass,
                    message:
                        "the loopback debug listener belongs to the signed ChatGPT process tree"
                            .to_owned(),
                });
                identity
            }
            Err(error) => {
                checks.push(failed_check("listener", &error));
                return doctor_report(self.port, checks, detected_session_modes);
            }
        };

        let targets = match discover_targets(self.port).await {
            Ok(targets) => {
                checks.push(DoctorCheck {
                    name: "discovery",
                    state: DoctorCheckState::Pass,
                    message: format!(
                        "found {} locally validated app renderer target(s)",
                        targets.len()
                    ),
                });
                targets
            }
            Err(error) => {
                checks.push(failed_check("discovery", &error));
                return doctor_report(self.port, checks, detected_session_modes);
            }
        };

        let mut valid_renderers = 0usize;
        let mut bridge_missing = false;
        for target in &targets {
            let Ok(mut session) = CdpSession::connect(target).await else {
                continue;
            };
            match probe_renderer(&mut session).await {
                Ok(()) => {
                    valid_renderers = valid_renderers.saturating_add(1);
                    if let Ok(status) = renderer_status(&mut session).await
                        && let Some(mode) = status
                            .diagnostics
                            .filter(|diagnostics| diagnostics.installed)
                            .map(|diagnostics| diagnostics.session_mode)
                        && !detected_session_modes.contains(&mode)
                    {
                        detected_session_modes.push(mode);
                    }
                }
                Err(error) if error.code() == CodexRefitErrorCode::BridgeUnavailable => {
                    bridge_missing = true;
                }
                Err(_) => {}
            }
            session.close().await;
        }
        if valid_renderers == 0 {
            checks.push(DoctorCheck {
                name: "renderer",
                state: DoctorCheckState::Fail,
                message: if bridge_missing {
                    "the expected local account bridge is unavailable".to_owned()
                } else {
                    "no renderer matched the expected app shell and sidebar".to_owned()
                },
            });
            return doctor_report(self.port, checks, detected_session_modes);
        }
        checks.push(DoctorCheck {
            name: "renderer",
            state: DoctorCheckState::Pass,
            message: format!(
                "validated {valid_renderers} renderer(s) with the expected local account bridge"
            ),
        });

        detected_session_modes.sort_unstable();
        let managed_active = self.state.managed_session_active();
        let (mode_state, mode_message) = match managed_active {
            Err(error) => (
                DoctorCheckState::Fail,
                format!("{}: {error}", error.code().as_str()),
            ),
            Ok(active)
                if detected_session_modes.contains(&SessionMode::Persistent) && !active =>
            {
                (
                    DoctorCheckState::Warning,
                    "persistent (managed) renderer artifacts exist without an active foreground manager"
                        .to_owned(),
                )
            }
            Ok(true) => (
                DoctorCheckState::Pass,
                "persistent (managed) mode is active".to_owned(),
            ),
            Ok(false) if detected_session_modes.contains(&SessionMode::Once) => (
                DoctorCheckState::Pass,
                "once (ephemeral) mode is installed for the current document".to_owned(),
            ),
            Ok(false) => (
                DoctorCheckState::Pass,
                "no active refit session; the default enable mode is persistent (managed)"
                    .to_owned(),
            ),
        };
        checks.push(DoctorCheck {
            name: "session-mode",
            state: mode_state,
            message: mode_message,
        });

        let port = self.port;
        match run_blocking(move || platform::revalidate_runtime(port, &identity)).await {
            Ok(()) => checks.push(DoctorCheck {
                name: "identity-stability",
                state: DoctorCheckState::Pass,
                message: "the listener identity remained stable during validation".to_owned(),
            }),
            Err(error) => checks.push(failed_check("identity-stability", &error)),
        }
        doctor_report(self.port, checks, detected_session_modes)
    }

    async fn connect_sessions(
        &self,
        manager_token: &str,
        launched_pid: Option<u32>,
    ) -> Result<Vec<CodexSession>, CodexRefitError> {
        let port = self.port;
        let identity = run_blocking(move || platform::validate_runtime(port)).await?;
        let targets = discover_targets(self.port).await?;
        let mut sessions = Vec::new();
        let mut bridge_missing = false;
        let mut session_error = None;
        for target in targets {
            let mut cdp = match CdpSession::connect(&target).await {
                Ok(session) => session,
                Err(error) => {
                    if session_error.is_none() {
                        session_error = Some(error);
                    }
                    continue;
                }
            };
            match probe_renderer(&mut cdp).await {
                Ok(()) => sessions.push(CodexSession {
                    cdp,
                    port: self.port,
                    state: self.state.clone(),
                    payload: Arc::clone(&self.payload),
                    manager_token: manager_token.to_owned(),
                    owned_script_identifiers: Vec::new(),
                }),
                Err(error) => {
                    bridge_missing |= error.code() == CodexRefitErrorCode::BridgeUnavailable;
                    let error = if launched_pid.is_some()
                        && error.code() == CodexRefitErrorCode::TargetValidationFailed
                    {
                        CodexRefitError::new(
                            CodexRefitErrorCode::TargetNotFound,
                            "the launched renderer is not ready for validation",
                        )
                    } else {
                        error
                    };
                    if error.code() != CodexRefitErrorCode::BridgeUnavailable
                        && session_error.is_none()
                    {
                        session_error = Some(error);
                    }
                    cdp.close().await;
                }
            }
        }
        if sessions.is_empty() {
            if bridge_missing {
                return Err(CodexRefitError::new(
                    CodexRefitErrorCode::BridgeUnavailable,
                    "the validated renderer does not expose the expected local account bridge",
                ));
            }
            if let Some(error) = session_error {
                return Err(error);
            }
            return Err(CodexRefitError::new(
                CodexRefitErrorCode::TargetValidationFailed,
                "no renderer matched the expected app shell and sidebar",
            ));
        }
        let port = self.port;
        let revalidation_identity = identity.clone();
        if let Err(error) =
            run_blocking(move || platform::revalidate_runtime(port, &revalidation_identity)).await
        {
            close_sessions(&mut sessions).await;
            return Err(error);
        }
        if let Some(launched_pid) = launched_pid
            && let Err(error) =
                run_blocking(move || platform::validate_launched_runtime(&identity, launched_pid))
                    .await
        {
            close_sessions(&mut sessions).await;
            return Err(error);
        }
        Ok(sessions)
    }

    async fn connect_and_enable(
        &self,
        mode: SessionMode,
        manager_token: &str,
        launched_pid: Option<u32>,
    ) -> Result<(Vec<CodexSession>, CodexRefitReport), CodexRefitError> {
        let mut sessions = self.connect_sessions(manager_token, launched_pid).await?;
        if let Err(error) = self.prune_absent_sessions(&sessions) {
            close_sessions(&mut sessions).await;
            return Err(error);
        }
        match lifecycle::enable(&mut sessions, mode).await {
            Ok(report) => Ok((sessions, self.report_with_port(report))),
            Err(error) => {
                close_sessions(&mut sessions).await;
                Err(error)
            }
        }
    }

    async fn connect_and_enable_with_policy(
        &self,
        mode: SessionMode,
        launch_policy: LaunchPolicy,
        manager_token: &str,
    ) -> Result<(Vec<CodexSession>, CodexRefitReport), CodexRefitError> {
        match self.connect_and_enable(mode, manager_token, None).await {
            Ok((sessions, mut report)) => {
                report.launch_policy = Some(launch_policy);
                report.launched = Some(false);
                return Ok((sessions, report));
            }
            Err(error) if launch_policy == LaunchPolicy::AttachOnly => return Err(error),
            Err(initial_error) => {
                let port = self.port;
                if run_blocking(move || platform::validate_runtime(port))
                    .await
                    .is_ok()
                {
                    return Err(initial_error);
                }
            }
        }

        let port = self.port;
        let backend = Arc::clone(&self.launch_backend);
        let launched_pid =
            run_blocking(move || launch::launch_if_stopped(backend.as_ref(), port)).await?;
        let (sessions, mut report) =
            launch::wait_for_endpoint(self.port, APP_LAUNCH_TIMEOUT, || {
                self.connect_and_enable(mode, manager_token, Some(launched_pid))
            })
            .await?;
        report.launch_policy = Some(launch_policy);
        report.launched = Some(true);
        Ok((sessions, report))
    }

    async fn existing_persistent_session(
        &self,
        launch_policy: LaunchPolicy,
    ) -> Result<CodexUsageSession, CodexRefitError> {
        let mut sessions = self.connect_sessions(&new_manager_token(), None).await?;
        let result = async {
            self.prune_absent_sessions(&sessions)?;
            let mut report = lifecycle::status(&mut sessions).await?;
            let usable = report.targets.iter().all(|target| {
                target.healthy
                    && target.session_mode == Some(SessionMode::Persistent)
                    && matches!(
                        target.state,
                        CodexRefitState::Enabled | CodexRefitState::Stale
                    )
            });
            if !usable {
                return Err(CodexRefitError::new(
                    CodexRefitErrorCode::Stale,
                    "an existing persistent manager did not expose a healthy managed runtime",
                ));
            }
            report.operation = CodexRefitOperation::Enable;
            report.port = self.port;
            report.session_mode = Some(SessionMode::Persistent);
            report.launch_policy = Some(launch_policy);
            report.launched = Some(false);
            Ok(CodexUsageSession {
                mode: SessionMode::Persistent,
                report,
                supervisor: None,
            })
        }
        .await;
        close_sessions(&mut sessions).await;
        result
    }

    fn prune_absent_sessions(&self, sessions: &[CodexSession]) -> Result<(), CodexRefitError> {
        let target_ids = sessions
            .iter()
            .map(|session| session.cdp.target_id().to_owned())
            .collect::<Vec<_>>();
        self.state.remove_absent_targets(self.port, &target_ids)
    }

    fn report_with_port(&self, mut report: CodexRefitReport) -> CodexRefitReport {
        report.port = self.port;
        report
    }

    async fn stop_managed_session(&self) -> Result<(), CodexRefitError> {
        let pid = self.state.managed_process_id(self.port)?.ok_or_else(|| {
            CodexRefitError::new(
                CodexRefitErrorCode::CleanupFailed,
                "the active persistent manager has no validated owner marker",
            )
        })?;
        run_blocking(move || platform::stop_managed_process(pid)).await?;
        let deadline = tokio::time::Instant::now() + MANAGER_STOP_TIMEOUT;
        while self.state.managed_session_active()? {
            if tokio::time::Instant::now() >= deadline {
                return Err(CodexRefitError::new(
                    CodexRefitErrorCode::CleanupFailed,
                    "the persistent Opsail manager did not stop before cleanup",
                ));
            }
            sleep(Duration::from_millis(25)).await;
        }
        Ok(())
    }
}

impl PersistentSupervisor {
    async fn run(mut self) -> Result<(), CodexRefitError> {
        let mut backoff = ReconnectBackoff::default();
        loop {
            let mut terminations = FuturesUnordered::new();
            for session in &self.sessions {
                terminations.push(wait_for_termination(session.cdp.termination_receiver()));
            }
            let termination = terminations
                .next()
                .await
                .expect("validated persistent sessions cannot be empty");
            tracing::info!(
                target: "opsail_refit_codex",
                ?termination,
                "[opsail-refit-codex] managed renderer connection ended"
            );
            close_sessions(&mut self.sessions).await;

            loop {
                let delay = backoff.next_delay();
                sleep(delay).await;
                let attempt = async {
                    let _operation_lock = self.adapter.state.try_operation_lock()?;
                    self.adapter
                        .connect_and_enable(SessionMode::Persistent, &self.manager_token, None)
                        .await
                }
                .await;
                match attempt {
                    Ok((sessions, _)) => {
                        self.sessions = sessions;
                        backoff.reset();
                        break;
                    }
                    Err(error) => tracing::warn!(
                        target: "opsail_refit_codex",
                        code = error.code().as_str(),
                        retry_delay_ms = backoff.next.as_millis(),
                        "[opsail-refit-codex] managed renderer reconnect failed"
                    ),
                }
            }
        }
    }
}

fn new_manager_token() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "opsail-refit-codex:{}:{timestamp}:{sequence}",
        std::process::id()
    )
}

struct CodexSession {
    cdp: CdpSession,
    port: u16,
    state: StateStore,
    payload: Arc<UsagePayload>,
    manager_token: String,
    owned_script_identifiers: Vec<String>,
}

impl ManagedSession for CodexSession {
    fn target_id(&self) -> &str {
        self.cdp.target_id()
    }

    fn health(&mut self) -> SessionFuture<'_, Result<CodexTargetHealth, CodexRefitError>> {
        Box::pin(async move {
            let runtime = renderer_status(&mut self.cdp).await?;
            let target_id = self.cdp.target_id().to_owned();
            let record =
                self.state
                    .current_record(self.port, &target_id, &self.payload.revision)?;
            let diagnostics = runtime.diagnostics.as_ref();
            let detected_mode = diagnostics
                .map(|value| value.session_mode)
                .or_else(|| record.as_ref().map(|value| value.session_mode));
            let lifecycle_healthy = match diagnostics.map(|value| value.session_mode) {
                Some(SessionMode::Once) => record.is_none(),
                Some(SessionMode::Persistent) => {
                    self.state.managed_session_active()?
                        && record.as_ref().is_some_and(|record| {
                            record.session_mode == SessionMode::Persistent
                                && diagnostics.is_some_and(|diagnostics| {
                                    record.manager_token == diagnostics.manager_token
                                })
                        })
                }
                None => false,
            };
            let diagnostics_healthy = diagnostics.is_some_and(|diagnostics| {
                diagnostics.installed
                    && diagnostics.mode == "usage"
                    && diagnostics.revision == self.payload.revision
                    && diagnostics.host_count <= 1
                    && diagnostics.style_count == 1
                    && diagnostics.details_count == 1
                    && diagnostics.listener_count == USAGE_RUNTIME_LISTENER_COUNT
                    && diagnostics.mutation_observer
                    && diagnostics.resize_observer
                    && diagnostics.refresh_timer
                    && diagnostics.bridge_available
            });
            if runtime.installed
                && runtime.revision.as_deref() == Some(&self.payload.revision)
                && lifecycle_healthy
                && diagnostics_healthy
            {
                let data_state = runtime
                    .diagnostics
                    .as_ref()
                    .map(|diagnostics| diagnostics.data_state.as_str());
                let stale = matches!(data_state, Some("stale" | "unavailable"));
                let mut health = CodexTargetHealth::new(
                    target_id,
                    if stale {
                        CodexRefitState::Stale
                    } else {
                        CodexRefitState::Enabled
                    },
                    true,
                );
                if let Some(mode) = detected_mode {
                    health = health.with_session_mode(mode);
                }
                return Ok(if stale {
                    health.with_detail(
                        "the usage UI is installed but its local account data is unavailable or stale",
                    )
                } else {
                    health
                });
            }
            if !runtime.installed
                && record.is_none()
                && runtime.host_count == 0
                && runtime.style_count == 0
                && runtime.details_count == 0
            {
                return Ok(CodexTargetHealth::new(
                    target_id,
                    CodexRefitState::Disabled,
                    true,
                ));
            }
            let mut health = CodexTargetHealth::new(target_id, CodexRefitState::Stale, false)
                .with_detail("renderer state and its session lifecycle require reconciliation");
            if let Some(mode) = detected_mode {
                health = health.with_session_mode(mode);
            }
            Ok(health)
        })
    }

    fn enable(&mut self, mode: SessionMode) -> SessionFuture<'_, Result<(), CodexRefitError>> {
        Box::pin(async move {
            let target_id = self.cdp.target_id().to_owned();
            for identifier in std::mem::take(&mut self.owned_script_identifiers) {
                self.cdp.remove_script(&identifier).await?;
            }
            match mode {
                SessionMode::Once => self.state.remove(self.port, &target_id)?,
                SessionMode::Persistent => {
                    let identifier = self
                        .cdp
                        .add_script(&self.payload.early(&self.manager_token))
                        .await?;
                    self.owned_script_identifiers.push(identifier.clone());
                    let record = TargetRecord {
                        port: self.port,
                        target_id: target_id.clone(),
                        revision: self.payload.revision.clone(),
                        session_mode: SessionMode::Persistent,
                        manager_token: self.manager_token.clone(),
                        manager_pid: std::process::id(),
                    };
                    if let Err(error) = self.state.replace(record) {
                        let _ = self.cdp.remove_script(&identifier).await;
                        self.owned_script_identifiers.clear();
                        let _ = self.cdp.evaluate(disable_expression()).await;
                        return Err(error);
                    }
                }
            }
            if let Err(error) = self
                .cdp
                .evaluate(&self.payload.current(mode, &self.manager_token))
                .await
            {
                for identifier in std::mem::take(&mut self.owned_script_identifiers) {
                    let _ = self.cdp.remove_script(&identifier).await;
                }
                let _ = self.state.remove(self.port, &target_id);
                let _ = self.cdp.evaluate(disable_expression()).await;
                return Err(CodexRefitError::new(
                    CodexRefitErrorCode::InjectionFailed,
                    error.to_string(),
                ));
            }
            Ok(())
        })
    }

    fn disable(&mut self) -> SessionFuture<'_, Result<(), CodexRefitError>> {
        Box::pin(async move {
            let target_id = self.cdp.target_id().to_owned();
            let mut script_error = None;
            for identifier in std::mem::take(&mut self.owned_script_identifiers) {
                if let Err(error) = self.cdp.remove_script(&identifier).await
                    && script_error.is_none()
                {
                    script_error = Some(error);
                }
            }
            let state_error = self.state.remove(self.port, &target_id).err();
            let cleanup_value = self
                .cdp
                .evaluate(disable_expression())
                .await
                .map_err(|error| {
                    if error.code() == CodexRefitErrorCode::InjectionFailed {
                        CodexRefitError::new(CodexRefitErrorCode::CleanupFailed, error.to_string())
                    } else {
                        error
                    }
                })?;
            let cleanup: CleanupResult = serde_json::from_value(cleanup_value).map_err(|_| {
                CodexRefitError::new(
                    CodexRefitErrorCode::CleanupFailed,
                    "the renderer returned an invalid cleanup result",
                )
            })?;
            if !cleanup.clean {
                return Err(CodexRefitError::new(
                    CodexRefitErrorCode::CleanupFailed,
                    "the renderer still contains refit artifacts after cleanup",
                ));
            }
            if let Some(error) = script_error {
                return Err(error);
            }
            if let Some(error) = state_error {
                return Err(error);
            }
            Ok(())
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RendererProbe {
    app_protocol: bool,
    shell: bool,
    sidebar: bool,
    bridge: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RendererStatus {
    installed: bool,
    revision: Option<String>,
    diagnostics: Option<RendererDiagnostics>,
    host_count: usize,
    style_count: usize,
    details_count: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RendererDiagnostics {
    installed: bool,
    mode: String,
    session_mode: SessionMode,
    manager_token: String,
    revision: String,
    host_count: usize,
    style_count: usize,
    details_count: usize,
    listener_count: usize,
    mutation_observer: bool,
    resize_observer: bool,
    refresh_timer: bool,
    bridge_available: bool,
    #[serde(default)]
    data_state: String,
}

#[derive(Deserialize)]
struct CleanupResult {
    clean: bool,
}

async fn probe_renderer(session: &mut CdpSession) -> Result<(), CodexRefitError> {
    let probe: RendererProbe = serde_json::from_value(
        session.evaluate(renderer_probe_expression()).await?,
    )
    .map_err(|_| {
        CodexRefitError::new(
            CodexRefitErrorCode::TargetValidationFailed,
            "the renderer returned an invalid identity probe",
        )
    })?;
    if !probe.app_protocol || !probe.shell || !probe.sidebar {
        return Err(CodexRefitError::new(
            CodexRefitErrorCode::TargetValidationFailed,
            "the renderer does not match the expected app shell and sidebar",
        ));
    }
    if !probe.bridge {
        return Err(CodexRefitError::new(
            CodexRefitErrorCode::BridgeUnavailable,
            "the renderer does not expose the expected local account bridge",
        ));
    }
    Ok(())
}

async fn renderer_status(session: &mut CdpSession) -> Result<RendererStatus, CodexRefitError> {
    serde_json::from_value(session.evaluate(&status_expression()).await?).map_err(|_| {
        CodexRefitError::new(
            CodexRefitErrorCode::Stale,
            "the renderer returned an invalid refit health result",
        )
    })
}

async fn close_sessions(sessions: &mut [CodexSession]) {
    for session in sessions {
        session.cdp.close().await;
    }
}

async fn run_blocking<T, F>(operation: F) -> Result<T, CodexRefitError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, CodexRefitError> + Send + 'static,
{
    tokio::task::spawn_blocking(operation).await.map_err(|_| {
        CodexRefitError::new(
            CodexRefitErrorCode::TargetValidationFailed,
            "a platform identity check did not complete",
        )
    })?
}

fn failed_check(name: &'static str, error: &CodexRefitError) -> DoctorCheck {
    DoctorCheck {
        name,
        state: DoctorCheckState::Fail,
        message: format!("{}: {error}", error.code().as_str()),
    }
}

fn doctor_report(
    port: u16,
    checks: Vec<DoctorCheck>,
    detected_session_modes: Vec<SessionMode>,
) -> CodexDoctorReport {
    let ready = checks
        .iter()
        .all(|check| check.state == DoctorCheckState::Pass);
    CodexDoctorReport {
        supported: platform::is_supported(),
        ready,
        port,
        default_session_mode: SessionMode::Persistent,
        detected_session_modes,
        checks,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use futures_util::{SinkExt as _, StreamExt as _};
    use serde_json::Value;
    use tempfile::tempdir;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio_tungstenite::accept_async;

    use super::*;

    async fn test_cdp_session(
        evaluation_value: Value,
    ) -> (CdpSession, oneshot::Receiver<(Vec<String>, bool)>) {
        test_cdp_session_sequence(vec![evaluation_value]).await
    }

    async fn test_cdp_session_sequence(
        evaluation_values: Vec<Value>,
    ) -> (CdpSession, oneshot::Receiver<(Vec<String>, bool)>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let target = cdp::RendererTarget {
            id: "renderer-test".to_owned(),
            websocket_url: url::Url::parse(&format!(
                "ws://127.0.0.1:{port}/devtools/page/renderer-test"
            ))
            .unwrap(),
        };
        let (observed_tx, observed_rx) = oneshot::channel();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let mut methods = Vec::new();
            let mut evaluation_values = VecDeque::from(evaluation_values);
            let mut closed = false;
            while let Some(message) = socket.next().await {
                match message.unwrap() {
                    tokio_tungstenite::tungstenite::Message::Text(text) => {
                        let request: Value = serde_json::from_str(text.as_ref()).unwrap();
                        methods.push(request["method"].as_str().unwrap().to_owned());
                        let evaluation_value = evaluation_values.pop_front().unwrap_or(Value::Null);
                        socket
                            .send(tokio_tungstenite::tungstenite::Message::Text(
                                serde_json::json!({
                                    "id": request["id"],
                                    "result": { "result": { "value": evaluation_value } }
                                })
                                .to_string()
                                .into(),
                            ))
                            .await
                            .unwrap();
                    }
                    tokio_tungstenite::tungstenite::Message::Close(_) => {
                        closed = true;
                        break;
                    }
                    _ => {}
                }
            }
            let _ = observed_tx.send((methods, closed));
        });
        (CdpSession::connect(&target).await.unwrap(), observed_rx)
    }

    fn healthy_renderer_status(
        payload: &UsagePayload,
        mode: SessionMode,
        manager_token: &str,
    ) -> Value {
        serde_json::json!({
            "installed": true,
            "revision": payload.revision,
            "diagnostics": {
                "installed": true,
                "mode": "usage",
                "sessionMode": mode,
                "managerToken": manager_token,
                "revision": payload.revision,
                "hostCount": 1,
                "styleCount": 1,
                "detailsCount": 1,
                "listenerCount": USAGE_RUNTIME_LISTENER_COUNT,
                "mutationObserver": true,
                "resizeObserver": true,
                "refreshTimer": true,
                "bridgeAvailable": true,
                "dataState": "ready"
            },
            "hostCount": 1,
            "styleCount": 1,
            "detailsCount": 1
        })
    }

    fn disabled_renderer_status() -> Value {
        serde_json::json!({
            "installed": false,
            "revision": null,
            "diagnostics": null,
            "hostCount": 0,
            "styleCount": 0,
            "detailsCount": 0
        })
    }

    #[test]
    fn rejects_privileged_debug_ports() {
        let error = match CodexRefit::new(
            CodexRefitConfig::new(80).with_state_dir(PathBuf::from("unused")),
        ) {
            Ok(_) => panic!("privileged debug port was accepted"),
            Err(error) => error,
        };
        assert_eq!(error.code(), CodexRefitErrorCode::TargetValidationFailed);
    }

    #[test]
    fn public_default_debug_port_is_the_private_high_port() {
        assert_eq!(DEFAULT_CODEX_DEBUG_PORT, 55321);
    }

    #[test]
    fn renderer_probe_contains_no_remote_or_model_channel() {
        let expression = renderer_probe_expression();
        assert!(expression.contains("location.protocol"));
        assert!(!expression.contains("fetch"));
        assert!(!expression.contains("account/rateLimits"));
    }

    #[test]
    fn report_serialization_uses_stable_state_names() {
        let report = CodexRefitReport {
            operation: CodexRefitOperation::Status,
            port: DEFAULT_CODEX_DEBUG_PORT,
            session_mode: Some(SessionMode::Once),
            launch_policy: Some(LaunchPolicy::LaunchIfStopped),
            launched: Some(true),
            targets: vec![
                CodexTargetHealth::new("renderer", CodexRefitState::Stale, false)
                    .with_session_mode(SessionMode::Once),
            ],
        };
        let value: Value = serde_json::to_value(report).unwrap();
        assert_eq!(value["operation"], "status");
        assert_eq!(value["port"], DEFAULT_CODEX_DEBUG_PORT);
        assert_eq!(value["sessionMode"], "once");
        assert_eq!(value["launchPolicy"], "launch-if-stopped");
        assert_eq!(value["launched"], true);
        assert_eq!(value["targets"][0]["state"], "stale");
        assert_eq!(value["targets"][0]["sessionMode"], "once");
    }

    #[tokio::test]
    async fn once_enable_evaluates_current_document_closes_and_writes_no_receipt() {
        let directory = tempdir().unwrap();
        let state = StateStore::new(directory.path().to_owned());
        let payload = Arc::new(usage_payload().unwrap());
        let token = "opsail-refit-codex:test-once";
        let (cdp, observed) = test_cdp_session_sequence(vec![
            disabled_renderer_status(),
            serde_json::json!({}),
            healthy_renderer_status(&payload, SessionMode::Once, token),
        ])
        .await;
        let session = CodexSession {
            cdp,
            port: DEFAULT_CODEX_DEBUG_PORT,
            state: state.clone(),
            payload,
            manager_token: token.to_owned(),
            owned_script_identifiers: Vec::new(),
        };
        let mut sessions = [session];

        let report = lifecycle::enable(&mut sessions, SessionMode::Once)
            .await
            .unwrap();
        assert_eq!(report.session_mode, Some(SessionMode::Once));
        sessions[0].cdp.close().await;
        let (methods, closed) = observed.await.unwrap();
        assert_eq!(
            methods,
            ["Runtime.evaluate", "Runtime.evaluate", "Runtime.evaluate"]
        );
        assert!(
            !methods
                .iter()
                .any(|method| method == "Page.addScriptToEvaluateOnNewDocument")
        );
        assert!(closed);
        assert!(
            state
                .records_for(DEFAULT_CODEX_DEBUG_PORT, "renderer-test")
                .unwrap()
                .is_empty()
        );
        assert!(!directory.path().join("state.json").exists());
    }

    #[tokio::test]
    async fn disable_after_once_never_removes_a_prior_session_script() {
        let directory = tempdir().unwrap();
        let state = StateStore::new(directory.path().to_owned());
        let (cdp, observed) = test_cdp_session(serde_json::json!({ "clean": true })).await;
        let mut session = CodexSession {
            cdp,
            port: DEFAULT_CODEX_DEBUG_PORT,
            state,
            payload: Arc::new(usage_payload().unwrap()),
            manager_token: "opsail-refit-codex:test-disable".to_owned(),
            owned_script_identifiers: Vec::new(),
        };

        session.disable().await.unwrap();
        session.cdp.close().await;
        let (methods, closed) = observed.await.unwrap();
        assert_eq!(methods, ["Runtime.evaluate"]);
        assert!(closed);
        assert!(
            !methods
                .iter()
                .any(|method| { method == "Page.removeScriptToEvaluateOnNewDocument" })
        );
    }

    #[test]
    fn reconnect_backoff_is_bounded_and_resets_after_success() {
        let mut backoff = ReconnectBackoff::default();
        assert_eq!(backoff.next_delay(), Duration::from_millis(250));
        for _ in 0..16 {
            assert!(backoff.next_delay() <= Duration::from_secs(30));
        }
        assert_eq!(backoff.next_delay(), Duration::from_secs(30));
        backoff.reset();
        assert_eq!(backoff.next_delay(), Duration::from_millis(250));
    }

    #[test]
    fn doctor_names_default_and_detected_session_lifecycles() {
        let report = doctor_report(
            DEFAULT_CODEX_DEBUG_PORT,
            vec![DoctorCheck {
                name: "session-mode",
                state: DoctorCheckState::Pass,
                message: "once (ephemeral) mode is installed".to_owned(),
            }],
            vec![SessionMode::Once],
        );
        assert_eq!(report.default_session_mode, SessionMode::Persistent);
        assert_eq!(report.detected_session_modes, [SessionMode::Once]);
        assert!(report.checks[0].message.contains("ephemeral"));
    }

    #[tokio::test]
    async fn status_distinguishes_once_managed_and_disconnected_managed_sessions() {
        let directory = tempdir().unwrap();
        let state = StateStore::new(directory.path().to_owned());
        let payload = Arc::new(usage_payload().unwrap());

        let once_token = "opsail-refit-codex:once-health";
        let (cdp, _) = test_cdp_session(healthy_renderer_status(
            &payload,
            SessionMode::Once,
            once_token,
        ))
        .await;
        let mut once = CodexSession {
            cdp,
            port: DEFAULT_CODEX_DEBUG_PORT,
            state: state.clone(),
            payload: Arc::clone(&payload),
            manager_token: once_token.to_owned(),
            owned_script_identifiers: Vec::new(),
        };
        let once_health = once.health().await.unwrap();
        assert!(once_health.healthy);
        assert_eq!(once_health.session_mode, Some(SessionMode::Once));
        once.cdp.close().await;

        let (cdp, _) = test_cdp_session(disabled_renderer_status()).await;
        let mut reloaded_once = CodexSession {
            cdp,
            port: DEFAULT_CODEX_DEBUG_PORT,
            state: state.clone(),
            payload: Arc::clone(&payload),
            manager_token: once_token.to_owned(),
            owned_script_identifiers: Vec::new(),
        };
        let reloaded_health = reloaded_once.health().await.unwrap();
        assert!(reloaded_health.healthy);
        assert_eq!(reloaded_health.state, CodexRefitState::Disabled);
        assert_eq!(reloaded_health.session_mode, None);
        reloaded_once.cdp.close().await;

        let persistent_token = "opsail-refit-codex:persistent-health";
        state
            .replace(TargetRecord {
                port: DEFAULT_CODEX_DEBUG_PORT,
                target_id: "renderer-test".to_owned(),
                revision: payload.revision.clone(),
                session_mode: SessionMode::Persistent,
                manager_token: persistent_token.to_owned(),
                manager_pid: std::process::id(),
            })
            .unwrap();
        let managed_lock = state.try_managed_session_lock().unwrap().unwrap();
        let (cdp, _) = test_cdp_session(healthy_renderer_status(
            &payload,
            SessionMode::Persistent,
            persistent_token,
        ))
        .await;
        let mut persistent = CodexSession {
            cdp,
            port: DEFAULT_CODEX_DEBUG_PORT,
            state: state.clone(),
            payload: Arc::clone(&payload),
            manager_token: persistent_token.to_owned(),
            owned_script_identifiers: Vec::new(),
        };
        let managed_health = persistent.health().await.unwrap();
        assert!(managed_health.healthy);
        assert_eq!(managed_health.session_mode, Some(SessionMode::Persistent));
        persistent.cdp.close().await;

        drop(managed_lock);
        let (cdp, _) = test_cdp_session(healthy_renderer_status(
            &payload,
            SessionMode::Persistent,
            persistent_token,
        ))
        .await;
        let mut disconnected = CodexSession {
            cdp,
            port: DEFAULT_CODEX_DEBUG_PORT,
            state,
            payload,
            manager_token: persistent_token.to_owned(),
            owned_script_identifiers: Vec::new(),
        };
        let disconnected_health = disconnected.health().await.unwrap();
        assert!(!disconnected_health.healthy);
        assert_eq!(disconnected_health.state, CodexRefitState::Stale);
        assert_eq!(
            disconnected_health.session_mode,
            Some(SessionMode::Persistent)
        );
        disconnected.cdp.close().await;
    }

    #[test]
    fn guide_documents_once_reload_and_restart_tradeoffs() {
        const GUIDE: &str = include_str!("../README.md");
        assert!(GUIDE.contains("opsail refit codex enable usage --launch"));
        assert!(GUIDE.contains("opsail refit codex enable usage --launch --once"));
        assert!(GUIDE.contains("55321"));
        assert!(GUIDE.contains("does not automatically choose another port"));
        assert!(GUIDE.contains("opsail refit codex enable usage --once"));
        assert!(GUIDE.contains("does not survive a hard reload"));
        assert!(GUIDE.contains("renderer reconstruction"));
        assert!(GUIDE.contains("application restart"));
        assert!(GUIDE.contains("`persistent` (managed)"));
        assert!(GUIDE.contains("`once` (ephemeral)"));
    }
}
