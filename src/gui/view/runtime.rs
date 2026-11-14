use super::*;
use crate::core::workspace_watcher::WorkspaceEvent;

impl DesktopView {
    pub(super) fn sync_daily_list(&mut self) {
        if self.list_generation == self.model.generation {
            return;
        }
        let top = self.daily_list.logical_scroll_top();
        self.daily_list.reset(self.model.daily_blocks.len());
        self.daily_list.scroll_to(top);
        self.list_generation = self.model.generation;
        self.refresh_sessions();
        let paths: std::collections::HashSet<_> = self
            .model
            .documents
            .iter()
            .map(|doc| doc.path.as_str())
            .collect();
        self.home
            .selected
            .retain(|path| paths.contains(path.as_str()));
    }

    pub(super) fn poll_runtime(&mut self, cx: &mut Context<Self>) {
        let events = self
            .watcher
            .as_mut()
            .map(|watcher| watcher.poll(&self.model.workspace))
            .unwrap_or_default();
        let mut changed = false;
        for event in events {
            match event {
                WorkspaceEvent::ContentChanged { .. } => {
                    if let Err(error) = self.model.refresh() {
                        self.status = format!("读取更新遇到问题：{error}");
                    }
                    self.sync_daily_list();
                    changed = true;
                }
                WorkspaceEvent::AgentReadsDirty { paths } => {
                    if let Some(agent) = &self.agent {
                        agent.sessions.mark_reads_dirty(&paths);
                    }
                }
                WorkspaceEvent::WatcherFailed(error) => {
                    self.status = error;
                    changed = true;
                }
                _ => {}
            }
        }
        if self.model.refresh_index() {
            if let Some(tag) = self.active_tag.clone() {
                self.model.select_tag(&tag);
            }
            changed = true;
        }
        if let Some(agent) = &mut self.agent {
            if agent.poll() {
                let from = agent.changed_from.min(self.agent_list.item_count());
                self.agent_list.splice(
                    from..self.agent_list.item_count(),
                    agent.rows.len().saturating_sub(from),
                );
                self.refresh_sessions();
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    pub(super) fn open_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        if !self.flush_save(window, cx) {
            return;
        }
        if page == Page::Agent && self.agent.is_none() {
            match super::super::agent::AgentClient::open(self.model.sessions.clone()) {
                Ok(agent) => {
                    self.agent_list.reset(agent.rows.len());
                    self.agent = Some(agent);
                }
                Err(error) => {
                    self.status = format!("打开 Agent 遇到问题：{error}");
                    cx.notify();
                    return;
                }
            }
        }
        self.page = page;
        if page == Page::Daily {
            self.daily_list.scroll_to_end();
        }
        match page {
            Page::Search => self.search.update(cx, |input, cx| input.focus(window, cx)),
            Page::Agent => self
                .agent_input
                .update(cx, |input, cx| input.focus(window, cx)),
            Page::Daily => self.editor.update(cx, |input, cx| input.focus(window, cx)),
            _ => self.focus.focus(window, cx),
        }
        self.model.revision += 1;
        cx.notify();
    }

    pub(super) fn queue_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.page != Page::Search {
            if !self.flush_save(window, cx) {
                return;
            }
            self.page = Page::Search;
        }
        let query = self.search.read(cx).value().trim().to_owned();
        self.model.query = query.clone();
        self.model.search_results.clear();
        self.search_pending = !query.is_empty();
        let workspace = self.model.workspace.clone();
        self.search_task = Some(cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            let results = cx
                .background_spawn(async move {
                    super::super::model::search_workspace(&workspace, &query)
                })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.model.search_results = results;
                view.search_pending = false;
                cx.notify();
            });
        }));
        cx.notify();
    }
}
