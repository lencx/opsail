use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tempfile::TempDir;
use tokio::process::Command;
use tokio::time::{sleep, timeout};

use crate::ChromeError;

#[cfg(not(windows))]
type ChromeChild = tokio::process::Child;
#[cfg(windows)]
type ChromeChild = Box<dyn process_wrap::tokio::ChildWrapper>;

const DEVTOOLS_ACTIVE_PORT: &str = "DevToolsActivePort";
const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(50);
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(750);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

pub(crate) struct LaunchedChrome {
    child: Option<ChromeChild>,
    endpoint: Option<String>,
    profile: Option<TempDir>,
}

impl LaunchedChrome {
    pub(crate) fn endpoint(&self) -> &str {
        self.endpoint
            .as_deref()
            .expect("a launched Chrome always has a DevTools endpoint")
    }

    pub(crate) async fn shutdown(mut self) -> Result<(), ChromeError> {
        let mut stopped = true;
        if let Some(mut child) = self.child.take() {
            stopped = if matches!(
                timeout(GRACEFUL_SHUTDOWN_TIMEOUT, child.wait()).await,
                Ok(Ok(_))
            ) {
                true
            } else {
                terminate_and_wait(&mut child).await
            };
        }
        let removed = self
            .profile
            .take()
            .is_none_or(|profile| profile.close().is_ok());
        (stopped && removed)
            .then_some(())
            .ok_or(ChromeError::ChromeCleanup)
    }
}

impl Drop for LaunchedChrome {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            terminate_and_wait_blocking(&mut child);
        }
    }
}

pub(crate) async fn launch(
    explicit: Option<&Path>,
    startup_timeout: Duration,
) -> Result<LaunchedChrome, ChromeError> {
    let executable = find_chrome(explicit).ok_or(ChromeError::ChromeNotFound)?;
    let profile = create_profile()?;

    let args = chrome_args(profile.path());

    let mut command = Command::new(executable);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let child = spawn_chrome(command)?;
    let mut chrome = LaunchedChrome {
        child: Some(child),
        endpoint: None,
        profile: Some(profile),
    };
    let endpoint = wait_for_devtools_endpoint(
        chrome
            .child
            .as_mut()
            .expect("a pending Chrome owns its child process"),
        chrome
            .profile
            .as_ref()
            .expect("a pending Chrome owns its profile")
            .path(),
        startup_timeout,
    )
    .await;
    match endpoint {
        Ok(endpoint) => {
            chrome.endpoint = Some(endpoint);
            Ok(chrome)
        }
        Err(error) => {
            let _ = chrome.shutdown().await;
            Err(error)
        }
    }
}

fn chrome_args(user_data_dir_path: &Path) -> [OsString; 8] {
    let mut user_data_dir = OsString::from("--user-data-dir=");
    user_data_dir.push(user_data_dir_path);
    [
        OsString::from("--headless"),
        OsString::from("--remote-debugging-address=127.0.0.1"),
        OsString::from("--remote-debugging-port=0"),
        user_data_dir,
        OsString::from("--no-first-run"),
        OsString::from("--no-default-browser-check"),
        OsString::from("--disable-background-networking"),
        OsString::from("about:blank"),
    ]
}

async fn wait_for_devtools_endpoint(
    child: &mut ChromeChild,
    user_data_dir: &Path,
    startup_timeout: Duration,
) -> Result<String, ChromeError> {
    let active_port_path = user_data_dir.join(DEVTOOLS_ACTIVE_PORT);
    let wait = async {
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return Err(ChromeError::ChromeExited),
                Ok(None) => {}
                Err(_) => return Err(ChromeError::ChromeLaunch),
            }

            match tokio::fs::read_to_string(&active_port_path).await {
                Ok(contents) => {
                    if let Some(endpoint) = parse_devtools_active_port(&contents) {
                        return Ok(endpoint);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(ChromeError::ChromeLaunch),
            }

            sleep(STARTUP_POLL_INTERVAL).await;
        }
    };

    timeout(startup_timeout, wait)
        .await
        .map_err(|_| ChromeError::ChromeStartupTimeout)?
}

fn parse_devtools_active_port(contents: &str) -> Option<String> {
    let mut lines = contents.lines();
    let port = lines.next()?.trim().parse::<u16>().ok()?;
    if port == 0 {
        return None;
    }

    let websocket_path = lines.next()?.trim();
    if !websocket_path.starts_with("/devtools/") {
        return None;
    }

    Some(format!("ws://127.0.0.1:{port}{websocket_path}"))
}

