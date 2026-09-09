use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::layout::PaneId;

use super::{state::PaneSettlementChange, App, AppState};

pub(crate) fn unix_seconds(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

fn state_is(value: Option<&str>, expected: &str) -> bool {
    value.is_some_and(|value| value.eq_ignore_ascii_case(expected))
}

fn work_context_settlement_key(
    context: &crate::work_context::PaneWorkContext,
    snapshot: Option<&crate::work_index::Snapshot>,
) -> Option<String> {
    let snapshot = snapshot?;
    let primary_pr = context.primary_pr();
    let pr_done = primary_pr.and_then(|primary| {
        snapshot.items.iter().find_map(|item| {
            let state = item.pr_state.as_deref()?;
            item.pr_url
                .as_deref()
                .is_some_and(|url| url.eq_ignore_ascii_case(primary))
                .then_some(())?;
            (state_is(Some(state), "merged") || state_is(Some(state), "closed")).then(|| {
                format!(
                    "pr:{}:{}",
                    primary.to_ascii_lowercase(),
                    state.to_ascii_lowercase()
                )
            })
        })
    });
    if pr_done.is_some() {
        return pr_done;
    }
    context.ticket_ids.iter().find_map(|identifier| {
        snapshot.items.iter().find_map(|item| {
            let item_state_done = item
                .ticket_ids
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(identifier))
                && state_is(item.ticket_state.as_deref(), "done");
            let detail_done = item.ticket_details.iter().any(|ticket| {
                ticket.identifier.eq_ignore_ascii_case(identifier)
                    && state_is(ticket.state.as_deref(), "done")
            });
            (item_state_done || detail_done)
                .then(|| format!("ticket:{}:done", identifier.to_ascii_lowercase()))
        })
    })
}

fn primary_pr_settled_label(
    terminal: &crate::terminal::TerminalState,
    snapshot: Option<&crate::work_index::Snapshot>,
) -> Option<String> {
    let primary_pr = terminal.effective_work_context().primary_pr()?;
    let item = snapshot?.items.iter().find(|item| {
        item.pr_url
            .as_deref()
            .is_some_and(|url| url.eq_ignore_ascii_case(primary_pr))
    })?;
    let number = item.pr_number?;
    let title = item.pr_title.as_deref()?.trim();
    (!title.is_empty()).then(|| format!("#{number} {title}"))
}

fn derived_pane_label(state: &AppState, ws_idx: usize, pane_id: PaneId) -> Option<String> {
    state
        .workspaces
        .get(ws_idx)?
        .pane_details(&state.terminals)
        .into_iter()
        .find(|detail| detail.pane_id == pane_id)?
        .pane_label
}

impl AppState {
    fn pane_state_mut(&mut self, pane_id: PaneId) -> Option<(usize, &mut crate::pane::PaneState)> {
        self.workspaces
            .iter_mut()
            .enumerate()
            .find_map(|(ws_idx, workspace)| {
                workspace
                    .tabs
                    .iter_mut()
                    .find_map(|tab| tab.panes.get_mut(&pane_id))
                    .map(|pane| (ws_idx, pane))
            })
    }

