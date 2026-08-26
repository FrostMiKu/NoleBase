//! The local tool implementations exposed to the model.
//!
//! Each submodule groups tools by theme. The tool structs are re-exported here so
//! the runtime (in [`crate::agent`]) can register them by name; private helpers stay
//! hidden inside their theme module, and only [`util`] and [`workspace_quota`]
//! are shared between themes.

mod attachment_ops;
mod calculator;
mod explore;
mod file_edit;
mod file_ops;
mod file_patch;
mod grep;
mod interactive;
mod jobs;
mod notes_tags;
pub(crate) mod read;
mod review;
mod shell;
mod subagent_tools;
mod todo;
mod util;
mod wait;
pub(crate) mod web;
mod wiki_links;
mod workspace_quota;
mod write_policy;

pub use self::{
    attachment_ops::*, calculator::*, explore::*, file_ops::*, file_patch::*, grep::*,
    interactive::*, jobs::*, notes_tags::*, read::*, review::*, shell::*, todo::*, wait::*,
    web::*, wiki_links::*,
};
