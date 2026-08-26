//! Inbox mode: one blocked agent at a time, oldest first.
//!
//! The overview sidebar answers "what is the fleet doing". This answers the
//! narrower question the operator actually returns to herdr with — "what is
//! waiting on me" — and it answers it one item at a time so that clearing the
//! queue is a loop rather than a scan.
//!
//! The queue is derived fresh on every read rather than cached. An agent that
//! answers its own gate, dies, or is closed leaves the queue without anything
//! having to invalidate it, and the count the operator sees is never stale.

use std::collections::HashSet;
use std::time::Instant;

use crate::detect::AgentState;
use crate::layout::PaneId;
use crate::terminal::TerminalId;

/// One blocked pane, resolved enough to render and to route keys to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlockedAgent {
    pub ws_idx: usize,
    pub pane_id: PaneId,
    pub terminal_id: TerminalId,
    /// Workspace name, for telling two repositories apart at a glance.
    pub workspace_label: String,
    /// The agent's own title, falling back to its CLI identity.
    pub agent_label: String,
    pub blocked_since: Option<Instant>,
    pub seq: Option<u64>,
}

/// Longest wait first.
///
/// An unobserved wait sorts behind every known one: it belongs to a pane that was
/// already blocked before the transition could be stamped, and promoting it to the
/// head of the queue would put the least trustworthy row where the operator looks
/// first. `pane_id` breaks remaining ties so a `HashMap` iteration order cannot
/// reshuffle the queue between frames.
fn by_longest_wait(a: &BlockedAgent, b: &BlockedAgent) -> std::cmp::Ordering {
    match (a.blocked_since, b.blocked_since) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
    .then_with(|| a.seq.cmp(&b.seq))
    .then_with(|| a.pane_id.cmp(&b.pane_id))
}

/// Cursor state for an open inbox. Holds only what cannot be re-derived: which
/// agents the operator chose to defer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct InboxState {
    deferred: HashSet<PaneId>,
}

impl InboxState {
    /// The agent to show. Deferred agents are skipped, but a queue whose every
    /// entry has been deferred wraps to the front rather than stranding the
    /// operator on an empty screen with work still outstanding.
    ///
    /// Pure, because the renderer holds `AppState` immutably; `defer` owns the
    /// only state change.
    pub(crate) fn current<'a>(&self, queue: &'a [BlockedAgent]) -> Option<&'a BlockedAgent> {
        queue
            .iter()
            .find(|agent| !self.deferred.contains(&agent.pane_id))
            .or_else(|| queue.first())
    }

    /// Defer the agent currently shown and move to the next one. Deferring the
    /// last undeferred entry resets the set, so the wrap `current` performs is
    /// reflected in the count the operator sees rather than contradicting it.
    pub(crate) fn defer(&mut self, pane_id: PaneId, queue: &[BlockedAgent]) {
        self.deferred.insert(pane_id);
        if queue
            .iter()
            .all(|agent| self.deferred.contains(&agent.pane_id))
        {
            self.deferred.clear();
        }
    }

    pub(crate) fn deferred_count(&self, queue: &[BlockedAgent]) -> usize {
        queue
            .iter()
            .filter(|agent| self.deferred.contains(&agent.pane_id))
            .count()
    }
}

impl crate::app::AppState {
    /// Every blocked pane across every workspace, oldest wait first.
    ///
    /// A pane with no `blocked_since` sorts after those that have one: it is a
    /// pane that was already blocked before the field existed, or whose
    /// transition was never observed, and guessing it is the oldest would put
    /// the least trustworthy row at the top of the queue.
    pub(crate) fn blocked_agents(&self) -> Vec<BlockedAgent> {
        let mut queue: Vec<BlockedAgent> = Vec::new();
        for (ws_idx, workspace) in self.workspaces.iter().enumerate() {
            for tab in &workspace.tabs {
                for (pane_id, pane) in &tab.panes {
                    let Some(terminal) = self.terminals.get(&pane.attached_terminal_id) else {
                        continue;
                    };
                    if terminal.state != AgentState::Blocked {
                        continue;
                    }
                    queue.push(BlockedAgent {
                        ws_idx,
                        pane_id: *pane_id,
                        terminal_id: pane.attached_terminal_id.clone(),
                        workspace_label: workspace.display_name_from_terminals(&self.terminals),
                        agent_label: terminal
                            .manual_label
                            .clone()
                            .or_else(|| terminal.effective_title())
                            .or_else(|| terminal.effective_agent_label().map(str::to_string))
                            .unwrap_or_else(|| "agent".to_string()),
                        blocked_since: terminal.blocked_since,
                        seq: terminal.last_agent_state_change_seq,
                    });
                }
            }
        }
        queue.sort_by(by_longest_wait);
        queue
    }

