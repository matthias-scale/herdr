use std::collections::HashSet;

use super::App;

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct TerminalTitleChanges {
    pub(crate) raw_changed: bool,
    pub(crate) stripped_changed: bool,
}

const RESTORED_AGENT_TITLE_LABEL_LIMIT: usize = 44;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TerminalTitleSyncChange {
    pub(crate) raw_changed: bool,
    pub(crate) stripped_changed: bool,
    pub(crate) chrome_changed: bool,
}

fn label_tracks_agent_title(label: Option<&str>, previous_title: Option<&str>) -> bool {
    let (Some(label), Some(previous_title)) = (label, previous_title) else {
        return false;
    };
    if label.trim() == previous_title.trim() {
        return true;
    }
    let restored_prefix = previous_title
        .chars()
        .take(RESTORED_AGENT_TITLE_LABEL_LIMIT)
        .collect::<String>();
    previous_title.chars().count() > RESTORED_AGENT_TITLE_LABEL_LIMIT
        && label.trim() == restored_prefix.trim()
}

impl App {
    pub(crate) fn sync_pending_terminal_titles(&mut self) -> TerminalTitleSyncChange {
        let sources = self.render_dirty.pending_terminal_title_sources();
        let changes = self.sync_terminal_titles_with_sources(Some(&sources));
        let title_changes = TerminalTitleChanges {
            raw_changed: changes.raw_changed,
            stripped_changed: changes.stripped_changed,
        };
        if changes.chrome_changed || self.terminal_title_sidebar_changed(&title_changes) {
            self.render_dirty.request_generic();
            self.render_notify.notify_one();
        }
        changes
    }

    pub(crate) fn terminal_title_sidebar_configured(&self) -> bool {
        let config = &self.state.sidebar_agents;
        std::iter::once(&config.rows)
            .chain(config.rows_by_agent.values())
            .flatten()
            .flatten()
            .any(|token| {
                matches!(
                    token.parts().0,
                    crate::config::AgentSidebarToken::TerminalTitle
                        | crate::config::AgentSidebarToken::TerminalTitleStripped
                )
            })
    }

    pub(crate) fn terminal_title_sidebar_changed(&self, changes: &TerminalTitleChanges) -> bool {
        let config = &self.state.sidebar_agents;
        std::iter::once(&config.rows)
            .chain(config.rows_by_agent.values())
            .flatten()
            .flatten()
            .any(|token| match token.parts().0 {
                crate::config::AgentSidebarToken::TerminalTitle => changes.raw_changed,
                crate::config::AgentSidebarToken::TerminalTitleStripped => changes.stripped_changed,
                _ => false,
            })
    }

    pub(crate) fn sync_terminal_titles(&mut self) -> TerminalTitleSyncChange {
        self.sync_terminal_titles_with_sources(None)
    }

    fn sync_terminal_titles_with_sources(
        &mut self,
        sources: Option<&HashSet<crate::layout::PaneId>>,
    ) -> TerminalTitleSyncChange {
        let mut observations = Vec::new();
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for tab in &workspace.tabs {
                for (pane_id, pane) in &tab.panes {
                    if sources.is_some_and(|sources| !sources.contains(pane_id)) {
                        continue;
                    }
                    let terminal_id = &pane.attached_terminal_id;
                    let Some(runtime) = self.terminal_runtimes.get(terminal_id) else {
                        continue;
                    };
                    observations.push((
                        ws_idx,
                        *pane_id,
                        terminal_id.clone(),
                        runtime.terminal_title(),
                    ));
                }
            }
        }

