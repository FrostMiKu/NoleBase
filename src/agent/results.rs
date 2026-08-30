//! Session-scoped storage for oversized UTF-8 tool results.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};

pub(crate) const RESULT_URI_PREFIX: &str = "nole://result/";

#[derive(Clone)]
pub(crate) struct SessionResultStore {
    pub(crate) root: PathBuf,
    next_id: Arc<Mutex<u64>>,
}

impl SessionResultStore {
    pub(crate) fn new(nole_root: &Path) -> Result<Self> {
        let root = nole_root.join("agent-session").join("results");
        let next_id = next_result_id(&root)?;
        Ok(Self {
            root,
            next_id: Arc::new(Mutex::new(next_id)),
        })
    }

    pub(crate) fn store(&self, text: &str) -> Result<String> {
        let mut next = self
            .next_id
            .lock()
            .map_err(|_| anyhow::anyhow!("session result counter poisoned"))?;
        if !self.root.exists() {
            *next = 1;
        }
        fs::create_dir_all(&self.root)
            .with_context(|| format!("creating {}", self.root.display()))?;
        let id = *next;
        *next = next.checked_add(1).context("session result id exhausted")?;
        let path = self.root.join(id.to_string());
        let temporary = self.root.join(format!(
            ".{id}.{}-{:016x}.tmp",
            std::process::id(),
            fastrand::u64(..)
        ));
        let outcome = (|| -> Result<()> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(&temporary)
                .with_context(|| format!("creating {}", temporary.display()))?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            fs::rename(&temporary, &path)
                .with_context(|| format!("publishing {}", path.display()))?;
            Ok(())
        })();
        if outcome.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        outcome?;
        Ok(format!("{RESULT_URI_PREFIX}{id}"))
    }
}

pub(crate) fn parse_result_id(value: &str) -> Result<Option<u64>> {
    let Some(raw) = value.strip_prefix(RESULT_URI_PREFIX) else {
        return Ok(None);
    };
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("result URI must be {RESULT_URI_PREFIX}<positive integer>");
    }
    let id = raw.parse::<u64>().context("result id is too large")?;
    if id == 0 {
        bail!("result id must be positive");
    }
    Ok(Some(id))
}

pub(crate) fn result_path(nole_root: &Path, id: u64) -> PathBuf {
    nole_root
        .join("agent-session")
        .join("results")
        .join(id.to_string())
}

/// Resolve a result owned by this session without allowing a filesystem link
/// to turn the opaque result URI into an arbitrary-file read primitive.
pub(crate) fn resolve_result_path(nole_root: &Path, id: u64) -> Result<PathBuf> {
    let path = result_path(nole_root, id);
    let metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("reading session result {RESULT_URI_PREFIX}{id}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("session result is not a regular stored result: {RESULT_URI_PREFIX}{id}");
    }
    Ok(path)
}

pub(crate) fn text_line_count(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        text.bytes().filter(|byte| *byte == b'\n').count() + usize::from(!text.ends_with('\n'))
    }
}

fn next_result_id(root: &Path) -> Result<u64> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(1),
        Err(error) => return Err(error).with_context(|| format!("reading {}", root.display())),
    };
    let mut maximum = 0u64;
    for entry in entries {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if let Ok(id) = name.parse::<u64>() {
            maximum = maximum.max(id);
        }
    }
    maximum
        .checked_add(1)
        .context("session result id exhausted")
}
