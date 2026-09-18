//! Document browsing, todo/search/tag views, and note actions.
//!
//! Submodules group the `App` methods by feature so each file carries a focused
//! slice of workspace behavior.

mod actions;
mod attachments;
mod document_view;
mod files;
mod search;
mod tags;
mod todos;
mod workspace_views;

pub(crate) use attachments::human_size;
