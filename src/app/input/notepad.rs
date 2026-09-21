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
    /// Show the focused pane's work context instead of a note.
    SelectContext,
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
        // The Context tab is read-only: navigation keys scroll it, everything
        // else plain is swallowed so it cannot edit the note it hides, and
        // modifier combinations keep reaching the global shortcuts.
        if self.notepad.context_active {
            match key.code {
                KeyCode::Esc => self.set_notepad_focus(false),
                KeyCode::Tab => self.request_notepad(NotepadRequest::Cycle { backwards: false }),
                KeyCode::BackTab => self.request_notepad(NotepadRequest::Cycle { backwards: true }),
                KeyCode::Up => self.dock_scroll = self.dock_scroll.saturating_sub(1),
                KeyCode::Down => self.dock_scroll = self.dock_scroll.saturating_add(1),
                KeyCode::PageUp => {
                    self.dock_scroll = self.dock_scroll.saturating_sub(self.notepad.body_rows())
                }
                KeyCode::PageDown => {
                    self.dock_scroll = self.dock_scroll.saturating_add(self.notepad.body_rows())
                }
                KeyCode::Char(_) if !ctrl && !alt => {}
                _ => {
                    let _ = shift;
                    return false;
                }
            }
            return true;
        }
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
        // A drag off the top edge leaves the panel's rows; the active
        // resize owns the gesture wherever the pointer goes.
        if matches!(mouse.kind, MouseEventKind::Drag(MouseButton::Left))
            && matches!(
                self.drag.as_ref().map(|drag| &drag.target),
                Some(crate::app::state::DragTarget::NotepadDivider)
            )
        {
            self.set_manual_notepad_height(mouse.row);
            return true;
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
                if self.notepad.context_active {
                    self.dock_scroll = self.dock_scroll.saturating_sub(1);
                } else {
                    self.notepad.scroll_by(-1);
                }
                true
            }
            MouseEventKind::ScrollDown => {
                if self.notepad.context_active {
                    self.dock_scroll = self.dock_scroll.saturating_add(1);
                } else {
                    self.notepad.scroll_by(1);
                }
                true
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(target) = self
                    .view
                    .notepad_tab_hit_areas
                    .iter()
                    .find(|(_, rect)| rect_contains(*rect, mouse.column, mouse.row))
                    .map(|(target, _)| *target)
                {
                    self.set_notepad_focus(true);
                    match target {
                        crate::notepad::NotepadTabTarget::Note(index) => {
                            if index != self.notepad.active || self.notepad.context_active {
                                self.request_notepad(NotepadRequest::Select(index));
                            }
                        }
                        crate::notepad::NotepadTabTarget::Context => {
                            if !self.notepad.context_active {
                                self.request_notepad(NotepadRequest::SelectContext);
                            }
                        }
                    }
                    return true;
                }
                // The header row outside the tabs is the panel's drag handle:
                // pressing it starts a vertical resize rather than an edit.
                if mouse.row == panel.y {
                    self.drag = Some(crate::app::state::DragState {
                        target: crate::app::state::DragTarget::NotepadDivider,
                    });
                    return true;
                }
                self.set_notepad_focus(true);
                if !self.notepad.context_active {
                    let body = notepad_body_rect(panel);
                    if rect_contains(body, mouse.column, mouse.row) {
                        self.place_notepad_caret(mouse.column, mouse.row, body);
                    }
                }
                true
            }
            _ => true,
        }
    }

    /// Drags the notepad's top edge to `row`: the panel is bottom-anchored, so
    /// the dragged row becomes its new top. The height persists per host.
    pub(crate) fn set_manual_notepad_height(&mut self, row: u16) {
        let panel = self.view.notepad_rect;
        if panel.height == 0 {
            return;
        }
        let height = panel
            .bottom()
            .saturating_sub(row)
            .clamp(crate::notepad::MIN_HEIGHT, crate::notepad::MAX_HEIGHT);
        if self.notepad.height != height {
            self.notepad.height = height;
            self.notepad_height_persistence_request = Some(height);
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
    pub(crate) fn intercept_pomodoro_send_off_raw_input_with_visibility(
        &mut self,
        source_id: crate::app::InputSourceId,
        event: &crate::raw_input::RawInputEvent,
        visible: bool,
    ) -> bool {
        if let crate::raw_input::RawInputEvent::Mouse(mouse) = event {
            if let Some(button) = self
                .pending_pomodoro_send_off_mouse_releases
                .get(&source_id)
                .copied()
            {
                if let crossterm::event::MouseEventKind::Up(released) = mouse.kind {
                    if released == button {
                        self.pending_pomodoro_send_off_mouse_releases
                            .remove(&source_id);
                        return true;
                    }
                    return false;
                }
                return true;
            }
        }
        if !visible {
            return false;
        }

        let dismisses = match event {
            crate::raw_input::RawInputEvent::Key(key) => {
                if key.kind == crossterm::event::KeyEventKind::Release {
                    return false;
                }
                true
            }
            crate::raw_input::RawInputEvent::Text(_)
            | crate::raw_input::RawInputEvent::Paste(_) => true,
            crate::raw_input::RawInputEvent::Mouse(mouse) => {
                if let crossterm::event::MouseEventKind::Down(button) = mouse.kind {
                    self.pending_pomodoro_send_off_mouse_releases
                        .insert(source_id, button);
                    true
                } else {
                    !matches!(mouse.kind, crossterm::event::MouseEventKind::Up(_))
                }
            }
            _ => false,
        };
        if !dismisses {
            return false;
        }
        self.state
            .pomodoro
            .dismiss_send_off_at(std::time::Instant::now());
        true
    }

    pub(crate) fn intercept_notepad_key_with_prompt_visibility(
        &mut self,
        key: &crate::input::TerminalKey,
        prompt_visible: bool,
    ) -> bool {
        let now = std::time::Instant::now();
        if prompt_visible {
            if self.state.pomodoro.prompt.is_some() {
                self.state
                    .handle_pomodoro_prompt_key(key.as_key_event(), now);
                self.apply_notepad_request();
            }
            return true;
        }
        if self.state.notepad.focused && self.state.handle_notepad_key(key.as_key_event(), now) {
            self.apply_notepad_request();
            return true;
        }
        false
    }

    /// Runs the filesystem half of a notepad interaction.
    pub(crate) fn apply_notepad_request(&mut self) -> bool {
        let Some(request) = self.state.notepad_request.take() else {
            return false;
        };
        match request {
            NotepadRequest::Select(index) => self.select_notepad_file(index),
            NotepadRequest::SelectContext => self.select_notepad_context(),
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
    fn clicking_the_context_tab_asks_for_it() {
        let mut state = state_with_notepad();
        state.view.notepad_rect = Rect::new(0, 10, 20, 5);
        state.view.notepad_tab_hit_areas = vec![(
            crate::notepad::NotepadTabTarget::Context,
            Rect::new(2, 10, 7, 1),
        )];
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 3,
            row: 10,
            modifiers: KeyModifiers::empty(),
        };
        assert!(state.handle_notepad_mouse(&event));
        assert_eq!(state.notepad_request, Some(NotepadRequest::SelectContext));
    }

    #[test]
    fn the_context_tab_is_read_only_but_still_cycles_and_scrolls() {
        let now = Instant::now();
        let mut state = state_with_notepad();
        state.notepad.context_active = true;
        state.notepad.set_body("keep me");
        // Plain keys never reach the note the tab hides.
        assert!(state.handle_notepad_key(key(KeyCode::Char('x')), now));
        assert_eq!(state.notepad.lines, vec!["keep me".to_string()]);
        // Scrolling drives the context view's scroll, not the note's.
        state.dock_scroll = 4;
        assert!(state.handle_notepad_key(key(KeyCode::Up), now));
        assert_eq!(state.dock_scroll, 3);
        // Global modifier shortcuts still fall through.
        assert!(!state.handle_notepad_key(
            KeyEvent::new(
                KeyCode::Char('m'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            ),
            now
        ));
        assert!(state.handle_notepad_key(key(KeyCode::Tab), now));
        assert_eq!(
            state.notepad_request,
            Some(NotepadRequest::Cycle { backwards: false })
        );
    }

    #[test]
    fn dragging_the_top_edge_resizes_the_panel_and_queues_persistence() {
        let mut state = state_with_notepad();
        state.notepad.focused = false;
        state.notepad.height = 6;
        state.view.notepad_rect = Rect::new(0, 14, 20, 6);
        let down = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 3,
            row: 14,
            modifiers: KeyModifiers::empty(),
        };
        assert!(state.handle_notepad_mouse(&down));
        assert!(matches!(
            state.drag.as_ref().map(|drag| &drag.target),
            Some(crate::app::state::DragTarget::NotepadDivider)
        ));
        // A grab on the handle does not hand the panel the keyboard.
        assert!(!state.notepad.focused);

        let drag = MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 3,
            row: 11,
            modifiers: KeyModifiers::empty(),
        };
        assert!(state.handle_notepad_mouse(&drag));
        assert_eq!(state.notepad.height, 9, "the panel is bottom-anchored");
        assert_eq!(state.notepad_height_persistence_request, Some(9));

        // The handle clamps to the same range the config key documents.
        state.view.notepad_rect = Rect::new(0, 14, 20, 26);
        state.set_manual_notepad_height(30);
        assert_eq!(state.notepad.height, 10);
        state.set_manual_notepad_height(2);
        assert_eq!(state.notepad.height, crate::notepad::MAX_HEIGHT);
        state.set_manual_notepad_height(45);
        assert_eq!(state.notepad.height, crate::notepad::MIN_HEIGHT);
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
