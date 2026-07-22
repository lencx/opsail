use std::sync::Arc;

use serde::Deserialize;

use crate::cdp::CdpSession;
use crate::error::{CodexRefitError, CodexRefitErrorCode};
use crate::lifecycle::{ManagedSession, SessionFuture};
use crate::model::{CodexRefitState, CodexTargetHealth, ResetCreditState, SessionMode};
use crate::payload::UsagePayload;
use crate::state::{StateStore, TargetRecord};

pub(super) const USAGE_RUNTIME_LISTENER_COUNT: usize = 10;
pub(super) const DOM_ADAPTER_API_VERSION: u64 = 1;

pub(super) struct CodexSession {
    cdp: CdpSession,
    port: u16,
    state: StateStore,
    payload: Arc<UsagePayload>,
    manager_token: String,
    owned_script_identifiers: Vec<String>,
}

impl CodexSession {
    pub(super) fn new(
        cdp: CdpSession,
        port: u16,
        state: StateStore,
        payload: Arc<UsagePayload>,
        manager_token: impl Into<String>,
    ) -> Self {
        Self {
            cdp,
            port,
            state,
            payload,
            manager_token: manager_token.into(),
            owned_script_identifiers: Vec::new(),
        }
    }

    pub(super) fn target_id(&self) -> &str {
        self.cdp.target_id()
    }

    pub(super) fn termination_receiver(
        &self,
    ) -> tokio::sync::watch::Receiver<crate::cdp::SessionTermination> {
        self.cdp.termination_receiver()
    }

    pub(super) async fn show_launch_notice(&mut self) -> Result<bool, CodexRefitError> {
        show_launch_notice(&mut self.cdp, &self.payload).await
    }

    #[cfg(test)]
    pub(super) async fn close(&mut self) {
        self.cdp.close().await;
    }
}

impl ManagedSession for CodexSession {
    fn target_id(&self) -> &str {
        self.target_id()
    }

    fn health(&mut self) -> SessionFuture<'_, Result<CodexTargetHealth, CodexRefitError>> {
        Box::pin(async move {
            let runtime = renderer_status(&mut self.cdp, &self.payload).await?;
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
                    && diagnostics.dom_adapter_version == DOM_ADAPTER_API_VERSION
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
                let visible = runtime
                    .diagnostics
                    .as_ref()
                    .is_some_and(|diagnostics| diagnostics.visible);
                let stale = !matches!(data_state, Some("ready")) || !visible;
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
                if let Some(reset_credit_state) =
                    diagnostics.and_then(|diagnostics| diagnostics.reset_credit_state)
                {
                    health = health.with_reset_credits(
                        reset_credit_state,
                        diagnostics.and_then(|diagnostics| diagnostics.reset_credit_count),
                    );
                }
                return Ok(match data_state {
                    Some("unavailable") => health.with_detail(
                        "the usage UI is installed but no valid rate-limit window is available after bounded startup calibration",
                    ),
                    Some("loading") => health.with_detail(
                        "the usage UI is installed and waiting for its initial local account snapshot",
                    ),
                    Some("stale") => health.with_detail(
                        "the usage UI is showing its last successful local account snapshot in a stale state",
                    ),
                    Some("ready") if !visible => health.with_detail(
                        "usage data is ready but the capsule is waiting for a safe account-row placement",
                    ),
                    Some("ready") => health,
                    _ => health.with_detail(
                        "the usage UI is installed but its renderer data state is not recognized",
                    ),
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
                        let _ = self.cdp.evaluate(self.payload.disable()).await;
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
                let _ = self.cdp.evaluate(self.payload.disable()).await;
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
            let cleanup_value =
                self.cdp
                    .evaluate(self.payload.disable())
                    .await
                    .map_err(|error| {
                        if error.code() == CodexRefitErrorCode::InjectionFailed {
                            CodexRefitError::new(
                                CodexRefitErrorCode::CleanupFailed,
                                error.to_string(),
                            )
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
    dom_adapter_version: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RendererStatus {
    installed: bool,
    revision: Option<String>,
    pub(super) diagnostics: Option<RendererDiagnostics>,
    host_count: usize,
    style_count: usize,
    details_count: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RendererDiagnostics {
    pub(super) installed: bool,
    mode: String,
    pub(super) session_mode: SessionMode,
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
    pub(super) dom_adapter_version: u64,
    #[serde(default)]
    data_state: String,
    #[serde(default)]
    visible: bool,
    #[serde(default)]
    reset_credit_state: Option<ResetCreditState>,
    #[serde(default)]
    reset_credit_count: Option<usize>,
}

#[derive(Deserialize)]
struct CleanupResult {
    clean: bool,
}

#[derive(Deserialize)]
struct LaunchNoticeResult {
    shown: bool,
}

pub(super) async fn probe_renderer(
    session: &mut CdpSession,
    payload: &UsagePayload,
) -> Result<(), CodexRefitError> {
    let probe: RendererProbe = serde_json::from_value(
        session.evaluate(payload.renderer_probe()).await?,
    )
    .map_err(|_| {
        CodexRefitError::new(
            CodexRefitErrorCode::TargetValidationFailed,
            "the renderer returned an invalid identity probe",
        )
    })?;
    if probe.dom_adapter_version != DOM_ADAPTER_API_VERSION
        || !probe.app_protocol
        || !probe.shell
        || !probe.sidebar
    {
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

pub(super) async fn renderer_status(
    session: &mut CdpSession,
    payload: &UsagePayload,
) -> Result<RendererStatus, CodexRefitError> {
    serde_json::from_value(session.evaluate(&payload.status()).await?).map_err(|_| {
        CodexRefitError::new(
            CodexRefitErrorCode::Stale,
            "the renderer returned an invalid refit health result",
        )
    })
}

pub(super) async fn show_launch_notice(
    session: &mut CdpSession,
    payload: &UsagePayload,
) -> Result<bool, CodexRefitError> {
    let result: LaunchNoticeResult =
        serde_json::from_value(session.evaluate(payload.launch_notice()).await?).map_err(|_| {
            CodexRefitError::new(
                CodexRefitErrorCode::InjectionFailed,
                "the renderer returned an invalid launch notice result",
            )
        })?;
    Ok(result.shown)
}

pub(super) async fn close_sessions(sessions: &mut [CodexSession]) {
    for session in sessions {
        session.cdp.close().await;
    }
}
