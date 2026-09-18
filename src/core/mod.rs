//! Nole's domain core: data model, persistence, indexing, markup, agent,
//! providers, and the embedded PTY terminal. This layer must not depend on
//! terminal presentation crates; rendering and input live under `tui`.

pub mod agent;
pub mod agent_service;
pub mod agent_session;
pub mod attachment;
pub mod attachment_index;
pub mod attachment_usage;
pub mod document_index;
pub mod embedded_terminal;
pub mod export;
pub mod image_data;
pub mod markup;
pub mod model;
pub mod nole;
pub mod observable;
pub mod provider;
pub mod skill;
pub mod storage;
pub mod wiki_link_index;
pub mod workspace;
pub mod workspace_index;
pub mod workspace_watcher;