        let mut sync_change = TerminalTitleSyncChange::default();
        let mut publish = Vec::new();
        let mut tab_updates = Vec::new();
        let mut copied_label_changed = false;
        for (ws_idx, pane_id, terminal_id, title) in observations {
            let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
                continue;
            };
            let previous_title = terminal.terminal_title_stripped();
            let agent_title_changed = terminal.detected_agent.is_some();
            let pane_label_tracks_title = agent_title_changed
                && label_tracks_agent_title(
                    terminal.manual_label.as_deref(),
                    previous_title.as_deref(),
                );
            let change = terminal.set_terminal_title(title);
            sync_change.raw_changed |= change.raw_changed;
            sync_change.stripped_changed |= change.stripped_changed;
            if change.stripped_changed {
                let current_title = terminal.terminal_title_stripped();
                if pane_label_tracks_title {
                    copied_label_changed = true;
                    match current_title.as_ref() {
                        Some(title) => terminal.set_manual_label(title.clone()),
                        None => terminal.clear_manual_label(),
                    }
                }
                if agent_title_changed {
                    sync_change.chrome_changed = true;
                    tab_updates.push((ws_idx, pane_id, previous_title, current_title));
                }
                publish.push((ws_idx, pane_id));
            }
        }

        for (ws_idx, pane_id, previous_title, current_title) in tab_updates {
            let Some(tab) = self.state.workspaces.get_mut(ws_idx).and_then(|workspace| {
                let tab_idx = workspace.find_tab_index_for_pane(pane_id)?;
                workspace.tabs.get_mut(tab_idx)
            }) else {
                continue;
            };
            if tab.layout.focused() != pane_id
                || !matches!(
                    tab.name_origin,
                    crate::workspace::TabNameOrigin::User
                        | crate::workspace::TabNameOrigin::AgentDerived
                )
                || !label_tracks_agent_title(tab.custom_name.as_deref(), previous_title.as_deref())
            {
                continue;
            }
            copied_label_changed = true;
            match current_title {
                Some(title) => {
                    tab.custom_name = Some(title);
                    tab.name_origin = crate::workspace::TabNameOrigin::AgentDerived;
                }
                None => {
                    tab.custom_name = None;
                    tab.name_origin = crate::workspace::TabNameOrigin::Structural;
                }
            }
        }

        if copied_label_changed {
            self.state.mark_session_dirty();
        }

        for (ws_idx, pane_id) in publish {
            self.emit_pane_updated(ws_idx, pane_id);
        }

        sync_change
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::detect::{Agent, AgentState};
    use crate::workspace::Workspace;

    #[tokio::test]
    async fn sync_keeps_latest_raw_title_and_emits_only_for_stripped_changes() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub.clone());
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(Agent::Claude);
        terminal.state = AgentState::Working;
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"");
        runtime.test_process_pty_bytes("\x1b]0;⠋ 修复🙂标题\x07".as_bytes());
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        let change = app.sync_terminal_titles();
        assert!(change.raw_changed);
        assert!(change.chrome_changed);
        let pane = app.pane_info(0, pane_id).unwrap();
        assert_eq!(pane.terminal_title.as_deref(), Some("⠋ 修复🙂标题"));
        assert_eq!(pane.terminal_title_stripped.as_deref(), Some("修复🙂标题"));
        assert_eq!(pane.title, None);
        assert_eq!(pane.agent_status, crate::api::schema::AgentStatus::Working);
        assert_eq!(pane.revision, 1);
        let agent = app.collect_agent_infos().pop().unwrap();
        assert_eq!(agent.terminal_title.as_deref(), Some("⠋ 修复🙂标题"));
        assert_eq!(agent.terminal_title_stripped.as_deref(), Some("修复🙂标题"));

        app.terminal_runtimes
            .get(&terminal_id)
            .unwrap()
            .test_process_pty_bytes("\x1b]2;⠙ 修复🙂标题\x1b\\".as_bytes());
        let change = app.sync_terminal_titles();
        assert!(change.raw_changed);
        assert!(!change.chrome_changed);
        let pane = app.pane_info(0, pane_id).unwrap();
        assert_eq!(pane.terminal_title.as_deref(), Some("⠙ 修复🙂标题"));
        assert_eq!(pane.terminal_title_stripped.as_deref(), Some("修复🙂标题"));
        assert_eq!(pane.revision, 1);
        assert_eq!(pane_updated_events(&event_hub), 1);

        app.terminal_runtimes
            .get(&terminal_id)
            .unwrap()
            .test_process_pty_bytes(b"\x1b]0;Done reviewing\x07");
        let change = app.sync_terminal_titles();
        assert!(change.raw_changed);
        assert!(change.chrome_changed);
        assert_eq!(pane_updated_events(&event_hub), 2);

        app.terminal_runtimes
            .get(&terminal_id)
            .unwrap()
            .test_process_pty_bytes(b"\x1b]0;\x07");
        let change = app.sync_terminal_titles();
        assert!(change.raw_changed);
        assert!(change.chrome_changed);
        let pane = app.pane_info(0, pane_id).unwrap();
        assert_eq!(pane.terminal_title, None);
        assert_eq!(pane.terminal_title_stripped, None);
        assert_eq!(pane.revision, 3);
        assert_eq!(pane_updated_events(&event_hub), 3);
    }

    #[tokio::test]
    async fn late_claude_and_codex_titles_replace_copied_restore_labels() {
        for (
            agent,
            initial_osc,
            updated_osc,
            final_osc,
            initial_title,
            updated_title,
            final_title,
        ) in [
            (
                Agent::Claude,
                "\x1b]0;✳ Initial Claude Summary Restored From A Much Longer Session Title\x07",
                "\x1b]0;✶ Updated Claude Summary\x07",
                "\x1b]0;✻ Final Claude Summary\x07",
                "Initial Claude Summary Restored From A Much Longer Session Title",
                "Updated Claude Summary",
                "Final Claude Summary",
            ),
            (
                Agent::Codex,
                "\x1b]2;◑ Initial Codex Summary\x1b\\",
                "\x1b]2;◒ Updated Codex Summary\x1b\\",
                "\x1b]2;◓ Final Codex Summary\x1b\\",
                "Initial Codex Summary",
                "Updated Codex Summary",
                "Final Codex Summary",
            ),
        ] {
            let event_hub = crate::api::EventHub::default();
            let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
            let mut app = App::new(&Config::default(), true, None, api_rx, event_hub);
            app.state.workspaces = vec![Workspace::test_new("one")];
            app.state.ensure_test_terminals();
            let pane_id = app.state.workspaces[0].tabs[0].root_pane;
            let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .detected_agent = Some(agent);
            let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"");
            runtime.test_process_pty_bytes(initial_osc.as_bytes());
            app.terminal_runtimes.insert(terminal_id.clone(), runtime);

            assert!(app.sync_terminal_titles().chrome_changed);
            assert_eq!(app.tab_info(0, 0).unwrap().label, initial_title);

            let copied_title = initial_title
                .chars()
                .take(RESTORED_AGENT_TITLE_LABEL_LIMIT)
                .collect::<String>();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .set_manual_label(copied_title.clone());
            app.state.workspaces[0].tabs[0].set_user_custom_name(copied_title);

            app.terminal_runtimes
                .get(&terminal_id)
                .unwrap()
                .test_process_pty_bytes(updated_osc.as_bytes());
            assert!(app.sync_terminal_titles().chrome_changed);

            assert_eq!(
                app.pane_info(0, pane_id).unwrap().label.as_deref(),
                Some(updated_title)
            );
            assert_eq!(app.tab_info(0, 0).unwrap().label, updated_title);

            app.terminal_runtimes
                .get(&terminal_id)
                .unwrap()
                .test_process_pty_bytes(final_osc.as_bytes());
            assert!(app.sync_terminal_titles().chrome_changed);
            assert_eq!(
                app.pane_info(0, pane_id).unwrap().label.as_deref(),
                Some(final_title)
            );
            assert_eq!(app.tab_info(0, 0).unwrap().label, final_title);
        }
    }

    #[tokio::test]
    async fn late_agent_title_preserves_distinct_human_labels() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub);
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(Agent::Claude);
        terminal.set_manual_label("Human pane label".into());
        app.state.workspaces[0].tabs[0].set_user_custom_name("Human tab label".into());
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"");
        runtime.test_process_pty_bytes("\x1b]0;✳ Updated Claude Summary\x07".as_bytes());
        app.terminal_runtimes.insert(terminal_id, runtime);

        assert!(app.sync_terminal_titles().chrome_changed);
        assert_eq!(
            app.pane_info(0, pane_id).unwrap().label.as_deref(),
            Some("Human pane label")
        );
        assert_eq!(app.tab_info(0, 0).unwrap().label, "Human tab label");
    }

    #[test]
    fn sidebar_redraws_only_for_the_configured_title_form() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub);
        app.state.sidebar_agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];
        app.state.sidebar_agents.rows_by_agent.insert(
            "claude".into(),
            vec![vec![
                crate::config::AgentSidebarToken::TerminalTitleStripped,
            ]],
        );

        let spinner_only = TerminalTitleChanges {
            raw_changed: true,
            ..TerminalTitleChanges::default()
        };
        assert!(!app.terminal_title_sidebar_changed(&spinner_only));
        assert!(app.terminal_title_sidebar_changed(&TerminalTitleChanges {
            stripped_changed: true,
            ..TerminalTitleChanges::default()
        }));

        app.state.sidebar_agents.rows_by_agent.insert(
            "claude".into(),
            vec![vec![crate::config::AgentSidebarToken::TerminalTitle]],
        );
        assert!(app.terminal_title_sidebar_changed(&spinner_only));
    }

    fn pane_updated_events(event_hub: &crate::api::EventHub) -> usize {
        event_hub
            .events_after(0)
            .iter()
            .filter(|(_, event)| event.event == crate::api::schema::EventKind::PaneUpdated)
            .count()
    }
}
