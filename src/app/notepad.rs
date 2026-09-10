//! App-side notepad plumbing: directory discovery, reads, debounced writes, the
//! filesystem watcher, and the optional git sync.
//!
//! The editing model itself is pure and lives in [`crate::notepad`]; nothing
//! here decides what the buffer looks like.

#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;

use crate::notepad::{self, NotepadState};
use crate::pomodoro::PomodoroConfirmation;

impl super::App {
    /// Points the notepad at its configured directory, loads the active note and
    /// registers the watcher. Cheap to call every frame: an unchanged directory
    /// returns immediately.
    pub(crate) fn ensure_notepad(&mut self) {
        if !self.state.notepad.enabled {
            if self.notepad_watcher.take().is_some() {
                self.notepad_watched_dir = None;
            }
            return;
        }
        let Some(dir) = self.state.notepad.dir.clone() else {
            return;
        };
        if self.notepad_watched_dir.as_ref() == Some(&dir) {
            return;
        }
        if let Err(error) = notepad::ensure_notes_dir(&dir, &self.notepad_preferred_files) {
            self.state.notepad.error = Some(error.to_string());
            // Retry on the next focus change rather than every frame.
            self.notepad_watched_dir = Some(dir);
            return;
        }
        self.rescan_notepad_files();
        self.load_active_note();
        self.notepad_watcher = if cfg!(test) {
            None
        } else {
            notepad::watch_notes(&dir, self.event_tx.clone())
        };
        self.notepad_watched_dir = Some(dir.clone());
        if self.notepad_git_sync {
            notepad::sync::pull(dir);
            self.notepad_next_git_pull = Some(Instant::now() + self.notepad_git_sync_interval);
        }
        self.request_notepad_repaint();
    }

    fn request_notepad_repaint(&mut self) {
        self.render_dirty.request_generic();
        self.render_notify.notify_one();
    }

    fn rescan_notepad_files(&mut self) {
        let Some(dir) = self.state.notepad.dir.clone() else {
            return;
        };
        let files = notepad::discover_notes(&dir, &self.notepad_preferred_files);
        self.state.notepad.set_files(files);
    }

    fn load_active_note(&mut self) {
        let Some(path) = self.state.notepad.active_path().map(PathBuf::from) else {
            self.state.notepad.set_body("");
            return;
        };
        match notepad::load_note(&path) {
            Ok(body) => {
                self.state.notepad.set_body(&body);
                self.state.notepad.error = None;
            }
            Err(error) => {
                self.state.notepad.error = Some(error);
            }
        }
    }

    /// Handles `AppEvent::NotepadChanged`. A dirty buffer is never overwritten
    /// from disk: the local edit is what the operator can see and is about to be
    /// written, so the note is only marked diverged.
    pub(crate) fn reload_notepad(&mut self) -> bool {
        if !self.state.notepad.enabled {
            return false;
        }
        let previous = self.state.notepad.active_path().map(PathBuf::from);
        self.rescan_notepad_files();
        let current = self.state.notepad.active_path().map(PathBuf::from);
        if self.state.notepad.dirty {
            self.state.notepad.diverged = true;
            return true;
        }
        if previous != current {
            self.load_active_note();
            return true;
        }
        let Some(path) = current else {
            return true;
        };
        let Ok(body) = notepad::load_note(&path) else {
            return true;
        };
        if body == self.state.notepad.body() {
            return false;
        }
        // Keeping the caret line across a remote edit is close enough: the
        // alternative is jumping the operator to the top on every sync.
        let (line, col) = (
            self.state.notepad.cursor_line,
            self.state.notepad.cursor_col,
        );
        self.state.notepad.set_body(&body);
        self.state.notepad.cursor_line = line.min(self.state.notepad.lines.len().saturating_sub(1));
        self.state.notepad.cursor_col = col;
        true
    }

    /// Switches the notepad to another note, flushing the current one first so
    /// nothing is lost between two files.
    pub(crate) fn select_notepad_file(&mut self, index: usize) -> bool {
        self.write_notepad_now();
        if !self.state.notepad.select(index) {
            return false;
        }
        self.load_active_note();
        true
    }

    pub(crate) fn cycle_notepad_file(&mut self, backwards: bool) -> bool {
        self.write_notepad_now();
        if !self.state.notepad.cycle(backwards) {
            return false;
        }
        self.load_active_note();
        true
    }

