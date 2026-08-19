//! Central capacity limits for the Agent workspace (`workspace/main`).
//!
//! Every Agent tool that creates or grows files under the workspace enforces
//! the per-file and total byte limits through this module so the quota has a
//! single source of truth: 64 MiB per file and 512 MiB across the whole
//! `workspace/main` tree. Limits are checked before an operation starts and
//! re-enforced while streaming, so a source that grows during a copy can
//! never push the workspace past its quota.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write as _};
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::storage::{AGENT_WORKSPACE_SUBDIR, WORKSPACE_DIR};

/// Maximum bytes for any single file under `workspace/main` (64 MiB).
pub(crate) const MAX_WORKSPACE_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum total bytes across all files under `workspace/main` (512 MiB).
pub(crate) const MAX_WORKSPACE_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

/// The `workspace/main` sandbox directory for a Nole root.
pub(crate) fn workspace_dir(root: &Path) -> PathBuf {
    root.join(WORKSPACE_DIR).join(AGENT_WORKSPACE_SUBDIR)
}

/// Resolve a new destination relative to `workspace/main`, creating safe
/// missing parents without following symlinks or escaping the sandbox.
pub(crate) fn workspace_destination(root: &Path, input: &str) -> Result<PathBuf> {
    let workspace = workspace_dir(root);
    fs::create_dir_all(&workspace).with_context(|| format!("creating {}", workspace.display()))?;
    let workspace = fs::canonicalize(&workspace)
        .with_context(|| format!("resolving {}", workspace.display()))?;
    let relative = Path::new(input);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("destination must stay within workspace/main");
    }
    let file_name = relative
        .file_name()
        .context("destination must name a file")?;
    let mut current = workspace.clone();
    for component in relative
        .parent()
        .map(Path::components)
        .into_iter()
        .flatten()
    {
        let candidate = current.join(component);
        match fs::symlink_metadata(&candidate) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
                    bail!("destination parent must be a real directory, not a symlink");
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&candidate)
                    .with_context(|| format!("creating directory {}", candidate.display()))?;
            }
            Err(error) => {
                return Err(error).with_context(|| format!("checking {}", candidate.display()));
            }
        }
        current = fs::canonicalize(&candidate)
            .with_context(|| format!("resolving {}", candidate.display()))?;
        if !current.starts_with(&workspace) {
            bail!("destination escapes workspace/main");
        }
    }
    let destination = current.join(file_name);
    match fs::symlink_metadata(&destination) {
        Ok(_) => bail!("destination already exists: {input}"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(destination),
        Err(error) => Err(error).with_context(|| format!("checking destination {input}")),
    }
}

/// Total bytes currently stored under `workspace`, walking the tree without
/// following symlinks (their entries are skipped, so content outside the
/// workspace never counts toward the quota).
pub(crate) fn workspace_used_bytes(workspace: &Path) -> Result<u64> {
    let entries = match fs::read_dir(workspace) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(error).with_context(|| format!("listing {}", workspace.display()));
        }
    };
    let mut used = 0u64;
    for entry in entries {
        let entry = entry.with_context(|| format!("reading {}", workspace.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("checking {}", entry.path().display()))?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            used = used.saturating_add(workspace_used_bytes(&entry.path())?);
        } else if file_type.is_file() {
            let len = entry
                .metadata()
                .with_context(|| format!("checking {}", entry.path().display()))?
                .len();
            used = used.saturating_add(len);
        }
    }
    Ok(used)
}

/// Enforce the workspace capacity limits for a prospective write of
/// `new_bytes` at `destination`. Destinations outside the workspace are never
/// limited, matching the path-zone rules. Replacement is accounted for
/// conservatively: the destination's current size is already part of the
/// usage, and the new bytes are charged in full on top, so a replace never
/// assumes the old content is freed before the new content lands.
pub(crate) fn check_workspace_write(root: &Path, destination: &Path, new_bytes: u64) -> Result<()> {
    let workspace = workspace_dir(root);
    if !destination.starts_with(&workspace) {
        return Ok(());
    }
    check_workspace_writes(root, std::iter::once((destination, new_bytes)))
}