    pub(crate) fn pane_is_settled(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        self.workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.pane_state(pane_id))
            .is_some_and(|pane| pane.settled_at.is_some())
    }

    pub(crate) fn settle_pane_at(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        settled_at: u64,
    ) -> bool {
        let Some(pane) = self.workspaces.get_mut(ws_idx).and_then(|workspace| {
            workspace
                .tabs
                .iter_mut()
                .find_map(|tab| tab.panes.get_mut(&pane_id))
        }) else {
            return false;
        };
        if pane.settled_at.is_some() {
            return false;
        }
        pane.settled_at = Some(settled_at);
        let workspace_id = self.workspaces[ws_idx].id.clone();
        self.pending_pane_settlement_changes
            .push(PaneSettlementChange {
                workspace_id,
                pane_id,
                settled_at: Some(settled_at),
            });
        self.mark_session_dirty();
        self.mark_sidebar_projection_changed();
        true
    }

    pub(crate) fn note_pane_activity_at(&mut self, pane_id: PaneId, now: Instant) -> bool {
        let Some((ws_idx, pane)) = self.pane_state_mut(pane_id) else {
            return false;
        };
        pane.activity.note(now);
        let changed = pane.settled_at.take().is_some();
        if changed {
            let workspace_id = self.workspaces[ws_idx].id.clone();
            self.pending_pane_settlement_changes
                .push(PaneSettlementChange {
                    workspace_id,
                    pane_id,
                    settled_at: None,
                });
            self.mark_session_dirty();
            self.mark_sidebar_projection_changed();
        }
        changed
    }

    pub(crate) fn observe_pane_detection_snapshot_at(
        &mut self,
        pane_id: PaneId,
        revision: u64,
        agent: Option<crate::detect::Agent>,
        snapshot: &str,
        now: Instant,
    ) -> bool {
        let Some((ws_idx, pane)) = self.pane_state_mut(pane_id) else {
            return false;
        };
        let activity = pane
            .activity
            .observe_detection_snapshot(revision, agent, snapshot, now);
        let changed = activity && pane.settled_at.take().is_some();
        if changed {
            let workspace_id = self.workspaces[ws_idx].id.clone();
            self.pending_pane_settlement_changes
                .push(PaneSettlementChange {
                    workspace_id,
                    pane_id,
                    settled_at: None,
                });
            self.mark_session_dirty();
            self.mark_sidebar_projection_changed();
        }
        changed
    }

    pub(crate) fn refresh_settled_panes_at(
        &mut self,
        snapshot: Option<&crate::work_index::Snapshot>,
        now: Instant,
        now_unix: u64,
    ) -> usize {
        let mut candidates = Vec::new();
        let mut observed_work_keys = Vec::new();
        for (ws_idx, workspace) in self.workspaces.iter().enumerate() {
            for tab in &workspace.tabs {
                for (pane_id, pane) in &tab.panes {
                    if pane.settled_at.is_some() {
                        continue;
                    }
                    let Some(context) = self
                        .terminals
                        .get(&pane.attached_terminal_id)
                        .map(crate::terminal::TerminalState::effective_work_context)
                    else {
                        continue;
                    };
                    let work_key = work_context_settlement_key(context, snapshot);
                    let new_work_trigger = work_key
                        .as_ref()
                        .is_some_and(|key| pane.settled_work_key.as_ref() != Some(key));
                    observed_work_keys.push((ws_idx, *pane_id, work_key.clone()));
                    let inactive = self.auto_settle_inactive
                        && pane.activity.inactive_for(now) >= self.settle_after;
                    let new_work_trigger = self.auto_settle_finished && new_work_trigger;
                    if pane.settled_at.is_none() && (inactive || new_work_trigger) {
                        candidates.push((ws_idx, *pane_id, work_key));
                    }
                }
            }
        }

        for (ws_idx, pane_id, work_key) in &observed_work_keys {
            if let Some(pane) = self.workspaces[*ws_idx]
                .tabs
                .iter_mut()
                .find_map(|tab| tab.panes.get_mut(pane_id))
            {
                pane.settled_work_key.clone_from(work_key);
            }
        }
        for (ws_idx, pane_id, _) in &candidates {
            self.settle_pane_at(*ws_idx, *pane_id, now_unix);
        }
        candidates.len()
    }
}

