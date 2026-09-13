use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ratatui::layout::Direction;
use tokio::sync::{mpsc, Notify};
use tracing::{error, warn};

use crate::detect::AgentState;
use crate::events::AppEvent;
use crate::layout::{Node, PaneId, TileLayout};
use crate::pane::{PaneLaunchEnv, PaneState, PaneTab};
use crate::render_signal::RenderSignal;
use crate::terminal::{TerminalId, TerminalRuntime, TerminalState};
use crate::workspace::Workspace;

use super::snapshot::{
    PaneAgentSessionSnapshot, PaneHistorySnapshot, PaneTabSnapshot, TerminalHistorySnapshot,
};
use super::{
    DirectionSnapshot, LayoutSnapshot, SessionHistorySnapshot, SessionSnapshot, WorkspaceSnapshot,
};

struct AgentRestoreState<'a> {
    enabled: bool,
    resumed_sessions: &'a mut HashSet<String>,
}

struct PaneRestoreStartup<'a> {
    restore_plan: Option<crate::agent_resume::AgentResumePlan>,
    initial_history_ansi: Option<&'a str>,
    duplicate_agent_session: bool,
    reserved_agent_session: Option<String>,
}

struct RestoreRuntimeContext<'a> {
    scrollback_limit_bytes: usize,
    shell_config: crate::pane::PaneShellConfig<'a>,
    resume_agents_on_restore: bool,
    handoff: bool,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<RenderSignal>,
}

type ImportedRuntimeMap = HashMap<TerminalId, crate::handoff_runtime::ImportedHandoffRuntime>;
type RestoredSession = (
    Vec<Workspace>,
    HashMap<TerminalId, TerminalState>,
    HashMap<TerminalId, TerminalRuntime>,
);
type RestoredWorkspace = (
    Workspace,
    Vec<TerminalState>,
    HashMap<TerminalId, TerminalRuntime>,
);
type RestoredPane = (
    Option<PaneState>,
    Vec<TerminalState>,
    HashMap<TerminalId, TerminalRuntime>,
);
type RestoreFailures<T> = (T, usize);

/// Restore workspaces from a snapshot. Each terminal tab gets a fresh shell in its saved cwd.
pub fn restore(
    snapshot: &SessionSnapshot,
    history: Option<&SessionHistorySnapshot>,
    rows: u16,
    cols: u16,
    scrollback_limit_bytes: usize,
    default_shell: &str,
    shell_mode: crate::config::ShellModeConfig,
    resume_agents_on_restore: bool,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<RenderSignal>,
) -> RestoredSession {
    let mut imports = HashMap::new();
    restore_with_imports_and_failures(
        snapshot,
        history,
        rows,
        cols,
        RestoreRuntimeContext {
            scrollback_limit_bytes,
            shell_config: crate::pane::PaneShellConfig::new(default_shell, shell_mode),
            resume_agents_on_restore,
            handoff: false,
            events,
            render_notify,
            render_dirty,
        },
        &mut imports,
    )
    .0
}

#[cfg(unix)]
pub fn restore_handoff(
    snapshot: &SessionSnapshot,
    scrollback_limit_bytes: usize,
    default_shell: &str,
    shell_mode: crate::config::ShellModeConfig,
    imports: &mut ImportedRuntimeMap,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<RenderSignal>,
) -> std::io::Result<RestoredSession> {
    super::snapshot::validate_snapshot(snapshot).map_err(std::io::Error::other)?;
    let (restored, failed_imports) = restore_with_imports_and_failures(
        snapshot,
        None,
        24,
        80,
        RestoreRuntimeContext {
            scrollback_limit_bytes,
            shell_config: crate::pane::PaneShellConfig::new(default_shell, shell_mode),
            resume_agents_on_restore: true,
            handoff: true,
            events,
            render_notify,
            render_dirty,
        },
        imports,
    );
    if failed_imports > 0 {
        return Err(std::io::Error::other(format!(
            "handoff failed to restore {failed_imports} terminal runtime(s)"
        )));
    }
    if !imports.is_empty() {
        return Err(std::io::Error::other(format!(
            "handoff import did not consume {} terminal runtime(s)",
            imports.len()
        )));
    }
    Ok(restored)
}

