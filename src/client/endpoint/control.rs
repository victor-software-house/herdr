use super::ClientEndpointId;

pub(crate) enum EndpointControlMessage {
    HealthPong,
    Snapshot(Box<crate::protocol::ClientShellSnapshot>),
    Ignored,
}

pub(crate) fn decode_endpoint_control(
    kind: &str,
    data: &str,
) -> Result<EndpointControlMessage, String> {
    if kind == crate::protocol::endpoint::HEALTH_PONG_KIND {
        return Ok(EndpointControlMessage::HealthPong);
    }
    if kind == crate::protocol::endpoint::SNAPSHOT_CODEC_V2 {
        let snapshot: crate::protocol::ClientShellSnapshotV2 = serde_json::from_str(data)
            .map_err(|error| format!("invalid endpoint snapshot: {error}"))?;
        return Ok(EndpointControlMessage::Snapshot(Box::new(
            client_projection_from_v2(snapshot),
        )));
    }
    if kind == crate::protocol::endpoint::ENDPOINT_SNAPSHOT_KIND {
        let snapshot = serde_json::from_str(data)
            .map_err(|error| format!("invalid endpoint snapshot: {error}"))?;
        return Ok(EndpointControlMessage::Snapshot(Box::new(snapshot)));
    }
    if kind.starts_with("shell.snapshot.") {
        return Err(format!(
            "unsupported mandatory endpoint snapshot codec {kind:?}"
        ));
    }
    Ok(EndpointControlMessage::Ignored)
}

fn client_projection_from_v2(
    snapshot: crate::protocol::ClientShellSnapshotV2,
) -> crate::protocol::ClientShellSnapshot {
    let focused_workspace_id = snapshot.focused_workspace_id.clone();
    let focused_workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.focused);
    let focused_pane_id = focused_workspace.map(|workspace| workspace.focused_pane_id.clone());
    let focused_tab_id = focused_workspace
        .and_then(|workspace| workspace.panes.iter().find(|pane| pane.focused))
        .map(|pane| pane.active_tab_id.clone());
    let mut workspaces = Vec::new();
    let mut tabs = Vec::new();
    let mut panes = Vec::new();
    let mut agents = Vec::new();
    for workspace in &snapshot.workspaces {
        let Some(focused_pane) = workspace
            .panes
            .iter()
            .find(|pane| pane.pane_id == workspace.focused_pane_id)
        else {
            continue;
        };
        workspaces.push(crate::protocol::ClientShellWorkspace {
            workspace_id: workspace.workspace_id.clone(),
            active_tab_id: focused_pane.active_tab_id.clone(),
            new_workspace_cwd: workspace.new_workspace_cwd.clone(),
            number: workspace.number,
            label: workspace.label.clone(),
            custom_label: workspace.custom_label,
            branch: workspace.branch.clone(),
            git_ahead_behind: workspace.git_ahead_behind,
            tokens: workspace.tokens.clone(),
            worktree: workspace.worktree.clone(),
            focused: workspace.focused,
            agent_status: workspace.agent_status,
        });
        tabs.extend(
            focused_pane
                .tabs
                .iter()
                .map(|tab| crate::protocol::ClientShellTab {
                    tab_id: tab.tab_id.clone(),
                    workspace_id: workspace.workspace_id.clone(),
                    number: tab.number,
                    label: tab.label.clone(),
                    custom_label: tab.custom_label,
                    zoomed: workspace.zoomed,
                    focused: workspace.focused && tab.active,
                    agent_status: tab.agent_status,
                }),
        );
        for pane in &workspace.panes {
            let Some(active_tab) = pane.tabs.iter().find(|tab| tab.active) else {
                continue;
            };
            panes.push(crate::protocol::ClientShellPane {
                pane_id: pane.pane_id.clone(),
                workspace_id: workspace.workspace_id.clone(),
                tab_id: active_tab.tab_id.clone(),
                label: active_tab.custom_label.then(|| active_tab.label.clone()),
                cwd: active_tab.cwd.clone(),
                foreground_cwd: active_tab.foreground_cwd.clone(),
                focused: workspace.focused && pane.focused,
                right_click_passthrough: pane.right_click_passthrough,
            });
            agents.extend(pane.tabs.iter().filter_map(|tab| {
                let agent = tab.agent.as_ref()?;
                Some(crate::protocol::ClientShellAgent {
                    pane_id: agent.pane_id.clone(),
                    workspace_id: agent.workspace_id.clone(),
                    tab_id: agent.tab_id.clone(),
                    name: agent.name.clone(),
                    display_agent: agent.display_agent.clone(),
                    agent: agent.agent.clone(),
                    title: agent.title.clone(),
                    terminal_title: agent.terminal_title.clone(),
                    terminal_title_stripped: agent.terminal_title_stripped.clone(),
                    agent_status: agent.agent_status,
                    state_change_seq: agent.state_change_seq,
                    state_labels: agent.state_labels.clone(),
                    tokens: agent.tokens.clone(),
                    focused: agent.focused,
                })
            }));
        }
    }
    crate::protocol::ClientShellSnapshot {
        boot_id: snapshot.boot_id,
        revision: snapshot.revision,
        config_diagnostic: snapshot.config_diagnostic,
        product_announcement: snapshot.product_announcement,
        update_available: snapshot.update_available,
        update_install_command: snapshot.update_install_command,
        server_keybindings_toml: snapshot.server_keybindings_toml,
        latest_release_notes_available: snapshot.latest_release_notes_available,
        integration_updates_available: snapshot.integration_updates_available,
        worktree_directory: snapshot.worktree_directory,
        release_notes: snapshot.release_notes,
        focused_workspace_id,
        focused_tab_id,
        focused_pane_id,
        tab_bar_right: snapshot.tab_bar_right,
        tab_bar_right_separator: snapshot.tab_bar_right_separator,
        agent_view_label: snapshot.agent_view_label,
        agent_order: snapshot.agent_order,
        workspaces,
        tabs,
        panes,
        agents,
        commands: snapshot.commands,
    }
}