/// Maximum candidate bytes an atomic edit may stage while the original file
/// still occupies its workspace quota.
pub(crate) fn workspace_edit_budget(root: &Path, destination: &Path) -> Result<u64> {
    let workspace = workspace_dir(root);
    if !destination.starts_with(&workspace) {
        return Ok(u64::MAX);
    }
    let used = workspace_used_bytes(&workspace)?;
    Ok(MAX_WORKSPACE_FILE_BYTES.min(MAX_WORKSPACE_TOTAL_BYTES.saturating_sub(used)))
}

/// Recheck an atomic edit after its candidate has been staged. The staging
/// file is already included in `used`, so subtract it before charging the same
/// candidate as the prospective destination.
pub(crate) fn check_workspace_staged_write(
    root: &Path,
    destination: &Path,
    staging: &Path,
    new_bytes: u64,
) -> Result<()> {
    let workspace = workspace_dir(root);
    if !destination.starts_with(&workspace) {
        return Ok(());
    }
    if new_bytes > MAX_WORKSPACE_FILE_BYTES {
        bail!(
            "{} exceeds the 64 MiB workspace per-file limit",
            destination.display()
        );
    }
    let used = workspace_used_bytes(&workspace)?;
    let staged = if staging.starts_with(&workspace) {
        fs::metadata(staging)
            .with_context(|| format!("checking staged edit {}", staging.display()))?
            .len()
    } else {
        0
    };
    if used.saturating_sub(staged).saturating_add(new_bytes) > MAX_WORKSPACE_TOTAL_BYTES {
        bail!(
            "edit would exceed the 512 MiB workspace total limit ({} bytes already in use)",
            used.saturating_sub(staged)
        );
    }
    Ok(())
}

/// Enforce workspace limits for a batch that will publish every destination.
/// The total check charges the complete batch against one usage snapshot so
/// individually valid moves cannot collectively exceed the workspace quota.
pub(crate) fn check_workspace_writes<'a>(
    root: &Path,
    writes: impl IntoIterator<Item = (&'a Path, u64)>,
) -> Result<()> {
    let workspace = workspace_dir(root);
    let mut charged = 0u64;
    for (destination, bytes) in writes {
        if !destination.starts_with(&workspace) {
            continue;
        }
        if bytes > MAX_WORKSPACE_FILE_BYTES {
            bail!(
                "{} exceeds the 64 MiB workspace per-file limit",
                destination.display()
            );
        }
        charged = charged.saturating_add(bytes);
    }
    let used = workspace_used_bytes(&workspace)?;
    if used.saturating_add(charged) > MAX_WORKSPACE_TOTAL_BYTES {
        bail!(
            "batch would exceed the 512 MiB workspace total limit ({} bytes already in use)",
            used
        );
    }
    Ok(())
}

fn enforce_limits(destination: &Path, new_bytes: u64, used: u64) -> Result<()> {
    if new_bytes > MAX_WORKSPACE_FILE_BYTES {
        bail!(
            "{} exceeds the 64 MiB workspace per-file limit",
            destination.display()
        );
    }
    if used.saturating_add(new_bytes) > MAX_WORKSPACE_TOTAL_BYTES {
        bail!(
            "{} would exceed the 512 MiB workspace total limit ({} bytes already in use)",
            destination.display(),
            used
        );
    }
    Ok(())
}

