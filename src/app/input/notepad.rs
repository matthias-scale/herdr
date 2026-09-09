//! Keyboard and mouse routing for the sidebar notepad and the break-timer
//! overlay.
//!
//! Both surfaces intercept input before the rest of the app sees it, so the
//! rules for when they may do that live in one file:
//!
//! - The break prompt takes every key while it is up. That is the feature.
//!   `ctrl+alt+b` is its documented escape hatch and pauses the timer instead.
//! - The notepad only takes keys while it is focused, which only a click on the
//!   panel or the `toggle_notepad` action can do.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::app::state::AppState;
use crate::ui::notepad::notepad_body_rect;

/// Work the notepad's input handlers cannot do themselves because it touches
/// the filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotepadRequest {
    /// Load another note, flushing the current one first.
    Select(usize),
    Cycle {
        backwards: bool,
    },
    /// Write the buffer without waiting for the debounce.
    Save,
    /// Log and advance a confirmed break prompt.
    ConfirmPomodoro,
}

fn rect_contains(rect: ratatui::layout::Rect, column: u16, row: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && column >= rect.x
        && column < rect.right()
        && row >= rect.y
        && row < rect.bottom()
}

impl AppState {
    fn request_notepad(&mut self, request: NotepadRequest) {
        self.notepad_request = Some(request);
    }

    pub(crate) fn set_notepad_focus(&mut self, focused: bool) {
        if self.notepad.focused == focused {
            return;
        }
        self.notepad.focused = focused;
        if !focused {
            self.request_notepad(NotepadRequest::Save);
        }
    }

    /// Toggles notepad focus. Off when the panel is disabled or has no note.
    pub(crate) fn toggle_notepad_focus(&mut self) -> bool {
        if !self.notepad.enabled {
            return false;
        }
        self.set_notepad_focus(!self.notepad.focused);
        true
    }

    /// Consumes every key while a break prompt is up. Returns whether the key
    /// was handled, which is always true except for the escape hatch, which is
    /// handled here too but reported so the caller can stop as well.
    pub(crate) fn handle_pomodoro_prompt_key(
        &mut self,
        key: KeyEvent,
        now: std::time::Instant,
    ) -> bool {
        if self.pomodoro.prompt.is_none() {
            return false;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('b') | KeyCode::Char('B') if ctrl && alt => {
                self.pomodoro.dismiss_and_pause(now);
            }
            KeyCode::Char('c') | KeyCode::Char('u') if ctrl => {
                if let Some(prompt) = self.pomodoro.prompt.as_mut() {
                    prompt.input.clear();
                    prompt.error = None;
                }
            }
            KeyCode::Backspace => {
                if let Some(prompt) = self.pomodoro.prompt.as_mut() {
                    prompt.input.pop();
                    prompt.error = None;
                }
            }
            KeyCode::Enter => self.request_notepad(NotepadRequest::ConfirmPomodoro),
            KeyCode::Char(ch) if !ctrl && !alt => {
                if let Some(prompt) = self.pomodoro.prompt.as_mut() {
                    // A one-line answer; the field is one row wide.
                    if prompt.input.chars().count() < 200 {
                        prompt.input.push(ch);
                    }
                    prompt.error = None;
                }
            }
            _ => {}
        }
        true
    }

    /// Routes a key into the note buffer while the panel is focused. Unhandled
    /// modifier combinations fall through so global shortcuts keep working.
    pub(crate) fn handle_notepad_key(&mut self, key: KeyEvent, now: std::time::Instant) -> bool {
        if !self.notepad.enabled || !self.notepad.focused {
            return false;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Esc => self.set_notepad_focus(false),
            KeyCode::Char('s') if ctrl => self.request_notepad(NotepadRequest::Save),
            KeyCode::Tab => self.request_notepad(NotepadRequest::Cycle { backwards: false }),
            KeyCode::BackTab => self.request_notepad(NotepadRequest::Cycle { backwards: true }),
            KeyCode::Enter => self.notepad.insert_newline(now),
            KeyCode::Backspace => self.notepad.backspace(now),
            KeyCode::Delete => self.notepad.delete(now),
            KeyCode::Left => self.notepad.move_left(),
            KeyCode::Right => self.notepad.move_right(),
            KeyCode::Up => self.notepad.move_up(),
            KeyCode::Down => self.notepad.move_down(),
            KeyCode::Home => self.notepad.move_line_start(),
            KeyCode::End => self.notepad.move_line_end(),
            KeyCode::PageUp => self
                .notepad
                .scroll_by(-(i32::from(self.notepad.body_rows()) as isize)),
            KeyCode::PageDown => self
                .notepad
                .scroll_by(i32::from(self.notepad.body_rows()) as isize),
            KeyCode::Char(ch) if !ctrl && !alt => self.notepad.insert_char(ch, now),
            _ => {
                let _ = shift;
                return false;
            }
        }
        true
    }

