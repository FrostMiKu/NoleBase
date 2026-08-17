//! Streaming preparation and atomic publication for line-based edits.

use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::file_ops::LineEdit;
use super::util::MAX_DIFF_BYTES;
use crate::agent::SnapshotIdentityHasher;

pub(super) struct FileInspection {
    pub identity: [u8; 32],
    pub total_lines: usize,
    pub len: u64,
    line_ending: &'static str,
    ends_with_newline: bool,
    permissions: Permissions,
}

pub(super) struct PreparedEdit {
    temporary: TemporaryFile,
    pub original_identity: [u8; 32],
    pub candidate_identity: [u8; 32],
    pub candidate_len: u64,
    pub diff: String,
}

impl PreparedEdit {
    pub(super) fn path(&self) -> &Path {
        &self.temporary.path
    }

    pub(super) fn publish(mut self, destination: &Path) -> Result<()> {
        replace_file(&self.temporary.path, destination)?;
        self.temporary.published = true;
        sync_parent(destination)
    }
}

struct TemporaryFile {
    path: PathBuf,
    published: bool,
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub(super) fn inspect_text_file(path: &Path) -> Result<FileInspection> {
    let metadata = fs::metadata(path).with_context(|| format!("checking {}", path.display()))?;
    if !metadata.is_file() {
        bail!("target must be a regular UTF-8 file");
    }
    let mut reader =
        BufReader::new(File::open(path).with_context(|| format!("opening {}", path.display()))?);
    let mut identity = SnapshotIdentityHasher::default();
    let mut line = Vec::new();
    let mut total_lines = 0usize;
    let mut uses_crlf = false;
    let mut ends_with_newline = false;
    loop {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .with_context(|| format!("reading {}", path.display()))?;
        if read == 0 {
            break;
        }
        std::str::from_utf8(&line)
            .with_context(|| format!("target is not valid UTF-8: {}", path.display()))?;
        identity.update(&line);
        total_lines = total_lines.saturating_add(1);
        uses_crlf |= line.ends_with(b"\r\n");
        ends_with_newline = line.ends_with(b"\n");
    }
    Ok(FileInspection {
        identity: identity.finish(),
        total_lines,
        len: metadata.len(),
        line_ending: if uses_crlf { "\r\n" } else { "\n" },
        ends_with_newline,
        permissions: metadata.permissions(),
    })
}

pub(super) fn prepare_edit(
    source: &Path,
    label: &str,
    edits: &[LineEdit],
    inspection: &FileInspection,
    max_candidate_bytes: u64,
) -> Result<PreparedEdit> {
    let (temporary, file) = create_temporary(source, inspection.permissions.clone())?;
    let mut writer = BufWriter::new(file);
    let mut reader = BufReader::new(
        File::open(source).with_context(|| format!("opening {}", source.display()))?,
    );
    let mut original_identity = SnapshotIdentityHasher::default();
    let mut line = Vec::new();
    let mut line_index = 0usize;
    let mut edit_index = 0usize;
    let mut written = 0u64;

    while line_index < inspection.total_lines {
        while edits
            .get(edit_index)
            .is_some_and(|edit| edit.insertion && edit.start_line == line_index)
        {
            let edit = &edits[edit_index];
            write_replacement(
                &mut writer,
                edit,
                inspection.line_ending,
                false,
                &mut written,
                max_candidate_bytes,
            )?;
            edit_index += 1;
        }

        if let Some(edit) = edits
            .get(edit_index)
            .filter(|edit| !edit.insertion && edit.start_line == line_index)
        {
            write_replacement(
                &mut writer,
                edit,
                inspection.line_ending,
                false,
                &mut written,
                max_candidate_bytes,
            )?;
            while line_index < edit.end_line_exclusive {
                read_source_line(&mut reader, source, &mut line, &mut original_identity)?;
                line_index += 1;
            }
            edit_index += 1;
            continue;
        }

        read_source_line(&mut reader, source, &mut line, &mut original_identity)?;
        write_bounded(&mut writer, &line, &mut written, max_candidate_bytes)?;
        line_index += 1;
    }

    while edits
        .get(edit_index)
        .is_some_and(|edit| edit.insertion && edit.start_line == inspection.total_lines)
    {
        let edit = &edits[edit_index];
        let leading_newline = inspection.len > 0 && !inspection.ends_with_newline;
        write_replacement(
            &mut writer,
            edit,
            inspection.line_ending,
            leading_newline,
            &mut written,
            max_candidate_bytes,
        )?;
        edit_index += 1;
    }
    if edit_index != edits.len() {
        bail!("edit ranges no longer match the source file");
    }

    line.clear();
    if reader
        .read_until(b'\n', &mut line)
        .with_context(|| format!("rechecking {}", source.display()))?
        != 0
    {
        bail!("file changed while preparing edit; read it again and retry");
    }
    let original_identity = original_identity.finish();
    if original_identity != inspection.identity {
        bail!("file changed while preparing edit; read it again and retry");
    }

    writer.flush().context("flushing edited candidate")?;
    writer
        .get_ref()
        .sync_all()
        .context("syncing edited candidate")?;
    drop(writer);
    let candidate = inspect_text_file(&temporary.path)?;
    Ok(PreparedEdit {
        temporary,
        original_identity,
        candidate_identity: candidate.identity,
        candidate_len: candidate.len,
        diff: build_diff_preview(
            source,
            label,
            edits,
            inspection.total_lines,
            original_identity,
        )?,
    })
}

fn create_temporary(source: &Path, permissions: Permissions) -> Result<(TemporaryFile, File)> {
    let parent = source
        .parent()
        .context("edit target has no parent directory")?;
    let name = source
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    for _ in 0..100 {
        let path = parent.join(format!(".{name}.nole-edit-{:016x}.tmp", fastrand::u64(..)));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                file.set_permissions(permissions)
                    .with_context(|| format!("preserving permissions for {}", source.display()))?;
                return Ok((
                    TemporaryFile {
                        path,
                        published: false,
                    },
                    file,
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("creating edit candidate for {}", source.display()))
            }
        }
    }
    bail!(
        "could not allocate a unique edit candidate for {}",
        source.display()
    )
}