impl App {
    pub(crate) fn refresh_pane_settlement_at(&mut self, now: Instant) -> bool {
        let snapshots = self
            .state
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.tabs.iter())
            .flat_map(|tab| tab.panes.iter())
            .filter_map(|(pane_id, pane)| {
                let terminal = self.state.terminals.get(&pane.attached_terminal_id)?;
                let runtime = self.terminal_runtimes.get(&pane.attached_terminal_id)?;
                let revision = runtime.content_revision();
                let agent = terminal.effective_known_agent();
                if !pane.activity.needs_detection_snapshot(revision, agent) {
                    return None;
                }
                let content = runtime.detection_text();
                let snapshot =
                    crate::detect::manifest::non_chrome_activity_content(agent, &content);
                Some((*pane_id, revision, agent, snapshot))
            })
            .collect::<Vec<_>>();
        let mut changed = false;
        for (pane_id, revision, agent, snapshot) in snapshots {
            changed |= self
                .state
                .observe_pane_detection_snapshot_at(pane_id, revision, agent, &snapshot, now);
        }
        changed |= self.state.refresh_settled_panes_at(
            self.work_index_snapshot.as_ref(),
            now,
            unix_seconds(SystemTime::now()),
        ) > 0;
        changed |= self.flush_pane_settlement_events();
        changed
    }

    pub(crate) fn flush_pane_settlement_events(&mut self) -> bool {
        let changes = std::mem::take(&mut self.state.pending_pane_settlement_changes);
        if changes.is_empty() {
            return false;
        }
        for change in changes {
            let Some(ws_idx) = self
                .state
                .workspaces
                .iter()
                .position(|workspace| workspace.id == change.workspace_id)
            else {
                continue;
            };
            let Some(pane_id) = self.public_pane_id(ws_idx, change.pane_id) else {
                continue;
            };
            let Some(terminal_id) = self
                .find_pane(change.pane_id)
                .map(|(_, pane)| pane.attached_terminal_id.clone())
            else {
                continue;
            };

            match change.settled_at {
                Some(_) if self.state.settle_stops_agent => {
                    let derived_label = derived_pane_label(&self.state, ws_idx, change.pane_id);
                    let (resume_plan, settled_label) =
                        self.state
                            .terminals
                            .get(&terminal_id)
                            .map(|terminal| {
                                let resume_plan = terminal
                                    .persisted_agent_session
                                    .as_ref()
                                    .and_then(|session| {
                                        crate::agent_resume::plan(
                                            &session.source,
                                            &session.agent,
                                            &session.session_ref,
                                        )
                                    });
                                let settled_label = terminal.manual_label.is_none().then(|| {
                                    primary_pr_settled_label(
                                        terminal,
                                        self.work_index_snapshot.as_ref(),
                                    )
                                    .or(derived_label)
                                });
                                (resume_plan, settled_label.flatten())
                            })
                            .unwrap_or_default();

                    if let Some(resume_plan) = resume_plan {
                        if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
                            terminal.pending_agent_resume_plan = Some(resume_plan);
                            if let Some(label) = settled_label {
                                terminal.set_manual_label(label.clone());
                                terminal.settled_auto_label = Some(label);
                            }
                        }
                        if let Some(runtime) = self.terminal_runtimes.get_mut(&terminal_id) {
                            runtime.suspend_processes();
                        }
                    } else {
                        tracing::debug!(
                            pane = change.pane_id.raw(),
                            terminal = %terminal_id,
                            "leaving settled pane running without a supported resume plan"
                        );
                    }
                }
                Some(_) => {}
                None => {
                    let settled_auto_label = self
                        .state
                        .terminals
                        .get_mut(&terminal_id)
                        .and_then(|terminal| terminal.settled_auto_label.take());
                    if let Some(settled_auto_label) = settled_auto_label {
                        if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
                            if terminal.manual_label.as_ref() == Some(&settled_auto_label) {
                                terminal.clear_manual_label();
                            }
                        }
                    }

                    let suspended_size = self
                        .terminal_runtimes
                        .get(&terminal_id)
                        .filter(|runtime| runtime.is_suspended())
                        .map(crate::terminal::TerminalRuntime::current_size);
                    if let Some((rows, cols)) = suspended_size {
                        drop(self.terminal_runtimes.remove(&terminal_id));
                        self.start_pending_agent_resume_for_terminal(
                            &terminal_id,
                            rows,
                            cols,
                            true,
                        );
                    }
                }
            }
            let workspace_id = self.public_workspace_id(ws_idx);
            let (event, data) = match change.settled_at {
                Some(settled_at) => (
                    crate::api::schema::EventKind::PaneSettled,
                    crate::api::schema::EventData::PaneSettled {
                        pane_id,
                        workspace_id,
                        settled_at,
                    },
                ),
                None => (
                    crate::api::schema::EventKind::PaneUnsettled,
                    crate::api::schema::EventData::PaneUnsettled {
                        pane_id,
                        workspace_id,
                    },
                ),
            };
            self.emit_event(crate::api::schema::EventEnvelope { event, data });
            self.emit_pane_updated(ws_idx, change.pane_id);
        }
        self.schedule_session_save();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        detect::AgentState,
        terminal::{TerminalId, TerminalRuntime, TerminalState},
        workspace::Workspace,
    };

    fn app_with_runtime(
        config: &crate::config::Config,
        context: crate::work_context::PaneWorkContext,
    ) -> (
        App,
        PaneId,
        TerminalId,
        tokio::sync::mpsc::Receiver<bytes::Bytes>,
    ) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(config, true, None, api_rx, crate::api::EventHub::default());
        let workspace = Workspace::test_new("settlement-runtime");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace
            .terminal_id(pane_id)
            .cloned()
            .expect("root pane terminal");
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal")
            .restore_work_context(context)
            .expect("valid work context");
        let (runtime, rx) = TerminalRuntime::test_with_channel_and_scrollback_bytes(
            80,
            24,
            1024,
            b"retained settlement output\r\n",
            4,
        );
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        (app, pane_id, terminal_id, rx)
    }

    fn set_persisted_session(app: &mut App, terminal_id: &TerminalId, source: &str) {
        app.state
            .terminals
            .get_mut(terminal_id)
            .expect("root terminal")
            .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: source.into(),
                agent: "codex".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("settled-session")
                    .expect("valid session id"),
            });
    }

    fn state_with_context(context: crate::work_context::PaneWorkContext) -> (AppState, PaneId) {
        let mut state = AppState::test_new();
        let workspace = Workspace::test_new("settled");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace
            .pane_state(pane_id)
            .expect("root pane")
            .attached_terminal_id
            .clone();
        let mut terminal = TerminalState::new(terminal_id.clone(), "/repo".into());
        terminal
            .restore_work_context(context)
            .expect("valid work context");
        state.terminals.insert(terminal_id, terminal);
        state.workspaces.push(workspace);
        (state, pane_id)
    }

    fn item() -> crate::work_index::WorkItem {
        crate::work_index::WorkItem {
            repo: "owner/repo".into(),
            pr_number: None,
            pr_url: None,
            pr_title: None,
            pr_state: None,
            draft: false,
            review_decision: None,
            created_at: None,
            updated_at: None,
            additions: 0,
            deletions: 0,
            author: None,
            assignees: Vec::new(),
            labels: Vec::new(),
            check_state: Default::default(),
            audience: Default::default(),
            cached_pr_detail: None,
            ticket_ids: Vec::new(),
            ticket_title: None,
            ticket_state: None,
            ticket_details: Vec::new(),
            branch: None,
            preview_urls: Vec::new(),
            panes: Vec::new(),
            source: Default::default(),
        }
    }

    fn snapshot(item: crate::work_index::WorkItem) -> crate::work_index::Snapshot {
        crate::work_index::Snapshot {
            items: vec![item],
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: SystemTime::now(),
        }
    }

    #[test]
    fn primary_pr_merged_or_closed_settles_and_input_unsettles() {
        for pr_state in ["merged", "closed"] {
            let url = "https://github.com/owner/repo/pull/7";
            let (mut state, pane_id) = state_with_context(crate::work_context::PaneWorkContext {
                pr_urls: vec![url.into()],
                ..Default::default()
            });
            let mut first_item = item();
            first_item.pr_url = Some(url.into());
            first_item.pr_state = Some(pr_state.into());

            assert_eq!(
                state.refresh_settled_panes_at(
                    Some(&snapshot(first_item)),
                    Instant::now(),
                    1_725_000_000
                ),
                1
            );
            assert!(state.pane_is_settled(0, pane_id));
            assert!(state.note_pane_activity_at(pane_id, Instant::now()));
            assert!(!state.pane_is_settled(0, pane_id));
            assert_eq!(
                state.refresh_settled_panes_at(
                    Some(&snapshot({
                        let mut item = item();
                        item.pr_url = Some(url.into());
                        item.pr_state = Some(pr_state.into());
                        item
                    })),
                    Instant::now(),
                    1_725_000_001
                ),
                0,
                "resuming completed work must consume the trigger"
            );
        }
    }

    #[test]
    fn linked_done_ticket_settles_and_agent_state_change_unsettles() {
        let (mut state, pane_id) = state_with_context(crate::work_context::PaneWorkContext {
            ticket_ids: vec!["SCA-42".into()],
            ..Default::default()
        });
        let mut item = item();
        item.ticket_ids = vec!["SCA-42".into()];
        item.ticket_state = Some("Done".into());
        assert_eq!(
            state.refresh_settled_panes_at(Some(&snapshot(item)), Instant::now(), 1_725_000_001),
            1
        );

        state
            .update_terminal_state(pane_id, |terminal| {
                Some(terminal.set_detected_state_with_screen_signals_at(
                    Some(crate::detect::Agent::Codex),
                    AgentState::Working,
                    false,
                    false,
                    false,
                    false,
                    false,
                    Instant::now(),
                ))
            })
            .expect("agent state transition");
        assert!(!state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn inactivity_settles_and_detection_snapshot_change_unsettles() {
        let (mut state, pane_id) = state_with_context(Default::default());
        let now = Instant::now();
        assert!(!state.observe_pane_detection_snapshot_at(pane_id, 1, None, "before", now));
        state.settle_after = Duration::from_secs(3 * 24 * 60 * 60);
        state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane")
            .activity
            .set_last_at(now - state.settle_after);
        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_002), 1);

        assert!(state.observe_pane_detection_snapshot_at(pane_id, 2, None, "after", now));
        assert!(!state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn redraw_only_screen_update_keeps_settled_pane_and_inactivity_clock() {
        let (mut state, pane_id) = state_with_context(Default::default());
        let now = Instant::now();
        state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane")
            .activity
            .set_last_at(now);
        let first_screen = "Done.\n\n────────────────────\n❯\n────────────────────\nstatus 10:41";
        let second_screen = "Done.\n\n────────────────────\n❯\n────────────────────\nstatus 10:42";
        assert_ne!(first_screen, second_screen);
        let first = crate::detect::manifest::non_chrome_activity_content(
            Some(crate::detect::Agent::Claude),
            first_screen,
        );
        let second = crate::detect::manifest::non_chrome_activity_content(
            Some(crate::detect::Agent::Claude),
            second_screen,
        );
        assert_eq!(first, "Done.");
        assert_eq!(first, second);
        assert!(!state.observe_pane_detection_snapshot_at(
            pane_id,
            1,
            Some(crate::detect::Agent::Claude),
            &first,
            now
        ));
        assert!(state.settle_pane_at(0, pane_id, 1_725_000_002));

        assert!(!state.observe_pane_detection_snapshot_at(
            pane_id,
            2,
            Some(crate::detect::Agent::Claude),
            &second,
            now + Duration::from_secs(60)
        ));
        assert!(state.pane_is_settled(0, pane_id));
        assert_eq!(
            state.workspaces[0].tabs[0].panes[&pane_id]
                .activity
                .inactive_for(now + Duration::from_secs(60)),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn detected_agent_classifier_change_rebaselines_without_activity() {
        let (mut state, pane_id) = state_with_context(Default::default());
        let now = Instant::now();
        assert!(!state.observe_pane_detection_snapshot_at(
            pane_id,
            1,
            None,
            "────────────────────\n❯\n────────────────────\nstatus 10:41",
            now,
        ));
        assert!(state.settle_pane_at(0, pane_id, 1_725_000_002));

        assert!(!state.observe_pane_detection_snapshot_at(
            pane_id,
            2,
            Some(crate::detect::Agent::Claude),
            "",
            now + Duration::from_secs(1),
        ));
        assert!(state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn delayed_claude_startup_chrome_keeps_settled_pane() {
        let (mut state, pane_id) = state_with_context(Default::default());
        let now = Instant::now();
        let prompt_only = "────────────────────\n❯\n────────────────────\nstatus 10:41";
        let with_startup = concat!(
            "           Claude Code v2.1.263\n",
            " ▐▛███▛█   Fable 5.1 with medium effort\n",
            "▝▜██████▀  Claude Max\n",
            "  ▝▝ ▝▝    ~/repo\n\n\n",
            "                                ◐ medium · /effort\n",
            "────────────────────\n❯\n────────────────────\nstatus 10:42",
        );
        let first = crate::detect::manifest::non_chrome_activity_content(
            Some(crate::detect::Agent::Claude),
            prompt_only,
        );
        let second = crate::detect::manifest::non_chrome_activity_content(
            Some(crate::detect::Agent::Claude),
            with_startup,
        );
        assert_eq!(first, "");
        assert_eq!(first, second);
        assert!(!state.observe_pane_detection_snapshot_at(
            pane_id,
            1,
            Some(crate::detect::Agent::Claude),
            &first,
            now,
        ));
        assert!(state.settle_pane_at(0, pane_id, 1_725_000_002));

        assert!(!state.observe_pane_detection_snapshot_at(
            pane_id,
            2,
            Some(crate::detect::Agent::Claude),
            &second,
            now + Duration::from_secs(60),
        ));
        assert!(state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn user_input_activity_clears_settled_pane() {
        let (mut state, pane_id) = state_with_context(Default::default());
        assert!(state.settle_pane_at(0, pane_id, 1_725_000_002));

        assert!(state.note_pane_activity_at(pane_id, Instant::now()));
        assert!(!state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn transition_to_working_or_blocked_clears_settled_pane() {
        for next_state in [AgentState::Working, AgentState::Blocked] {
            let (mut state, pane_id) = state_with_context(Default::default());
            assert!(state.settle_pane_at(0, pane_id, 1_725_000_002));

            state
                .update_terminal_state(pane_id, |terminal| {
                    Some(terminal.set_detected_state_with_screen_signals_at(
                        Some(crate::detect::Agent::Claude),
                        next_state,
                        next_state == AgentState::Blocked,
                        false,
                        next_state == AgentState::Working,
                        false,
                        false,
                        Instant::now(),
                    ))
                })
                .expect("active agent transition");

            assert!(!state.pane_is_settled(0, pane_id), "{next_state:?}");
        }
    }

    #[tokio::test]
    async fn settling_resumable_pane_suspends_and_survives_pane_died() {
        let (mut app, pane_id, terminal_id, _rx) =
            app_with_runtime(&crate::config::Config::default(), Default::default());
        set_persisted_session(&mut app, &terminal_id, "herdr:codex");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .respawn_shell_on_exit = true;

        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_010));
        assert!(app.flush_pane_settlement_events());

        assert!(app.state.terminals[&terminal_id]
            .pending_agent_resume_plan
            .is_some());
        let runtime = app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("settled runtime remains available for rendering");
        assert!(runtime.is_suspended());
        assert!(runtime
            .snapshot_history()
            .is_some_and(|history| history.contains("retained settlement output")));

        app.handle_internal_event(crate::events::AppEvent::PaneDied { pane_id });

        assert!(app.find_pane(pane_id).is_some());
        assert!(app
            .terminal_runtimes
            .get(&terminal_id)
            .is_some_and(TerminalRuntime::is_suspended));
        assert!(app.state.terminals[&terminal_id].respawn_shell_on_exit);
    }

    #[tokio::test]
    async fn settling_unresumable_pane_keeps_runtime_live() {
        let (mut app, pane_id, terminal_id, _rx) =
            app_with_runtime(&crate::config::Config::default(), Default::default());
        set_persisted_session(&mut app, &terminal_id, "custom:codex");

        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_011));
        assert!(app.flush_pane_settlement_events());

        assert!(app.state.terminals[&terminal_id]
            .pending_agent_resume_plan
            .is_none());
        assert!(app
            .terminal_runtimes
            .get(&terminal_id)
            .is_some_and(|runtime| !runtime.is_suspended()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unsettling_suspended_pane_starts_resume_at_saved_size() {
        let (mut app, pane_id, terminal_id, _rx) =
            app_with_runtime(&crate::config::Config::default(), Default::default());
        set_persisted_session(&mut app, &terminal_id, "herdr:codex");
        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_012));
        assert!(app.flush_pane_settlement_events());
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
            dedupe_key: "settled-resume-test".into(),
        });

        assert!(app.state.note_pane_activity_at(pane_id, Instant::now()));
        assert!(app.flush_pane_settlement_events());

        let runtime = app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("unsettling should install a replacement runtime");
        assert!(!runtime.is_suspended());
        assert_eq!(runtime.current_size(), (24, 80));
        assert!(app.state.terminals[&terminal_id]
            .pending_agent_resume_plan
            .is_none());
        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_unsettle_resume_keeps_pending_plan() {
        let (mut app, pane_id, terminal_id, _rx) =
            app_with_runtime(&crate::config::Config::default(), Default::default());
        set_persisted_session(&mut app, &terminal_id, "herdr:codex");
        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_013));
        assert!(app.flush_pane_settlement_events());
        app.state.default_shell = "/__herdr_missing_settled_shell__".into();
        app.state.shell_mode = crate::config::ShellModeConfig::Login;

        assert!(app.state.note_pane_activity_at(pane_id, Instant::now()));
        assert!(app.flush_pane_settlement_events());

        assert!(app.terminal_runtimes.get(&terminal_id).is_none());
        assert!(app.state.terminals[&terminal_id]
            .pending_agent_resume_plan
            .is_some());
    }

    #[tokio::test]
    async fn settlement_label_prefers_pr_and_clears_only_unchanged_label() {
        let pr_url = "https://github.com/owner/repo/pull/42";
        let (mut app, pane_id, terminal_id, _rx) = app_with_runtime(
            &crate::config::Config::default(),
            crate::work_context::PaneWorkContext {
                pr_urls: vec![pr_url.into()],
                work_title: Some("fallback title".into()),
                ..Default::default()
            },
        );
        set_persisted_session(&mut app, &terminal_id, "herdr:codex");
        let mut pr = item();
        pr.pr_url = Some(pr_url.into());
        pr.pr_number = Some(42);
        pr.pr_title = Some("Fix login".into());
        app.work_index_snapshot = Some(snapshot(pr));

        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_014));
        assert!(app.flush_pane_settlement_events());
        assert_eq!(
            app.state.terminals[&terminal_id].manual_label.as_deref(),
            Some("#42 Fix login")
        );
        assert_eq!(
            app.state.terminals[&terminal_id]
                .settled_auto_label
                .as_deref(),
            Some("#42 Fix login")
        );

        drop(app.terminal_runtimes.remove(&terminal_id));
        assert!(app.state.note_pane_activity_at(pane_id, Instant::now()));
        assert!(app.flush_pane_settlement_events());
        assert!(app.state.terminals[&terminal_id].manual_label.is_none());

        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_015));
        assert!(app.flush_pane_settlement_events());
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_manual_label("Human label".into());
        assert!(app.state.note_pane_activity_at(pane_id, Instant::now()));
        assert!(app.flush_pane_settlement_events());
        assert_eq!(
            app.state.terminals[&terminal_id].manual_label.as_deref(),
            Some("Human label")
        );
        assert!(app.state.terminals[&terminal_id]
            .settled_auto_label
            .is_none());
    }

    #[tokio::test]
    async fn settlement_label_freezes_current_derived_title_without_pr() {
        let (mut app, pane_id, terminal_id, _rx) = app_with_runtime(
            &crate::config::Config::default(),
            crate::work_context::PaneWorkContext {
                work_title: Some("Current derived title".into()),
                ..Default::default()
            },
        );
        set_persisted_session(&mut app, &terminal_id, "herdr:codex");

        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_016));
        assert!(app.flush_pane_settlement_events());

        assert_eq!(
            app.state.terminals[&terminal_id].manual_label.as_deref(),
            Some("Current derived title")
        );
    }

    #[tokio::test]
    async fn settle_stops_agent_false_preserves_live_behavior() {
        let mut config = crate::config::Config::default();
        config.session.settle_stops_agent = false;
        let (mut app, pane_id, terminal_id, _rx) = app_with_runtime(
            &config,
            crate::work_context::PaneWorkContext {
                work_title: Some("Live title".into()),
                ..Default::default()
            },
        );
        set_persisted_session(&mut app, &terminal_id, "herdr:codex");

        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_017));
        assert!(app.flush_pane_settlement_events());

        assert!(app
            .terminal_runtimes
            .get(&terminal_id)
            .is_some_and(|runtime| !runtime.is_suspended()));
        assert!(app.state.terminals[&terminal_id]
            .pending_agent_resume_plan
            .is_none());
        assert!(app.state.terminals[&terminal_id].manual_label.is_none());
        assert!(app.state.terminals[&terminal_id]
            .settled_auto_label
            .is_none());
    }

    #[test]
    fn settlement_transitions_emit_settled_and_unsettled_events() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            event_hub.clone(),
        );
        let (state, pane_id) = state_with_context(Default::default());
        app.state = state;
        let now = Instant::now();
        app.state.settle_after = Duration::ZERO;

        assert_eq!(
            app.state.refresh_settled_panes_at(None, now, 1_725_000_003),
            1
        );
        assert!(app.flush_pane_settlement_events());
        assert!(app.state.note_pane_activity_at(pane_id, now));
        assert!(app.flush_pane_settlement_events());

        let events = event_hub.events_after(0);
        assert!(events.iter().any(|(_, event)| matches!(
            &event.data,
            crate::api::schema::EventData::PaneSettled {
                settled_at: 1_725_000_003,
                ..
            }
        )));
        assert!(events.iter().any(|(_, event)| matches!(
            event.data,
            crate::api::schema::EventData::PaneUnsettled { .. }
        )));
    }
}