    pub(crate) fn toggle_inbox(&mut self) {
        self.inbox = match self.inbox.take() {
            Some(_) => None,
            None => Some(InboxState::default()),
        };
    }

    pub(crate) fn clear_inbox(&mut self) {
        self.inbox = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn agent(pane: PaneId, blocked_since: Option<Instant>, seq: Option<u64>) -> BlockedAgent {
        BlockedAgent {
            ws_idx: 0,
            pane_id: pane,
            terminal_id: TerminalId::alloc(),
            workspace_label: "ws".to_string(),
            agent_label: "agent".to_string(),
            blocked_since,
            seq,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn only_blocked_panes_reach_the_queue() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("inbox")];
        app.active = Some(0);
        app.ensure_test_terminals();
        let pane_id = app.workspaces[0].focused_pane_id().expect("focused pane");
        let terminal_id = app.workspaces[0]
            .terminal_id(pane_id)
            .expect("terminal")
            .clone();

        // Idle by default: the inbox is empty even though a pane exists.
        assert!(app.blocked_agents().is_empty());

        let stamped = Instant::now();
        {
            let terminal = app.terminals.get_mut(&terminal_id).expect("terminal state");
            terminal.state = AgentState::Blocked;
            terminal.blocked_since = Some(stamped);
        }

        let queue = app.blocked_agents();
        assert_eq!(queue.len(), 1, "queue: {queue:?}");
        assert_eq!(queue[0].pane_id, pane_id);
        assert_eq!(queue[0].blocked_since, Some(stamped));
    }

    #[test]
    fn opening_and_closing_the_inbox_is_one_toggle() {
        let mut app = crate::app::state::AppState::test_new();
        assert!(app.inbox.is_none());
        app.toggle_inbox();
        assert!(app.inbox.is_some());
        app.toggle_inbox();
        assert!(app.inbox.is_none());
    }

    #[test]
    fn the_queue_shows_its_first_entry_until_it_is_deferred() {
        let first = PaneId::alloc();
        let second = PaneId::alloc();
        let now = Instant::now();
        let queue = vec![
            agent(first, Some(now - Duration::from_secs(600)), Some(1)),
            agent(second, Some(now - Duration::from_secs(60)), Some(2)),
        ];
        let mut inbox = InboxState::default();

        assert_eq!(inbox.current(&queue).map(|a| a.pane_id), Some(first));
        inbox.defer(first, &queue);
        assert_eq!(inbox.current(&queue).map(|a| a.pane_id), Some(second));
    }

    #[test]
    fn deferring_everything_cycles_rather_than_showing_an_empty_queue() {
        let first = PaneId::alloc();
        let second = PaneId::alloc();
        let queue = vec![agent(first, None, Some(1)), agent(second, None, Some(2))];
        let mut inbox = InboxState::default();

        inbox.defer(first, &queue);
        inbox.defer(second, &queue);

        // Work is still outstanding, so the queue wraps instead of claiming empty.
        assert_eq!(inbox.current(&queue).map(|a| a.pane_id), Some(first));
        assert_eq!(inbox.deferred_count(&queue), 0);
    }

    #[test]
    fn an_empty_queue_forgets_what_was_deferred() {
        let stale = PaneId::alloc();
        let mut inbox = InboxState::default();
        inbox.defer(stale, &[]);

        assert_eq!(inbox.current(&[]), None);
        assert_eq!(inbox.deferred_count(&[]), 0);
    }

    #[test]
    fn an_unobserved_wait_sorts_behind_every_known_one() {
        let known = PaneId::alloc();
        let unknown = PaneId::alloc();
        let mut queue = [
            agent(unknown, None, Some(1)),
            agent(known, Some(Instant::now()), Some(2)),
        ];

        queue.sort_by(by_longest_wait);

        assert_eq!(queue[0].pane_id, known);
    }

    #[test]
    fn the_longest_wait_comes_first() {
        let now = Instant::now();
        let oldest = PaneId::alloc();
        let newest = PaneId::alloc();
        let mut queue = [
            agent(newest, Some(now - Duration::from_secs(30)), Some(2)),
            agent(oldest, Some(now - Duration::from_secs(3_600)), Some(1)),
        ];

        queue.sort_by(by_longest_wait);

        assert_eq!(queue[0].pane_id, oldest);
    }

    #[test]
    fn equal_waits_keep_a_stable_order_across_frames() {
        let a = agent(PaneId::alloc(), None, None);
        let b = agent(PaneId::alloc(), None, None);

        assert_eq!(by_longest_wait(&a, &b), a.pane_id.cmp(&b.pane_id));
        assert_ne!(by_longest_wait(&a, &b), std::cmp::Ordering::Equal);
    }
}
