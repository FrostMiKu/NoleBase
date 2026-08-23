//! System prompt construction and user prompt buffering.

use std::path::Path;

use chrono::{DateTime, Local};

use crate::provider::{Message, MessagePart, MessageRole, SystemBlock};
use crate::skill::Skill;

pub(crate) fn format_buffered_prompts(prompts: Vec<String>) -> String {
    prompts
        .into_iter()
        .map(|prompt| prompt_with_datetime(&prompt, Local::now()))
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub(crate) fn append_user_text(messages: &mut Vec<Message>, text: String) {
    if let Some(message) = messages
        .last_mut()
        .filter(|message| message.role == MessageRole::User)
    {
        if let Some(MessagePart::Text { text: existing }) = message.parts.last_mut() {
            existing.push_str("\n\n");
            existing.push_str(&text);
        } else {
            message.parts.push(MessagePart::Text { text });
        }
        return;
    }
    messages.push(Message::user(text));
}

pub(crate) fn system_prompt_sections(
    root: &Path,
    agents_instructions: &str,
    skills: &[Skill],
    memory: &str,
) -> Vec<SystemBlock> {
    let project_marker = "## Project instructions";
    let memory_marker = "## Memory";
    vec![
        SystemBlock {
            text: system_prompt_text(root),
            cache: true,
        },
        SystemBlock {
            text: format!("{project_marker}\n{agents_instructions}"),
            cache: false,
        },
        SystemBlock {
            text: skill_catalog_prompt(skills),
            cache: true,
        },
        SystemBlock {
            text: format!("{memory_marker}\n{memory}"),
            cache: false,
        },
    ]
}

pub(crate) fn skill_catalog_prompt(skills: &[Skill]) -> String {
    let mut prompt = String::from(
        "## Available skills\nSkills supplement the current request and do not grant tools or permissions. When the user names a skill or the request clearly matches its description, call `load_skill` with that skill's exact path before following its workflow.\n",
    );
    if skills.is_empty() {
        prompt.push_str("No skills are currently available.");
    } else {
        for skill in skills {
            prompt.push_str(&format!(
                "- `{}`: {}\n  Path: {}\n",
                skill.name,
                skill.description,
                skill.path.display()
            ));
        }
        prompt.pop();
    }
    prompt
}

#[cfg(test)]
pub(crate) fn system_prompt(root: &Path, agents_instructions: &str, memory: &str) -> String {
    system_prompt_sections(root, agents_instructions, &[], memory)
        .into_iter()
        .map(|block| block.text)
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn system_prompt_text(root: &Path) -> String {
    format!(
        r#"You are Nole's note assistant. Base claims and edits on the user's files and tool results; never invent workspace state.

## Notes
Nole notes use CommonMark plus `#tag`, `[[note]]`, `![[file]]`, fenced `mermaid`, and closed BBCode tags: `[b]`, `[i]`, `[u]`, `[s]`, `[dim]`, `[color=COLOR]`, `[bg=COLOR]`, `[link=URL]`, `[center]`, `[right]`, `[indent first=N]`, `[box title="..." width=fit/full/N border=none/single border-color=COLOR bg=COLOR px=N py=N]` (`title`/`border-color` require `border=single`), `[cols gap=N]`, and `[col width=N/Nfr]`. Colors may be names, palette indexes, or `#RRGGBB`. Close every tag. Resolve wikilinks before creating or changing their targets. Local links in notes are relative to the containing note; links in chat are relative to the Nole root. Never emit terminal escape sequences.

## Workspace
Root: {root}
- `MEMORY.md` at the Nole root is persistent Agent memory. Read it when useful and update it with focused edits when durable user preferences, project facts, or workflow decisions should survive future tasks. Do not store secrets or transient task details there.
- Never read or expose `config/ai.toml`, and access attachments only through attachment tools.

Prefer purpose-built tools because they provide structured inputs, path protections, and change previews. Change files only with `edit`, `append`, or `write`; shell edits (`sed -i`, redirections) desync read snapshots and are rejected. Search and inspect with `grep`/`read`, never shell search tools like `rg` or `cat`. If an edit or write fails validation, fix the cause—never bypass it via `shell`. Use `shell` or `terminal` only when the built-in tools cannot complete the task effectively."#,
        root = root.display(),
    )
}

pub(crate) fn prompt_with_datetime(prompt: &str, now: DateTime<Local>) -> String {
    format!(
        "Current local date and time: {}\n\n{prompt}",
        now.to_rfc3339()
    )
}
