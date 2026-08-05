//! Central validation policy for files written by Agent tools.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};

pub(super) enum WriteSource<'a> {
    Text(&'a str),
    File(&'a Path),
}

/// Validate the complete candidate at `destination` before a tool changes files.
pub(super) fn validate_write(
    root: &Path,
    destination: &Path,
    source: WriteSource<'_>,
) -> Result<()> {
    let skills = root.join("skills");
    if destination.starts_with(&skills) {
        validate_skill_destination(&skills, destination)?;
        let content = source.read_skill_text()?;
        if let Err(error) = crate::skill::validate_skill_source(content.as_ref()) {
            bail!(
                "invalid Skill document for {}: {error:#}",
                destination.display()
            );
        }
        return Ok(());
    }

    match source {
        WriteSource::Text(content) => validate_mbdown(destination, content)?,
        WriteSource::File(path) if is_mbdown_path(destination) => {
            let content = fs::read_to_string(path)
                .with_context(|| format!("reading Markdown candidate {}", path.display()))?;
            validate_mbdown(destination, &content)?;
        }
        WriteSource::File(_) => {}
    }
    Ok(())
}

fn validate_skill_destination(skills: &Path, destination: &Path) -> Result<()> {
    if destination.parent() != Some(skills) {
        bail!("Skills must be direct files under skills/");
    }
    if destination
        .extension()
        .and_then(|extension| extension.to_str())
        != Some("md")
    {
        bail!("Skill files must use the .md extension");
    }
    let id = destination
        .file_stem()
        .and_then(|name| name.to_str())
        .context("Skill file name must be valid UTF-8")?;
    crate::skill::validate_skill_id(id)
}

impl<'a> WriteSource<'a> {
    fn read_skill_text(self) -> Result<std::borrow::Cow<'a, str>> {
        match self {
            Self::Text(content) => {
                if content.len() as u64 > crate::skill::MAX_SKILL_BYTES {
                    bail!("Skill content exceeds 1 MB");
                }
                Ok(std::borrow::Cow::Borrowed(content))
            }
            Self::File(path) => {
                let metadata = fs::metadata(path)
                    .with_context(|| format!("reading metadata for {}", path.display()))?;
                if metadata.len() > crate::skill::MAX_SKILL_BYTES {
                    bail!("Skill content exceeds 1 MB");
                }
                let content = fs::read_to_string(path)
                    .with_context(|| format!("reading Skill source {}", path.display()))?;
                Ok(std::borrow::Cow::Owned(content))
            }
        }
    }
}

fn is_mbdown_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("mb")
        })
}

fn validate_mbdown(path: &Path, content: &str) -> Result<()> {
    if !is_mbdown_path(path) {
        return Ok(());
    }
    if let Err(error) = mbdown::validate(content) {
        bail!("MBDown validation failed for {}: {error}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_SKILL: &str = "---\ndescription: A useful workflow\n---\n\n# Steps\n";

    #[test]
    fn skill_writes_require_valid_front_matter_and_flat_paths() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::create_dir(root.join("skills")).unwrap();

        validate_write(
            root,
            &root.join("skills/valid-skill.md"),
            WriteSource::Text(VALID_SKILL),
        )
        .unwrap();
        assert!(validate_write(
            root,
            &root.join("skills/broken.md"),
            WriteSource::Text("# Missing front matter"),
        )
        .unwrap_err()
        .to_string()
        .contains("front matter"));
        assert!(validate_write(
            root,
            &root.join("skills/nested/valid-skill.md"),
            WriteSource::Text(VALID_SKILL),
        )
        .unwrap_err()
        .to_string()
        .contains("direct files"));
    }

    #[test]
    fn skills_keep_their_context_size_limit() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::create_dir(root.join("skills")).unwrap();
        let oversized = "x".repeat(crate::skill::MAX_SKILL_BYTES as usize + 1);

        let error = validate_write(
            root,
            &root.join("skills/oversized.md"),
            WriteSource::Text(&oversized),
        )
        .unwrap_err();
        assert!(error.to_string().contains("Skill content exceeds 1 MB"));
    }

    #[test]
    fn file_transfers_into_skills_use_the_same_policy() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::create_dir(root.join("skills")).unwrap();
        let valid = root.join("valid.txt");
        let invalid = root.join("invalid.txt");
        fs::write(&valid, VALID_SKILL).unwrap();
        fs::write(&invalid, "not a Skill").unwrap();

        validate_write(
            root,
            &root.join("skills/transferred.md"),
            WriteSource::File(&valid),
        )
        .unwrap();
        assert!(validate_write(
            root,
            &root.join("skills/transferred.md"),
            WriteSource::File(&invalid),
        )
        .is_err());
    }
}