fn read_source_line(
    reader: &mut BufReader<File>,
    source: &Path,
    line: &mut Vec<u8>,
    identity: &mut SnapshotIdentityHasher,
) -> Result<()> {
    line.clear();
    if reader
        .read_until(b'\n', line)
        .with_context(|| format!("reading {}", source.display()))?
        == 0
    {
        bail!("file changed while preparing edit; read it again and retry");
    }
    std::str::from_utf8(line)
        .with_context(|| format!("target is not valid UTF-8: {}", source.display()))?;
    identity.update(line);
    Ok(())
}

fn write_replacement(
    writer: &mut BufWriter<File>,
    edit: &LineEdit,
    line_ending: &str,
    leading_newline: bool,
    written: &mut u64,
    maximum: u64,
) -> Result<()> {
    if edit.lines.is_empty() {
        return Ok(());
    }
    if leading_newline {
        write_bounded(writer, line_ending.as_bytes(), written, maximum)?;
    }
    for line in &edit.lines {
        write_bounded(writer, line.as_bytes(), written, maximum)?;
        write_bounded(writer, line_ending.as_bytes(), written, maximum)?;
    }
    Ok(())
}

fn write_bounded(
    writer: &mut BufWriter<File>,
    bytes: &[u8],
    written: &mut u64,
    maximum: u64,
) -> Result<()> {
    let next = written.saturating_add(bytes.len() as u64);
    if next > maximum {
        bail!("edited candidate exceeds the available workspace capacity");
    }
    writer.write_all(bytes)?;
    *written = next;
    Ok(())
}

const DIFF_CONTEXT_LINES: usize = 3;

