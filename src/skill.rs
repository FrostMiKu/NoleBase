//! Standard Agent Skills stored as `<skills-root>/<name>/SKILL.md`.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

pub const MAX_SKILL_BYTES: u64 = 1_000_000;
pub const MAX_SKILL_NAME_LEN: usize = 64;
pub const MAX_SKILL_DESCRIPTION_LEN: usize = 1024;
pub const SKILL_FILE_NAME: &str = "SKILL.md";
pub const DEFAULT_CREATE_SKILL: &str = include_str!("../assets/default-skills/create-skill.md");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
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
    name: String,
    description: String,
}

/// Create the workspace skills root and seed its standard skill creator once.
pub fn ensure_skills_directory(directory: &Path) -> Result<()> {
    match fs::create_dir(directory) {
        Ok(()) => {
            let skill_directory = directory.join("create-skill");
            let path = skill_directory.join(SKILL_FILE_NAME);
            let result = fs::create_dir(&skill_directory)
                .with_context(|| {
                    format!(
                        "creating default skill directory {}",
                        skill_directory.display()
                    )
                })
                .and_then(|()| {
                    OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)
                        .with_context(|| format!("creating default skill {}", path.display()))
                })
                .and_then(|mut file| {
                    file.write_all(DEFAULT_CREATE_SKILL.as_bytes())
                        .with_context(|| format!("writing default skill {}", path.display()))
                });
            if let Err(error) = result {
                let _ = fs::remove_file(&path);
                let _ = fs::remove_dir(&skill_directory);
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

/// Load standard skills directly below one skills root.
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
        let directory_path = entry.path();
        let metadata = match fs::metadata(&directory_path) {
            Ok(metadata) => metadata,
            Err(error) => {
                catalog.warnings.push(format!(
                    "Could not inspect skill directory {}: {error}",
                    directory_path.display()
                ));
                continue;
            }
        };
        if !metadata.is_dir() {
            continue;
        }
        let path = directory_path.join(SKILL_FILE_NAME);
        if !path.exists() {
            continue;
        }
        match load_skill(&path) {
            Ok(skill) => catalog.skills.push(skill),
            Err(error) => catalog
                .warnings
                .push(format!("Invalid skill {}: {error}", path.display())),
        }
    }
    sort_catalog(&mut catalog);
    Ok(catalog)
}

/// Load and combine multiple roots. Missing optional roots are ignored.
pub fn load_skill_catalogs<'a>(directories: impl IntoIterator<Item = &'a Path>) -> SkillCatalog {
    let mut combined = SkillCatalog::default();
    for directory in directories {
        if !directory.exists() {
            continue;
        }
        match load_skill_catalog(directory) {
            Ok(mut catalog) => {
                combined.skills.append(&mut catalog.skills);
                combined.warnings.append(&mut catalog.warnings);
            }
            Err(error) => combined.warnings.push(format!(
                "Could not load skills directory {}: {error}",
                directory.display()
            )),
        }
    }
    sort_catalog(&mut combined);
    combined
}

/// The standard per-user skill root included in every Agent run.
pub fn user_skills_directory() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".agents").join("skills"))
}

/// Load workspace skills plus the standard per-user skill root.
pub fn load_default_skill_catalog(workspace_directory: &Path) -> SkillCatalog {
    let user_directory = user_skills_directory();
    let mut directories = vec![workspace_directory];
    if let Some(user_directory) = user_directory.as_deref() {
        directories.push(user_directory);
    }
    load_skill_catalogs(directories)
}

pub fn load_skill(path: &Path) -> Result<Skill> {
    if path.file_name().and_then(|name| name.to_str()) != Some(SKILL_FILE_NAME) {
        bail!("skill instructions must be named {SKILL_FILE_NAME}");
    }
    let directory_name = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .context("skill directory name must be valid UTF-8")?;
    validate_skill_name(directory_name)?;

    let metadata =
        fs::metadata(path).with_context(|| format!("checking skill {}", path.display()))?;
    if !metadata.is_file() {
        bail!("{SKILL_FILE_NAME} must be a regular file");
    }
    if metadata.len() > MAX_SKILL_BYTES {
        bail!("skill exceeds the 1 MB size limit");
    }
    let canonical =
        fs::canonicalize(path).with_context(|| format!("resolving skill {}", path.display()))?;
    let source = fs::read_to_string(&canonical)
        .with_context(|| format!("reading skill {}", path.display()))?;
    let (metadata, body) = parse_skill_source(&source)?;
    if metadata.name != directory_name {
        bail!(
            "skill name `{}` must match its directory `{directory_name}`",
            metadata.name
        );
    }
    Ok(Skill {
        name: metadata.name,
        description: metadata.description,
        body,
        path: canonical,
    })
}