    /// Runs the debounced write and the periodic git pull. Called from the
    /// server's scheduled-task pass.
    pub(crate) fn tick_notepad(&mut self, now: Instant) -> bool {
        if !self.state.notepad.enabled {
            return false;
        }
        let mut changed = false;
        if self.state.notepad.take_due_save(now) {
            changed |= self.write_notepad_now();
        }
        if self.notepad_git_sync && self.notepad_next_git_pull.is_some_and(|due| now >= due) {
            self.notepad_next_git_pull = Some(now + self.notepad_git_sync_interval);
            if let Some(dir) = self.state.notepad.dir.clone() {
                notepad::sync::pull(dir);
            }
        }
        changed
    }

    /// Writes the buffer if it has unsaved edits. Returns whether anything was
    /// written, which is also what tells the caller the frame changed.
    pub(crate) fn write_notepad_now(&mut self) -> bool {
        if !self.state.notepad.enabled || !self.state.notepad.dirty {
            return false;
        }
        let Some(path) = self.state.notepad.active_path().map(PathBuf::from) else {
            return false;
        };
        let body = self.state.notepad.body();
        match notepad::save_note(&path, &body) {
            Ok(()) => {
                self.state.notepad.dirty = false;
                self.state.notepad.diverged = false;
                self.state.notepad.save_due = None;
                self.state.notepad.error = None;
                if self.notepad_git_sync {
                    if let Some(dir) = self.state.notepad.dir.clone() {
                        let name = path
                            .file_name()
                            .map(|name| name.to_string_lossy().to_string())
                            .unwrap_or_else(|| "notes".to_string());
                        notepad::sync::push(dir, format!("notes: update {name}"));
                    }
                }
                true
            }
            Err(error) => {
                self.state.notepad.error = Some(error);
                self.state.notepad.save_due = None;
                true
            }
        }
    }

    /// Advances the break timer. Returns whether the frame changed.
    pub(crate) fn tick_pomodoro(&mut self, now: Instant) -> bool {
        let tick = self.state.pomodoro.tick(now);
        if tick.phase_ended {
            // The overlay is the reminder; the sound is what reaches an operator
            // who is looking at another window.
            if self.state.local_sound_playback && self.state.sound.allows(None) {
                crate::sound::play(crate::sound::Sound::Request, &self.state.sound);
            }
            tracing::debug!("pomodoro phase ended; prompting for confirmation");
        }
        tick.changed
    }

    /// Confirms a due reminder and appends what the operator typed to the break
    /// log next to their notes.
    pub(crate) fn confirm_pomodoro(&mut self, now: Instant) -> bool {
        let Some(confirmation) = self.state.pomodoro.confirm(now) else {
            return true;
        };
        self.log_pomodoro_confirmation(&confirmation);
        true
    }

    fn log_pomodoro_confirmation(&mut self, confirmation: &PomodoroConfirmation) {
        let file = self.pomodoro_log_file.trim();
        if file.is_empty() || !self.state.notepad.enabled {
            return;
        }
        let Some(dir) = self.state.notepad.dir.clone() else {
            return;
        };
        let name = if file.ends_with(".md") {
            file.to_string()
        } else {
            format!("{file}.md")
        };
        if name.contains(['/', '\\']) {
            tracing::warn!(file = %name, "pomodoro log file must be a bare note name");
            return;
        }
        let stamp = crate::platform::local_datetime()
            .and_then(|now| {
                time::format_description::parse("[year]-[month]-[day] [hour]:[minute]")
                    .ok()
                    .and_then(|format| now.format(&format).ok())
            })
            .unwrap_or_default();
        let line = format!(
            "- {stamp} {} ended → {}",
            confirmation.ended.label(),
            confirmation.note
        );
        if let Err(error) = notepad::append_line(&dir.join(&name), &line) {
            tracing::warn!(error = %error, "failed to append pomodoro log line");
        }
    }

