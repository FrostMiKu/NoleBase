//! Atomic storage for independently managed Session notes.
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::core::agent::PermissionMode;
use crate::core::agent_session::AgentSessionScope;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SessionNote {
    pub id: Uuid,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub completed: u64,
    pub read: u64,
    pub permission: PermissionMode,
    pub scope: AgentSessionScope,
    pub run_active: bool,
}

pub(super) fn directory(root: &Path, id: Uuid) -> PathBuf {
    root.join("agents").join(id.to_string())
}

pub(super) fn load(root: &Path) -> Result<Vec<SessionNote>> {
    let parent = root.join("agents");
    fs::create_dir_all(&parent)?;
    let mut notes = Vec::new();
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(|name| Uuid::parse_str(name).ok())
        else {
            continue;
        };
        anyhow::ensure!(
            entry.file_type()?.is_dir(),
            "Session directory must be a regular directory"
        );
        let path = entry.path().join("session.json");
        if !path.exists() {
            continue;
        }
        anyhow::ensure!(
            fs::symlink_metadata(&path)?.is_file(),
            "Session note must be a regular file"
        );
        let note: SessionNote = serde_json::from_reader(fs::File::open(&path)?)
            .with_context(|| format!("reading Session note {}", path.display()))?;
        anyhow::ensure!(note.id == id, "Session identity differs from its directory");
        notes.push(note);
    }
    Ok(notes)
}

pub(super) fn write(root: &Path, note: &SessionNote) -> Result<()> {
    let directory = directory(root, note.id);
    fs::create_dir_all(&directory)?;
    let target = directory.join("session.json");
    let temporary = directory.join(format!(".{}.tmp", Uuid::new_v4()));
    let outcome = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        serde_json::to_writer(&mut file, note)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        crate::core::storage::replace_file_atomically(&temporary, &target)?;
        Ok(())
    })();
    if outcome.is_err() {
        let _ = fs::remove_file(temporary);
    }
    outcome.context("saving Session note")
}
