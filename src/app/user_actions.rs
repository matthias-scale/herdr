use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{state::AddActionField, App, Mode};

impl App {
    pub(crate) fn handle_add_action_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.state.add_action = None;
            self.state.mode = Mode::Terminal;
            return;
        }
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            let backwards =
                key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
            if let Some(action) = self.state.add_action.as_mut() {
                let current = AddActionField::ALL
                    .iter()
                    .position(|field| *field == action.field)
                    .unwrap_or_default();
                let next = if backwards {
                    current
                        .checked_sub(1)
                        .unwrap_or(AddActionField::ALL.len() - 1)
                } else {
                    (current + 1) % AddActionField::ALL.len()
                };
                action.field = AddActionField::ALL[next];
                action.error = None;
            }
            return;
        }

        let Some(field) = self.state.add_action.as_ref().map(|action| action.field) else {
            self.state.mode = Mode::Terminal;
            return;
        };
        if key.code == KeyCode::Enter {
            match field {
                AddActionField::RunOnWorktreeCreate => {
                    if let Some(action) = self.state.add_action.as_mut() {
                        action.run_on_worktree_create = !action.run_on_worktree_create;
                    }
                }
                AddActionField::OpenInBottomPane => {
                    if let Some(action) = self.state.add_action.as_mut() {
                        action.open_in_bottom_pane = !action.open_in_bottom_pane;
                    }
                }
                _ => self.save_add_action(),
            }
            return;
        }

        match field {
            AddActionField::Name | AddActionField::Command => {
                let Some(action) = self.state.add_action.as_mut() else {
                    return;
                };
                let target = if field == AddActionField::Name {
                    &mut action.name
                } else {
                    &mut action.command
                };
                match key.code {
                    KeyCode::Backspace => {
                        target.pop();
                    }
                    KeyCode::Char(character)
                        if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                    {
                        target.push(character);
                    }
                    _ => {}
                }
                action.error = None;
            }
            AddActionField::Key => {
                if key.code == KeyCode::Backspace || key.code == KeyCode::Delete {
                    if let Some(action) = self.state.add_action.as_mut() {
                        action.key.clear();
                        action.error = None;
                    }
                    return;
                }
                if matches!(key.code, KeyCode::Modifier(_)) {
                    return;
                }
                let label = crate::config::format_key_combo((key.code, key.modifiers));
                let mut config = crate::config::Config::load().config;
                match crate::config::validate_user_action_key(&mut config, &label) {
                    Ok(label) => {
                        if let Some(action) = self.state.add_action.as_mut() {
                            action.key = label;
                            action.error = None;
                        }
                    }
                    Err(error) => {
                        if let Some(action) = self.state.add_action.as_mut() {
                            action.error = Some(error);
                        }
                    }
                }
            }
            AddActionField::RunOnWorktreeCreate | AddActionField::OpenInBottomPane => {
                if key.code == KeyCode::Char(' ') {
                    if let Some(action) = self.state.add_action.as_mut() {
                        if field == AddActionField::RunOnWorktreeCreate {
                            action.run_on_worktree_create = !action.run_on_worktree_create;
                        } else {
                            action.open_in_bottom_pane = !action.open_in_bottom_pane;
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn apply_save_add_action_request(&mut self) -> bool {
        if !std::mem::take(&mut self.state.request_save_add_action) {
            return false;
        }
        self.save_add_action();
        true
    }

    pub(crate) fn save_add_action(&mut self) {
        let Some(draft) = self.state.add_action.clone() else {
            return;
        };
        match self.persist_add_action(&draft) {
            Ok(()) => {
                self.state.add_action = None;
                self.state.mode = Mode::Terminal;
            }
            Err(error) => {
                if let Some(action) = self.state.add_action.as_mut() {
                    action.error = Some(error);
                }
            }
        }
    }

    fn persist_add_action(
        &mut self,
        draft: &crate::app::state::AddActionState,
    ) -> Result<(), String> {
        let name = draft.name.trim();
        if name.is_empty() {
            return Err("Name is required".into());
        }
        let command = draft.command.trim();
        if command.is_empty() {
            return Err("Command is required".into());
        }

        let path = crate::config::config_path();
        let mut config = match std::fs::read_to_string(&path) {
            Ok(content) => toml::from_str::<crate::config::Config>(&content)
                .map_err(|error| format!("Config parse failed: {error}"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                crate::config::Config::default()
            }
            Err(error) => return Err(format!("Config read failed: {error}")),
        };
        if config
            .actions
            .iter()
            .any(|action| action.name.trim().eq_ignore_ascii_case(name))
        {
            return Err(format!("Action {name:?} already exists"));
        }
        let key = (!draft.key.trim().is_empty()).then(|| draft.key.trim().to_string());
        if let Some(key) = key.as_deref() {
            crate::config::validate_user_action_key(&mut config, key)?;
        }
        config.actions.push(crate::config::ActionConfig {
            name: name.into(),
            command: command.into(),
            key,
            run_on_worktree_create: draft.run_on_worktree_create,
            open_in_bottom_pane: draft.open_in_bottom_pane,
            repo: self.state.focused_repo_slug(),
        });
        crate::config::write_actions_atomically(&path, &config.actions)
            .map_err(|error| format!("Action save failed: {error}"))?;
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        )
    }

    #[test]
    fn add_action_modal_round_trips_without_touching_other_config_bytes() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let directory = std::env::temp_dir().join(format!(
            "herdr-add-action-{}",
            crate::config::test_unique_suffix()
        ));
        let path = directory.join("config.toml");
        std::fs::create_dir_all(&directory).expect("temp config directory");
        let original = "# keep this comment\n[ui]\nsidebar_width = 31\n";
        std::fs::write(&path, original).expect("seed config");
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        app.state.mode = Mode::AddAction;
        app.state.add_action = Some(crate::app::state::AddActionState {
            name: "test".into(),
            key: "ctrl+t".into(),
            command: "just test".into(),
            run_on_worktree_create: true,
            open_in_bottom_pane: true,
            field: AddActionField::Command,
            error: None,
        });
        app.save_add_action();

        let saved = std::fs::read_to_string(&path).expect("saved config");
        assert!(saved.starts_with(original));
        let parsed: crate::config::Config = toml::from_str(&saved).expect("valid saved config");
        assert_eq!(parsed.actions.len(), 1);
        assert_eq!(parsed.actions[0].name, "test");
        assert_eq!(parsed.actions[0].command, "just test");
        assert!(parsed.actions[0].run_on_worktree_create);
        assert_eq!(app.state.mode, Mode::Terminal);
        assert_eq!(app.state.keybinds.user_actions.len(), 1);

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        std::fs::remove_dir_all(directory).expect("remove temp config");
    }

    #[test]
    fn add_action_modal_refuses_an_unparseable_config_without_writing() {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let directory = std::env::temp_dir().join(format!(
            "herdr-add-action-invalid-{}",
            crate::config::test_unique_suffix()
        ));
        let path = directory.join("config.toml");
        std::fs::create_dir_all(&directory).expect("temp config directory");
        let original = "[ui\ninvalid";
        std::fs::write(&path, original).expect("seed invalid config");
        env.set(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        app.state.mode = Mode::AddAction;
        app.state.add_action = Some(crate::app::state::AddActionState {
            name: "test".into(),
            command: "just test".into(),
            ..Default::default()
        });
        app.save_add_action();

        assert_eq!(
            std::fs::read_to_string(&path).expect("unchanged config"),
            original
        );
        assert!(app
            .state
            .add_action
            .as_ref()
            .and_then(|action| action.error.as_deref())
            .is_some_and(|error| error.contains("Config parse failed")));

        env.remove(crate::config::CONFIG_PATH_ENV_VAR);
        std::fs::remove_dir_all(directory).expect("remove temp config");
    }
}
