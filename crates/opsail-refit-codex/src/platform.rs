use std::path::PathBuf;

use crate::error::{CodexRefitError, CodexRefitErrorCode};

#[derive(Debug, Clone)]
pub(crate) struct RuntimeIdentity {
    pub listener_pids: Vec<u32>,
}

#[cfg(target_os = "macos")]
mod imp {
    use std::collections::BTreeSet;
    use std::fs;
    use std::os::unix::process::CommandExt as _;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    use super::*;

    const APP_BUNDLE: &str = "/Applications/ChatGPT.app";
    const APP_IDENTIFIER: &str = "com.openai.codex";
    const APP_EXECUTABLE: &str = "ChatGPT";
    const APP_EXECUTABLE_PATH: &str = "/Applications/ChatGPT.app/Contents/MacOS/ChatGPT";
    const TEAM_IDENTIFIER: &str = "2DC432GLL2";
    const MAX_PARENT_DEPTH: usize = 32;

    pub(super) fn default_state_dir() -> Result<PathBuf, CodexRefitError> {
        let home = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| {
                platform_error(
                    CodexRefitErrorCode::StateIo,
                    "could not resolve the current macOS home directory",
                )
            })?;
        Ok(home
            .join("Library")
            .join("Application Support")
            .join("opsail")
            .join("refit")
            .join("codex"))
    }

    pub(super) fn validate_app() -> Result<(), CodexRefitError> {
        let bundle = Path::new(APP_BUNDLE);
        let bundle_metadata = fs::symlink_metadata(bundle).map_err(|_| {
            platform_error(
                CodexRefitErrorCode::TargetNotFound,
                "the supported ChatGPT app was not found in /Applications",
            )
        })?;
        if bundle_metadata.file_type().is_symlink() || !bundle_metadata.is_dir() {
            return Err(validation_error(
                "the supported ChatGPT app path is not a regular application bundle",
            ));
        }
        let info = bundle.join("Contents/Info.plist");
        if !info.is_file() {
            return Err(platform_error(
                CodexRefitErrorCode::TargetNotFound,
                "the supported ChatGPT app was not found in /Applications",
            ));
        }
        let identifier = command_text(
            "/usr/bin/plutil",
            &[
                "-extract",
                "CFBundleIdentifier",
                "raw",
                "-o",
                "-",
                path_text(&info)?,
            ],
        )?;
        if identifier != APP_IDENTIFIER {
            return Err(validation_error(
                "the ChatGPT bundle identifier is not recognized",
            ));
        }
        let executable_name = command_text(
            "/usr/bin/plutil",
            &[
                "-extract",
                "CFBundleExecutable",
                "raw",
                "-o",
                "-",
                path_text(&info)?,
            ],
        )?;
        if executable_name != APP_EXECUTABLE {
            return Err(validation_error(
                "the ChatGPT bundle executable is not recognized",
            ));
        }
        let executable = bundle.join("Contents/MacOS").join(APP_EXECUTABLE);
        if !executable.is_file() {
            return Err(validation_error("the signed ChatGPT executable is missing"));
        }
        let requirement = format!(
            "=anchor apple generic and certificate leaf[subject.OU] = \"{TEAM_IDENTIFIER}\""
        );
        let status = Command::new("/usr/bin/codesign")
            .args(["--verify", "--strict", "--test-requirement", &requirement])
            .arg(bundle)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| validation_error("could not verify the ChatGPT signature"))?;
        if !status.success() {
            return Err(validation_error("the ChatGPT signature is not valid"));
        }
        let signature = Command::new("/usr/bin/codesign")
            .args(["-dv", "--verbose=4"])
            .arg(bundle)
            .output()
            .map_err(|_| validation_error("could not inspect the ChatGPT signature"))?;
        if !signature.status.success() || signature.stderr.len() > 64 * 1024 {
            return Err(validation_error(
                "the ChatGPT signature details are unavailable",
            ));
        }
        let details = String::from_utf8_lossy(&signature.stderr);
        let team = details
            .lines()
            .find_map(|line| line.strip_prefix("TeamIdentifier="))
            .unwrap_or_default();
        if team != TEAM_IDENTIFIER {
            return Err(validation_error(
                "the ChatGPT signing team is not recognized",
            ));
        }
        Ok(())
    }

    pub(super) fn validate_runtime(port: u16) -> Result<RuntimeIdentity, CodexRefitError> {
        validate_app()?;
        let expected_executable = fs::canonicalize(
            Path::new(APP_BUNDLE)
                .join("Contents/MacOS")
                .join(APP_EXECUTABLE),
        )
        .map_err(|_| validation_error("could not resolve the ChatGPT executable"))?;
        let current_uid = command_text("/usr/bin/id", &["-u"])?
            .parse::<u32>()
            .map_err(|_| validation_error("could not determine the current user"))?;
        let mut listener_pids = listener_pids(port)?;
        if listener_pids.is_empty() {
            return Err(platform_error(
                CodexRefitErrorCode::SessionUnavailable,
                format!("no loopback debug listener is available on port {port}"),
            ));
        }
        listener_pids.sort_unstable();
        listener_pids.dedup();
        for pid in &listener_pids {
            validate_listener_owner(*pid, current_uid, &expected_executable)?;
        }
        Ok(RuntimeIdentity { listener_pids })
    }

    pub(super) fn app_is_running() -> Result<bool, CodexRefitError> {
        validate_app()?;
        let current_uid = command_text("/usr/bin/id", &["-u"])?
            .parse::<u32>()
            .map_err(|_| launch_error("could not determine the current user"))?;
        let expected_executable = fs::canonicalize(APP_EXECUTABLE_PATH)
            .map_err(|_| launch_error("could not resolve the ChatGPT executable"))?;
        let output = Command::new("/usr/bin/pgrep")
            .args([
                "-f",
                "^/Applications/ChatGPT[.]app/Contents/MacOS/ChatGPT( |$)",
            ])
            .output()
            .map_err(|_| launch_error("could not inspect running ChatGPT processes"))?;
        if output.status.code() == Some(1) {
            return Ok(false);
        }
        if !output.status.success() || output.stdout.len() > 64 * 1024 {
            return Err(launch_error("could not inspect running ChatGPT processes"));
        }
        let text = String::from_utf8(output.stdout)
            .map_err(|_| launch_error("the ChatGPT process list is invalid"))?;
        for pid in text
            .lines()
            .filter_map(|line| line.trim().parse::<u32>().ok())
        {
            if process_number(pid, "uid").ok() != Some(current_uid) {
                continue;
            }
            if process_executable(pid)
                .and_then(|path| fs::canonicalize(path).ok())
                .as_deref()
                == Some(expected_executable.as_path())
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) fn port_has_listener(port: u16) -> Result<bool, CodexRefitError> {
        let output = Command::new("/usr/sbin/lsof")
            .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fp"])
            .output()
            .map_err(|_| port_error(port))?;
        if output.stdout.len() > 64 * 1024 {
            return Err(port_error(port));
        }
        if output.status.success() {
            return Ok(output.stdout.contains(&b'p'));
        }
        if output.status.code() == Some(1) && output.stdout.is_empty() {
            return Ok(false);
        }
        Err(port_error(port))
    }

    pub(super) fn launch_app(port: u16) -> Result<u32, CodexRefitError> {
        validate_app()?;
        let mut command = launch_command(port);
        let mut child = command
            .spawn()
            .map_err(|_| launch_error("could not start the validated ChatGPT executable"))?;
        let pid = child.id();
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(pid)
    }

    fn launch_command(port: u16) -> Command {
        let port_argument = format!("--remote-debugging-port={port}");
        let mut command = Command::new(APP_EXECUTABLE_PATH);
        command
            .args(["--remote-debugging-address=127.0.0.1", &port_argument])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        command
    }

    pub(super) fn validate_launched_runtime(
        identity: &RuntimeIdentity,
        launched_pid: u32,
    ) -> Result<(), CodexRefitError> {
        let current_uid = command_text("/usr/bin/id", &["-u"])?
            .parse::<u32>()
            .map_err(|_| validation_error("could not determine the current user"))?;
        let expected_executable = fs::canonicalize(APP_EXECUTABLE_PATH)
            .map_err(|_| validation_error("could not resolve the ChatGPT executable"))?;
        if process_number(launched_pid, "uid")? != current_uid
            || process_executable(launched_pid)
                .and_then(|path| fs::canonicalize(path).ok())
                .as_deref()
                != Some(expected_executable.as_path())
        {
            return Err(validation_error(
                "the launched ChatGPT process identity changed before CDP validation",
            ));
        }
        if identity
            .listener_pids
            .iter()
            .all(|pid| process_descends_from(*pid, launched_pid).unwrap_or(false))
        {
            Ok(())
        } else {
            Err(validation_error(
                "the CDP listener is not owned by the ChatGPT process started by Opsail",
            ))
        }
    }

    pub(super) fn stop_managed_process(pid: u32) -> Result<(), CodexRefitError> {
        if pid <= 1 || pid == std::process::id() {
            return Err(cleanup_error(
                "refusing to signal an invalid manager process",
            ));
        }
        let current_uid = command_text("/usr/bin/id", &["-u"])?
            .parse::<u32>()
            .map_err(|_| cleanup_error("could not determine the current user"))?;
        if process_number(pid, "uid")? != current_uid {
            return Err(cleanup_error(
                "the persistent manager belongs to another user",
            ));
        }
        let current_executable = std::env::current_exe()
            .ok()
            .and_then(|path| fs::canonicalize(path).ok())
            .ok_or_else(|| cleanup_error("could not resolve the current Opsail executable"))?;
        let managed_executable = process_executable(pid)
            .and_then(|path| fs::canonicalize(path).ok())
            .ok_or_else(|| cleanup_error("could not resolve the persistent manager executable"))?;
        if managed_executable != current_executable {
            return Err(cleanup_error(
                "the persistent manager executable does not match Opsail",
            ));
        }
        let command = command_text(
            "/bin/ps",
            &["-ww", "-p", &pid.to_string(), "-o", "command="],
        )?;
        if !is_managed_enable_command(&command) {
            return Err(cleanup_error(
                "the recorded process is not a Codex usage manager",
            ));
        }
        let status = Command::new("/bin/kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .map_err(|_| cleanup_error("could not stop the persistent Opsail manager"))?;
        if status.success() {
            Ok(())
        } else {
            Err(cleanup_error(
                "could not stop the persistent Opsail manager",
            ))
        }
    }

    fn is_managed_enable_command(command: &str) -> bool {
        let arguments = command.split_whitespace().collect::<Vec<_>>();
        arguments
            .windows(4)
            .any(|values| values == ["refit", "codex", "enable", "usage"])
    }

    fn process_descends_from(mut pid: u32, ancestor: u32) -> Result<bool, CodexRefitError> {
        for _ in 0..MAX_PARENT_DEPTH {
            if pid == ancestor {
                return Ok(true);
            }
            if pid <= 1 {
                return Ok(false);
            }
            let parent = process_number(pid, "ppid")?;
            if parent <= 1 || parent == pid {
                return Ok(false);
            }
            pid = parent;
        }
        Ok(false)
    }

    fn listener_pids(port: u16) -> Result<Vec<u32>, CodexRefitError> {
        let output = Command::new("/usr/sbin/lsof")
            .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fpn"])
            .output()
            .map_err(|_| validation_error("could not inspect the loopback listener"))?;
        if !output.status.success() && output.stdout.is_empty() {
            return Ok(Vec::new());
        }
        if output.stdout.len() > 64 * 1024 {
            return Err(validation_error("the loopback listener list is too large"));
        }
        let text = String::from_utf8(output.stdout)
            .map_err(|_| validation_error("the loopback listener list is invalid"))?;
        parse_listener_pids(&text, port)
    }

    fn parse_listener_pids(text: &str, port: u16) -> Result<Vec<u32>, CodexRefitError> {
        let mut values = BTreeSet::new();
        let mut current_pid = None;
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
            if let Some(value) = line.strip_prefix('p') {
                current_pid =
                    Some(value.parse::<u32>().map_err(|_| {
                        validation_error("the loopback listener identity is invalid")
                    })?);
                continue;
            }
            let Some(binding) = line.strip_prefix('n') else {
                continue;
            };
            let pid = current_pid
                .ok_or_else(|| validation_error("the loopback listener identity is incomplete"))?;
            if !is_loopback_binding(binding, port) {
                return Err(validation_error(
                    "the debug listener is not bound exclusively to a loopback address",
                ));
            }
            values.insert(pid);
        }
        Ok(values.into_iter().collect())
    }

    fn is_loopback_binding(value: &str, port: u16) -> bool {
        value == format!("127.0.0.1:{port}")
    }

    fn validate_listener_owner(
        listener_pid: u32,
        current_uid: u32,
        expected_executable: &Path,
    ) -> Result<(), CodexRefitError> {
        let listener_uid = process_number(listener_pid, "uid")?;
        if listener_uid != current_uid {
            return Err(validation_error(
                "the debug listener belongs to another user",
            ));
        }
        let mut current = listener_pid;
        for _ in 0..MAX_PARENT_DEPTH {
            if let Some(path) = process_executable(current)
                && fs::canonicalize(path).ok().as_deref() == Some(expected_executable)
            {
                return Ok(());
            }
            if current <= 1 {
                break;
            }
            let parent = process_number(current, "ppid")?;
            if parent <= 1 || parent == current {
                break;
            }
            current = parent;
        }
        Err(validation_error(
            "the loopback debug listener is not owned by the signed ChatGPT process tree",
        ))
    }

    fn process_executable(pid: u32) -> Option<PathBuf> {
        let output = Command::new("/usr/sbin/lsof")
            .args(["-a", "-p", &pid.to_string(), "-d", "txt", "-Fn"])
            .output()
            .ok()?;
        String::from_utf8(output.stdout)
            .ok()?
            .lines()
            .find_map(|line| line.strip_prefix('n'))
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    }

    fn process_number(pid: u32, field: &str) -> Result<u32, CodexRefitError> {
        command_text(
            "/bin/ps",
            &["-p", &pid.to_string(), "-o", &format!("{field}=")],
        )?
        .parse::<u32>()
        .map_err(|_| validation_error("a process identity changed during validation"))
    }

    fn path_text(path: &Path) -> Result<&str, CodexRefitError> {
        path.to_str()
            .ok_or_else(|| validation_error("the ChatGPT app path is not valid UTF-8"))
    }

    fn command_text(program: &str, arguments: &[&str]) -> Result<String, CodexRefitError> {
        let output = Command::new(program)
            .args(arguments)
            .output()
            .map_err(|_| validation_error("a macOS identity check could not run"))?;
        if !output.status.success() || output.stdout.len() > 64 * 1024 {
            return Err(validation_error("a macOS identity check failed"));
        }
        String::from_utf8(output.stdout)
            .map(|value| value.trim().to_owned())
            .map_err(|_| validation_error("a macOS identity check returned invalid output"))
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

    fn port_error(port: u16) -> CodexRefitError {
        platform_error(
            CodexRefitErrorCode::PortUnavailable,
            format!("could not preflight loopback CDP port {port}"),
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn listener_parser_accepts_only_explicit_loopback_bindings() {
            assert_eq!(
                parse_listener_pids("p41\nn127.0.0.1:55321\n", 55321).unwrap(),
                [41]
            );
            for binding in [
                "*:55321",
                "0.0.0.0:55321",
                "[::1]:55321",
                "192.0.2.10:55321",
            ] {
                let error = parse_listener_pids(&format!("p41\nn{binding}\n"), 55321).unwrap_err();
                assert_eq!(error.code(), CodexRefitErrorCode::TargetValidationFailed);
            }
        }

        #[test]
        fn managed_command_detection_requires_the_exact_refit_path() {
            assert!(is_managed_enable_command(
                "/usr/local/bin/opsail refit codex enable usage --launch"
            ));
            assert!(!is_managed_enable_command(
                "/usr/local/bin/opsail refit codex status"
            ));
        }

        #[test]
        fn launch_command_uses_the_validated_executable_and_loopback_flags() {
            let command = launch_command(55321);
            assert_eq!(command.get_program(), APP_EXECUTABLE_PATH);
            assert_eq!(
                command.get_args().collect::<Vec<_>>(),
                [
                    "--remote-debugging-address=127.0.0.1",
                    "--remote-debugging-port=55321",
                ]
            );
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;

    pub(super) fn default_state_dir() -> Result<PathBuf, CodexRefitError> {
        Err(unsupported())
    }

    pub(super) fn validate_app() -> Result<(), CodexRefitError> {
        Err(unsupported())
    }

    pub(super) fn validate_runtime(_port: u16) -> Result<RuntimeIdentity, CodexRefitError> {
        Err(unsupported())
    }

    pub(super) fn app_is_running() -> Result<bool, CodexRefitError> {
        Err(unsupported())
    }

    pub(super) fn port_has_listener(_port: u16) -> Result<bool, CodexRefitError> {
        Ok(false)
    }

    pub(super) fn launch_app(_port: u16) -> Result<u32, CodexRefitError> {
        Err(unsupported())
    }

    pub(super) fn validate_launched_runtime(
        _identity: &RuntimeIdentity,
        _launched_pid: u32,
    ) -> Result<(), CodexRefitError> {
        Err(unsupported())
    }

    pub(super) fn stop_managed_process(_pid: u32) -> Result<(), CodexRefitError> {
        Err(unsupported())
    }

    fn unsupported() -> CodexRefitError {
        platform_error(
            CodexRefitErrorCode::Unsupported,
            "the Codex refit adapter currently supports only the verified macOS ChatGPT app",
        )
    }
}

pub(crate) fn default_state_dir() -> Result<PathBuf, CodexRefitError> {
    imp::default_state_dir()
}

pub(crate) const fn is_supported() -> bool {
    cfg!(target_os = "macos")
}

pub(crate) fn validate_app() -> Result<(), CodexRefitError> {
    imp::validate_app()
}

pub(crate) fn validate_runtime(port: u16) -> Result<RuntimeIdentity, CodexRefitError> {
    imp::validate_runtime(port)
}

pub(crate) fn app_is_running() -> Result<bool, CodexRefitError> {
    imp::app_is_running()
}

pub(crate) fn launch_app(port: u16) -> Result<u32, CodexRefitError> {
    imp::launch_app(port)
}

pub(crate) fn validate_launched_runtime(
    identity: &RuntimeIdentity,
    launched_pid: u32,
) -> Result<(), CodexRefitError> {
    imp::validate_launched_runtime(identity, launched_pid)
}

pub(crate) fn stop_managed_process(pid: u32) -> Result<(), CodexRefitError> {
    imp::stop_managed_process(pid)
}

pub(crate) fn loopback_port_available(port: u16) -> Result<bool, CodexRefitError> {
    use std::io::ErrorKind;
    use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};

    if imp::port_has_listener(port)? {
        return Ok(false);
    }
    match TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)) {
        Ok(listener) => {
            drop(listener);
            Ok(true)
        }
        Err(error) if error.kind() == ErrorKind::AddrInUse => Ok(false),
        Err(_) => Err(platform_error(
            CodexRefitErrorCode::PortUnavailable,
            format!("could not preflight loopback CDP port {port}"),
        )),
    }
}

pub(crate) fn revalidate_runtime(
    port: u16,
    previous: &RuntimeIdentity,
) -> Result<(), CodexRefitError> {
    let current = validate_runtime(port)?;
    if current.listener_pids != previous.listener_pids {
        return Err(platform_error(
            CodexRefitErrorCode::TargetValidationFailed,
            "the debug listener identity changed during renderer validation",
        ));
    }
    Ok(())
}

fn platform_error(code: CodexRefitErrorCode, message: impl Into<String>) -> CodexRefitError {
    CodexRefitError::new(code, message)
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;

    #[test]
    fn loopback_port_preflight_detects_conflicts_without_starting_an_application() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(!loopback_port_available(port).unwrap());
        drop(listener);
        assert!(loopback_port_available(port).unwrap());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn port_preflight_rejects_an_ipv6_listener_on_the_same_port() {
        let listener = TcpListener::bind("[::1]:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(!loopback_port_available(port).unwrap());
    }
}
