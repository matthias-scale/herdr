use std::time::{Duration, Instant};

use bytes::Bytes;

use super::App;

const STALL_NUDGE_SUBMIT_DELAY: Duration = Duration::from_millis(300);

#[derive(Debug, Clone)]
pub(crate) struct StallNudgeEpisode {
    pane_id: crate::layout::PaneId,
    nudges_sent: u32,
    next_nudge_at: Option<Instant>,
    declaration_kind: &'static str,
    last_drop_reason: Option<&'static str>,
}

/// Everything the stalled-pane nudge decision needs, read in one pass.
struct AutoNudgeFacts {
    enabled: bool,
    message_present: bool,
    settled: bool,
    supervisor_stale: bool,
    quiet_for: Duration,
    nudge_after: Duration,
    blocked: bool,
    has_closing_gates: bool,
    human_draft: bool,
    resume_pending: bool,
    launch_pending: bool,
    detected_agent: bool,
    runtime_hosts_agent: bool,
    nudges_sent: u32,
    max_nudges: u32,
    next_nudge_at: Option<Instant>,
    now: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoNudgeDecision {
    Nudge,
    RetryAt(Instant),
    Reset(&'static str),
    Drop(&'static str),
}

fn auto_nudge_decision(facts: &AutoNudgeFacts) -> AutoNudgeDecision {
    if !facts.supervisor_stale {
        return AutoNudgeDecision::Reset("a fresh status report cleared the stale mark");
    }
    if !facts.enabled {
        return AutoNudgeDecision::Drop("automatic stalled-agent nudges are disabled");
    }
    if !facts.message_present {
        return AutoNudgeDecision::Drop("the stalled-agent nudge message is empty");
    }
    if facts.settled {
        return AutoNudgeDecision::Drop("the pane is settled");
    }
    if facts.blocked {
        return AutoNudgeDecision::Drop("the agent is blocked");
    }
    if facts.has_closing_gates {
        return AutoNudgeDecision::Drop("the pane declares a closing gate");
    }
    if facts.human_draft {
        return AutoNudgeDecision::Drop("the pane holds a draft the human typed");
    }
    if facts.resume_pending {
        return AutoNudgeDecision::Drop("the pane is parked for a pending resume");
    }
    if facts.launch_pending {
        return AutoNudgeDecision::Drop("the agent launch is pending");
    }
    if !facts.detected_agent {
        return AutoNudgeDecision::Drop("the pane has no detected agent");
    }
    if facts.nudges_sent >= facts.max_nudges {
        return AutoNudgeDecision::Drop("the stall episode exhausted its nudge budget");
    }
    if facts.quiet_for < facts.nudge_after {
        return AutoNudgeDecision::RetryAt(
            facts.now + facts.nudge_after.saturating_sub(facts.quiet_for),
        );
    }
    if let Some(next_nudge_at) = facts.next_nudge_at.filter(|due| *due > facts.now) {
        return AutoNudgeDecision::RetryAt(next_nudge_at);
    }
    if !facts.runtime_hosts_agent {
        return AutoNudgeDecision::Drop("the pane runtime does not host the detected agent");
    }
    AutoNudgeDecision::Nudge
}

fn next_stall_nudge_delay(
    nudge_after: Duration,
    nudges_sent_after_send: u32,
    max_nudges: u32,
) -> Option<Duration> {
    if nudges_sent_after_send >= max_nudges {
        return None;
    }
    let multiplier = 1_u32.checked_shl(nudges_sent_after_send)?;
    nudge_after.checked_mul(multiplier)
}

struct AutoNudgeTarget {
    pane_id: crate::layout::PaneId,
    terminal_id: crate::terminal::TerminalId,
    declaration_kind: &'static str,
    quiet_for: Duration,
    decision: AutoNudgeDecision,
}

impl App {
    pub(crate) fn next_auto_nudge_deadline(&self, now: Instant) -> Option<Instant> {
        if !self.state.auto_nudge_stalled_agents || self.state.stall_nudge_message.trim().is_empty()
        {
            return None;
        }
        self.auto_nudge_targets(now, false)
            .into_iter()
            .filter_map(|target| match target.decision {
                AutoNudgeDecision::Nudge => self
                    .stall_nudge_episodes
                    .get(&target.terminal_id)
                    .and_then(|episode| episode.last_drop_reason)
                    .is_none()
                    .then_some(now),
                AutoNudgeDecision::RetryAt(deadline) => Some(deadline),
                AutoNudgeDecision::Reset(_) | AutoNudgeDecision::Drop(_) => None,
            })
            .min()
    }

    pub(crate) fn tick_auto_nudges(&mut self, now: Instant) -> bool {
        if !self.state.auto_nudge_stalled_agents || self.state.stall_nudge_message.trim().is_empty()
        {
            let reason = if self.state.auto_nudge_stalled_agents {
                "the stalled-agent nudge message is empty"
            } else {
                "automatic stalled-agent nudges are disabled"
            };
            self.clear_stall_nudge_episodes(now, reason);
            return false;
        }
        self.prune_stall_nudge_episodes(now);

        let targets = self.auto_nudge_targets(now, true);
        let mut fire = Vec::new();
        for target in targets {
            match target.decision {
                AutoNudgeDecision::Reset(reason) => {
                    self.drop_stall_nudge_episode(
                        &target.terminal_id,
                        target.pane_id,
                        target.declaration_kind,
                        target.quiet_for,
                        reason,
                    );
                }
                AutoNudgeDecision::Drop(reason) => {
                    let episode = self
                        .stall_nudge_episodes
                        .entry(target.terminal_id.clone())
                        .or_insert(StallNudgeEpisode {
                            pane_id: target.pane_id,
                            nudges_sent: 0,
                            next_nudge_at: None,
                            declaration_kind: target.declaration_kind,
                            last_drop_reason: None,
                        });
                    episode.pane_id = target.pane_id;
                    episode.declaration_kind = target.declaration_kind;
                    if episode.last_drop_reason != Some(reason) {
                        tracing::debug!(
                            pane = target.pane_id.raw(),
                            terminal = %target.terminal_id,
                            declaration = target.declaration_kind,
                            quiet_seconds = target.quiet_for.as_secs(),
                            reason,
                            "dropping stalled-agent nudge candidate"
                        );
                        episode.last_drop_reason = Some(reason);
                    }
                }
                AutoNudgeDecision::RetryAt(_) => {
                    let episode = self
                        .stall_nudge_episodes
                        .entry(target.terminal_id.clone())
                        .or_insert(StallNudgeEpisode {
                            pane_id: target.pane_id,
                            nudges_sent: 0,
                            next_nudge_at: None,
                            declaration_kind: target.declaration_kind,
                            last_drop_reason: None,
                        });
                    episode.pane_id = target.pane_id;
                    episode.declaration_kind = target.declaration_kind;
                    episode.last_drop_reason = None;
                }
                AutoNudgeDecision::Nudge => fire.push(target),
            }
        }

        let mut changed = false;
        for target in fire {
            self.stall_nudge_episodes
                .entry(target.terminal_id.clone())
                .or_insert(StallNudgeEpisode {
                    pane_id: target.pane_id,
                    nudges_sent: 0,
                    next_nudge_at: None,
                    declaration_kind: target.declaration_kind,
                    last_drop_reason: None,
                });
            if !self.send_stall_nudge(&target, now) {
                if let Some(episode) = self.stall_nudge_episodes.get_mut(&target.terminal_id) {
                    episode.last_drop_reason = Some("failed to write the stalled-agent nudge");
                }
                continue;
            }

            let nudge_after = self.state.nudge_after;
            let max_nudges = self.state.max_nudges;
            let Some(episode) = self.stall_nudge_episodes.get_mut(&target.terminal_id) else {
                continue;
            };
            episode.nudges_sent = episode.nudges_sent.saturating_add(1);
            episode.next_nudge_at =
                next_stall_nudge_delay(nudge_after, episode.nudges_sent, max_nudges)
                    .and_then(|delay| now.checked_add(delay));
            episode.last_drop_reason = None;
            tracing::info!(
                pane = target.pane_id.raw(),
                terminal = %target.terminal_id,
                declaration = target.declaration_kind,
                quiet_seconds = target.quiet_for.as_secs(),
                nudge = episode.nudges_sent,
                max_nudges,
                "nudged a stalled agent pane"
            );
            changed = true;
        }
        changed
    }

    fn auto_nudge_targets(&self, now: Instant, verify_runtime_host: bool) -> Vec<AutoNudgeTarget> {
        let mut targets = Vec::new();
        for workspace in &self.state.workspaces {
            for tab in &workspace.tabs {
                for (pane_id, pane) in &tab.panes {
                    let terminal_id = &pane.attached_terminal_id;
                    let Some(terminal) = self.state.terminals.get(terminal_id) else {
                        continue;
                    };
                    let episode = self.stall_nudge_episodes.get(terminal_id);
                    let agent = terminal.effective_known_agent();
                    let quiet_for = pane.activity.inactive_for(now);
                    let nudges_sent = episode.map_or(0, |episode| episode.nudges_sent);
                    let next_nudge_at = episode.and_then(|episode| episode.next_nudge_at);
                    let runtime = self.terminal_runtimes.get(terminal_id);
                    let declaration_kind = if terminal.declares_running_subagents() {
                        "running_subagents"
                    } else {
                        "agent_status"
                    };
                    let mut facts = AutoNudgeFacts {
                        enabled: self.state.auto_nudge_stalled_agents,
                        message_present: !self.state.stall_nudge_message.trim().is_empty(),
                        settled: pane.settled_at.is_some(),
                        supervisor_stale: terminal.supervisor_stale,
                        quiet_for,
                        nudge_after: self.state.nudge_after,
                        blocked: terminal.state == crate::detect::AgentState::Blocked,
                        has_closing_gates: !terminal.closing_gates.is_empty(),
                        human_draft: self
                            .state
                            .pending_human_drafts
                            .get(pane_id)
                            .is_some_and(|draft| !draft.is_empty()),
                        resume_pending: terminal.pending_agent_resume_plan.is_some(),
                        launch_pending: terminal.managed_agent_launch_pending(),
                        detected_agent: agent.is_some(),
                        // The pure decision first proves every cheap gate. Only a
                        // nudge that is due earns a live process-tree inspection.
                        runtime_hosts_agent: agent.is_some() && runtime.is_some(),
                        nudges_sent,
                        max_nudges: self.state.max_nudges,
                        next_nudge_at,
                        now,
                    };
                    let mut decision = auto_nudge_decision(&facts);
                    if verify_runtime_host && decision == AutoNudgeDecision::Nudge {
                        facts.runtime_hosts_agent = agent.is_some_and(|agent| {
                            runtime.is_some_and(|runtime| {
                                super::agents::runtime_hosts_agent(runtime, agent)
                            })
                        });
                        decision = auto_nudge_decision(&facts);
                    }
                    targets.push(AutoNudgeTarget {
                        pane_id: *pane_id,
                        terminal_id: terminal_id.clone(),
                        declaration_kind,
                        quiet_for,
                        decision,
                    });
                }
            }
        }
        targets
    }

    fn prune_stall_nudge_episodes(&mut self, now: Instant) {
        let drop_ids = self
            .stall_nudge_episodes
            .keys()
            .filter_map(|terminal_id| match self.state.terminals.get(terminal_id) {
                None => Some((terminal_id.clone(), "the terminal is gone")),
                Some(terminal) if !terminal.supervisor_stale => Some((
                    terminal_id.clone(),
                    "a fresh status report cleared the stale mark",
                )),
                Some(_) => None,
            })
            .collect::<Vec<_>>();
        for (terminal_id, reason) in drop_ids {
            let Some(episode) = self.stall_nudge_episodes.remove(&terminal_id) else {
                continue;
            };
            let quiet_for = self
                .find_pane(episode.pane_id)
                .map_or(Duration::ZERO, |(_, pane)| pane.activity.inactive_for(now));
            tracing::debug!(
                pane = episode.pane_id.raw(),
                terminal = %terminal_id,
                declaration = episode.declaration_kind,
                quiet_seconds = quiet_for.as_secs(),
                reason,
                "dropping stalled-agent nudge candidate"
            );
        }
    }

    fn clear_stall_nudge_episodes(&mut self, now: Instant, reason: &'static str) {
        let episodes = std::mem::take(&mut self.stall_nudge_episodes);
        for (terminal_id, episode) in episodes {
            let quiet_for = self
                .find_pane(episode.pane_id)
                .map_or(Duration::ZERO, |(_, pane)| pane.activity.inactive_for(now));
            tracing::debug!(
                pane = episode.pane_id.raw(),
                terminal = %terminal_id,
                declaration = episode.declaration_kind,
                quiet_seconds = quiet_for.as_secs(),
                reason,
                "dropping stalled-agent nudge candidate"
            );
        }
    }

    fn drop_stall_nudge_episode(
        &mut self,
        terminal_id: &crate::terminal::TerminalId,
        pane_id: crate::layout::PaneId,
        declaration_kind: &'static str,
        quiet_for: Duration,
        reason: &'static str,
    ) {
        if self.stall_nudge_episodes.remove(terminal_id).is_none() {
            return;
        }
        tracing::debug!(
            pane = pane_id.raw(),
            terminal = %terminal_id,
            declaration = declaration_kind,
            quiet_seconds = quiet_for.as_secs(),
            reason,
            "dropping stalled-agent nudge candidate"
        );
    }

    fn send_stall_nudge(&mut self, target: &AutoNudgeTarget, now: Instant) -> bool {
        let message = self.state.stall_nudge_message.clone();
        let Some(runtime) = self.terminal_runtimes.get(&target.terminal_id) else {
            return false;
        };
        let (text, enter) = crate::app::api_helpers::encode_api_submission_parts(runtime, &message);
        if let Err(err) = runtime.try_send_bytes(Bytes::from(text)) {
            tracing::warn!(
                pane = target.pane_id.raw(),
                terminal = %target.terminal_id,
                declaration = target.declaration_kind,
                quiet_seconds = target.quiet_for.as_secs(),
                err = %err,
                "failed to send stalled-agent nudge"
            );
            return false;
        }
        runtime.send_bytes_after(Bytes::from(enter), STALL_NUDGE_SUBMIT_DELAY);
        self.retire_blocked_hook_authority_for_pane(target.pane_id, now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Agent, AgentState};
    use crate::terminal::{TerminalId, TerminalRuntime};
    use crate::workspace::Workspace;

    fn ready_facts(now: Instant) -> AutoNudgeFacts {
        AutoNudgeFacts {
            enabled: true,
            message_present: true,
            settled: false,
            supervisor_stale: true,
            quiet_for: Duration::from_secs(20 * 60),
            nudge_after: Duration::from_secs(20 * 60),
            blocked: false,
            has_closing_gates: false,
            human_draft: false,
            resume_pending: false,
            launch_pending: false,
            detected_agent: true,
            runtime_hosts_agent: true,
            nudges_sent: 0,
            max_nudges: 3,
            next_nudge_at: None,
            now,
        }
    }

    #[test]
    fn a_quiet_stale_agent_is_an_auto_nudge_candidate() {
        let now = Instant::now();
        assert_eq!(
            auto_nudge_decision(&ready_facts(now)),
            AutoNudgeDecision::Nudge
        );
    }

    #[test]
    fn every_auto_nudge_safety_gate_rejects_the_candidate() {
        let now = Instant::now();
        let cases = [
            AutoNudgeFacts {
                enabled: false,
                ..ready_facts(now)
            },
            AutoNudgeFacts {
                message_present: false,
                ..ready_facts(now)
            },
            AutoNudgeFacts {
                settled: true,
                ..ready_facts(now)
            },
            AutoNudgeFacts {
                blocked: true,
                ..ready_facts(now)
            },
            AutoNudgeFacts {
                has_closing_gates: true,
                ..ready_facts(now)
            },
            AutoNudgeFacts {
                human_draft: true,
                ..ready_facts(now)
            },
            AutoNudgeFacts {
                resume_pending: true,
                ..ready_facts(now)
            },
            AutoNudgeFacts {
                launch_pending: true,
                ..ready_facts(now)
            },
            AutoNudgeFacts {
                detected_agent: false,
                ..ready_facts(now)
            },
            AutoNudgeFacts {
                runtime_hosts_agent: false,
                ..ready_facts(now)
            },
            AutoNudgeFacts {
                nudges_sent: 3,
                ..ready_facts(now)
            },
        ];

        for facts in cases {
            assert!(matches!(
                auto_nudge_decision(&facts),
                AutoNudgeDecision::Drop(_)
            ));
        }
    }

    #[test]
    fn a_fresh_report_resets_the_stall_episode() {
        let now = Instant::now();
        let facts = AutoNudgeFacts {
            supervisor_stale: false,
            nudges_sent: 2,
            ..ready_facts(now)
        };

        assert!(matches!(
            auto_nudge_decision(&facts),
            AutoNudgeDecision::Reset(_)
        ));
    }

    #[test]
    fn screen_activity_and_backoff_both_have_to_be_due() {
        let now = Instant::now();
        let quiet = AutoNudgeFacts {
            quiet_for: Duration::from_secs(19 * 60),
            ..ready_facts(now)
        };
        assert_eq!(
            auto_nudge_decision(&quiet),
            AutoNudgeDecision::RetryAt(now + Duration::from_secs(60))
        );

        let backoff_at = now + Duration::from_secs(40 * 60);
        let backing_off = AutoNudgeFacts {
            next_nudge_at: Some(backoff_at),
            ..ready_facts(now)
        };
        assert_eq!(
            auto_nudge_decision(&backing_off),
            AutoNudgeDecision::RetryAt(backoff_at)
        );
    }

    #[test]
    fn successful_nudges_back_off_and_stop_at_the_episode_cap() {
        let base = Duration::from_secs(20 * 60);
        assert_eq!(next_stall_nudge_delay(base, 1, 3), Some(base * 2));
        assert_eq!(next_stall_nudge_delay(base, 2, 3), Some(base * 4));
        assert_eq!(next_stall_nudge_delay(base, 3, 3), None);
    }

    fn app_with_stalled_pane(
        now: Instant,
    ) -> (
        App,
        crate::layout::PaneId,
        TerminalId,
        tokio::sync::mpsc::Receiver<bytes::Bytes>,
    ) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut config = crate::config::Config::default();
        config.session.auto_nudge_stalled_agents = true;
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());
        let mut workspace = Workspace::test_new("auto-nudge");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace
            .terminal_id(pane_id)
            .cloned()
            .expect("root pane terminal");
        workspace.tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane")
            .activity
            .set_last_at(now - app.state.nudge_after);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let terminal = app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal");
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.supervisor_stale = true;
        let (runtime, rx) =
            TerminalRuntime::test_with_channel_and_scrollback_bytes(80, 24, 1024, b"", 8);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        (app, pane_id, terminal_id, rx)
    }

    fn drain(rx: &mut tokio::sync::mpsc::Receiver<bytes::Bytes>) -> String {
        let mut out = String::new();
        while let Ok(bytes) = rx.try_recv() {
            out.push_str(&String::from_utf8_lossy(&bytes));
        }
        out
    }

    #[tokio::test]
    async fn auto_nudge_episode_sends_three_times_then_stops() {
        let now = Instant::now();
        let (mut app, _pane_id, terminal_id, mut rx) = app_with_stalled_pane(now);

        assert!(app.tick_auto_nudges(now));
        assert!(drain(&mut rx).contains("/status"));

        assert!(!app.tick_auto_nudges(now + Duration::from_secs(39 * 60)));
        assert_eq!(drain(&mut rx), "");
        assert!(app.tick_auto_nudges(now + Duration::from_secs(40 * 60)));
        assert!(drain(&mut rx).contains("/status"));

        assert!(app.tick_auto_nudges(now + Duration::from_secs(120 * 60)));
        assert!(drain(&mut rx).contains("/status"));
        assert!(!app.tick_auto_nudges(now + Duration::from_secs(1_000 * 60)));
        assert_eq!(drain(&mut rx), "");
        assert_eq!(app.stall_nudge_episodes[&terminal_id].nudges_sent, 3);
    }

    #[tokio::test]
    async fn fresh_status_report_resets_the_runtime_stall_episode() {
        let now = Instant::now();
        let (mut app, _pane_id, terminal_id, mut rx) = app_with_stalled_pane(now);
        assert!(app.tick_auto_nudges(now));
        assert!(drain(&mut rx).contains("/status"));
        assert!(app.stall_nudge_episodes.contains_key(&terminal_id));

        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal")
            .supervisor_stale = false;
        assert!(!app.tick_auto_nudges(now + Duration::from_secs(1)));
        assert!(!app.stall_nudge_episodes.contains_key(&terminal_id));
    }

    #[tokio::test]
    async fn a_human_draft_suppresses_runtime_auto_nudge_without_resetting_budget() {
        let now = Instant::now();
        let (mut app, pane_id, terminal_id, mut rx) = app_with_stalled_pane(now);
        app.state
            .pending_human_drafts
            .insert(pane_id, "half typed".into());

        assert!(!app.tick_auto_nudges(now));
        assert_eq!(drain(&mut rx), "");
        assert_eq!(app.stall_nudge_episodes[&terminal_id].nudges_sent, 0);
    }

    #[test]
    fn app_projects_and_reloads_stalled_agent_nudge_config() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut config = crate::config::Config::default();
        config.session.auto_nudge_stalled_agents = true;
        config.session.nudge_after_minutes = 12;
        config.session.max_nudges = 5;
        config.session.stall_nudge_message = "report".into();
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());
        assert!(app.state.auto_nudge_stalled_agents);
        assert_eq!(app.state.nudge_after, Duration::from_secs(12 * 60));
        assert_eq!(app.state.max_nudges, 5);
        assert_eq!(app.state.stall_nudge_message, "report");

        config.session.auto_nudge_stalled_agents = false;
        config.session.nudge_after_minutes = 7;
        config.session.max_nudges = 2;
        config.session.stall_nudge_message = "still working?".into();
        app.apply_live_config(&config, &[], &[], false);
        assert!(!app.state.auto_nudge_stalled_agents);
        assert_eq!(app.state.nudge_after, Duration::from_secs(7 * 60));
        assert_eq!(app.state.max_nudges, 2);
        assert_eq!(app.state.stall_nudge_message, "still working?");
    }
}