#[derive(Clone, Copy)]
struct PreviewHunk {
    start_line: usize,
    end_line_exclusive: usize,
    first_edit: usize,
    end_edit_exclusive: usize,
}

fn build_diff_preview(
    source: &Path,
    label: &str,
    edits: &[LineEdit],
    total_lines: usize,
    expected_identity: [u8; 32],
) -> Result<String> {
    let reader = BufReader::new(
        File::open(source)
            .with_context(|| format!("opening {} for diff preview", source.display()))?,
    );
    let (preview, identity) = build_diff_preview_from_reader(reader, label, edits, total_lines)?;
    if identity != expected_identity {
        bail!("file changed while preparing edit; read it again and retry");
    }
    Ok(preview)
}

fn build_diff_preview_from_reader<R: BufRead>(
    mut reader: R,
    label: &str,
    edits: &[LineEdit],
    total_lines: usize,
) -> Result<(String, [u8; 32])> {
    let hunks = preview_hunks(edits, total_lines);
    let mut preview = DiffPreview::new(label);
    let mut identity = SnapshotIdentityHasher::default();
    let mut line = Vec::new();
    let mut source_line = 0usize;

    for hunk in hunks {
        while source_line < hunk.start_line {
            read_preview_line(&mut reader, &mut line, &mut identity)?;
            source_line += 1;
        }

        let preceding_edits = &edits[..hunk.first_edit];
        let removed_before = preceding_edits
            .iter()
            .map(|edit| edit.end_line_exclusive - edit.start_line)
            .sum::<usize>();
        let added_before = preceding_edits
            .iter()
            .map(|edit| edit.lines.len())
            .sum::<usize>();
        let new_start_index = hunk.start_line - removed_before + added_before;
        let hunk_edits = &edits[hunk.first_edit..hunk.end_edit_exclusive];
        let removed = hunk_edits
            .iter()
            .map(|edit| edit.end_line_exclusive - edit.start_line)
            .sum::<usize>();
        let added = hunk_edits
            .iter()
            .map(|edit| edit.lines.len())
            .sum::<usize>();
        let old_count = hunk.end_line_exclusive - hunk.start_line;
        let new_count = old_count - removed + added;
        let old_start = if old_count == 0 {
            hunk.start_line
        } else {
            hunk.start_line + 1
        };
        let new_start = if new_count == 0 {
            new_start_index
        } else {
            new_start_index + 1
        };
        preview.begin_hunk(old_start, old_count, new_start, new_count);

        for edit in hunk_edits {
            while source_line < edit.start_line {
                read_preview_line(&mut reader, &mut line, &mut identity)?;
                preview.source_line(' ', &line);
                source_line += 1;
            }
            while source_line < edit.end_line_exclusive {
                read_preview_line(&mut reader, &mut line, &mut identity)?;
                preview.source_line('-', &line);
                source_line += 1;
            }
            preview.add_lines(&edit.lines);
        }
        while source_line < hunk.end_line_exclusive {
            read_preview_line(&mut reader, &mut line, &mut identity)?;
            preview.source_line(' ', &line);
            source_line += 1;
        }
    }

    while source_line < total_lines {
        read_preview_line(&mut reader, &mut line, &mut identity)?;
        source_line += 1;
    }
    line.clear();
    if reader
        .read_until(b'\n', &mut line)
        .context("checking diff preview source length")?
        != 0
    {
        bail!("file changed while preparing edit; read it again and retry");
    }
    Ok((preview.finish(), identity.finish()))
}

