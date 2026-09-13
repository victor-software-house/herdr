use std::path::PathBuf;

use super::{api_helpers::pane_agent_status, App, Mode};
use crate::api::schema::{EventData, EventEnvelope, EventKind};
use crate::{config::NewTerminalCwdConfig, workspace::Workspace};

pub(crate) fn resolve_new_terminal_cwd(
    policy: &NewTerminalCwdConfig,
    follow_cwd: Option<PathBuf>,
) -> PathBuf {
    match policy {
        NewTerminalCwdConfig::Follow => follow_cwd
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/")),
        NewTerminalCwdConfig::Home => std::env::var_os("HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/")),
        NewTerminalCwdConfig::Current => {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
        }
        NewTerminalCwdConfig::Path(path) => crate::worktree::expand_tilde_path(path),
    }
}

pub(super) fn launch_cwd_for_terminal(
    terminal_id: &crate::terminal::TerminalId,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
) -> Option<PathBuf> {
    terminal_runtimes
        .get(terminal_id)
        .and_then(|runtime| runtime.follow_cwd())
        .or_else(|| {
            terminals
                .get(terminal_id)
                .map(|terminal| terminal.cwd.clone())
        })
}

impl App {
    pub(super) fn seed_cwd_from_workspace(&self, ws_idx: usize) -> Option<PathBuf> {
        self.state
            .workspaces
            .get(ws_idx)?
            .resolved_identity_cwd_from(&self.state.terminals, &self.terminal_runtimes)
    }

