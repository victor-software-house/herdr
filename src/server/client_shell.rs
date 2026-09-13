use ratatui::layout::Rect;

use crate::app;
use crate::protocol::{self, FrameData};

#[cfg(test)]
pub(super) fn snapshot(
    app: &app::App,
    boot_id: &str,
    revision: u64,
    config_diagnostic: Option<&str>,
    location: Option<&crate::server::clients::ClientShellLocation>,
) -> protocol::ClientShellSnapshot {
    snapshot_v1_from_v2(snapshot_v2(
        app,
        boot_id,
        revision,
        config_diagnostic,
        location,
    ))
}

pub(super) fn snapshot_v2(
    app: &app::App,
    boot_id: &str,
    revision: u64,
    config_diagnostic: Option<&str>,
    location: Option<&crate::server::clients::ClientShellLocation>,
) -> protocol::ClientShellSnapshotV2 {
    let api_snapshot = app.session_snapshot();
    let focused_workspace_id = location
        .and_then(|location| location.focused_workspace_id.clone())
        .or_else(|| api_snapshot.focused_workspace_id.clone());
    let tab_infos = api_snapshot
        .tabs
        .into_iter()
        .map(|tab| (tab.tab_id.clone(), tab))
        .collect::<std::collections::HashMap<_, _>>();
    let agent_infos = api_snapshot
        .agents
        .into_iter()
        .map(|agent| (agent.tab_id.clone(), agent))
        .collect::<std::collections::HashMap<_, _>>();
    let workspace_infos = api_snapshot
        .workspaces
        .into_iter()
        .map(|workspace| (workspace.workspace_id.clone(), workspace))
        .collect::<std::collections::HashMap<_, _>>();

    let workspaces = app
        .state
        .workspaces
        .iter()
        .enumerate()
        .filter_map(|(workspace_index, workspace)| {
            let workspace_id = app.public_workspace_id(workspace_index);
            let info = workspace_infos.get(&workspace_id)?;
            let default_focused_pane = workspace.focused_pane_id()?;
            let focused_pane = location
                .and_then(|location| location.focused_pane_ids.get(&workspace_id))
                .and_then(|pane_id| app.parse_pane_id(pane_id))
                .filter(|(owner, pane_id)| {
                    *owner == workspace_index && workspace.pane_state(*pane_id).is_some()
                })
                .map(|(_, pane_id)| pane_id)
                .unwrap_or(default_focused_pane);
            let focused_pane_id = app.public_pane_id(workspace_index, focused_pane)?;
            let panes = workspace
                .layout
                .pane_ids()
                .into_iter()
                .filter_map(|pane_id| {
                    let public_pane_id = app.public_pane_id(workspace_index, pane_id)?;
                    let pane = workspace.pane_state(pane_id)?;
                    let active_tab_index = location
                        .and_then(|location| location.active_tab_ids.get(&public_pane_id))
                        .and_then(|tab_id| app.parse_public_tab_id(tab_id))
                        .filter(|(owner, owner_pane, _)| {
                            *owner == workspace_index && *owner_pane == pane_id
                        })
                        .map(|(_, _, tab_index)| tab_index)
                        .unwrap_or(pane.active_tab);
                    let active_tab_id =
                        app.public_tab_id_for_pane(workspace_index, pane_id, active_tab_index)?;
                    let tabs = pane
                        .tabs
                        .iter()
                        .enumerate()
                        .filter_map(|(tab_index, pane_tab)| {
                            let tab_id =
                                app.public_tab_id_for_pane(workspace_index, pane_id, tab_index)?;
                            let tab = tab_infos.get(&tab_id)?;
                            let terminal = app.state.terminals.get(&pane_tab.terminal_id)?;
                            let agent = agent_infos.get(&tab_id).map(|agent| {
                                let mut state_labels =
                                    agent.state_labels.clone().into_iter().collect::<Vec<_>>();
                                state_labels.sort_by(|left, right| left.0.cmp(&right.0));
                                let mut tokens =
                                    agent.tokens.clone().into_iter().collect::<Vec<_>>();
                                tokens.sort_by(|left, right| left.0.cmp(&right.0));
                                protocol::ClientShellAgentV2 {
                                    pane_id: public_pane_id.clone(),
                                    workspace_id: workspace_id.clone(),
                                    tab_id: tab_id.clone(),
                                    terminal_id: agent.terminal_id.clone(),
                                    name: agent.name.clone(),
                                    display_agent: agent.display_agent.clone(),
                                    agent: agent.agent.clone(),
                                    title: agent.title.clone(),
                                    terminal_title: agent.terminal_title.clone(),
                                    terminal_title_stripped: agent.terminal_title_stripped.clone(),
                                    agent_status: agent.agent_status,
                                    state_change_seq: agent.state_change_seq,
                                    state_labels,
                                    tokens,
                                    focused: focused_workspace_id.as_deref()
                                        == Some(workspace_id.as_str())
                                        && pane_id == focused_pane
                                        && tab_index == active_tab_index,
                                }
                            });
                            Some(protocol::ClientShellTabV2 {
                                tab_id,
                                terminal_id: pane_tab.terminal_id.to_string(),
                                number: tab.number,
                                label: tab.label.clone(),
                                custom_label: !pane_tab.is_auto_named(),
                                active: tab_index == active_tab_index,
                                cwd: Some(terminal.cwd.display().to_string()),
                                foreground_cwd: app
                                    .terminal_runtimes
                                    .get(&pane_tab.terminal_id)
                                    .and_then(crate::terminal::TerminalRuntime::foreground_cwd)
                                    .map(|cwd| cwd.display().to_string()),
                                agent_status: tab.agent_status,
                                agent,
                            })
                        })
                        .collect();
                    Some(protocol::ClientShellPaneV2 {
                        pane_id: public_pane_id,
                        active_tab_id,
                        focused: pane_id == focused_pane,
                        right_click_passthrough: pane.right_click_passthrough,
                        tabs,
                    })
                })
                .collect();
            let mut tokens = info.tokens.clone().into_iter().collect::<Vec<_>>();
            tokens.sort_by(|left, right| left.0.cmp(&right.0));
            Some(protocol::ClientShellWorkspaceV2 {
                workspace_id: workspace_id.clone(),
                new_workspace_cwd: app
                    .resolved_new_workspace_cwd_from(workspace_index)
                    .display()
                    .to_string(),
                number: info.number,
                label: info.label.clone(),
                custom_label: workspace.custom_name.is_some(),
                branch: workspace.branch(),
                git_ahead_behind: workspace.git_ahead_behind(),
                tokens,
                worktree: info
                    .worktree
                    .as_ref()
                    .map(|worktree| protocol::ClientShellWorktree {
                        key: worktree.repo_key.clone(),
                        label: worktree.repo_name.clone(),
                        is_linked_worktree: worktree.is_linked_worktree,
                    }),
                focused: focused_workspace_id.as_deref() == Some(workspace_id.as_str()),
                focused_pane_id,
                zoomed: workspace.zoomed,
                agent_status: info.agent_status,
                panes,
            })
        })
        .collect::<Vec<_>>();
    let agent_order = crate::ui::agent_panel_entries_from(&app.state, &app.terminal_runtimes)
        .into_iter()
        .filter_map(|entry| app.public_tab_id_for_pane(entry.ws_idx, entry.pane_id, entry.tab_idx))
        .collect();
    let zoomed = workspaces
        .iter()
        .find(|workspace| workspace.focused)
        .is_some_and(|workspace| workspace.zoomed);
    let tab_bar_right = app
        .state
        .tab_bar_right
        .iter()
        .filter_map(|segment| match segment {
            crate::app::state::TabBarStatusSegment::Zoom if zoomed => {
                Some(protocol::ClientShellTabStatusSegment {
                    text: "ZOOM".to_owned(),
                    accent: true,
                })
            }
            crate::app::state::TabBarStatusSegment::Text(Some(text)) if !text.is_empty() => {
                Some(protocol::ClientShellTabStatusSegment {
                    text: text.clone(),
                    accent: false,
                })
            }
            crate::app::state::TabBarStatusSegment::Zoom
            | crate::app::state::TabBarStatusSegment::Text(_) => None,
        })
        .collect();
    let product_announcement = app.state.product_announcement.as_ref().map(|announcement| {
        protocol::ClientShellProductAnnouncement {
            version: announcement.version.clone(),
            id: announcement.id.clone(),
            title: announcement.title.clone(),
            body: announcement.body.clone(),
            preview: announcement.preview,
        }
    });
    let release_notes =
        app.state
            .latest_release_notes
            .as_ref()
            .map(|notes| protocol::ClientShellReleaseNotes {
                version: notes.version.clone(),
                body: notes.body.clone(),
                preview: notes.preview,
            });
    let agent_view_label = app
        .state
        .agent_view_override
        .as_ref()
        .map(|view| view.label.clone().unwrap_or_else(|| "filtered".to_owned()));

    protocol::ClientShellSnapshotV2 {
        boot_id: boot_id.to_owned(),
        revision,
        config_diagnostic: config_diagnostic.map(str::to_owned),
        product_announcement,
        update_available: app.state.update_available.clone(),
        update_install_command: app.state.update_install_command.clone(),
        server_keybindings_toml: app.client_shell_keybindings_profile().map(str::to_owned),
        latest_release_notes_available: app.state.latest_release_notes_available,
        integration_updates_available: app.state.integration_updates_available(),
        worktree_directory: app.state.worktree_directory.to_string_lossy().into_owned(),
        release_notes,
        focused_workspace_id,
        tab_bar_right,
        tab_bar_right_separator: app.state.tab_bar_right_separator.clone(),
        agent_view_label,
        agent_order,
        workspaces,
        commands: app.client_shell_command_manifest(),
    }
}

