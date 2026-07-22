//! Audited native Windows boundary for the Codex refit adapter.
//!
//! This crate denies unsafe code everywhere except this module. Windows APIs
//! which require raw buffers or COM calls are isolated here and exposed as
//! bounded safe values. Every unsafe block documents the invariant it relies
//! on.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use thiserror::Error;

#[path = "windows_native/windows.rs"]
mod windows;

/// A bounded Windows platform failure without command output or user data.
#[derive(Debug, Error)]
#[error("{operation}")]
pub struct WindowsPlatformError {
    operation: &'static str,
}

impl WindowsPlatformError {
    #[cfg(windows)]
    fn new(operation: &'static str) -> Self {
        Self { operation }
    }
}

/// Exact Store package contract accepted by an Opsail adapter.
#[derive(Debug, Clone, Copy)]
pub struct StoreAppCriteria<'a> {
    pub package_name: &'a str,
    pub package_family_name: &'a str,
    pub application_id: &'a str,
}

/// A currently registered, Store-signed application identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreAppIdentity {
    pub package_root: PathBuf,
    pub executable: PathBuf,
    pub package_full_name: String,
    pub package_family_name: String,
    pub app_user_model_id: String,
    pub user_sid: String,
}

/// Stable identity fields for one live Windows process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub executable: PathBuf,
    pub user_sid: String,
    pub created_at: u64,
    pub package_full_name: Option<String>,
    pub package_family_name: Option<String>,
    pub app_user_model_id: Option<String>,
}

/// One TCP listener and its owning process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TcpListenerIdentity {
    pub address: IpAddr,
    pub port: u16,
    pub pid: u32,
}

/// ACL shape for one private Opsail path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivatePathKind {
    Directory,
    File,
}

/// A process handle kept open while Opsail waits for an activated app to exit.
#[derive(Debug)]
pub struct WaitableProcess {
    #[cfg(windows)]
    inner: windows::OwnedProcessHandle,
    identity: ProcessIdentity,
}

impl WaitableProcess {
    pub fn identity(&self) -> &ProcessIdentity {
        &self.identity
    }

    pub fn wait(self) -> Result<(), WindowsPlatformError> {
        #[cfg(windows)]
        {
            self.inner.wait()
        }
        #[cfg(not(windows))]
        {
            Err(unsupported())
        }
    }
}

/// Resolve the current user's Local AppData known folder.
pub fn local_app_data_dir() -> Result<PathBuf, WindowsPlatformError> {
    #[cfg(windows)]
    {
        windows::local_app_data_dir()
    }
    #[cfg(not(windows))]
    {
        Err(unsupported())
    }
}

/// Resolve and validate the best matching registered Store application.
pub fn find_store_app(
    criteria: StoreAppCriteria<'_>,
) -> Result<Option<StoreAppIdentity>, WindowsPlatformError> {
    #[cfg(windows)]
    {
        windows::find_store_app(criteria)
    }
    #[cfg(not(windows))]
    {
        let _ = criteria;
        Err(unsupported())
    }
}

/// Activate a registered application and retain a handle to the returned PID.
pub fn activate_application(
    app_user_model_id: &str,
    arguments: &[String],
) -> Result<WaitableProcess, WindowsPlatformError> {
    #[cfg(windows)]
    {
        windows::activate_application(app_user_model_id, arguments)
    }
    #[cfg(not(windows))]
    {
        let _ = (app_user_model_id, arguments);
        Err(unsupported())
    }
}

/// Inspect one process without retaining a handle.
pub fn process_identity(pid: u32) -> Result<ProcessIdentity, WindowsPlatformError> {
    #[cfg(windows)]
    {
        windows::process_identity(pid)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        Err(unsupported())
    }
}

/// Read the kernel creation time used to distinguish reused process IDs.
pub fn process_instance_id(pid: u32) -> Result<u64, WindowsPlatformError> {
    #[cfg(windows)]
    {
        windows::process_instance_id(pid)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        Err(unsupported())
    }
}

