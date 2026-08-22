//! Progressive loading for user-owned Agent skills.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::agent::{Tool, ToolExecutionPolicy};
use crate::skill::Skill;

pub struct LoadSkill {
    skills: HashMap<String, String>,
}

impl LoadSkill {
    pub fn new(skills: &[Skill]) -> Self {
        Self {
            skills: skills
                .iter()
                .map(|skill| {
                    let directory = skill.path.parent().unwrap_or(&skill.path);
                    (
                        skill.path.to_string_lossy().into_owned(),
                        format!("Skill directory: {}\n\n{}", directory.display(), skill.body),
                    )
                })
                .collect(),
        }
    }
}

#[async_trait::async_trait]
impl Tool for LoadSkill {
    fn name(&self) -> &'static str {
        "load_skill"
    }
    fn execution_policy(&self) -> ToolExecutionPolicy {
        ToolExecutionPolicy::LocalRead
    }

    fn description(&self) -> &'static str {
        "Load the complete instructions for one available Agent skill by its exact catalog path. Load a relevant skill before following its workflow."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Exact SKILL.md path from the available skills catalog"
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let path = input
            .get("path")
            .and_then(Value::as_str)
            .context("field path must be a string")?;
        self.skills
            .get(path)
            .cloned()
            .with_context(|| format!("unknown or unavailable skill path: {path}"))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn loader_returns_only_the_requested_skill_body() {
        let loader = LoadSkill::new(&[
            Skill {
                name: "alpha".to_string(),
                description: "Alpha".to_string(),
                body: "Alpha body".to_string(),
                path: PathBuf::from("skills/alpha/SKILL.md"),
            },
            Skill {
                name: "beta".to_string(),
                description: "Beta".to_string(),
                body: "Beta body".to_string(),
                path: PathBuf::from("skills/beta/SKILL.md"),
            },
        ]);
        let runtime = crate::agent::test_support::test_runtime();

        assert_eq!(
            runtime
                .block_on(loader.execute(&json!({"path": "skills/beta/SKILL.md"})))
                .unwrap(),
            "Skill directory: skills/beta\n\nBeta body"
        );
        assert!(runtime
            .block_on(loader.execute(&json!({"path": "skills/missing/SKILL.md"})))
            .unwrap_err()
            .to_string()
            .contains("unknown or unavailable skill"));
    }
}
