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

    pub(crate) fn observe_pane_output_at(
        &mut self,
        pane_id: PaneId,
        revision: u64,
        now: Instant,
    ) -> bool {
        let Some((ws_idx, pane)) = self.pane_state_mut(pane_id) else {
            return false;
        };
        let activity = pane.activity.observe_content_revision(revision, now);
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
                    let inactive = pane.activity.inactive_for(now) >= self.settle_after;
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
        let revisions = self
            .state
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.tabs.iter())
            .flat_map(|tab| tab.panes.iter())
            .filter_map(|(pane_id, pane)| {
                self.terminal_runtimes
                    .get(&pane.attached_terminal_id)
                    .map(|runtime| (*pane_id, runtime.content_revision()))
            })
            .collect::<Vec<_>>();
        let mut changed = false;
        for (pane_id, revision) in revisions {
            changed |= self.state.observe_pane_output_at(pane_id, revision, now);
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
    use crate::{detect::AgentState, terminal::TerminalState, workspace::Workspace};

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
    fn inactivity_settles_and_output_unsettles() {
        let (mut state, pane_id) = state_with_context(Default::default());
        let now = Instant::now();
        assert!(!state.observe_pane_output_at(pane_id, 0, now));
        state.settle_after = Duration::from_secs(3 * 24 * 60 * 60);
        state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane")
            .activity
            .set_last_at(now - state.settle_after);
        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_002), 1);

        assert!(state.observe_pane_output_at(pane_id, 1, now));
        assert!(!state.pane_is_settled(0, pane_id));
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