async fn terminate_and_wait(child: &mut ChromeChild) -> bool {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return true;
    }

    let _ = child.start_kill();
    matches!(timeout(SHUTDOWN_TIMEOUT, child.wait()).await, Ok(Ok(_)))
}

fn terminate_and_wait_blocking(child: &mut ChromeChild) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }

    let _ = child.start_kill();
    let deadline = std::time::Instant::now() + SHUTDOWN_TIMEOUT;
    while std::time::Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => return,
        }
    }
}

fn create_profile() -> Result<TempDir, ChromeError> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("opsail-chrome-");
    match env::var_os("OPSAIL_CHROME_TEMP_ROOT").filter(|value| !value.is_empty()) {
        Some(root) => builder.tempdir_in(root),
        None => builder.tempdir(),
    }
    .map_err(|_| ChromeError::ChromeLaunch)
}

#[cfg(not(windows))]
fn spawn_chrome(mut command: Command) -> Result<ChromeChild, ChromeError> {
    command.kill_on_drop(true);
    command.spawn().map_err(|_| ChromeError::ChromeLaunch)
}

#[cfg(windows)]
fn spawn_chrome(command: Command) -> Result<ChromeChild, ChromeError> {
    use process_wrap::tokio::{CommandWrap, JobObject, KillOnDrop};

    let mut command = CommandWrap::from(command);
    command.wrap(KillOnDrop);
    command.wrap(JobObject);
    command.spawn().map_err(|_| ChromeError::ChromeLaunch)
}

fn find_chrome(explicit: Option<&Path>) -> Option<PathBuf> {
    let override_path = env::var_os("OPSAIL_CHROME_PATH");
    let search_path = env::var_os("PATH");
    let system_candidates = system_chrome_candidates();
    discover_executable(
        explicit,
        override_path.as_deref(),
        &system_candidates,
        search_path.as_deref(),
        path_executable_names(),
    )
}