    /// Handles a mouse event over the notepad panel. Returns whether the event
    /// was consumed; a click outside the panel only drops focus and is left for
    /// its real target.
    pub(crate) fn handle_notepad_mouse(&mut self, mouse: &MouseEvent) -> bool {
        if !self.notepad.enabled {
            return false;
        }
        let panel = self.view.notepad_rect;
        let inside = rect_contains(panel, mouse.column, mouse.row);
        if !inside {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                self.set_notepad_focus(false);
            }
            return false;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.notepad.scroll_by(-1);
                true
            }
            MouseEventKind::ScrollDown => {
                self.notepad.scroll_by(1);
                true
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.set_notepad_focus(true);
                if let Some(index) = self
                    .view
                    .notepad_tab_hit_areas
                    .iter()
                    .find(|(_, rect)| rect_contains(*rect, mouse.column, mouse.row))
                    .map(|(index, _)| *index)
                {
                    if index != self.notepad.active {
                        self.request_notepad(NotepadRequest::Select(index));
                    }
                    return true;
                }
                let body = notepad_body_rect(panel);
                if rect_contains(body, mouse.column, mouse.row) {
                    self.place_notepad_caret(mouse.column, mouse.row, body);
                }
                true
            }
            _ => true,
        }
    }

    fn place_notepad_caret(&mut self, column: u16, row: u16, body: ratatui::layout::Rect) {
        let line = self.notepad.scroll + usize::from(row.saturating_sub(body.y));
        let line = line.min(self.notepad.lines.len().saturating_sub(1));
        let target = usize::from(column.saturating_sub(body.x));
        let len = self
            .notepad
            .lines
            .get(line)
            .map(|text| text.chars().count())
            .unwrap_or(0);
        self.notepad.cursor_line = line;
        self.notepad.cursor_col = target.min(len);
    }

    /// Ends the current phase early without a reminder.
    pub(crate) fn skip_pomodoro_phase(&mut self, now: std::time::Instant) -> bool {
        if !self.pomodoro.enabled {
            return false;
        }
        self.pomodoro.skip(now);
        true
    }

    /// Toggles the break timer, or dismisses a prompt that is already up.
    pub(crate) fn toggle_pomodoro(&mut self, now: std::time::Instant) -> bool {
        if !self.pomodoro.enabled {
            return false;
        }
        if self.pomodoro.prompt.is_some() {
            self.pomodoro.dismiss_and_pause(now);
        } else {
            self.pomodoro.toggle_pause(now);
        }
        true
    }
}

