use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;

use super::mutations::{CollaboratorShell, MutationTools};
use super::{
    Backlinks, Calculate, Grep, Notes, Read, SearchNotes, SearchTag, SearchWeb, SkillParser, Tags,
    Wikilink,
};
use crate::core::agent::collaboration::CollaboratorAccess;
use crate::core::agent::Agent;
use crate::core::agent::SnapshotStore;
use crate::core::agent::{AgentJobsHandle, RegisterBank};
use crate::core::wiki_link_index::WikiLinkIndexHandle;
use crate::core::workspace_index::WorkspaceIndexHandle;
use std::sync::atomic::AtomicBool;

/// Shared inputs for the collaborators' inspection tools.
pub(crate) struct AgentTools {
    pub root: PathBuf,
    pub session_directory: PathBuf,
    pub workspace_index: WorkspaceIndexHandle,
    pub wiki_links: WikiLinkIndexHandle,
    pub client: reqwest::Client,
    pub tavily_api_key: String,
    pub skills: Vec<crate::core::skill::Skill>,
    pub mutations: MutationTools,
    pub jobs: AgentJobsHandle,
}

impl AgentTools {
    pub fn register(
        &self,
        runner: &mut Agent,
        access: CollaboratorAccess,
        name: &str,
        cancelled: Arc<AtomicBool>,
    ) -> Result<()> {
        let reads = Arc::new(SnapshotStore::default());
        self.register_inspection(runner, reads.clone())?;
        if access == CollaboratorAccess::ReadWrite {
            let mutations = self.mutations.for_actor(name, cancelled.clone());
            mutations.register(runner, reads, Arc::new(RegisterBank::default()))?;
            runner.register(mutations.wrap(CollaboratorShell(super::Shell::new(
                &self.root,
                mutations.gate.clone(),
                cancelled,
                self.jobs.clone(),
                Arc::default(),
                false,
                0,
            ))));
        }
        Ok(())
    }

    /// Shared inspection tools for the main Agent and both collaborator access levels.
    pub fn register_inspection(
        &self,
        registry: &mut crate::core::agent::Agent,
        reads: Arc<SnapshotStore>,
    ) -> Result<()> {
        let Self {
            root,
            session_directory,
            workspace_index,
            wiki_links,
            client,
            tavily_api_key,
            skills,
            ..
        } = self;
        let mut read =
            Read::new(root, reads, client.clone())?.with_session_directory(session_directory);
        read.register(SkillParser::new(skills));
        registry.register(read);
        registry.register(Notes::new(root)?);
        registry.register(Grep::new(root)?);
        registry.register(SearchNotes::new(root)?);
        registry.register(Tags::new(workspace_index.clone()));
        registry.register(SearchTag::new(root, workspace_index.clone())?);
        registry.register(Wikilink::new(root, wiki_links.clone())?);
        registry.register(Backlinks::new(root, wiki_links.clone())?);
        registry.register(Calculate);
        registry.register(super::ListAttachments::new(root)?);
        registry.register(super::AttachmentInfo::new(root)?);
        if !tavily_api_key.is_empty() {
            registry.register(SearchWeb {
                client: client.clone(),
                api_key: tavily_api_key.clone(),
            });
        }
        Ok(())
    }
}
