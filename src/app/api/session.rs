use crate::api::schema::{ResponseResult, SessionSnapshot};
use crate::app::App;

use super::responses::encode_success;

impl App {
    pub(super) fn handle_session_snapshot(&mut self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::SessionSnapshot {
                snapshot: Box::new(self.session_snapshot()),
            },
        )
    }

    pub(crate) fn session_snapshot(&self) -> SessionSnapshot {
        let focused_workspace_id = self
            .state
            .active
            .map(|ws_idx| self.public_workspace_id(ws_idx));
        let focused_tab_id = self.state.active.and_then(|ws_idx| {
            let ws = self.state.workspaces.get(ws_idx)?;
            let pane_id = ws.focused_pane_id()?;
            let tab_idx = ws.pane_state(pane_id)?.active_tab;
            self.public_tab_id_for_pane(ws_idx, pane_id, tab_idx)
        });
        let focused_pane_id = self.state.active.and_then(|ws_idx| {
            let ws = self.state.workspaces.get(ws_idx)?;
            self.public_pane_id(ws_idx, ws.focused_pane_id()?)
        });

        let mut workspaces = Vec::new();
        let mut tabs = Vec::new();
        let mut layouts = Vec::new();
        for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
            workspaces.push(self.workspace_info(ws_idx));
            for pane_id in ws.layout.pane_ids() {
                let Some(pane) = ws.pane_state(pane_id) else {
                    continue;
                };
                for tab_idx in 0..pane.tabs.len() {
                    if let Some(tab) = self.tab_info_for_pane(ws_idx, pane_id, tab_idx) {
                        tabs.push(tab);
                    }
                }
            }
            if let Some(layout) = self.pane_layout_snapshot_for_focused_pane(ws_idx) {
                layouts.push(layout);
            }
        }

        SessionSnapshot {
            version: crate::build_info::version(),
            protocol: crate::protocol::PROTOCOL_VERSION,
            focused_workspace_id,
            focused_tab_id,
            focused_pane_id,
            workspaces,
            tabs,
            panes: self.collect_panes_for_workspace(None).unwrap_or_default(),
            layouts,
            agents: self.collect_agent_infos(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::api::schema::{EmptyParams, Method, ResponseResult, SuccessResponse};
    use crate::{config::Config, workspace::Workspace};

    fn app_with_two_tabs() -> crate::app::App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = Workspace::test_new("snapshot");
        workspace.test_add_tab(None);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app
    }

    #[test]
    fn session_snapshot_bootstraps_runtime_resources() {
        let mut app = app_with_two_tabs();
        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_snapshot".into(),
            method: Method::SessionSnapshot(EmptyParams::default()),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::SessionSnapshot { snapshot } = success.result else {
            panic!("expected session snapshot response");
        };
        assert_eq!(success.id, "req_snapshot");
        assert_eq!(snapshot.workspaces.len(), 1);
        assert_eq!(snapshot.tabs.len(), 2);
        assert_eq!(snapshot.panes.len(), 1);
        assert_eq!(snapshot.layouts.len(), 1);
        assert_eq!(
            snapshot.focused_workspace_id.as_deref(),
            Some(snapshot.workspaces[0].workspace_id.as_str())
        );
        assert_eq!(
            snapshot.focused_tab_id.as_deref(),
            Some(snapshot.tabs[0].tab_id.as_str())
        );
        assert_eq!(
            snapshot.focused_pane_id.as_deref(),
            Some(snapshot.panes[0].pane_id.as_str())
        );
    }

    #[test]
    fn session_snapshot_lists_all_pane_tabs_with_one_workspace_layout() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = Workspace::test_new("snapshot");
        let first_pane = workspace.root_pane;
        workspace.test_add_tab_to_pane(first_pane, Some("first-extra"));
        let second_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.test_add_tab_to_pane(second_pane, Some("second-extra"));
        workspace.layout.focus_pane(first_pane);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);

        let snapshot = app.session_snapshot();

        assert_eq!(snapshot.tabs.len(), 4);
        assert_eq!(snapshot.panes.len(), 2);
        assert_eq!(snapshot.layouts.len(), 1);
        assert_eq!(
            snapshot.layouts[0].workspace_id,
            snapshot.workspaces[0].workspace_id
        );
        assert_eq!(
            snapshot.layouts[0].pane_id,
            snapshot.workspaces[0].focused_pane_id
        );
        assert!(snapshot
            .tabs
            .iter()
            .any(|tab| tab.pane_id == app.public_pane_id(0, second_pane).unwrap()));
    }
}