pub(crate) fn protocol_failure_is_fatal(endpoint_id: &ClientEndpointId) -> bool {
    endpoint_id.is_local()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::endpoint::ProfileId;

    #[test]
    fn unknown_optional_controls_are_ignored() {
        assert!(matches!(
            decode_endpoint_control("future.optional", "not json").unwrap(),
            EndpointControlMessage::Ignored
        ));
    }

    #[test]
    fn snapshot_v2_preserves_inactive_agent_tab_identity() {
        let message = decode_endpoint_control(
            crate::protocol::endpoint::SNAPSHOT_CODEC_V2,
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/endpoint-snapshot-v2.json"
            )),
        )
        .unwrap();
        let EndpointControlMessage::Snapshot(snapshot) = message else {
            panic!("expected snapshot");
        };
        assert_eq!(snapshot.tabs.len(), 2);
        assert_eq!(snapshot.panes.len(), 2);
        assert_eq!(snapshot.agents.len(), 1);
        assert_eq!(snapshot.agents[0].pane_id, "w_alpha:p1");
        assert_eq!(snapshot.agents[0].tab_id, "w_alpha:p1:t2");
        assert!(!snapshot.agents[0].focused);
    }

    #[test]
    fn unknown_snapshot_codecs_are_rejected() {
        assert_eq!(
            decode_endpoint_control("shell.snapshot.v3", "{}")
                .err()
                .as_deref(),
            Some("unsupported mandatory endpoint snapshot codec \"shell.snapshot.v3\"")
        );
    }

    #[test]
    fn only_local_protocol_failures_end_the_client() {
        let remote =
            ClientEndpointId::Ssh(ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap());
        assert!(protocol_failure_is_fatal(&ClientEndpointId::Local));
        assert!(!protocol_failure_is_fatal(&remote));
    }
}
