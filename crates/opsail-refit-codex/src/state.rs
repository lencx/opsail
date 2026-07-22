use std::fs::{self, OpenOptions, TryLockError};
use std::io;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::atomic_file::{
    AtomicWriteError, ensure_no_windows_reparse_points, is_symlink_or_windows_reparse_point,
    write_private_atomically,
};
use crate::error::{CodexRefitError, CodexRefitErrorCode};
use crate::model::SessionMode;

const STATE_SCHEMA_VERSION: u8 = 2;
const MAX_RECORDS: usize = 32;

#[derive(Debug, Clone)]
pub(crate) struct StateStore {
    path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct StateOperationLock {
    _file: fs::File,
}

#[derive(Debug)]
pub(crate) struct StateManagedSessionLock {
    _file: fs::File,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StateDocument {
    schema_version: u8,
    records: Vec<TargetRecord>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TargetRecord {
    pub port: u16,
    pub target_id: String,
    pub revision: String,
    pub session_mode: SessionMode,
    pub manager_token: String,
    pub manager_pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manager_created_at: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ManagedProcessIdentity {
    pub pid: u32,
    pub created_at: Option<u64>,
}

impl StateStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            path: root.join("state.json"),
        }
    }

    pub fn records_for(
        &self,
        port: u16,
        target_id: &str,
    ) -> Result<Vec<TargetRecord>, CodexRefitError> {
        Ok(self
            .read()?
            .records
            .into_iter()
            .filter(|record| record.port == port && record.target_id == target_id)
            .collect())
    }

    pub fn current_record(
        &self,
        port: u16,
        target_id: &str,
        revision: &str,
    ) -> Result<Option<TargetRecord>, CodexRefitError> {
        Ok(self
            .records_for(port, target_id)?
            .into_iter()
            .find(|record| record.revision == revision))
    }

    pub fn managed_process_identity(
        &self,
        port: u16,
    ) -> Result<Option<ManagedProcessIdentity>, CodexRefitError> {
        let records = self
            .read()?
            .records
            .into_iter()
            .filter(|record| record.port == port)
            .collect::<Vec<_>>();
        let Some(first) = records.first() else {
            return Ok(None);
        };
        if records.iter().any(|record| {
            record.manager_pid != first.manager_pid
                || record.manager_created_at != first.manager_created_at
        }) {
            return Err(state_error(
                "managed target markers disagree about their owner process",
            ));
        }
        Ok(Some(ManagedProcessIdentity {
            pid: first.manager_pid,
            created_at: first.manager_created_at,
        }))
    }

