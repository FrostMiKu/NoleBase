//! Flat, user-owned Agent skills stored directly under `skills/`.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

pub const MAX_SKILL_BYTES: u64 = 1_000_000;
pub const MAX_SKILL_ID_LEN: usize = 64;
pub const DEFAULT_CREATE_SKILL: &str = include_str!("../assets/default-skills/create-skill.md");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    pub id: String,
    pub description: String,
    pub body: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SkillCatalog {
    pub skills: Vec<Skill>,
    pub warnings: Vec<String>,
}

#[derive(Deserialize)]
struct SkillMetadata {
    description: String,
}

pub fn ensure_skills_directory(directory: &Path) -> Result<()> {
    match fs::create_dir(directory) {
        Ok(()) => {
            let path = directory.join("create-skill.md");
            let result = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .with_context(|| format!("creating default skill {}", path.display()))
                .and_then(|mut file| {
                    file.write_all(DEFAULT_CREATE_SKILL.as_bytes())
                        .with_context(|| format!("writing default skill {}", path.display()))
                });
            if let Err(error) = result {
                let _ = fs::remove_file(&path);
                let _ = fs::remove_dir(directory);
                return Err(error);
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if directory.is_dir() {
                Ok(())
            } else {
                bail!("skills path is not a directory: {}", directory.display())
            }
        }
        Err(error) => Err(error).with_context(|| format!("creating {}", directory.display())),
    }
}

pub fn load_skill_catalog(directory: &Path) -> Result<SkillCatalog> {
    let entries = fs::read_dir(directory)
        .with_context(|| format!("reading skills directory {}", directory.display()))?;
    let mut catalog = SkillCatalog::default();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                catalog
                    .warnings
                    .push(format!("Could not read a skills directory entry: {error}"));
                continue;
            }
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                catalog.warnings.push(format!(
                    "Could not inspect skill {}: {error}",
                    path.display()
                ));
                continue;
            }
        };
        if file_type.is_dir() {
            continue;
        }
        if file_type.is_symlink() || !file_type.is_file() {
            catalog
                .warnings
                .push(format!("Ignored non-regular skill file {}", path.display()));
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
            continue;
        }
        match load_skill(&path) {
            Ok(skill) => catalog.skills.push(skill),
            Err(error) => catalog
                .warnings
                .push(format!("Invalid skill {}: {error}", path.display())),
        }
    }
    catalog.skills.sort_by(|left, right| left.id.cmp(&right.id));
    catalog.warnings.sort();
    Ok(catalog)
}

pub fn load_skill(path: &Path) -> Result<Skill> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("checking skill {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        bail!("skill must be a regular file and cannot be a symlink");
    }
    if metadata.len() > MAX_SKILL_BYTES {
        bail!("skill exceeds the 1 MB size limit");
    }
    let path =
        fs::canonicalize(path).with_context(|| format!("resolving skill {}", path.display()))?;
    let id = path
        .file_stem()
        .and_then(|name| name.to_str())
        .context("skill file name must be valid UTF-8")?;
    validate_skill_id(id)?;
    if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
        bail!("skill file must use the .md extension");
    }
    let source =
        fs::read_to_string(&path).with_context(|| format!("reading skill {}", path.display()))?;
    let (metadata, body) = parse_skill_source(&source)?;
    Ok(Skill {
        id: id.to_string(),
        description: metadata.description,
        body,
        path,
    })
}

pub fn validate_skill_path(directory: &Path, path: &Path) -> Result<PathBuf> {
    let file_type = fs::symlink_metadata(path)
        .with_context(|| format!("checking skill {}", path.display()))?
        .file_type();
    if file_type.is_symlink() || !file_type.is_file() {
        bail!("skill must be a regular file and cannot be a symlink");
    }
    let canonical_directory = fs::canonicalize(directory)
        .with_context(|| format!("resolving skills directory {}", directory.display()))?;
    let canonical =
        fs::canonicalize(path).with_context(|| format!("resolving skill {}", path.display()))?;
    if canonical.parent() != Some(canonical_directory.as_path()) {
        bail!("skill must be a direct child of the skills directory");
    }
    load_skill(&canonical)?;
    Ok(canonical)
}

pub fn rename_skill(directory: &Path, from: &Path, new_id: &str) -> Result<PathBuf> {
    let from = validate_skill_path(directory, from)?;
    validate_skill_id(new_id)?;
    let destination = directory.join(format!("{new_id}.md"));
    if destination.file_name() == from.file_name() {
        return Ok(from);
    }
    match fs::symlink_metadata(&destination) {
        Ok(_) => bail!("a skill named {new_id} already exists"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("checking {}", destination.display()));
        }
    }
    fs::rename(&from, &destination).with_context(|| {
        format!(
            "renaming skill {} to {}",
            from.display(),
            destination.display()
        )
    })?;
    fs::canonicalize(&destination)
        .with_context(|| format!("resolving renamed skill {}", destination.display()))
}

