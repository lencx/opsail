use std::collections::BTreeSet;
use std::fs;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use super::windows_native::{self, ProcessIdentity, StoreAppCriteria};

use super::*;
use crate::atomic_file::{ensure_no_windows_reparse_points, is_symlink_or_windows_reparse_point};

const PACKAGE_NAME: &str = "OpenAI.Codex";
const PACKAGE_FAMILY_NAME: &str = "OpenAI.Codex_2p2nqsd0c76g0";
const APPLICATION_ID: &str = "App";

pub(super) fn default_state_dir() -> Result<PathBuf, CodexRefitError> {
    let mut path = windows_native::local_app_data_dir().map_err(|_| {
        platform_error(
            CodexRefitErrorCode::StateIo,
            "could not resolve the current Windows Local AppData directory",
        )
    })?;
    for component in ["opsail", "refit", "codex"] {
        path.push(component);
        ensure_no_windows_reparse_points(&path)
            .map_err(|_| state_path_error("could not verify the Windows refit state path"))?;
        match fs::symlink_metadata(&path) {
            Ok(metadata)
                if is_symlink_or_windows_reparse_point(&metadata) || !metadata.is_dir() =>
            {
                return Err(state_path_error(
                    "the Windows refit state path is not a regular directory",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {
                fs::create_dir(&path).map_err(|_| {
                    state_path_error("could not create the Windows refit state directory")
                })?;
            }
            Err(_) => {
                return Err(state_path_error(
                    "could not inspect the Windows refit state directory",
                ));
            }
        }
        ensure_no_windows_reparse_points(&path)
            .map_err(|_| state_path_error("could not verify the Windows refit state path"))?;
        windows_native::protect_current_user_path(
            &path,
            super::windows_native::PrivatePathKind::Directory,
        )
        .map_err(|_| state_path_error("could not protect the Windows refit state directory"))?;
    }
    Ok(path)
}

pub(super) fn validate_app() -> Result<ValidatedAppIdentity, CodexRefitError> {
    let criteria = StoreAppCriteria {
        package_name: PACKAGE_NAME,
        package_family_name: PACKAGE_FAMILY_NAME,
        application_id: APPLICATION_ID,
    };
    let app = windows_native::find_store_app(criteria).map_err(|_| {
        validation_error("the registered ChatGPT Store package failed identity validation")
    })?;
    let app = app.ok_or_else(|| {
        platform_error(
            CodexRefitErrorCode::TargetNotFound,
            "the supported ChatGPT Store application is not installed for the current user",
        )
    })?;
    Ok(ValidatedAppIdentity {
        executable: app.executable,
        package_full_name: app.package_full_name,
        package_family_name: app.package_family_name,
        app_user_model_id: app.app_user_model_id,
        user_sid: app.user_sid,
    })
}

pub(super) fn validate_runtime(
    port: u16,
    app: &ValidatedAppIdentity,
) -> Result<RuntimeIdentity, CodexRefitError> {
    let listeners = windows_native::tcp_listeners(port)
        .map_err(|_| validation_error("could not inspect Windows TCP listeners"))?;
    if listeners.is_empty() {
        return Err(platform_error(
            CodexRefitErrorCode::SessionUnavailable,
            format!("no loopback debug listener is available on port {port}"),
        ));
    }
    if listeners
        .iter()
        .any(|listener| listener.address != IpAddr::V4(Ipv4Addr::LOCALHOST))
    {
        return Err(validation_error(
            "the debug listener is not bound exclusively to 127.0.0.1",
        ));
    }

    let listener_pids = listeners
        .iter()
        .map(|listener| listener.pid)
        .collect::<BTreeSet<_>>();
    let mut listener_processes = Vec::with_capacity(listener_pids.len());
    for pid in &listener_pids {
        let process = windows_native::process_identity(*pid)
            .map_err(|_| validation_error("could not inspect the debug listener process"))?;
        validate_app_process(&process, app)?;
        listener_processes.push(process);
    }
    listener_processes.sort_by_key(|process| (process.created_at, process.pid));
    Ok(RuntimeIdentity {
        listener_pids: listener_pids.into_iter().collect(),
        listener_processes,
    })
}

pub(super) fn app_is_running(app: &ValidatedAppIdentity) -> Result<bool, CodexRefitError> {
    let processes = windows_native::package_processes(&app.package_family_name)
        .map_err(|_| launch_error("could not inspect running ChatGPT processes"))?;
    Ok(processes
        .iter()
        .any(|process| same_identity_text(&process.user_sid, &app.user_sid)))
}

pub(super) fn port_has_listener(port: u16) -> Result<bool, CodexRefitError> {
    windows_native::tcp_listeners(port)
        .map(|listeners| !listeners.is_empty())
        .map_err(|_| {
            platform_error(
                CodexRefitErrorCode::PortUnavailable,
                format!("could not preflight loopback CDP port {port}"),
            )
        })
}

pub(super) fn launch_app(
    port: u16,
    app: &ValidatedAppIdentity,
) -> Result<LaunchedProcess, CodexRefitError> {
    let process = windows_native::activate_application(
        &app.app_user_model_id,
        &[
            "--remote-debugging-address=127.0.0.1".to_owned(),
            format!("--remote-debugging-port={port}"),
        ],
    )
    .map_err(|_| launch_error("Windows could not activate the validated ChatGPT application"))?;
    validate_app_process(process.identity(), app).map_err(|_| {
        launch_error("the activated ChatGPT process did not match the validated Store package")
    })?;

    let identity = LaunchedProcessIdentity {
        pid: process.identity().pid,
        created_at: process.identity().created_at,
    };
    let (exit_tx, exit) = watch::channel(false);
    std::thread::spawn(move || {
        let _ = process.wait();
        let _ = exit_tx.send(true);
    });
    Ok(LaunchedProcess {
        identity,
        exit,
        #[cfg(test)]
        _test_guard: None,
    })
}

pub(super) fn validate_launched_runtime(
    identity: &RuntimeIdentity,
    app: &ValidatedAppIdentity,
    launched: LaunchedProcessIdentity,
) -> Result<(), CodexRefitError> {
    let process = windows_native::process_identity(launched.pid).map_err(|_| {
        validation_error("the launched ChatGPT process exited before CDP validation")
    })?;
    validate_launched_runtime_identity(identity, app, launched, &process)
}

fn validate_launched_runtime_identity(
    identity: &RuntimeIdentity,
    app: &ValidatedAppIdentity,
    launched: LaunchedProcessIdentity,
    process: &ProcessIdentity,
) -> Result<(), CodexRefitError> {
    if process.created_at != launched.created_at {
        return Err(validation_error(
            "the launched ChatGPT process identity changed before CDP validation",
        ));
    }
    validate_app_process(process, app)?;
    if identity.listener_processes.as_slice() != std::slice::from_ref(process) {
        return Err(validation_error(
            "the CDP listener is not owned by the exact ChatGPT process started by Opsail",
        ));
    }
    Ok(())
}

pub(super) fn current_process_instance_id() -> Result<Option<u64>, CodexRefitError> {
    windows_native::process_instance_id(std::process::id())
        .map(Some)
        .map_err(|_| {
            platform_error(
                CodexRefitErrorCode::StateIo,
                "could not record the Windows manager process identity",
            )
        })
}

pub(super) fn stop_managed_process(
    pid: u32,
    created_at: Option<u64>,
) -> Result<(), CodexRefitError> {
    if pid <= 1 || pid == std::process::id() {
        return Err(cleanup_error(
            "refusing to stop an invalid Windows manager process",
        ));
    }
    let managed = windows_native::process_identity(pid)
        .map_err(|_| cleanup_error("could not inspect the persistent Opsail manager"))?;
    let expected_created_at = created_at.ok_or_else(|| {
        cleanup_error("the persistent Windows manager marker has no process creation time")
    })?;
    let current = windows_native::process_identity(std::process::id())
        .map_err(|_| cleanup_error("could not inspect the current Opsail process"))?;
    let same_executable = windows_native::same_file(&managed.executable, &current.executable)
        .map_err(|_| cleanup_error("could not compare the persistent manager executable"))?;
    if managed.created_at != expected_created_at
        || !same_executable
        || !same_identity_text(&managed.user_sid, &current.user_sid)
        || managed.package_full_name.is_some()
        || managed.package_family_name.is_some()
        || managed.app_user_model_id.is_some()
    {
        return Err(cleanup_error(
            "the persistent manager identity does not match Opsail",
        ));
    }
    windows_native::terminate_process(&managed)
        .map_err(|_| cleanup_error("could not stop the persistent Opsail manager"))
}

fn validate_app_process(
    process: &ProcessIdentity,
    app: &ValidatedAppIdentity,
) -> Result<(), CodexRefitError> {
    let package_full_name = process.package_full_name.as_deref().unwrap_or_default();
    let package_family_name = process.package_family_name.as_deref().unwrap_or_default();
    let app_user_model_id = process.app_user_model_id.as_deref().unwrap_or_default();
    let same_executable = windows_native::same_file(&process.executable, &app.executable)
        .map_err(|_| validation_error("could not compare the Windows process executable"))?;
    if process.created_at == 0
        || !same_executable
        || !same_identity_text(&process.user_sid, &app.user_sid)
        || !same_identity_text(package_full_name, &app.package_full_name)
        || !same_identity_text(package_family_name, &app.package_family_name)
        || !same_identity_text(app_user_model_id, &app.app_user_model_id)
    {
        return Err(validation_error(
            "the Windows process does not match the validated ChatGPT Store identity",
        ));
    }
    Ok(())
}

fn same_identity_text(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn validation_error(message: impl Into<String>) -> CodexRefitError {
    platform_error(CodexRefitErrorCode::TargetValidationFailed, message)
}

fn launch_error(message: impl Into<String>) -> CodexRefitError {
    platform_error(CodexRefitErrorCode::LaunchFailed, message)
}

fn cleanup_error(message: impl Into<String>) -> CodexRefitError {
    platform_error(CodexRefitErrorCode::CleanupFailed, message)
}

fn state_path_error(message: impl Into<String>) -> CodexRefitError {
    platform_error(CodexRefitErrorCode::StateIo, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> ValidatedAppIdentity {
        let mut app = ValidatedAppIdentity::for_test();
        app.executable = std::env::current_exe().unwrap();
        app
    }

    fn process() -> ProcessIdentity {
        let app = app();
        ProcessIdentity {
            pid: 42,
            executable: app.executable.clone(),
            user_sid: app.user_sid.to_ascii_lowercase(),
            created_at: 100,
            package_full_name: Some(app.package_full_name.to_ascii_lowercase()),
            package_family_name: Some(app.package_family_name.to_ascii_lowercase()),
            app_user_model_id: Some(app.app_user_model_id.to_ascii_lowercase()),
        }
    }

    #[test]
    fn process_validation_binds_every_store_identity_field() {
        let app = app();
        let process = process();
        validate_app_process(&process, &app).unwrap();

        let mut wrong = process;
        wrong.created_at = 0;
        assert_eq!(
            validate_app_process(&wrong, &app).unwrap_err().code(),
            CodexRefitErrorCode::TargetValidationFailed
        );
    }

    #[test]
    fn launched_runtime_rejects_an_unrelated_later_store_process() {
        let app = app();
        let launched_process = process();
        let mut unrelated = launched_process.clone();
        unrelated.pid += 1;
        unrelated.created_at += 1;
        let identity = RuntimeIdentity {
            listener_pids: vec![unrelated.pid],
            listener_processes: vec![unrelated],
        };
        let launched = LaunchedProcessIdentity {
            pid: launched_process.pid,
            created_at: launched_process.created_at,
        };

        assert_eq!(
            validate_launched_runtime_identity(&identity, &app, launched, &launched_process)
                .unwrap_err()
                .code(),
            CodexRefitErrorCode::TargetValidationFailed
        );
    }
}