    pub fn try_operation_lock(&self) -> Result<StateOperationLock, CodexRefitError> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| state_error("refit state path has no parent directory"))?;
        prepare_directory(parent)?;
        let lock_path = parent.join("operation.lock");
        ensure_no_windows_reparse_points(&lock_path)
            .map_err(|_| state_error("could not verify the refit operation lock path"))?;
        match fs::symlink_metadata(&lock_path) {
            Ok(metadata)
                if is_symlink_or_windows_reparse_point(&metadata) || !metadata.is_file() =>
            {
                return Err(state_error("refit operation lock is not a regular file"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(_) => return Err(state_error("could not inspect the refit operation lock")),
        }
        let file = open_lock_file(&lock_path, true)
            .map_err(|_| state_error("could not open the refit operation lock"))?;
        ensure_no_windows_reparse_points(&lock_path)
            .map_err(|_| state_error("could not verify the refit operation lock path"))?;
        set_file_permissions(&lock_path)?;
        match file.try_lock() {
            Ok(()) => Ok(StateOperationLock { _file: file }),
            Err(TryLockError::WouldBlock) => Err(CodexRefitError::new(
                CodexRefitErrorCode::SessionUnavailable,
                "another Codex refit operation is already running",
            )),
            Err(TryLockError::Error(_)) => {
                Err(state_error("could not acquire the refit operation lock"))
            }
        }
    }

    pub fn try_managed_session_lock(
        &self,
    ) -> Result<Option<StateManagedSessionLock>, CodexRefitError> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| state_error("refit state path has no parent directory"))?;
        prepare_directory(parent)?;
        let lock_path = parent.join("managed-session.lock");
        ensure_no_windows_reparse_points(&lock_path)
            .map_err(|_| state_error("could not verify the managed-session lock path"))?;
        match fs::symlink_metadata(&lock_path) {
            Ok(metadata)
                if is_symlink_or_windows_reparse_point(&metadata) || !metadata.is_file() =>
            {
                return Err(state_error("managed-session lock is not a regular file"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(_) => return Err(state_error("could not inspect the managed-session lock")),
        }
        let file = open_lock_file(&lock_path, true)
            .map_err(|_| state_error("could not open the managed-session lock"))?;
        ensure_no_windows_reparse_points(&lock_path)
            .map_err(|_| state_error("could not verify the managed-session lock path"))?;
        set_file_permissions(&lock_path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(StateManagedSessionLock { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(_)) => {
                Err(state_error("could not acquire the managed-session lock"))
            }
        }
    }

    pub fn managed_session_active(&self) -> Result<bool, CodexRefitError> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| state_error("refit state path has no parent directory"))?;
        let lock_path = parent.join("managed-session.lock");
        ensure_no_windows_reparse_points(&lock_path)
            .map_err(|_| state_error("could not verify the managed-session lock path"))?;
        let metadata = match fs::symlink_metadata(&lock_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err(state_error("could not inspect the managed-session lock")),
        };
        if is_symlink_or_windows_reparse_point(&metadata) || !metadata.is_file() {
            return Err(state_error("managed-session lock is not a regular file"));
        }
        let file = open_lock_file(&lock_path, false)
            .map_err(|_| state_error("could not open the managed-session lock"))?;
        ensure_no_windows_reparse_points(&lock_path)
            .map_err(|_| state_error("could not verify the managed-session lock path"))?;
        match file.try_lock() {
            Ok(()) => Ok(false),
            Err(TryLockError::WouldBlock) => Ok(true),
            Err(TryLockError::Error(_)) => {
                Err(state_error("could not inspect the managed-session lock"))
            }
        }
    }

    pub fn replace(&self, record: TargetRecord) -> Result<(), CodexRefitError> {
        validate_record(&record)?;
        let mut document = self.read()?;
        document
            .records
            .retain(|current| current.port != record.port || current.target_id != record.target_id);
        document.records.push(record);
        if document.records.len() > MAX_RECORDS {
            return Err(state_error("refit state contains too many target records"));
        }
        self.write(&document)
    }

    pub fn remove(&self, port: u16, target_id: &str) -> Result<(), CodexRefitError> {
        let mut document = self.read()?;
        document
            .records
            .retain(|record| record.port != port || record.target_id != target_id);
        self.persist(&document)
    }

    pub fn remove_port(&self, port: u16) -> Result<Vec<TargetRecord>, CodexRefitError> {
        let mut document = self.read()?;
        let mut removed = Vec::new();
        document.records.retain(|record| {
            if record.port == port {
                removed.push(record.clone());
                false
            } else {
                true
            }
        });
        if !removed.is_empty() {
            self.persist(&document)?;
        }
        Ok(removed)
    }

    pub fn remove_absent_targets(
        &self,
        port: u16,
        active_target_ids: &[String],
    ) -> Result<(), CodexRefitError> {
        let mut document = self.read()?;
        let original_len = document.records.len();
        document.records.retain(|record| {
            record.port != port || active_target_ids.iter().any(|id| id == &record.target_id)
        });
        if document.records.len() == original_len {
            return Ok(());
        }
        self.persist(&document)
    }

    pub fn validate(&self) -> Result<(), CodexRefitError> {
        self.read().map(|_| ())
    }

    fn read(&self) -> Result<StateDocument, CodexRefitError> {
        ensure_no_windows_reparse_points(&self.path)
            .map_err(|_| state_error("could not verify the refit state path"))?;
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(StateDocument {
                    schema_version: STATE_SCHEMA_VERSION,
                    records: Vec::new(),
                });
            }
            Err(_) => return Err(state_error("could not read refit state")),
        };
        if is_symlink_or_windows_reparse_point(&metadata) || !metadata.is_file() {
            return Err(state_error("refit state is not a regular file"));
        }
        let bytes = fs::read(&self.path).map_err(|_| state_error("could not read refit state"))?;
        if bytes.len() > 256 * 1024 {
            return Err(state_error("refit state exceeds its size limit"));
        }
        let document: StateDocument =
            serde_json::from_slice(&bytes).map_err(|_| state_error("refit state is invalid"))?;
        if document.schema_version != STATE_SCHEMA_VERSION || document.records.len() > MAX_RECORDS {
            return Err(state_error("refit state uses an unsupported schema"));
        }
        for record in &document.records {
            validate_record(record)?;
        }
        Ok(document)
    }

