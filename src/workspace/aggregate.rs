use std::collections::HashMap;
use std::time::Instant;

use crate::detect::{Agent, AgentState};
use crate::layout::PaneId;
use crate::terminal::{TerminalId, TerminalState};

use super::{Tab, Workspace};

/// Detail info for a single pane, used by the agent detail panel.
pub struct PaneDetail {
    pub pane_id: PaneId,
    pub tab_idx: usize,
    pub pane_label: Option<String>,
    pub terminal_title: Option<String>,
    pub terminal_title_stripped: Option<String>,
    pub agent_label: String,
    pub agent_kind_label: Option<String>,
    pub agent: Option<Agent>,
    pub agent_context: Option<Agent>,
    pub has_agent: bool,
    pub state: AgentState,
    pub background_job_count: Option<u16>,
    pub seen: bool,
    pub last_agent_state_change_seq: Option<u64>,
    pub activity_at: Option<Instant>,
    pub state_labels: HashMap<String, String>,
    pub tokens: HashMap<String, String>,
}

impl Tab {
    fn pane_details(
        &self,
        terminals: &HashMap<TerminalId, TerminalState>,
        tab_idx: usize,
    ) -> Vec<PaneDetail> {
        self.layout
            .pane_ids()
            .iter()
            .enumerate()
            .filter_map(|(pane_idx, id)| {
                let pane = self.panes.get(id)?;
                let terminal = terminals.get(&pane.attached_terminal_id)?;
                let agent_kind_label = terminal.effective_agent_label().map(str::to_string);
                let fallback_agent_label = terminal
                    .agent_name
                    .as_deref()
                    .or(agent_kind_label.as_deref())
                    .map(str::to_string);
                let agent_label = terminal
                    .effective_display_agent()
                    .or(fallback_agent_label)
                    .unwrap_or_else(|| "Terminal".to_string());
                let presentation = terminal.effective_presentation();
                Some(PaneDetail {
                    pane_id: *id,
                    tab_idx,
                    // A pane is a child of the tab in every sidebar projection.
                    // Never derive its label from the effective tab title: that
                    // would render the persisted title twice. Keep this order
                    // deterministic for agentless panes as well.
                    pane_label: terminal
                        .manual_label
                        .clone()
                        .or_else(|| terminal.effective_display_agent())
                        .or_else(|| terminal.agent_name.clone())
                        .or(agent_kind_label.clone())
                        .or_else(|| terminal.terminal_title_stripped())
                        .or_else(|| Some(format!("Pane {}", pane_idx + 1))),
                    terminal_title: terminal.terminal_title.clone(),
                    terminal_title_stripped: terminal.terminal_title_stripped(),
                    agent_label,
                    agent_kind_label,
                    agent: terminal.effective_known_agent(),
                    agent_context: terminal.agent_lifecycle_context(),
                    has_agent: terminal.agent_lifecycle_context().is_some(),
                    state: terminal.state,
                    background_job_count: terminal.background_job_count,
                    seen: pane.seen,
                    last_agent_state_change_seq: terminal.last_agent_state_change_seq,
                    activity_at: terminal.agent_activity_at(),
                    state_labels: presentation.state_labels,
                    tokens: terminal.metadata_tokens.values(),
                })
            })
            .collect()
    }
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
        self.tabs
            .iter()
            .flat_map(|tab| tab.panes.values())
            .filter_map(|pane| {
                terminals
                    .get(&pane.attached_terminal_id)
                    .map(|terminal| (terminal.state, pane.seen))
            })
            .max_by_key(|(state, seen)| pane_attention_priority(*state, *seen))
            .unwrap_or((AgentState::Unknown, true))
    }

    pub fn pane_details(&self, terminals: &HashMap<TerminalId, TerminalState>) -> Vec<PaneDetail> {
        self.tabs
            .iter()
            .enumerate()
            .flat_map(|(tab_idx, tab)| tab.pane_details(terminals, tab_idx).into_iter())
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
        let root = ws.tabs[0].root_pane;
        let terminal = terminal_for_pane(&ws, root);
        terminals.insert(terminal.id.clone(), terminal);
        let (state, seen) = ws.aggregate_state(&terminals);
        assert_eq!(state, AgentState::Unknown);
        assert!(seen);
    }

    #[test]
    fn aggregate_state_priority() {
        let mut ws = Workspace::test_new("test");
        let id2 = ws.test_split(Direction::Horizontal);
        let root_id = ws.tabs[0]
            .panes
            .keys()
            .find(|id| **id != id2)
            .copied()
            .unwrap();
        let mut terminals = HashMap::new();
        let mut root_terminal = terminal_for_pane(&ws, root_id);
        root_terminal.state = AgentState::Idle;
        terminals.insert(root_terminal.id.clone(), root_terminal);
        let mut second_terminal = terminal_for_pane(&ws, id2);
        second_terminal.state = AgentState::Working;
        terminals.insert(second_terminal.id.clone(), second_terminal);

        let (state, seen) = ws.aggregate_state(&terminals);

        assert_eq!(state, AgentState::Working);
        assert!(seen);
    }

    #[test]
    fn aggregate_state_done_unseen_beats_working() {
        let mut ws = Workspace::test_new("test");
        let id2 = ws.test_split(Direction::Horizontal);
        let root_id = ws.tabs[0]
            .panes
            .keys()
            .find(|id| **id != id2)
            .copied()
            .unwrap();
        let mut terminals = HashMap::new();
        let mut root_terminal = terminal_for_pane(&ws, root_id);
        root_terminal.state = AgentState::Idle;
        terminals.insert(root_terminal.id.clone(), root_terminal);
        let mut second_terminal = terminal_for_pane(&ws, id2);
        second_terminal.state = AgentState::Working;
        terminals.insert(second_terminal.id.clone(), second_terminal);
        let root = ws.tabs[0].panes.get_mut(&root_id).unwrap();
        root.seen = false;

        let (state, seen) = ws.aggregate_state(&terminals);

        assert_eq!(state, AgentState::Idle);
        assert!(!seen);
    }

    #[test]
    fn pane_details_prefers_agent_name_over_detected_agent_label() {
        let ws = Workspace::test_new("test");
        let root_pane = ws.tabs[0].root_pane;
        let mut terminals = HashMap::new();
        let mut terminal = terminal_for_pane(&ws, root_pane);
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);
        terminal.set_agent_name("planner".into());
        terminals.insert(terminal.id.clone(), terminal);

        let labels: Vec<_> = ws
            .pane_details(&terminals)
            .into_iter()
            .map(|detail| (detail.agent_label, detail.agent))
            .collect();

        assert_eq!(labels, vec![("planner".into(), Some(Agent::Pi))]);
    }

    #[test]
    fn pane_details_keeps_pane_labels_independent_from_tab_titles() {
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].custom_name = Some("main".into());
        let root_pane = ws.tabs[0].root_pane;
        let second_tab = ws.test_add_tab(Some("review"));
        let review_pane = ws.tabs[second_tab].root_pane;
        let mut terminals = HashMap::new();
        let mut root_terminal = terminal_for_pane(&ws, root_pane);
        root_terminal.set_hook_authority(
            "test".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
        );
        terminals.insert(root_terminal.id.clone(), root_terminal);
        let mut review_terminal = terminal_for_pane(&ws, review_pane);
        review_terminal.set_hook_authority(
            "test".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
        );
        terminals.insert(review_terminal.id.clone(), review_terminal);

        let labels: Vec<_> = ws
            .pane_details(&terminals)
            .into_iter()
            .map(|detail| (detail.agent_label, detail.agent))
            .collect();

        assert_eq!(
            labels,
            vec![
                ("pi".into(), Some(Agent::Pi)),
                ("claude".into(), Some(Agent::Claude)),
            ]
        );
    }

    #[test]
    fn pane_details_use_tab_vector_index_not_stable_public_tab_number() {
        let mut ws = Workspace::test_new("test");
        let removed_tab = ws.test_add_tab(Some("removed"));
        let survivor_tab = ws.test_add_tab(Some("survivor"));
        let survivor_pane = ws.tabs[survivor_tab].root_pane;
        assert!(ws.close_tab(removed_tab));

        let mut terminals = HashMap::new();
        let mut terminal = terminal_for_pane(&ws, survivor_pane);
        terminal.detected_agent = Some(Agent::Codex);
        terminals.insert(terminal.id.clone(), terminal);

        let details = ws.pane_details(&terminals);
        let survivor = details
            .iter()
            .find(|detail| detail.pane_id == survivor_pane)
            .expect("surviving tab agent should be listed");

        assert_eq!(ws.tabs[1].number, 3);
        assert_eq!(survivor.tab_idx, 1);
    }

    #[test]
    fn pane_details_include_agentless_panes_in_layout_order_with_fallback_labels() {
        let mut ws = Workspace::test_new("test");
        let second = ws.test_split(Direction::Horizontal);
        let mut terminals = HashMap::new();
        for pane_id in ws.tabs[0].layout.pane_ids() {
            let terminal = terminal_for_pane(&ws, pane_id);
            terminals.insert(terminal.id.clone(), terminal);
        }

        let details = ws.pane_details(&terminals);
        assert_eq!(details.len(), 2);
        assert_eq!(details[0].pane_id, ws.tabs[0].layout.pane_ids()[0]);
        assert_eq!(details[1].pane_id, second);
        assert_eq!(details[0].pane_label.as_deref(), Some("Pane 1"));
        assert_eq!(details[1].pane_label.as_deref(), Some("Pane 2"));
    }
}