#[cfg(unix)]
pub fn handoff_pane_aliases(
    snapshot: &SessionSnapshot,
    workspaces: &[Workspace],
) -> HashMap<u32, PaneId> {
    let mut aliases = HashMap::new();
    for (ws_snap, workspace) in snapshot.workspaces.iter().zip(workspaces) {
        let old_ids = collect_snapshot_pane_ids(&ws_snap.layout);
        let new_ids = workspace.layout.pane_ids();
        for (old_id, new_id) in old_ids.into_iter().zip(new_ids) {
            if old_id != new_id.raw() {
                aliases.insert(old_id, new_id);
            }
        }
    }
    aliases
}

pub fn restore_public_pane_aliases(
    snapshot: &SessionSnapshot,
    workspaces: &[Workspace],
) -> HashMap<String, PaneId> {
    snapshot
        .public_pane_aliases
        .iter()
        .filter_map(|(alias, target)| {
            workspaces.iter().find_map(|workspace| {
                workspace
                    .public_pane_numbers
                    .iter()
                    .find_map(|(pane_id, number)| {
                        (crate::workspace::public_pane_id_for_number(&workspace.id, *number)
                            == *target)
                            .then(|| (alias.clone(), *pane_id))
                    })
            })
        })
        .collect()
}

#[cfg(unix)]
fn collect_snapshot_pane_ids(node: &LayoutSnapshot) -> Vec<u32> {
    let mut ids = Vec::new();
    collect_snapshot_ids_inner(node, &mut ids);
    ids
}

#[cfg(unix)]
fn collect_snapshot_ids_inner(node: &LayoutSnapshot, ids: &mut Vec<u32>) {
    match node {
        LayoutSnapshot::Pane(id) => ids.push(*id),
        LayoutSnapshot::Split { first, second, .. } => {
            collect_snapshot_ids_inner(first, ids);
            collect_snapshot_ids_inner(second, ids);
        }
    }
}

fn restore_with_imports_and_failures(
    snapshot: &SessionSnapshot,
    history: Option<&SessionHistorySnapshot>,
    rows: u16,
    cols: u16,
    runtime_context: RestoreRuntimeContext<'_>,
    imports: &mut ImportedRuntimeMap,
) -> RestoreFailures<RestoredSession> {
    let mut workspaces = Vec::new();
    let mut terminals = HashMap::new();
    let mut terminal_runtimes = HashMap::new();
    let mut resumed_agent_sessions = HashSet::new();
    let mut failed_imports = 0;

    for workspace_snapshot in &snapshot.workspaces {
        let workspace_history =
            history.and_then(|history| history.workspaces.get(&workspace_snapshot.id));
        let (restored, workspace_failures) = restore_workspace(
            workspace_snapshot,
            workspace_history,
            rows,
            cols,
            &runtime_context,
            &mut resumed_agent_sessions,
            imports,
        );
        failed_imports += workspace_failures;
        if let Some((workspace, restored_terminals, restored_runtimes)) = restored {
            for terminal in restored_terminals {
                terminals.insert(terminal.id.clone(), terminal);
            }
            terminal_runtimes.extend(restored_runtimes);
            workspaces.push(workspace);
        }
    }

    crate::workspace::reserve_workspace_ids(&workspaces);
    ((workspaces, terminals, terminal_runtimes), failed_imports)
}

