//! Atomic publication primitives shared by storage-backed files.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{bail, Context, Result};

/// Publish `staged` at `destination` as one filesystem operation.
///
/// A missing destination is installed and an existing destination is replaced
/// atomically. Each platform uses one publication syscall, which resolves
/// concurrent destination changes within the filesystem operation.
pub(crate) fn replace_file_atomically(staged: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(not(windows))]
    {
        fs::rename(staged, destination)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };

        let staged = staged
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let destination = destination
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let published = unsafe {
            MoveFileExW(
                staged.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if published == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

/// Create a unique hidden temp file next to `target` (same directory, same
/// filesystem) and write `content` into it, so a caller can later publish it
/// atomically with [`replace_file_atomically`]. `create_new` guarantees a
/// coincidental file is never clobbered. Multi-file rewrites stage every
/// replacement first so an ordinary write failure (full disk, permissions)
/// leaves every original untouched.
pub(crate) fn stage_replacement(target: &Path, content: &str) -> Result<PathBuf> {
    use std::io::Write as _;

    let parent = target
        .parent()
        .context("staged replacement has no parent directory")?;
    let name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("nole-note");
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    for attempt in 0..16u32 {
        let temp_name = format!(
            ".{name}.nole-rename-{}-{nonce}-{attempt}.tmp",
            std::process::id()
        );
        let temp = parent.join(temp_name);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        match options.open(&temp) {
            Ok(mut file) => {
                if let Err(error) = file.write_all(content.as_bytes()) {
                    let _ = fs::remove_file(&temp);
                    return Err(error)
                        .with_context(|| format!("writing staged file {}", temp.display()));
                }
                return Ok(temp);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("creating staged file next to {}", target.display()))
            }
        }
    }
    bail!(
        "could not allocate a staged file next to {}",
        target.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishes_when_destination_is_missing_and_replaces_existing_content() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("published");
        let first = directory.path().join("first");
        fs::write(&first, "first").unwrap();
        replace_file_atomically(&first, &destination).unwrap();
        assert_eq!(fs::read_to_string(&destination).unwrap(), "first");

        let second = directory.path().join("second");
        fs::write(&second, "second").unwrap();
        replace_file_atomically(&second, &destination).unwrap();
        assert_eq!(fs::read_to_string(&destination).unwrap(), "second");
        assert!(!second.exists());
    }
}