    fn write(&self, document: &StateDocument) -> Result<(), CodexRefitError> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| state_error("refit state path has no parent directory"))?;
        prepare_directory(parent)?;

        let mut bytes = serde_json::to_vec_pretty(document)
            .map_err(|_| state_error("could not serialize refit state"))?;
        bytes.push(b'\n');
        write_private_atomically(&self.path, &bytes).map_err(|error| match error {
            AtomicWriteError::CreateTemporary => {
                state_error("could not create temporary refit state")
            }
            AtomicWriteError::WriteTemporary => state_error("could not write refit state"),
            AtomicWriteError::FlushTemporary => state_error("could not flush refit state"),
            AtomicWriteError::UnsafeDestination => {
                state_error("could not verify the refit state path")
            }
            AtomicWriteError::ReplaceDestination => state_error("could not replace refit state"),
        })?;
        set_file_permissions(&self.path)
    }

    fn persist(&self, document: &StateDocument) -> Result<(), CodexRefitError> {
        if !document.records.is_empty() {
            return self.write(document);
        }
        ensure_no_windows_reparse_points(&self.path)
            .map_err(|_| state_error("could not verify the refit state path"))?;
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(_) => Err(state_error("could not remove refit state")),
        }
    }
}

fn open_lock_file(path: &Path, create: bool) -> io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options
        .create(create)
        .read(true)
        .write(true)
        .truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;

        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
    }
    options.open(path)
}

fn prepare_directory(path: &Path) -> Result<(), CodexRefitError> {
    ensure_no_windows_reparse_points(path)
        .map_err(|_| state_error("could not verify the refit state directory"))?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if is_symlink_or_windows_reparse_point(&metadata) || !metadata.is_dir() => {
            return Err(state_error(
                "refit state directory is not a regular directory",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {
            fs::create_dir_all(path)
                .map_err(|_| state_error("could not create refit state directory"))?;
            let metadata = fs::symlink_metadata(path)
                .map_err(|_| state_error("could not inspect refit state directory"))?;
            if is_symlink_or_windows_reparse_point(&metadata) || !metadata.is_dir() {
                return Err(state_error(
                    "refit state directory is not a regular directory",
                ));
            }
        }
        Err(_) => return Err(state_error("could not inspect refit state directory")),
    }
    ensure_no_windows_reparse_points(path)
        .map_err(|_| state_error("could not verify the refit state directory"))?;
    set_directory_permissions(path)
}

fn validate_record(record: &TargetRecord) -> Result<(), CodexRefitError> {
    let target_valid = !record.target_id.is_empty()
        && record.target_id.len() <= 200
        && record
            .target_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    let revision_valid =
        !record.revision.is_empty() && record.revision.len() <= 128 && record.revision.is_ascii();
    let token_valid = !record.manager_token.is_empty()
        && record.manager_token.len() <= 128
        && record.manager_token.is_ascii()
        && !record.manager_token.contains(['\r', '\n']);
    if target_valid
        && revision_valid
        && record.session_mode == SessionMode::Persistent
        && token_valid
        && record.manager_pid > 1
        && record.manager_created_at != Some(0)
    {
        Ok(())
    } else {
        Err(state_error("refit state contains an invalid target record"))
    }
}

#[cfg(unix)]
fn set_directory_permissions(path: &Path) -> Result<(), CodexRefitError> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| state_error("could not protect the refit state directory"))
}

#[cfg(windows)]
fn set_directory_permissions(path: &Path) -> Result<(), CodexRefitError> {
    crate::platform::protect_private_directory(path)
        .map_err(|_| state_error("could not protect the refit state directory"))
}

#[cfg(not(any(unix, windows)))]
fn set_directory_permissions(_path: &Path) -> Result<(), CodexRefitError> {
    Ok(())
}

#[cfg(unix)]
fn set_file_permissions(path: &Path) -> Result<(), CodexRefitError> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|_| state_error("could not protect refit state"))
}

