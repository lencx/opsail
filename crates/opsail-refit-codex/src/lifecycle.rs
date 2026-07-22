use std::future::Future;
use std::pin::Pin;

use crate::error::{CodexRefitError, CodexRefitErrorCode};
use crate::model::{
    CodexRefitOperation, CodexRefitReport, CodexRefitState, CodexTargetHealth, SessionMode,
};

pub(crate) type SessionFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub(crate) trait ManagedSession: Send {
    fn target_id(&self) -> &str;
    fn health(&mut self) -> SessionFuture<'_, Result<CodexTargetHealth, CodexRefitError>>;
    fn enable(&mut self, mode: SessionMode) -> SessionFuture<'_, Result<(), CodexRefitError>>;
    fn disable(&mut self) -> SessionFuture<'_, Result<(), CodexRefitError>>;
}

pub(crate) async fn enable<S>(
    sessions: &mut [S],
    mode: SessionMode,
) -> Result<CodexRefitReport, CodexRefitError>
where
    S: ManagedSession,
{
    let mut reports = Vec::with_capacity(sessions.len());
    let mut changed = Vec::with_capacity(sessions.len());
    for index in 0..sessions.len() {
        let before = match sessions[index].health().await {
            Ok(health) => health,
            Err(error) => {
                rollback(sessions, &changed).await;
                return Err(error);
            }
        };
        let should_change = !is_usable_enabled(&before, mode);
        if should_change && let Err(error) = sessions[index].enable(mode).await {
            let _ = sessions[index].disable().await;
            rollback(sessions, &changed).await;
            return Err(error);
        }
        let mut after = match sessions[index].health().await {
            Ok(health) => health,
            Err(error) => {
                if should_change {
                    let _ = sessions[index].disable().await;
                }
                rollback(sessions, &changed).await;
                return Err(error);
            }
        };
        if !is_usable_enabled(&after, mode) {
            if should_change {
                let _ = sessions[index].disable().await;
            }
            rollback(sessions, &changed).await;
            return Err(CodexRefitError::new(
                CodexRefitErrorCode::Stale,
                format!(
                    "target `{}` did not reach a healthy enabled state",
                    sessions[index].target_id()
                ),
            ));
        }
        after.changed = should_change;
        reports.push(after);
        changed.push((index, should_change));
    }
    Ok(CodexRefitReport {
        operation: CodexRefitOperation::Enable,
        port: 0,
        session_mode: Some(mode),
        launch_policy: None,
        launched: None,
        renderer_assets: None,
        targets: reports,
    })
}

fn is_usable_enabled(health: &CodexTargetHealth, mode: SessionMode) -> bool {
    health.healthy
        && health.session_mode == Some(mode)
        && matches!(
            health.state,
            CodexRefitState::Enabled | CodexRefitState::Stale
        )
}

pub(crate) async fn disable<S>(sessions: &mut [S]) -> Result<CodexRefitReport, CodexRefitError>
where
    S: ManagedSession,
{
    let mut reports = Vec::with_capacity(sessions.len());
    let mut first_error = None;
    for session in sessions {
        let target_id = session.target_id().to_owned();
        let before = session.health().await.ok();
        let detected_mode = before.as_ref().and_then(|health| health.session_mode);
        let changed = before
            .as_ref()
            .map(|health| health.state != CodexRefitState::Disabled || !health.healthy)
            .unwrap_or(true);
        let action_error = if changed {
            session.disable().await.err()
        } else {
            None
        };
        let after = session.health().await;
        if let Some(error) = action_error {
            if first_error.is_none() {
                first_error = Some(error);
            }
            continue;
        }
        let mut after = match after {
            Ok(health) => health,
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
                continue;
            }
        };
        if after.state != CodexRefitState::Disabled || !after.healthy {
            let error = CodexRefitError::new(
                CodexRefitErrorCode::CleanupFailed,
                format!("target `{target_id}` did not reach a clean disabled state"),
            );
            if first_error.is_none() {
                first_error = Some(error);
            }
            continue;
        }
        after.session_mode = detected_mode;
        after.changed = changed;
        reports.push(after);
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(CodexRefitReport {
        operation: CodexRefitOperation::Disable,
        port: 0,
        session_mode: common_session_mode(&reports),
        launch_policy: None,
        launched: None,
        renderer_assets: None,
        targets: reports,
    })
}

pub(crate) async fn status<S>(sessions: &mut [S]) -> Result<CodexRefitReport, CodexRefitError>
where
    S: ManagedSession,
{
    let mut reports = Vec::with_capacity(sessions.len());
    for session in sessions {
        reports.push(session.health().await?);
    }
    Ok(CodexRefitReport {
        operation: CodexRefitOperation::Status,
        port: 0,
        session_mode: common_session_mode(&reports),
        launch_policy: None,
        launched: None,
        renderer_assets: None,
        targets: reports,
    })
}

fn common_session_mode(targets: &[CodexTargetHealth]) -> Option<SessionMode> {
    let first = targets.first()?.session_mode?;
    targets
        .iter()
        .all(|target| target.session_mode == Some(first))
        .then_some(first)
}