fn restore_workspace(
    snapshot: &WorkspaceSnapshot,
    history: Option<&super::snapshot::WorkspaceHistorySnapshot>,
    rows: u16,
    cols: u16,
    runtime_context: &RestoreRuntimeContext<'_>,
    resumed_agent_sessions: &mut HashSet<String>,
    imports: &mut ImportedRuntimeMap,
) -> RestoreFailures<Option<RestoredWorkspace>> {
    let (node, id_map) = restore_node_remapped(&snapshot.layout);
    let reverse_id_map: HashMap<PaneId, u32> = id_map
        .iter()
        .map(|(&old_id, &new_id)| (new_id, old_id))
        .collect();
    let layout_pane_ids = collect_pane_ids(&node);
    let mut panes = HashMap::new();
    let mut public_pane_numbers = HashMap::new();
    let mut terminals = Vec::new();
    let mut terminal_runtimes = HashMap::new();
    let mut failed_imports = 0;

    for pane_id in &layout_pane_ids {
        let Some(old_pane_id) = reverse_id_map.get(pane_id).copied() else {
            continue;
        };
        let Some(saved_pane) = snapshot.panes.get(&old_pane_id) else {
            continue;
        };
        let saved_history = history.and_then(|history| history.panes.get(&saved_pane.number));
        let ((restored_pane, pane_terminals, pane_runtimes), pane_failures) = restore_pane(
            *pane_id,
            saved_pane,
            saved_history,
            &snapshot.id,
            rows,
            cols,
            runtime_context,
            resumed_agent_sessions,
            imports,
        );
        failed_imports += pane_failures;
        if let Some(pane) = restored_pane {
            public_pane_numbers.insert(*pane_id, saved_pane.number);
            panes.insert(*pane_id, pane);
            terminals.extend(pane_terminals);
            terminal_runtimes.extend(pane_runtimes);
        }
    }

    if panes.is_empty() {
        warn!(workspace = %snapshot.id, "no panes could be restored for workspace");
        return (None, failed_imports);
    }

    let surviving: HashSet<PaneId> = panes.keys().copied().collect();
    let Some(node) = prune_restored_node(node, &surviving) else {
        return (None, failed_imports);
    };
    let surviving_pane_ids = collect_pane_ids(&node);
    let Some(focus) = resolve_restored_pane(
        Some(snapshot.focused),
        &id_map,
        &surviving,
        &surviving_pane_ids,
    ) else {
        return (None, failed_imports);
    };
    let Some(root_pane) = resolve_restored_pane(
        Some(snapshot.root_pane),
        &id_map,
        &surviving,
        &surviving_pane_ids,
    ) else {
        return (None, failed_imports);
    };
    let layout = TileLayout::from_saved(node, focus);
    let worktree_space = restored_worktree_space_membership(snapshot.worktree_space.clone());
    let (cached_git_space, cached_auto_label, cached_git_status_key) =
        crate::workspace::discover_workspace_git_identity(&snapshot.identity_cwd);

    (
        Some((
            Workspace {
                id: snapshot.id.clone(),
                custom_name: snapshot.custom_name.clone(),
                identity_cwd: snapshot.identity_cwd.clone(),
                cached_identity_cwd: snapshot.identity_cwd.clone(),
                cached_auto_label,
                cached_git_status_key,
                cached_git_branch: crate::workspace::git_branch(&snapshot.identity_cwd),
                cached_git_ahead_behind: None,
                cached_git_space,
                worktree_space,
                metadata_tokens: crate::metadata_tokens::MetadataTokens::default(),
                metadata_token_sequences: HashMap::new(),
                public_pane_numbers,
                next_public_pane_number: snapshot.next_public_pane_number,
                next_public_tab_number: snapshot.next_public_tab_number,
                root_pane,
                layout,
                panes,
                zoomed: snapshot.zoomed,
                events: runtime_context.events.clone(),
                render_notify: runtime_context.render_notify.clone(),
                render_dirty: runtime_context.render_dirty.clone(),
                #[cfg(test)]
                test_runtimes: HashMap::new(),
            },
            terminals,
            terminal_runtimes,
        )),
        failed_imports,
    )
}

#[allow(clippy::too_many_arguments)]
fn restore_pane(
    pane_id: PaneId,
    snapshot: &super::snapshot::PaneSnapshot,
    history: Option<&PaneHistorySnapshot>,
    workspace_id: &str,
    rows: u16,
    cols: u16,
    runtime_context: &RestoreRuntimeContext<'_>,
    resumed_agent_sessions: &mut HashSet<String>,
    imports: &mut ImportedRuntimeMap,
) -> RestoreFailures<RestoredPane> {
    let active_tab_number = snapshot.tabs.get(snapshot.active_tab).map(|tab| tab.number);
    let mut tabs = Vec::new();
    let mut terminals = Vec::new();
    let mut runtimes = HashMap::new();
    let mut failures = 0;

    for tab_snapshot in &snapshot.tabs {
        let tab_history = history.and_then(|history| history.tabs.get(&tab_snapshot.number));
        let (restored, failed) = restore_terminal_tab(
            pane_id,
            snapshot.number,
            tab_snapshot,
            tab_history,
            workspace_id,
            rows,
            cols,
            runtime_context,
            resumed_agent_sessions,
            imports,
        );
        failures += failed;
        if let Some((tab, terminal, runtime)) = restored {
            tabs.push(tab);
            terminals.push(terminal);
            if let Some(runtime) = runtime {
                runtimes.insert(
                    terminals.last().expect("terminal was pushed").id.clone(),
                    runtime,
                );
            }
        }
    }

    if tabs.is_empty() {
        return ((None, terminals, runtimes), failures);
    }
    let active_tab = active_tab_number
        .and_then(|number| tabs.iter().position(|tab| tab.number == number))
        .unwrap_or(0);
    (
        (
            Some(PaneState {
                tabs,
                active_tab,
                right_click_passthrough: snapshot.right_click_passthrough,
            }),
            terminals,
            runtimes,
        ),
        failures,
    )
}