/// Validate a workspace-managed `SKILL.md` and reject symlink escapes.
pub fn validate_skill_path(directory: &Path, path: &Path) -> Result<PathBuf> {
    let file_type = fs::symlink_metadata(path)
        .with_context(|| format!("checking skill {}", path.display()))?
        .file_type();
    if file_type.is_symlink() || !file_type.is_file() {
        bail!("managed {SKILL_FILE_NAME} must be a regular file and cannot be a symlink");
    }
    let canonical_directory = fs::canonicalize(directory)
        .with_context(|| format!("resolving skills directory {}", directory.display()))?;
    let canonical =
        fs::canonicalize(path).with_context(|| format!("resolving skill {}", path.display()))?;
    let skill_directory = canonical
        .parent()
        .context("skill instructions have no parent directory")?;
    if skill_directory.parent() != Some(canonical_directory.as_path())
        || canonical.file_name().and_then(|name| name.to_str()) != Some(SKILL_FILE_NAME)
    {
        bail!("skill must be stored as <skills-root>/<name>/{SKILL_FILE_NAME}");
    }
    let directory_type = fs::symlink_metadata(skill_directory)?.file_type();
    if directory_type.is_symlink() || !directory_type.is_dir() {
        bail!("managed skill directory cannot be a symlink");
    }
    load_skill(&canonical)?;
    Ok(canonical)
}

pub fn rename_skill(directory: &Path, from: &Path, new_name: &str) -> Result<PathBuf> {
    let from = validate_skill_path(directory, from)?;
    validate_skill_name(new_name)?;
    let source_directory = from.parent().context("skill has no parent directory")?;
    let destination_directory = directory.join(new_name);
    if destination_directory.file_name() == source_directory.file_name() {
        return Ok(from);
    }
    match fs::symlink_metadata(&destination_directory) {
        Ok(_) => bail!("a skill named {new_name} already exists"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("checking {}", destination_directory.display()));
        }
    }

    let source =
        fs::read_to_string(&from).with_context(|| format!("reading skill {}", from.display()))?;
    let renamed_source = replace_skill_name(&source, new_name)?;
    fs::write(&from, renamed_source)
        .with_context(|| format!("updating skill name in {}", from.display()))?;
    if let Err(error) = fs::rename(source_directory, &destination_directory) {
        let _ = fs::write(&from, source);
        return Err(error).with_context(|| {
            format!(
                "renaming skill directory {} to {}",
                source_directory.display(),
                destination_directory.display()
            )
        });
    }
    fs::canonicalize(destination_directory.join(SKILL_FILE_NAME)).with_context(|| {
        format!(
            "resolving renamed skill {}",
            destination_directory.display()
        )
    })
}

pub fn delete_skill(directory: &Path, path: &Path) -> Result<()> {
    let path = validate_skill_path(directory, path)?;
    let skill_directory = path.parent().context("skill has no parent directory")?;
    fs::remove_dir_all(skill_directory)
        .with_context(|| format!("deleting skill directory {}", skill_directory.display()))
}

pub fn validate_skill_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > MAX_SKILL_NAME_LEN {
        bail!("skill name must contain between 1 and {MAX_SKILL_NAME_LEN} characters");
    }
    if name.starts_with('-') || name.ends_with('-') {
        bail!("skill name cannot start or end with a hyphen");
    }
    if name.contains("--") {
        bail!("skill name cannot contain consecutive hyphens");
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        bail!("skill name may contain only lowercase ASCII letters, digits, and hyphens");
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
    metadata.name = metadata.name.trim().to_string();
    metadata.description = metadata.description.trim().to_string();
    validate_skill_name(&metadata.name)?;
    if metadata.description.is_empty() {
        bail!("skill description cannot be empty");
    }
    if metadata.description.chars().count() > MAX_SKILL_DESCRIPTION_LEN {
        bail!("skill description exceeds {MAX_SKILL_DESCRIPTION_LEN} characters");
    }
    let body = body.trim();
    if body.is_empty() {
        bail!("skill instructions cannot be empty");
    }
    Ok((metadata, body.to_string()))
}

/// Validate a complete `SKILL.md` document without a filesystem location.
pub fn validate_skill_source(source: &str) -> Result<()> {
    parse_skill_source(source).map(|_| ())
}

/// Validate a complete `SKILL.md` and require its name to match its directory.
pub fn validate_skill_source_for_name(source: &str, expected_name: &str) -> Result<()> {
    validate_skill_name(expected_name)?;
    let (metadata, _) = parse_skill_source(source)?;
    if metadata.name != expected_name {
        bail!(
            "skill name `{}` must match its directory `{expected_name}`",
            metadata.name
        );
    }
    Ok(())
}

