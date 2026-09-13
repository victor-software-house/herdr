use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::terminal::TerminalId;

#[derive(Debug, Default)]
pub(crate) struct RenderRequest {
    pub(crate) generic: bool,
    pub(crate) pty_sources: HashSet<TerminalId>,
    pub(crate) terminal_title_sources: HashSet<TerminalId>,
}

/// Coalesces render requests while retaining enough origin information for the
/// headless server to discard PTY-only updates hidden from every client.
#[derive(Debug, Default)]
pub(crate) struct RenderSignal {
    pending: AtomicBool,
    state: Mutex<RenderSignalState>,
}

#[derive(Debug, Default)]
struct RenderSignalState {
    request: RenderRequest,
    immediate_pty_sources: HashSet<TerminalId>,
}

impl RenderSignal {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    pub(crate) fn request_generic(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.request.generic = true;
        self.pending.store(true, Ordering::Release);
    }

    /// Returns true when the signal becomes pending or visible PTY work joins it.
    pub(crate) fn request_pty(&self, terminal_id: &TerminalId) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let source_added = if state.request.pty_sources.contains(terminal_id) {
            false
        } else {
            state.request.pty_sources.insert(terminal_id.clone())
        };
        let wake_for_source = source_added && state.immediate_pty_sources.contains(terminal_id);
        let became_pending = !self.pending.swap(true, Ordering::AcqRel);
        became_pending || wake_for_source
    }

    pub(crate) fn set_immediate_pty_sources(&self, sources: HashSet<TerminalId>) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .immediate_pty_sources = sources;
    }

    pub(crate) fn has_immediate_work(&self) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.request.generic
            || !state.request.terminal_title_sources.is_empty()
            || state
                .request
                .pty_sources
                .iter()
                .any(|terminal_id| state.immediate_pty_sources.contains(terminal_id))
    }

    /// Coalesces terminal-title changes separately from ordinary PTY damage so
    /// consumers can update metadata without inspecting every pane.
    pub(crate) fn request_terminal_title(&self, terminal_id: &TerminalId) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let source_added = if state.request.terminal_title_sources.contains(terminal_id) {
            false
        } else {
            state
                .request
                .terminal_title_sources
                .insert(terminal_id.clone())
        };
        let became_pending = !self.pending.swap(true, Ordering::AcqRel);
        became_pending || source_added
    }

    pub(crate) fn pending_terminal_title_sources(&self) -> HashSet<TerminalId> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .request
            .terminal_title_sources
            .clone()
    }

    pub(crate) fn take(&self) -> RenderRequest {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.pending.store(false, Ordering::Release);
        std::mem::take(&mut state.request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal_id() -> TerminalId {
        TerminalId::alloc()
    }

    #[test]
    fn coalesces_pty_sources_until_taken() {
        let signal = RenderSignal::new();
        let first = terminal_id();
        let second = terminal_id();

        assert!(signal.request_pty(&first));
        assert!(!signal.request_pty(&first));
        assert!(!signal.request_pty(&second));

        let request = signal.take();
        assert!(!request.generic);
        assert_eq!(request.pty_sources, HashSet::from([first, second]));
        assert!(request.terminal_title_sources.is_empty());
        assert!(!signal.is_pending());
    }

    #[test]
    fn hidden_pty_sources_coalesce_to_one_wake() {
        let signal = RenderSignal::new();
        let immediate = terminal_id();
        signal.set_immediate_pty_sources(HashSet::from([immediate]));

        let wakes = (0..50)
            .filter(|_| signal.request_pty(&terminal_id()))
            .count();

        assert_eq!(wakes, 1);
    }

    #[test]
    fn immediate_pty_source_wakes_pending_hidden_work() {
        let signal = RenderSignal::new();
        let hidden = terminal_id();
        let other_hidden = terminal_id();
        let visible = terminal_id();
        signal.set_immediate_pty_sources(HashSet::from([visible.clone()]));

        assert!(signal.request_pty(&hidden));
        assert!(!signal.request_pty(&other_hidden));
        assert!(signal.request_pty(&visible));
        assert!(!signal.request_pty(&visible));
    }

    #[test]
    fn terminal_title_source_wakes_pending_pty_work() {
        let signal = RenderSignal::new();
        let terminal_id = terminal_id();

        assert!(signal.request_pty(&terminal_id));
        assert!(signal.request_terminal_title(&terminal_id));
        assert!(!signal.request_terminal_title(&terminal_id));
    }

    #[test]
    fn coalesces_terminal_title_sources_without_making_them_pty_damage() {
        let signal = RenderSignal::new();
        let terminal_id = terminal_id();

        assert!(signal.request_terminal_title(&terminal_id));
        assert!(!signal.request_terminal_title(&terminal_id));
        assert_eq!(
            signal.pending_terminal_title_sources(),
            HashSet::from([terminal_id.clone()])
        );

        let request = signal.take();
        assert!(request.pty_sources.is_empty());
        assert_eq!(request.terminal_title_sources, HashSet::from([terminal_id]));
    }

    #[test]
    fn keeps_generic_and_pty_requests_distinct() {
        let signal = RenderSignal::new();
        let terminal_id = terminal_id();

        signal.request_generic();
        assert!(!signal.request_pty(&terminal_id));

        let request = signal.take();
        assert!(request.generic);
        assert_eq!(request.pty_sources, HashSet::from([terminal_id]));
    }
}