#[allow(clippy::too_many_arguments)]
fn restore_terminal_tab(
    pane_id: PaneId,
    pane_number: usize,
    snapshot: &PaneTabSnapshot,
    history: Option<&TerminalHistorySnapshot>,
    workspace_id: &str,
    rows: u16,
    cols: u16,
    runtime_context: &RestoreRuntimeContext<'_>,
    resumed_agent_sessions: &mut HashSet<String>,
    imports: &mut ImportedRuntimeMap,
) -> RestoreFailures<Option<(PaneTab, TerminalState, Option<TerminalRuntime>)>> {
    let saved = &snapshot.terminal;
    let cwd = restored_cwd(&saved.cwd);
    let saved_agent = saved
        .managed_agent_kind
        .as_deref()
        .and_then(crate::detect::parse_canonical_agent_label);
    let startup = {
        let mut agent_restore = AgentRestoreState {
            enabled: runtime_context.resume_agents_on_restore,
            resumed_sessions: resumed_agent_sessions,
        };
        pane_restore_startup(saved.agent_session.as_ref(), history, &mut agent_restore)
    };
    let restored_agent_session = restored_terminal_agent_session(
        saved.agent_session.as_ref(),
        startup.duplicate_agent_session,
    );
    let initial_restore_agent = startup
        .restore_plan
        .as_ref()
        .and_then(|plan| crate::detect::parse_agent_label(&plan.agent));
    let imported = imports.remove(&snapshot.terminal_id);
    let was_imported = imported.is_some();

    if runtime_context.handoff && snapshot.runtime_attached != was_imported {
        error!(
            workspace = workspace_id,
            pane = pane_number,
            tab = snapshot.number,
            terminal = %snapshot.terminal_id,
            expected_runtime = snapshot.runtime_attached,
            "handoff terminal runtime ownership does not match the snapshot"
        );
        return (None, 1);
    }

    let terminal_id = if runtime_context.handoff {
        snapshot.terminal_id.clone()
    } else {
        TerminalId::alloc()
    };
    let mut pane_tab = PaneTab::new(snapshot.number, terminal_id.clone());
    pane_tab.custom_name = snapshot.custom_name.clone();
    pane_tab.seen = snapshot.seen;

    if !was_imported {
        if let Some(plan) = startup.restore_plan.clone() {
            let mut terminal =
                TerminalState::new(terminal_id, cwd).with_pending_agent_resume_plan(plan);
            restore_terminal_metadata(
                &mut terminal,
                saved,
                restored_agent_session,
                saved_agent,
                initial_restore_agent,
                false,
            );
            return (Some((pane_tab, terminal, None)), 0);
        }
        if runtime_context.handoff {
            let mut terminal = TerminalState::new(terminal_id, cwd);
            if let Some(argv) = saved.launch_argv.clone() {
                terminal = terminal.with_launch_argv(argv);
            }
            restore_terminal_metadata(
                &mut terminal,
                saved,
                restored_agent_session,
                saved_agent,
                initial_restore_agent,
                true,
            );
            return (Some((pane_tab, terminal, None)), 0);
        }
    }

    let launch_env = PaneLaunchEnv::from_extra(Vec::new()).with_identity(
        workspace_id.to_string(),
        crate::workspace::public_pane_tab_id_for_number(workspace_id, pane_number, snapshot.number),
        crate::workspace::public_pane_id_for_number(workspace_id, pane_number),
    );
    let runtime_result = {
        #[cfg(unix)]
        if let Some(imported) = imported {
            TerminalRuntime::from_handoff_fd(
                terminal_id.clone(),
                crate::handoff_runtime::ImportedHandoffRuntime {
                    master_fd: imported.master_fd,
                    state: imported.state.with_pane_id(pane_id),
                },
                runtime_context.scrollback_limit_bytes,
                crate::terminal_theme::TerminalTheme::default(),
                None,
                runtime_context.events.clone(),
                runtime_context.render_notify.clone(),
                runtime_context.render_dirty.clone(),
            )
        } else {
            spawn_restored_runtime(
                pane_id,
                terminal_id.clone(),
                rows,
                cols,
                cwd.clone(),
                &launch_env,
                startup.initial_history_ansi,
                runtime_context,
            )
        }

        #[cfg(not(unix))]
        {
            spawn_restored_runtime(
                pane_id,
                terminal_id.clone(),
                rows,
                cols,
                cwd.clone(),
                &launch_env,
                startup.initial_history_ansi,
                runtime_context,
            )
        }
    };

    match runtime_result {
        Ok(runtime) => {
            let mut terminal = TerminalState::new(terminal_id, cwd);
            if let Some(argv) = saved.launch_argv.clone() {
                terminal = terminal.with_launch_argv(argv);
                if was_imported {
                    terminal = terminal.with_respawn_shell_on_exit();
                }
            }
            restore_terminal_metadata(
                &mut terminal,
                saved,
                restored_agent_session,
                saved_agent,
                initial_restore_agent,
                was_imported,
            );
            (Some((pane_tab, terminal, Some(runtime))), 0)
        }
        Err(err) => {
            if let Some(key) = startup.reserved_agent_session.as_deref() {
                resumed_agent_sessions.remove(key);
            }
            error!(
                workspace = workspace_id,
                pane = pane_number,
                tab = snapshot.number,
                err = %err,
                "failed to restore terminal tab"
            );
            (None, usize::from(was_imported))
        }
    }
}