impl crate::app::App {
    /// Runs the filesystem half of a notepad interaction.
    pub(crate) fn apply_notepad_request(&mut self) -> bool {
        let Some(request) = self.state.notepad_request.take() else {
            return false;
        };
        match request {
            NotepadRequest::Select(index) => self.select_notepad_file(index),
            NotepadRequest::Cycle { backwards } => self.cycle_notepad_file(backwards),
            NotepadRequest::Save => self.write_notepad_now(),
            NotepadRequest::ConfirmPomodoro => self.confirm_pomodoro(std::time::Instant::now()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use std::time::{Duration, Instant};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn state_with_notepad() -> AppState {
        let mut state = AppState::test_new();
        state.notepad.enabled = true;
        state.notepad.focused = true;
        state.notepad.set_files(vec![
            crate::notepad::NotepadFile {
                path: "/notes/todo.md".into(),
                name: "todo".into(),
            },
            crate::notepad::NotepadFile {
                path: "/notes/ideas.md".into(),
                name: "ideas".into(),
            },
        ]);
        state
    }

    #[test]
    fn typing_reaches_the_note_buffer_only_while_focused() {
        let now = Instant::now();
        let mut state = state_with_notepad();
        assert!(state.handle_notepad_key(key(KeyCode::Char('h')), now));
        assert_eq!(state.notepad.lines, vec!["h".to_string()]);
        state.notepad.focused = false;
        assert!(!state.handle_notepad_key(key(KeyCode::Char('x')), now));
        assert_eq!(state.notepad.lines, vec!["h".to_string()]);
    }

    #[test]
    fn escape_leaves_the_notepad_and_asks_for_a_write() {
        let now = Instant::now();
        let mut state = state_with_notepad();
        state.handle_notepad_key(key(KeyCode::Char('a')), now);
        assert!(state.handle_notepad_key(key(KeyCode::Esc), now));
        assert!(!state.notepad.focused);
        assert_eq!(state.notepad_request, Some(NotepadRequest::Save));
    }

    #[test]
    fn tab_asks_for_the_next_note() {
        let now = Instant::now();
        let mut state = state_with_notepad();
        assert!(state.handle_notepad_key(key(KeyCode::Tab), now));
        assert_eq!(
            state.notepad_request,
            Some(NotepadRequest::Cycle { backwards: false })
        );
    }

    #[test]
    fn an_unknown_control_combination_falls_through_to_global_shortcuts() {
        let now = Instant::now();
        let mut state = state_with_notepad();
        let key = KeyEvent::new(
            KeyCode::Char('e'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        assert!(!state.handle_notepad_key(key, now));
    }

    #[test]
    fn a_click_below_the_last_line_puts_the_caret_on_it() {
        let mut state = state_with_notepad();
        state.notepad.set_body("one\ntwo");
        state.view.notepad_rect = Rect::new(0, 10, 20, 5);
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: 14,
            modifiers: KeyModifiers::empty(),
        };
        assert!(state.handle_notepad_mouse(&event));
        assert_eq!(
            (state.notepad.cursor_line, state.notepad.cursor_col),
            (1, 2)
        );
    }

    #[test]
    fn a_click_outside_the_panel_only_drops_focus() {
        let mut state = state_with_notepad();
        state.view.notepad_rect = Rect::new(0, 10, 20, 5);
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 40,
            row: 3,
            modifiers: KeyModifiers::empty(),
        };
        assert!(!state.handle_notepad_mouse(&event));
        assert!(!state.notepad.focused);
        assert_eq!(state.notepad_request, Some(NotepadRequest::Save));
    }

    #[test]
    fn the_break_prompt_swallows_ordinary_keys_and_confirms_on_enter() {
        let now = Instant::now();
        let mut state = AppState::test_new();
        state.pomodoro = crate::pomodoro::PomodoroState::from_config(
            &crate::config::PomodoroConfig {
                enabled: true,
                ..Default::default()
            },
            now,
        );
        state.pomodoro.tick(now + Duration::from_secs(25 * 60));
        assert!(state.handle_pomodoro_prompt_key(key(KeyCode::Char('t')), now));
        assert!(state.handle_pomodoro_prompt_key(key(KeyCode::Char('e')), now));
        assert!(state.handle_pomodoro_prompt_key(key(KeyCode::Char('a')), now));
        assert_eq!(
            state.pomodoro.prompt.as_ref().map(|p| p.input.as_str()),
            Some("tea")
        );
        assert!(state.handle_pomodoro_prompt_key(key(KeyCode::Enter), now));
        assert_eq!(state.notepad_request, Some(NotepadRequest::ConfirmPomodoro));
    }

    #[test]
    fn the_break_prompt_can_always_be_escaped_with_the_timer_shortcut() {
        let now = Instant::now();
        let mut state = AppState::test_new();
        state.pomodoro = crate::pomodoro::PomodoroState::from_config(
            &crate::config::PomodoroConfig {
                enabled: true,
                ..Default::default()
            },
            now,
        );
        state.pomodoro.tick(now + Duration::from_secs(25 * 60));
        let escape = KeyEvent::new(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        assert!(state.handle_pomodoro_prompt_key(escape, now));
        assert!(state.pomodoro.prompt.is_none());
        assert!(state.pomodoro.paused());
    }
}
