use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use ratatui::layout::Direction;
use tokio::sync::{mpsc, Notify};

use crate::events::AppEvent;
use crate::layout::{Node, PaneId, TileLayout};
use crate::pane::{PaneLaunchEnv, PaneState, PaneTab};
use crate::render_signal::RenderSignal;
use crate::terminal::{TerminalId, TerminalRuntime, TerminalRuntimeRegistry, TerminalState};

mod aggregate;
mod git;
mod tab;

use self::git::git_status_cache_key_for_space;
pub(crate) use self::{git::git_status_snapshot_for_cwd_with_demand, tab::MovedPane};
pub use self::{
    git::{
        derive_label_from_cwd, fallback_label_from_cwd, git_branch, git_space_metadata,
        git_status_cache_key, GitSpaceMetadata, GitStatusCacheEntry, GitStatusRefreshDemand,
    },
    tab::NewPane,
};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorktreeSpaceMembership {
    pub key: String,
    pub label: String,
    pub repo_root: PathBuf,
    pub checkout_path: PathBuf,
    pub is_linked_worktree: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceGitStatus {
    pub workspace_id: String,
    pub resolved_identity_cwd: PathBuf,
    pub status_cache_key: PathBuf,
    pub demand: GitStatusRefreshDemand,
    pub auto_label: String,
    pub branch: Option<String>,
    pub ahead_behind: Option<(usize, usize)>,
    pub space: Option<GitSpaceMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceGitStatusSnapshot {
    pub auto_label: String,
    pub branch: Option<String>,
    pub ahead_behind: Option<(usize, usize)>,
    pub space: Option<GitSpaceMetadata>,
}

pub(crate) fn discover_workspace_git_identity(
    cwd: &std::path::Path,
) -> (Option<GitSpaceMetadata>, String, PathBuf) {
    let space = git_space_metadata(cwd);
    let auto_label = space
        .as_ref()
        .map(|space| self::git::automatic_workspace_label(cwd, &space.repo_root))
        .unwrap_or_else(|| fallback_label_from_cwd(cwd));
    let status_cache_key = space
        .as_ref()
        .map(git_status_cache_key_for_space)
        .unwrap_or_else(|| cwd.to_path_buf());
    (space, auto_label, status_cache_key)
}

impl WorkspaceGitStatusSnapshot {
    pub fn into_workspace_status(
        self,
        workspace_id: String,
        resolved_identity_cwd: PathBuf,
        status_cache_key: PathBuf,
        demand: GitStatusRefreshDemand,
    ) -> WorkspaceGitStatus {
        let auto_label = self
            .space
            .as_ref()
            .map(|space| {
                self::git::automatic_workspace_label(&resolved_identity_cwd, &space.repo_root)
            })
            .unwrap_or_else(|| fallback_label_from_cwd(&resolved_identity_cwd));
        WorkspaceGitStatus {
            workspace_id,
            resolved_identity_cwd,
            status_cache_key,
            demand,
            auto_label,
            branch: self.branch,
            ahead_behind: self.ahead_behind,
            space: self.space,
        }
    }
}

static NEXT_WORKSPACE_ID: AtomicU64 = AtomicU64::new(1);
const PUBLIC_ID_ALPHABET: &[u8; 32] = b"123456789ABCDEFGHJKMNPQRSTVWXYZ0";

pub(crate) fn generate_workspace_id() -> String {
    let counter = NEXT_WORKSPACE_ID.fetch_add(1, Ordering::Relaxed);
    format!("w{}", encode_public_number(counter as usize))
}

pub(crate) fn encode_public_number(mut value: usize) -> String {
    if value == 0 {
        return "0".to_string();
    }

    let mut encoded = Vec::new();
    while value > 0 {
        let digit = (value - 1) % PUBLIC_ID_ALPHABET.len();
        encoded.push(PUBLIC_ID_ALPHABET[digit] as char);
        value = (value - 1) / PUBLIC_ID_ALPHABET.len();
    }
    encoded.iter().rev().collect()
}

pub(crate) fn decode_public_number(value: &str) -> Option<usize> {
    let mut decoded = 0usize;
    for ch in value.chars() {
        let digit = PUBLIC_ID_ALPHABET
            .iter()
            .position(|candidate| *candidate as char == ch)?;
        decoded = decoded
            .checked_mul(PUBLIC_ID_ALPHABET.len())?
            .checked_add(digit + 1)?;
    }
    Some(decoded)
}

pub(crate) fn public_workspace_number(id: &str) -> Option<usize> {
    id.strip_prefix('w').and_then(decode_public_number)
}

pub(crate) fn public_pane_id_for_number(workspace_id: &str, pane_number: usize) -> String {
    format!("{workspace_id}:p{}", encode_public_number(pane_number))
}

pub(crate) fn public_tab_id_for_number(workspace_id: &str, tab_number: usize) -> String {
    format!("{workspace_id}:t{}", encode_public_number(tab_number))
}

pub(crate) fn public_pane_tab_id_for_number(
    workspace_id: &str,
    pane_number: usize,
    tab_number: usize,
) -> String {
    format!(
        "{}:t{}",
        public_pane_id_for_number(workspace_id, pane_number),
        encode_public_number(tab_number)
    )
}

pub(crate) fn reserve_workspace_ids(workspaces: &[Workspace]) {
    let Some(next) = workspaces
        .iter()
        .filter_map(|workspace| public_workspace_number(&workspace.id))
        .max()
        .and_then(|max| u64::try_from(max.checked_add(1)?).ok())
    else {
        return;
    };

    let mut current = NEXT_WORKSPACE_ID.load(Ordering::Relaxed);
    while current < next {
        match NEXT_WORKSPACE_ID.compare_exchange_weak(
            current,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

/// A named workspace containing one stable pane layout. Each pane owns its terminal-tab stack.
pub struct Workspace {
    /// Stable public workspace identity, independent of display order.
    pub id: String,
    /// User-provided override. If set, auto-derived identity stops updating.
    pub custom_name: Option<String>,
    /// Fallback workspace identity source for tests, old snapshots, or missing runtimes.
    pub identity_cwd: PathBuf,
    /// CWD from which the cached automatic label and Git metadata were derived.
    pub(crate) cached_identity_cwd: PathBuf,
    /// Automatic workspace label cached outside the render path.
    pub(crate) cached_auto_label: String,
    /// Cache key for periodic Git status associated with `cached_identity_cwd`.
    pub(crate) cached_git_status_key: PathBuf,
    /// Cached current git branch for the workspace repo.
    pub(crate) cached_git_branch: Option<String>,
    /// Cached ahead/behind counts for the workspace repo's current branch upstream.
    pub(crate) cached_git_ahead_behind: Option<(usize, usize)>,
    /// Cached derived Git repo metadata for worktree actions and status display.
    pub(crate) cached_git_space: Option<GitSpaceMetadata>,
    /// Explicit Herdr-managed worktree grouping provenance.
    pub worktree_space: Option<WorktreeSpaceMembership>,
    pub(crate) metadata_tokens: crate::metadata_tokens::MetadataTokens,
    pub(crate) metadata_token_sequences: HashMap<String, u64>,
    /// Public pane numbers within this workspace. Closed pane numbers are not reused.
    pub public_pane_numbers: HashMap<PaneId, usize>,
    pub(crate) next_public_pane_number: usize,
    pub(crate) next_public_tab_number: usize,
    /// Identity source for this workspace's pane tree.
    pub root_pane: PaneId,
    pub layout: TileLayout,
    /// Stable pane viewport state and pane-owned logical terminal tabs.
    pub panes: HashMap<PaneId, PaneState>,
    pub zoomed: bool,
    pub events: mpsc::Sender<AppEvent>,
    pub(crate) render_notify: Arc<Notify>,
    pub(crate) render_dirty: Arc<RenderSignal>,
    #[cfg(test)]
    pub(crate) test_runtimes: HashMap<PaneId, TerminalRuntime>,
}

enum SplitCommand<'a> {
    Shell { command: &'a str },
    Argv { argv: &'a [String] },
}

impl Workspace {
    pub(crate) fn from_existing_pane(
        label: Option<String>,
        tab_label: Option<String>,
        identity_cwd: PathBuf,
        mut moved: MovedPane,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
    ) -> Self {
        let id = generate_workspace_id();
        let root_pane = moved.pane_id;
        if let Some(tab_label) = tab_label {
            moved.pane_state.active_tab_mut().custom_name = Some(tab_label);
        }
        let next_public_tab_number = moved
            .pane_state
            .tabs
            .iter()
            .map(|tab| tab.number)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let layout = TileLayout::from_saved(Node::Pane(root_pane), root_pane);
        let mut panes = HashMap::new();
        panes.insert(root_pane, moved.pane_state);
        let mut public_pane_numbers = HashMap::new();
        public_pane_numbers.insert(root_pane, 1);
        let (cached_git_space, cached_auto_label, cached_git_status_key) =
            discover_workspace_git_identity(&identity_cwd);
        Self {
            id,
            custom_name: label,
            identity_cwd: identity_cwd.clone(),
            cached_identity_cwd: identity_cwd.clone(),
            cached_auto_label,
            cached_git_status_key,
            cached_git_branch: git_branch(&identity_cwd),
            cached_git_ahead_behind: None,
            cached_git_space,
            worktree_space: None,
            metadata_tokens: crate::metadata_tokens::MetadataTokens::default(),
            metadata_token_sequences: HashMap::new(),
            public_pane_numbers,
            next_public_pane_number: 2,
            next_public_tab_number,
            root_pane,
            layout,
            panes,
            zoomed: false,
            events,
            render_notify,
            render_dirty,
            #[cfg(test)]
            test_runtimes: HashMap::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_extra_env(
        initial_cwd: PathBuf,
        rows: u16,
        cols: u16,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        shell_config: crate::pane::PaneShellConfig<'_>,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
        extra_env: Vec<(String, String)>,
    ) -> std::io::Result<(Self, TerminalState, TerminalRuntime)> {
        Self::new_with_tab(
            initial_cwd,
            rows,
            cols,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            shell_config,
            events,
            render_notify,
            render_dirty,
            None,
            extra_env,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_with_tab(
        initial_cwd: PathBuf,
        rows: u16,
        cols: u16,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        shell_config: crate::pane::PaneShellConfig<'_>,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
        argv: Option<&[String]>,
        extra_env: Vec<(String, String)>,
    ) -> std::io::Result<(Self, TerminalState, TerminalRuntime)> {
        let id = generate_workspace_id();
        let (layout, root_pane) = TileLayout::new();
        let launch_env = PaneLaunchEnv::from_extra(extra_env).with_identity(
            id.clone(),
            public_pane_tab_id_for_number(&id, 1, 1),
            public_pane_id_for_number(&id, 1),
        );
        let terminal_id = TerminalId::alloc();
        let runtime = if let Some(argv) = argv {
            TerminalRuntime::spawn_argv_command(
                root_pane,
                terminal_id.clone(),
                rows,
                cols,
                initial_cwd.clone(),
                argv,
                &launch_env,
                crate::pane::AgentDetection::Enabled,
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                events.clone(),
                render_notify.clone(),
                render_dirty.clone(),
            )?
        } else {
            TerminalRuntime::spawn(
                root_pane,
                terminal_id.clone(),
                rows,
                cols,
                initial_cwd.clone(),
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                shell_config,
                &launch_env,
                events.clone(),
                render_notify.clone(),
                render_dirty.clone(),
            )?
        };
        let terminal = match argv {
            Some(argv) => TerminalState::new(terminal_id.clone(), initial_cwd.clone())
                .with_launch_argv(argv.to_vec()),
            None => TerminalState::new(terminal_id.clone(), initial_cwd.clone()),
        };
        let mut panes = HashMap::new();
        panes.insert(root_pane, PaneState::new(1, terminal_id));
        let mut public_pane_numbers = HashMap::new();
        public_pane_numbers.insert(root_pane, 1);
        let (cached_git_space, cached_auto_label, cached_git_status_key) =
            discover_workspace_git_identity(&initial_cwd);
        Ok((
            Self {
                id,
                custom_name: None,
                identity_cwd: initial_cwd.clone(),
                cached_identity_cwd: initial_cwd.clone(),
                cached_auto_label,
                cached_git_status_key,
                cached_git_branch: git_branch(&initial_cwd),
                cached_git_ahead_behind: None,
                cached_git_space,
                worktree_space: None,
                metadata_tokens: crate::metadata_tokens::MetadataTokens::default(),
                metadata_token_sequences: HashMap::new(),
                public_pane_numbers,
                next_public_pane_number: 2,
                next_public_tab_number: 2,
                root_pane,
                layout,
                panes,
                zoomed: false,
                events,
                render_notify,
                render_dirty,
                #[cfg(test)]
                test_runtimes: HashMap::new(),
            },
            terminal,
            runtime,
        ))
    }

    fn focused_pane(&self) -> Option<&PaneState> {
        self.panes.get(&self.layout.focused())
    }

    fn focused_pane_mut(&mut self) -> Option<&mut PaneState> {
        self.panes.get_mut(&self.layout.focused())
    }

    pub fn active_tab(&self) -> Option<&PaneTab> {
        self.focused_pane().map(PaneState::active_tab)
    }

    pub fn active_tab_index(&self) -> usize {
        self.focused_pane().map_or(0, |pane| pane.active_tab)
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut PaneTab> {
        self.focused_pane_mut().map(PaneState::active_tab_mut)
    }

    pub fn tab_count(&self) -> usize {
        self.focused_pane().map_or(0, |pane| pane.tabs.len())
    }

    pub fn tab(&self, tab_idx: usize) -> Option<&PaneTab> {
        self.focused_pane()?.tabs.get(tab_idx)
    }

    #[cfg(test)]
    pub fn tab_mut(&mut self, tab_idx: usize) -> Option<&mut PaneTab> {
        self.focused_pane_mut()?.tabs.get_mut(tab_idx)
    }

    pub fn active_tab_display_name(&self) -> Option<String> {
        self.tab_display_name(self.active_tab_index())
    }

    pub fn tab_display_name(&self, tab_idx: usize) -> Option<String> {
        let tab = self.tab(tab_idx)?;
        Some(
            tab.custom_name
                .clone()
                .unwrap_or_else(|| (tab_idx + 1).to_string()),
        )
    }

    pub fn switch_tab(&mut self, idx: usize) {
        if let Some(pane) = self.focused_pane_mut() {
            pane.switch_tab(idx);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_tab_in_pane(
        &mut self,
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        cwd: PathBuf,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        shell_config: crate::pane::PaneShellConfig<'_>,
        extra_env: Vec<(String, String)>,
    ) -> std::io::Result<(usize, TerminalState, TerminalRuntime)> {
        self.create_tab_with_runtime(
            pane_id,
            rows,
            cols,
            cwd,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            shell_config,
            None,
            extra_env,
        )
    }

    pub fn create_tab_argv_command(
        &mut self,
        rows: u16,
        cols: u16,
        cwd: PathBuf,
        argv: &[String],
        extra_env: Vec<(String, String)>,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
    ) -> std::io::Result<(usize, TerminalState, TerminalRuntime)> {
        self.create_tab_argv_command_in_pane(
            self.layout.focused(),
            rows,
            cols,
            cwd,
            argv,
            extra_env,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_tab_argv_command_in_pane(
        &mut self,
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        cwd: PathBuf,
        argv: &[String],
        extra_env: Vec<(String, String)>,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
    ) -> std::io::Result<(usize, TerminalState, TerminalRuntime)> {
        self.create_tab_with_runtime(
            pane_id,
            rows,
            cols,
            cwd,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            crate::pane::PaneShellConfig::new("", crate::config::ShellModeConfig::NonLogin),
            Some(argv),
            extra_env,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn create_tab_with_runtime(
        &mut self,
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        cwd: PathBuf,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        shell_config: crate::pane::PaneShellConfig<'_>,
        argv: Option<&[String]>,
        extra_env: Vec<(String, String)>,
    ) -> std::io::Result<(usize, TerminalState, TerminalRuntime)> {
        if !self.panes.contains_key(&pane_id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "tab owner pane not found",
            ));
        }
        let number = self.next_public_tab_number;
        let pane_number = self
            .public_pane_number(pane_id)
            .expect("pane must have a public number");
        let launch_env = self.launch_env_for_new_pane(number, pane_number, extra_env);
        let terminal_id = TerminalId::alloc();
        let runtime = if let Some(argv) = argv {
            TerminalRuntime::spawn_argv_command(
                pane_id,
                terminal_id.clone(),
                rows,
                cols,
                cwd.clone(),
                argv,
                &launch_env,
                crate::pane::AgentDetection::Enabled,
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                self.events.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            )?
        } else {
            TerminalRuntime::spawn(
                pane_id,
                terminal_id.clone(),
                rows,
                cols,
                cwd.clone(),
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                shell_config,
                &launch_env,
                self.events.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            )?
        };
        let terminal = match argv {
            Some(argv) => {
                TerminalState::new(terminal_id.clone(), cwd).with_launch_argv(argv.to_vec())
            }
            None => TerminalState::new(terminal_id.clone(), cwd),
        };
        self.next_public_tab_number += 1;
        let pane = self
            .panes
            .get_mut(&pane_id)
            .expect("pane must have pane state");
        pane.tabs.push(PaneTab::new(number, terminal_id));
        let tab_idx = pane.tabs.len() - 1;
        Ok((tab_idx, terminal, runtime))
    }

    pub fn close_tab(&mut self, idx: usize) -> bool {
        use crate::pane::PaneTabCloseOutcome;
        let pane_id = self.layout.focused();
        let Some(pane) = self.panes.get_mut(&pane_id) else {
            return false;
        };
        match pane.close_tab(idx) {
            PaneTabCloseOutcome::Removed(_) => true,
            PaneTabCloseOutcome::NotFound => false,
            PaneTabCloseOutcome::SoleTab if self.layout.pane_count() <= 1 => false,
            PaneTabCloseOutcome::SoleTab => {
                self.detach_pane(pane_id);
                true
            }
        }
    }

    #[cfg(test)]
    pub fn move_tab(&mut self, source_idx: usize, insert_idx: usize) -> bool {
        self.focused_pane_mut()
            .is_some_and(|pane| pane.move_tab(source_idx, insert_idx))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn split_focused_command(
        &mut self,
        direction: Direction,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        command: &str,
        extra_env: Vec<(String, String)>,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
    ) -> std::io::Result<crate::workspace::tab::NewPane> {
        let pane_id = self.layout.focused();
        self.split_pane_runtime(
            pane_id,
            true,
            direction,
            None,
            rows,
            cols,
            cwd,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            crate::pane::PaneShellConfig::new("", crate::config::ShellModeConfig::NonLogin),
            extra_env,
            Some(SplitCommand::Shell { command }),
        )
    }

    // Workspace split routing carries pane identity, geometry, host context, and focus policy.
    #[allow(clippy::too_many_arguments)]
    pub fn split_pane(
        &mut self,
        pane_id: PaneId,
        direction: Direction,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        shell_config: crate::pane::PaneShellConfig<'_>,
        extra_env: Vec<(String, String)>,
        focus_new_pane: bool,
    ) -> Option<std::io::Result<(usize, crate::workspace::tab::NewPane)>> {
        self.panes.get(&pane_id)?;
        Some(
            self.split_pane_runtime(
                pane_id,
                focus_new_pane,
                direction,
                None,
                rows,
                cols,
                cwd,
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                shell_config,
                extra_env,
                None,
            )
            .map(|pane| (0, pane)),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn split_pane_with_ratio(
        &mut self,
        pane_id: PaneId,
        direction: Direction,
        ratio: f32,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        shell_config: crate::pane::PaneShellConfig<'_>,
        extra_env: Vec<(String, String)>,
        focus_new_pane: bool,
    ) -> Option<std::io::Result<(usize, crate::workspace::tab::NewPane)>> {
        self.panes.get(&pane_id)?;
        Some(
            self.split_pane_runtime(
                pane_id,
                focus_new_pane,
                direction,
                Some(ratio),
                rows,
                cols,
                cwd,
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                shell_config,
                extra_env,
                None,
            )
            .map(|pane| (0, pane)),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn split_pane_argv_command(
        &mut self,
        pane_id: PaneId,
        direction: Direction,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        argv: &[String],
        extra_env: Vec<(String, String)>,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        focus_new_pane: bool,
    ) -> Option<std::io::Result<(usize, crate::workspace::tab::NewPane)>> {
        self.split_pane_argv_command_with_ratio(
            pane_id,
            direction,
            0.5,
            rows,
            cols,
            cwd,
            argv,
            extra_env,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            focus_new_pane,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn split_pane_argv_command_with_ratio(
        &mut self,
        pane_id: PaneId,
        direction: Direction,
        ratio: f32,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        argv: &[String],
        extra_env: Vec<(String, String)>,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        focus_new_pane: bool,
    ) -> Option<std::io::Result<(usize, crate::workspace::tab::NewPane)>> {
        self.panes.get(&pane_id)?;
        let argv_command = SplitCommand::Argv { argv };
        Some(
            self.split_pane_runtime(
                pane_id,
                focus_new_pane,
                direction,
                Some(ratio),
                rows,
                cols,
                cwd,
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                crate::pane::PaneShellConfig::new("", crate::config::ShellModeConfig::NonLogin),
                extra_env,
                Some(argv_command),
            )
            .map(|pane| (0, pane)),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn split_pane_runtime(
        &mut self,
        pane_id: PaneId,
        focus_new_pane: bool,
        direction: Direction,
        ratio: Option<f32>,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        shell_config: crate::pane::PaneShellConfig<'_>,
        extra_env: Vec<(String, String)>,
        command: Option<SplitCommand<'_>>,
    ) -> std::io::Result<crate::workspace::tab::NewPane> {
        let Some(new_id) = self
            .layout
            .split_pane(pane_id, direction, ratio.unwrap_or(0.5))
        else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "split target pane is not in the layout",
            ));
        };
        let actual_cwd =
            cwd.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()));
        let number = self.next_public_tab_number;
        let pane_number = self.next_public_pane_number;
        let launch_env = self.launch_env_for_new_pane(number, pane_number, extra_env);
        let launch_argv = match &command {
            Some(SplitCommand::Argv { argv, .. }) => Some((*argv).to_vec()),
            _ => None,
        };
        let terminal_id = TerminalId::alloc();
        let runtime = match command {
            Some(SplitCommand::Shell { command }) => TerminalRuntime::spawn_shell_command(
                new_id,
                terminal_id.clone(),
                rows,
                cols,
                actual_cwd.clone(),
                command,
                &launch_env,
                crate::pane::AgentDetection::Enabled,
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                self.events.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            ),
            Some(SplitCommand::Argv { argv }) => TerminalRuntime::spawn_argv_command(
                new_id,
                terminal_id.clone(),
                rows,
                cols,
                actual_cwd.clone(),
                argv,
                &launch_env,
                crate::pane::AgentDetection::Enabled,
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                self.events.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            ),
            None => TerminalRuntime::spawn(
                new_id,
                terminal_id.clone(),
                rows,
                cols,
                actual_cwd.clone(),
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                shell_config,
                &launch_env,
                self.events.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            ),
        };
        let runtime = match runtime {
            Ok(runtime) => runtime,
            Err(error) => {
                self.layout.close_pane(new_id);
                return Err(error);
            }
        };
        let terminal = match launch_argv {
            Some(argv) => {
                TerminalState::new(terminal_id.clone(), actual_cwd).with_launch_argv(argv)
            }
            None => TerminalState::new(terminal_id.clone(), actual_cwd),
        };
        self.next_public_tab_number += 1;
        self.register_new_pane_with_number(new_id, pane_number);
        self.panes
            .insert(new_id, PaneState::new(number, terminal_id));
        if focus_new_pane {
            self.layout.focus_pane(new_id);
        }
        self.zoomed = false;
        Ok(crate::workspace::tab::NewPane {
            pane_id: new_id,
            terminal,
            runtime,
        })
    }

    /// Close the focused pane. Returns true if the workspace should close.
    #[cfg(test)]
    pub fn close_focused(&mut self) -> bool {
        let pane_id = self.layout.focused();
        self.close_pane(pane_id)
    }

    pub(crate) fn take_pane_for_move(&mut self, pane_id: PaneId) -> Option<TakenPane> {
        if !self.panes.contains_key(&pane_id) {
            return None;
        }
        let workspace_empty = self.layout.pane_count() <= 1;
        let pane_state = if workspace_empty {
            self.panes.remove(&pane_id)?
        } else {
            self.detach_pane(pane_id)?.pane_state
        };
        Some(TakenPane {
            moved: MovedPane {
                pane_id,
                pane_state,
            },
            removed_tab_idx: None,
            workspace_empty,
        })
    }

    pub(crate) fn insert_moved_pane_into_tab(
        &mut self,
        _tab_idx: usize,
        target_pane_id: PaneId,
        mut moved: MovedPane,
        direction: Direction,
        ratio: f32,
        focus: bool,
    ) -> Result<PaneId, MovedPane> {
        if !self
            .layout
            .insert_pane_near(target_pane_id, moved.pane_id, direction, ratio, focus)
        {
            return Err(moved);
        }
        self.reserve_moved_tab_numbers(&mut moved.pane_state);
        let pane_id = moved.pane_id;
        self.panes.insert(pane_id, moved.pane_state);
        if !self.public_pane_numbers.contains_key(&pane_id) {
            self.register_new_pane_with_number(pane_id, self.next_public_pane_number);
        }
        self.zoomed = false;
        Ok(pane_id)
    }

    pub(crate) fn create_tab_from_existing_pane(
        &mut self,
        mut moved: MovedPane,
        label: Option<String>,
        _fallback_events: mpsc::Sender<AppEvent>,
        _fallback_render_notify: Arc<Notify>,
        _fallback_render_dirty: Arc<RenderSignal>,
    ) -> usize {
        if let Some(label) = label {
            moved.pane_state.active_tab_mut().custom_name = Some(label);
        }
        let target = self.layout.focused();
        let pane_id = moved.pane_id;
        self.reserve_moved_tab_numbers(&mut moved.pane_state);
        if self
            .layout
            .insert_pane_near(target, pane_id, Direction::Horizontal, 0.5, true)
        {
            self.panes.insert(pane_id, moved.pane_state);
            if !self.public_pane_numbers.contains_key(&pane_id) {
                self.register_new_pane_with_number(pane_id, self.next_public_pane_number);
            }
            self.active_tab_index()
        } else {
            0
        }
    }

    fn reserve_moved_tab_numbers(&mut self, pane: &mut PaneState) {
        let mut used = self
            .panes
            .values()
            .flat_map(|pane| pane.tabs.iter().map(|tab| tab.number))
            .collect::<std::collections::HashSet<_>>();
        for tab in &mut pane.tabs {
            if used.contains(&tab.number) {
                tab.number = self.next_public_tab_number;
                self.next_public_tab_number += 1;
            } else {
                self.next_public_tab_number = self.next_public_tab_number.max(tab.number + 1);
            }
            used.insert(tab.number);
        }
    }

    fn detach_pane(&mut self, pane_id: PaneId) -> Option<MovedPane> {
        if self.layout.pane_count() <= 1 {
            return None;
        }
        let next_root = (self.root_pane == pane_id)
            .then(|| self.layout.pane_ids().into_iter().find(|id| *id != pane_id))
            .flatten();
        self.layout.close_pane(pane_id);
        let pane_state = self.panes.remove(&pane_id)?;
        self.unregister_pane(pane_id);
        if let Some(next_root) = next_root {
            self.root_pane = next_root;
        }
        self.zoomed = false;
        Some(MovedPane {
            pane_id,
            pane_state,
        })
    }

    pub(crate) fn unregister_moved_pane(&mut self, pane_id: PaneId) {
        self.unregister_pane(pane_id);
    }

    pub(crate) fn restore_moved_pane_number(&mut self, pane_id: PaneId, number: usize) {
        self.public_pane_numbers.insert(pane_id, number);
    }

    pub fn public_pane_number(&self, pane_id: PaneId) -> Option<usize> {
        self.public_pane_numbers.get(&pane_id).copied()
    }

    fn launch_env_for_new_pane(
        &self,
        tab_number: usize,
        pane_number: usize,
        extra_env: Vec<(String, String)>,
    ) -> PaneLaunchEnv {
        PaneLaunchEnv::from_extra(extra_env).with_identity(
            self.id.clone(),
            public_pane_tab_id_for_number(&self.id, pane_number, tab_number),
            public_pane_id_for_number(&self.id, pane_number),
        )
    }

    pub fn public_tab_number(&self, tab_idx: usize) -> Option<usize> {
        self.tab(tab_idx).map(|tab| tab.number)
    }

    pub fn set_custom_name(&mut self, name: String) {
        self.custom_name = Some(name);
    }

    #[cfg(test)]
    pub fn resolved_identity_cwd(&self) -> Option<PathBuf> {
        Some(self.identity_cwd.clone())
    }

    pub fn resolved_identity_cwd_from(
        &self,
        terminals: &HashMap<TerminalId, TerminalState>,
        terminal_runtimes: &TerminalRuntimeRegistry,
    ) -> Option<PathBuf> {
        self.cwd_for_pane(self.root_pane, terminals, terminal_runtimes)
            .or_else(|| Some(self.identity_cwd.clone()))
    }

    #[cfg(test)]
    pub fn display_name(&self) -> String {
        if let Some(name) = &self.custom_name {
            return name.clone();
        }
        self.automatic_display_name_for_cwd(&self.identity_cwd)
    }

    pub(crate) fn display_name_from_terminals(
        &self,
        terminals: &HashMap<TerminalId, TerminalState>,
    ) -> String {
        if let Some(name) = &self.custom_name {
            return name.clone();
        }
        let cwd = self
            .terminal_id(self.root_pane)
            .and_then(|terminal_id| terminals.get(terminal_id))
            .map(|terminal| &terminal.cwd)
            .unwrap_or(&self.identity_cwd);
        self.automatic_display_name_for_cwd(cwd)
    }

    pub fn display_name_from(
        &self,
        terminals: &HashMap<TerminalId, TerminalState>,
        terminal_runtimes: &TerminalRuntimeRegistry,
    ) -> String {
        if let Some(name) = &self.custom_name {
            return name.clone();
        }
        self.resolved_identity_cwd_from(terminals, terminal_runtimes)
            .map(|cwd| self.automatic_display_name_for_cwd(&cwd))
            .unwrap_or_else(|| "workspace".into())
    }

    fn automatic_display_name_for_cwd(&self, cwd: &std::path::Path) -> String {
        if cwd == self.cached_identity_cwd {
            self.cached_auto_label.clone()
        } else {
            fallback_label_from_cwd(cwd)
        }
    }

    pub fn branch(&self) -> Option<String> {
        self.cached_git_branch.clone()
    }

    pub fn git_ahead_behind(&self) -> Option<(usize, usize)> {
        self.cached_git_ahead_behind
    }

    pub fn git_space(&self) -> Option<&GitSpaceMetadata> {
        self.cached_git_space.as_ref()
    }

    pub fn worktree_space(&self) -> Option<&WorktreeSpaceMembership> {
        self.worktree_space.as_ref()
    }

    /// Compatibility projection: every pane is in the workspace's one layout.
    pub fn find_tab_index_for_pane(&self, pane_id: PaneId) -> Option<usize> {
        self.panes.contains_key(&pane_id).then_some(0)
    }

    pub fn pane_state(&self, pane_id: PaneId) -> Option<&PaneState> {
        self.panes.get(&pane_id)
    }

    pub fn pane_state_mut(&mut self, pane_id: PaneId) -> Option<&mut PaneState> {
        self.panes.get_mut(&pane_id)
    }

    pub fn terminal_id(&self, pane_id: PaneId) -> Option<&TerminalId> {
        self.panes.get(&pane_id).map(PaneState::active_terminal_id)
    }

    pub fn terminal_location(&self, terminal_id: &TerminalId) -> Option<(PaneId, usize)> {
        self.panes.iter().find_map(|(pane_id, pane)| {
            pane.tabs
                .iter()
                .position(|tab| &tab.terminal_id == terminal_id)
                .map(|tab_idx| (*pane_id, tab_idx))
        })
    }

    pub fn terminal_ids(&self) -> impl Iterator<Item = &TerminalId> {
        self.panes
            .values()
            .flat_map(|pane| pane.tabs.iter().map(|tab| &tab.terminal_id))
    }

    pub fn focused_pane_id(&self) -> Option<PaneId> {
        self.panes
            .contains_key(&self.layout.focused())
            .then(|| self.layout.focused())
    }

    pub fn close_pane(&mut self, pane_id: PaneId) -> bool {
        if !self.panes.contains_key(&pane_id) {
            return false;
        }
        if self.layout.pane_count() <= 1 {
            return true;
        }
        self.detach_pane(pane_id);
        false
    }

    pub(crate) fn remove_terminal(&mut self, terminal_id: &TerminalId) -> WorkspaceTerminalRemoval {
        let Some((pane_id, _tab_idx)) = self.terminal_location(terminal_id) else {
            return WorkspaceTerminalRemoval::NotFound;
        };
        let pane_tab_count = self.panes[&pane_id].tabs.len();
        if pane_tab_count > 1 {
            let crate::pane::PaneTabCloseOutcome::Removed(tab) = self
                .panes
                .get_mut(&pane_id)
                .expect("pane exists")
                .remove_terminal(terminal_id)
            else {
                return WorkspaceTerminalRemoval::NotFound;
            };
            return WorkspaceTerminalRemoval::TabRemoved { pane_id, tab };
        }
        if self.layout.pane_count() <= 1 {
            return WorkspaceTerminalRemoval::WorkspaceWouldClose { pane_id };
        }
        let moved = self.detach_pane(pane_id).expect("non-sole pane detaches");
        WorkspaceTerminalRemoval::PaneRemoved {
            pane_id,
            tabs: moved.pane_state.tabs,
        }
    }

    pub fn cwd_for_pane(
        &self,
        pane_id: PaneId,
        terminals: &HashMap<TerminalId, TerminalState>,
        terminal_runtimes: &TerminalRuntimeRegistry,
    ) -> Option<PathBuf> {
        let terminal_id = self.terminal_id(pane_id)?;
        terminal_runtimes
            .get(terminal_id)
            .and_then(|runtime| runtime.cwd())
            .or_else(|| {
                terminals
                    .get(terminal_id)
                    .map(|terminal| terminal.cwd.clone())
            })
    }

    pub fn foreground_cwd_for_pane(
        &self,
        pane_id: PaneId,
        terminal_runtimes: &TerminalRuntimeRegistry,
    ) -> Option<PathBuf> {
        let terminal_id = self.terminal_id(pane_id)?;
        terminal_runtimes
            .get(terminal_id)
            .and_then(|runtime| runtime.foreground_cwd())
    }

    #[cfg(test)]
    fn register_new_pane(&mut self, pane_id: PaneId) {
        self.register_new_pane_with_number(pane_id, self.next_public_pane_number);
    }

    fn register_new_pane_with_number(&mut self, pane_id: PaneId, number: usize) {
        self.public_pane_numbers.insert(pane_id, number);
        self.next_public_pane_number = self.next_public_pane_number.max(number + 1);
    }

    fn unregister_pane(&mut self, pane_id: PaneId) {
        self.public_pane_numbers.remove(&pane_id);
    }
}

pub(crate) enum WorkspaceTerminalRemoval {
    NotFound,
    TabRemoved { pane_id: PaneId, tab: PaneTab },
    PaneRemoved { pane_id: PaneId, tabs: Vec<PaneTab> },
    WorkspaceWouldClose { pane_id: PaneId },
}

pub(crate) struct TakenPane {
    pub moved: MovedPane,
    pub removed_tab_idx: Option<usize>,
    pub workspace_empty: bool,
}

#[cfg(test)]
impl Workspace {
    pub(crate) fn test_new(name: &str) -> Self {
        let (events, _) = mpsc::channel(64);
        let render_notify = Arc::new(Notify::new());
        let render_dirty = Arc::new(RenderSignal::new());
        let identity_cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
        let (layout, root_pane) = TileLayout::new();
        let mut panes = HashMap::new();
        panes.insert(root_pane, PaneState::new(1, TerminalId::alloc()));
        let mut public_pane_numbers = HashMap::new();
        public_pane_numbers.insert(root_pane, 1);
        Self {
            id: generate_workspace_id(),
            custom_name: Some(name.to_string()),
            identity_cwd: identity_cwd.clone(),
            cached_identity_cwd: identity_cwd.clone(),
            cached_auto_label: fallback_label_from_cwd(&identity_cwd),
            cached_git_status_key: identity_cwd.clone(),
            cached_git_branch: git_branch(&identity_cwd),
            cached_git_ahead_behind: None,
            cached_git_space: None,
            worktree_space: None,
            metadata_tokens: crate::metadata_tokens::MetadataTokens::default(),
            metadata_token_sequences: HashMap::new(),
            public_pane_numbers,
            next_public_pane_number: 2,
            next_public_tab_number: 2,
            root_pane,
            layout,
            panes,
            zoomed: false,
            events,
            render_notify,
            render_dirty,
            test_runtimes: HashMap::new(),
        }
    }

    pub(crate) fn insert_test_runtime(&mut self, pane_id: PaneId, runtime: TerminalRuntime) {
        self.test_runtimes.insert(pane_id, runtime);
    }

    pub(crate) fn test_split(&mut self, direction: Direction) -> PaneId {
        let new_id = self.layout.split_focused(direction);
        let number = self.next_public_tab_number;
        self.next_public_tab_number += 1;
        self.panes
            .insert(new_id, PaneState::new(number, TerminalId::alloc()));
        self.register_new_pane(new_id);
        new_id
    }

    pub(crate) fn test_add_tab(&mut self, name: Option<&str>) -> usize {
        let pane_id = self.layout.focused();
        self.test_add_tab_to_pane(pane_id, name)
    }

    pub(crate) fn test_add_tab_to_pane(&mut self, pane_id: PaneId, name: Option<&str>) -> usize {
        let mut tab = PaneTab::new(self.next_public_tab_number, TerminalId::alloc());
        tab.custom_name = name.map(str::to_string);
        self.next_public_tab_number += 1;
        let pane = self.panes.get_mut(&pane_id).expect("test pane must exist");
        pane.tabs.push(tab);
        pane.tabs.len() - 1
    }

    pub(crate) fn test_adversarial_identity_state() -> Self {
        let mut ws = Self::test_new("adversarial-identity");
        let removed_pane = ws.test_split(Direction::Horizontal);
        ws.test_split(Direction::Vertical);
        assert!(!ws.close_pane(removed_pane));
        let _unused_raw_id = PaneId::alloc();
        let later_pane = ws.test_split(Direction::Horizontal);
        ws.layout.focus_pane(ws.root_pane);

        let removed_tab = ws.test_add_tab(Some("removed"));
        ws.test_add_tab(None);
        let final_tab = ws.test_add_tab(None);
        assert!(ws.close_tab(removed_tab));
        assert!(ws.move_tab(0, ws.tab_count()));
        ws.switch_tab(final_tab.saturating_sub(1));

        assert_ne!(
            ws.active_tab_index() + 1,
            ws.active_tab().expect("active tab").number,
            "adversarial active tab must distinguish position from public tab number"
        );
        assert_ne!(
            later_pane.raw() as usize,
            ws.public_pane_number(later_pane).unwrap(),
            "adversarial pane must distinguish raw pane id from public pane number"
        );
        ws
    }

    pub(crate) fn assert_invariants_for_test(&self) {
        let layout_panes = self.layout.pane_ids();
        let layout_set: std::collections::HashSet<_> = layout_panes.iter().copied().collect();
        assert!(
            !layout_set.is_empty(),
            "workspace {} layout must not be empty",
            self.id
        );
        assert_eq!(
            layout_panes.len(),
            layout_set.len(),
            "workspace {} layout contains duplicate pane ids",
            self.id
        );
        assert!(
            layout_set.contains(&self.layout.focused()),
            "workspace {} focused pane is not in layout",
            self.id
        );
        let pane_set: std::collections::HashSet<_> = self.panes.keys().copied().collect();
        assert_eq!(
            layout_set, pane_set,
            "workspace {} layout panes must exactly match pane states",
            self.id
        );
        assert!(
            self.panes.contains_key(&self.root_pane),
            "workspace {} root pane must be live",
            self.id
        );

        let mut tab_numbers = std::collections::HashSet::new();
        let mut terminal_ids = std::collections::HashSet::new();
        let mut max_tab_number = 0usize;
        for (pane_id, pane) in &self.panes {
            assert!(
                !pane.tabs.is_empty(),
                "workspace {} pane {:?} must contain a logical tab",
                self.id,
                pane_id
            );
            assert!(
                pane.active_tab < pane.tabs.len(),
                "workspace {} pane {:?} active tab is out of bounds",
                self.id,
                pane_id
            );
            assert!(
                self.public_pane_numbers.contains_key(pane_id),
                "workspace {} pane {:?} has no public pane number",
                self.id,
                pane_id
            );
            for tab in &pane.tabs {
                assert!(tab.number > 0, "workspace {} has tab number 0", self.id);
                assert!(
                    tab_numbers.insert(tab.number),
                    "workspace {} has duplicate public tab number {}",
                    self.id,
                    tab.number
                );
                max_tab_number = max_tab_number.max(tab.number);
                assert!(
                    terminal_ids.insert(tab.terminal_id.clone()),
                    "workspace {} terminal {} is owned more than once",
                    self.id,
                    tab.terminal_id
                );
            }
        }
        assert!(
            self.next_public_tab_number > max_tab_number,
            "workspace {} next tab number must exceed all live numbers",
            self.id
        );

        let public_pane_keys: std::collections::HashSet<_> =
            self.public_pane_numbers.keys().copied().collect();
        assert_eq!(
            public_pane_keys, pane_set,
            "workspace {} public pane map must exactly match live panes",
            self.id
        );
        let mut pane_numbers = std::collections::HashSet::new();
        let mut max_pane_number = 0usize;
        for (pane_id, pane_number) in &self.public_pane_numbers {
            assert!(
                *pane_number > 0,
                "workspace {} pane {:?} has number 0",
                self.id,
                pane_id
            );
            assert!(
                pane_numbers.insert(*pane_number),
                "workspace {} has duplicate pane number {}",
                self.id,
                pane_number
            );
            max_pane_number = max_pane_number.max(*pane_number);
        }
        assert!(
            self.next_public_pane_number > max_pane_number,
            "workspace {} next pane number must exceed all live numbers",
            self.id
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_workspace_ids_are_short_base32_handles() {
        let first = generate_workspace_id();
        let second = generate_workspace_id();

        assert!(first.starts_with('w'));
        assert!(second.starts_with('w'));
        assert_ne!(first, second);
        assert!(first.len() <= 3, "unexpectedly long workspace id: {first}");
        assert!(
            second.len() <= 3,
            "unexpectedly long workspace id: {second}"
        );
    }

    #[test]
    fn public_numbers_round_trip_readable_base32_handles() {
        assert_eq!(encode_public_number(1), "1");
        assert_eq!(encode_public_number(9), "9");
        assert_eq!(encode_public_number(10), "A");
        assert_eq!(encode_public_number(31), "Z");
        assert_eq!(encode_public_number(32), "0");
        assert_eq!(encode_public_number(33), "11");

        for value in [1, 9, 10, 31, 32, 33, 1024, 1025] {
            let encoded = encode_public_number(value);
            assert_eq!(decode_public_number(&encoded), Some(value));
        }
    }

    #[test]
    fn reserving_restored_workspace_ids_prevents_reuse() {
        let mut restored = Workspace::test_new("restored");
        restored.id = "wZ".to_string();

        reserve_workspace_ids(&[restored]);

        let generated = generate_workspace_id();
        assert_ne!(generated, "wZ");
        assert!(public_workspace_number(&generated) > public_workspace_number("wZ"));
    }

    #[test]
    fn pane_public_numbers_are_stable_and_not_reused_after_close() {
        let mut ws = Workspace::test_new("test");
        let root = ws.root_pane;
        let second = ws.test_split(Direction::Horizontal);
        let third = ws.test_split(Direction::Vertical);

        assert_eq!(ws.public_pane_number(root), Some(1));
        assert_eq!(ws.public_pane_number(second), Some(2));
        assert_eq!(ws.public_pane_number(third), Some(3));

        assert!(!ws.close_pane(second));

        assert_eq!(ws.public_pane_number(root), Some(1));
        assert_eq!(ws.public_pane_number(second), None);
        assert_eq!(ws.public_pane_number(third), Some(3));

        let fourth = ws.test_split(Direction::Horizontal);
        assert_eq!(ws.public_pane_number(fourth), Some(4));
    }

    #[test]
    fn tab_public_numbers_are_stable_and_not_reused_after_close() {
        let mut ws = Workspace::test_new("test");
        let second_tab = ws.test_add_tab(None);
        let third_tab = ws.test_add_tab(None);

        assert_eq!(ws.public_tab_number(0), Some(1));
        assert_eq!(ws.public_tab_number(second_tab), Some(2));
        assert_eq!(ws.public_tab_number(third_tab), Some(3));

        assert!(ws.close_tab(second_tab));

        assert_eq!(ws.public_tab_number(0), Some(1));
        assert_eq!(ws.public_tab_number(second_tab), Some(3));

        let fourth_tab = ws.test_add_tab(None);
        assert_eq!(ws.public_tab_number(fourth_tab), Some(4));
        ws.assert_invariants_for_test();
    }

    #[test]
    fn adversarial_identity_state_satisfies_workspace_invariants_after_mutation() {
        let mut ws = Workspace::test_adversarial_identity_state();
        ws.assert_invariants_for_test();

        let active_public = ws.active_tab().expect("active tab").number;
        assert_ne!(ws.active_tab_index() + 1, active_public);
        let divergent_pane = ws
            .public_pane_numbers
            .iter()
            .find_map(|(pane_id, public_number)| {
                (pane_id.raw() as usize != *public_number).then_some(*pane_id)
            })
            .expect("adversarial state should contain raw/public pane divergence");
        assert_ne!(
            divergent_pane.raw() as usize,
            ws.public_pane_number(divergent_pane).unwrap()
        );

        let new_pane = ws.test_split(Direction::Vertical);
        assert!(ws.public_pane_number(new_pane).is_some());
        ws.layout.focus_pane(ws.root_pane);
        assert!(ws.move_tab(0, ws.tab_count()));
        ws.assert_invariants_for_test();
    }

    #[test]
    fn failed_moved_pane_insert_returns_pane_for_recovery() {
        let mut source = Workspace::test_new("source");
        let source_pane = source.root_pane;
        let taken = source
            .take_pane_for_move(source_pane)
            .expect("source pane should be movable");
        let mut target = Workspace::test_new("target");
        let missing_target = PaneId::alloc();

        let recovered = target
            .insert_moved_pane_into_tab(
                0,
                missing_target,
                taken.moved,
                Direction::Horizontal,
                0.5,
                true,
            )
            .expect_err("invalid target should return the moved pane");

        assert_eq!(recovered.pane_id, source_pane);
        assert!(!target.panes.contains_key(&source_pane));
    }

    #[test]
    fn linked_worktree_auto_label_uses_checkout_name_not_repo_name() {
        let (base, repo, checkout) =
            self::git::test_support::create_repo_with_linked_worktree("linked-auto-label");

        let (space, auto_label, _) = discover_workspace_git_identity(&checkout);

        assert_eq!(
            space.unwrap().repo_name,
            repo.file_name().unwrap().to_str().unwrap()
        );
        assert_eq!(auto_label, checkout.file_name().unwrap().to_str().unwrap());

        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn display_name_reads_cached_identity_without_rechecking_filesystem() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be after unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "herdr-workspace-label-cache-{}-{stamp}",
            std::process::id()
        ));
        let cwd = root.join("deep/nested");
        std::fs::create_dir_all(&cwd).expect("create nested cwd");

        let mut ws = Workspace::test_new("ignored");
        ws.custom_name = None;
        ws.identity_cwd = cwd.clone();
        ws.cached_identity_cwd = cwd;
        ws.cached_auto_label = "cached-repo".into();

        std::fs::remove_dir_all(root).expect("remove cwd after cache admission");

        assert_eq!(ws.display_name(), "cached-repo");
    }

    #[test]
    fn terminal_aware_display_name_uses_latest_admitted_identity_cache() {
        let mut ws = Workspace::test_new("ignored");
        let root_pane = ws.root_pane;
        let terminal_id = ws.terminal_id(root_pane).unwrap().clone();
        ws.custom_name = None;
        ws.identity_cwd = PathBuf::from("/old/workspace");
        ws.cached_identity_cwd = PathBuf::from("/new/repo/deep");
        ws.cached_auto_label = "repo".into();
        let terminals = HashMap::from([(
            terminal_id.clone(),
            TerminalState::new(terminal_id, PathBuf::from("/new/repo/deep")),
        )]);

        assert_eq!(ws.display_name_from_terminals(&terminals), "repo");
        assert_eq!(ws.display_name(), "workspace");
    }

    #[test]
    fn workspace_identity_follows_first_tab_root_pane_cwd() {
        let mut ws = Workspace::test_new("ignored");
        ws.custom_name = None;
        let root_pane = ws.root_pane;
        let terminal_id = ws.terminal_id(root_pane).unwrap().clone();
        let mut terminals = HashMap::new();
        terminals.insert(
            terminal_id.clone(),
            TerminalState::new(terminal_id, PathBuf::from("/herdr-test/pion")),
        );
        let terminal_runtimes = TerminalRuntimeRegistry::new();

        assert_eq!(ws.display_name_from(&terminals, &terminal_runtimes), "pion");
        assert_eq!(
            ws.resolved_identity_cwd_from(&terminals, &terminal_runtimes),
            Some(PathBuf::from("/herdr-test/pion"))
        );
    }

    #[test]
    fn moving_tab_keeps_active_identity_and_stable_tab_numbers() {
        let mut ws = Workspace::test_new("test");
        let moved_number = ws.tab(0).expect("first tab").number;
        ws.test_add_tab(Some("foo"));
        let final_auto_idx = ws.test_add_tab(None);
        let active_number = ws.tab(final_auto_idx).expect("active candidate").number;
        ws.switch_tab(final_auto_idx);

        assert!(ws.move_tab(0, ws.tab_count()));

        let labels: Vec<_> = (0..ws.tab_count())
            .map(|tab_idx| ws.tab_display_name(tab_idx).unwrap())
            .collect();
        assert_eq!(labels, vec!["foo", "2", "3"]);
        assert_eq!(ws.tab(0).unwrap().custom_name.as_deref(), Some("foo"));
        assert!(ws.tab(1).unwrap().custom_name.is_none());
        assert!(ws.tab(2).unwrap().custom_name.is_none());
        assert_eq!(ws.tab(0).unwrap().number, 2);
        assert_eq!(ws.tab(1).unwrap().number, 3);
        assert_eq!(ws.tab(2).unwrap().number, moved_number);
        assert_eq!(ws.active_tab().expect("active tab").number, active_number);
        ws.assert_invariants_for_test();
    }
}