fn restored_cwd(saved_cwd: &Path) -> PathBuf {
    if saved_cwd.exists() {
        return saved_cwd.to_path_buf();
    }
    warn!(cwd = %saved_cwd.display(), "saved terminal cwd does not exist, falling back to HOME");
    std::env::var("HOME")
        .map(PathBuf::from)
        .ok()
        .filter(|home| home.exists())
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn spawn_restored_runtime(
    pane_id: PaneId,
    terminal_id: TerminalId,
    rows: u16,
    cols: u16,
    cwd: PathBuf,
    launch_env: &PaneLaunchEnv,
    initial_history_ansi: Option<&str>,
    runtime_context: &RestoreRuntimeContext<'_>,
) -> std::io::Result<TerminalRuntime> {
    TerminalRuntime::spawn_with_initial_history(
        pane_id,
        terminal_id,
        rows,
        cols,
        cwd,
        runtime_context.scrollback_limit_bytes,
        crate::terminal_theme::TerminalTheme::default(),
        None,
        runtime_context.shell_config,
        launch_env,
        initial_history_ansi,
        runtime_context.events.clone(),
        runtime_context.render_notify.clone(),
        runtime_context.render_dirty.clone(),
    )
}

fn restore_terminal_metadata(
    terminal: &mut TerminalState,
    saved: &super::snapshot::TerminalSnapshot,
    restored_agent_session: Option<crate::agent_resume::PersistedAgentSession>,
    saved_managed_agent: Option<crate::detect::Agent>,
    initial_restore_agent: Option<crate::detect::Agent>,
    preserve_agent_state: bool,
) {
    if let Some(label) = saved.label.clone() {
        terminal.set_manual_label(label);
    }
    if let Some(session) = restored_agent_session {
        terminal.set_persisted_agent_session(session);
    }
    match (saved.agent_name.clone(), saved_managed_agent) {
        (Some(agent_name), Some(agent)) if preserve_agent_state => {
            terminal.restore_managed_agent(agent_name, agent)
        }
        (Some(agent_name), None) if preserve_agent_state => terminal.set_agent_name(agent_name),
        _ => {}
    }
    if let Some(agent) = initial_restore_agent {
        let _ = terminal.set_detected_state_with_screen_signals_at(
            Some(agent),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            std::time::Instant::now(),
        );
    }
}

fn restored_worktree_space_membership(
    space: Option<crate::workspace::WorktreeSpaceMembership>,
) -> Option<crate::workspace::WorktreeSpaceMembership> {
    space.filter(|space| {
        space.checkout_path.exists()
            && crate::workspace::git_space_metadata(&space.checkout_path)
                .is_some_and(|current| current.key == space.key)
    })
}

fn pane_restore_startup<'a>(
    session: Option<&PaneAgentSessionSnapshot>,
    history: Option<&'a TerminalHistorySnapshot>,
    agent_restore: &mut AgentRestoreState<'_>,
) -> PaneRestoreStartup<'a> {
    let restore_plan =
        session.and_then(|session| restore_plan_for_snapshot(session, agent_restore.enabled));
    let has_native_agent_restore = restore_plan.is_some();
    let mut reserved_agent_session = None;
    let duplicate_agent_session = restore_plan.as_ref().is_some_and(|plan| {
        if agent_restore
            .resumed_sessions
            .insert(plan.dedupe_key.clone())
        {
            reserved_agent_session = Some(plan.dedupe_key.clone());
            false
        } else {
            true
        }
    });
    let restore_plan = if duplicate_agent_session {
        None
    } else {
        restore_plan
    };

    PaneRestoreStartup {
        restore_plan,
        initial_history_ansi: if has_native_agent_restore {
            None
        } else {
            history.map(|history| history.ansi.as_str())
        },
        duplicate_agent_session,
        reserved_agent_session,
    }
}

