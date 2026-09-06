use super::App;

/// Write `content` to `path` without ever leaving a half-written config behind.
///
/// The config is read back on the next reload, so a torn write is not a lost
/// setting, it is a session that starts with a parse error. Writing a sibling
/// temp file and renaming it over the target makes the swap atomic on both
/// supported platforms.
fn write_config_atomically(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;

    let directory = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config.toml".to_string());
    let temp_path = directory.join(format!(".{file_name}.{}.tmp", std::process::id()));

    let write = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temp_path)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()
    })();
    if let Err(err) = write {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }

    // `rename` refuses to replace an existing file on Windows only when the
    // target is open elsewhere; the std implementation already uses
    // MoveFileEx with the replace flag, so one call covers both platforms.
    if let Err(err) = std::fs::rename(&temp_path, path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }
    Ok(())
}

impl App {
    /// The single config-edit path: read, rewrite only what `update` touches,
    /// write atomically. Every setting the UI saves goes through it, so no
    /// surface can invent its own partial rewrite of the user's file.
    pub(super) fn update_config_file<F>(&mut self, error_context: &str, update: F) -> bool
    where
        F: FnOnce(&str) -> String,
    {
        #[cfg(test)]
        if std::env::var_os(crate::config::CONFIG_PATH_ENV_VAR).is_none() {
            return false;
        }

        let path = crate::config::config_path();
        if let Some(parent) = path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                crate::logging::config_write_failed(&path, error_context, &err.to_string());
                self.state.config_diagnostic =
                    Some(format!("failed to save {error_context}: {err}"));
                self.config_diagnostic_deadline =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
                return false;
            }
        }

        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let new_content = update(&content);
        if new_content == content {
            return true;
        }
        if let Err(err) = write_config_atomically(&path, &new_content) {
            crate::logging::config_write_failed(&path, error_context, &err.to_string());
            self.state.config_diagnostic = Some(format!("failed to save {error_context}: {err}"));
            self.config_diagnostic_deadline =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
            return false;
        }

        true
    }

    /// Persist one settings-screen edit and reload, so the change is live in
    /// the same frame the operator made it.
    pub(super) fn save_config_edit(&mut self, edit: crate::app::settings_general::ConfigEdit) {
        use crate::app::settings_general::ConfigEdit;

        let saved = match edit {
            ConfigEdit::Bool {
                section,
                key,
                value,
            } => self.update_config_file(key, |content| {
                crate::config::upsert_section_bool(content, section, key, value)
            }),
            ConfigEdit::Integer {
                section,
                key,
                value,
            } => self.update_config_file(key, |content| {
                crate::config::upsert_section_value(content, section, key, &value.to_string())
            }),
            ConfigEdit::Text {
                section,
                key,
                value,
            } => self.update_config_file(key, |content| {
                crate::config::upsert_section_value(content, section, key, &format!("\"{value}\""))
            }),
        };
        if saved {
            self.apply_config_from_disk(false);
        }
    }

    /// Persist one captured keybinding and reload.
    ///
    /// Built-ins go through the same single config-edit path every other
    /// settings row uses; user actions go through 6b's atomic `[[actions]]`
    /// rewrite, because their key lives inside their own table.
    pub(super) fn save_keybinding(
        &mut self,
        target: &crate::app::settings_keybindings::KeybindTarget,
        key: &str,
    ) -> Result<(), String> {
        use crate::app::settings_keybindings::KeybindTarget;

        match target {
            KeybindTarget::BuiltIn { field } => {
                let field = *field;
                let value = format!("\"{key}\"");
                if !self.update_config_file(field, |content| {
                    crate::config::upsert_section_value(content, "keys", field, &value)
                }) {
                    return Err(format!("failed to save {field}"));
                }
            }
            KeybindTarget::UserAction { name } => {
                let path = crate::config::config_path();
                let mut config = match std::fs::read_to_string(&path) {
                    Ok(content) => toml::from_str::<crate::config::Config>(&content)
                        .map_err(|error| format!("Config parse failed: {error}"))?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        crate::config::Config::default()
                    }
                    Err(error) => return Err(format!("Config read failed: {error}")),
                };
                let Some(action) = config
                    .actions
                    .iter_mut()
                    .find(|action| action.name.trim().eq_ignore_ascii_case(name.trim()))
                else {
                    return Err(format!("action {name:?} is no longer in the config"));
                };
                action.key = Some(key.to_string());
                crate::config::write_actions_atomically(&path, &config.actions)
                    .map_err(|error| format!("Action save failed: {error}"))?;
            }
        }

        let report = self.apply_config_from_disk(false);
        if report.status == crate::config::ConfigReloadStatus::Failed {
            return Err(report
                .diagnostics
                .first()
                .cloned()
                .unwrap_or_else(|| "Config reload failed".into()));
        }
        Ok(())
    }

    pub(super) fn mark_onboarding_complete(&mut self) {
        self.update_config_file("onboarding setting", |content| {
            crate::config::upsert_top_level_bool(content, "onboarding", false)
        });
    }

    pub(super) fn save_theme(&mut self, name: &str) {
        if self.update_config_file("theme", |content| {
            let content = crate::config::upsert_section_value(
                content,
                "theme",
                "name",
                &format!("\"{name}\""),
            );
            crate::config::upsert_section_bool(&content, "theme", "auto_switch", false)
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_status_indicators(&mut self, style: crate::config::StatusIndicatorStyle) {
        if self.update_config_file("status indicators", |content| {
            crate::config::upsert_section_value(
                content,
                "ui",
                "status_indicators",
                &format!("\"{}\"", style.as_str()),
            )
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_sound(&mut self, enabled: bool) {
        if self.update_config_file("sound setting", |content| {
            crate::config::upsert_section_bool(content, "ui.sound", "enabled", enabled)
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_toast_delivery(&mut self, delivery: crate::config::ToastDelivery) {
        let value = match delivery {
            crate::config::ToastDelivery::Off => "\"off\"",
            crate::config::ToastDelivery::Herdr => "\"herdr\"",
            crate::config::ToastDelivery::Terminal => "\"terminal\"",
            crate::config::ToastDelivery::System => "\"system\"",
        };
        if self.update_config_file("toast setting", |content| {
            let content =
                crate::config::upsert_section_value(content, "ui.toast", "delivery", value);
            crate::config::remove_section_key(&content, "ui.toast", "enabled")
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_agent_border_labels(&mut self, enabled: bool) {
        if self.update_config_file("agent border labels", |content| {
            crate::config::upsert_section_bool(
                content,
                "ui",
                "show_agent_labels_on_pane_borders",
                enabled,
            )
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_agent_panel_sort(&mut self, sort: crate::app::state::AgentPanelSort) {
        let value = match sort {
            crate::app::state::AgentPanelSort::Spaces => {
                crate::config::AgentPanelSortConfig::Spaces.as_str()
            }
            crate::app::state::AgentPanelSort::Priority => {
                crate::config::AgentPanelSortConfig::Priority.as_str()
            }
        };
        if self.update_config_file("agent panel sort", |content| {
            crate::config::upsert_section_value(
                content,
                "ui",
                "agent_panel_sort",
                &format!("\"{value}\""),
            )
        }) {
            self.apply_config_from_disk(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::write_config_atomically;

    fn scratch_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-config-io-{}",
            crate::config::test_unique_suffix()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn atomic_config_write_replaces_the_file_and_leaves_no_temp_behind() {
        let dir = scratch_dir();
        let path = dir.join("config.toml");
        std::fs::write(&path, "[ui]\nconfirm_close = true\n").expect("seed");

        write_config_atomically(&path, "[ui]\nconfirm_close = false\n").expect("write");

        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "[ui]\nconfirm_close = false\n"
        );
        let leftovers = std::fs::read_dir(&dir)
            .expect("list")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn config_edit_rewrites_only_the_touched_key() {
        let original = "# a comment kept verbatim\n[ui]\nsidebar_width = 30\nconfirm_close = true\n\n[session]\nsettle_after_days = 7\n";
        let updated = crate::config::upsert_section_bool(original, "ui", "confirm_close", false);

        assert_eq!(
            updated,
            "# a comment kept verbatim\n[ui]\nsidebar_width = 30\nconfirm_close = false\n\n[session]\nsettle_after_days = 7\n"
        );
    }
}

#[cfg(test)]
mod keybinding_capture_tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::app::{
        settings_keybindings::{settings_keybinding_rows, KeybindTarget},
        state::SettingsSection,
        App,
    };

    struct Scratch {
        env: crate::config::TestConfigEnvGuard,
        directory: std::path::PathBuf,
        path: std::path::PathBuf,
    }

    impl Scratch {
        fn new(seed: &str) -> Self {
            let mut env = crate::config::TestConfigEnvGuard::acquire();
            let directory = std::env::temp_dir().join(format!(
                "herdr-keybind-capture-{}",
                crate::config::test_unique_suffix()
            ));
            std::fs::create_dir_all(&directory).expect("temp config directory");
            let path = directory.join("config.toml");
            std::fs::write(&path, seed).expect("seed config");
            env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);
            Self {
                env,
                directory,
                path,
            }
        }

        fn app(&self) -> App {
            App::new(
                &crate::config::Config::load().config,
                true,
                None,
                tokio::sync::mpsc::unbounded_channel().1,
                crate::api::EventHub::default(),
            )
        }

        fn read(&self) -> String {
            std::fs::read_to_string(&self.path).expect("config")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            self.env.remove(crate::config::CONFIG_PATH_ENV_VAR);
            std::fs::remove_dir_all(&self.directory).ok();
        }
    }

    fn select_row(app: &mut App, matches: impl Fn(&KeybindTarget) -> bool) -> usize {
        let row = settings_keybinding_rows(&app.state)
            .into_iter()
            .position(|row| row.target.as_ref().is_some_and(&matches))
            .expect("row exists");
        app.state.settings.list.select(row);
        row
    }

    #[test]
    fn keybinding_capture_writes_a_free_chord_and_refuses_a_bound_one() {
        let scratch = Scratch::new("# keep this comment\n[ui]\nsidebar_width = 31\n");
        let mut app = scratch.app();
        crate::app::input::open_settings_at(&mut app.state, SettingsSection::Keybindings);
        let row = select_row(&mut app, |target| {
            *target == KeybindTarget::BuiltIn { field: "usage" }
        });

        // Enter arms the row rather than saving anything.
        app.handle_settings_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(
            app.state
                .settings
                .keybind_capture
                .as_ref()
                .map(|capture| capture.row),
            Some(row)
        );

        // A chord that navigate mode reserves is refused on the row.
        app.handle_settings_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::empty()));
        assert!(app
            .state
            .settings
            .keybind_capture
            .as_ref()
            .and_then(|capture| capture.error.as_deref())
            .is_some());
        assert_eq!(
            scratch.read(),
            "# keep this comment\n[ui]\nsidebar_width = 31\n"
        );

        // A free chord is written under [keys] and reloaded.
        app.handle_settings_key(KeyEvent::new(KeyCode::F(9), KeyModifiers::empty()));
        assert_eq!(app.state.settings.keybind_capture, None);
        let saved = scratch.read();
        assert!(saved.starts_with("# keep this comment\n"), "{saved}");
        assert!(saved.contains("[keys]"), "{saved}");
        assert!(saved.contains("usage = \"f9\""), "{saved}");
        assert_eq!(app.state.keybinds.usage.label().as_deref(), Some("f9"));
    }

    #[test]
    fn keybinding_capture_writes_a_user_action_key_into_its_actions_table() {
        let scratch = Scratch::new("[[actions]]\nname = \"tests\"\ncommand = \"just test\"\n");
        let mut app = scratch.app();
        crate::app::input::open_settings_at(&mut app.state, SettingsSection::Keybindings);
        assert_eq!(app.state.keybinds.user_actions.len(), 1);
        select_row(
            &mut app,
            |target| matches!(target, KeybindTarget::UserAction { name } if name == "tests"),
        );

        app.handle_settings_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        app.handle_settings_key(KeyEvent::new(KeyCode::F(9), KeyModifiers::empty()));

        assert_eq!(app.state.settings.keybind_capture, None);
        let saved = scratch.read();
        assert!(saved.contains("key = \"f9\""), "{saved}");
        assert!(!saved.contains("[keys]"), "{saved}");
        assert_eq!(
            app.state.keybinds.user_actions[0]
                .bindings
                .label()
                .as_deref(),
            Some("f9")
        );
    }

    #[test]
    fn esc_cancels_a_capture_without_writing() {
        let scratch = Scratch::new("[ui]\nsidebar_width = 31\n");
        let mut app = scratch.app();
        crate::app::input::open_settings_at(&mut app.state, SettingsSection::Keybindings);
        select_row(&mut app, |target| {
            *target == KeybindTarget::BuiltIn { field: "usage" }
        });

        app.handle_settings_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        app.handle_settings_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));

        assert_eq!(app.state.settings.keybind_capture, None);
        assert_eq!(app.state.mode, crate::app::Mode::Settings);
        assert_eq!(scratch.read(), "[ui]\nsidebar_width = 31\n");
    }
}