    /// Applies a live config reload to both surfaces.
    pub(crate) fn apply_notepad_config(&mut self, config: &crate::config::NotepadConfig) {
        self.notepad_preferred_files = config.files.clone();
        self.notepad_git_sync = config.git_sync;
        self.notepad_git_sync_interval =
            std::time::Duration::from_secs(config.git_sync_interval_seconds.clamp(15, 3600));

        let next = NotepadState::from_config(config);
        if next.dir != self.state.notepad.dir || next.enabled != self.state.notepad.enabled {
            self.write_notepad_now();
            let focused = self.state.notepad.focused && next.enabled;
            self.state.notepad = next;
            self.state.notepad.focused = focused;
            self.notepad_watcher = None;
            self.notepad_watched_dir = None;
        } else {
            self.state.notepad.height = next.height;
        }
    }

    pub(crate) fn apply_pomodoro_config(&mut self, config: &crate::config::PomodoroConfig) {
        self.pomodoro_log_file = config.log_file.clone();
        self.state.pomodoro.apply_config(config, Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-notepad-app-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn app_with_notes(dir: &Path) -> App {
        let config = crate::config::Config {
            notepad: crate::config::NotepadConfig {
                enabled: true,
                dir: dir.display().to_string(),
                files: vec!["todo".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        )
    }

    #[test]
    fn ensuring_the_notepad_creates_a_first_note() {
        let dir = temp_dir("first-note");
        let mut app = app_with_notes(&dir);
        app.ensure_notepad();
        assert_eq!(
            app.state
                .notepad
                .active_file()
                .map(|file| file.name.clone()),
            Some("todo".to_string())
        );
        assert!(dir.join("todo.md").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_due_write_reaches_the_file() {
        let dir = temp_dir("due-write");
        let mut app = app_with_notes(&dir);
        app.ensure_notepad();
        let now = Instant::now();
        app.state.notepad.insert_text("- buy milk", now);
        assert!(!app.tick_notepad(now));
        assert!(app.tick_notepad(now + crate::notepad::SAVE_DEBOUNCE));
        assert_eq!(
            std::fs::read_to_string(dir.join("todo.md")).expect("note"),
            "- buy milk\n"
        );
        assert!(!app.state.notepad.dirty);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_external_edit_reloads_a_clean_buffer() {
        let dir = temp_dir("external-clean");
        let mut app = app_with_notes(&dir);
        app.ensure_notepad();
        std::fs::write(dir.join("todo.md"), "from another device\n").expect("write");
        assert!(app.reload_notepad());
        assert_eq!(
            app.state.notepad.lines,
            vec!["from another device".to_string()]
        );
        assert!(!app.state.notepad.diverged);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_external_edit_never_discards_local_typing() {
        let dir = temp_dir("external-dirty");
        let mut app = app_with_notes(&dir);
        app.ensure_notepad();
        app.state.notepad.insert_text("local", Instant::now());
        std::fs::write(dir.join("todo.md"), "remote\n").expect("write");
        assert!(app.reload_notepad());
        assert_eq!(app.state.notepad.lines, vec!["local".to_string()]);
        assert!(app.state.notepad.diverged);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn switching_notes_flushes_the_previous_one() {
        let dir = temp_dir("switch");
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("todo.md"), "").expect("write");
        std::fs::write(dir.join("ideas.md"), "an idea\n").expect("write");
        let mut app = app_with_notes(&dir);
        app.ensure_notepad();
        app.state.notepad.insert_text("unsaved", Instant::now());
        assert!(app.cycle_notepad_file(false));
        assert_eq!(
            std::fs::read_to_string(dir.join("todo.md")).expect("note"),
            "unsaved\n"
        );
        assert_eq!(app.state.notepad.lines, vec!["an idea".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_confirmed_break_is_appended_to_the_log_note() {
        let dir = temp_dir("log");
        let config = crate::config::Config {
            notepad: crate::config::NotepadConfig {
                enabled: true,
                dir: dir.display().to_string(),
                ..Default::default()
            },
            pomodoro: crate::config::PomodoroConfig {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut app = App::new(
            &config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.ensure_notepad();
        let now = Instant::now();
        app.state
            .pomodoro
            .tick(now + std::time::Duration::from_secs(25 * 60));
        if let Some(prompt) = app.state.pomodoro.prompt.as_mut() {
            prompt.input = "walked around".into();
        }
        app.confirm_pomodoro(now);
        let log = std::fs::read_to_string(dir.join("pomodoro-log.md")).expect("log");
        assert!(log.contains("walked around"), "{log}");
        assert!(log.contains("focus ended"), "{log}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