/// Inspect every queryable process carrying the requested package family.
pub fn package_processes(
    package_family_name: &str,
) -> Result<Vec<ProcessIdentity>, WindowsPlatformError> {
    #[cfg(windows)]
    {
        windows::package_processes(package_family_name)
    }
    #[cfg(not(windows))]
    {
        let _ = package_family_name;
        Err(unsupported())
    }
}

/// Inspect IPv4 and IPv6 TCP listeners for one port.
pub fn tcp_listeners(port: u16) -> Result<Vec<TcpListenerIdentity>, WindowsPlatformError> {
    #[cfg(windows)]
    {
        windows::tcp_listeners(port)
    }
    #[cfg(not(windows))]
    {
        let _ = port;
        Err(unsupported())
    }
}

/// Terminate only the exact process instance described by `expected`.
pub fn terminate_process(expected: &ProcessIdentity) -> Result<(), WindowsPlatformError> {
    #[cfg(windows)]
    {
        windows::terminate_process(expected)
    }
    #[cfg(not(windows))]
    {
        let _ = expected;
        Err(unsupported())
    }
}

/// Replace a path's DACL with current-user and LocalSystem full access.
pub fn protect_current_user_path(
    path: &Path,
    kind: PrivatePathKind,
) -> Result<(), WindowsPlatformError> {
    windows::protect_current_user_path(path, kind)
}

/// Compare two existing paths by volume and stable Windows file ID.
pub fn same_file(left: &Path, right: &Path) -> Result<bool, WindowsPlatformError> {
    windows::same_file(left, right)
}

/// Quote an argv vector according to the Windows `CommandLineToArgvW` rules.
pub fn windows_argument_line(arguments: &[String]) -> String {
    arguments
        .iter()
        .map(|argument| quote_windows_argument(argument))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_windows_argument(argument: &str) -> String {
    if !argument.is_empty()
        && !argument
            .chars()
            .any(|character| character.is_whitespace() || character == '"')
    {
        return argument.to_owned();
    }

    let mut quoted = String::from('"');
    let mut backslashes = 0usize;
    for character in argument.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(character);
                backslashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

#[cfg(not(windows))]
fn unsupported() -> WindowsPlatformError {
    WindowsPlatformError {
        operation: "Windows platform APIs are unavailable on this operating system",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argument_line_quotes_spaces_quotes_and_trailing_backslashes() {
        assert_eq!(
            windows_argument_line(&[
                "plain".to_owned(),
                "two words".to_owned(),
                "quoted\"value".to_owned(),
                "trailing slash\\".to_owned(),
                String::new(),
            ]),
            "plain \"two words\" \"quoted\\\"value\" \"trailing slash\\\\\" \"\""
        );
    }

    #[test]
    fn waitable_process_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<WaitableProcess>();
    }

    #[test]
    fn native_process_and_file_identity_cover_the_current_executable() {
        let current_executable = std::env::current_exe().unwrap();
        let identity = process_identity(std::process::id()).unwrap();

        assert!(identity.created_at > 0);
        assert!(identity.user_sid.starts_with("S-1-"));
        assert!(same_file(&identity.executable, &current_executable).unwrap());
        assert!(identity.package_full_name.is_none());
        assert!(identity.package_family_name.is_none());
        assert!(identity.app_user_model_id.is_none());
    }

    #[test]
    fn native_tcp_table_reports_a_loopback_listener() {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let listeners = tcp_listeners(port).unwrap();

        assert!(listeners.iter().any(|entry| {
            entry.address == std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
                && entry.pid == std::process::id()
        }));
    }

    #[test]
    fn native_known_folder_is_absolute() {
        assert!(local_app_data_dir().unwrap().is_absolute());
    }

    #[test]
    fn package_discovery_initializes_winrt_on_a_fresh_thread() {
        let result = std::thread::spawn(|| {
            find_store_app(StoreAppCriteria {
                package_name: "Opsail.Missing",
                package_family_name: "Opsail.Missing_0000000000000",
                application_id: "App",
            })
        })
        .join()
        .unwrap()
        .unwrap();

        assert!(result.is_none());
    }
}
