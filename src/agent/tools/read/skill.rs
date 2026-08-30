//! Skill parser for the unified `read` tool.
//!
//! Skills are loaded progressively: the system prompt carries a catalog of
//! names and descriptions, and `read skill://<name>` returns the full body of
//! one skill on demand — the same way an attachment is reached through its
//! `nole://attachment/<uuid>` URI. Resolution is by skill name; when the same
//! name exists in both the workspace and user roots the workspace copy wins,
//! because the catalog is built workspace-first.

use anyhow::{bail, Result};

use super::{ParseContext, ReadParser, ReadPayload, Target};

/// The skill URI scheme, including the trailing separator.
/// `skill://<name>` is the only form the read tool recognizes.
pub const SKILL_URI_SCHEME: &str = "skill://";

pub struct SkillParser {
    skills: Vec<crate::skill::Skill>,
}

impl SkillParser {
    pub fn new(skills: &[crate::skill::Skill]) -> Self {
        Self {
            skills: skills.to_vec(),
        }
    }
}

#[async_trait::async_trait]
impl ReadParser for SkillParser {
    fn name(&self) -> &'static str {
        "skill"
    }

    fn matches(&self, target: &Target) -> bool {
        matches!(target, Target::Skill { .. })
    }

    async fn parse(
        &self,
        _ctx: &ParseContext,
        target: &Target,
        _input: &serde_json::Value,
    ) -> Result<ReadPayload> {
        let Target::Skill { name } = target else {
            bail!("skill parser received non-skill target");
        };
        match self.skills.iter().find(|skill| skill.name == name.as_str()) {
            Some(skill) => {
                let directory = skill.path.parent().unwrap_or(&skill.path);
                Ok(ReadPayload::Text(format!(
                    "Skill directory: {}\n\n{}",
                    directory.display(),
                    skill.body
                )))
            }
            None => bail!("unknown or unavailable skill: {name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::super::Read;
    use super::SkillParser;
    use crate::agent::SnapshotStore;
    use crate::agent::Tool;
    use crate::skill::Skill;

    fn record(name: &str, body: &str, path: &str) -> Skill {
        Skill {
            name: name.to_string(),
            description: name.to_string(),
            body: body.to_string(),
            path: PathBuf::from(path),
        }
    }

    fn read_with_skills(skills: &[Skill]) -> Read {
        let mut read = Read::new(
            tempfile::tempdir().unwrap().path(),
            std::sync::Arc::new(SnapshotStore::default()),
            reqwest::Client::new(),
        )
        .unwrap();
        read.register(SkillParser::new(skills));
        read
    }

    #[tokio::test]
    async fn skill_uri_returns_the_requested_skill_body() {
        let read = read_with_skills(&[
            record("alpha", "Alpha body", "skills/alpha/SKILL.md"),
            record("beta", "Beta body", "skills/beta/SKILL.md"),
        ]);

        let output = read
            .execute(&json!({"path": "skill://beta"}))
            .await
            .unwrap();
        assert_eq!(output, "Skill directory: skills/beta\n\nBeta body");

        let error = read
            .execute(&json!({"path": "skill://missing"}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("unknown or unavailable skill"));
    }

    #[tokio::test]
    async fn skill_uri_collision_prefers_the_workspace_copy() {
        // The catalog is built workspace-first, so the workspace copy of a
        // same-named skill shadows the user copy.
        let read = read_with_skills(&[
            record("shared", "Workspace body", "skills/shared/SKILL.md"),
            record("shared", "User body", ".agents/skills/shared/SKILL.md"),
        ]);

        let output = read
            .execute(&json!({"path": "skill://shared"}))
            .await
            .unwrap();
        assert!(output.contains("Workspace body"));
    }
}
