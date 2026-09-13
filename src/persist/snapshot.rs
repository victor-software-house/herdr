use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use ratatui::layout::Direction;
use serde::{Deserialize, Serialize};

use crate::layout::Node;
use crate::terminal::{TerminalId, TerminalRuntimeRegistry};
use crate::workspace::Workspace;

/// Current snapshot format version.
pub(super) const SNAPSHOT_VERSION: u32 = 4;

/// Serializable snapshot of the entire HerDL session.
#[derive(Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// Format version — HerDL intentionally accepts exactly v4.
    pub version: u32,
    pub workspaces: Vec<WorkspaceSnapshot>,
    pub active: Option<usize>,
    pub selected: usize,
    /// Previous public pane IDs mapped to the moved pane's current public ID.
    #[serde(default)]
    pub public_pane_aliases: HashMap<String, String>,
    #[serde(default)]
    pub sidebar_width: Option<u16>,
    #[serde(default)]
    pub sidebar_section_split: Option<f32>,
    #[serde(default)]
    pub collapsed_space_keys: HashSet<String>,
}

#[derive(Serialize, Deserialize)]
pub struct SessionHistorySnapshot {
    /// Format version follows the matching session snapshot version.
    pub version: u32,
    /// History is keyed by stable workspace identity, not display order.
    pub workspaces: HashMap<String, WorkspaceHistorySnapshot>,
}

#[derive(Serialize, Deserialize)]
pub struct WorkspaceHistorySnapshot {
    /// Panes are keyed by stable, non-reused public pane number.
    pub panes: HashMap<usize, PaneHistorySnapshot>,
}

#[derive(Serialize, Deserialize)]
pub struct PaneHistorySnapshot {
    /// Terminal histories are keyed by stable, non-reused public tab number.
    pub tabs: HashMap<usize, TerminalHistorySnapshot>,
}

#[derive(Serialize, Deserialize)]
pub struct TerminalHistorySnapshot {
    pub ansi: String,
    pub lines: usize,
}

#[derive(Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub id: String,
    #[serde(default)]
    pub custom_name: Option<String>,
    pub identity_cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_space: Option<crate::workspace::WorktreeSpaceMembership>,
    pub next_public_pane_number: usize,
    pub next_public_tab_number: usize,
    pub layout: LayoutSnapshot,
    pub panes: HashMap<u32, PaneSnapshot>,
    pub zoomed: bool,
    pub focused: u32,
    pub root_pane: u32,
}

#[derive(Serialize, Deserialize)]
pub struct PaneSnapshot {
    /// Stable, non-reused public pane number within the workspace.
    pub number: usize,
    pub tabs: Vec<PaneTabSnapshot>,
    pub active_tab: usize,
    #[serde(default)]
    pub right_click_passthrough: bool,
}

#[derive(Serialize, Deserialize)]
pub struct PaneTabSnapshot {
    /// Stable, non-reused public tab number within the workspace.
    pub number: usize,
    #[serde(default)]
    pub custom_name: Option<String>,
    pub terminal_id: TerminalId,
    /// Whether this terminal had a live runtime when the snapshot was captured.
    pub runtime_attached: bool,
    #[serde(default = "default_seen")]
    pub seen: bool,
    pub terminal: TerminalSnapshot,
}

fn default_seen() -> bool {
    true
}

