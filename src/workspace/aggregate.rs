use std::collections::HashMap;

use crate::detect::AgentState;
use crate::layout::PaneId;
use crate::terminal::{TerminalId, TerminalState};

use super::Workspace;

/// Detail info for a single logical terminal tab, used by the agent detail panel.
pub struct PaneDetail {
    pub pane_id: PaneId,
    pub tab_idx: usize,
    pub agent_kind_label: Option<String>,
    pub state: AgentState,
    pub seen: bool,
    pub last_agent_state_change_seq: Option<u64>,
    pub tokens: HashMap<String, String>,
}

fn pane_attention_priority(state: AgentState, seen: bool) -> u8 {
    match (state, seen) {
        (AgentState::Blocked, _) => 4,
        (AgentState::Idle, false) => 3,
        (AgentState::Working, _) => 2,
        (AgentState::Idle, true) => 1,
        (AgentState::Unknown, _) => 0,
    }
}

impl Workspace {
    pub fn aggregate_state(
        &self,
        terminals: &HashMap<TerminalId, TerminalState>,
    ) -> (AgentState, bool) {
        self.panes
            .values()
            .flat_map(|pane| pane.tabs.iter())
            .filter_map(|tab| {
                terminals
                    .get(&tab.terminal_id)
                    .map(|terminal| (terminal.state, tab.seen))
            })
            .max_by_key(|(state, seen)| pane_attention_priority(*state, *seen))
            .unwrap_or((AgentState::Unknown, true))
    }

    pub fn pane_details(&self, terminals: &HashMap<TerminalId, TerminalState>) -> Vec<PaneDetail> {
        self.layout
            .pane_ids()
            .into_iter()
            .flat_map(|pane_id| {
                self.panes.get(&pane_id).into_iter().flat_map(move |pane| {
                    pane.tabs
                        .iter()
                        .enumerate()
                        .filter_map(move |(tab_idx, tab)| {
                            let terminal = terminals.get(&tab.terminal_id)?;
                            let agent_kind_label =
                                terminal.effective_agent_label().map(str::to_string);
                            if terminal.agent_name.is_none() && agent_kind_label.is_none() {
                                return None;
                            }
                            Some(PaneDetail {
                                pane_id,
                                tab_idx,
                                agent_kind_label,
                                state: terminal.state,
                                seen: tab.seen,
                                last_agent_state_change_seq: terminal.last_agent_state_change_seq,
                                tokens: terminal.metadata_tokens.values(),
                            })
                        })
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Direction;

    use super::*;
    use crate::detect::Agent;

    fn terminal_for_pane(ws: &Workspace, pane_id: PaneId) -> TerminalState {
        TerminalState::new(ws.terminal_id(pane_id).unwrap().clone(), "/tmp".into())
    }

    #[test]
    fn aggregate_state_all_unknown() {
        let ws = Workspace::test_new("test");
        let mut terminals = HashMap::new();
        let terminal = terminal_for_pane(&ws, ws.root_pane);
        terminals.insert(terminal.id.clone(), terminal);
        let (state, seen) = ws.aggregate_state(&terminals);
        assert_eq!(state, AgentState::Unknown);
        assert!(seen);
    }

    #[test]
    fn aggregate_state_priority() {
        let mut ws = Workspace::test_new("test");
        let id2 = ws.test_split(Direction::Horizontal);
        let mut terminals = HashMap::new();
        let mut root_terminal = terminal_for_pane(&ws, ws.root_pane);
        root_terminal.state = AgentState::Idle;
        terminals.insert(root_terminal.id.clone(), root_terminal);
        let mut second_terminal = terminal_for_pane(&ws, id2);
        second_terminal.state = AgentState::Working;
        terminals.insert(second_terminal.id.clone(), second_terminal);

        assert_eq!(ws.aggregate_state(&terminals), (AgentState::Working, true));
    }

    #[test]
    fn aggregate_state_done_unseen_beats_working() {
        let mut ws = Workspace::test_new("test");
        let id2 = ws.test_split(Direction::Horizontal);
        let mut terminals = HashMap::new();
        let mut root_terminal = terminal_for_pane(&ws, ws.root_pane);
        root_terminal.state = AgentState::Idle;
        terminals.insert(root_terminal.id.clone(), root_terminal);
        let mut second_terminal = terminal_for_pane(&ws, id2);
        second_terminal.state = AgentState::Working;
        terminals.insert(second_terminal.id.clone(), second_terminal);
        ws.panes
            .get_mut(&ws.root_pane)
            .unwrap()
            .active_tab_mut()
            .seen = false;

        assert_eq!(ws.aggregate_state(&terminals), (AgentState::Idle, false));
    }

    #[test]
    fn pane_details_use_stack_index_not_stable_public_tab_number() {
        let mut ws = Workspace::test_new("test");
        let removed_tab = ws.test_add_tab(Some("removed"));
        let survivor_tab = ws.test_add_tab(Some("survivor"));
        assert!(ws.close_tab(removed_tab));
        let survivor_terminal = ws.panes[&ws.root_pane].tabs[survivor_tab - 1]
            .terminal_id
            .clone();

        let mut terminal = TerminalState::new(survivor_terminal, "/tmp".into());
        terminal.detected_agent = Some(Agent::Codex);
        let terminals = HashMap::from([(terminal.id.clone(), terminal)]);
        let details = ws.pane_details(&terminals);
        let survivor = details
            .iter()
            .find(|detail| detail.pane_id == ws.root_pane)
            .unwrap();

        assert_eq!(ws.panes[&ws.root_pane].tabs[1].number, 3);
        assert_eq!(survivor.tab_idx, 1);
    }
}
