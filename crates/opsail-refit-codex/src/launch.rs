use std::future::Future;
use std::time::Duration;

use tokio::time::{Instant, sleep};

use crate::ProgressReporter;
use crate::error::{CodexRefitError, CodexRefitErrorCode};
use crate::model::CodexRefitStage;
use crate::platform::{self, LaunchedProcess, ValidatedAppIdentity};

pub(crate) trait LaunchBackend: Send + Sync {
    #[cfg(test)]
    fn validate_app(&self) -> Result<ValidatedAppIdentity, CodexRefitError>;
    fn loopback_port_available(&self, port: u16) -> Result<bool, CodexRefitError>;
    fn app_is_running(&self, app: &ValidatedAppIdentity) -> Result<bool, CodexRefitError>;
    fn spawn(
        &self,
        port: u16,
        app: &ValidatedAppIdentity,
    ) -> Result<LaunchedProcess, CodexRefitError>;
}

pub(crate) struct SystemLaunchBackend;

impl LaunchBackend for SystemLaunchBackend {
    #[cfg(test)]
    fn validate_app(&self) -> Result<ValidatedAppIdentity, CodexRefitError> {
        platform::validate_app()
    }

    fn loopback_port_available(&self, port: u16) -> Result<bool, CodexRefitError> {
        platform::loopback_port_available(port)
    }

    fn app_is_running(&self, app: &ValidatedAppIdentity) -> Result<bool, CodexRefitError> {
        platform::app_is_running(app)
    }

    fn spawn(
        &self,
        port: u16,
        app: &ValidatedAppIdentity,
    ) -> Result<LaunchedProcess, CodexRefitError> {
        platform::launch_app(port, app)
    }
}

#[cfg(test)]
fn launch_if_stopped(
    backend: &dyn LaunchBackend,
    port: u16,
) -> Result<(ValidatedAppIdentity, LaunchedProcess), CodexRefitError> {
    let app = backend.validate_app()?;
    let process = launch_validated(backend, port, &app, &ProgressReporter::default())?;
    Ok((app, process))
}

pub(crate) fn launch_validated(
    backend: &dyn LaunchBackend,
    port: u16,
    app: &ValidatedAppIdentity,
    progress: &ProgressReporter,
) -> Result<LaunchedProcess, CodexRefitError> {
    progress.report(CodexRefitStage::CheckLaunchReadiness);
    if !backend.loopback_port_available(port)? {
        return Err(CodexRefitError::new(
            CodexRefitErrorCode::PortUnavailable,
            format!("loopback CDP port {port} is already occupied"),
        ));
    }
    if backend.app_is_running(app)? {
        return Err(CodexRefitError::new(
            CodexRefitErrorCode::RestartRequired,
            "ChatGPT is already running without the requested CDP listener; quit and relaunch it manually or choose attach-only after configuring CDP",
        ));
    }
    progress.report(CodexRefitStage::LaunchApplication);
    backend.spawn(port, app)
}

pub(crate) async fn wait_for_endpoint<T, F, Fut>(
    port: u16,
    timeout: Duration,
    mut connect: F,
) -> Result<T, CodexRefitError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, CodexRefitError>>,
{
    let deadline = Instant::now() + timeout;
    let mut delay = Duration::from_millis(100);
    loop {
        match connect().await {
            Ok(value) => return Ok(value),
            Err(error) if !retryable_launch_wait(error.code()) => return Err(error),
            Err(_) if Instant::now() >= deadline => {
                return Err(CodexRefitError::new(
                    CodexRefitErrorCode::LaunchFailed,
                    format!(
                        "ChatGPT did not expose a validated loopback CDP endpoint on port {port} before the launch timeout"
                    ),
                ));
            }
            Err(_) => {
                sleep(delay.min(deadline.saturating_duration_since(Instant::now()))).await;
                delay = delay.saturating_mul(2).min(Duration::from_secs(1));
            }
        }
    }
}

