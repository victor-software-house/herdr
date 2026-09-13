use super::{api_helpers::pane_agent_status, App};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalTarget {
    pub ws_idx: usize,
    pub tab_idx: usize,
    pub pane_id: crate::layout::PaneId,
    pub terminal_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalTargetCandidate {
    pub terminal_id: String,
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub cwd: Option<String>,
    pub agent_status: crate::api::schema::AgentStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TerminalTargetError {
    NotFound {
        target: String,
    },
    Ambiguous {
        target: String,
        candidates: Vec<TerminalTargetCandidate>,
    },
}

impl App {
    fn resolve_terminal_or_tab_target(
        &self,
        target: &str,
    ) -> Result<Option<TerminalTarget>, TerminalTargetError> {
        let terminal_matches: Vec<_> = self
            .terminal_targets()
            .into_iter()
            .filter(|candidate| candidate.terminal_id == target)
            .collect();
        if let Some(resolved) = self.single_terminal_match(target, terminal_matches)? {
            return Ok(Some(resolved));
        }

        if let Some((ws_idx, pane_id, tab_idx)) = self.parse_public_tab_id(target) {
            let terminal_id = self.state.workspaces[ws_idx].panes[&pane_id].tabs[tab_idx]
                .terminal_id
                .to_string();
            return Ok(Some(TerminalTarget {
                ws_idx,
                tab_idx,
                pane_id,
                terminal_id,
            }));
        }

        Ok(None)
    }

    pub(crate) fn resolve_pane_report_target(
        &self,
        target: &str,
    ) -> Result<TerminalTarget, TerminalTargetError> {
        if let Some(resolved) = self.resolve_terminal_or_tab_target(target)? {
            return Ok(resolved);
        }

        if let Some((ws_idx, pane_id)) = self.parse_current_public_pane_id(target) {
            let pane_matches = self.state.workspaces[ws_idx]
                .panes
                .get(&pane_id)
                .into_iter()
                .flat_map(|pane| pane.tabs.iter().enumerate())
                .map(|(tab_idx, tab)| TerminalTarget {
                    ws_idx,
                    tab_idx,
                    pane_id,
                    terminal_id: tab.terminal_id.to_string(),
                })
                .collect();
            if let Some(resolved) = self.single_terminal_match(target, pane_matches)? {
                return Ok(resolved);
            }
        }

        Err(TerminalTargetError::NotFound {
            target: target.to_string(),
        })
    }

    pub(crate) fn resolve_terminal_target(
        &self,
        target: &str,
    ) -> Result<TerminalTarget, TerminalTargetError> {
        if let Some(resolved) = self.resolve_terminal_or_tab_target(target)? {
            return Ok(resolved);
        }

        if let Some((ws_idx, pane_id)) = self.parse_current_public_pane_id(target) {
            if let Some(resolved) = self.terminal_target_for_pane(ws_idx, pane_id) {
                return Ok(resolved);
            }
        }

        let agent_matches: Vec<_> = self
            .terminal_targets()
            .into_iter()
            .filter(|candidate| {
                self.state
                    .terminals
                    .values()
                    .find(|terminal| terminal.id.to_string() == candidate.terminal_id)
                    .is_some_and(|terminal| {
                        terminal.agent_name.as_deref() == Some(target)
                            || terminal.effective_agent_label() == Some(target)
                    })
            })
            .collect();
        if let Some(resolved) = self.single_terminal_match(target, agent_matches)? {
            return Ok(resolved);
        }

        Err(TerminalTargetError::NotFound {
            target: target.to_string(),
        })
    }

    pub(crate) fn resolve_agent_target(
        &self,
        target: &str,
    ) -> Result<TerminalTarget, TerminalTargetError> {
        if let Some(resolved) = self.resolve_terminal_or_tab_target(target)? {
            return self
                .target_is_agent(&resolved)
                .then_some(resolved)
                .ok_or_else(|| TerminalTargetError::NotFound {
                    target: target.to_string(),
                });
        }

        if let Some((ws_idx, pane_id)) = self.parse_current_public_pane_id(target) {
            if let Some(resolved) = self
                .terminal_target_for_pane(ws_idx, pane_id)
                .filter(|resolved| self.target_is_agent(resolved))
            {
                return Ok(resolved);
            }
        }

        let name_matches: Vec<_> = self
            .terminal_targets()
            .into_iter()
            .filter(|candidate| {
                self.state
                    .terminals
                    .values()
                    .find(|terminal| terminal.id.to_string() == candidate.terminal_id)
                    .is_some_and(|terminal| terminal.agent_name.as_deref() == Some(target))
            })
            .collect();
        if let Some(resolved) = self.single_terminal_match(target, name_matches)? {
            return Ok(resolved);
        }

        Err(TerminalTargetError::NotFound {
            target: target.to_string(),
        })
    }

    fn target_is_agent(&self, target: &TerminalTarget) -> bool {
        self.state
            .terminals
            .values()
            .find(|terminal| terminal.id.to_string() == target.terminal_id)
            .is_some_and(|terminal| terminal.is_agent_terminal())
    }

    fn single_terminal_match(
        &self,
        target: &str,
        matches: Vec<TerminalTarget>,
    ) -> Result<Option<TerminalTarget>, TerminalTargetError> {
        match matches.len() {
            0 => Ok(None),
            1 => Ok(matches.into_iter().next()),
            _ => Err(TerminalTargetError::Ambiguous {
                target: target.to_string(),
                candidates: matches
                    .into_iter()
                    .filter_map(|candidate| {
                        self.terminal_target_candidate(
                            candidate.ws_idx,
                            candidate.pane_id,
                            candidate.tab_idx,
                        )
                    })
                    .collect(),
            }),
        }
    }

    fn terminal_targets(&self) -> Vec<TerminalTarget> {
        self.state
            .workspaces
            .iter()
            .enumerate()
            .flat_map(|(ws_idx, workspace)| {
                workspace.panes.iter().flat_map(move |(&pane_id, pane)| {
                    pane.tabs
                        .iter()
                        .enumerate()
                        .map(move |(tab_idx, tab)| TerminalTarget {
                            ws_idx,
                            tab_idx,
                            pane_id,
                            terminal_id: tab.terminal_id.to_string(),
                        })
                })
            })
            .collect()
    }

    fn terminal_target_for_pane(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<TerminalTarget> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane = ws.pane_state(pane_id)?;
        let tab_idx = pane.active_tab;
        let terminal_id = pane.active_terminal_id().to_string();
        Some(TerminalTarget {
            ws_idx,
            tab_idx,
            pane_id,
            terminal_id,
        })
    }

    fn terminal_target_candidate(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        tab_idx: usize,
    ) -> Option<TerminalTargetCandidate> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane = ws.pane_state(pane_id)?;
        let pane_tab = pane.tabs.get(tab_idx)?;
        let terminal = self.state.terminals.get(&pane_tab.terminal_id)?;
        Some(TerminalTargetCandidate {
            terminal_id: terminal.id.to_string(),
            pane_id: self.public_pane_id(ws_idx, pane_id)?,
            workspace_id: self.public_workspace_id(ws_idx),
            tab_id: self.public_tab_id_for_pane(ws_idx, pane_id, tab_idx)?,
            cwd: Some(terminal.cwd.display().to_string()),
            agent_status: pane_agent_status(terminal.state, pane_tab.seen),
        })
    }
}