pub(super) fn snapshot_v1_from_v2(
    snapshot: protocol::ClientShellSnapshotV2,
) -> protocol::ClientShellSnapshot {
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
        workspaces.push(protocol::ClientShellWorkspace {
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
                .map(|tab| protocol::ClientShellTab {
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
            panes.push(protocol::ClientShellPane {
                pane_id: pane.pane_id.clone(),
                workspace_id: workspace.workspace_id.clone(),
                tab_id: active_tab.tab_id.clone(),
                label: active_tab.custom_label.then(|| active_tab.label.clone()),
                cwd: active_tab.cwd.clone(),
                foreground_cwd: active_tab.foreground_cwd.clone(),
                focused: workspace.focused && pane.focused,
                right_click_passthrough: pane.right_click_passthrough,
            });
            if let Some(agent) = active_tab.agent.as_ref() {
                agents.push(protocol::ClientShellAgent {
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
                });
            }
        }
    }
    let agent_order = snapshot
        .agent_order
        .iter()
        .filter_map(|tab_id| agents.iter().find(|agent| &agent.tab_id == tab_id))
        .map(|agent| agent.pane_id.clone())
        .collect();

    protocol::ClientShellSnapshot {
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
        agent_order,
        workspaces,
        tabs,
        panes,
        agents,
        commands: snapshot.commands,
    }
}

pub(super) struct RenderedPaneSurface {
    pub(super) frame: FrameData,
    pub(super) panes: Vec<protocol::PaneSurfacePane>,
    pub(super) splits: Vec<protocol::PaneSurfaceSplit>,
    pub(super) popup: Option<Box<protocol::ClientShellPopupSurface>>,
    pub(super) graphics: protocol::SurfaceGraphicsScene,
    pub(super) graphics_delivery: crate::kitty_graphics::surface::DeliveryCache,
}

pub(super) fn render_pane_surface(
    app: &mut app::App,
    target: Option<crate::ui::TabSurfaceTarget>,
    area: Rect,
    resize_panes: bool,
    show_popup: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
    graphics_delivery: &crate::kitty_graphics::surface::DeliveryCache,
    client_id: u64,
    location: Option<&crate::server::clients::ClientShellLocation>,
) -> RenderedPaneSurface {
    let (client_terminals, focused_pane) = target
        .map(|target| client_pane_terminals(app, target.workspace_index, location))
        .unwrap_or_default();
    let content_revisions_before = target
        .and_then(|target| {
            let workspace = app.state.workspaces.get(target.workspace_index)?;
            Some(
                workspace
                    .layout
                    .pane_ids()
                    .into_iter()
                    .filter_map(|pane_id| {
                        selected_runtime(app, &client_terminals, target.workspace_index, pane_id)
                            .map(|runtime| (pane_id, runtime.content_seq()))
                    })
                    .collect::<std::collections::HashMap<_, _>>(),
            )
        })
        .unwrap_or_default();
    let (buffer, cursor, hyperlinks, layout) =
        crate::server::render_stream::render_tab_surface_virtual(
            &app.state,
            &app.terminal_runtimes,
            target,
            area,
            resize_panes,
            cell_size,
            Some(&client_terminals),
            focused_pane,
        );
    let panes = target
        .map(|target| {
            let workspace_index = target.workspace_index;
            layout
                .pane_infos
                .iter()
                .filter_map(|pane| {
                    app.public_pane_id(workspace_index, pane.id).map(|pane_id| {
                        let runtime =
                            selected_runtime(app, &client_terminals, workspace_index, pane.id);
                        let mouse_reporting =
                            runtime.is_some_and(|runtime| runtime.mouse_reporting_enabled());
                        let sgr_pixel_mouse =
                            runtime.is_some_and(|runtime| runtime.sgr_pixel_mouse_enabled());
                        let (pixel_width, pixel_height) = if cell_size.is_known() {
                            (
                                u32::from(pane.inner_rect.width) * cell_size.width_px,
                                u32::from(pane.inner_rect.height) * cell_size.height_px,
                            )
                        } else {
                            (0, 0)
                        };
                        let content_revision = runtime.map_or(0, |runtime| {
                            let after = runtime.content_seq();
                            if content_revisions_before.get(&pane.id).copied() == Some(after)
                                && after.is_multiple_of(2)
                            {
                                after
                            } else {
                                after | 1
                            }
                        });
                        protocol::PaneSurfacePane {
                            pane_id,
                            content_revision,
                            rect: pane.rect.into(),
                            inner_rect: pane.inner_rect.into(),
                            scrollbar_rect: pane.scrollbar_rect.map(Into::into),
                            scroll: runtime.and_then(|runtime| runtime.scroll_metrics()).map(
                                |metrics| protocol::PaneSurfaceScrollMetrics {
                                    offset_from_bottom: metrics.offset_from_bottom as u64,
                                    max_offset_from_bottom: metrics.max_offset_from_bottom as u64,
                                    viewport_rows: metrics.viewport_rows as u64,
                                },
                            ),
                            focused: pane.is_focused,
                            mouse_reporting,
                            sgr_pixel_mouse,
                            alternate_screen_active: runtime
                                .is_some_and(|runtime| runtime.alternate_screen_active()),
                            pixel_width,
                            pixel_height,
                        }
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let pane_frames = layout
        .pane_infos
        .iter()
        .map(|pane| pane.rect)
        .collect::<Vec<_>>();
    let splits = layout
        .split_borders
        .iter()
        .filter_map(|split| {
            let hit_rect = split_hit_rect(
                split,
                app.state.pane_borders.draws_borders(),
                app.state.pane_gaps,
                &pane_frames,
            )?;
            let direction = match split.direction {
                ratatui::layout::Direction::Horizontal => {
                    protocol::PaneSurfaceSplitDirection::Horizontal
                }
                ratatui::layout::Direction::Vertical => {
                    protocol::PaneSurfaceSplitDirection::Vertical
                }
            };
            Some(protocol::PaneSurfaceSplit {
                direction,
                pos: split.pos,
                area: split.area.into(),
                hit_rect: hit_rect.into(),
                path: split.path.clone(),
            })
        })
        .collect();
    let popup = show_popup
        .then(|| render_popup_surface(app, area, resize_panes, cell_size))
        .flatten();
    let (graphics, next_graphics_delivery) = crate::server::client_shell_graphics::collect(
        app,
        &layout.pane_infos,
        &layout.split_borders,
        popup.as_deref(),
        target,
        cell_size,
        graphics_delivery,
        client_id,
        Some(&client_terminals),
    );
    RenderedPaneSurface {
        frame: FrameData::from_ratatui_buffer_with_hyperlinks(&buffer, cursor, &hyperlinks),
        panes,
        splits,
        popup,
        graphics,
        graphics_delivery: next_graphics_delivery,
    }
}

pub(super) fn client_pane_terminals(
    app: &app::App,
    workspace_index: usize,
    location: Option<&crate::server::clients::ClientShellLocation>,
) -> (
    std::collections::HashMap<crate::layout::PaneId, crate::terminal::TerminalId>,
    Option<crate::layout::PaneId>,
) {
    let Some(workspace) = app.state.workspaces.get(workspace_index) else {
        return Default::default();
    };
    let focused_pane = location
        .and_then(crate::server::clients::ClientShellLocation::focused_pane_id)
        .and_then(|pane_id| app.parse_pane_id(pane_id))
        .filter(|(owner, _)| *owner == workspace_index)
        .map(|(_, pane_id)| pane_id)
        .or_else(|| workspace.focused_pane_id());
    let terminals = workspace
        .layout
        .pane_ids()
        .into_iter()
        .filter_map(|pane_id| {
            let pane = workspace.pane_state(pane_id)?;
            let public_pane_id = app.public_pane_id(workspace_index, pane_id)?;
            let tab_index = location
                .and_then(|location| location.active_tab_ids.get(&public_pane_id))
                .and_then(|tab_id| app.parse_public_tab_id(tab_id))
                .filter(|(owner, owner_pane, _)| {
                    *owner == workspace_index && *owner_pane == pane_id
                })
                .map(|(_, _, tab_index)| tab_index)
                .unwrap_or(pane.active_tab);
            Some((pane_id, pane.tabs.get(tab_index)?.terminal_id.clone()))
        })
        .collect();
    (terminals, focused_pane)
}

fn selected_runtime<'a>(
    app: &'a app::App,
    terminals: &std::collections::HashMap<crate::layout::PaneId, crate::terminal::TerminalId>,
    workspace_index: usize,
    pane_id: crate::layout::PaneId,
) -> Option<&'a crate::terminal::TerminalRuntime> {
    let terminal_id = terminals.get(&pane_id)?;
    let workspace = app.state.workspaces.get(workspace_index)?;
    if workspace
        .pane_state(pane_id)
        .is_some_and(|pane| pane.active_terminal_id() == terminal_id)
    {
        if let Some(runtime) = app.state.runtime_for_pane_in_workspace(
            &app.terminal_runtimes,
            workspace_index,
            pane_id,
        ) {
            return Some(runtime);
        }
    }
    app.terminal_runtimes.get(terminal_id)
}

fn render_popup_surface(
    app: &app::App,
    area: Rect,
    resize_runtime: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) -> Option<Box<protocol::ClientShellPopupSurface>> {
    let popup = app.state.popup_pane.as_ref()?;
    let geometry = if resize_runtime {
        resize_popup_runtime(app, area, cell_size)?
    } else {
        crate::popup_size::resolve_popup_geometry(popup.width, popup.height, area)?
    };
    let runtime = app.terminal_runtimes.get(&popup.terminal_id)?;
    let content_area = Rect::new(0, 0, geometry.inner.width, geometry.inner.height);
    let (buffer, cursor) =
        crate::server::render_stream::render_terminal_virtual(runtime, content_area);
    let hyperlinks = runtime.visible_hyperlinks(content_area);
    let title = app
        .state
        .terminals
        .get(&popup.terminal_id)
        .and_then(|terminal| terminal.manual_label.clone())
        .unwrap_or_else(|| "popup".to_owned());
    let (pixel_width, pixel_height) = if cell_size.is_known() {
        (
            u32::from(content_area.width) * cell_size.width_px,
            u32::from(content_area.height) * cell_size.height_px,
        )
    } else {
        (0, 0)
    };
    Some(Box::new(protocol::ClientShellPopupSurface {
        terminal_id: popup.terminal_id.to_string(),
        title,
        width: popup.width.map(client_popup_size),
        height: popup.height.map(client_popup_size),
        frame: FrameData::from_ratatui_buffer_with_hyperlinks(&buffer, cursor, &hyperlinks),
        mouse_reporting: runtime.mouse_reporting_enabled(),
        sgr_pixel_mouse: runtime.sgr_pixel_mouse_enabled(),
        pixel_width,
        pixel_height,
    }))
}

pub(super) fn resize_popup_runtime(
    app: &app::App,
    area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) -> Option<crate::popup_size::PopupResolvedGeometry> {
    let popup = app.state.popup_pane.as_ref()?;
    let geometry = crate::popup_size::resolve_popup_geometry(popup.width, popup.height, area)?;
    let runtime = app.terminal_runtimes.get(&popup.terminal_id)?;
    if !app
        .state
        .direct_attach_resize_locks
        .contains(&popup.terminal_id)
    {
        runtime.resize(
            geometry.inner.height,
            geometry.inner.width,
            cell_size.width_px,
            cell_size.height_px,
        );
    }
    Some(geometry)
}

fn client_popup_size(size: crate::popup_size::PopupSize) -> protocol::ClientShellPopupSize {
    match size {
        crate::popup_size::PopupSize::Cells(cells) => protocol::ClientShellPopupSize::Cells(cells),
        crate::popup_size::PopupSize::Percent(percent) => {
            protocol::ClientShellPopupSize::Percent(percent)
        }
    }
}

fn split_hit_rect(
    split: &crate::layout::SplitBorder,
    pane_borders: bool,
    pane_gaps: bool,
    pane_frames: &[Rect],
) -> Option<Rect> {
    let hit = match (split.direction, pane_borders, pane_gaps) {
        (ratatui::layout::Direction::Horizontal, true, false) => {
            Rect::new(split.pos, split.area.y, 1, split.area.height)
        }
        (ratatui::layout::Direction::Horizontal, true, true) => {
            let start = split.pos.saturating_sub(1);
            Rect::new(
                start,
                split.area.y,
                split.pos.saturating_sub(start).saturating_add(1),
                split.area.height,
            )
        }
        (ratatui::layout::Direction::Horizontal, false, true) => Rect::new(
            split.pos.checked_sub(1)?,
            split.area.y,
            1,
            split.area.height,
        ),
        (ratatui::layout::Direction::Vertical, true, false) => {
            Rect::new(split.area.x, split.pos, split.area.width, 1)
        }
        (ratatui::layout::Direction::Vertical, true, true) => {
            let start = split.pos.saturating_sub(1);
            Rect::new(
                split.area.x,
                start,
                split.area.width,
                split.pos.saturating_sub(start).saturating_add(1),
            )
        }
        (ratatui::layout::Direction::Vertical, false, true) => {
            Rect::new(split.area.x, split.pos.checked_sub(1)?, split.area.width, 1)
        }
        (_, false, false) => return None,
    };
    if !pane_borders
        && pane_frames.iter().any(|pane| {
            hit.x < pane.right()
                && hit.right() > pane.x
                && hit.y < pane.bottom()
                && hit.bottom() > pane.y
        })
    {
        return None;
    }
    Some(hit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_fallback_projects_only_focused_stack_active_panes_and_active_agents() {
        let mut snapshot_v2: protocol::ClientShellSnapshotV2 =
            serde_json::from_str(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/endpoint-snapshot-v2.json"
            )))
            .unwrap();
        let inactive_agent = snapshot_v2.workspaces[0].panes[0].tabs[1]
            .agent
            .clone()
            .unwrap();
        let mut focused_agent = inactive_agent.clone();
        focused_agent.tab_id = "w_alpha:p1:t1".into();
        focused_agent.terminal_id = "terminal-1".into();
        focused_agent.focused = true;
        snapshot_v2.workspaces[0].panes[0].tabs[0].agent = Some(focused_agent);
        let mut background_agent = inactive_agent;
        background_agent.pane_id = "w_alpha:p2".into();
        background_agent.tab_id = "w_alpha:p2:t3".into();
        background_agent.terminal_id = "terminal-3".into();
        snapshot_v2.workspaces[0].panes[1].tabs[0].agent = Some(background_agent);
        snapshot_v2.agent_order = vec![
            "w_alpha:p1:t2".into(),
            "w_alpha:p1:t1".into(),
            "w_alpha:p2:t3".into(),
        ];

        let snapshot_v1 = snapshot_v1_from_v2(snapshot_v2);

        assert_eq!(
            snapshot_v1
                .tabs
                .iter()
                .map(|tab| tab.tab_id.as_str())
                .collect::<Vec<_>>(),
            vec!["w_alpha:p1:t1", "w_alpha:p1:t2"]
        );
        assert_eq!(
            snapshot_v1
                .panes
                .iter()
                .map(|pane| (pane.pane_id.as_str(), pane.tab_id.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("w_alpha:p1", "w_alpha:p1:t1"),
                ("w_alpha:p2", "w_alpha:p2:t3")
            ]
        );
        assert_eq!(
            snapshot_v1
                .agents
                .iter()
                .map(|agent| (agent.pane_id.as_str(), agent.tab_id.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("w_alpha:p1", "w_alpha:p1:t1"),
                ("w_alpha:p2", "w_alpha:p2:t3")
            ]
        );
        assert_eq!(snapshot_v1.agent_order, vec!["w_alpha:p1", "w_alpha:p2"]);
    }

    #[test]
    fn v2_projects_every_stable_pane_and_pane_owned_tab_while_v1_is_duplicate_free() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = crate::workspace::Workspace::test_new("nested");
        let first_pane = workspace.root_pane;
        workspace.test_add_tab_to_pane(first_pane, Some("inactive"));
        let second_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.test_add_tab_to_pane(second_pane, Some("second-inactive"));
        workspace.layout.focus_pane(first_pane);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);

        let snapshot_v2 = snapshot_v2(&app, "boot", 1, None, None);
        assert_eq!(snapshot_v2.workspaces.len(), 1);
        assert_eq!(snapshot_v2.workspaces[0].panes.len(), 2);
        assert_eq!(snapshot_v2.workspaces[0].panes[0].tabs.len(), 2);
        assert_eq!(snapshot_v2.workspaces[0].panes[1].tabs.len(), 2);
        assert_ne!(
            snapshot_v2.workspaces[0].panes[0].pane_id,
            snapshot_v2.workspaces[0].panes[1].pane_id
        );

        let snapshot_v1 = snapshot_v1_from_v2(snapshot_v2);
        assert_eq!(snapshot_v1.workspaces.len(), 1);
        assert_eq!(snapshot_v1.tabs.len(), 2);
        assert!(snapshot_v1
            .tabs
            .iter()
            .all(|tab| tab.tab_id.starts_with(&format!(
                "{}:",
                snapshot_v1.focused_pane_id.as_deref().unwrap()
            ))));
        assert_eq!(snapshot_v1.panes.len(), 2);
        assert!(snapshot_v1.panes.iter().all(|pane| pane.tab_id
            == snapshot_v1.workspaces[0].active_tab_id
            || pane.pane_id != snapshot_v1.focused_pane_id.as_deref().unwrap()));
        let unique_panes = snapshot_v1
            .panes
            .iter()
            .map(|pane| pane.pane_id.as_str())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(unique_panes.len(), snapshot_v1.panes.len());
        let unique_agents = snapshot_v1
            .agents
            .iter()
            .map(|agent| agent.pane_id.as_str())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(unique_agents.len(), snapshot_v1.agents.len());
        assert!(snapshot_v1.agents.iter().all(|agent| {
            snapshot_v1
                .panes
                .iter()
                .any(|pane| pane.pane_id == agent.pane_id && pane.tab_id == agent.tab_id)
        }));
    }

    #[test]
    fn snapshot_projects_cached_release_and_update_facts() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.integration_recommendations.clear();
        app.state.update_available = Some("0.8.3".into());
        app.state.update_install_command = "herdr update".into();
        app.state.latest_release_notes_available = true;
        app.state.latest_release_notes = Some(crate::release_notes::ReleaseNotes {
            version: "0.8.3".into(),
            body: "### Changed\n- Client shell".into(),
            preview: true,
        });

        let snapshot = snapshot(&app, "boot", 7, None, None);

        assert_eq!(snapshot.update_available.as_deref(), Some("0.8.3"));
        assert_eq!(snapshot.update_install_command, "herdr update");
        assert!(snapshot.latest_release_notes_available);
        assert!(!snapshot.integration_updates_available);
        assert_eq!(
            snapshot.release_notes.as_ref().map(|notes| (
                notes.version.as_str(),
                notes.body.as_str(),
                notes.preview
            )),
            Some(("0.8.3", "### Changed\n- Client shell", true))
        );
    }

    #[test]
    fn snapshot_badges_only_outdated_integrations() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.integration_recommendations =
            vec![crate::integration::IntegrationRecommendation {
                target: crate::api::schema::IntegrationTarget::Claude,
                label: "claude",
                command: "claude",
                available: true,
                path: std::path::PathBuf::from("claude-hook"),
                state: crate::integration::IntegrationStatusKind::NotInstalled,
            }];

        assert!(!snapshot(&app, "boot", 1, None, None).integration_updates_available);

        app.state.integration_recommendations[0].state =
            crate::integration::IntegrationStatusKind::Outdated;
        assert!(snapshot(&app, "boot", 2, None, None).integration_updates_available);
    }

    #[test]
    fn split_hits_follow_released_border_and_gap_geometry() {
        let horizontal = crate::layout::SplitBorder {
            pos: 20,
            direction: ratatui::layout::Direction::Horizontal,
            ratio: 0.5,
            area: Rect::new(2, 3, 40, 12),
            path: vec![false],
        };
        assert_eq!(
            split_hit_rect(&horizontal, true, false, &[]),
            Some(Rect::new(20, 3, 1, 12))
        );
        assert_eq!(
            split_hit_rect(&horizontal, true, true, &[]),
            Some(Rect::new(19, 3, 2, 12))
        );
        assert_eq!(
            split_hit_rect(&horizontal, false, true, &[]),
            Some(Rect::new(19, 3, 1, 12))
        );
        assert_eq!(split_hit_rect(&horizontal, false, false, &[]), None);

        let vertical = crate::layout::SplitBorder {
            pos: 9,
            direction: ratatui::layout::Direction::Vertical,
            ratio: 0.5,
            area: Rect::new(2, 3, 40, 12),
            path: vec![true],
        };
        assert_eq!(
            split_hit_rect(&vertical, true, true, &[]),
            Some(Rect::new(2, 8, 40, 2))
        );

        let edge = crate::layout::SplitBorder {
            pos: 0,
            direction: ratatui::layout::Direction::Horizontal,
            ratio: 0.5,
            area: Rect::new(0, 0, 1, 4),
            path: Vec::new(),
        };
        assert_eq!(
            split_hit_rect(&edge, true, true, &[]),
            Some(Rect::new(0, 0, 1, 4))
        );
        assert_eq!(split_hit_rect(&edge, false, true, &[]), None);
        assert_eq!(
            split_hit_rect(&horizontal, false, true, &[Rect::new(19, 3, 1, 12)]),
            None
        );
    }
}
