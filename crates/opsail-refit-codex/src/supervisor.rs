use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::stream::{FuturesUnordered, StreamExt as _};
use tokio::time::sleep;

use crate::cdp::{SessionTermination, wait_for_termination};
use crate::error::CodexRefitError;
use crate::model::SessionMode;
use crate::platform::{LaunchedProcess, ValidatedAppIdentity};
use crate::state::StateManagedSessionLock;
use crate::{CodexRefit, CodexSession, close_sessions, run_blocking};

pub(super) struct PersistentSupervisor {
    adapter: CodexRefit,
    sessions: Vec<CodexSession>,
    manager_token: String,
    app_identity: ValidatedAppIdentity,
    launched_process: Option<LaunchedProcess>,
    _managed_lock: StateManagedSessionLock,
}

impl PersistentSupervisor {
    pub(super) fn new(
        adapter: CodexRefit,
        sessions: Vec<CodexSession>,
        manager_token: String,
        app_identity: ValidatedAppIdentity,
        launched_process: Option<LaunchedProcess>,
        managed_lock: StateManagedSessionLock,
    ) -> Self {
        Self {
            adapter,
            sessions,
            manager_token,
            app_identity,
            launched_process,
            _managed_lock: managed_lock,
        }
    }

    pub(super) async fn run(mut self) -> Result<(), CodexRefitError> {
        let mut backoff = ReconnectBackoff::default();
        loop {
            let mut terminations = FuturesUnordered::new();
            for session in &self.sessions {
                terminations.push(wait_for_termination(session.termination_receiver()));
            }
            let event = if let Some(process) = &self.launched_process {
                let exit = process.exit_receiver();
                tokio::select! {
                    termination = terminations.next() => SupervisorEvent::Session(
                        termination.expect("validated persistent sessions cannot be empty")
                    ),
                    () = wait_for_process_exit(exit) => SupervisorEvent::LaunchedProcessExit,
                }
            } else {
                SupervisorEvent::Session(
                    terminations
                        .next()
                        .await
                        .expect("validated persistent sessions cannot be empty"),
                )
            };
            if event == SupervisorEvent::LaunchedProcessExit {
                self.launched_process = None;
                if !self.confirm_app_running().await {
                    self.adapter
                        .state
                        .remove_absent_targets(self.adapter.port, &[])?;
                    tracing::info!(
                        target: "opsail_refit_codex",
                        "[opsail-refit-codex] launched ChatGPT process exited; managed session stopped"
                    );
                    return Ok(());
                }
                continue;
            }
            let SupervisorEvent::Session(termination) = event else {
                unreachable!();
            };
            tracing::info!(
                target: "opsail_refit_codex",
                ?termination,
                "[opsail-refit-codex] managed renderer connection ended"
            );
            close_sessions(&mut self.sessions).await;

            loop {
                let delay = backoff.next_delay();
                if self.wait_for_reconnect_or_app_exit(delay).await == RecoveryDecision::Stop {
                    self.adapter
                        .state
                        .remove_absent_targets(self.adapter.port, &[])?;
                    tracing::info!(
                        target: "opsail_refit_codex",
                        "[opsail-refit-codex] ChatGPT exited; managed session stopped"
                    );
                    return Ok(());
                }
                let attempt = async {
                    let _operation_lock = self.adapter.state.try_operation_lock()?;
                    self.adapter
                        .connect_and_enable_validated(
                            SessionMode::Persistent,
                            &self.manager_token,
                            None,
                            &self.app_identity,
                        )
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

    async fn wait_for_reconnect_or_app_exit(&mut self, delay: Duration) -> RecoveryDecision {
        let backend = Arc::clone(&self.adapter.launch_backend);
        let app = self.app_identity.clone();
        let process_exit = self
            .launched_process
            .as_ref()
            .map(LaunchedProcess::exit_receiver);
        let process_exited = Arc::new(AtomicBool::new(false));
        let observed_process_exit = Arc::clone(&process_exited);
        let decision = wait_for_reconnect_or_app_exit(
            delay,
            || {
                let backend = Arc::clone(&backend);
                let app = app.clone();
                async move {
                    match run_blocking(move || backend.app_is_running(&app)).await {
                        Ok(running) => running,
                        Err(error) => {
                            tracing::warn!(
                                target: "opsail_refit_codex",
                                code = error.code().as_str(),
                                "[opsail-refit-codex] could not confirm ChatGPT process state"
                            );
                            true
                        }
                    }
                }
            },
            async move {
                if let Some(exit) = process_exit {
                    wait_for_process_exit(exit).await;
                    observed_process_exit.store(true, Ordering::Release);
                } else {
                    std::future::pending::<()>().await;
                }
            },
        )
        .await;
        if process_exited.load(Ordering::Acquire) {
            self.launched_process = None;
        }
        decision
    }

    async fn confirm_app_running(&self) -> bool {
        let backend = Arc::clone(&self.adapter.launch_backend);
        let app = self.app_identity.clone();
        match run_blocking(move || backend.app_is_running(&app)).await {
            Ok(running) => running,
            Err(error) => {
                tracing::warn!(
                    target: "opsail_refit_codex",
                    code = error.code().as_str(),
                    "[opsail-refit-codex] could not confirm ChatGPT process state"
                );
                true
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct ReconnectBackoff {
    pub(super) next: Duration,
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
    pub(super) fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = self.next.saturating_mul(2).min(self.maximum);
        delay
    }

    pub(super) fn reset(&mut self) {
        self.next = Duration::from_millis(250);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupervisorEvent {
    Session(SessionTermination),
    LaunchedProcessExit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecoveryDecision {
    Reconnect,
    Stop,
}

pub(super) async fn wait_for_reconnect_or_app_exit<F, Fut, E>(
    delay: Duration,
    mut app_is_running: F,
    process_exit: E,
) -> RecoveryDecision
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
    E: std::future::Future<Output = ()>,
{
    if !app_is_running().await {
        return RecoveryDecision::Stop;
    }
    tokio::select! {
        () = sleep(delay) => {}
        () = process_exit => {}
    }
    if app_is_running().await {
        RecoveryDecision::Reconnect
    } else {
        RecoveryDecision::Stop
    }
}

async fn wait_for_process_exit(mut exit: tokio::sync::watch::Receiver<bool>) {
    if *exit.borrow() {
        return;
    }
    while exit.changed().await.is_ok() {
        if *exit.borrow() {
            return;
        }
    }
}

pub(super) fn new_manager_token() -> String {
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