#[cfg(windows)]
fn set_file_permissions(path: &Path) -> Result<(), CodexRefitError> {
    crate::platform::protect_private_file(path)
        .map_err(|_| state_error("could not protect refit state"))
}

#[cfg(not(any(unix, windows)))]
fn set_file_permissions(_path: &Path) -> Result<(), CodexRefitError> {
    Ok(())
}

fn state_error(message: impl Into<String>) -> CodexRefitError {
    CodexRefitError::new(CodexRefitErrorCode::StateIo, message)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn record(target_id: &str, token: &str) -> TargetRecord {
        TargetRecord {
            port: 55321,
            target_id: target_id.to_owned(),
            revision: "revision-1".to_owned(),
            session_mode: SessionMode::Persistent,
            manager_token: token.to_owned(),
            manager_pid: 4242,
            manager_created_at: None,
        }
    }

    #[test]
    fn replacing_and_removing_records_is_idempotent() {
        let directory = tempdir().unwrap();
        let store = StateStore::new(directory.path().to_owned());
        store.replace(record("renderer", "manager-1")).unwrap();
        store.replace(record("renderer", "manager-2")).unwrap();

        let records = store.records_for(55321, "renderer").unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].manager_token, "manager-2");
        assert!(
            store
                .current_record(55321, "renderer", "revision-1")
                .unwrap()
                .is_some()
        );
        let serialized = fs::read_to_string(directory.path().join("state.json")).unwrap();
        assert!(serialized.contains("managerToken"));
        assert!(!serialized.contains("scriptIdentifier"));

        store.remove(55321, "renderer").unwrap();
        store.remove(55321, "renderer").unwrap();
        assert!(store.records_for(55321, "renderer").unwrap().is_empty());
    }

    #[test]
    fn malformed_state_fails_closed() {
        let directory = tempdir().unwrap();
        let store = StateStore::new(directory.path().to_owned());
        fs::create_dir_all(directory.path()).unwrap();
        fs::write(directory.path().join("state.json"), b"{}").unwrap();
        assert_eq!(
            store.validate().unwrap_err().code(),
            CodexRefitErrorCode::StateIo
        );
    }

    #[test]
    fn operation_lock_rejects_concurrent_mutation_and_releases_on_drop() {
        let directory = tempdir().unwrap();
        let store = StateStore::new(directory.path().to_owned());
        let first = store.try_operation_lock().unwrap();
        assert_eq!(
            store.try_operation_lock().unwrap_err().code(),
            CodexRefitErrorCode::SessionUnavailable
        );
        drop(first);
        store.try_operation_lock().unwrap();
    }

    #[test]
    fn managed_session_lock_reports_liveness_without_polling() {
        let directory = tempdir().unwrap();
        let store = StateStore::new(directory.path().to_owned());
        assert!(!store.managed_session_active().unwrap());
        let managed = store.try_managed_session_lock().unwrap().unwrap();
        assert!(store.managed_session_active().unwrap());
        assert!(store.try_managed_session_lock().unwrap().is_none());
        drop(managed);
        assert!(!store.managed_session_active().unwrap());
    }

    #[test]
    fn absent_renderer_markers_are_pruned_without_touching_active_targets() {
        let directory = tempdir().unwrap();
        let store = StateStore::new(directory.path().to_owned());
        store.replace(record("active", "manager-1")).unwrap();
        store.replace(record("gone", "manager-2")).unwrap();

        store
            .remove_absent_targets(55321, &["active".to_owned()])
            .unwrap();
        assert_eq!(store.records_for(55321, "active").unwrap().len(), 1);
        assert!(store.records_for(55321, "gone").unwrap().is_empty());
    }

    #[test]
    fn removing_a_port_returns_stale_markers_and_is_repeatable() {
        let directory = tempdir().unwrap();
        let store = StateStore::new(directory.path().to_owned());
        store.replace(record("first", "manager-1")).unwrap();
        let mut other = record("other-port", "manager-2");
        other.port = 55322;
        store.replace(other).unwrap();

        let removed = store.remove_port(55321).unwrap();
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].target_id, "first");
        assert!(store.remove_port(55321).unwrap().is_empty());
        assert_eq!(store.records_for(55322, "other-port").unwrap().len(), 1);
    }
}