pub(crate) async fn wait_for_endpoint_or_process_exit<T, F, Fut>(
    port: u16,
    timeout: Duration,
    mut process_exit: tokio::sync::watch::Receiver<bool>,
    connect: F,
) -> Result<T, CodexRefitError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, CodexRefitError>>,
{
    tokio::select! {
        result = wait_for_endpoint(port, timeout, connect) => result,
        () = wait_for_process_exit(&mut process_exit) => Err(CodexRefitError::new(
            CodexRefitErrorCode::LaunchFailed,
            "the launched ChatGPT process exited before its validated CDP endpoint was ready",
        )),
    }
}

async fn wait_for_process_exit(exit: &mut tokio::sync::watch::Receiver<bool>) {
    if *exit.borrow() {
        return;
    }
    while exit.changed().await.is_ok() {
        if *exit.borrow() {
            return;
        }
    }
}

fn retryable_launch_wait(code: CodexRefitErrorCode) -> bool {
    matches!(
        code,
        CodexRefitErrorCode::SessionUnavailable
            | CodexRefitErrorCode::TargetNotFound
            | CodexRefitErrorCode::BridgeUnavailable
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;

    struct FakeBackend {
        validation_error: bool,
        port_available: bool,
        running: bool,
        spawn_error: bool,
        validations: AtomicUsize,
        port_checks: AtomicUsize,
        inspections: AtomicUsize,
        spawns: AtomicUsize,
    }

    impl FakeBackend {
        fn new(port_available: bool, running: bool, spawn_error: bool) -> Self {
            Self {
                validation_error: false,
                port_available,
                running,
                spawn_error,
                validations: AtomicUsize::new(0),
                port_checks: AtomicUsize::new(0),
                inspections: AtomicUsize::new(0),
                spawns: AtomicUsize::new(0),
            }
        }

        fn invalid() -> Self {
            Self {
                validation_error: true,
                ..Self::new(true, false, false)
            }
        }
    }

    impl LaunchBackend for FakeBackend {
        fn validate_app(&self) -> Result<ValidatedAppIdentity, CodexRefitError> {
            self.validations.fetch_add(1, Ordering::Relaxed);
            if self.validation_error {
                Err(CodexRefitError::new(
                    CodexRefitErrorCode::TargetValidationFailed,
                    "planned application validation failure",
                ))
            } else {
                Ok(ValidatedAppIdentity::for_test())
            }
        }

        fn loopback_port_available(&self, _port: u16) -> Result<bool, CodexRefitError> {
            self.port_checks.fetch_add(1, Ordering::Relaxed);
            Ok(self.port_available)
        }

        fn app_is_running(&self, _app: &ValidatedAppIdentity) -> Result<bool, CodexRefitError> {
            self.inspections.fetch_add(1, Ordering::Relaxed);
            Ok(self.running)
        }

        fn spawn(
            &self,
            _port: u16,
            _app: &ValidatedAppIdentity,
        ) -> Result<LaunchedProcess, CodexRefitError> {
            self.spawns.fetch_add(1, Ordering::Relaxed);
            if self.spawn_error {
                Err(CodexRefitError::new(
                    CodexRefitErrorCode::LaunchFailed,
                    "planned launch failure",
                ))
            } else {
                Ok(LaunchedProcess::untracked(4242))
            }
        }
    }

    #[test]
    fn invalid_application_fails_before_port_or_process_work() {
        let backend = FakeBackend::invalid();
        let error = launch_if_stopped(&backend, 55321).unwrap_err();
        assert_eq!(error.code(), CodexRefitErrorCode::TargetValidationFailed);
        assert_eq!(backend.validations.load(Ordering::Relaxed), 1);
        assert_eq!(backend.port_checks.load(Ordering::Relaxed), 0);
        assert_eq!(backend.inspections.load(Ordering::Relaxed), 0);
        assert_eq!(backend.spawns.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn occupied_port_fails_before_process_inspection_or_spawn() {
        let backend = FakeBackend::new(false, false, false);
        let error = launch_if_stopped(&backend, 55321).unwrap_err();
        assert_eq!(error.code(), CodexRefitErrorCode::PortUnavailable);
        assert_eq!(backend.validations.load(Ordering::Relaxed), 1);
        assert_eq!(backend.port_checks.load(Ordering::Relaxed), 1);
        assert_eq!(backend.inspections.load(Ordering::Relaxed), 0);
        assert_eq!(backend.spawns.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn running_app_requires_manual_restart_and_is_never_spawned() {
        let backend = FakeBackend::new(true, true, false);
        let error = launch_if_stopped(&backend, 55321).unwrap_err();
        assert_eq!(error.code(), CodexRefitErrorCode::RestartRequired);
        assert_eq!(backend.inspections.load(Ordering::Relaxed), 1);
        assert_eq!(backend.spawns.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn stopped_app_is_spawned_exactly_once() {
        let backend = FakeBackend::new(true, false, false);
        let (_, process) = launch_if_stopped(&backend, 55321).unwrap();
        assert_eq!(process.pid(), 4242);
        assert_eq!(backend.spawns.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn launch_reports_preflight_before_spawn() {
        let backend = FakeBackend::new(true, false, false);
        let observed = Arc::new(Mutex::new(Vec::new()));
        let progress_observed = Arc::clone(&observed);
        let progress = ProgressReporter(Some(Arc::new(move |stage| {
            progress_observed.lock().unwrap().push(stage);
        })));
        launch_validated(
            &backend,
            55321,
            &ValidatedAppIdentity::for_test(),
            &progress,
        )
        .unwrap();
        assert_eq!(
            *observed.lock().unwrap(),
            [
                CodexRefitStage::CheckLaunchReadiness,
                CodexRefitStage::LaunchApplication,
            ]
        );
    }

    #[test]
    fn spawn_failure_has_a_distinct_diagnostic() {
        let backend = FakeBackend::new(true, false, true);
        let error = launch_if_stopped(&backend, 55321).unwrap_err();
        assert_eq!(error.code(), CodexRefitErrorCode::LaunchFailed);
        assert_eq!(backend.spawns.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn endpoint_wait_retries_bounded_startup_states_and_then_succeeds() {
        let attempts = AtomicUsize::new(0);
        let value = wait_for_endpoint(55321, Duration::from_secs(1), || async {
            let attempt = attempts.fetch_add(1, Ordering::Relaxed);
            if attempt < 2 {
                Err(CodexRefitError::new(
                    CodexRefitErrorCode::SessionUnavailable,
                    "not ready",
                ))
            } else {
                Ok("ready")
            }
        })
        .await
        .unwrap();
        assert_eq!(value, "ready");
        assert_eq!(attempts.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn endpoint_wait_times_out_without_respawning() {
        let attempts = AtomicUsize::new(0);
        let error = wait_for_endpoint(55321, Duration::from_millis(1), || async {
            attempts.fetch_add(1, Ordering::Relaxed);
            Err::<(), _>(CodexRefitError::new(
                CodexRefitErrorCode::SessionUnavailable,
                "not ready",
            ))
        })
        .await
        .unwrap_err();
        assert_eq!(error.code(), CodexRefitErrorCode::LaunchFailed);
        assert!(attempts.load(Ordering::Relaxed) >= 1);
    }

    #[tokio::test]
    async fn endpoint_wait_stops_as_soon_as_the_launched_process_exits() {
        let (exit_tx, exit) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            let _ = exit_tx.send(true);
        });
        let error = wait_for_endpoint_or_process_exit(55321, Duration::from_secs(30), exit, || {
            std::future::pending::<Result<(), CodexRefitError>>()
        })
        .await
        .unwrap_err();
        assert_eq!(error.code(), CodexRefitErrorCode::LaunchFailed);
        assert!(error.to_string().contains("exited"));
    }
}