fn restore_plan_for_snapshot(
    session: &PaneAgentSessionSnapshot,
    resume_agents_on_restore: bool,
) -> Option<crate::agent_resume::AgentResumePlan> {
    if !resume_agents_on_restore {
        return None;
    }
    let persisted = persisted_agent_session_from_snapshot(session)?;
    crate::agent_resume::plan(&session.source, &session.agent, &persisted.session_ref)
}

fn persisted_agent_session_from_snapshot(
    session: &PaneAgentSessionSnapshot,
) -> Option<crate::agent_resume::PersistedAgentSession> {
    crate::agent_resume::session_ref_from_snapshot(
        &session.source,
        &session.agent,
        session.kind,
        &session.value,
    )
}

fn restored_terminal_agent_session(
    session: Option<&PaneAgentSessionSnapshot>,
    duplicate_agent_session: bool,
) -> Option<crate::agent_resume::PersistedAgentSession> {
    if duplicate_agent_session {
        return None;
    }
    session.and_then(persisted_agent_session_from_snapshot)
}

#[cfg(test)]
fn take_restore_plan_for_snapshot(
    session: &PaneAgentSessionSnapshot,
    resume_agents_on_restore: bool,
    resumed_agent_sessions: &mut HashSet<String>,
) -> Option<crate::agent_resume::AgentResumePlan> {
    restore_plan_for_snapshot(session, resume_agents_on_restore)
        .filter(|plan| resumed_agent_sessions.insert(plan.dedupe_key.clone()))
}

pub(super) fn prune_restored_node(node: Node, surviving: &HashSet<PaneId>) -> Option<Node> {
    match node {
        Node::Pane(id) => surviving.contains(&id).then_some(Node::Pane(id)),
        Node::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let first = prune_restored_node(*first, surviving);
            let second = prune_restored_node(*second, surviving);
            match (first, second) {
                (Some(first), Some(second)) => Some(Node::Split {
                    direction,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(remaining), None) | (None, Some(remaining)) => Some(remaining),
                (None, None) => None,
            }
        }
    }
}

pub(super) fn resolve_restored_pane(
    saved_old_id: Option<u32>,
    id_map: &HashMap<u32, PaneId>,
    surviving: &HashSet<PaneId>,
    pane_ids: &[PaneId],
) -> Option<PaneId> {
    saved_old_id
        .and_then(|old_id| id_map.get(&old_id).copied())
        .filter(|pane_id| surviving.contains(pane_id))
        .or_else(|| pane_ids.first().copied())
}

/// Restore a layout tree, remapping every pane ID to a fresh globally unique one.
/// Returns the new tree and a map of old_raw_id → new PaneId.
pub(super) fn restore_node_remapped(snap: &LayoutSnapshot) -> (Node, HashMap<u32, PaneId>) {
    let mut id_map = HashMap::new();
    let node = remap_inner(snap, &mut id_map);
    (node, id_map)
}

fn remap_inner(snap: &LayoutSnapshot, id_map: &mut HashMap<u32, PaneId>) -> Node {
    match snap {
        LayoutSnapshot::Pane(old_id) => {
            let new_id = PaneId::alloc();
            id_map.insert(*old_id, new_id);
            Node::Pane(new_id)
        }
        LayoutSnapshot::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let first_node = remap_inner(first, id_map);
            let second_node = remap_inner(second, id_map);
            let dir = match direction {
                DirectionSnapshot::Horizontal => Direction::Horizontal,
                DirectionSnapshot::Vertical => Direction::Vertical,
            };
            Node::Split {
                direction: dir,
                ratio: *ratio,
                first: Box::new(first_node),
                second: Box::new(second_node),
            }
        }
    }
}

pub(super) fn collect_pane_ids(node: &Node) -> Vec<PaneId> {
    let mut ids = Vec::new();
    collect_ids_inner(node, &mut ids);
    ids
}