fn replace_skill_name(source: &str, new_name: &str) -> Result<String> {
    validate_skill_source(source)?;
    let normalized = source.replace("\r\n", "\n");
    let source = normalized
        .strip_prefix("---\n")
        .context("skill front matter is malformed")?;
    let (front_matter, body) = source
        .split_once("\n---\n")
        .context("skill front matter is malformed")?;
    let mut output = String::from("---\n");
    let mut replaced = false;
    for line in front_matter.lines() {
        if line.starts_with("name:") {
            if replaced {
                bail!("skill front matter contains duplicate name fields");
            }
            output.push_str(&format!("name: {new_name}\n"));
            replaced = true;
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    if !replaced {
        bail!("skill front matter is missing the name field");
    }
    output.push_str("---\n");
    output.push_str(body);
    validate_skill_source_for_name(&output, new_name)?;
    Ok(output)
}

fn sort_catalog(catalog: &mut SkillCatalog) {
    catalog.skills.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.path.cmp(&right.path))
    });
    catalog.warnings.sort();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(root: &Path, name: &str, description: &str) -> PathBuf {
        let directory = root.join(name);
        fs::create_dir(&directory).unwrap();
        let path = directory.join(SKILL_FILE_NAME);
        fs::write(
            &path,
            format!("---\nname: {name}\ndescription: {description}\n---\n\n# Instructions\n"),
        )
        .unwrap();
        path
    }

    #[test]
    fn first_initialization_seeds_a_standard_create_skill_only_once() {
        let directory = tempfile::tempdir().unwrap();
        let skills = directory.path().join("skills");
        ensure_skills_directory(&skills).unwrap();
        let path = skills.join("create-skill").join(SKILL_FILE_NAME);
        assert_eq!(fs::read_to_string(&path).unwrap(), DEFAULT_CREATE_SKILL);
        fs::write(&path, "user changed this").unwrap();
        ensure_skills_directory(&skills).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "user changed this");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
        ensure_skills_directory(&skills).unwrap();
        assert!(!path.exists(), "a deleted default skill must stay deleted");
    }

    #[test]
    fn catalog_loads_standard_directories_and_ignores_flat_files() {
        let directory = tempfile::tempdir().unwrap();
        let skills = directory.path().join("skills");
        fs::create_dir(&skills).unwrap();
        write_skill(&skills, "z-last", "Last skill");
        write_skill(&skills, "a-first", "First skill");
        fs::create_dir(skills.join("not-a-skill")).unwrap();
        fs::write(skills.join("old-flat.md"), "ignored").unwrap();
        let catalog = load_skill_catalog(&skills).unwrap();
        assert_eq!(
            catalog
                .skills
                .iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>(),
            vec!["a-first", "z-last"]
        );
        assert!(catalog.warnings.is_empty());
    }

    #[test]
    fn skill_accepts_crlf_and_requires_matching_name() {
        let directory = tempfile::tempdir().unwrap();
        let skill_directory = directory.path().join("windows");
        fs::create_dir(&skill_directory).unwrap();
        let path = skill_directory.join(SKILL_FILE_NAME);
        fs::write(
            &path,
            "---\r\nname: windows\r\ndescription: Windows lines\r\n---\r\n\r\n# Instructions\r\n",
        )
        .unwrap();
        let skill = load_skill(&path).unwrap();
        assert_eq!(skill.description, "Windows lines");
        assert_eq!(skill.body, "# Instructions");
        fs::write(
            &path,
            "---\nname: other\ndescription: Wrong name\n---\nBody\n",
        )
        .unwrap();
        assert!(load_skill(&path)
            .unwrap_err()
            .to_string()
            .contains("must match"));
    }

    #[test]
    fn combined_catalog_keeps_same_named_skills_from_multiple_roots() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let user = directory.path().join("user");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&user).unwrap();
        write_skill(&workspace, "shared-name", "Workspace");
        write_skill(&user, "shared-name", "User");
        let catalog = load_skill_catalogs([workspace.as_path(), user.as_path()]);
        assert_eq!(catalog.skills.len(), 2);
        assert!(catalog
            .skills
            .iter()
            .all(|skill| skill.name == "shared-name"));
    }

    #[test]
    fn rename_updates_metadata_and_delete_removes_the_whole_skill() {
        let directory = tempfile::tempdir().unwrap();
        let skills = directory.path().join("skills");
        fs::create_dir(&skills).unwrap();
        let original = write_skill(&skills, "before", "Rename me");
        fs::create_dir(original.parent().unwrap().join("scripts")).unwrap();
        fs::write(original.parent().unwrap().join("scripts/run.sh"), "exit 0").unwrap();
        let renamed = rename_skill(&skills, &original, "after").unwrap();
        assert!(fs::read_to_string(&renamed)
            .unwrap()
            .contains("name: after"));
        assert!(renamed.parent().unwrap().join("scripts/run.sh").exists());
        let renamed_directory = renamed.parent().unwrap().to_path_buf();
        delete_skill(&skills, &renamed).unwrap();
        assert!(!renamed_directory.exists());
    }

    #[test]
    fn standard_names_reject_consecutive_hyphens() {
        assert!(validate_skill_name("valid-name").is_ok());
        assert!(validate_skill_name("bad--name").is_err());
    }
}