/// Copy `source` into a brand-new `destination`, enforcing the workspace
/// limits before and during the transfer. Destinations outside the workspace
/// copy without limits. On any failure the partial destination is removed so
/// a quota abort never leaves a truncated file behind.
pub(crate) fn copy_with_workspace_limits(
    root: &Path,
    source: &Path,
    destination: &Path,
) -> Result<u64> {
    let workspace = workspace_dir(root);
    if !destination.starts_with(&workspace) {
        return copy_bounded(source, destination, u64::MAX);
    }
    let used = workspace_used_bytes(&workspace)?;
    let source_len = fs::metadata(source)
        .with_context(|| format!("reading metadata for {}", source.display()))?
        .len();
    enforce_limits(destination, source_len, used)?;
    // The streaming budget covers both limits: never exceed the per-file cap
    // and never grow the workspace past its total cap.
    let budget = MAX_WORKSPACE_FILE_BYTES.min(MAX_WORKSPACE_TOTAL_BYTES.saturating_sub(used));
    copy_bounded(source, destination, budget)
}

fn copy_bounded(source: &Path, destination: &Path, budget: u64) -> Result<u64> {
    let mut input =
        fs::File::open(source).with_context(|| format!("opening source {}", source.display()))?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .with_context(|| format!("creating destination {}", destination.display()))?;
    match copy_limited(&mut input, &mut output, budget) {
        Ok(bytes) => Ok(bytes),
        Err(error) => {
            drop(output);
            let _ = fs::remove_file(destination);
            Err(anyhow::anyhow!(
                "copying to {}: {error}",
                destination.display()
            ))
        }
    }
}

/// Stream from `input` to `output`, failing as soon as more than `budget`
/// bytes would be written so a growing source can never push a file past the
/// quota.
fn copy_limited(input: &mut fs::File, output: &mut fs::File, budget: u64) -> io::Result<u64> {
    let mut written = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            return Ok(written);
        }
        let next = written + read as u64;
        if next > budget {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                format!("workspace limit exceeded: {next} bytes would exceed the allowed budget"),
            ));
        }
        output.write_all(&buffer[..read])?;
        written = next;
    }
}

#[cfg(test)]
mod tests {
    use crate::agent::Tool;
    use serde_json::json;

    use super::*;

