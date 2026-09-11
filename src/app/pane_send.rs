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
            .filter(|entry| exclude != Some((entry.ws_idx, entry.pane_id)))
            .map(|entry| {
                let label = if entry.space_label.is_empty() || entry.space_label_redundant {
                    entry.primary_label.clone()
                } else {
                    format!("{} · {}", entry.primary_label, entry.space_label)
                };
                AgentPickerCandidate {
                    ws_idx: entry.ws_idx,
                    pane_id: entry.pane_id,
                    label,
                }
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
        self.state
            .note_pane_activity_at(pane_id, std::time::Instant::now());
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
            self.state.mode = crate::app::state::Mode::Terminal;
            return;
        };
        if key.code == KeyCode::Esc {
            self.state.agent_picker = None;
            self.state.mode = picker.return_mode;
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
        self.state.mode = picker.return_mode;
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
                    return_mode: crate::app::state::Mode::Terminal,
                });
                self.state.mode = crate::app::state::Mode::AgentPicker;
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
        app.state.mode = Mode::Terminal;
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

        app.send_text_to_chosen_agent(0, panes[0], "look at this".into());

        assert!(app.state.agent_picker.is_none());
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    /// Two or more targets earn the second step.
    #[test]
    fn several_targets_open_the_picker() {
        let (mut app, panes) = app_with_agents(3);

        app.send_text_to_chosen_agent(0, panes[0], "look at this".into());

        let picker = app.state.agent_picker.as_ref().expect("agent picker");
        assert_eq!(picker.candidates.len(), 2);
        assert_eq!(picker.text, "look at this");
        assert_eq!(app.state.mode, Mode::AgentPicker);
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
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    /// A digit past the last candidate is a miss, not a send to something else.
    #[test]
    fn a_digit_with_no_candidate_behind_it_leaves_the_picker_open() {
        let (mut app, panes) = app_with_agents(3);
        app.send_text_to_chosen_agent(0, panes[0], "look at this".into());

        app.handle_agent_picker_key(KeyEvent::new(KeyCode::Char('9'), KeyModifiers::empty()));

        assert!(app.state.agent_picker.is_some());
        assert_eq!(app.state.mode, Mode::AgentPicker);
    }

    #[test]
    fn picking_an_agent_closes_the_picker() {
        let (mut app, panes) = app_with_agents(3);
        app.send_text_to_chosen_agent(0, panes[0], "look at this".into());

        app.handle_agent_picker_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::empty()));

        assert!(app.state.agent_picker.is_none());
        assert_eq!(app.state.mode, Mode::Terminal);
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
}
