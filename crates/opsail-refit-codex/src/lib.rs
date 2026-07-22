//! Safe Codex renderer refits for Opsail.
//!
//! The adapter is intentionally narrow: it connects only to a `127.0.0.1` CDP
//! endpoint owned by the signed macOS ChatGPT process tree. Enable is
//! attach-only unless the caller explicitly selects [`LaunchPolicy::LaunchIfStopped`].
//! Opsail never quits, restarts, modifies, or signs the application.

mod cdp;
mod error;
mod github_update;
mod launch;
mod lifecycle;
mod model;
mod payload;
mod platform;
mod renderer_assets;
mod renderer_session;
mod state;
mod supervisor;

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::sleep;

use cdp::{CdpSession, discover_targets};
pub use error::{CodexRefitError, CodexRefitErrorCode};
use github_update::{GithubRendererAssetClient, RendererAssetUpdateClient};
use launch::{LaunchBackend, SystemLaunchBackend};
#[cfg(test)]
use lifecycle::ManagedSession;
pub use model::{
    CodexDoctorReport, CodexRefitOperation, CodexRefitReport, CodexRefitStage, CodexRefitState,
    CodexTargetHealth, CodexUpdateReport, DoctorCheck, DoctorCheckState, LaunchPolicy,
    RendererAssetActivation, RendererAssetInfo, RendererAssetSource, RendererAssetUpdatePolicy,
    ResetCreditState, SessionMode,
};
#[cfg(test)]
use payload::usage_payload;
use payload::{UsagePayload, build_usage_payload};
use platform::{LaunchedProcess, ValidatedAppIdentity};
use renderer_assets::{RendererAssetInstallPolicy, RendererAssetStore, embedded_bundle};
use renderer_session::{CodexSession, close_sessions, probe_renderer, renderer_status};
#[cfg(test)]
use renderer_session::{
    DOM_ADAPTER_API_VERSION, RendererStatus, USAGE_RUNTIME_LISTENER_COUNT, show_launch_notice,
};
use state::StateStore;
#[cfg(test)]
use state::TargetRecord;
use supervisor::{PersistentSupervisor, new_manager_token};
#[cfg(test)]
use supervisor::{ReconnectBackoff, RecoveryDecision, wait_for_reconnect_or_app_exit};

pub const DEFAULT_CODEX_DEBUG_PORT: u16 = 55321;
const APP_LAUNCH_TIMEOUT: Duration = Duration::from_secs(15);
const MANAGER_STOP_TIMEOUT: Duration = Duration::from_secs(3);

type ProgressHandler = dyn Fn(CodexRefitStage) + Send + Sync + 'static;

#[derive(Clone, Default)]
struct ProgressReporter(Option<Arc<ProgressHandler>>);

impl ProgressReporter {
    fn report(&self, stage: CodexRefitStage) {
        if let Some(handler) = &self.0 {
            handler(stage);
        }
    }
}

impl fmt::Debug for ProgressReporter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ProgressReporter")
            .field(&self.0.as_ref().map(|_| "configured"))
            .finish()
    }
}

/// Configuration for the verified Codex renderer adapter.
#[derive(Debug, Clone)]
pub struct CodexRefitConfig {
    port: u16,
    state_dir: Option<PathBuf>,
    progress: ProgressReporter,
}

impl Default for CodexRefitConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_CODEX_DEBUG_PORT,
            state_dir: None,
            progress: ProgressReporter::default(),
        }
    }
}

impl CodexRefitConfig {
    pub fn new(port: u16) -> Self {
        Self {
            port,
            state_dir: None,
            progress: ProgressReporter::default(),
        }
    }

    /// Override the state directory for an embedding application or test.
    pub fn with_state_dir(mut self, state_dir: PathBuf) -> Self {
        self.state_dir = Some(state_dir);
        self
    }

