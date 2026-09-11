use crate::detect::AgentState;
use crate::terminal::state::{attention_tier, AttentionTier};
use crate::terminal::{TerminalId, TerminalState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneAgentProjection {
    pub state: AgentState,
    pub seen: bool,
    pub stale: bool,
    pub attention_tier: AttentionTier,
    pub open_blockers: bool,
    pub gate_count: usize,
    pub usage_limited: bool,
}

impl PaneAgentProjection {
    pub(crate) fn counts_as_blocked(self) -> bool {
        self.attention_tier == AttentionTier::Blocked
            && (self.state != AgentState::Working || self.usage_limited)
    }

    pub(crate) fn needs_human_attention(self) -> bool {
        self.attention_tier == AttentionTier::Attention || self.counts_as_blocked()
    }

    pub(crate) fn status_key(self) -> &'static str {
        if self.attention_tier == AttentionTier::Attention {
            return "attention";
        }
        if self.counts_as_blocked() {
            return "blocked";
        }
        if self.stale {
            return "stale";
        }
        match (self.state, self.seen) {
            (AgentState::Blocked, _) => "blocked",
            (AgentState::Working, _) => "working",
            (AgentState::Idle, false) => "done",
            (AgentState::Idle, true) => "idle",
            (AgentState::Unknown, _) => "unknown",
        }
    }
}

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

    /// Public agent state after pane-level lifecycle policy is applied.
    /// Settling retires every outstanding demand without rewriting detector state.
    pub(crate) fn agent_projection(&self, terminal: &TerminalState) -> PaneAgentProjection {
        if self.settled_at.is_some() {
            return PaneAgentProjection {
                state: AgentState::Unknown,
                seen: true,
                stale: false,
                attention_tier: AttentionTier::None,
                open_blockers: false,
                gate_count: 0,
                usage_limited: false,
            };
        }
        let (state, seen) = terminal.sidebar_projection(self.seen);
        let open_blockers = !terminal.closing_gates.is_empty();
        PaneAgentProjection {
            state,
            seen,
            stale: terminal.supervisor_stale,
            attention_tier: attention_tier(
                state,
                open_blockers,
                !terminal.closing_items.is_empty(),
                terminal.usage_limited,
            ),
            open_blockers,
            gate_count: terminal.closing_gates.len(),
            usage_limited: terminal.usage_limited,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn projection(state: AgentState, attention_tier: AttentionTier) -> PaneAgentProjection {
        PaneAgentProjection {
            state,
            seen: true,
            stale: false,
            attention_tier,
            open_blockers: attention_tier == AttentionTier::Blocked,
            gate_count: usize::from(attention_tier == AttentionTier::Blocked),
            usage_limited: false,
        }
    }

    #[test]
    fn status_key_uses_attention_projection_instead_of_raw_lifecycle() {
        assert_eq!(
            projection(AgentState::Blocked, AttentionTier::Attention).status_key(),
            "attention"
        );
        assert_eq!(
            projection(AgentState::Idle, AttentionTier::Blocked).status_key(),
            "blocked"
        );
        assert_eq!(
            projection(AgentState::Working, AttentionTier::Blocked).status_key(),
            "working"
        );
    }
}
