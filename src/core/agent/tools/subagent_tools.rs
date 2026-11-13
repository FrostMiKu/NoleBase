use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;

use super::{
    Backlinks, Calculate, Grep, Notes, Read, SearchNotes, SearchTag, SearchWeb, SkillParser, Tags,
    Wikilink,
};
use crate::core::agent::subagent::SubagentRunner;
use crate::core::agent::SnapshotStore;
use crate::core::wiki_link_index::WikiLinkIndexHandle;
use crate::core::workspace_index::WorkspaceIndexHandle;

/// Shared inputs for the collaborators' inspection tools.
pub(crate) struct SubagentTools {
    pub root: PathBuf,
    pub session_directory: PathBuf,
    pub workspace_index: WorkspaceIndexHandle,
    pub wiki_links: WikiLinkIndexHandle,
    pub client: reqwest::Client,
    pub tavily_api_key: String,
    pub skills: Vec<crate::core::skill::Skill>,
}

impl SubagentTools {
    /// Both collaborator profiles use this authoritative capability set.
    pub fn register(&self, runner: &mut SubagentRunner) -> Result<()> {
        let Self {
            root,
            session_directory,
            workspace_index,
            wiki_links,
            client,
            tavily_api_key,
            skills,
        } = self;
        let reads = Arc::new(SnapshotStore::default());
        let mut read =
            Read::new(root, reads, client.clone())?.with_session_directory(session_directory);
        read.register(SkillParser::new(skills));
        runner.register(read);
        runner.register(Notes::new(root)?);
        runner.register(Grep::new(root)?);
        runner.register(SearchNotes::new(root)?);
        runner.register(Tags::new(workspace_index.clone()));
        runner.register(SearchTag::new(root, workspace_index.clone())?);
        runner.register(Wikilink::new(root, wiki_links.clone())?);
        runner.register(Backlinks::new(root, wiki_links.clone())?);
        runner.register(Calculate);
        if !tavily_api_key.is_empty() {
            runner.register(SearchWeb {
                client: client.clone(),
                api_key: tavily_api_key.clone(),
            });
        }
        Ok(())
    }
}