async fn rollback<S>(sessions: &mut [S], changed: &[(usize, bool)])
where
    S: ManagedSession,
{
    for (index, did_change) in changed.iter().rev() {
        if *did_change {
            let _ = sessions[*index].disable().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Default)]
    struct FakeState {
        enabled: BTreeMap<String, bool>,
        modes: BTreeMap<String, SessionMode>,
        enable_calls: BTreeMap<String, usize>,
        disable_calls: BTreeMap<String, usize>,
        fail_enable: Option<String>,
        fail_disable: Option<String>,
    }

    struct FakeSession {
        target: String,
        state: Arc<Mutex<FakeState>>,
    }

    impl ManagedSession for FakeSession {
        fn target_id(&self) -> &str {
            &self.target
        }

        fn health(&mut self) -> SessionFuture<'_, Result<CodexTargetHealth, CodexRefitError>> {
            Box::pin(async move {
                let enabled = self
                    .state
                    .lock()
                    .unwrap()
                    .enabled
                    .get(&self.target)
                    .copied()
                    .unwrap_or(false);
                let health = CodexTargetHealth::new(
                    &self.target,
                    if enabled {
                        CodexRefitState::Enabled
                    } else {
                        CodexRefitState::Disabled
                    },
                    true,
                );
                Ok(if enabled {
                    let mode = self
                        .state
                        .lock()
                        .unwrap()
                        .modes
                        .get(&self.target)
                        .copied()
                        .unwrap_or(SessionMode::Persistent);
                    health.with_session_mode(mode)
                } else {
                    health
                })
            })
        }

        fn enable(&mut self, mode: SessionMode) -> SessionFuture<'_, Result<(), CodexRefitError>> {
            Box::pin(async move {
                let mut state = self.state.lock().unwrap();
                *state.enable_calls.entry(self.target.clone()).or_default() += 1;
                if state.fail_enable.as_deref() == Some(&self.target) {
                    return Err(CodexRefitError::new(
                        CodexRefitErrorCode::InjectionFailed,
                        "planned enable failure",
                    ));
                }
                state.enabled.insert(self.target.clone(), true);
                state.modes.insert(self.target.clone(), mode);
                Ok(())
            })
        }

        fn disable(&mut self) -> SessionFuture<'_, Result<(), CodexRefitError>> {
            Box::pin(async move {
                let mut state = self.state.lock().unwrap();
                *state.disable_calls.entry(self.target.clone()).or_default() += 1;
                if state.fail_disable.as_deref() == Some(&self.target) {
                    return Err(CodexRefitError::new(
                        CodexRefitErrorCode::CleanupFailed,
                        "planned disable failure",
                    ));
                }
                state.enabled.insert(self.target.clone(), false);
                Ok(())
            })
        }
    }

    fn sessions(targets: &[&str]) -> (Vec<FakeSession>, Arc<Mutex<FakeState>>) {
        let state = Arc::new(Mutex::new(FakeState::default()));
        let sessions = targets
            .iter()
            .map(|target| FakeSession {
                target: (*target).to_owned(),
                state: Arc::clone(&state),
            })
            .collect();
        (sessions, state)
    }

    #[tokio::test]
    async fn repeated_enable_and_disable_are_idempotent() {
        let (mut sessions, state) = sessions(&["renderer"]);
        assert!(
            enable(&mut sessions, SessionMode::Persistent)
                .await
                .unwrap()
                .targets[0]
                .changed
        );
        assert!(
            !enable(&mut sessions, SessionMode::Persistent)
                .await
                .unwrap()
                .targets[0]
                .changed
        );
        assert!(disable(&mut sessions).await.unwrap().targets[0].changed);
        assert!(!disable(&mut sessions).await.unwrap().targets[0].changed);

        let state = state.lock().unwrap();
        assert_eq!(state.enable_calls["renderer"], 1);
        assert_eq!(state.disable_calls["renderer"], 1);
    }

    #[test]
    fn healthy_stale_data_does_not_trigger_reinjection() {
        let health = CodexTargetHealth::new("renderer", CodexRefitState::Stale, true);
        let health = health.with_session_mode(SessionMode::Once);
        assert!(is_usable_enabled(&health, SessionMode::Once));
        assert!(!is_usable_enabled(&health, SessionMode::Persistent));
        assert!(!is_usable_enabled(
            &CodexTargetHealth::new("renderer", CodexRefitState::Stale, false,),
            SessionMode::Once
        ));
    }

    #[tokio::test]
    async fn partial_enable_failure_rolls_back_prior_target() {
        let (mut sessions, state) = sessions(&["first", "second"]);
        state.lock().unwrap().fail_enable = Some("second".to_owned());
        let error = enable(&mut sessions, SessionMode::Persistent)
            .await
            .unwrap_err();
        assert_eq!(error.code(), CodexRefitErrorCode::InjectionFailed);

        let state = state.lock().unwrap();
        assert!(!state.enabled["first"]);
        assert_eq!(state.disable_calls["first"], 1);
    }

    #[tokio::test]
    async fn once_enable_reports_the_ephemeral_session_mode() {
        let (mut sessions, _) = sessions(&["renderer"]);
        let report = enable(&mut sessions, SessionMode::Once).await.unwrap();
        assert_eq!(report.session_mode, Some(SessionMode::Once));
        assert_eq!(report.targets[0].session_mode, Some(SessionMode::Once));
    }

    #[tokio::test]
    async fn disable_continues_cleaning_other_targets_after_a_failure() {
        let (mut sessions, state) = sessions(&["first", "second"]);
        {
            let mut state = state.lock().unwrap();
            state.enabled.insert("first".to_owned(), true);
            state.enabled.insert("second".to_owned(), true);
            state.fail_disable = Some("first".to_owned());
        }

        let error = disable(&mut sessions).await.unwrap_err();
        assert_eq!(error.code(), CodexRefitErrorCode::CleanupFailed);
        let state = state.lock().unwrap();
        assert!(state.enabled["first"]);
        assert!(!state.enabled["second"]);
        assert_eq!(state.disable_calls["second"], 1);
    }
}
