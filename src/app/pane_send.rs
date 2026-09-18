//! Handing a pane's clicked text to an agent, new or running.
//!
//! The pane menu resolves the text; this module decides where it lands. A new
//! agent gets it as its opening prompt, a running one as a submitted turn.

use std::time::Duration;

use bytes::Bytes;

use crate::app::state::AgentPickerCandidate;
use crate::app::App;
use crate::app::AppState;
use crate::layout::PaneId;

/// The pause `agent.prompt` leaves between the text and its Enter, so an agent
/// that redraws its composer on input still sees a complete line.
const SUBMIT_DELAY: Duration = Duration::from_millis(300);

impl AppState {
    /// Every running agent except the one in `pane_id`, in sidebar order.
    pub(crate) fn agent_send_candidates(
        &self,
        exclude: Option<(usize, PaneId)>,
    ) -> Vec<AgentPickerCandidate> {
        crate::ui::sidebar::all_agent_panel_entries(self)
            .into_iter()
            .filter_map(|entry| {
                let target = entry.local_target()?;
                if exclude == Some((target.ws_idx, target.pane_id)) {
                    return None;
                }
                let label = if entry.space_label.is_empty() || entry.space_label_redundant {
                    entry.primary_label.clone()
                } else {
                    format!("{} · {}", entry.primary_label, entry.space_label)
                };
                Some(AgentPickerCandidate {
                    ws_idx: target.ws_idx,
                    pane_id: target.pane_id,
                    label,
                })
            })
            .collect()
    }

    pub(crate) fn has_agent_other_than(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        !self
            .agent_send_candidates(Some((ws_idx, pane_id)))
            .is_empty()
    }
}

impl App {
    /// Submit `text` as a turn in the agent running in that pane, the same way
    /// `agent.prompt` does over the API.
    pub(crate) fn send_text_to_agent_pane(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        text: &str,
    ) -> bool {
        self.cancel_pending_stall_nudge_for_pane(pane_id);
        let Some(runtime) =
            self.state
                .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
        else {
            return false;
        };
        let (text, enter) = crate::app::api_helpers::encode_api_submission_parts(runtime, text);
        if runtime.try_send_bytes(Bytes::from(text)).is_err() {
            return false;
        }
        runtime.send_bytes_after(Bytes::from(enter), SUBMIT_DELAY);
        // A submitted turn is explicit input: it releases a blocked closing gate
        // the same way typing into the pane or `agent.prompt` does.
        self.retire_blocked_hook_authority_for_pane(pane_id, std::time::Instant::now());
        true
    }

    /// Spawn an agent on `text`, in the same Space and directory as the pane
    /// the text came from, using the defaults the home composer would offer.
    pub(crate) fn send_text_to_new_agent(
        &mut self,
        ws_idx: usize,
        text: &str,
    ) -> Result<(), String> {
        let mut home = self.state.new_home_state();
        home.prompt = text.to_string();
        if let Some(workspace) = self.state.workspaces.get(ws_idx) {
            home.target = crate::app::home::HomeTarget::Existing(workspace.id.clone());
            home.directory = workspace.identity_cwd.clone();
            home.ref_directory = workspace.identity_cwd.clone();
        }
        let plan = home.dispatch_plan()?;
        self.dispatch_home_composer(plan)
            .map_err(|error| format!("could not start an agent: {error}"))
    }
}

impl App {
    /// Digit picks an agent, `esc` abandons the send. Mirrors the work-link
    /// picker, including its refusal to act on a target that has since gone.
    pub(crate) fn handle_agent_picker_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;

