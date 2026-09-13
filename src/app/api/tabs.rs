use std::path::PathBuf;

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, ResponseResult, TabCreateInPaneParams, TabCreateParams,
    TabListParams, TabMoveParams, TabRenameParams, TabTarget,
};
use crate::app::{App, Mode};

use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_tab_list(&mut self, id: String, params: TabListParams) -> String {
        let tabs = if let Some(workspace_id) = params.workspace_id {
            let Some(ws_idx) = self.parse_workspace_id(&workspace_id) else {
                return workspace_not_found(id, &workspace_id);
            };
            let Some(_) = self.state.workspaces.get(ws_idx) else {
                return workspace_not_found(id, &workspace_id);
            };
            self.tab_list_info(ws_idx)
        } else {
            (0..self.state.workspaces.len())
                .flat_map(|ws_idx| self.tab_list_info(ws_idx))
                .collect()
        };

        encode_success(id, ResponseResult::TabList { tabs })
    }

    pub(super) fn handle_tab_get(&mut self, id: String, target: TabTarget) -> String {
        let Some((ws_idx, pane_id, tab_idx)) = self.parse_public_tab_id(&target.tab_id) else {
            return tab_not_found(id, &target.tab_id);
        };
        let Some(tab) = self.tab_info_for_pane(ws_idx, pane_id, tab_idx) else {
            return tab_not_found(id, &target.tab_id);
        };

        encode_success(id, ResponseResult::TabInfo { tab })
    }

    pub(super) fn handle_tab_create(&mut self, id: String, params: TabCreateParams) -> String {
        self.handle_tab_create_targeted(id, params, None)
    }

    pub(super) fn handle_tab_create_in_pane(
        &mut self,
        id: String,
        params: TabCreateInPaneParams,
    ) -> String {
        let TabCreateInPaneParams {
            workspace_id,
            pane_id,
            cwd,
            focus,
            label,
            env,
        } = params;
        self.handle_tab_create_targeted(
            id,
            TabCreateParams {
                workspace_id,
                cwd,
                focus,
                label,
                env,
            },
            Some(pane_id),
        )
    }

    fn handle_tab_create_targeted(
        &mut self,
        id: String,
        params: TabCreateParams,
        pane_id: Option<String>,
    ) -> String {
        let TabCreateParams {
            workspace_id,
            cwd,
            focus,
            label,
            env,
        } = params;
        let target = if let Some(pane_id) = pane_id.as_deref() {
            let Some((ws_idx, pane_id)) = self.parse_current_public_pane_id(pane_id) else {
                return encode_error(id, "pane_not_found", format!("pane {pane_id} not found"));
            };
            if workspace_id
                .as_deref()
                .is_some_and(|workspace_id| self.parse_workspace_id(workspace_id) != Some(ws_idx))
            {
                return encode_error(
                    id,
                    "invalid_target",
                    "workspace_id and pane_id must identify the same workspace",
                );
            }
            Some((ws_idx, pane_id))
        } else if let Some(workspace_id) = workspace_id.as_deref() {
            self.parse_workspace_id(workspace_id).and_then(|ws_idx| {
                let pane_id = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
                Some((ws_idx, pane_id))
            })
        } else {
            self.state.active.and_then(|ws_idx| {
                let pane_id = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
                Some((ws_idx, pane_id))
            })
        };
        let Some((ws_idx, owner_pane_id)) = target else {
            return match workspace_id {
                Some(workspace_id) => workspace_not_found(id, &workspace_id),
                None => encode_error(id, "workspace_not_found", "no active workspace"),
            };
        };
        let cwd = cwd.map(PathBuf::from).unwrap_or_else(|| {
            self.resolve_new_terminal_cwd(
                self.launch_cwd_for_pane_in_workspace(ws_idx, owner_pane_id),
            )
        });
        let (rows, cols) = self.state.estimate_pane_size();
        let default_shell = self.state.default_shell.clone();
        let scrollback_limit_bytes = self.state.pane_scrollback_limit_bytes;
        let host_terminal_theme = self.state.host_terminal_theme;
        let host_terminal_appearance = self.state.host_terminal_appearance;
        let extra_env = match super::env::normalize_launch_env(env) {
            Ok(env) => env,
            Err((code, message)) => return encode_error(id, &code, message),
        };
        let result = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .ok_or_else(|| std::io::Error::other("workspace disappeared"))
            .and_then(|ws| {
                ws.create_tab_in_pane(
                    owner_pane_id,
                    rows,
                    cols,
                    cwd,
                    scrollback_limit_bytes,
                    host_terminal_theme,
                    host_terminal_appearance,
                    crate::pane::PaneShellConfig::new(&default_shell, self.state.shell_mode),
                    extra_env,
                )
            });
        match result {
            Ok((tab_idx, terminal, runtime)) => {
                self.terminal_runtimes.insert(terminal.id.clone(), runtime);
                self.state.terminals.insert(terminal.id.clone(), terminal);
                if let Some(label) = label {
                    let tab_id = self
                        .public_tab_id_for_pane(ws_idx, owner_pane_id, tab_idx)
                        .expect("new tab must have a public ID");
                    if let Some(tab) = self
                        .state
                        .workspaces
                        .get_mut(ws_idx)
                        .and_then(|ws| ws.pane_state_mut(owner_pane_id))
                        .and_then(|pane| pane.tabs.get_mut(tab_idx))
                    {
                        tab.set_custom_name(label);
                        crate::logging::tab_renamed(&self.public_workspace_id(ws_idx), &tab_id);
                    }
                }
                let focus_change =
                    focus.then(|| self.focus_pane_owned_tab(ws_idx, owner_pane_id, tab_idx));
                self.schedule_session_save();
                self.emit_tab_created_event_for_pane(ws_idx, owner_pane_id, tab_idx);
                if focus_change
                    .is_some_and(|(pane_changed, tab_changed)| pane_changed || tab_changed)
                {
                    self.emit_tab_focused_event(ws_idx, owner_pane_id, tab_idx);
                }
                encode_success(
                    id,
                    self.tab_created_result_for_pane(ws_idx, owner_pane_id, tab_idx)
                        .expect("new tab should produce a complete create response"),
                )
            }
            Err(err) => encode_error(id, "tab_create_failed", err.to_string()),
        }
    }

    pub(super) fn handle_tab_focus(&mut self, id: String, target: TabTarget) -> String {
        let Some((ws_idx, pane_id, tab_idx)) = self.parse_public_tab_id(&target.tab_id) else {
            return tab_not_found(id, &target.tab_id);
        };
        let (pane_changed, tab_changed) = self.focus_pane_owned_tab(ws_idx, pane_id, tab_idx);
        self.schedule_session_save();
        let tab = self
            .tab_info_for_pane(ws_idx, pane_id, tab_idx)
            .expect("resolved tab must remain available");
        if pane_changed || tab_changed {
            self.emit_tab_focused_event(ws_idx, pane_id, tab_idx);
        }
        encode_success(id, ResponseResult::TabInfo { tab })
    }

    pub(super) fn handle_tab_rename(&mut self, id: String, params: TabRenameParams) -> String {
        let Some((ws_idx, pane_id, tab_idx)) = self.parse_public_tab_id(&params.tab_id) else {
            return tab_not_found(id, &params.tab_id);
        };
        let tab_id = self
            .public_tab_id_for_pane(ws_idx, pane_id, tab_idx)
            .expect("resolved tab must have a public ID");
        let workspace_id = self.public_workspace_id(ws_idx);
        let Some(tab) = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.pane_state_mut(pane_id))
            .and_then(|pane| pane.tabs.get_mut(tab_idx))
        else {
            return tab_not_found(id, &params.tab_id);
        };
        tab.set_custom_name(params.label.clone());
        crate::logging::tab_renamed(&workspace_id, &tab_id);
        self.schedule_session_save();
        self.emit_event(EventEnvelope {
            event: EventKind::TabRenamed,
            data: EventData::TabRenamed {
                tab_id,
                workspace_id,
                label: params.label,
            },
        });
        let tab = self
            .tab_info_for_pane(ws_idx, pane_id, tab_idx)
            .expect("renamed tab must remain available");

        encode_success(id, ResponseResult::TabInfo { tab })
    }

    pub(super) fn handle_tab_move(&mut self, id: String, params: TabMoveParams) -> String {
        let Some((ws_idx, pane_id, tab_idx)) = self.parse_public_tab_id(&params.tab_id) else {
            return tab_not_found(id, &params.tab_id);
        };
        let Some(pane) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.pane_state(pane_id))
        else {
            return tab_not_found(id, &params.tab_id);
        };
        if params.insert_index > pane.tabs.len() {
            return encode_error(
                id,
                "tab_move_failed",
                format!("insert_index {} is out of bounds", params.insert_index),
            );
        }

        let tab_id = self
            .public_tab_id_for_pane(ws_idx, pane_id, tab_idx)
            .expect("resolved tab must have a public ID");
        let workspace_id = self.public_workspace_id(ws_idx);
        let insert_index = params.insert_index;
        let moved = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.pane_state_mut(pane_id))
            .is_some_and(|pane| pane.move_tab(tab_idx, insert_index));
        let tabs = self.tab_list_info_for_pane(ws_idx, pane_id);
        if moved {
            self.schedule_session_save();
            self.emit_event(EventEnvelope {
                event: EventKind::TabMoved,
                data: EventData::TabMoved {
                    tab_id,
                    workspace_id,
                    insert_index,
                    tabs: tabs.clone(),
                },
            });
        }

        encode_success(id, ResponseResult::TabList { tabs })
    }

    pub(super) fn handle_tab_close(&mut self, id: String, target: TabTarget) -> String {
        let Some((ws_idx, pane_id, tab_idx)) = self.parse_public_tab_id(&target.tab_id) else {
            return tab_not_found(id, &target.tab_id);
        };
        let tab_id = self
            .public_tab_id_for_pane(ws_idx, pane_id, tab_idx)
            .expect("resolved tab must have a public ID");
        let workspace_id = self.public_workspace_id(ws_idx);
        let Some((pane_tab_count, pane_count, terminal_id, public_pane_id)) =
            self.state.workspaces.get(ws_idx).and_then(|ws| {
                let pane = ws.pane_state(pane_id)?;
                Some((
                    pane.tabs.len(),
                    ws.layout.pane_count(),
                    pane.tabs.get(tab_idx)?.terminal_id.clone(),
                    self.public_pane_id(ws_idx, pane_id)?,
                ))
            })
        else {
            return tab_not_found(id, &target.tab_id);
        };
        let closes_workspace = pane_tab_count == 1 && pane_count == 1;
        if closes_workspace && self.state.confirm_implicit_worktree_group_close(ws_idx) {
            return encode_error(
                id,
                "confirmation_required",
                "closing this tab would close a worktree group",
            );
        }
        let workspace_snapshot = closes_workspace.then(|| self.workspace_info(ws_idx));

        if pane_tab_count > 1 {
            let removed = self
                .state
                .workspaces
                .get_mut(ws_idx)
                .and_then(|ws| ws.pane_state_mut(pane_id))
                .is_some_and(|pane| {
                    matches!(
                        pane.close_tab(tab_idx),
                        crate::pane::PaneTabCloseOutcome::Removed(_)
                    )
                });
            if !removed {
                return encode_error(
                    id,
                    "tab_close_failed",
                    format!("tab {} could not be closed", target.tab_id),
                );
            }
        } else if closes_workspace {
            self.state.selected = ws_idx;
            self.state.close_selected_workspace();
        } else if let Some(ws) = self.state.workspaces.get_mut(ws_idx) {
            ws.close_pane(pane_id);
            self.state.remove_plugin_pane_records([pane_id]);
        }

        self.state.remove_unattached_terminal_ids([terminal_id]);
        self.shutdown_detached_terminal_runtimes();
        self.schedule_session_save();
        self.emit_event(EventEnvelope {
            event: EventKind::TabClosed,
            data: EventData::TabClosed {
                tab_id,
                workspace_id: workspace_id.clone(),
            },
        });
        if pane_tab_count == 1 {
            self.emit_event(EventEnvelope {
                event: EventKind::PaneClosed,
                data: EventData::PaneClosed {
                    pane_id: public_pane_id,
                    workspace_id: workspace_id.clone(),
                },
            });
        }
        if let Some(workspace) = workspace_snapshot {
            self.emit_event(EventEnvelope {
                event: EventKind::WorkspaceClosed,
                data: EventData::WorkspaceClosed {
                    workspace_id,
                    workspace: Some(workspace),
                },
            });
        } else if pane_tab_count == 1 {
            self.emit_layout_updated_event(ws_idx, 0);
        }

        encode_success(id, ResponseResult::Ok {})
    }

    fn focus_pane_owned_tab(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        tab_idx: usize,
    ) -> (bool, bool) {
        let was_active = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.pane_state(pane_id))
            .is_some_and(|pane| pane.active_tab == tab_idx);
        let pane_changed = self.state.focus_pane_in_workspace(ws_idx, pane_id);
        let resolved = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.pane_state_mut(pane_id))
            .is_some_and(|pane| pane.switch_tab(tab_idx));
        let tab_changed = resolved && !was_active;
        if tab_changed {
            self.state.mark_session_dirty();
        }
        self.state.mode = Mode::Terminal;
        (pane_changed, tab_changed)
    }

    fn emit_tab_focused_event(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        tab_idx: usize,
    ) {
        let Some(tab_id) = self.public_tab_id_for_pane(ws_idx, pane_id, tab_idx) else {
            return;
        };
        self.emit_event(EventEnvelope {
            event: EventKind::TabFocused,
            data: EventData::TabFocused {
                tab_id,
                workspace_id: self.public_workspace_id(ws_idx),
            },
        });
    }

    fn tab_list_info(&self, ws_idx: usize) -> Vec<crate::api::schema::TabInfo> {
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return Vec::new();
        };
        ws.layout
            .pane_ids()
            .into_iter()
            .flat_map(|pane_id| self.tab_list_info_for_pane(ws_idx, pane_id))
            .collect()
    }

    fn tab_list_info_for_pane(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Vec<crate::api::schema::TabInfo> {
        self.state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.pane_state(pane_id))
            .map(|pane| {
                (0..pane.tabs.len())
                    .filter_map(|tab_idx| self.tab_info_for_pane(ws_idx, pane_id, tab_idx))
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn workspace_not_found(id: String, workspace_id: &str) -> String {
    encode_error(
        id,
        "workspace_not_found",
        format!("workspace {workspace_id} not found"),
    )
}

fn tab_not_found(id: String, tab_id: &str) -> String {
    encode_error(id, "tab_not_found", format!("tab {tab_id} not found"))
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{exiting_test_command, shutdown_test_runtimes};
    use super::*;
    use crate::{
        api::schema::SuccessResponse,
        config::{Config, ShellModeConfig},
        workspace::Workspace,
    };

    #[test]
    fn api_tab_close_last_tab_closes_pane_and_workspace_events() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            event_hub.clone(),
        );
        app.state.workspaces = vec![Workspace::test_new("tabs")];
        app.state.active = Some(0);
        app.state.selected = 0;
        let pane_id = app.state.workspaces[0].root_pane;
        let tab_id = app.public_tab_id_for_pane(0, pane_id, 0).unwrap();
        let workspace_id = app.public_workspace_id(0);

        let response = app.handle_tab_close(
            "req".into(),
            TabTarget {
                tab_id: tab_id.clone(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.result, ResponseResult::Ok {});
        assert!(app.state.workspaces.is_empty());
        assert!(app.state.active.is_none());
        let events = event_hub.events_after(0);
        assert_eq!(
            events
                .iter()
                .map(|(_, event)| event.event)
                .collect::<Vec<_>>(),
            [
                EventKind::TabClosed,
                EventKind::PaneClosed,
                EventKind::WorkspaceClosed
            ]
        );
        assert!(matches!(
            &events[0].1.data,
            EventData::TabClosed {
                tab_id: closed_tab_id,
                workspace_id: closed_workspace_id,
            } if closed_tab_id == &tab_id && closed_workspace_id == &workspace_id
        ));
        assert!(matches!(
            &events[2].1.data,
            EventData::WorkspaceClosed {
                workspace_id: closed_workspace_id,
                workspace: Some(workspace),
            } if closed_workspace_id == &workspace_id
                && workspace.workspace_id == workspace_id
        ));
    }

    #[test]
    fn api_tab_move_reorders_tabs_in_target_workspace() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            event_hub.clone(),
        );
        let mut workspace = Workspace::test_new("tabs");
        workspace.test_add_tab(Some("two"));
        workspace.test_add_tab(Some("three"));
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        let moved_number = app.state.workspaces[0].tab(0).unwrap().number;
        let pane_id = app.state.workspaces[0].root_pane;
        let moved_id = app.public_tab_id_for_pane(0, pane_id, 0).unwrap();

        let response = app.handle_tab_move(
            "req".into(),
            TabMoveParams {
                tab_id: moved_id.clone(),
                insert_index: 3,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TabList { tabs } = success.result else {
            panic!("expected tab list");
        };
        assert_eq!(app.state.workspaces[0].tab(2).unwrap().number, moved_number);
        assert_eq!(
            tabs[2].tab_id,
            app.public_tab_id_for_pane(0, pane_id, 2).unwrap()
        );
        let events = event_hub.events_after(0);
        assert!(events.iter().any(|(_, event)| {
            matches!(
                &event.data,
                EventData::TabMoved {
                    tab_id,
                    workspace_id,
                    insert_index: 3,
                    tabs,
                } if tab_id == &moved_id
                    && workspace_id == &app.public_workspace_id(0)
                    && tabs[2].tab_id == moved_id
            )
        }));
    }

    #[tokio::test]
    async fn tab_create_follows_cached_focused_pane_cwd_without_runtime() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            event_hub,
        );
        app.state.default_shell = exiting_test_command().into();
        app.state.shell_mode = ShellModeConfig::NonLogin;
        let workspace = Workspace::test_new("tabs");
        let focused_pane = workspace.root_pane;
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        let cached_cwd = std::env::temp_dir();
        let terminal_id = app.state.workspaces[0]
            .terminal_id(focused_pane)
            .cloned()
            .unwrap();
        app.state.terminals.get_mut(&terminal_id).unwrap().cwd = cached_cwd.clone();

        let response = app.handle_tab_create(
            "req".into(),
            TabCreateParams {
                workspace_id: None,
                cwd: None,
                focus: false,
                label: None,
                env: Default::default(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::TabCreated { .. }));
        let created_terminal_id = app.state.workspaces[0].tab(1).unwrap().terminal_id.clone();
        let created_cwd = &app.state.terminals[&created_terminal_id].cwd;
        assert_eq!(
            crate::worktree::canonical_or_original(created_cwd),
            crate::worktree::canonical_or_original(&cached_cwd)
        );
        shutdown_test_runtimes(&mut app);
    }

    #[test]
    fn tab_list_and_focus_cover_every_pane_stack() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = Workspace::test_new("tabs");
        let first_pane = workspace.root_pane;
        workspace.test_add_tab_to_pane(first_pane, Some("first-extra"));
        let second_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        let second_tab = workspace.test_add_tab_to_pane(second_pane, Some("second-extra"));
        workspace.layout.focus_pane(first_pane);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;

        let response = app.handle_tab_list("list".into(), TabListParams::default());
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TabList { tabs } = success.result else {
            panic!("expected tab list");
        };
        assert_eq!(tabs.len(), 4);
        assert!(tabs.iter().all(|tab| tab.tab_id.starts_with(&tab.pane_id)));
        assert_eq!(
            tabs.iter()
                .filter(|tab| tab.pane_id == app.public_pane_id(0, second_pane).unwrap())
                .count(),
            2
        );

        let target_id = app
            .public_tab_id_for_pane(0, second_pane, second_tab)
            .unwrap();
        let response = app.handle_tab_focus(
            "focus".into(),
            TabTarget {
                tab_id: target_id.clone(),
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TabInfo { tab } = success.result else {
            panic!("expected tab info");
        };
        assert_eq!(tab.tab_id, target_id);
        assert!(tab.focused);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(second_pane));
        assert_eq!(
            app.state.workspaces[0]
                .pane_state(second_pane)
                .unwrap()
                .active_tab,
            second_tab
        );
        assert!(app.event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(
                &event.data,
                EventData::TabFocused { tab_id, .. } if tab_id == &target_id
            )
        }));
    }

    #[tokio::test]
    async fn tab_create_targets_explicit_pane_without_emitting_pane_created() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            event_hub.clone(),
        );
        app.state.default_shell = exiting_test_command().into();
        app.state.shell_mode = ShellModeConfig::NonLogin;
        let mut workspace = Workspace::test_new("tabs");
        let first_pane = workspace.root_pane;
        let second_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.layout.focus_pane(first_pane);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        let second_public = app.public_pane_id(0, second_pane).unwrap();

        let response = app.handle_tab_create_in_pane(
            "create".into(),
            TabCreateInPaneParams {
                workspace_id: None,
                pane_id: second_public.clone(),
                cwd: None,
                focus: false,
                label: Some("logs".into()),
                env: Default::default(),
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TabCreated { tab, root_pane } = success.result else {
            panic!("expected tab create result");
        };
        assert_eq!(tab.pane_id, second_public);
        assert_eq!(root_pane.pane_id, tab.pane_id);
        assert_eq!(root_pane.tab_id, tab.tab_id);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(first_pane));
        assert_eq!(
            app.state.workspaces[0]
                .pane_state(second_pane)
                .unwrap()
                .tabs
                .len(),
            2
        );

        let events = event_hub.events_after(0);
        assert!(events
            .iter()
            .any(|(_, event)| event.event == EventKind::TabCreated));
        assert!(!events
            .iter()
            .any(|(_, event)| event.event == EventKind::PaneCreated));
        shutdown_test_runtimes(&mut app);
    }
}
