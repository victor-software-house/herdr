use crate::terminal::TerminalId;

/// A logical terminal tab owned by one stable pane.
#[derive(Debug, PartialEq, Eq)]
pub struct PaneTab {
    /// User-provided override. Automatic labels remain derived from terminal state.
    pub custom_name: Option<String>,
    /// Stable public tab number within the workspace. Closed numbers are never reused.
    pub number: usize,
    pub terminal_id: TerminalId,
    /// Whether the user has seen this tab since its last state change to Idle.
    pub seen: bool,
}

impl PaneTab {
    pub fn new(number: usize, terminal_id: TerminalId) -> Self {
        Self {
            custom_name: None,
            number,
            terminal_id,
            seen: true,
        }
    }

    pub fn is_auto_named(&self) -> bool {
        self.custom_name.is_none()
    }

    pub fn set_custom_name(&mut self, name: String) {
        self.custom_name = Some(name);
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PaneTabCloseOutcome {
    NotFound,
    SoleTab,
    Removed(PaneTab),
}

/// Viewport and logical terminal-tab state for a stable pane.
pub struct PaneState {
    /// Ordered logical terminal tabs. A live pane always contains at least one tab.
    pub tabs: Vec<PaneTab>,
    /// Default active tab used by API and headless operations.
    pub active_tab: usize,
    /// Whether unmodified right-click gestures should be forwarded to the pane application.
    pub right_click_passthrough: bool,
}

impl PaneState {
    pub fn new(tab_number: usize, terminal_id: TerminalId) -> Self {
        Self {
            tabs: vec![PaneTab::new(tab_number, terminal_id)],
            active_tab: 0,
            right_click_passthrough: false,
        }
    }

    pub fn active_tab(&self) -> &PaneTab {
        &self.tabs[self.active_tab]
    }

    pub fn active_tab_mut(&mut self) -> &mut PaneTab {
        &mut self.tabs[self.active_tab]
    }

    pub fn active_terminal_id(&self) -> &TerminalId {
        &self.active_tab().terminal_id
    }

    pub fn switch_tab(&mut self, index: usize) -> bool {
        let Some(tab) = self.tabs.get_mut(index) else {
            return false;
        };
        self.active_tab = index;
        tab.seen = true;
        true
    }

    pub fn move_tab(&mut self, source_index: usize, insert_index: usize) -> bool {
        if source_index >= self.tabs.len() || insert_index > self.tabs.len() {
            return false;
        }
        let target_index = if source_index < insert_index {
            insert_index.saturating_sub(1)
        } else {
            insert_index
        }
        .min(self.tabs.len().saturating_sub(1));
        if source_index == target_index {
            return false;
        }

        let active_number = self.active_tab().number;
        let tab = self.tabs.remove(source_index);
        self.tabs.insert(target_index, tab);
        self.active_tab = self
            .tabs
            .iter()
            .position(|tab| tab.number == active_number)
            .unwrap_or(target_index);
        true
    }

    pub fn close_tab(&mut self, index: usize) -> PaneTabCloseOutcome {
        if index >= self.tabs.len() {
            return PaneTabCloseOutcome::NotFound;
        }
        if self.tabs.len() == 1 {
            return PaneTabCloseOutcome::SoleTab;
        }
        let removed = self.tabs.remove(index);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        } else if index <= self.active_tab && self.active_tab > 0 {
            self.active_tab -= 1;
        }
        PaneTabCloseOutcome::Removed(removed)
    }

    pub fn remove_terminal(&mut self, terminal_id: &TerminalId) -> PaneTabCloseOutcome {
        let Some(index) = self
            .tabs
            .iter()
            .position(|tab| &tab.terminal_id == terminal_id)
        else {
            return PaneTabCloseOutcome::NotFound;
        };
        self.close_tab(index)
    }
}
