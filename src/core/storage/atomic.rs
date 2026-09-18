//! Atomic publication primitives shared by storage-backed files.

use std::fs;
use std::io;
use std::path::Path;

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