fn preview_hunks(edits: &[LineEdit], total_lines: usize) -> Vec<PreviewHunk> {
    let mut hunks = Vec::<PreviewHunk>::new();
    for (index, edit) in edits.iter().enumerate() {
        let start_line = edit.start_line.saturating_sub(DIFF_CONTEXT_LINES);
        let end_line_exclusive = edit
            .end_line_exclusive
            .max(edit.start_line)
            .saturating_add(DIFF_CONTEXT_LINES)
            .min(total_lines);
        if let Some(hunk) = hunks
            .last_mut()
            .filter(|hunk| start_line <= hunk.end_line_exclusive)
        {
            hunk.end_line_exclusive = hunk.end_line_exclusive.max(end_line_exclusive);
            hunk.end_edit_exclusive = index + 1;
        } else {
            hunks.push(PreviewHunk {
                start_line,
                end_line_exclusive,
                first_edit: index,
                end_edit_exclusive: index + 1,
            });
        }
    }
    hunks
}

fn read_preview_line<R: BufRead>(
    reader: &mut R,
    line: &mut Vec<u8>,
    identity: &mut SnapshotIdentityHasher,
) -> Result<()> {
    line.clear();
    if reader
        .read_until(b'\n', line)
        .context("reading source for diff preview")?
        == 0
    {
        bail!("file changed while preparing edit; read it again and retry");
    }
    std::str::from_utf8(line).context("source is not valid UTF-8 while building diff preview")?;
    identity.update(line);
    Ok(())
}

struct DiffPreview {
    output: String,
    truncated: bool,
}

impl DiffPreview {
    fn new(label: &str) -> Self {
        Self {
            output: format!("--- {label}\n+++ {label}\n"),
            truncated: false,
        }
    }

    fn begin_hunk(
        &mut self,
        old_start: usize,
        old_count: usize,
        new_start: usize,
        new_count: usize,
    ) {
        self.push(&format!(
            "@@ -{old_start},{old_count} +{new_start},{new_count} @@\n"
        ));
    }

    fn add_lines(&mut self, lines: &[String]) {
        for line in lines {
            self.push_prefixed('+', line.as_bytes());
        }
    }

    fn source_line(&mut self, prefix: char, line: &[u8]) {
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        self.push_prefixed(prefix, line);
    }

    fn push_prefixed(&mut self, prefix: char, bytes: &[u8]) {
        if self.truncated {
            return;
        }
        let text = String::from_utf8_lossy(bytes);
        self.push(&format!("{prefix}{text}\n"));
    }

    fn push(&mut self, text: &str) {
        if self.truncated {
            return;
        }
        let remaining = MAX_DIFF_BYTES.saturating_sub(self.output.len());
        if text.len() <= remaining {
            self.output.push_str(text);
            return;
        }
        let mut end = remaining.min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.output.push_str(&text[..end]);
        self.truncated = true;
    }

    fn finish(mut self) -> String {
        if self.truncated {
            self.output.push_str("\n... diff truncated ...\n");
        }
        self.output
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn diff_preview_includes_context_around_insertions_and_deletions() {
        let edits = vec![
            LineEdit {
                start_line: 1,
                end_line_exclusive: 1,
                lines: vec!["插入一行".to_string()],
                insertion: true,
                anchor_line: 2,
            },
            LineEdit {
                start_line: 2,
                end_line_exclusive: 3,
                lines: Vec::new(),
                insertion: false,
                anchor_line: 3,
            },
        ];
        let source = "测试文本\n第二行\n第三行\n";

        let (preview, _) =
            build_diff_preview_from_reader(Cursor::new(source), "note.md", &edits, 3).unwrap();

        assert_eq!(
            preview,
            "--- note.md\n+++ note.md\n@@ -1,3 +1,3 @@\n 测试文本\n+插入一行\n 第二行\n-第三行\n"
        );
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> Result<()> {
    fs::rename(source, destination).with_context(|| {
        format!(
            "publishing edited file {} -> {}",
            source.display(),
            destination.display()
        )
    })
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        return Err(std::io::Error::last_os_error()).context("publishing edited file");
    }
    Ok(())
}

fn sync_parent(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let parent = path
            .parent()
            .context("edited file has no parent directory")?;
        File::open(parent)
            .with_context(|| format!("opening {} for sync", parent.display()))?
            .sync_all()
            .with_context(|| format!("syncing {}", parent.display()))?;
    }
    Ok(())
}