fn discover_executable(
    explicit: Option<&Path>,
    override_path: Option<&OsStr>,
    system_candidates: &[PathBuf],
    search_path: Option<&OsStr>,
    executable_names: &[&str],
) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return is_executable(path).then(|| path.to_path_buf());
    }

    if let Some(path) = override_path.filter(|path| !path.is_empty()) {
        let path = PathBuf::from(path);
        return is_executable(&path).then_some(path);
    }

    if let Some(path) = system_candidates.iter().find(|path| is_executable(path)) {
        return Some(path.clone());
    }

    search_path.and_then(|path| {
        env::split_paths(path)
            .flat_map(|directory| {
                executable_names
                    .iter()
                    .map(move |name| directory.join(name))
            })
            .find(|candidate| is_executable(candidate))
    })
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(target_os = "macos")]
fn system_chrome_candidates() -> Vec<PathBuf> {
    let names = [
        "/Applications/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Google Chrome Beta.app/Contents/MacOS/Google Chrome Beta",
        "/Applications/Google Chrome Dev.app/Contents/MacOS/Google Chrome Dev",
        "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ];
    let mut candidates = names.into_iter().map(PathBuf::from).collect::<Vec<_>>();
    if let Some(home) = env::var_os("HOME") {
        let applications = PathBuf::from(home).join("Applications");
        if applications.is_absolute() {
            candidates.extend([
                applications
                    .join("Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"),
                applications.join("Google Chrome.app/Contents/MacOS/Google Chrome"),
                applications.join("Google Chrome Beta.app/Contents/MacOS/Google Chrome Beta"),
                applications.join("Google Chrome Dev.app/Contents/MacOS/Google Chrome Dev"),
                applications.join("Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary"),
                applications.join("Chromium.app/Contents/MacOS/Chromium"),
            ]);
        }
    }
    candidates
}

#[cfg(target_os = "linux")]
fn system_chrome_candidates() -> Vec<PathBuf> {
    [
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/usr/bin/google-chrome-beta",
        "/usr/bin/google-chrome-unstable",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/opt/google/chrome/chrome",
        "/snap/bin/chromium",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

#[cfg(target_os = "windows")]
fn system_chrome_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for variable in ["PROGRAMFILES", "PROGRAMFILES(X86)"] {
        if let Some(root) = env::var_os(variable) {
            let root = PathBuf::from(root);
            candidates.push(root.join(r"Google\Chrome\Application\chrome.exe"));
            candidates.push(root.join(r"Google\Chrome Beta\Application\chrome.exe"));
            candidates.push(root.join(r"Google\Chrome Dev\Application\chrome.exe"));
            candidates.push(root.join(r"Chromium\Application\chrome.exe"));
        }
    }
    if let Some(root) = env::var_os("LOCALAPPDATA") {
        let root = PathBuf::from(root);
        candidates.push(root.join(r"Google\Chrome\Application\chrome.exe"));
        candidates.push(root.join(r"Google\Chrome SxS\Application\chrome.exe"));
        candidates.push(root.join(r"Chromium\Application\chrome.exe"));
    }
    candidates
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn system_chrome_candidates() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(target_os = "windows")]
fn path_executable_names() -> &'static [&'static str] {
    &["chrome.exe", "chromium.exe"]
}

#[cfg(not(target_os = "windows"))]
fn path_executable_names() -> &'static [&'static str] {
    &[
        "google-chrome",
        "google-chrome-stable",
        "google-chrome-beta",
        "google-chrome-unstable",
        "chromium",
        "chromium-browser",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_file(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, b"test").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn parses_devtools_active_port() {
        assert_eq!(
            parse_devtools_active_port("49222\n/devtools/browser/test-id\n"),
            Some("ws://127.0.0.1:49222/devtools/browser/test-id".to_owned())
        );
        assert_eq!(
            parse_devtools_active_port("49222\r\n/devtools/browser/test-id\r\n"),
            Some("ws://127.0.0.1:49222/devtools/browser/test-id".to_owned())
        );
    }

    #[test]
    fn launch_arguments_follow_chrome_remote_debugging_contract() {
        let profile = Path::new("isolated-profile");
        let args = chrome_args(profile);
        let args = args
            .iter()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>();

        assert!(args.iter().any(|argument| argument == "--headless"));
        assert!(
            args.iter()
                .any(|argument| argument == "--remote-debugging-address=127.0.0.1")
        );
        assert!(
            args.iter()
                .any(|argument| argument == "--remote-debugging-port=0")
        );
        assert!(
            args.iter()
                .any(|argument| argument == "--user-data-dir=isolated-profile")
        );
        assert!(!args.iter().any(|argument| argument == "--no-sandbox"));
    }

    #[test]
    fn rejects_incomplete_or_invalid_devtools_active_port() {
        assert_eq!(parse_devtools_active_port(""), None);
        assert_eq!(
            parse_devtools_active_port("not-a-port\n/devtools/browser/id"),
            None
        );
        assert_eq!(parse_devtools_active_port("0\n/devtools/browser/id"), None);
        assert_eq!(parse_devtools_active_port("9222"), None);
        assert_eq!(parse_devtools_active_port("9222\n/not-devtools/id"), None);
    }

    #[test]
    fn executable_discovery_uses_documented_priority() {
        let temp = tempfile::tempdir().unwrap();
        let explicit = temp.path().join("explicit-chrome");
        let override_path = temp.path().join("override-chrome");
        let system = temp.path().join("system-chrome");
        let path_directory = temp.path().join("bin");
        let path_candidate = path_directory.join("test-chrome");
        for path in [&explicit, &override_path, &system, &path_candidate] {
            create_file(path);
        }
        let search_path = env::join_paths([&path_directory]).unwrap();
        let system_candidates = [system.clone()];
        let names = ["test-chrome"];

        assert_eq!(
            discover_executable(
                Some(&explicit),
                Some(override_path.as_os_str()),
                &system_candidates,
                Some(search_path.as_os_str()),
                &names,
            ),
            Some(explicit)
        );
        assert_eq!(
            discover_executable(
                None,
                Some(override_path.as_os_str()),
                &system_candidates,
                Some(search_path.as_os_str()),
                &names,
            ),
            Some(override_path)
        );
        assert_eq!(
            discover_executable(
                None,
                None,
                &system_candidates,
                Some(search_path.as_os_str()),
                &names,
            ),
            Some(system)
        );
        assert_eq!(
            discover_executable(None, None, &[], Some(search_path.as_os_str()), &names,),
            Some(path_candidate)
        );
    }

    #[test]
    fn platform_candidates_are_absolute_without_probing_the_host() {
        assert!(
            system_chrome_candidates()
                .iter()
                .all(|candidate| candidate.is_absolute())
        );
        assert!(!path_executable_names().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn path_discovery_skips_non_executable_files() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let blocked = first.path().join("test-chrome");
        let executable = second.path().join("test-chrome");
        std::fs::write(&blocked, b"test").unwrap();
        create_file(&executable);
        let search_path = env::join_paths([first.path(), second.path()]).unwrap();

        assert_eq!(
            discover_executable(
                None,
                None,
                &[],
                Some(search_path.as_os_str()),
                &["test-chrome"],
            ),
            Some(executable)
        );
        assert_eq!(
            discover_executable(Some(&blocked), None, &[], None, &["test-chrome"]),
            None
        );
    }
}
