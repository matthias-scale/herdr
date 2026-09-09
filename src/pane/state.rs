use crate::terminal::TerminalId;

/// Viewport state for a pane.
///
/// Terminal identity, cwd, labels, and agent metadata live in TerminalState.
pub struct PaneState {
    pub attached_terminal_id: TerminalId,
    /// Whether the user has seen this pane since its last state change to Idle.
    /// False = "Done" (agent finished while user was in another workspace).
    pub seen: bool,
    /// Whether unmodified right-click gestures should be forwarded to the pane application.
    pub right_click_passthrough: bool,
    /// When this pane's public lifecycle projection entered Done.
    pub done_since: Option<std::time::Instant>,
    /// Unix timestamp recorded when this pane left the active work set.
    pub settled_at: Option<u64>,
    /// Completed work trigger already consumed by this pane's latest resume.
    pub(crate) settled_work_key: Option<String>,
    /// When the linked work first read as finished for a trigger that has not
    /// settled yet. Runtime-only: a restart re-arms the grace window, which can
    /// only delay settling, never settle a pane early.
    pub(crate) finished_since: Option<std::time::Instant>,
    pub(crate) activity: Box<crate::activity_age::PaneActivity>,
}

impl PaneState {
    pub fn new(attached_terminal_id: TerminalId) -> Self {
        Self {
            attached_terminal_id,
            seen: true,
            right_click_passthrough: false,
            done_since: None,
            settled_at: None,
            settled_work_key: None,
            finished_since: None,
            activity: Box::new(crate::activity_age::PaneActivity::new(
                std::time::Instant::now(),
            )),
        }
    }
}