    pub(super) fn launch_cwd_for_pane_in_workspace(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<PathBuf> {
        let workspace = self.state.workspaces.get(ws_idx)?;
        launch_cwd_for_terminal(
            workspace.terminal_id(pane_id)?,
            &self.state.terminals,
            &self.terminal_runtimes,
        )
    }

    pub(super) fn focused_pane_cwd_in_workspace(&self, ws_idx: usize) -> Option<PathBuf> {
        let pane_id = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
        self.launch_cwd_for_pane_in_workspace(ws_idx, pane_id)
    }

    pub(super) fn resolve_new_terminal_cwd(&self, follow_cwd: Option<PathBuf>) -> PathBuf {
        resolve_new_terminal_cwd(&self.state.new_terminal_cwd, follow_cwd)
    }

    pub(crate) fn resolved_new_workspace_cwd_from(&self, ws_idx: usize) -> PathBuf {
        let tab_idx = self
            .state
            .workspaces
            .get(ws_idx)
            .map(crate::workspace::Workspace::active_tab_index);
        self.resolved_new_workspace_cwd_from_tab(ws_idx, tab_idx)
    }

    pub(crate) fn resolved_new_workspace_cwd_from_tab(
        &self,
        ws_idx: usize,
        tab_idx: Option<usize>,
    ) -> PathBuf {
        let follow_cwd = tab_idx
            .and_then(|tab_idx| self.state.workspaces.get(ws_idx)?.tab(tab_idx))
            .and_then(|_| self.state.workspaces.get(ws_idx)?.focused_pane_id())
            .and_then(|pane_id| self.launch_cwd_for_pane_in_workspace(ws_idx, pane_id))
            .or_else(|| self.seed_cwd_from_workspace(ws_idx));
        self.resolve_new_terminal_cwd(follow_cwd)
    }

    pub(super) fn workspace_creation_source(&self) -> Option<usize> {
        if self.state.mode == Mode::Navigate
            && self.state.workspaces.get(self.state.selected).is_some()
        {
            return Some(self.state.selected);
        }

        self.state.active.or_else(|| {
            self.state
                .workspaces
                .get(self.state.selected)
                .map(|_| self.state.selected)
        })
    }

    pub(crate) fn create_workspace_with_options(
        &mut self,
        initial_cwd: PathBuf,
        focus: bool,
    ) -> std::io::Result<usize> {
        self.create_workspace_with_launch_env(initial_cwd, focus, Vec::new())
    }

    pub(crate) fn create_workspace_with_launch_env(
        &mut self,
        initial_cwd: PathBuf,
        focus: bool,
        extra_env: Vec<(String, String)>,
    ) -> std::io::Result<usize> {
        let (rows, cols) = self.state.estimate_pane_size();
        let (ws, terminal, runtime) = Workspace::new_with_extra_env(
            initial_cwd,
            rows,
            cols,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            self.state.host_terminal_appearance,
            crate::pane::PaneShellConfig::new(&self.state.default_shell, self.state.shell_mode),
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
            extra_env,
        )?;
        self.terminal_runtimes.insert(terminal.id.clone(), runtime);
        self.state.terminals.insert(terminal.id.clone(), terminal);
        self.state.workspaces.push(ws);
        let idx = self.state.workspaces.len() - 1;
        self.state
            .remove_alias_shadowed_by_new_pane(self.state.workspaces[idx].root_pane);
        let workspace_id = self.state.workspaces[idx].id.clone();
        let root_pane = self.state.workspaces[idx].root_pane.raw();
        crate::logging::workspace_created(&workspace_id, root_pane);
        if focus || self.state.active.is_none() {
            self.state.switch_workspace(idx);
            self.state.mode = Mode::Terminal;
        }
        self.schedule_session_save();
        Ok(idx)
    }

    pub(super) fn collect_panes_for_workspace(
        &self,
        workspace_id: Option<&str>,
    ) -> Result<Vec<crate::api::schema::PaneInfo>, (String, String)> {
        if let Some(workspace_id) = workspace_id {
            let Some(ws_idx) = self.parse_workspace_id(workspace_id) else {
                return Err((
                    "workspace_not_found".into(),
                    format!("workspace {workspace_id} not found"),
                ));
            };
            let Some(ws) = self.state.workspaces.get(ws_idx) else {
                return Err((
                    "workspace_not_found".into(),
                    format!("workspace {workspace_id} not found"),
                ));
            };
            Ok(ws
                .layout
                .pane_ids()
                .into_iter()
                .filter_map(|pane_id| self.pane_info(ws_idx, pane_id))
                .collect())
        } else {
            Ok(self
                .state
                .workspaces
                .iter()
                .enumerate()
                .flat_map(|(ws_idx, ws)| {
                    ws.layout
                        .pane_ids()
                        .into_iter()
                        .filter_map(move |pane_id| self.pane_info(ws_idx, pane_id))
                })
                .collect())
        }
    }

    pub(super) fn tab_info(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::api::schema::TabInfo> {
        let pane_id = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
        self.tab_info_for_pane(ws_idx, pane_id, tab_idx)
    }

    pub(super) fn tab_info_for_pane(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        tab_idx: usize,
    ) -> Option<crate::api::schema::TabInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane = ws.pane_state(pane_id)?;
        let tab = pane.tabs.get(tab_idx)?;
        let terminal = self.state.terminals.get(&tab.terminal_id)?;
        Some(crate::api::schema::TabInfo {
            tab_id: self.public_tab_id_for_pane(ws_idx, pane_id, tab_idx)?,
            workspace_id: self.public_workspace_id(ws_idx),
            pane_id: self.public_pane_id(ws_idx, pane_id)?,
            number: tab.number,
            label: tab
                .custom_name
                .clone()
                .unwrap_or_else(|| (tab_idx + 1).to_string()),
            focused: self.state.active == Some(ws_idx)
                && ws.focused_pane_id() == Some(pane_id)
                && pane.active_tab == tab_idx,
            pane_count: 1,
            agent_status: pane_agent_status(terminal.state, tab.seen),
        })
    }

    pub(crate) fn emit_workspace_open_events(&mut self, ws_idx: usize) {
        let workspace_info = self.workspace_info(ws_idx);
        let Some(tab) = self.tab_info(ws_idx, 0) else {
            return;
        };
        let Some(root_pane) = self.root_pane_info(ws_idx, 0) else {
            return;
        };
        self.emit_event(EventEnvelope {
            event: EventKind::WorkspaceCreated,
            data: EventData::WorkspaceCreated {
                workspace: workspace_info,
            },
        });
        self.emit_tab_and_pane_created_events(tab, root_pane);
        self.emit_layout_updated_event(ws_idx, 0);
    }

    pub(crate) fn emit_tab_created_event_for_pane(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        tab_idx: usize,
    ) {
        let Some(tab) = self.tab_info_for_pane(ws_idx, pane_id, tab_idx) else {
            return;
        };
        self.emit_event(EventEnvelope {
            event: EventKind::TabCreated,
            data: EventData::TabCreated { tab },
        });
    }

    fn emit_tab_and_pane_created_events(
        &mut self,
        tab: crate::api::schema::TabInfo,
        root_pane: crate::api::schema::PaneInfo,
    ) {
        self.emit_event(EventEnvelope {
            event: EventKind::TabCreated,
            data: EventData::TabCreated { tab },
        });
        self.emit_event(EventEnvelope {
            event: EventKind::PaneCreated,
            data: EventData::PaneCreated { pane: root_pane },
        });
    }

    pub(super) fn workspace_created_result(
        &self,
        ws_idx: usize,
    ) -> Option<crate::api::schema::ResponseResult> {
        Some(crate::api::schema::ResponseResult::WorkspaceCreated {
            workspace: self.workspace_info(ws_idx),
            tab: self.tab_info(ws_idx, 0)?,
            root_pane: self.root_pane_info(ws_idx, 0)?,
        })
    }

    #[cfg(test)]
    pub(super) fn tab_created_result(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::api::schema::ResponseResult> {
        let pane_id = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
        self.tab_created_result_for_pane(ws_idx, pane_id, tab_idx)
    }

    pub(super) fn tab_created_result_for_pane(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        tab_idx: usize,
    ) -> Option<crate::api::schema::ResponseResult> {
        Some(crate::api::schema::ResponseResult::TabCreated {
            tab: self.tab_info_for_pane(ws_idx, pane_id, tab_idx)?,
            root_pane: self.pane_info_for_tab(ws_idx, pane_id, tab_idx)?,
        })
    }

    pub(super) fn root_pane_info(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::api::schema::PaneInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane_id = ws.focused_pane_id()?;
        ws.pane_state(pane_id)?.tabs.get(tab_idx)?;
        self.pane_info_for_tab(ws_idx, pane_id, tab_idx)
    }

    pub(super) fn pane_info(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<crate::api::schema::PaneInfo> {
        let tab_idx = self
            .state
            .workspaces
            .get(ws_idx)?
            .pane_state(pane_id)?
            .active_tab;
        self.pane_info_for_tab(ws_idx, pane_id, tab_idx)
    }

    fn pane_info_for_tab(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        tab_idx: usize,
    ) -> Option<crate::api::schema::PaneInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane = ws.pane_state(pane_id)?;
        let pane_tab = pane.tabs.get(tab_idx)?;
        let terminal = self.state.terminals.get(&pane_tab.terminal_id)?;
        let runtime = self
            .terminal_runtimes
            .get(&pane_tab.terminal_id)
            .or_else(|| {
                (pane.active_tab == tab_idx)
                    .then(|| {
                        self.state.runtime_for_pane_in_workspace(
                            &self.terminal_runtimes,
                            ws_idx,
                            pane_id,
                        )
                    })
                    .flatten()
            });
        let scroll = runtime
            .and_then(|runtime| runtime.scroll_metrics())
            .map(|metrics| crate::api::schema::PaneScrollInfo {
                offset_from_bottom: metrics.offset_from_bottom as u64,
                max_offset_from_bottom: metrics.max_offset_from_bottom as u64,
                viewport_rows: metrics.viewport_rows as u64,
            });
        let focused = self.state.active == Some(ws_idx)
            && ws.focused_pane_id() == Some(pane_id)
            && pane.active_tab == tab_idx;
        let foreground_cwd = if pane.active_tab == tab_idx {
            ws.foreground_cwd_for_pane(pane_id, &self.terminal_runtimes)
        } else {
            runtime.and_then(crate::terminal::TerminalRuntime::foreground_cwd)
        };
        let presentation = terminal.effective_presentation();
        Some(crate::api::schema::PaneInfo {
            pane_id: self.public_pane_id(ws_idx, pane_id)?,
            terminal_id: terminal.id.to_string(),
            workspace_id: self.public_workspace_id(ws_idx),
            tab_id: self.public_tab_id_for_pane(ws_idx, pane_id, tab_idx)?,
            focused,
            cwd: runtime
                .and_then(crate::terminal::TerminalRuntime::cwd)
                .unwrap_or_else(|| terminal.cwd.clone())
                .display()
                .to_string()
                .into(),
            foreground_cwd: foreground_cwd.map(|cwd| cwd.display().to_string()),
            label: terminal.manual_label.clone(),
            agent: terminal.effective_agent_label().map(str::to_string),
            title: presentation.title,
            terminal_title: terminal.terminal_title.clone(),
            terminal_title_stripped: terminal.terminal_title_stripped(),
            display_agent: presentation.display_agent,
            agent_status: pane_agent_status(terminal.state, pane_tab.seen),
            state_labels: presentation.state_labels,
            tokens: terminal.metadata_tokens.values(),
            agent_session: terminal_agent_session_info(terminal),
            scroll,
            revision: terminal.revision,
        })
    }

    pub(super) fn lookup_runtime(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<(&crate::terminal::TerminalRuntime, String)> {
        let runtime =
            self.state
                .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)?;
        Some((runtime, self.public_workspace_id(ws_idx)))
    }

    pub(super) fn lookup_runtime_sender(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<&crate::terminal::TerminalRuntime> {
        self.state
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
    }

    pub(super) fn workspace_info(&self, index: usize) -> crate::api::schema::WorkspaceInfo {
        let ws = &self.state.workspaces[index];
        let focused_pane_id = ws
            .focused_pane_id()
            .expect("workspace must have a focused pane");
        let focused_tab_idx = ws
            .pane_state(focused_pane_id)
            .expect("focused pane must exist")
            .active_tab;
        let (agg_state, seen) = ws.aggregate_state(&self.state.terminals);
        crate::api::schema::WorkspaceInfo {
            workspace_id: self.public_workspace_id(index),
            number: index + 1,
            label: ws.display_name_from(&self.state.terminals, &self.terminal_runtimes),
            focused: self.state.active == Some(index),
            pane_count: ws.public_pane_numbers.len(),
            tab_count: ws.panes.values().map(|pane| pane.tabs.len()).sum(),
            focused_pane_id: self
                .public_pane_id(index, focused_pane_id)
                .expect("focused pane must have a public ID"),
            active_tab_id: self
                .public_tab_id_for_pane(index, focused_pane_id, focused_tab_idx)
                .expect("focused tab must have a public ID"),
            agent_status: pane_agent_status(agg_state, seen),
            tokens: ws.metadata_tokens.values(),
            worktree: ws
                .worktree_space()
                .map(|space| crate::api::schema::WorkspaceWorktreeInfo {
                    repo_key: space.key.clone(),
                    repo_name: space.label.clone(),
                    repo_root: space.repo_root.display().to_string(),
                    checkout_path: space.checkout_path.display().to_string(),
                    is_linked_worktree: space.is_linked_worktree,
                }),
        }
    }
}

pub(super) fn terminal_agent_session_info(
    terminal: &crate::terminal::TerminalState,
) -> Option<crate::api::schema::AgentSessionInfo> {
    if let Some(authority) = terminal.hook_authority.as_ref() {
        if let Some(session_ref) = authority.session_ref.as_ref() {
            return Some(crate::api::schema::AgentSessionInfo {
                source: authority.source.clone(),
                agent: authority.agent_label.clone(),
                kind: session_ref.kind,
                value: session_ref.value.clone(),
            });
        }
    }

    terminal
        .persisted_agent_session
        .as_ref()
        .map(|session| crate::api::schema::AgentSessionInfo {
            source: session.source.clone(),
            agent: session.agent.clone(),
            kind: session.session_ref.kind,
            value: session.session_ref.value.clone(),
        })
}
