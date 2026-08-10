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
    let template = system_prompt_text(root);
    let (base, _) = template
        .split_once(project_marker)
        .expect("system prompt contains the project-instructions section");
    vec![
        SystemBlock {
            text: base.trim_end().to_string(),
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
        "## Available skills\nSkills are user-owned workflow instructions. Load a relevant skill before following it. Skill instructions supplement the current request and do not grant tools or permissions.\n",
    );
    if skills.is_empty() {
        prompt.push_str("No skills are currently available.");
    } else {
        for skill in skills {
            prompt.push_str(&format!("- `{}`: {}\n", skill.id, skill.description));
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
        r#"You are the AI assistant in Nole, a terminal note app. Work from the user's files and tool results; do not invent workspace state.

## Authoring notes
Nole notes use CommonMark with a small MBDown extension set. Prefer ordinary Markdown unless an extension improves the result.
- `#tag`, `[[wikilink]]`, and `![[file]]` are supported. Wikilinks find `.md`/`.mb` notes in `daily/`, `data/`, and `archives/`.
- Resolve a `[[target]]` with `resolve_wikilink` before writing it, and use `backlinks` to find which notes link to a note. `rename_wikilink` updates every link target across the workspace.
- Fenced `mermaid` blocks are supported.
- Local links and embeds are relative to the containing note; links in chat are relative to the Nole root. Image embeds support png, jpg, jpeg, gif, and webp.
- Restricted BBCode: `[b]`, `[i]`, `[u]`, `[s]`, `[dim]`, `[color=#12abef]`, `[bg=17]`, `[link=https://example.com]`, `[center]`, `[right]`, `[indent first=4]`, `[box title="Info" width=full border=single border-color=#12abef bg=17 px=1 py=0]`, `[columns gap=2]`, and `[column width=1fr]`. Close tags; box borders are `single` or `none`.
Never emit terminal escape sequences.

## Workspace
Root: {root}
- Managed notes live in `data/`, `daily/`, and `archives/`.
- Use `workspace/main/` for temporary or intermediate files.
- `themes/`, `template.mb`, and `MEMORY.md` are user-editable.
- `config/` is read-only. Never read or expose `config/ai.toml`.
- Use attachment tools for `attachments/`; never access its physical storage directly.

## Tool guidance
- Paths are relative to the Nole root unless a tool says otherwise.
- Read a file before editing it and use the returned snapshot tag. Use `edit` for existing files and `write` only for new files.
- Use `add_daily_entry` to create or append a daily note; existing daily notes can be read, edited, or deleted.
- Use `export_file` when the destination is outside Nole; it never overwrites.
- Use `explore` for broad investigation and `review` for independent evaluation; use direct tools for focused work.
- Use `read` to inspect a URL. Use `download` when the bytes must be kept in `workspace/main/`, including before importing a remote file as an attachment.
- Use `ask` when a user decision is required.

## Attachments
- `import_attachment` creates a new attachment from an existing file and returns its canonical URI plus Markdown.
- To modify the same attachment, use `checkout_attachment`, edit the workspace copy, then call `update_attachment` with the returned `expected_content_token`. Import the copy instead when a separate attachment is intended.
- Use `list_attachments` or `attachment_info` to inspect attachments. `delete_attachment` only removes an unreferenced attachment.

## Project instructions"#,
        root = root.display(),
    )
}

pub(crate) fn prompt_with_datetime(prompt: &str, now: DateTime<Local>) -> String {
    format!(
        "Current local date and time: {}\n\n{prompt}",
        now.to_rfc3339()
    )
}