    /// Receive infrequent lifecycle milestones. Handlers should return promptly.
    pub fn with_progress_handler(
        mut self,
        handler: impl Fn(CodexRefitStage) + Send + Sync + 'static,
    ) -> Self {
        self.progress = ProgressReporter(Some(Arc::new(handler)));
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
    renderer_assets: RendererAssetStore,
    renderer_asset_warning: Option<String>,
    renderer_asset_content_identity: String,
    update_client: Arc<dyn RendererAssetUpdateClient>,
    launch_backend: Arc<dyn LaunchBackend>,
    progress: ProgressReporter,
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

impl CodexRefit {
    pub fn new(config: CodexRefitConfig) -> Result<Self, CodexRefitError> {
        if config.port < 1024 {
            return Err(CodexRefitError::new(
                CodexRefitErrorCode::TargetValidationFailed,
                "the Codex debug port must be between 1024 and 65535",
            ));
        }
        config.progress.report(CodexRefitStage::LoadRendererAssets);
        let state_dir = match config.state_dir {
            Some(path) => path,
            None => platform::default_state_dir()?,
        };
        let renderer_assets = RendererAssetStore::new(state_dir.clone());
        let selection = renderer_assets.load_or_embedded()?;
        let selected_identity = selection.bundle.content_identity();
        let selected_payload =
            build_usage_payload(selection.bundle.sources(), selection.info.clone());
        let (payload, renderer_asset_warning, renderer_asset_content_identity) =
            match selected_payload {
                Ok(payload) => (payload, selection.warning, selected_identity),
                Err(error) if selection.info.source == RendererAssetSource::Github => {
                    let embedded = embedded_bundle()?;
                    let embedded_info = embedded.info(RendererAssetSource::Embedded);
                    let identity = embedded.content_identity();
                    let payload = build_usage_payload(embedded.sources(), embedded_info)?;
                    (
                        payload,
                        Some(format!(
                            "installed renderer assets failed payload validation: {error}; using embedded renderer assets"
                        )),
                        identity,
                    )
                }
                Err(error) => return Err(error),
            };
        Ok(Self {
            port: config.port,
            state: StateStore::new(state_dir),
            payload: Arc::new(payload),
            renderer_assets,
            renderer_asset_warning,
            renderer_asset_content_identity,
            update_client: Arc::new(GithubRendererAssetClient),
            launch_backend: Arc::new(SystemLaunchBackend),
            progress: config.progress,
        })
    }

    /// Check the official GitHub repository and install a validated renderer bundle.
    ///
    /// This operation never discovers, connects to, launches, or stops ChatGPT. Changed
    /// JavaScript requires [`RendererAssetUpdatePolicy::Force`]; all integrity and
    /// compatibility checks remain mandatory in forced mode.
    pub async fn update_renderer_assets(
        &self,
        policy: RendererAssetUpdatePolicy,
    ) -> Result<CodexUpdateReport, CodexRefitError> {
        self.progress.report(CodexRefitStage::FetchUpdateManifest);
        let manifest = self.update_client.fetch_latest_manifest().await?;
        let candidate_info = manifest.info(RendererAssetSource::Github);
        let candidate_version = semver::Version::parse(&candidate_info.version)
            .expect("validated candidate asset version remains valid");
        let current_version = semver::Version::parse(&self.payload.asset_info.version)
            .expect("validated current asset version remains valid");
        if candidate_version < current_version {
            return Err(CodexRefitError::new(
                CodexRefitErrorCode::UpdateFailed,
                "renderer asset downgrade was rejected by version policy",
            ));
        }
        let content_changed = manifest.content_identity() != self.renderer_asset_content_identity;
        if content_changed && policy == RendererAssetUpdatePolicy::RequireUnchanged {
            return Err(CodexRefitError::new(
                CodexRefitErrorCode::UpdateFailed,
                "renderer JavaScript SHA-256 values changed; rerun `opsail refit codex update --force` (or `-f`) to install the validated bundle",
            ));
        }
        let files = manifest.file_count();
        let allow_content_change = policy == RendererAssetUpdatePolicy::Force;
        if candidate_version == current_version
            && !content_changed
            && self.renderer_asset_warning.is_none()
        {
            return Ok(CodexUpdateReport {
                operation: CodexRefitOperation::Update,
                previous: self.payload.asset_info.clone(),
                installed: self.payload.asset_info.clone(),
                changed: false,
                forced: allow_content_change,
                activation: RendererAssetActivation::Current,
                files,
            });
        }

        self.progress
            .report(CodexRefitStage::DownloadRendererAssets);
        let bundle = self.update_client.fetch_bundle(manifest).await?;
        build_usage_payload(bundle.sources(), candidate_info).map_err(|error| {
            CodexRefitError::new(
                CodexRefitErrorCode::UpdateFailed,
                format!("downloaded renderer assets failed payload validation: {error}"),
            )
        })?;
        let previous = self.payload.asset_info.clone();
        let store = self.renderer_assets.clone();
        let install_policy = if allow_content_change {
            RendererAssetInstallPolicy::AllowSameVersionChange
        } else {
            RendererAssetInstallPolicy::Strict
        };
        self.progress.report(CodexRefitStage::InstallRendererAssets);
        let install = tokio::task::spawn_blocking(move || store.install(&bundle, install_policy))
            .await
            .map_err(|_| {
                CodexRefitError::new(
                    CodexRefitErrorCode::UpdateFailed,
                    "renderer asset installation task did not complete",
                )
            })??;
        Ok(CodexUpdateReport {
            operation: CodexRefitOperation::Update,
            previous,
            installed: install.installed,
            changed: install.changed,
            forced: allow_content_change,
            activation: if install.changed {
                RendererAssetActivation::NextSession
            } else {
                RendererAssetActivation::Current
            },
            files,
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
                        "a persistent manager is active; stop it before using once mode",
                    ));
                }
                let manager_token = new_manager_token();
                let (mut sessions, result, _, _) = self
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
                let (sessions, report, app_identity, launched_process) = self
                    .connect_and_enable_with_policy(
                        SessionMode::Persistent,
                        launch_policy,
                        &manager_token,
                    )
                    .await?;
                self.progress.report(CodexRefitStage::StartManager);
                let mut supervisor_adapter = self.clone();
                supervisor_adapter.progress = ProgressReporter::default();
                Ok(CodexUsageSession {
                    mode,
                    report,
                    supervisor: Some(PersistentSupervisor::new(
                        supervisor_adapter,
                        sessions,
                        manager_token,
                        app_identity,
                        launched_process,
                        managed_lock,
                    )),
                })
            }
        }
    }

    pub async fn disable_usage(&self) -> Result<CodexRefitReport, CodexRefitError> {
        let _operation_lock = self.state.try_operation_lock()?;
        if self.state.managed_session_active()? {
            self.progress.report(CodexRefitStage::StopManager);
            self.stop_managed_session().await?;
        }
        let mut sessions = match self.connect_sessions(&new_manager_token(), None).await {
            Ok(sessions) => sessions,
            Err(error) => {
                let port = self.port;
                let listener_present =
                    run_blocking(move || platform::debug_listener_present(port)).await?;
                if can_cleanup_offline(error.code(), listener_present) {
                    self.progress.report(CodexRefitStage::CleanupUsage);
                    return self.offline_disable_report();
                }
                return Err(error);
            }
        };
        if let Err(error) = self.prune_absent_sessions(&sessions) {
            close_sessions(&mut sessions).await;
            return Err(error);
        }
        self.progress.report(CodexRefitStage::CleanupUsage);
        let result = lifecycle::disable(&mut sessions)
            .await
            .map(|report| self.report_with_port(report));
        close_sessions(&mut sessions).await;
        result
    }

    pub async fn status(&self) -> Result<CodexRefitReport, CodexRefitError> {
        let mut sessions = self.connect_sessions(&new_manager_token(), None).await?;
        self.progress.report(CodexRefitStage::InspectUsage);
        let result = lifecycle::status(&mut sessions)
            .await
            .map(|report| self.report_with_port(report));
        close_sessions(&mut sessions).await;
        result
    }

    /// Run read-only checks. This never injects, launches, stops, or reloads the app.
    pub async fn doctor(&self) -> CodexDoctorReport {
        self.progress.report(CodexRefitStage::RunDiagnostics);
        let mut checks = Vec::new();
        let mut detected_session_modes = Vec::new();
        checks.push(DoctorCheck {
            name: "renderer-assets",
            state: if self.renderer_asset_warning.is_some() {
                DoctorCheckState::Warning
            } else {
                DoctorCheckState::Pass
            },
            message: self.renderer_asset_warning.clone().unwrap_or_else(|| {
                format!(
                    "renderer JavaScript {} ({}) is valid",
                    self.payload.asset_info.version,
                    match self.payload.asset_info.source {
                        RendererAssetSource::Embedded => "embedded",
                        RendererAssetSource::Github => "github",
                    }
                )
            }),
        });
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
                renderer_assets: self.payload.asset_info.clone(),
                checks,
            };
        }
        checks.push(DoctorCheck {
            name: "platform",
            state: DoctorCheckState::Pass,
            message: "macOS is supported".to_owned(),
        });

        self.progress.report(CodexRefitStage::ValidateApplication);
        let app_identity = match run_blocking(platform::validate_app).await {
            Ok(identity) => identity,
            Err(error) => {
                checks.push(failed_check("application", &error));
                return doctor_report(
                    self.port,
                    self.payload.asset_info.clone(),
                    checks,
                    detected_session_modes,
                );
            }
        };
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

        self.progress.report(CodexRefitStage::ValidateListener);
        let port = self.port;
        let runtime_app = app_identity.clone();
        let identity =
            match run_blocking(move || platform::validate_runtime(port, &runtime_app)).await {
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
                    return doctor_report(
                        self.port,
                        self.payload.asset_info.clone(),
                        checks,
                        detected_session_modes,
                    );
                }
            };

        self.progress.report(CodexRefitStage::DiscoverRenderer);
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
                return doctor_report(
                    self.port,
                    self.payload.asset_info.clone(),
                    checks,
                    detected_session_modes,
                );
            }
        };

        self.progress.report(CodexRefitStage::ValidateRenderer);
        let mut valid_renderers = 0usize;
        let mut bridge_missing = false;
        for target in &targets {
            let Ok(mut session) = CdpSession::connect(target).await else {
                continue;
            };
            match probe_renderer(&mut session, &self.payload).await {
                Ok(()) => {
                    valid_renderers = valid_renderers.saturating_add(1);
                    if let Ok(status) = renderer_status(&mut session, &self.payload).await
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
            return doctor_report(
                self.port,
                self.payload.asset_info.clone(),
                checks,
                detected_session_modes,
            );
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
            Ok(active) if detected_session_modes.contains(&SessionMode::Persistent) && !active => (
                DoctorCheckState::Warning,
                "persistent (managed) renderer artifacts exist without an active manager"
                    .to_owned(),
            ),
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
        match run_blocking(move || platform::revalidate_runtime(port, &app_identity, &identity))
            .await
        {
            Ok(()) => checks.push(DoctorCheck {
                name: "identity-stability",
                state: DoctorCheckState::Pass,
                message: "the listener identity remained stable during validation".to_owned(),
            }),
            Err(error) => checks.push(failed_check("identity-stability", &error)),
        }
        doctor_report(
            self.port,
            self.payload.asset_info.clone(),
            checks,
            detected_session_modes,
        )
    }

    async fn connect_sessions(
        &self,
        manager_token: &str,
        launched_pid: Option<u32>,
    ) -> Result<Vec<CodexSession>, CodexRefitError> {
        self.progress.report(CodexRefitStage::ValidateApplication);
        let app = run_blocking(platform::validate_app).await?;
        self.connect_sessions_validated(manager_token, launched_pid, &app)
            .await
    }

    async fn connect_sessions_validated(
        &self,
        manager_token: &str,
        launched_pid: Option<u32>,
        app: &ValidatedAppIdentity,
    ) -> Result<Vec<CodexSession>, CodexRefitError> {
        self.progress.report(CodexRefitStage::ValidateListener);
        let port = self.port;
        let runtime_app = app.clone();
        let identity = run_blocking(move || platform::validate_runtime(port, &runtime_app)).await?;
        self.progress.report(CodexRefitStage::DiscoverRenderer);
        let targets = discover_targets(self.port).await?;
        self.progress.report(CodexRefitStage::ValidateRenderer);
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
            match probe_renderer(&mut cdp, &self.payload).await {
                Ok(()) => sessions.push(CodexSession::new(
                    cdp,
                    self.port,
                    self.state.clone(),
                    Arc::clone(&self.payload),
                    manager_token,
                )),
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
        let revalidation_app = app.clone();
        if let Err(error) = run_blocking(move || {
            platform::revalidate_runtime(port, &revalidation_app, &revalidation_identity)
        })
        .await
        {
            close_sessions(&mut sessions).await;
            return Err(error);
        }
        if let Some(launched_pid) = launched_pid
            && let Err(error) = {
                let launched_app = app.clone();
                run_blocking(move || {
                    platform::validate_launched_runtime(&identity, &launched_app, launched_pid)
                })
                .await
            }
        {
            close_sessions(&mut sessions).await;
            return Err(error);
        }
        Ok(sessions)
    }

    async fn connect_and_enable_validated(
        &self,
        mode: SessionMode,
        manager_token: &str,
        launched_pid: Option<u32>,
        app: &ValidatedAppIdentity,
    ) -> Result<(Vec<CodexSession>, CodexRefitReport), CodexRefitError> {
        let mut sessions = self
            .connect_sessions_validated(manager_token, launched_pid, app)
            .await?;
        if let Err(error) = self.prune_absent_sessions(&sessions) {
            close_sessions(&mut sessions).await;
            return Err(error);
        }
        self.progress.report(CodexRefitStage::InspectUsage);
        match lifecycle::enable(&mut sessions, mode, &self.progress).await {
            Ok(report) => {
                if launched_pid.is_some()
                    && let Some(session) = sessions.first_mut()
                {
                    match session.show_launch_notice().await {
                        Ok(true) => {}
                        Ok(false) => tracing::warn!(
                            target: "opsail_refit_codex",
                            "[opsail-refit-codex] renderer declined the launch success notice"
                        ),
                        Err(error) => tracing::warn!(
                            target: "opsail_refit_codex",
                            code = error.code().as_str(),
                            "[opsail-refit-codex] launch succeeded but its renderer notice failed"
                        ),
                    }
                }
                Ok((sessions, self.report_with_port(report)))
            }
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
    ) -> Result<
        (
            Vec<CodexSession>,
            CodexRefitReport,
            ValidatedAppIdentity,
            Option<LaunchedProcess>,
        ),
        CodexRefitError,
    > {
        self.progress.report(CodexRefitStage::ValidateApplication);
        let app = run_blocking(platform::validate_app).await?;
        self.progress.report(CodexRefitStage::InspectEndpoint);
        match self
            .connect_and_enable_validated(mode, manager_token, None, &app)
            .await
        {
            Ok((sessions, mut report)) => {
                report.launch_policy = Some(launch_policy);
                report.launched = Some(false);
                return Ok((sessions, report, app, None));
            }
            Err(error) if launch_policy == LaunchPolicy::AttachOnly => return Err(error),
            Err(initial_error) => {
                let port = self.port;
                let runtime_app = app.clone();
                if run_blocking(move || platform::validate_runtime(port, &runtime_app))
                    .await
                    .is_ok()
                {
                    return Err(initial_error);
                }
            }
        }

        let port = self.port;
        let backend = Arc::clone(&self.launch_backend);
        let launch_app = app.clone();
        let launch_progress = self.progress.clone();
        let launched_process = run_blocking(move || {
            launch::launch_validated(backend.as_ref(), port, &launch_app, &launch_progress)
        })
        .await?;
        let launched_pid = launched_process.pid();
        self.progress.report(CodexRefitStage::WaitForEndpoint);
        let (sessions, mut report) = launch::wait_for_endpoint_or_process_exit(
            self.port,
            APP_LAUNCH_TIMEOUT,
            launched_process.exit_receiver(),
            || async {
                let result = self
                    .connect_and_enable_validated(mode, manager_token, Some(launched_pid), &app)
                    .await;
                if result.is_err() {
                    self.progress.report(CodexRefitStage::WaitForEndpoint);
                }
                result
            },
        )
        .await?;
        report.launch_policy = Some(launch_policy);
        report.launched = Some(true);
        Ok((sessions, report, app, Some(launched_process)))
    }

    async fn existing_persistent_session(
        &self,
        launch_policy: LaunchPolicy,
    ) -> Result<CodexUsageSession, CodexRefitError> {
        let mut sessions = self.connect_sessions(&new_manager_token(), None).await?;
        let result = async {
            self.prune_absent_sessions(&sessions)?;
            self.progress.report(CodexRefitStage::InspectUsage);
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
            report.renderer_assets = Some(self.payload.asset_info.clone());
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
            .map(|session| session.target_id().to_owned())
            .collect::<Vec<_>>();
        self.state.remove_absent_targets(self.port, &target_ids)
    }

    fn report_with_port(&self, mut report: CodexRefitReport) -> CodexRefitReport {
        report.port = self.port;
        report.renderer_assets = Some(self.payload.asset_info.clone());
        report
    }

    fn offline_disable_report(&self) -> Result<CodexRefitReport, CodexRefitError> {
        let removed = self.state.remove_port(self.port)?;
        let session_mode = removed
            .first()
            .map(|record| record.session_mode)
            .filter(|mode| removed.iter().all(|record| record.session_mode == *mode));
        let targets = removed
            .into_iter()
            .map(|record| {
                let mut health =
                    CodexTargetHealth::new(record.target_id, CodexRefitState::Disabled, true)
                        .with_session_mode(record.session_mode)
                        .with_detail(
                            "renderer unavailable; removed the stale local managed marker",
                        );
                health.changed = true;
                health
            })
            .collect();
        Ok(CodexRefitReport {
            operation: CodexRefitOperation::Disable,
            port: self.port,
            session_mode,
            launch_policy: None,
            launched: None,
            renderer_assets: Some(self.payload.asset_info.clone()),
            targets,
        })
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

fn can_cleanup_offline(code: CodexRefitErrorCode, listener_present: bool) -> bool {
    !listener_present
        && matches!(
            code,
            CodexRefitErrorCode::SessionUnavailable | CodexRefitErrorCode::TargetNotFound
        )
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
    renderer_assets: RendererAssetInfo,
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
        renderer_assets,
        checks,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    use futures_util::{SinkExt as _, StreamExt as _};
    use serde_json::Value;
    use tempfile::tempdir;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio_tungstenite::accept_async;

    use super::*;

    #[derive(Clone)]
    struct FakeUpdateClient {
        bundle: renderer_assets::RendererAssetBundle,
        bundle_fetches: Arc<AtomicUsize>,
    }

    impl RendererAssetUpdateClient for FakeUpdateClient {
        fn fetch_latest_manifest(&self) -> github_update::ManifestFuture<'_> {
            let manifest = self.bundle.manifest().clone();
            Box::pin(async move { Ok(manifest) })
        }

        fn fetch_bundle(
            &self,
            manifest: renderer_assets::RendererAssetManifest,
        ) -> github_update::BundleFuture<'_> {
            self.bundle_fetches.fetch_add(1, AtomicOrdering::Relaxed);
            let bundle = self.bundle.clone();
            Box::pin(async move {
                if manifest.content_identity() != bundle.manifest().content_identity() {
                    return Err(CodexRefitError::new(
                        CodexRefitErrorCode::UpdateFailed,
                        "fake update manifest changed",
                    ));
                }
                Ok(bundle)
            })
        }
    }

    fn use_fake_update(
        adapter: &mut CodexRefit,
        bundle: renderer_assets::RendererAssetBundle,
    ) -> Arc<AtomicUsize> {
        let bundle_fetches = Arc::new(AtomicUsize::new(0));
        adapter.update_client = Arc::new(FakeUpdateClient {
            bundle,
            bundle_fetches: Arc::clone(&bundle_fetches),
        });
        bundle_fetches
    }

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
                "domAdapterVersion": DOM_ADAPTER_API_VERSION,
                "dataState": "ready",
                "visible": true,
                "resetCreditState": "empty",
                "resetCreditCount": 0
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
    fn config_progress_handler_receives_bounded_lifecycle_stages() {
        let directory = tempdir().unwrap();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let progress_observed = Arc::clone(&observed);
        CodexRefit::new(
            CodexRefitConfig::default()
                .with_state_dir(directory.path().to_owned())
                .with_progress_handler(move |stage| {
                    progress_observed.lock().unwrap().push(stage);
                }),
        )
        .unwrap();
        assert_eq!(
            *observed.lock().unwrap(),
            [CodexRefitStage::LoadRendererAssets]
        );
    }

    #[test]
    fn offline_cleanup_requires_an_absent_listener_and_a_missing_endpoint() {
        assert!(can_cleanup_offline(
            CodexRefitErrorCode::SessionUnavailable,
            false,
        ));
        assert!(can_cleanup_offline(
            CodexRefitErrorCode::TargetNotFound,
            false,
        ));
        assert!(!can_cleanup_offline(
            CodexRefitErrorCode::TargetValidationFailed,
            false,
        ));
        assert!(!can_cleanup_offline(
            CodexRefitErrorCode::SessionUnavailable,
            true,
        ));
    }

    #[test]
    fn offline_disable_removes_only_local_markers_and_is_idempotent() {
        let directory = tempdir().unwrap();
        let adapter = CodexRefit::new(
            CodexRefitConfig::default().with_state_dir(directory.path().to_owned()),
        )
        .unwrap();
        adapter
            .state
            .replace(TargetRecord {
                port: DEFAULT_CODEX_DEBUG_PORT,
                target_id: "offline-renderer".to_owned(),
                revision: adapter.payload.revision.clone(),
                session_mode: SessionMode::Persistent,
                manager_token: "opsail-refit-codex:offline".to_owned(),
                manager_pid: 4242,
            })
            .unwrap();

        let first = adapter.offline_disable_report().unwrap();
        assert_eq!(first.operation, CodexRefitOperation::Disable);
        assert_eq!(first.targets.len(), 1);
        assert!(first.targets[0].healthy);
        assert!(first.targets[0].changed);
        assert_eq!(first.targets[0].state, CodexRefitState::Disabled);
        let second = adapter.offline_disable_report().unwrap();
        assert!(second.targets.is_empty());
    }

    #[tokio::test]
    async fn update_requires_force_before_changed_javascript_is_written() {
        let directory = tempdir().unwrap();
        let mut adapter = CodexRefit::new(
            CodexRefitConfig::default().with_state_dir(directory.path().to_owned()),
        )
        .unwrap();
        let bundle_fetches = use_fake_update(
            &mut adapter,
            renderer_assets::test_bundle_with_change(
                "1.1.0",
                Some("opsail-refit-codex-dom-adapter.js"),
            ),
        );

        let error = adapter
            .update_renderer_assets(RendererAssetUpdatePolicy::RequireUnchanged)
            .await
            .unwrap_err();
        assert_eq!(error.code(), CodexRefitErrorCode::UpdateFailed);
        assert!(error.to_string().contains("--force"));
        assert_eq!(bundle_fetches.load(AtomicOrdering::Relaxed), 0);
        let selection = RendererAssetStore::new(directory.path().to_owned())
            .load_or_embedded()
            .unwrap();
        assert_eq!(selection.info.source, RendererAssetSource::Embedded);
        assert!(
            !directory
                .path()
                .join("renderer-assets/current.json")
                .exists()
        );
    }

    #[tokio::test]
    async fn forced_update_installs_for_the_next_session_without_a_cdp_target() {
        let directory = tempdir().unwrap();
        let mut adapter = CodexRefit::new(
            CodexRefitConfig::default().with_state_dir(directory.path().to_owned()),
        )
        .unwrap();
        let bundle_fetches = use_fake_update(
            &mut adapter,
            renderer_assets::test_bundle_with_change(
                "1.1.0",
                Some("opsail-refit-codex-dom-adapter.js"),
            ),
        );

        let report = adapter
            .update_renderer_assets(RendererAssetUpdatePolicy::Force)
            .await
            .unwrap();
        assert_eq!(report.operation, CodexRefitOperation::Update);
        assert!(report.changed);
        assert!(report.forced);
        assert_eq!(report.activation, RendererAssetActivation::NextSession);
        assert_eq!(report.previous.source, RendererAssetSource::Embedded);
        assert_eq!(report.installed.source, RendererAssetSource::Github);
        assert_eq!(report.installed.version, "1.1.0");
        assert_eq!(report.files, renderer_assets::RENDERER_ASSET_FILES.len());
        assert_eq!(bundle_fetches.load(AtomicOrdering::Relaxed), 1);

        let next = CodexRefit::new(
            CodexRefitConfig::default().with_state_dir(directory.path().to_owned()),
        )
        .unwrap();
        assert_eq!(next.payload.asset_info, report.installed);
    }

    #[tokio::test]
    async fn force_installs_changed_javascript_without_inflating_the_prerelease_version() {
        let directory = tempdir().unwrap();
        let mut adapter = CodexRefit::new(
            CodexRefitConfig::default().with_state_dir(directory.path().to_owned()),
        )
        .unwrap();
        let bundle_fetches = use_fake_update(
            &mut adapter,
            renderer_assets::test_bundle_with_change(
                "1.0.0",
                Some("opsail-refit-codex-dom-adapter.js"),
            ),
        );

        let report = adapter
            .update_renderer_assets(RendererAssetUpdatePolicy::Force)
            .await
            .unwrap();
        assert!(report.changed);
        assert!(report.forced);
        assert_eq!(report.installed.version, "1.0.0");
        assert_eq!(report.installed.source, RendererAssetSource::Github);
        assert_eq!(bundle_fetches.load(AtomicOrdering::Relaxed), 1);

        let next = CodexRefit::new(
            CodexRefitConfig::default().with_state_dir(directory.path().to_owned()),
        )
        .unwrap();
        assert_eq!(next.payload.asset_info, report.installed);
    }

    #[tokio::test]
    async fn unchanged_javascript_can_advance_manifest_version_without_force() {
        let directory = tempdir().unwrap();
        let mut adapter = CodexRefit::new(
            CodexRefitConfig::default().with_state_dir(directory.path().to_owned()),
        )
        .unwrap();
        let bundle_fetches = use_fake_update(
            &mut adapter,
            renderer_assets::test_bundle_with_change("1.1.0", None),
        );

        let report = adapter
            .update_renderer_assets(RendererAssetUpdatePolicy::RequireUnchanged)
            .await
            .unwrap();
        assert!(report.changed);
        assert!(!report.forced);
        assert_eq!(report.installed.version, "1.1.0");
        assert_eq!(bundle_fetches.load(AtomicOrdering::Relaxed), 1);
    }

    #[tokio::test]
    async fn unchanged_current_manifest_avoids_javascript_downloads() {
        let directory = tempdir().unwrap();
        let mut adapter = CodexRefit::new(
            CodexRefitConfig::default().with_state_dir(directory.path().to_owned()),
        )
        .unwrap();
        let bundle_fetches = use_fake_update(
            &mut adapter,
            renderer_assets::test_bundle_with_change("1.0.0", None),
        );

        let report = adapter
            .update_renderer_assets(RendererAssetUpdatePolicy::RequireUnchanged)
            .await
            .unwrap();
        assert!(!report.changed);
        assert_eq!(report.activation, RendererAssetActivation::Current);
        assert_eq!(report.installed.source, RendererAssetSource::Embedded);
        assert_eq!(bundle_fetches.load(AtomicOrdering::Relaxed), 0);
    }

    #[test]
    fn renderer_probe_contains_no_remote_or_model_channel() {
        let payload = usage_payload().unwrap();
        let expression = payload.renderer_probe();
        assert!(expression.contains("location.protocol"));
        assert!(!expression.contains("fetch"));
        assert!(!expression.contains("account/rateLimits"));
    }

    #[tokio::test]
    async fn launch_notice_uses_one_renderer_evaluation_and_reports_success() {
        let payload = usage_payload().unwrap();
        let (mut cdp, observed) = test_cdp_session(serde_json::json!({ "shown": true })).await;
        assert!(show_launch_notice(&mut cdp, &payload).await.unwrap());
        cdp.close().await;
        let (methods, closed) = observed.await.unwrap();
        assert_eq!(methods, ["Runtime.evaluate"]);
        assert!(closed);
        assert!(payload.launch_notice().contains("launch-notice"));
    }

    #[test]
    fn report_serialization_uses_stable_state_names() {
        let report = CodexRefitReport {
            operation: CodexRefitOperation::Status,
            port: DEFAULT_CODEX_DEBUG_PORT,
            session_mode: Some(SessionMode::Once),
            launch_policy: Some(LaunchPolicy::LaunchIfStopped),
            launched: Some(true),
            renderer_assets: Some(RendererAssetInfo {
                version: "1.0.0".to_owned(),
                source: RendererAssetSource::Embedded,
            }),
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
        assert_eq!(value["rendererAssets"]["version"], "1.0.0");
        assert_eq!(value["rendererAssets"]["source"], "embedded");
        assert_eq!(value["targets"][0]["state"], "stale");
        assert_eq!(value["targets"][0]["sessionMode"], "once");

        let update = CodexUpdateReport {
            operation: CodexRefitOperation::Update,
            previous: RendererAssetInfo {
                version: "1.0.0".to_owned(),
                source: RendererAssetSource::Embedded,
            },
            installed: RendererAssetInfo {
                version: "1.1.0".to_owned(),
                source: RendererAssetSource::Github,
            },
            changed: true,
            forced: true,
            activation: RendererAssetActivation::NextSession,
            files: renderer_assets::RENDERER_ASSET_FILES.len(),
        };
        let value: Value = serde_json::to_value(update).unwrap();
        assert_eq!(value["operation"], "update");
        assert_eq!(value["installed"]["source"], "github");
        assert_eq!(value["activation"], "next-session");
        assert_eq!(value["forced"], true);
    }

    #[test]
    fn legacy_renderer_diagnostics_remain_parseable_for_reconciliation() {
        let payload = usage_payload().unwrap();
        let mut value = healthy_renderer_status(
            &payload,
            SessionMode::Once,
            "opsail-refit-codex:legacy-health",
        );
        value["diagnostics"]
            .as_object_mut()
            .unwrap()
            .remove("domAdapterVersion");

        let status: RendererStatus = serde_json::from_value(value).unwrap();
        assert_eq!(status.diagnostics.unwrap().dom_adapter_version, 0);
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
        let session =
            CodexSession::new(cdp, DEFAULT_CODEX_DEBUG_PORT, state.clone(), payload, token);
        let mut sessions = [session];

        let report = lifecycle::enable(
            &mut sessions,
            SessionMode::Once,
            &ProgressReporter::default(),
        )
        .await
        .unwrap();
        assert_eq!(report.session_mode, Some(SessionMode::Once));
        sessions[0].close().await;
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
        let mut session = CodexSession::new(
            cdp,
            DEFAULT_CODEX_DEBUG_PORT,
            state,
            Arc::new(usage_payload().unwrap()),
            "opsail-refit-codex:test-disable",
        );

        session.disable().await.unwrap();
        session.close().await;
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

    #[tokio::test]
    async fn disconnected_manager_stops_immediately_when_chatgpt_is_gone() {
        let inspections = AtomicUsize::new(0);
        let decision = wait_for_reconnect_or_app_exit(
            Duration::from_secs(30),
            || {
                inspections.fetch_add(1, AtomicOrdering::Relaxed);
                std::future::ready(false)
            },
            std::future::pending(),
        )
        .await;

        assert_eq!(decision, RecoveryDecision::Stop);
        assert_eq!(inspections.load(AtomicOrdering::Relaxed), 1);
    }

    #[tokio::test]
    async fn disconnected_manager_retries_when_chatgpt_is_still_running() {
        let inspections = AtomicUsize::new(0);
        let decision = wait_for_reconnect_or_app_exit(
            Duration::from_millis(1),
            || {
                inspections.fetch_add(1, AtomicOrdering::Relaxed);
                std::future::ready(true)
            },
            std::future::pending(),
        )
        .await;

        assert_eq!(decision, RecoveryDecision::Reconnect);
        assert_eq!(inspections.load(AtomicOrdering::Relaxed), 2);
    }

    #[test]
    fn doctor_names_default_and_detected_session_lifecycles() {
        let report = doctor_report(
            DEFAULT_CODEX_DEBUG_PORT,
            RendererAssetInfo {
                version: "1.0.0".to_owned(),
                source: RendererAssetSource::Embedded,
            },
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
    async fn status_distinguishes_unavailable_data_from_missing_layout() {
        let directory = tempdir().unwrap();
        let state = StateStore::new(directory.path().to_owned());
        let payload = Arc::new(usage_payload().unwrap());
        let token = "opsail-refit-codex:diagnostic-health";

        let mut unavailable = healthy_renderer_status(&payload, SessionMode::Once, token);
        unavailable["diagnostics"]["dataState"] = serde_json::json!("unavailable");
        unavailable["diagnostics"]["visible"] = serde_json::json!(false);
        let (cdp, _) = test_cdp_session(unavailable).await;
        let mut session = CodexSession::new(
            cdp,
            DEFAULT_CODEX_DEBUG_PORT,
            state.clone(),
            Arc::clone(&payload),
            token,
        );
        let health = session.health().await.unwrap();
        assert!(health.healthy);
        assert_eq!(health.state, CodexRefitState::Stale);
        assert!(
            health
                .detail
                .as_deref()
                .unwrap()
                .contains("no valid rate-limit window")
        );
        session.close().await;

        let mut no_layout = healthy_renderer_status(&payload, SessionMode::Once, token);
        no_layout["diagnostics"]["visible"] = serde_json::json!(false);
        let (cdp, _) = test_cdp_session(no_layout).await;
        let mut session = CodexSession::new(cdp, DEFAULT_CODEX_DEBUG_PORT, state, payload, token);
        let health = session.health().await.unwrap();
        assert!(health.healthy);
        assert_eq!(health.state, CodexRefitState::Stale);
        assert!(
            health
                .detail
                .as_deref()
                .unwrap()
                .contains("safe account-row placement")
        );
        session.close().await;
    }

    #[tokio::test]
    async fn status_reports_only_observed_reset_credit_state() {
        let directory = tempdir().unwrap();
        let state = StateStore::new(directory.path().to_owned());
        let payload = Arc::new(usage_payload().unwrap());
        let token = "opsail-refit-codex:reset-credit-observation";

        for (serialized, expected, reported_count, expected_count) in [
            ("not-observed", ResetCreditState::NotObserved, 0, None),
            ("empty", ResetCreditState::Empty, 0, Some(0)),
            ("available", ResetCreditState::Available, 3, Some(3)),
        ] {
            let mut status = healthy_renderer_status(&payload, SessionMode::Once, token);
            status["diagnostics"]["resetCreditState"] = serde_json::json!(serialized);
            status["diagnostics"]["resetCreditCount"] = serde_json::json!(reported_count);
            let (cdp, _) = test_cdp_session(status).await;
            let mut session = CodexSession::new(
                cdp,
                DEFAULT_CODEX_DEBUG_PORT,
                state.clone(),
                Arc::clone(&payload),
                token,
            );

            let health = session.health().await.unwrap();
            assert!(health.healthy);
            assert_eq!(health.state, CodexRefitState::Enabled);
            assert_eq!(health.reset_credit_state, Some(expected));
            assert_eq!(health.reset_credit_count, expected_count);
            assert!(health.detail.is_none());
            session.close().await;
        }
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
        let mut once = CodexSession::new(
            cdp,
            DEFAULT_CODEX_DEBUG_PORT,
            state.clone(),
            Arc::clone(&payload),
            once_token,
        );
        let once_health = once.health().await.unwrap();
        assert!(once_health.healthy);
        assert_eq!(once_health.session_mode, Some(SessionMode::Once));
        once.close().await;

        let (cdp, _) = test_cdp_session(disabled_renderer_status()).await;
        let mut reloaded_once = CodexSession::new(
            cdp,
            DEFAULT_CODEX_DEBUG_PORT,
            state.clone(),
            Arc::clone(&payload),
            once_token,
        );
        let reloaded_health = reloaded_once.health().await.unwrap();
        assert!(reloaded_health.healthy);
        assert_eq!(reloaded_health.state, CodexRefitState::Disabled);
        assert_eq!(reloaded_health.session_mode, None);
        reloaded_once.close().await;

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
        let mut persistent = CodexSession::new(
            cdp,
            DEFAULT_CODEX_DEBUG_PORT,
            state.clone(),
            Arc::clone(&payload),
            persistent_token,
        );
        let managed_health = persistent.health().await.unwrap();
        assert!(managed_health.healthy);
        assert_eq!(managed_health.session_mode, Some(SessionMode::Persistent));
        persistent.close().await;

        drop(managed_lock);
        let (cdp, _) = test_cdp_session(healthy_renderer_status(
            &payload,
            SessionMode::Persistent,
            persistent_token,
        ))
        .await;
        let mut disconnected = CodexSession::new(
            cdp,
            DEFAULT_CODEX_DEBUG_PORT,
            state,
            payload,
            persistent_token,
        );
        let disconnected_health = disconnected.health().await.unwrap();
        assert!(!disconnected_health.healthy);
        assert_eq!(disconnected_health.state, CodexRefitState::Stale);
        assert_eq!(
            disconnected_health.session_mode,
            Some(SessionMode::Persistent)
        );
        disconnected.close().await;
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
        assert!(GUIDE.contains("detached manager"));
        assert!(GUIDE.contains("ChatGPT has exited"));
        assert!(GUIDE.contains("there is no timer or process polling while the socket is healthy"));
        assert!(GUIDE.contains("--foreground"));
    }
}
