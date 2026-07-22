use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AtomicWriteError {
    CreateTemporary,
    WriteTemporary,
    FlushTemporary,
    UnsafeDestination,
    ReplaceDestination,
}

/// Persist bytes through a private temporary file in the destination directory.
///
/// `std::fs::rename` replaces an existing destination on supported Unix and Windows
/// filesystems. Keeping both paths in one directory makes the commit a same-volume
/// rename and avoids a delete-before-replace window.
pub(crate) fn write_private_atomically(
    destination: &Path,
    bytes: &[u8],
) -> Result<(), AtomicWriteError> {
    ensure_no_windows_reparse_points(destination)
        .map_err(|_| AtomicWriteError::UnsafeDestination)?;
    let parent = destination
        .parent()
        .ok_or(AtomicWriteError::CreateTemporary)?;
    let (temporary, mut file) = create_private_temporary(parent)?;

    let write_result = file
        .write_all(bytes)
        .map_err(|_| AtomicWriteError::WriteTemporary)
        .and_then(|()| {
            file.sync_all()
                .map_err(|_| AtomicWriteError::FlushTemporary)
        });
    drop(file);

    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }

    if ensure_no_windows_reparse_points(destination).is_err() {
        let _ = fs::remove_file(&temporary);
        return Err(AtomicWriteError::UnsafeDestination);
    }
    if fs::rename(&temporary, destination).is_err() {
        let _ = fs::remove_file(&temporary);
        return Err(AtomicWriteError::ReplaceDestination);
    }
    Ok(())
}

fn create_private_temporary(parent: &Path) -> Result<(PathBuf, File), AtomicWriteError> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);

    for _ in 0..32 {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(
            ".opsail-atomic-{}-{sequence}.tmp",
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        match options.open(&temporary) {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(AtomicWriteError::CreateTemporary),
        }
    }

    Err(AtomicWriteError::CreateTemporary)
}

pub(crate) fn is_symlink_or_windows_reparse_point(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;

        has_windows_reparse_attribute(metadata.file_attributes())
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(windows)]
pub(crate) fn ensure_no_windows_reparse_points(path: &Path) -> io::Result<()> {
    let absolute = std::path::absolute(path)?;
    for component in absolute.ancestors() {
        if component.as_os_str().is_empty() {
            continue;
        }
        match fs::symlink_metadata(component) {
            Ok(metadata) if is_symlink_or_windows_reparse_point(&metadata) => {
                return Err(io::Error::new(
                    ErrorKind::InvalidInput,
                    "path contains a Windows reparse point",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn ensure_no_windows_reparse_points(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(any(windows, test))]
fn has_windows_reparse_attribute(attributes: u32) -> bool {
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

    attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn replaces_an_existing_file_and_removes_the_temporary_file() {
        let directory = tempdir().unwrap();
        let destination = directory.path().join("state.json");
        fs::write(&destination, b"old").unwrap();

        write_private_atomically(&destination, b"new").unwrap();

        assert_eq!(fs::read(&destination).unwrap(), b"new");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn failed_replacement_preserves_the_existing_destination() {
        let directory = tempdir().unwrap();
        let destination = directory.path().join("existing-directory");
        fs::create_dir(&destination).unwrap();

        assert_eq!(
            write_private_atomically(&destination, b"new").unwrap_err(),
            AtomicWriteError::ReplaceDestination
        );
        assert!(destination.is_dir());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn windows_reparse_attribute_mask_is_detected_without_ffi() {
        assert!(!has_windows_reparse_attribute(0));
        assert!(has_windows_reparse_attribute(0x400));
        assert!(has_windows_reparse_attribute(0x400 | 0x20));
    }
}