#[derive(Serialize, Deserialize)]
pub struct TerminalSnapshot {
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_agent_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session: Option<PaneAgentSessionSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_argv: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneAgentSessionSnapshot {
    pub source: String,
    pub agent: String,
    pub kind: crate::agent_resume::AgentSessionRefKind,
    pub value: String,
}

/// Serializable BSP tree.
#[derive(Serialize, Deserialize)]
pub enum LayoutSnapshot {
    Pane(u32),
    Split {
        direction: DirectionSnapshot,
        ratio: f32,
        first: Box<LayoutSnapshot>,
        second: Box<LayoutSnapshot>,
    },
}

#[derive(Serialize, Deserialize)]
pub enum DirectionSnapshot {
    Horizontal,
    Vertical,
}

#[derive(Deserialize)]
struct SnapshotVersion {
    version: Option<u32>,
}

/// Capture the current app state into a serializable snapshot.
pub fn capture(
    workspaces: &[Workspace],
    terminals: &HashMap<TerminalId, crate::terminal::TerminalState>,
    terminal_runtimes: &TerminalRuntimeRegistry,
    public_pane_id_aliases: &HashMap<String, crate::layout::PaneId>,
    active: Option<usize>,
    selected: usize,
) -> SessionSnapshot {
    SessionSnapshot {
        version: SNAPSHOT_VERSION,
        workspaces: workspaces
            .iter()
            .map(|workspace| capture_workspace(workspace, terminals, terminal_runtimes))
            .collect(),
        active,
        selected,
        public_pane_aliases: public_pane_id_aliases
            .iter()
            .filter_map(|(alias, pane_id)| {
                workspaces.iter().find_map(|workspace| {
                    workspace.public_pane_number(*pane_id).map(|number| {
                        (
                            alias.clone(),
                            crate::workspace::public_pane_id_for_number(&workspace.id, number),
                        )
                    })
                })
            })
            .collect(),
        sidebar_width: None,
        sidebar_section_split: None,
        collapsed_space_keys: HashSet::new(),
    }
}

fn capture_workspace(
    workspace: &Workspace,
    terminals: &HashMap<TerminalId, crate::terminal::TerminalState>,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> WorkspaceSnapshot {
    let panes = workspace
        .panes
        .iter()
        .map(|(pane_id, pane)| {
            let tabs = pane
                .tabs
                .iter()
                .map(|tab| PaneTabSnapshot {
                    number: tab.number,
                    custom_name: tab.custom_name.clone(),
                    terminal_id: tab.terminal_id.clone(),
                    runtime_attached: terminal_runtimes.get(&tab.terminal_id).is_some(),
                    seen: tab.seen,
                    terminal: capture_terminal(&tab.terminal_id, terminals, terminal_runtimes),
                })
                .collect();
            (
                pane_id.raw(),
                PaneSnapshot {
                    number: workspace
                        .public_pane_number(*pane_id)
                        .expect("persisted pane must have a public number"),
                    tabs,
                    active_tab: pane.active_tab,
                    right_click_passthrough: pane.right_click_passthrough,
                },
            )
        })
        .collect();

    WorkspaceSnapshot {
        id: workspace.id.clone(),
        custom_name: workspace.custom_name.clone(),
        identity_cwd: workspace
            .resolved_identity_cwd_from(terminals, terminal_runtimes)
            .unwrap_or_else(|| workspace.identity_cwd.clone()),
        worktree_space: workspace.worktree_space.clone(),
        next_public_pane_number: workspace.next_public_pane_number,
        next_public_tab_number: workspace.next_public_tab_number,
        layout: capture_node(workspace.layout.root()),
        panes,
        zoomed: workspace.zoomed,
        focused: workspace.layout.focused().raw(),
        root_pane: workspace.root_pane.raw(),
    }
}

fn capture_terminal(
    terminal_id: &TerminalId,
    terminals: &HashMap<TerminalId, crate::terminal::TerminalState>,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> TerminalSnapshot {
    let terminal = terminals.get(terminal_id);
    let cwd = terminal_runtimes
        .get(terminal_id)
        .and_then(|runtime| runtime.cwd())
        .or_else(|| terminal.map(|terminal| terminal.cwd.clone()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()));
    let label = terminal.and_then(|terminal| terminal.manual_label.clone());
    let (agent_name, managed_agent_kind) = terminal
        .filter(|terminal| !terminal.managed_agent_launch_pending())
        .map(|terminal| {
            (
                terminal.agent_name.clone(),
                terminal
                    .managed_agent_kind()
                    .map(|agent| crate::detect::agent_label(agent).to_string()),
            )
        })
        .unwrap_or_default();
    let launch_argv = terminal.and_then(|terminal| terminal.launch_argv.clone());
    let agent_session = terminal.and_then(capture_agent_session);

    TerminalSnapshot {
        cwd,
        label,
        agent_name,
        managed_agent_kind,
        agent_session,
        launch_argv,
    }
}

fn capture_agent_session(
    terminal: &crate::terminal::TerminalState,
) -> Option<PaneAgentSessionSnapshot> {
    if let Some(authority) = terminal.hook_authority.as_ref() {
        if let Some(session_ref) = authority.session_ref.as_ref() {
            return Some(PaneAgentSessionSnapshot {
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
        .map(|session| PaneAgentSessionSnapshot {
            source: session.source.clone(),
            agent: session.agent.clone(),
            kind: session.session_ref.kind,
            value: session.session_ref.value.clone(),
        })
}

/// Capture terminal screen history separately from the structural session snapshot.
pub fn capture_history(
    workspaces: &[Workspace],
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> SessionHistorySnapshot {
    SessionHistorySnapshot {
        version: SNAPSHOT_VERSION,
        workspaces: workspaces
            .iter()
            .map(|workspace| {
                let panes = workspace
                    .panes
                    .iter()
                    .filter_map(|(pane_id, pane)| {
                        let pane_number = workspace.public_pane_number(*pane_id)?;
                        let tabs = pane
                            .tabs
                            .iter()
                            .filter_map(|tab| {
                                capture_terminal_history(&tab.terminal_id, terminal_runtimes)
                                    .map(|history| (tab.number, history))
                            })
                            .collect();
                        Some((pane_number, PaneHistorySnapshot { tabs }))
                    })
                    .collect();
                (workspace.id.clone(), WorkspaceHistorySnapshot { panes })
            })
            .collect(),
    }
}

fn capture_terminal_history(
    terminal_id: &TerminalId,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Option<TerminalHistorySnapshot> {
    let ansi = terminal_runtimes.get(terminal_id)?.snapshot_history()?;
    let lines = ansi.lines().count();
    Some(TerminalHistorySnapshot { ansi, lines })
}

pub(super) fn capture_node(node: &Node) -> LayoutSnapshot {
    match node {
        Node::Pane(id) => LayoutSnapshot::Pane(id.raw()),
        Node::Split {
            direction,
            ratio,
            first,
            second,
        } => LayoutSnapshot::Split {
            direction: match direction {
                Direction::Horizontal => DirectionSnapshot::Horizontal,
                Direction::Vertical => DirectionSnapshot::Vertical,
            },
            ratio: *ratio,
            first: Box::new(capture_node(first)),
            second: Box::new(capture_node(second)),
        },
    }
}

pub(super) fn parse_snapshot(content: &str) -> Result<SessionSnapshot, String> {
    require_exact_version(content, "snapshot")?;
    let snapshot = serde_json::from_str::<SessionSnapshot>(content).map_err(|e| e.to_string())?;
    validate_snapshot(&snapshot)?;
    Ok(snapshot)
}

pub(super) fn parse_history_snapshot(content: &str) -> Result<SessionHistorySnapshot, String> {
    require_exact_version(content, "history snapshot")?;
    serde_json::from_str::<SessionHistorySnapshot>(content).map_err(|e| e.to_string())
}

fn require_exact_version(content: &str, kind: &str) -> Result<(), String> {
    let version = serde_json::from_str::<SnapshotVersion>(content)
        .map_err(|e| e.to_string())?
        .version
        .ok_or_else(|| {
            format!("{kind} has no version; only version {SNAPSHOT_VERSION} is supported")
        })?;
    if version != SNAPSHOT_VERSION {
        return Err(format!(
            "{kind} version {version} is incompatible; only version {SNAPSHOT_VERSION} is supported"
        ));
    }
    Ok(())
}

pub(crate) fn validate_snapshot(snapshot: &SessionSnapshot) -> Result<(), String> {
    if snapshot.version != SNAPSHOT_VERSION {
        return Err(format!(
            "snapshot version {} is incompatible; only version {SNAPSHOT_VERSION} is supported",
            snapshot.version
        ));
    }
    if snapshot
        .active
        .is_some_and(|active| active >= snapshot.workspaces.len())
        || (!snapshot.workspaces.is_empty() && snapshot.selected >= snapshot.workspaces.len())
        || (snapshot.workspaces.is_empty() && snapshot.selected != 0)
    {
        return Err("snapshot contains an invalid active or selected workspace".into());
    }

    let mut workspace_ids = HashSet::new();
    let mut raw_pane_ids = HashSet::new();
    let mut public_pane_ids = HashSet::new();
    let mut terminal_ids = HashSet::new();
    for workspace in &snapshot.workspaces {
        if !workspace_ids.insert(workspace.id.as_str()) {
            return Err(format!("duplicate workspace id {}", workspace.id));
        }
        validate_workspace_snapshot(
            workspace,
            &mut raw_pane_ids,
            &mut public_pane_ids,
            &mut terminal_ids,
        )?;
    }
    for (alias, target) in &snapshot.public_pane_aliases {
        if !public_pane_ids.contains(target) {
            return Err(format!(
                "public pane alias {alias} has no restored target {target}"
            ));
        }
        if public_pane_ids.contains(alias) {
            return Err(format!(
                "public pane alias {alias} shadows a current pane id"
            ));
        }
    }
    Ok(())
}

fn validate_workspace_snapshot(
    workspace: &WorkspaceSnapshot,
    raw_pane_ids: &mut HashSet<u32>,
    public_pane_ids: &mut HashSet<String>,
    terminal_ids: &mut HashSet<TerminalId>,
) -> Result<(), String> {
    let mut layout_panes = Vec::new();
    collect_layout_pane_ids(&workspace.layout, &mut layout_panes);
    let layout_set: HashSet<_> = layout_panes.iter().copied().collect();
    let pane_set: HashSet<_> = workspace.panes.keys().copied().collect();
    if layout_set.len() != layout_panes.len() || layout_set != pane_set {
        return Err(format!(
            "workspace {} layout and pane entries do not match",
            workspace.id
        ));
    }
    if !pane_set.contains(&workspace.focused) || !pane_set.contains(&workspace.root_pane) {
        return Err(format!(
            "workspace {} focus or root pane is missing",
            workspace.id
        ));
    }

    let mut pane_numbers = HashSet::new();
    let mut tab_numbers = HashSet::new();
    let mut max_pane_number = 0;
    let mut max_tab_number = 0;
    for (raw_pane_id, pane) in &workspace.panes {
        if !raw_pane_ids.insert(*raw_pane_id) {
            return Err(format!("duplicate raw pane id {raw_pane_id}"));
        }
        if pane.tabs.is_empty() || pane.active_tab >= pane.tabs.len() {
            return Err(format!(
                "workspace {} contains an invalid pane tab stack",
                workspace.id
            ));
        }
        if pane.number == 0 || !pane_numbers.insert(pane.number) {
            return Err(format!(
                "workspace {} contains invalid or duplicate pane numbers",
                workspace.id
            ));
        }
        public_pane_ids.insert(crate::workspace::public_pane_id_for_number(
            &workspace.id,
            pane.number,
        ));
        max_pane_number = max_pane_number.max(pane.number);
        for tab in &pane.tabs {
            if tab.number == 0 || !tab_numbers.insert(tab.number) {
                return Err(format!(
                    "workspace {} contains invalid or duplicate tab numbers",
                    workspace.id
                ));
            }
            if !terminal_ids.insert(tab.terminal_id.clone()) {
                return Err(format!("duplicate terminal id {}", tab.terminal_id));
            }
            max_tab_number = max_tab_number.max(tab.number);
        }
    }
    if workspace.next_public_pane_number == 0
        || workspace.next_public_tab_number == 0
        || workspace.next_public_pane_number <= max_pane_number
        || workspace.next_public_tab_number <= max_tab_number
    {
        return Err(format!(
            "workspace {} contains a reused identity counter",
            workspace.id
        ));
    }
    Ok(())
}

fn collect_layout_pane_ids(layout: &LayoutSnapshot, ids: &mut Vec<u32>) {
    match layout {
        LayoutSnapshot::Pane(id) => ids.push(*id),
        LayoutSnapshot::Split { first, second, .. } => {
            collect_layout_pane_ids(first, ids);
            collect_layout_pane_ids(second, ids);
        }
    }
}

pub(super) fn snapshot_file_version(content: &str) -> Option<u32> {
    serde_json::from_str::<SnapshotVersion>(content)
        .ok()
        .and_then(|raw| raw.version)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::layout::Direction;

    use super::*;
    use crate::app::{AppState, Mode};
    use crate::workspace::Workspace;

    fn state_with_workspace() -> AppState {
        let mut state = AppState::test_new();
        state.workspaces = vec![Workspace::test_new("one")];
        state.ensure_test_terminals();
        state.active = Some(0);
        state.selected = 0;
        state.mode = Mode::Terminal;
        state
    }

    fn capture_from_state(state: &AppState) -> SessionSnapshot {
        capture(
            &state.workspaces,
            &state.terminals,
            &TerminalRuntimeRegistry::new(),
            &HashMap::new(),
            state.active,
            state.selected,
        )
    }

    fn fixture_value() -> serde_json::Value {
        serde_json::from_str(include_str!(
            "../../tests/fixtures/session/current-herdl-v4.json"
        ))
        .unwrap()
    }

    #[test]
    fn v4_fixture_parses() {
        let snapshot = parse_snapshot(include_str!(
            "../../tests/fixtures/session/current-herdl-v4.json"
        ))
        .expect("v4 fixture should parse");

        assert_eq!(snapshot.version, SNAPSHOT_VERSION);
        assert_eq!(snapshot.workspaces.len(), 1);
        let workspace = &snapshot.workspaces[0];
        assert_eq!(workspace.id, "wfixture");
        assert_eq!(workspace.panes.len(), 2);
        assert_eq!(workspace.panes[&10].tabs.len(), 2);
        assert_eq!(workspace.panes[&10].active_tab, 1);
        assert_eq!(workspace.panes[&10].tabs[1].number, 4);
    }

    #[test]
    fn only_v4_snapshots_and_history_are_accepted() {
        for version in [0, 1, 2, 3, 5, 999] {
            let session = serde_json::json!({
                "version": version,
                "workspaces": [],
                "active": null,
                "selected": 0
            })
            .to_string();
            let history = serde_json::json!({
                "version": version,
                "workspaces": {}
            })
            .to_string();
            assert!(parse_snapshot(&session).is_err());
            assert!(parse_history_snapshot(&history).is_err());
        }
        assert!(parse_snapshot(r#"{"workspaces":[],"active":null,"selected":0}"#).is_err());
        assert!(parse_history_snapshot(r#"{"workspaces":{}}"#).is_err());
    }

    #[test]
    fn round_trip_empty_v4_session() {
        let snapshot = SessionSnapshot {
            version: SNAPSHOT_VERSION,
            workspaces: Vec::new(),
            active: None,
            selected: 0,
            public_pane_aliases: HashMap::new(),
            sidebar_width: Some(26),
            sidebar_section_split: Some(0.5),
            collapsed_space_keys: HashSet::new(),
        };

        let restored = parse_snapshot(&serde_json::to_string(&snapshot).unwrap()).unwrap();

        assert!(restored.workspaces.is_empty());
        assert_eq!(restored.active, None);
        assert_eq!(restored.sidebar_width, Some(26));
        assert_eq!(restored.sidebar_section_split, Some(0.5));
    }

    #[test]
    fn capture_tracks_one_layout_and_every_pane_owned_tab() {
        let mut state = state_with_workspace();
        let root = state.workspaces[0].root_pane;
        let second = state.workspaces[0].test_split(Direction::Horizontal);
        let root_second_tab = state.workspaces[0].test_add_tab_to_pane(root, Some("logs"));
        let second_second_tab = state.workspaces[0].test_add_tab_to_pane(second, Some("tests"));
        state.ensure_test_terminals();
        assert!(state.workspaces[0]
            .panes
            .get_mut(&root)
            .unwrap()
            .switch_tab(root_second_tab));
        state.workspaces[0]
            .panes
            .get_mut(&root)
            .unwrap()
            .active_tab_mut()
            .seen = false;
        assert!(state.workspaces[0]
            .panes
            .get_mut(&second)
            .unwrap()
            .switch_tab(second_second_tab));
        state.workspaces[0]
            .panes
            .get_mut(&second)
            .unwrap()
            .right_click_passthrough = true;
        state.workspaces[0].layout.focus_pane(second);
        state.workspaces[0].zoomed = true;

        let snapshot = capture_from_state(&state);
        let workspace = &snapshot.workspaces[0];

        assert!(matches!(workspace.layout, LayoutSnapshot::Split { .. }));
        assert_eq!(workspace.focused, second.raw());
        assert_eq!(workspace.root_pane, root.raw());
        assert!(workspace.zoomed);
        assert_eq!(workspace.panes.len(), 2);
        assert_eq!(workspace.panes[&root.raw()].tabs.len(), 2);
        assert_eq!(workspace.panes[&root.raw()].active_tab, root_second_tab);
        assert_eq!(
            workspace.panes[&root.raw()].tabs[root_second_tab]
                .custom_name
                .as_deref(),
            Some("logs")
        );
        assert!(!workspace.panes[&root.raw()].tabs[root_second_tab].seen);
        assert!(workspace.panes[&second.raw()].right_click_passthrough);
        assert_eq!(workspace.next_public_pane_number, 3);
        assert_eq!(workspace.next_public_tab_number, 5);
    }

    #[test]
    fn capture_tracks_each_terminal_payload() {
        let mut state = state_with_workspace();
        let root = state.workspaces[0].root_pane;
        let inactive = state.workspaces[0].test_add_tab_to_pane(root, Some("review"));
        state.ensure_test_terminals();
        let inactive_id = state.workspaces[0].panes[&root].tabs[inactive]
            .terminal_id
            .clone();
        let session_path = std::env::current_dir()
            .unwrap()
            .join("pi-session.jsonl")
            .display()
            .to_string();
        let terminal = state.terminals.get_mut(&inactive_id).unwrap();
        terminal.cwd = PathBuf::from("/tmp/review");
        terminal.set_manual_label("reviewer".into());
        terminal.launch_argv = Some(vec!["pi".into(), "--session".into(), session_path.clone()]);
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdl:pi".into(),
            agent: "pi".into(),
            session_ref: crate::agent_resume::AgentSessionRef::path(session_path.clone()).unwrap(),
        });

        let snapshot = capture_from_state(&state);
        let saved = &snapshot.workspaces[0].panes[&root.raw()].tabs[inactive];

        assert_eq!(saved.terminal_id, inactive_id);
        assert_eq!(saved.terminal.cwd, PathBuf::from("/tmp/review"));
        assert_eq!(saved.terminal.label.as_deref(), Some("reviewer"));
        assert_eq!(saved.terminal.launch_argv.as_ref().unwrap()[0], "pi");
        assert_eq!(
            saved.terminal.agent_session.as_ref().unwrap().value,
            session_path
        );
    }

    #[tokio::test]
    async fn history_is_captured_for_every_tab_by_stable_identity() {
        let mut state = state_with_workspace();
        let root = state.workspaces[0].root_pane;
        let second_tab = state.workspaces[0].test_add_tab_to_pane(root, Some("two"));
        state.ensure_test_terminals();
        let pane_number = state.workspaces[0].public_pane_number(root).unwrap();
        let tabs = &state.workspaces[0].panes[&root].tabs;
        let first_number = tabs[0].number;
        let second_number = tabs[second_tab].number;
        let mut runtimes = TerminalRuntimeRegistry::new();
        runtimes.insert(
            tabs[0].terminal_id.clone(),
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                20,
                3,
                4096,
                b"first-history\r\n",
            ),
        );
        runtimes.insert(
            tabs[second_tab].terminal_id.clone(),
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                20,
                3,
                4096,
                b"second-history\r\n",
            ),
        );

        let history = capture_history(&state.workspaces, &runtimes);
        let pane_history = &history.workspaces[&state.workspaces[0].id].panes[&pane_number];

        assert!(pane_history.tabs[&first_number]
            .ansi
            .contains("first-history"));
        assert!(pane_history.tabs[&second_number]
            .ansi
            .contains("second-history"));
    }

    #[test]
    fn validation_rejects_duplicate_stable_or_terminal_identities() {
        let mut state = state_with_workspace();
        let root = state.workspaces[0].root_pane;
        state.workspaces[0].test_add_tab_to_pane(root, None);
        state.ensure_test_terminals();
        let mut snapshot = capture_from_state(&state);
        let pane = snapshot.workspaces[0].panes.get_mut(&root.raw()).unwrap();
        pane.tabs[1].number = pane.tabs[0].number;
        assert!(validate_snapshot(&snapshot).is_err());

        let mut snapshot = capture_from_state(&state);
        let pane = snapshot.workspaces[0].panes.get_mut(&root.raw()).unwrap();
        pane.tabs[1].terminal_id = pane.tabs[0].terminal_id.clone();
        assert!(validate_snapshot(&snapshot).is_err());
    }

    #[test]
    fn validation_rejects_zero_exhausted_and_cross_workspace_raw_pane_identities() {
        let mut value = fixture_value();
        value["workspaces"][0]["panes"]["10"]["number"] = 0.into();
        assert!(parse_snapshot(&value.to_string()).is_err());

        let mut value = fixture_value();
        value["workspaces"][0]["panes"]["10"]["tabs"][0]["number"] = 0.into();
        assert!(parse_snapshot(&value.to_string()).is_err());

        let mut value = fixture_value();
        value["workspaces"][0]["next_public_tab_number"] = 7.into();
        assert!(parse_snapshot(&value.to_string()).is_err());

        let mut value = fixture_value();
        let mut duplicate_workspace = value["workspaces"][0].clone();
        duplicate_workspace["id"] = "wsecond".into();
        value["workspaces"]
            .as_array_mut()
            .unwrap()
            .push(duplicate_workspace);
        assert!(parse_snapshot(&value.to_string()).is_err());
    }

    #[test]
    fn capture_persists_public_pane_aliases_as_stable_targets() {
        let state = state_with_workspace();
        let root = state.workspaces[0].root_pane;
        let alias = "wold:p1".to_string();
        let aliases = HashMap::from([(alias.clone(), root)]);

        let snapshot = capture(
            &state.workspaces,
            &state.terminals,
            &TerminalRuntimeRegistry::new(),
            &aliases,
            state.active,
            state.selected,
        );

        assert_eq!(
            snapshot.public_pane_aliases[&alias],
            crate::workspace::public_pane_id_for_number(&state.workspaces[0].id, 1)
        );
    }
}
