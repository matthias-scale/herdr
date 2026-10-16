//! Pure state and key handling for the local focus capture composer.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaptureAck {
    pub(crate) text: String,
    pub(crate) deadline: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaptureState {
    pub(crate) text: String,
    pub(crate) restored_age: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) gate_reason: Option<String>,
    pub(crate) entering_reason: bool,
    pub(crate) queued_count: usize,
    pub(crate) spawned_count: usize,
    pub(crate) active_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureAction {
    Consumed,
    Close,
    Save(bool),
    Spawn,
    ConfirmSpawn,
}

impl CaptureState {
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> CaptureAction {
        if self.entering_reason {
            match key.code {
                KeyCode::Esc if key.modifiers.is_empty() => {
                    self.entering_reason = false;
                    return CaptureAction::Save(false);
                }
                KeyCode::Enter if key.modifiers.is_empty() => {
                    self.entering_reason = false;
                    return CaptureAction::ConfirmSpawn;
                }
                KeyCode::Backspace if key.modifiers.is_empty() => {
                    self.gate_reason.as_mut().map(String::pop);
                    return CaptureAction::Consumed;
                }
                KeyCode::Char(c)
                    if !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) && !c.is_control() =>
                {
                    self.gate_reason.get_or_insert_with(String::new).push(c);
                    return CaptureAction::Consumed;
                }
                _ => return CaptureAction::Consumed,
            }
        }
        if key.code == KeyCode::Char('s') && key.modifiers == KeyModifiers::CONTROL {
            return CaptureAction::Spawn;
        }
        match key.code {
            KeyCode::Esc if key.modifiers.is_empty() => CaptureAction::Close,
            KeyCode::Enter if key.modifiers == KeyModifiers::CONTROL => CaptureAction::Save(true),
            KeyCode::Enter
                if key.modifiers.contains(KeyModifiers::SHIFT)
                    || key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.text.push('\n');
                self.changed();
                CaptureAction::Consumed
            }
            KeyCode::Enter if key.modifiers.is_empty() => CaptureAction::Save(false),
            KeyCode::Backspace if key.modifiers.is_empty() => {
                self.text.pop();
                self.changed();
                CaptureAction::Consumed
            }
            KeyCode::Char(c)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) && !c.is_control() =>
            {
                self.text.push(c);
                self.changed();
                CaptureAction::Consumed
            }
            _ => CaptureAction::Consumed,
        }
    }

    fn changed(&mut self) {
        self.error = None;
        self.restored_age = None;
    }

    pub(crate) fn insert_text(&mut self, text: &str) {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        self.text
            .extend(normalized.chars().filter(|c| !c.is_control() || *c == '\n'));
        self.changed();
    }
}

impl crate::app::state::AppState {
    pub(crate) fn focus_active_agent_count(&self) -> usize {
        self.workspaces
            .iter()
            .flat_map(|workspace| workspace.tabs.iter())
            .flat_map(|tab| tab.panes.values())
            .filter(|pane| {
                self.terminals
                    .get(&pane.attached_terminal_id)
                    .is_some_and(|terminal| {
                        let projection = pane.agent_projection(terminal);
                        !projection.stale
                            && matches!(
                                projection.state,
                                crate::detect::AgentState::Working
                                    | crate::detect::AgentState::Blocked
                            )
                    })
            })
            .count()
    }

    pub(crate) fn open_capture(&mut self) {
        if self.capture.is_some() {
            return;
        }
        let draft = self
            .sidebar_presentation
            .load_capture_draft()
            .filter(|draft| !draft.text.trim().is_empty());
        let (text, restored_age) = draft.map_or((String::new(), None), |draft| {
            let age = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|now| now.as_secs().saturating_sub(draft.saved_at_unix))
                .unwrap_or_default();
            (draft.text, Some(crate::app::spawn_dock::format_age(age)))
        });
        let counts = crate::focus_capture::queue_counts(&self.focus_dir);
        self.capture = Some(CaptureState {
            text,
            restored_age,
            error: None,
            gate_reason: None,
            entering_reason: false,
            queued_count: counts.queued,
            spawned_count: counts.spawned,
            active_count: self.focus_active_agent_count(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }
    #[test]
    fn routes_edit_save_close_and_newline_keys() {
        let mut state = CaptureState {
            text: String::new(),
            restored_age: None,
            error: None,
            gate_reason: None,
            entering_reason: false,
            queued_count: 0,
            spawned_count: 0,
            active_count: 0,
        };
        assert_eq!(
            state.handle_key(key(KeyCode::Enter, KeyModifiers::NONE)),
            CaptureAction::Save(false)
        );
        assert_eq!(
            state.handle_key(key(KeyCode::Enter, KeyModifiers::CONTROL)),
            CaptureAction::Save(true)
        );
        assert_eq!(
            state.handle_key(key(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            CaptureAction::Spawn
        );
        for modifiers in [KeyModifiers::SHIFT, KeyModifiers::ALT] {
            assert_eq!(
                state.handle_key(key(KeyCode::Enter, modifiers)),
                CaptureAction::Consumed
            );
        }
        state.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE));
        state.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(state.text, "\n\n");
        state.insert_text("a\r\nb\rc\u{1b}");
        assert_eq!(state.text, "\n\na\nb\nc");
        assert_eq!(
            state.handle_key(key(KeyCode::Esc, KeyModifiers::NONE)),
            CaptureAction::Close
        );
    }

    #[test]
    fn gated_spawn_requires_reason_and_enter_confirms() {
        let mut state = CaptureState {
            text: "draft".into(),
            restored_age: None,
            error: None,
            gate_reason: Some(String::new()),
            entering_reason: true,
            queued_count: 0,
            spawned_count: 0,
            active_count: 3,
        };
        assert_eq!(
            state.handle_key(key(KeyCode::Enter, KeyModifiers::NONE)),
            CaptureAction::ConfirmSpawn
        );
        state.entering_reason = true;
        state.handle_key(key(KeyCode::Char('u'), KeyModifiers::NONE));
        assert_eq!(state.gate_reason.as_deref(), Some("u"));
        assert_eq!(
            state.handle_key(key(KeyCode::Enter, KeyModifiers::NONE)),
            CaptureAction::ConfirmSpawn
        );
    }
}
