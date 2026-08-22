use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use super::{
    Backlinks, Calculate, Grep, ListNotes, ListTags, LoadSkill, Read, ResolveWikilink, SearchFiles,
    SearchTag, SearchWeb,
};
use crate::agent::subagent::SubagentRunner;
use crate::agent::SnapshotStore;
use crate::skill::Skill;
use crate::wiki_link_index::WikiLinkIndexHandle;
use crate::workspace_index::WorkspaceIndexHandle;

/// Register the common read-only tool surface used by both isolated profiles.
/// Profile instructions remain owned by Explore and Review; this helper owns
/// only their intentionally identical capability set and wiring.
pub(super) fn register_read_only_tools(
    runner: &mut SubagentRunner,
    root: &Path,
    workspace_index: WorkspaceIndexHandle,
    wiki_links: WikiLinkIndexHandle,
    client: reqwest::Client,
    tavily_api_key: String,
    skills: &[Skill],
) -> Result<()> {
    let reads = Arc::new(SnapshotStore::default());
    runner.register(Read::new(root, reads, client.clone())?);
    runner.register(ListNotes::new(root)?);
    runner.register(Grep::new(root)?);
    runner.register(SearchFiles::new(root)?);
    runner.register(ListTags::new(workspace_index.clone()));
    runner.register(SearchTag::new(root, workspace_index)?);
    runner.register(ResolveWikilink::new(root, wiki_links.clone())?);
    runner.register(Backlinks::new(root, wiki_links)?);
    runner.register(Calculate);
    runner.register(LoadSkill::new(skills));
    if !tavily_api_key.is_empty() {
        runner.register(SearchWeb {
            client,
            api_key: tavily_api_key,
        });
    }
    Ok(())
}