        let Some(picker) = self.state.agent_picker.clone() else {
            return;
        };
        if key.code == KeyCode::Esc {
            self.state.agent_picker = None;
            return;
        }
        let Some(index) = (match key.code {
            KeyCode::Char(digit @ '1'..='9') if key.modifiers.is_empty() => {
                Some(usize::from(digit as u8 - b'1'))
            }
            _ => None,
        }) else {
            return;
        };
        let Some(candidate) = picker.candidates.get(index).cloned() else {
            return;
        };
        self.state.agent_picker = None;
        let still_live = self
            .state
            .agent_send_candidates(None)
            .iter()
            .any(|live| live.ws_idx == candidate.ws_idx && live.pane_id == candidate.pane_id);
        if !still_live {
            self.show_work_link_notice("that agent is gone");
            return;
        }
        if self.send_text_to_agent_pane(candidate.ws_idx, candidate.pane_id, &picker.text) {
            self.show_work_link_notice(&format!("sent to {}", candidate.label));
        } else {
            self.show_work_link_notice("could not reach that agent");
        }
    }

    /// Step one of "Send to agent...": act straight away when there is only one
    /// agent to pick, and only open the picker when the choice is real.
    pub(crate) fn send_text_to_chosen_agent(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        text: String,
    ) {
        let candidates = self.state.agent_send_candidates(Some((ws_idx, pane_id)));
        match candidates.as_slice() {
            [] => self.show_work_link_notice("no other agent is running"),
            [only] => {
                let (target_ws, target_pane, label) =
                    (only.ws_idx, only.pane_id, only.label.clone());
                if self.send_text_to_agent_pane(target_ws, target_pane, &text) {
                    self.show_work_link_notice(&format!("sent to {label}"));
                } else {
                    self.show_work_link_notice("could not reach that agent");
                }
            }
            _ => {
                self.state.agent_picker = Some(crate::app::state::AgentPickerState {
                    candidates: candidates.into_iter().take(9).collect(),
                    text,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::Mode;
    use crate::config::Config;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn app_with_agents(count: usize) -> (App, Vec<PaneId>) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = crate::workspace::Workspace::test_new("test");
        for _ in 1..count {
            workspace.test_split(ratatui::layout::Direction::Horizontal);
        }
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.ensure_test_terminals();

        let panes = app.state.window_pane_ids(0, 0);
        for pane_id in panes.iter().copied() {
            let terminal_id = app.state.workspaces[0]
                .terminal_id(pane_id)
                .expect("pane terminal")
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .expect("terminal state")
                .detected_agent = Some(crate::detect::Agent::Claude);
        }
        (app, panes)
    }

    fn app_with_agent_workspaces(count: usize) -> (App, Vec<PaneId>) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = (0..count)
            .map(|_| crate::workspace::Workspace::test_new("test"))
            .collect();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        app.state.toast_config.delay_seconds = 1;
        app.state.ensure_test_terminals();

        let panes = app
            .state
            .workspaces
            .iter()
            .map(|workspace| workspace.tabs[0].root_pane)
            .collect::<Vec<_>>();
        for (ws_idx, pane_id) in panes.iter().copied().enumerate() {
            let terminal_id = app.state.workspaces[ws_idx]
                .terminal_id(pane_id)
                .expect("pane terminal")
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .expect("terminal state")
                .detected_agent = Some(crate::detect::Agent::Claude);
        }
        (app, panes)
    }

    fn prepare_blocked_background_agent(
        app: &mut App,
        ws_idx: usize,
        pane_id: PaneId,
    ) -> (
        crate::terminal::TerminalId,
        tokio::sync::mpsc::Receiver<Bytes>,
    ) {
        assert_ne!(app.state.active, Some(ws_idx));
        let terminal_id = app.state.workspaces[ws_idx]
            .terminal_id(pane_id)
            .cloned()
            .expect("pane terminal");
        let terminal = app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state");
        terminal.set_detected_state(
            Some(crate::detect::Agent::Claude),
            crate::detect::AgentState::Idle,
        );
        terminal.set_hook_authority(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            crate::detect::AgentState::Blocked,
            None,
            Some(1),
        );
        let (runtime, rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 1024, b"", 8,
            );
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        (terminal_id, rx)
    }

    fn assert_input_retirement_is_not_completion(
        app: &App,
        ws_idx: usize,
        pane_id: PaneId,
        terminal_id: &crate::terminal::TerminalId,
        event_start: u64,
    ) {
        assert_eq!(
            app.state.terminals[terminal_id].raw_agent_state(),
            crate::detect::AgentState::Idle
        );
        assert!(!app.state.pending_agent_notifications.contains_key(&pane_id));
        assert!(!matches!(
            app.state.toast.as_ref().map(|toast| toast.kind),
            Some(crate::app::state::ToastKind::Finished)
        ));
        assert_eq!(
            app.agent_info(ws_idx, pane_id)
                .expect("agent info")
                .agent_status,
            crate::api::schema::AgentStatus::Idle
        );
        let statuses = app
            .event_hub
            .events_after(event_start)
            .into_iter()
            .filter_map(|(_, event)| match event.data {
                crate::api::schema::EventData::PaneAgentStatusChanged { agent_status, .. } => {
                    Some(agent_status)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(statuses, vec![crate::api::schema::AgentStatus::Idle]);
    }

    /// The pane you right-clicked is not a place to send its own text.
    #[test]
    fn the_clicked_agent_is_not_one_of_its_own_targets() {
        let (app, panes) = app_with_agents(2);

        let candidates = app.state.agent_send_candidates(Some((0, panes[0])));

        assert!(!candidates
            .iter()
            .any(|candidate| candidate.pane_id == panes[0]));
        assert!(candidates
            .iter()
            .any(|candidate| candidate.pane_id == panes[1]));
        assert!(app.state.has_agent_other_than(0, panes[0]));
    }

    /// One target is not a choice, so the text goes straight there.
    #[test]
    fn a_single_target_is_sent_to_without_a_picker() {
        let (mut app, panes) = app_with_agents(2);
        app.state.set_server_mode(Mode::Settings);

        app.send_text_to_chosen_agent(0, panes[0], "look at this".into());

        assert!(app.state.agent_picker.is_none());
        assert_eq!(app.state.server_mode(), Mode::Settings);
    }

    /// Two or more targets earn the second step.
    #[test]
    fn several_targets_open_the_picker() {
        let (mut app, panes) = app_with_agents(3);
        app.state.set_server_mode(Mode::Settings);

        app.send_text_to_chosen_agent(0, panes[0], "look at this".into());

        let picker = app.state.agent_picker.as_ref().expect("agent picker");
        assert_eq!(picker.candidates.len(), 2);
        assert_eq!(picker.text, "look at this");
        assert_eq!(app.state.server_mode(), Mode::Settings);
    }

    /// The only agent is the one you clicked, so there is nowhere to send.
    #[test]
    fn a_lone_agent_has_nowhere_to_send() {
        let (mut app, panes) = app_with_agents(1);

        app.send_text_to_chosen_agent(0, panes[0], "look at this".into());

        assert!(app.state.agent_picker.is_none());
        assert!(!app.state.has_agent_other_than(0, panes[0]));
    }

    #[test]
    fn escape_abandons_the_picker_without_sending() {
        let (mut app, panes) = app_with_agents(3);
        app.send_text_to_chosen_agent(0, panes[0], "look at this".into());

        app.handle_agent_picker_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));

        assert!(app.state.agent_picker.is_none());
        assert_eq!(app.state.server_mode(), Mode::Terminal);
    }

    /// A digit past the last candidate is a miss, not a send to something else.
    #[test]
    fn a_digit_with_no_candidate_behind_it_leaves_the_picker_open() {
        let (mut app, panes) = app_with_agents(3);
        app.send_text_to_chosen_agent(0, panes[0], "look at this".into());

        app.handle_agent_picker_key(KeyEvent::new(KeyCode::Char('9'), KeyModifiers::empty()));

        assert!(app.state.agent_picker.is_some());
        assert_eq!(app.state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn picking_an_agent_closes_the_picker() {
        let (mut app, panes) = app_with_agents(3);
        app.state.set_server_mode(Mode::Settings);
        app.send_text_to_chosen_agent(0, panes[0], "look at this".into());

        app.handle_agent_picker_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::empty()));

        assert!(app.state.agent_picker.is_none());
        assert_eq!(app.state.server_mode(), Mode::Settings);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn send_to_new_agent_preserves_shared_mode() {
        let (mut app, _) = app_with_agents(1);
        app.state.set_server_mode(Mode::Settings);
        let tabs_before = app.state.workspaces[0].tabs.len();

        app.send_text_to_new_agent(0, "review this")
            .expect("new agent should be created");

        assert_eq!(app.state.workspaces[0].tabs.len(), tabs_before + 1);
        assert_eq!(app.state.server_mode(), Mode::Settings);
        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn single_target_send_does_not_complete_a_blocked_background_agent() {
        let (mut app, panes) = app_with_agent_workspaces(2);
        app.state.sound.enabled = true;
        let target_ws = 1;
        let target_pane = panes[target_ws];
        let (terminal_id, mut rx) =
            prepare_blocked_background_agent(&mut app, target_ws, target_pane);
        let event_start = app.event_hub.current_sequence();

        app.send_text_to_chosen_agent(0, panes[0], "continue".into());

        assert!(rx.try_recv().is_ok(), "send text was not written");
        assert_input_retirement_is_not_completion(
            &app,
            target_ws,
            target_pane,
            &terminal_id,
            event_start,
        );

        app.handle_internal_event(crate::events::AppEvent::StateChanged {
            pane_id: target_pane,
            agent: Some(crate::detect::Agent::Claude),
            state: crate::detect::AgentState::Working,
            visible_blocker: false,
            visible_working: true,
            usage_limited: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        app.handle_internal_event(crate::events::AppEvent::StateChanged {
            pane_id: target_pane,
            agent: Some(crate::detect::Agent::Claude),
            state: crate::detect::AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            usage_limited: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let deadline = app
            .state
            .next_pending_agent_notification_deadline()
            .expect("genuine completion notification");
        let deliveries = app.state.drain_due_agent_notifications(deadline);
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].kind, crate::app::state::ToastKind::Finished);
        assert_eq!(deliveries[0].sound, Some(crate::sound::Sound::Done));
        assert_eq!(
            app.agent_info(target_ws, target_pane)
                .expect("agent info")
                .agent_status,
            crate::api::schema::AgentStatus::Done
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn picker_selection_does_not_complete_a_blocked_background_agent() {
        let (mut app, panes) = app_with_agent_workspaces(3);
        app.send_text_to_chosen_agent(0, panes[0], "continue".into());
        let candidate = app
            .state
            .agent_picker
            .as_ref()
            .expect("agent picker")
            .candidates[0]
            .clone();
        let (terminal_id, mut rx) =
            prepare_blocked_background_agent(&mut app, candidate.ws_idx, candidate.pane_id);
        let event_start = app.event_hub.current_sequence();

        app.handle_agent_picker_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::empty()));

        assert!(rx.try_recv().is_ok(), "send text was not written");
        assert_input_retirement_is_not_completion(
            &app,
            candidate.ws_idx,
            candidate.pane_id,
            &terminal_id,
            event_start,
        );
    }

    /// AC6: pane-send abandons the pending stall-nudge Enter before writing its own turn.
    #[tokio::test]
    async fn pane_send_cancels_a_pending_stall_nudge_submission() {
        let now = std::time::Instant::now();
        let (mut app, panes) = app_with_agents(1);
        let pane_id = panes[0];
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("pane terminal");
        app.state.auto_nudge_stalled_agents = true;
        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("agent pane")
            .activity
            .set_last_at(now - app.state.nudge_after);
        let terminal = app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state");
        terminal.set_detected_state(
            Some(crate::detect::Agent::Claude),
            crate::detect::AgentState::Idle,
        );
        terminal.supervisor_stale = true;
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 1024, b"", 8,
            );
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);

        assert!(app.tick_auto_nudges(now));
        assert!(rx.try_recv().is_ok(), "stall nudge text was not written");
        assert!(app
            .pending_stall_nudge_submissions
            .contains_key(&terminal_id));

        assert!(app.send_text_to_agent_pane(0, pane_id, "human turn"));

        assert!(!app
            .pending_stall_nudge_submissions
            .contains_key(&terminal_id));
        assert!(!app.tick_auto_nudges(now + SUBMIT_DELAY));
    }

    #[tokio::test]
    async fn pane_send_releases_a_blocked_closing_gate() {
        let (mut app, panes) = app_with_agents(1);
        let pane_id = panes[0];
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("pane terminal");
        let terminal = app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state");
        terminal.set_detected_state(
            Some(crate::detect::Agent::Claude),
            crate::detect::AgentState::Idle,
        );
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            crate::detect::AgentState::Blocked,
            None,
            None,
            Some(1),
            std::time::Instant::now(),
        );
        assert_eq!(
            terminal.raw_agent_state(),
            crate::detect::AgentState::Blocked
        );
        let (runtime, _rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 1024, b"", 8,
            );
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);

        assert!(app.send_text_to_agent_pane(0, pane_id, "answer to the gate"));

        assert_ne!(
            app.state.terminals[&terminal_id].raw_agent_state(),
            crate::detect::AgentState::Blocked
        );
    }
}
