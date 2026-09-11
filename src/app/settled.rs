use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::layout::PaneId;

use super::{state::PaneSettlementChange, App, AppState};

struct PaneSettlementCandidate {
    agent_ref: crate::api::schema::AgentRef,
    ws_idx: usize,
    pane_id: PaneId,
}

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
            // A closed, unmerged pull request usually means work continues.
            // Only merging makes the pull request a settlement candidate.
            state_is(Some(state), "merged")
                .then(|| format!("pr:{}:merged", primary.to_ascii_lowercase()))
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

/// Whether the pane can be brought back after its agent is stopped.
pub(crate) fn pane_has_resume_plan(terminal: &crate::terminal::TerminalState) -> bool {
    terminal
        .persisted_agent_session
        .as_ref()
        .is_some_and(|session| {
            crate::agent_resume::plan(&session.source, &session.agent, &session.session_ref)
                .is_some()
        })
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
    fn settle_owned_candidates(
        &mut self,
        candidates: &[PaneSettlementCandidate],
        now_unix: u64,
    ) -> usize {
        let mut settled = 0;
        for candidate in candidates {
            if candidate.agent_ref.host != self.agent_host_name {
                // This client does not own remote panes. Selection may point at
                // one, but settlement must never mutate a colliding local id.
                continue;
            }
            settled +=
                usize::from(self.settle_pane_at(candidate.ws_idx, candidate.pane_id, now_unix));
        }
        settled
    }

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
        self.mark_session_dirty();
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
        if activity {
            self.mark_session_dirty();
        }
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
        let mut arm_writes = Vec::new();
        for (ws_idx, workspace) in self.workspaces.iter().enumerate() {
            for (tab_idx, tab) in workspace.tabs.iter().enumerate() {
                for (pane_id, pane) in &tab.panes {
                    let Some(terminal) = self.terminals.get(&pane.attached_terminal_id) else {
                        continue;
                    };
                    let projection = pane.agent_projection(terminal);
                    let quiet = crate::app::pane_lifecycle::pane_is_quiet(pane, terminal);
                    let quiet_observation_changed = pane.activity.quiet_observation_changes(quiet);
                    if pane.settled_at.is_some() {
                        if quiet_observation_changed {
                            arm_writes.push((ws_idx, *pane_id, pane.finished_since, quiet));
                        }
                        continue;
                    }
                    if projection.needs_human_attention()
                        || projection.open_blockers
                        || terminal.declares_running_subagents()
                        || terminal.holds_shell
                    {
                        // Settling suspends the agent and would bury an
                        // unanswered question or stop work below the parent.
                        // A pane waiting on the human is never a settle
                        // candidate, no matter its severity. Do not age the
                        // finished-work grace behind a guard.
                        if pane.finished_since.is_some() || quiet_observation_changed {
                            arm_writes.push((ws_idx, *pane_id, None, quiet));
                        }
                        continue;
                    }
                    let context = terminal.effective_work_context();
                    let work_key = work_context_settlement_key(context, snapshot);
                    let new_work_trigger = work_key
                        .as_ref()
                        .is_some_and(|key| pane.settled_work_key.as_ref() != Some(key));
                    let inactive_for = pane.activity.inactive_for(now);
                    let inactive = self.auto_settle_inactive && inactive_for >= self.settle_after;
                    let new_work_trigger = self.auto_settle_finished && new_work_trigger;
                    // A finished reading is evidence, not a verdict. A pull
                    // request reads merged while its agent works the follow-up,
                    // and a ticket can be moved to done by someone else. So the
                    // reading has to survive `settle_finished_after` with the
                    // pane quiet before it settles: settling stops the agent,
                    // which costs far more than waiting out a wrong reading.
                    let finished_since = pane.finished_since.unwrap_or(now);
                    let finished_ripe = now.saturating_duration_since(finished_since)
                        >= self.settle_finished_after
                        && inactive_for >= self.settle_finished_after;
                    // A quiet agent has no work running and no human decision
                    // pending. Use the pane activity clock rather than the
                    // transient unread-Done clock, which focus clears.
                    let quiet_ripe = self.auto_settle_done
                        && !self.is_active_pane(ws_idx, tab_idx, *pane_id)
                        && !tab.pinned
                        && quiet
                        && pane.activity.quiet_for(now).unwrap_or_default()
                            >= self.settle_done_after;
                    let holding = new_work_trigger && !finished_ripe;
                    let armed = holding.then_some(finished_since);
                    if pane.finished_since != armed || quiet_observation_changed {
                        arm_writes.push((ws_idx, *pane_id, armed, quiet));
                    }
                    // Recording the observed key consumes the one-shot trigger,
                    // so it must wait until the grace window resolves.
                    if !holding {
                        observed_work_keys.push((ws_idx, *pane_id, work_key.clone()));
                    }
                    if inactive || quiet_ripe || (new_work_trigger && finished_ripe) {
                        let Ok(agent_ref) = crate::api::schema::AgentRef::new(
                            self.agent_host_name.clone(),
                            pane_id.raw().to_string(),
                        ) else {
                            continue;
                        };
                        candidates.push(PaneSettlementCandidate {
                            agent_ref,
                            ws_idx,
                            pane_id: *pane_id,
                        });
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
        let mut quiet_clock_changed = false;
        for (ws_idx, pane_id, armed, quiet) in &arm_writes {
            if let Some(pane) = self.workspaces[*ws_idx]
                .tabs
                .iter_mut()
                .find_map(|tab| tab.panes.get_mut(pane_id))
            {
                pane.finished_since = *armed;
                quiet_clock_changed |= pane.activity.observe_quiet(*quiet, now);
            }
        }
        if quiet_clock_changed {
            self.mark_session_dirty();
        }
        self.settle_owned_candidates(&candidates, now_unix)
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

    fn settled_state_with_agent_state(
        agent_state: AgentState,
        quiet_since: Instant,
    ) -> (AppState, PaneId) {
        let (mut state, pane_id) = state_with_context(Default::default());
        let terminal_id = state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let _ = state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal")
            .set_detected_state(Some(crate::detect::Agent::Codex), agent_state);
        state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane")
            .activity
            .set_last_at(quiet_since);
        assert!(state.settle_pane_at(0, pane_id, 1_725_000_002));
        (state, pane_id)
    }

    fn transition_agent_state(
        state: &mut AppState,
        pane_id: PaneId,
        agent_state: AgentState,
        now: Instant,
    ) {
        state
            .update_terminal_state_at(pane_id, now, |terminal| {
                Some(terminal.set_detected_state_with_screen_signals_at(
                    Some(crate::detect::Agent::Codex),
                    agent_state,
                    false,
                    false,
                    false,
                    false,
                    false,
                    now,
                ))
            })
            .expect("agent state transition");
    }

    fn assert_quiet_clock_restarted(state: &AppState, pane_id: PaneId, transition_at: Instant) {
        assert_eq!(
            state.workspaces[0].tabs[0].panes[&pane_id]
                .activity
                .inactive_for(transition_at + Duration::from_secs(60)),
            Duration::from_secs(60)
        );
    }

    fn settle_ready_remote_collision(
        now: Instant,
    ) -> (AppState, PaneId, crate::work_index::Snapshot) {
        let url = "https://github.com/owner/repo/pull/7";
        let (mut state, pane_id) = state_with_context(crate::work_context::PaneWorkContext {
            pr_urls: vec![url.into()],
            ..Default::default()
        });
        state.agent_host_name = "local".into();
        state.auto_settle_inactive = false;
        let quiet_since = now - state.settle_finished_after;
        let pane = state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane");
        pane.activity.set_last_at(quiet_since);
        pane.finished_since = Some(quiet_since);

        let remote_agent: crate::api::schema::AgentInfo =
            serde_json::from_value(serde_json::json!({
                "agent_ref": format!("remote::{}", pane_id.raw()),
                "terminal_id": "remote-term",
                "name": "remote reviewer",
                "agent": "codex",
                "agent_status": "idle",
                "workspace_id": "remote-workspace",
                "tab_id": "remote-tab",
                "pane_id": pane_id.raw().to_string(),
                "focused": false,
                "state_change_seq": 1,
                "revision": 1
            }))
            .expect("remote agent fixture");
        state.remote_agent_panel_entries =
            crate::ui::remote_agent_panel_entries(&crate::fleet::Snapshot {
                hosts: vec![crate::fleet::HostSnapshot {
                    name: "remote".into(),
                    target: "remote".into(),
                    local: false,
                    session: None,
                    socket: None,
                    state: crate::fleet::HostState::Reachable,
                    version: None,
                    protocol: None,
                    error: None,
                    entries: vec![crate::fleet::FleetRow::test_agent_info_row(
                        "remote",
                        remote_agent,
                    )],
                }],
                ..crate::fleet::Snapshot::default()
            });
        let mut merged_item = item();
        merged_item.pr_url = Some(url.into());
        merged_item.pr_state = Some("merged".into());
        (state, pane_id, snapshot(merged_item))
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
    fn primary_pr_merged_settles_and_input_unsettles() {
        let url = "https://github.com/owner/repo/pull/7";
        let (mut state, pane_id) = state_with_context(crate::work_context::PaneWorkContext {
            pr_urls: vec![url.into()],
            ..Default::default()
        });
        let mut first_item = item();
        first_item.pr_url = Some(url.into());
        first_item.pr_state = Some("merged".into());

        let now = Instant::now();
        assert_eq!(
            state.refresh_settled_panes_at(Some(&snapshot(first_item.clone())), now, 1_725_000_000),
            0,
            "a finished pull request arms the grace window instead of settling"
        );
        assert!(!state.pane_is_settled(0, pane_id));
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(first_item)),
                now + state.settle_finished_after,
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
                    item.pr_state = Some("merged".into());
                    item
                })),
                Instant::now(),
                1_725_000_001
            ),
            0,
            "resuming completed work must consume the trigger"
        );
    }

    #[test]
    fn primary_pr_closed_without_merge_never_settles() {
        let url = "https://github.com/owner/repo/pull/7";
        let (mut state, pane_id) = state_with_context(crate::work_context::PaneWorkContext {
            pr_urls: vec![url.into()],
            ..Default::default()
        });
        state.auto_settle_inactive = false;
        let mut closed_item = item();
        closed_item.pr_url = Some(url.into());
        closed_item.pr_state = Some("closed".into());

        let now = Instant::now();
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(closed_item.clone())),
                now,
                1_725_000_000
            ),
            0
        );
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(closed_item)),
                now + state.settle_finished_after * 3,
                1_725_000_001
            ),
            0
        );
        assert!(!state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn blocked_pane_with_merged_pr_waits_for_a_fresh_grace_window() {
        let url = "https://github.com/owner/repo/pull/7";
        let (mut state, pane_id) = state_with_context(crate::work_context::PaneWorkContext {
            pr_urls: vec![url.into()],
            ..Default::default()
        });
        let terminal_id = state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal")
            .set_detected_state(Some(crate::detect::Agent::Codex), AgentState::Blocked);
        state.settle_after = Duration::ZERO;
        let mut merged_item = item();
        merged_item.pr_url = Some(url.into());
        merged_item.pr_state = Some("merged".into());

        let now = Instant::now();
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(merged_item.clone())),
                now,
                1_725_000_000
            ),
            0
        );
        let after_old_deadline = now + state.settle_finished_after * 3;
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(merged_item.clone())),
                after_old_deadline,
                1_725_000_001
            ),
            0
        );
        let pane = &state.workspaces[0].tabs[0].panes[&pane_id];
        assert!(!state.pane_is_settled(0, pane_id));
        assert!(pane.finished_since.is_none());
        assert!(pane.settled_work_key.is_none());

        state.auto_settle_inactive = false;
        state
            .update_terminal_state_at(pane_id, after_old_deadline, |terminal| {
                Some(terminal.set_detected_state_with_screen_signals_at(
                    Some(crate::detect::Agent::Codex),
                    AgentState::Idle,
                    false,
                    false,
                    false,
                    false,
                    false,
                    after_old_deadline,
                ))
            })
            .expect("unblocking state transition");
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(merged_item.clone())),
                after_old_deadline,
                1_725_000_002
            ),
            0,
            "unblocking starts the normal grace window"
        );
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(merged_item)),
                after_old_deadline + state.settle_finished_after,
                1_725_000_003
            ),
            1
        );
        assert!(state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn finished_reading_that_disappears_during_the_grace_window_revokes_the_trigger() {
        let url = "https://github.com/owner/repo/pull/7";
        let (mut state, pane_id) = state_with_context(crate::work_context::PaneWorkContext {
            pr_urls: vec![url.into()],
            ..Default::default()
        });
        let merged = || {
            let mut item = item();
            item.pr_url = Some(url.into());
            item.pr_state = Some("merged".into());
            item
        };
        let open = || {
            let mut item = item();
            item.pr_url = Some(url.into());
            item.pr_state = Some("open".into());
            item
        };

        let now = Instant::now();
        assert_eq!(
            state.refresh_settled_panes_at(Some(&snapshot(merged())), now, 1_725_000_000),
            0
        );
        // The reading is revoked mid-window: the pane must never settle for it.
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(open())),
                now + state.settle_finished_after / 2,
                1_725_000_001
            ),
            0
        );
        assert!(!state.pane_is_settled(0, pane_id));

        // A fresh finished reading arms a fresh window rather than inheriting
        // the revoked one.
        let restart = now + state.settle_finished_after;
        assert_eq!(
            state.refresh_settled_panes_at(Some(&snapshot(merged())), restart, 1_725_000_002),
            0
        );
        assert!(!state.pane_is_settled(0, pane_id));
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(merged())),
                restart + state.settle_finished_after,
                1_725_000_003
            ),
            1
        );
        assert!(state.pane_is_settled(0, pane_id));
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
        let now = Instant::now();
        assert_eq!(
            state.refresh_settled_panes_at(Some(&snapshot(item.clone())), now, 1_725_000_001),
            0,
            "a done ticket arms the grace window instead of settling"
        );
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(item)),
                now + state.settle_finished_after,
                1_725_000_001
            ),
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
    fn settled_pane_stays_settled_when_idle_becomes_unknown_and_restarts_quiet_clock() {
        let now = Instant::now();
        let transition_at = now + Duration::from_secs(60);
        let (mut state, pane_id) = settled_state_with_agent_state(AgentState::Idle, now);

        transition_agent_state(&mut state, pane_id, AgentState::Unknown, transition_at);

        assert!(state.pane_is_settled(0, pane_id));
        assert_quiet_clock_restarted(&state, pane_id, transition_at);
    }

    #[test]
    fn settled_pane_stays_settled_when_unknown_becomes_idle_and_restarts_quiet_clock() {
        let now = Instant::now();
        let transition_at = now + Duration::from_secs(60);
        let (mut state, pane_id) = settled_state_with_agent_state(AgentState::Unknown, now);

        transition_agent_state(&mut state, pane_id, AgentState::Idle, transition_at);

        assert!(state.pane_is_settled(0, pane_id));
        assert_quiet_clock_restarted(&state, pane_id, transition_at);
    }

    #[test]
    fn settled_pane_unsettles_when_agent_enters_working() {
        let now = Instant::now();
        let transition_at = now + Duration::from_secs(60);
        let (mut state, pane_id) = settled_state_with_agent_state(AgentState::Idle, now);

        transition_agent_state(&mut state, pane_id, AgentState::Working, transition_at);

        assert!(!state.pane_is_settled(0, pane_id));
        assert_quiet_clock_restarted(&state, pane_id, transition_at);
    }

    #[test]
    fn revoked_finished_reading_requires_a_fresh_grace_window() {
        let url = "https://github.com/owner/repo/pull/7";
        let (mut state, pane_id) = state_with_context(crate::work_context::PaneWorkContext {
            pr_urls: vec![url.into()],
            ..Default::default()
        });
        let mut finished_item = item();
        finished_item.pr_url = Some(url.into());
        finished_item.pr_state = Some("merged".into());
        let mut unfinished_item = finished_item.clone();
        unfinished_item.pr_state = Some("open".into());
        let now = Instant::now();
        let old_deadline = now + state.settle_finished_after;

        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(finished_item.clone())),
                now,
                1_725_000_001
            ),
            0
        );
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(unfinished_item.clone())),
                now + state.settle_finished_after / 2,
                1_725_000_002
            ),
            0
        );
        assert!(state.workspaces[0].tabs[0].panes[&pane_id]
            .finished_since
            .is_none());
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(unfinished_item)),
                old_deadline,
                1_725_000_003
            ),
            0,
            "revoked finished work must stay unsettled past the old deadline"
        );
        assert!(!state.pane_is_settled(0, pane_id));

        let rearmed_at = old_deadline + Duration::from_secs(1);
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(finished_item.clone())),
                rearmed_at,
                1_725_000_004
            ),
            0,
            "a fresh finished reading re-arms the grace window"
        );
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&snapshot(finished_item)),
                rearmed_at + state.settle_finished_after,
                1_725_000_005
            ),
            1
        );
        assert!(state.pane_is_settled(0, pane_id));
    }

    /// A Done pane the way the lifecycle sees one: an idle detected agent whose
    /// output nobody has read since it stopped.
    fn done_state(resumable: bool, done_since: Instant) -> (AppState, PaneId) {
        let (mut state, pane_id) = state_with_context(Default::default());
        let terminal_id = state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal");
        terminal.detected_agent = Some(crate::detect::Agent::Codex);
        terminal.set_raw_agent_state_for_test(crate::detect::AgentState::Idle);
        if resumable {
            terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("done-session")
                    .expect("valid session id"),
            });
        }
        let pane = state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane");
        pane.seen = false;
        pane.done_since = Some(done_since);
        pane.activity.set_last_at(done_since);
        pane.activity.observe_quiet(true, done_since);
        state.settle_done_after = Duration::from_secs(30 * 60);
        // Isolate the Done trigger from the other two.
        state.auto_settle_inactive = false;
        state.auto_settle_finished = false;
        (state, pane_id)
    }

    fn stale_state(screen_state: Option<AgentState>, quiet_since: Instant) -> (AppState, PaneId) {
        let (mut state, pane_id) = state_with_context(Default::default());
        let terminal_id = state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal");
        terminal.set_detected_state(Some(crate::detect::Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
            Some(1_000),
            quiet_since,
        );
        terminal
            .mark_agent_status_stale_at(quiet_since + crate::terminal::state::AGENT_STALE_SILENCE)
            .expect("working report should become stale");
        assert!(terminal.supervisor_stale);
        if screen_state.is_none() {
            terminal.set_raw_agent_state_for_test(AgentState::Idle);
        }
        state.note_pane_activity_at(pane_id, quiet_since);
        state.auto_settle_inactive = false;
        state.auto_settle_finished = false;
        state.settle_done_after = Duration::from_secs(30 * 60);
        state.handle_app_event(crate::events::AppEvent::PaneProcessStateChanged {
            pane_id,
            holds_shell: false,
            stale_resolution: screen_state.map(|state| (state, false)),
        });
        let _ = state.refresh_settled_panes_at(None, quiet_since, 1_724_999_999);
        (state, pane_id)
    }

    #[test]
    fn done_resumable_pane_settles_once_the_done_window_passes() {
        let now = Instant::now();
        let (mut state, pane_id) = done_state(true, now - Duration::from_secs(29 * 60));
        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_030), 0);
        assert!(!state.pane_is_settled(0, pane_id));

        let (mut state, pane_id) = done_state(true, now - Duration::from_secs(31 * 60));
        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_031), 1);
        assert!(state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn quiet_seen_pane_settles_after_focus_moves_elsewhere() {
        let (mut state, pane_id) = state_with_context(Default::default());
        let terminal_id = state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let now = Instant::now();
        state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal")
            .set_detected_state(Some(crate::detect::Agent::Codex), AgentState::Idle);
        state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal")
            .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("seen-session")
                    .expect("valid session id"),
            });
        state.auto_settle_inactive = false;
        state.auto_settle_finished = false;
        state.settle_done_after = Duration::from_secs(30 * 60);
        state.note_pane_activity_at(pane_id, now);

        state.active = Some(0);
        state.focus_pane_in_workspace(0, pane_id);
        assert!(state.workspaces[0].tabs[0].panes[&pane_id].seen);
        assert_eq!(
            state.refresh_settled_panes_at(None, now, 1_725_000_035),
            0,
            "focus blocks settling while the quiet clock is observed"
        );
        let other = Workspace::test_new("other");
        let other_pane = other.tabs[0].root_pane;
        state.workspaces.push(other);
        state.ensure_test_terminals();
        state.focus_pane_in_workspace(1, other_pane);

        assert_eq!(
            state.refresh_settled_panes_at(None, now + state.settle_done_after, 1_725_000_036),
            1
        );
        assert!(state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn stale_pane_settles_only_when_its_screen_resolves_idle() {
        let now = Instant::now();
        let (mut idle, idle_pane) = stale_state(Some(AgentState::Idle), now);
        let settle_at = idle.workspaces[0].tabs[0].panes[&idle_pane]
            .activity
            .last_at()
            + idle.settle_done_after;
        assert_eq!(
            idle.refresh_settled_panes_at(None, settle_at, 1_725_000_037),
            1
        );
        assert!(idle.pane_is_settled(0, idle_pane));

        let (mut unresolved, unresolved_pane) = stale_state(None, now);
        let settle_at = unresolved.workspaces[0].tabs[0].panes[&unresolved_pane]
            .activity
            .last_at()
            + unresolved.settle_done_after;
        assert_eq!(
            unresolved.refresh_settled_panes_at(None, settle_at, 1_725_000_038),
            0,
            "a stale pane needs screen evidence before it can settle"
        );
        assert!(!unresolved.pane_is_settled(0, unresolved_pane));

        for screen_state in [AgentState::Working, AgentState::Blocked] {
            let (mut state, pane_id) = stale_state(Some(screen_state), now);
            let settle_at = state.workspaces[0].tabs[0].panes[&pane_id]
                .activity
                .last_at()
                + state.settle_done_after;
            assert_eq!(
                state.refresh_settled_panes_at(None, settle_at, 1_725_000_039),
                0,
                "stale {screen_state:?} pane settled"
            );
            assert!(!state.pane_is_settled(0, pane_id));
        }
    }

    #[test]
    fn quiet_settle_deadline_covers_seen_and_stale_idle_panes() {
        let now = Instant::now();
        let expected = now + Duration::from_secs(30 * 60);
        let (mut seen, seen_pane) = done_state(true, now);
        let pane = seen.workspaces[0].tabs[0]
            .panes
            .get_mut(&seen_pane)
            .expect("seen pane");
        pane.seen = true;
        pane.activity.set_last_at(now);
        assert_eq!(seen.next_done_settle_deadline(now), Some(expected));

        let (stale, stale_pane) = stale_state(Some(AgentState::Idle), now);
        let stale_expected = stale.workspaces[0].tabs[0].panes[&stale_pane]
            .activity
            .last_at()
            + stale.settle_done_after;
        assert_eq!(stale.next_done_settle_deadline(now), Some(stale_expected));
    }

    #[test]
    fn stale_resolution_event_starts_the_quiet_window_at_the_evaluator() {
        let now = Instant::now();
        let old_activity = now - Duration::from_secs(2 * 60 * 60);
        let (mut state, pane_id) = stale_state(Some(AgentState::Working), old_activity);

        state.handle_app_event(crate::events::AppEvent::PaneProcessStateChanged {
            pane_id,
            holds_shell: false,
            stale_resolution: Some((AgentState::Idle, false)),
        });

        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_040), 0);
        assert_eq!(
            state.refresh_settled_panes_at(
                None,
                now + state.settle_done_after - Duration::from_nanos(1),
                1_725_001_839,
            ),
            0
        );
        assert_eq!(
            state.refresh_settled_panes_at(None, now + state.settle_done_after, 1_725_001_840,),
            1
        );
    }

    #[test]
    fn held_shell_end_event_starts_the_quiet_window_at_the_evaluator() {
        let now = Instant::now();
        let old_activity = now - Duration::from_secs(2 * 60 * 60);
        let (mut state, pane_id) = stale_state(Some(AgentState::Idle), old_activity);
        state.handle_app_event(crate::events::AppEvent::PaneProcessStateChanged {
            pane_id,
            holds_shell: true,
            stale_resolution: Some((AgentState::Idle, false)),
        });
        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_042), 0);

        state.handle_app_event(crate::events::AppEvent::PaneProcessStateChanged {
            pane_id,
            holds_shell: false,
            stale_resolution: Some((AgentState::Idle, false)),
        });

        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_043), 0);
        assert_eq!(
            state.refresh_settled_panes_at(
                None,
                now + state.settle_done_after - Duration::from_nanos(1),
                1_725_001_841,
            ),
            0
        );
        assert_eq!(
            state.refresh_settled_panes_at(None, now + state.settle_done_after, 1_725_001_842,),
            1
        );
    }

    #[test]
    fn stale_resolution_event_unsettles_an_active_projection() {
        let now = Instant::now();
        for active_state in [AgentState::Working, AgentState::Blocked] {
            let (mut state, pane_id) = stale_state(Some(AgentState::Idle), now);
            state.workspaces[0].tabs[0]
                .panes
                .get_mut(&pane_id)
                .expect("root pane")
                .settled_at = Some(1_725_000_044);

            state.handle_app_event(crate::events::AppEvent::PaneProcessStateChanged {
                pane_id,
                holds_shell: false,
                stale_resolution: Some((active_state, false)),
            });

            assert!(
                !state.pane_is_settled(0, pane_id),
                "stale {active_state:?} projection stayed settled"
            );
        }
    }

    #[test]
    fn quiet_pane_without_a_resume_plan_settles_and_remains_reapable() {
        let now = Instant::now();
        let (mut state, pane_id) = done_state(false, now - Duration::from_secs(5 * 60 * 60));
        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_032), 1);
        assert!(state.pane_is_settled(0, pane_id));
        assert_eq!(
            state.next_done_reap_deadline(now),
            Some(now),
            "settling an unresumable pane must preserve its reap clock"
        );
        assert!(state.next_done_settle_deadline(now).is_none());
    }

    #[test]
    fn done_settle_skips_the_active_pane_and_pinned_tabs() {
        let now = Instant::now();
        let done_since = now - Duration::from_secs(31 * 60);
        let (mut state, pane_id) = done_state(true, done_since);
        state.active = Some(0);
        state.workspaces[0].tabs[0].panes.get_mut(&pane_id);
        assert!(
            state.is_active_pane(0, 0, pane_id),
            "the focused pane is the one the human is looking at"
        );
        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_033), 0);
        assert!(!state.pane_is_settled(0, pane_id));

        let (mut state, pane_id) = done_state(true, done_since);
        state.active = None;
        state.workspaces[0].tabs[0].pinned = true;
        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_034), 0);
        assert!(!state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn quiet_settle_preserves_blocked_and_closing_gate_exclusions() {
        let now = Instant::now();
        let quiet_since = now - Duration::from_secs(31 * 60);
        let (mut eligible, eligible_pane) = done_state(true, quiet_since);
        eligible.workspaces[0].tabs[0]
            .panes
            .get_mut(&eligible_pane)
            .expect("eligible pane")
            .seen = true;
        assert_eq!(
            eligible.refresh_settled_panes_at(None, now, 1_725_000_038),
            1,
            "the control pane must be eligible before exclusions are applied"
        );

        let (mut blocked, blocked_pane) = done_state(true, quiet_since);
        let blocked_terminal_id = blocked.workspaces[0].tabs[0].panes[&blocked_pane]
            .attached_terminal_id
            .clone();
        blocked
            .terminals
            .get_mut(&blocked_terminal_id)
            .expect("blocked terminal")
            .set_raw_agent_state_for_test(AgentState::Blocked);
        assert_eq!(
            blocked.refresh_settled_panes_at(None, now, 1_725_000_039),
            0
        );

        let (mut gated, gated_pane) = done_state(true, quiet_since);
        let gated_terminal_id = gated.workspaces[0].tabs[0].panes[&gated_pane]
            .attached_terminal_id
            .clone();
        gated
            .terminals
            .get_mut(&gated_terminal_id)
            .expect("gated terminal")
            .closing_gates = vec![crate::api::schema::ClosingBlockItem {
            n: 1,
            label: "Gate".into(),
            text: "Choose the release path".into(),
            pr: None,
            ticket: None,
            url: None,
            default: None,
            default_at: None,
        }];
        assert_eq!(gated.refresh_settled_panes_at(None, now, 1_725_000_040), 0);
    }

    #[test]
    fn done_settle_can_be_turned_off() {
        let now = Instant::now();
        let (mut state, pane_id) = done_state(true, now - Duration::from_secs(5 * 60 * 60));
        state.auto_settle_done = false;
        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_035), 0);
        assert!(!state.pane_is_settled(0, pane_id));
        assert!(state.next_done_settle_deadline(now).is_none());
    }

    #[test]
    fn done_settle_deadline_is_the_pane_done_window() {
        let now = Instant::now();
        let (state, _) = done_state(true, now - Duration::from_secs(10 * 60));
        assert_eq!(
            state.next_done_settle_deadline(now),
            Some(now + Duration::from_secs(20 * 60))
        );
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
    fn inactivity_still_settles_a_stale_working_pane() {
        let (mut state, pane_id) = state_with_context(Default::default());
        let terminal_id = state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let now = Instant::now();
        state.settle_after = Duration::from_secs(3 * 24 * 60 * 60);
        state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal")
            .set_detected_state(Some(crate::detect::Agent::Codex), AgentState::Working);
        state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane")
            .activity
            .set_last_at(now - state.settle_after);

        assert_eq!(state.refresh_settled_panes_at(None, now, 1_725_000_003), 1);
        assert!(state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn held_shell_blocks_overdue_inactivity_and_ripe_finished_work() {
        let url = "https://github.com/owner/repo/pull/17";
        let (mut state, pane_id) = state_with_context(crate::work_context::PaneWorkContext {
            pr_urls: vec![url.into()],
            ..Default::default()
        });
        let mut merged_item = item();
        merged_item.pr_url = Some(url.into());
        merged_item.pr_state = Some("merged".into());
        let work = snapshot(merged_item);
        let now = Instant::now();
        state.auto_settle_done = false;
        state.auto_settle_inactive = false;
        state.settle_after = Duration::from_secs(60);
        state.settle_finished_after = Duration::from_secs(30);
        state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane")
            .activity
            .set_last_at(now - Duration::from_secs(120));
        let armed_at = now - state.settle_finished_after;
        assert_eq!(
            state.refresh_settled_panes_at(Some(&work), armed_at, 1_725_000_010),
            0
        );

        state.handle_app_event(crate::events::AppEvent::PaneProcessStateChanged {
            pane_id,
            holds_shell: true,
            stale_resolution: None,
        });
        state.auto_settle_inactive = true;
        assert!(
            state.workspaces[0].tabs[0].panes[&pane_id]
                .activity
                .inactive_for(now)
                >= state.settle_after
        );
        assert_eq!(
            state.workspaces[0].tabs[0].panes[&pane_id].finished_since,
            Some(armed_at)
        );
        assert_eq!(
            state.refresh_settled_panes_at(Some(&work), now, 1_725_000_011),
            0,
            "a held shell must outrank both ripe settle triggers"
        );
        assert!(!state.pane_is_settled(0, pane_id));

        state.handle_app_event(crate::events::AppEvent::PaneProcessStateChanged {
            pane_id,
            holds_shell: false,
            stale_resolution: None,
        });
        state.auto_settle_inactive = false;
        assert_eq!(
            state.refresh_settled_panes_at(Some(&work), now, 1_725_000_012),
            0,
            "clearing the shell must arm a fresh finished-work window"
        );
        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&work),
                now + state.settle_finished_after,
                1_725_000_013,
            ),
            1
        );
        assert!(state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn remote_candidate_never_settles_a_colliding_local_pane() {
        let (mut state, pane_id) = state_with_context(Default::default());
        state.agent_host_name = "local".into();
        let candidate = PaneSettlementCandidate {
            agent_ref: crate::api::schema::AgentRef::new("remote", pane_id.raw().to_string())
                .expect("valid remote agent reference"),
            ws_idx: 0,
            pane_id,
        };

        assert_eq!(
            state.settle_owned_candidates(&[candidate], 1_725_000_002),
            0
        );
        assert!(!state.pane_is_settled(0, pane_id));
        assert!(state.pending_pane_settlement_changes.is_empty());
    }

    #[test]
    fn refresh_never_settles_remote_row_with_colliding_local_pane_id() {
        let now = Instant::now();
        let (mut state, pane_id, work) = settle_ready_remote_collision(now);
        assert_eq!(state.remote_agent_panel_entries[0].agent_ref.host, "remote");
        assert_eq!(
            state.remote_agent_panel_entries[0].agent_ref.agent,
            pane_id.raw().to_string()
        );
        assert_eq!(
            state.remote_agent_panel_entries[0].entry.pane_id,
            PaneId::from_raw(0),
            "remote projection carries no local pane target"
        );

        state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane")
            .finished_since = None;
        assert_eq!(
            state.refresh_settled_panes_at(Some(&work), now, 1_725_000_003),
            0,
            "a fresh finished reading waits through the grace window"
        );
        assert!(!state.pane_is_settled(0, pane_id));
        assert!(state.pending_pane_settlement_changes.is_empty());

        assert_eq!(
            state.refresh_settled_panes_at(
                Some(&work),
                now + state.settle_finished_after,
                1_725_000_004
            ),
            1,
            "the eligible local pane settles through the refresh path"
        );
        assert!(state.pane_is_settled(0, pane_id));
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
    fn meaningful_activity_marks_the_session_for_persistence() {
        let (mut state, pane_id) = state_with_context(Default::default());
        state.session_dirty = false;
        state.session_dirty_revision = 0;

        assert!(!state.note_pane_activity_at(pane_id, Instant::now()));
        assert!(state.session_dirty);
        assert_eq!(state.session_dirty_revision, 1);
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