fn collect_ids_inner(node: &Node, ids: &mut Vec<PaneId>) {
    match node {
        Node::Pane(id) => ids.push(*id),
        Node::Split { first, second, .. } => {
            collect_ids_inner(first, ids);
            collect_ids_inner(second, ids);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    fn test_restore_shell() -> &'static str {
        "C:\\Windows\\System32\\whoami.exe"
    }

    #[cfg(not(windows))]
    fn test_restore_shell() -> &'static str {
        "/bin/sh"
    }

    fn fixture_snapshot() -> SessionSnapshot {
        super::super::snapshot::parse_snapshot(include_str!(
            "../../tests/fixtures/session/current-herdl-v4.json"
        ))
        .expect("v4 fixture should parse")
    }

    #[test]
    fn capture_and_restore_node_round_trip() {
        let node = Node::Split {
            direction: Direction::Horizontal,
            ratio: 0.5,
            first: Box::new(Node::Pane(PaneId::from_raw(10))),
            second: Box::new(Node::Split {
                direction: Direction::Vertical,
                ratio: 0.3,
                first: Box::new(Node::Pane(PaneId::from_raw(20))),
                second: Box::new(Node::Pane(PaneId::from_raw(30))),
            }),
        };

        let snapshot = super::super::snapshot::capture_node(&node);
        let (restored, id_map) = restore_node_remapped(&snapshot);

        assert_eq!(id_map.len(), 3);
        let ids = collect_pane_ids(&restored);
        let unique: HashSet<_> = ids.iter().copied().collect();
        assert_eq!(ids.len(), 3);
        assert_eq!(unique.len(), 3);
        assert!(ids.iter().all(|id| ![10, 20, 30].contains(&id.raw())));
    }

    #[test]
    fn prune_restored_node_collapses_missing_branch() {
        let keep = PaneId::from_raw(11);
        let missing = PaneId::from_raw(12);
        let node = Node::Split {
            direction: Direction::Horizontal,
            ratio: 0.5,
            first: Box::new(Node::Pane(keep)),
            second: Box::new(Node::Pane(missing)),
        };

        let restored = prune_restored_node(node, &HashSet::from([keep])).unwrap();

        assert!(matches!(restored, Node::Pane(id) if id == keep));
    }

    #[test]
    fn restore_plan_respects_opt_in_and_deduplicates_sessions() {
        let session_path = std::env::current_dir()
            .unwrap()
            .join("pi-session.jsonl")
            .display()
            .to_string();
        let session = PaneAgentSessionSnapshot {
            source: "herdl:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: session_path.clone(),
        };
        let mut resumed = HashSet::new();

        assert!(take_restore_plan_for_snapshot(&session, false, &mut resumed).is_none());
        assert!(resumed.is_empty());
        assert_eq!(
            take_restore_plan_for_snapshot(&session, true, &mut resumed)
                .unwrap()
                .argv,
            vec!["pi", "--session", session_path.as_str()]
        );
        assert!(take_restore_plan_for_snapshot(&session, true, &mut resumed).is_none());
    }

    #[test]
    fn native_agent_resume_suppresses_terminal_history() {
        let session = PaneAgentSessionSnapshot {
            source: "herdl:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: std::env::current_dir()
                .unwrap()
                .join("pi-session.jsonl")
                .display()
                .to_string(),
        };
        let history = TerminalHistorySnapshot {
            ansi: "RESTORED_HISTORY\r\n".into(),
            lines: 1,
        };
        let mut resumed = HashSet::new();
        let mut agent_restore = AgentRestoreState {
            enabled: true,
            resumed_sessions: &mut resumed,
        };

        let startup = pane_restore_startup(Some(&session), Some(&history), &mut agent_restore);

        assert!(startup.restore_plan.is_some());
        assert!(startup.initial_history_ansi.is_none());
        assert!(!startup.duplicate_agent_session);
    }

    #[tokio::test]
    async fn restore_recreates_every_pane_owned_tab_and_active_state() {
        let mut snapshot = fixture_snapshot();
        snapshot
            .public_pane_aliases
            .insert("wold:p1".into(), "wfixture:p2".into());
        let (events, _event_rx) = mpsc::channel(16);

        let (workspaces, terminals, runtimes) = restore(
            &snapshot,
            None,
            24,
            80,
            4096,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        );

        assert_eq!(workspaces.len(), 1);
        let workspace = &workspaces[0];
        assert_eq!(workspace.id, "wfixture");
        assert_eq!(workspace.next_public_pane_number, 6);
        assert_eq!(workspace.next_public_tab_number, 8);
        assert_eq!(workspace.layout.pane_count(), 2);
        assert_eq!(workspace.panes.len(), 2);
        assert_eq!(
            workspace
                .public_pane_numbers
                .values()
                .copied()
                .collect::<HashSet<_>>(),
            HashSet::from([2, 5])
        );
        let stacked = workspace
            .panes
            .values()
            .find(|pane| pane.tabs.len() == 2)
            .expect("stacked pane should restore");
        assert_eq!(stacked.active_tab, 1);
        assert_eq!(stacked.tabs[0].number, 1);
        assert_eq!(stacked.tabs[1].number, 4);
        assert_eq!(stacked.tabs[1].custom_name.as_deref(), Some("agent"));
        assert!(!stacked.tabs[1].seen);
        assert!(stacked.right_click_passthrough);
        assert_eq!(terminals.len(), 3);
        assert_eq!(runtimes.len(), 3);
        assert_eq!(workspace.terminal_ids().count(), 3);
        assert_eq!(workspace.terminal_ids().collect::<HashSet<_>>().len(), 3);
        assert!(stacked
            .tabs
            .iter()
            .all(|tab| terminals.contains_key(&tab.terminal_id)));
        let restored_agent = &terminals[&stacked.tabs[1].terminal_id];
        assert_eq!(restored_agent.manual_label.as_deref(), Some("reviewer"));
        assert_eq!(
            restored_agent.launch_argv.as_ref().unwrap(),
            &["pi", "--session", "fixture-session"]
        );
        assert_eq!(
            restored_agent
                .persisted_agent_session
                .as_ref()
                .unwrap()
                .session_ref
                .value,
            "fixture-session"
        );
        let aliases = restore_public_pane_aliases(&snapshot, &workspaces);
        let aliased_pane = aliases["wold:p1"];
        assert_eq!(workspace.public_pane_number(aliased_pane), Some(2));
    }

    #[tokio::test]
    async fn restore_looks_up_each_history_by_stable_workspace_pane_and_tab_identity() {
        let snapshot = fixture_snapshot();
        let history = SessionHistorySnapshot {
            version: super::super::snapshot::SNAPSHOT_VERSION,
            workspaces: HashMap::from([(
                "wfixture".into(),
                super::super::snapshot::WorkspaceHistorySnapshot {
                    panes: HashMap::from([(
                        2,
                        PaneHistorySnapshot {
                            tabs: HashMap::from([
                                (
                                    1,
                                    TerminalHistorySnapshot {
                                        ansi: "FIRST_STABLE_HISTORY\r\n".into(),
                                        lines: 1,
                                    },
                                ),
                                (
                                    4,
                                    TerminalHistorySnapshot {
                                        ansi: "SECOND_STABLE_HISTORY\r\n".into(),
                                        lines: 1,
                                    },
                                ),
                            ]),
                        },
                    )]),
                },
            )]),
        };
        let (events, _event_rx) = mpsc::channel(16);

        let (workspaces, _terminals, runtimes) = restore(
            &snapshot,
            Some(&history),
            5,
            40,
            4096,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        );
        let stacked = workspaces[0]
            .panes
            .values()
            .find(|pane| pane.tabs.len() == 2)
            .unwrap();
        let first = runtimes[&stacked.tabs[0].terminal_id].recent_unwrapped_text(10);
        let second = runtimes[&stacked.tabs[1].terminal_id].recent_unwrapped_text(10);

        assert!(first.contains("FIRST_STABLE_HISTORY"));
        assert!(second.contains("SECOND_STABLE_HISTORY"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn handoff_rejects_missing_runtime_instead_of_spawning_replacements() {
        let snapshot = fixture_snapshot();
        let mut imports = HashMap::new();

        let result = restore_handoff(
            &snapshot,
            4096,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            &mut imports,
            mpsc::channel(16).0,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        );

        let error = match result {
            Ok(_) => panic!("handoff without runtimes should fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("terminal runtime"));
    }

    #[test]
    fn restored_worktree_membership_drops_missing_checkout() {
        let missing =
            std::env::temp_dir().join(format!("herdl-missing-worktree-{}", std::process::id()));
        let membership = crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: missing.join("repo"),
            checkout_path: missing.join("checkout"),
            is_linked_worktree: true,
        };

        assert_eq!(restored_worktree_space_membership(Some(membership)), None);
    }
}