pub fn delete_skill(directory: &Path, path: &Path) -> Result<()> {
    let path = validate_skill_path(directory, path)?;
    fs::remove_file(&path).with_context(|| format!("deleting skill {}", path.display()))
}

pub fn validate_skill_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > MAX_SKILL_ID_LEN {
        bail!("skill id must contain between 1 and {MAX_SKILL_ID_LEN} characters");
    }
    if id.starts_with('-') || id.ends_with('-') {
        bail!("skill id cannot start or end with a hyphen");
    }
    if !id
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        bail!("skill id may contain only lowercase ASCII letters, digits, and hyphens");
    }
    Ok(())
}

fn parse_skill_source(source: &str) -> Result<(SkillMetadata, String)> {
    let normalized = source.replace("\r\n", "\n");
    let source = normalized
        .strip_prefix("---\n")
        .context("skill must start with YAML front matter delimited by ---")?;
    let (front_matter, body) = source
        .split_once("\n---\n")
        .context("skill front matter must end with --- on its own line")?;
    let mut metadata: SkillMetadata =
        serde_yml::from_str(front_matter).context("parsing skill front matter")?;
    metadata.description = metadata.description.trim().to_string();
    if metadata.description.is_empty() {
        bail!("skill description cannot be empty");
    }
    let body = body.trim();
    if body.is_empty() {
        bail!("skill instructions cannot be empty");
    }
    Ok((metadata, body.to_string()))
}

/// Validate a complete Skill document without reading or writing a file.
pub fn validate_skill_source(source: &str) -> Result<()> {
    parse_skill_source(source).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_initialization_seeds_an_editable_create_skill_only_once() {
        let directory = tempfile::tempdir().unwrap();
        let skills = directory.path().join("skills");

        ensure_skills_directory(&skills).unwrap();
        let path = skills.join("create-skill.md");
        assert_eq!(fs::read_to_string(&path).unwrap(), DEFAULT_CREATE_SKILL);

        fs::write(&path, "user changed this").unwrap();
        ensure_skills_directory(&skills).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "user changed this");

        fs::remove_file(&path).unwrap();
        ensure_skills_directory(&skills).unwrap();
        assert!(!path.exists(), "a deleted default skill must stay deleted");
    }

    #[test]
    fn catalog_loads_flat_skills_in_id_order_and_ignores_directories() {
        let directory = tempfile::tempdir().unwrap();
        let skills = directory.path().join("skills");
        fs::create_dir(&skills).unwrap();
        fs::write(
            skills.join("z-last.md"),
            "---\ndescription: Last skill\n---\n\n# Last\n",
        )
        .unwrap();
        fs::write(
            skills.join("a-first.md"),
            "---\ndescription: First skill\n---\n\n# First\n",
        )
        .unwrap();
        fs::create_dir(skills.join("nested")).unwrap();

        let catalog = load_skill_catalog(&skills).unwrap();
        assert_eq!(
            catalog
                .skills
                .iter()
                .map(|skill| skill.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a-first", "z-last"]
        );
        assert!(catalog.warnings.is_empty());
        assert_eq!(catalog.skills[0].body, "# First");
    }

    #[test]
    fn skill_front_matter_accepts_crlf_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("windows.md");
        fs::write(
            &path,
            "---\r\ndescription: Windows lines\r\n---\r\n\r\n# Instructions\r\n",
        )
        .unwrap();

        let skill = load_skill(&path).unwrap();
        assert_eq!(skill.description, "Windows lines");
        assert_eq!(skill.body, "# Instructions");
    }

    #[test]
    fn catalog_skips_invalid_skills_with_actionable_warnings() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("Upper.md"),
            "---\ndescription: Invalid id\n---\nBody\n",
        )
        .unwrap();
        fs::write(directory.path().join("broken.md"), "no front matter").unwrap();

        let catalog = load_skill_catalog(directory.path()).unwrap();
        assert!(catalog.skills.is_empty());
        assert_eq!(catalog.warnings.len(), 2);
        assert!(catalog
            .warnings
            .iter()
            .any(|warning| warning.contains("Upper.md")));
        assert!(catalog
            .warnings
            .iter()
            .any(|warning| warning.contains("broken.md")));
    }

    #[test]
    fn rename_and_delete_stay_within_the_flat_skills_directory() {
        let directory = tempfile::tempdir().unwrap();
        let skills = directory.path().join("skills");
        fs::create_dir(&skills).unwrap();
        let original = skills.join("before.md");
        fs::write(
            &original,
            "---\ndescription: Rename me\n---\n\n# Instructions\n",
        )
        .unwrap();

        let renamed = rename_skill(&skills, &original, "after").unwrap();
        assert_eq!(renamed, fs::canonicalize(skills.join("after.md")).unwrap());
        assert!(!original.exists());
        assert!(rename_skill(&skills, &renamed, "Invalid").is_err());
        assert!(delete_skill(&skills, directory.path()).is_err());

        delete_skill(&skills, &renamed).unwrap();
        assert!(!renamed.exists());
    }
}