    fn fresh_root() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("root");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(root.join("workspace/main")).unwrap();
        fs::create_dir_all(root.join("data")).unwrap();
        (directory, root)
    }

    #[test]
    fn per_file_limit_rejects_oversized_writes() {
        let (_directory, root) = fresh_root();
        let destination = root.join("workspace/main/big.bin");
        let error =
            check_workspace_write(&root, &destination, MAX_WORKSPACE_FILE_BYTES + 1).unwrap_err();
        assert!(error.to_string().contains("64 MiB"));
        check_workspace_write(&root, &destination, MAX_WORKSPACE_FILE_BYTES).unwrap();
    }

    #[test]
    fn total_limit_counts_replacement_conservatively() {
        let (_directory, root) = fresh_root();
        let existing = root.join("workspace/main/large.bin");
        // Sparse file: logical size counts toward the quota without disk use.
        fs::File::create(&existing)
            .unwrap()
            .set_len(500 * 1024 * 1024)
            .unwrap();
        // Replacing the 500 MiB file with 13 MiB fits by net accounting but
        // is rejected: the old bytes are never assumed freed.
        let error = check_workspace_write(&root, &existing, 13 * 1024 * 1024).unwrap_err();
        assert!(error.to_string().contains("512 MiB"));
        // A 12 MiB replacement stays within the conservative accounting.
        check_workspace_write(&root, &existing, 12 * 1024 * 1024).unwrap();
        let second = root.join("workspace/main/other.bin");
        assert!(check_workspace_write(&root, &second, 13 * 1024 * 1024).is_err());
        check_workspace_write(&root, &second, 12 * 1024 * 1024).unwrap();
    }

    #[test]
    fn batch_limit_charges_all_destinations_together() {
        let (_directory, root) = fresh_root();
        let workspace = root.join("workspace/main");
        fs::File::create(workspace.join("existing.bin"))
            .unwrap()
            .set_len(500 * 1024 * 1024)
            .unwrap();
        let first = workspace.join("first.bin");
        let second = workspace.join("second.bin");
        let error = check_workspace_writes(
            &root,
            [
                (first.as_path(), 7 * 1024 * 1024),
                (second.as_path(), 7 * 1024 * 1024),
            ],
        )
        .unwrap_err();
        assert!(error.to_string().contains("512 MiB"));
        check_workspace_writes(
            &root,
            [
                (first.as_path(), 6 * 1024 * 1024),
                (second.as_path(), 6 * 1024 * 1024),
            ],
        )
        .unwrap();
    }

    #[test]
    fn outside_workspace_destinations_are_not_limited() {
        let (_directory, root) = fresh_root();
        let destination = root.join("data/plain.bin");
        check_workspace_write(&root, &destination, MAX_WORKSPACE_TOTAL_BYTES + 1).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn usage_walk_skips_symlinks() {
        use std::os::unix::fs::symlink;

        let (_directory, root) = fresh_root();
        let workspace = root.join("workspace/main");
        fs::write(workspace.join("a.txt"), "12345").unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.bin");
        fs::write(&secret, vec![0u8; 4096]).unwrap();
        symlink(&secret, workspace.join("link.bin")).unwrap();
        assert_eq!(workspace_used_bytes(&workspace).unwrap(), 5);
    }

    #[test]
    fn copy_enforces_per_file_limit_and_leaves_no_partial_file() {
        let (_directory, root) = fresh_root();
        let source = root.join("data/source.bin");
        // Sparse source: metadata size exceeds the limit without real disk use.
        fs::File::create(&source)
            .unwrap()
            .set_len(MAX_WORKSPACE_FILE_BYTES + 1)
            .unwrap();
        let destination = root.join("workspace/main/copy.bin");
        let error = copy_with_workspace_limits(&root, &source, &destination).unwrap_err();
        assert!(error.to_string().contains("64 MiB"));
        assert!(!destination.exists(), "no partial file may remain");
    }

    #[test]
    fn copy_stream_is_bounded_even_if_source_grows() {
        let (_directory, root) = fresh_root();
        let source = root.join("data/growing.bin");
        fs::write(&source, vec![0u8; 4096]).unwrap();
        let destination = root.join("workspace/main/bounded.bin");
        // A budget below the source size aborts mid-stream and cleans up.
        let error = copy_bounded(&source, &destination, 1024).unwrap_err();
        assert!(error.to_string().contains("workspace limit"));
        assert!(!destination.exists(), "no partial file may remain");
        // A budget covering the source succeeds.
        let bytes = copy_bounded(&source, &destination, 4096).unwrap();
        assert_eq!(bytes, 4096);
        assert_eq!(fs::metadata(&destination).unwrap().len(), 4096);
    }

    #[test]
    fn copy_outside_workspace_ignores_limits() {
        let (_directory, root) = fresh_root();
        let source = root.join("data/source.bin");
        fs::write(&source, b"payload").unwrap();
        let destination = root.join("data/copied.bin");
        let bytes = copy_with_workspace_limits(&root, &source, &destination).unwrap();
        assert_eq!(bytes, 7);
        assert_eq!(fs::read(&destination).unwrap(), b"payload");
    }

    #[test]
    fn copy_tool_wiring_refuses_oversized_workspace_destinations() {
        use super::super::file_ops::Copy;
        use crate::agent::test_support::{bypass_gate, test_runtime};

        let (_directory, root) = fresh_root();
        let source = root.join("data/source.bin");
        fs::File::create(&source)
            .unwrap()
            .set_len(MAX_WORKSPACE_FILE_BYTES + 1)
            .unwrap();
        let copy = Copy::new(&root, bypass_gate()).unwrap();
        let error = test_runtime()
            .block_on(copy.execute(&json!({
                "source": source.to_string_lossy(),
                "destination": "workspace/main/big.bin"
            })))
            .unwrap_err();
        assert!(error.to_string().contains("64 MiB"));
        assert!(!root.join("workspace/main/big.bin").exists());
    }
}
