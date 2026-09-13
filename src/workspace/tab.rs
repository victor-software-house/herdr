use crate::layout::PaneId;
use crate::pane::PaneState;
use crate::terminal::{TerminalRuntime, TerminalState};

pub(crate) struct MovedPane {
    pub pane_id: PaneId,
    pub pane_state: PaneState,
}

pub struct NewPane {
    pub pane_id: PaneId,
    pub terminal: TerminalState,
    pub runtime: TerminalRuntime,
}
