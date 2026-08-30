use std::time::Duration;

use bytes::Bytes;

use crate::api::schema::{
    AgentPromptParams, AgentRenameParams, AgentSendKeysParams, AgentStartParams, AgentTarget,
    PaneReadResult, ResponseResult,
};
use crate::app::App;

use super::responses::{encode_error, encode_error_body, encode_success};

const AGENT_PROMPT_SUBMIT_DELAY: Duration = Duration::from_millis(300);

impl App {
    pub(super) fn handle_agent_list(&mut self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::AgentList {
                agents: self.collect_agent_infos(),
            },
        )
    }

    pub(super) fn handle_agent_get(&mut self, id: String, target: AgentTarget) -> String {
        self.reconcile_managed_agent_target(&target.target);
        let agent = match self.agent_info_for_target(&target.target) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_focus(&mut self, id: String, target: AgentTarget) -> String {
        let agent = match self.focus_agent_target(&target.target) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_rename(&mut self, id: String, params: AgentRenameParams) -> String {
        let agent = match self.rename_agent_target(&params.target, params.name) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_rename_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_start(&mut self, id: String, params: AgentStartParams) -> String {
        let (agent, argv) = match self.start_agent(params) {
            Ok(started) => started,
            Err(err) => return encode_error_body(id, self.agent_start_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentStarted { agent, argv })
    }

    pub(super) fn handle_agent_prompt(&mut self, id: String, params: AgentPromptParams) -> String {
        if params.text.is_empty() {
            return encode_error(id, "empty_agent_prompt", "agent prompt must not be empty");
        }
        let resolved = match self.resolve_agent_target(&params.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
            .cloned()
        else {
            return agent_not_found(id, &params.target);
        };
        let Some(terminal) = self.state.terminals.get(&terminal_id) else {
            return agent_not_found(id, &params.target);
        };
        let Some(expected_agent) = terminal.effective_known_agent() else {
            return agent_not_ready(id, &params.target);
        };
        if terminal.managed_agent_launch_pending() {
            return agent_not_ready(id, &params.target);
        }
        let Some(runtime) = self.lookup_runtime_sender(resolved.ws_idx, resolved.pane_id) else {
            return agent_not_found(id, &params.target);
        };
        if !super::super::agents::runtime_hosts_agent(runtime, expected_agent) {
            return encode_error(
                id,
                "agent_not_ready",
                format!(
                    "agent {} is no longer the pane foreground process",
                    params.target
                ),
            );
        }
        let (text, enter) =
            crate::app::api_helpers::encode_api_submission_parts(runtime, &params.text);
        if let Err(err) = runtime.try_send_bytes(Bytes::from(text)) {
            return encode_error(id, "agent_prompt_failed", err.to_string());
        }
        runtime.send_bytes_after(Bytes::from(enter), AGENT_PROMPT_SUBMIT_DELAY);
        self.retire_blocked_hook_authority_for_pane(resolved.pane_id, std::time::Instant::now());
        let Some(agent) = self.agent_info(resolved.ws_idx, resolved.pane_id) else {
            return agent_not_found(id, &params.target);
        };
        encode_success(id, ResponseResult::AgentPrompted { agent })
    }

    pub(super) fn handle_agent_read(
        &mut self,
        id: String,
        params: crate::api::schema::AgentReadParams,
    ) -> String {
        let resolved = match self.resolve_agent_target(&params.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some((pane, workspace_id)) = self.lookup_runtime(resolved.ws_idx, resolved.pane_id)
        else {
            return agent_not_found(id, &params.target);
        };
        let snapshot = crate::app::api_helpers::read_terminal_snapshot(
            pane,
            params.source,
            params.format,
            params.lines,
        );

        encode_success(
            id,
            ResponseResult::PaneRead {
                read: PaneReadResult {
                    pane_id: self
                        .public_pane_id(resolved.ws_idx, resolved.pane_id)
                        .unwrap_or_else(|| params.target.clone()),
                    workspace_id,
                    tab_id: self
                        .public_tab_id(resolved.ws_idx, resolved.tab_idx)
                        .unwrap(),
                    source: params.source,
                    format: params.format,
                    text: snapshot.text,
                    revision: pane.content_revision(),
                    truncated: snapshot.truncated,
                },
            },
        )
    }

    pub(super) fn handle_agent_explain(&mut self, id: String, target: AgentTarget) -> String {
        let resolved = match self.resolve_agent_target(&target.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some((pane, _workspace_id)) = self.lookup_runtime(resolved.ws_idx, resolved.pane_id)
        else {
            return agent_not_found(id, &target.target);
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
        else {
            return agent_not_found(id, &target.target);
        };
        let Some(terminal) = self.state.terminals.get(terminal_id) else {
            return agent_not_found(id, &target.target);
        };
        let Some(agent) = terminal.effective_known_agent().or(terminal.detected_agent) else {
            return encode_error(
                id,
                "agent_explain_unavailable",
                format!(
                    "agent target {} does not have a detected agent label",
                    target.target
                ),
            );
        };

        let screen = pane.detection_text();
        let osc_title = pane.agent_osc_title();
        let osc_progress = pane.agent_osc_progress();
        let explain = crate::detect::manifest::explain_with_input(
            agent,
            crate::detect::manifest::DetectionInput {
                screen: &screen,
                osc_title: &osc_title,
                osc_progress: &osc_progress,
            },
        );
        let mut value = crate::detect::manifest::explain_to_json_value(&explain);
        if let Some(object) = value.as_object_mut() {
            let screen_state = crate::detect::manifest::agent_state_label(explain.state);
            let effective_state = terminal.state;
            let arbitration = terminal.effective_state_arbitration();
            object.insert("screen_state".into(), serde_json::json!(screen_state));
            object.insert(
                "state".into(),
                serde_json::json!(crate::detect::manifest::agent_state_label(effective_state)),
            );
            object.insert(
                "effective_state".into(),
                serde_json::json!(crate::detect::manifest::agent_state_label(effective_state)),
            );
            object.insert("arbitration".into(), serde_json::json!(arbitration));
            object.insert("screen_detection_skipped".into(), serde_json::json!(false));
            object.insert(
                "screen_detection_skip_reason".into(),
                serde_json::Value::Null,
            );
        }

        encode_success(id, ResponseResult::AgentExplain { explain: value })
    }

    pub(super) fn handle_agent_send_keys(
        &mut self,
        id: String,
        params: AgentSendKeysParams,
    ) -> String {
        let resolved = match self.resolve_agent_target(&params.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
        else {
            return agent_not_found(id, &params.target);
        };
        let Some(expected_agent) = self
            .state
            .terminals
            .get(terminal_id)
            .and_then(|terminal| terminal.effective_known_agent())
        else {
            return agent_not_ready(id, &params.target);
        };
        let Some(runtime) = self.lookup_runtime_sender(resolved.ws_idx, resolved.pane_id) else {
            return agent_not_found(id, &params.target);
        };
        if !super::super::agents::runtime_hosts_agent(runtime, expected_agent) {
            return agent_not_ready(id, &params.target);
        }
        let encoded = match super::super::api_helpers::encode_api_keys(runtime, &params.keys) {
            Ok(encoded) => encoded,
            Err(key) => {
                return encode_error(id, "invalid_key", format!("unsupported key {key}"));
            }
        };
        let bytes: Vec<u8> = encoded.into_iter().flatten().collect();
        let has_bytes = !bytes.is_empty();
        if let Err(err) = runtime.try_send_bytes(Bytes::from(bytes)) {
            return encode_error(id, "agent_send_keys_failed", err.to_string());
        }
        if has_bytes {
            self.retire_blocked_hook_authority_for_pane(
                resolved.pane_id,
                std::time::Instant::now(),
            );
        }

        encode_success(id, ResponseResult::Ok {})
    }
}

fn agent_not_ready(id: String, target: &str) -> String {
    encode_error(
        id,
        "agent_not_ready",
        format!("agent {target} is not an active named agent"),
    )
}

fn agent_not_found(id: String, target: &str) -> String {
    encode_error(
        id,
        "agent_not_found",
        format!("agent target {target} not found"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::schema::{AgentStatus, SuccessResponse},
        app::Mode,
        config::Config,
        detect::{Agent, AgentState},
        workspace::Workspace,
    };

    fn app_with_agent() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("agent")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app
    }

    #[tokio::test(flavor = "current_thread")]
    async fn visible_blocker_overrides_fresh_hook_authority_in_explain_api() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let screen_observed_at = std::time::Instant::now();
        let hook_reported_at = screen_observed_at + std::time::Duration::from_secs(1);
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Blocked,
            true,
            false,
            false,
            false,
            false,
            screen_observed_at,
        );
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
            Some(1),
            hook_reported_at,
        );
        let screen = include_bytes!(
            "../../../tests/fixtures/agent-detection/claude-native-bash-permission-20260825.txt"
        );
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, screen);
        app.state.insert_test_runtime(pane_id, runtime);
        let target = app.public_pane_id(0, pane_id).unwrap();

        let response = app.handle_agent_explain("explain".into(), AgentTarget { target });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentExplain { explain } = success.result else {
            panic!("expected agent explain response");
        };
        assert_eq!(explain["screen_detection_skipped"], false);
        assert_eq!(explain["matched_rule"]["id"], "generic_permission_prompt");
        assert_eq!(explain["screen_state"], "blocked");
        assert_eq!(explain["effective_state"], "working");
        assert_eq!(explain["arbitration"], "closing_block_report");
        assert_eq!(app.state.terminals[&terminal_id].state, AgentState::Working);
        assert_eq!(
            app.agent_info(0, pane_id).unwrap().agent_status,
            AgentStatus::Working
        );

        app.handle_internal_event(crate::events::AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Claude),
            state: AgentState::Blocked,
            visible_blocker: true,
            visible_working: false,
            usage_limited: false,
            process_exited: false,
            observed_at: hook_reported_at + std::time::Duration::from_secs(1),
        });
        let response = app.handle_agent_explain(
            "newer-blocker".into(),
            AgentTarget {
                target: app.public_pane_id(0, pane_id).unwrap(),
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentExplain { explain } = success.result else {
            panic!("expected newer blocker explain response");
        };
        assert_eq!(explain["effective_state"], "blocked");
        assert_eq!(explain["arbitration"], "visible_blocker_over_hook");
        assert_eq!(
            app.agent_info(0, pane_id).unwrap().agent_status,
            AgentStatus::Blocked
        );

        let mut cleared_prompt = b"\x1b[3J\x1b[2J\x1b[H".to_vec();
        cleared_prompt.extend_from_slice(include_bytes!(
            "../../../tests/fixtures/agent-detection/claude-empty-prompt-ub1-wM-pJ-20260825.txt"
        ));
        app.state.workspaces[0]
            .test_runtimes
            .get(&pane_id)
            .unwrap()
            .test_process_pty_bytes(&cleared_prompt);
        app.handle_internal_event(crate::events::AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Claude),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            usage_limited: false,
            process_exited: false,
            observed_at: hook_reported_at + std::time::Duration::from_secs(2),
        });
        let response = app.handle_agent_explain(
            "panel-removed".into(),
            AgentTarget {
                target: app.public_pane_id(0, pane_id).unwrap(),
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentExplain { explain } = success.result else {
            panic!("expected panel-removed explain response");
        };
        assert_eq!(explain["screen_state"], "idle");
        assert_eq!(explain["effective_state"], "idle");
        assert_eq!(explain["arbitration"], "screen");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn explain_api_reports_screen_and_foreground_working_owners() {
        for (case, visible_working, foreground_process, expected_owner) in [
            ("visible working", true, false, "screen"),
            ("detected working", false, false, "screen"),
            ("foreground process", false, true, "foreground_process"),
        ] {
            let mut app = app_with_agent();
            let pane_id = app.state.workspaces[0].tabs[0].root_pane;
            let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let now = std::time::Instant::now();
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
            terminal.set_hook_authority_at(
                "herdr:claude".into(),
                "claude".into(),
                AgentState::Idle,
                None,
                None,
                None,
                now,
            );
            if foreground_process {
                terminal.set_foreground_process(
                    Some("cargo".into()),
                    true,
                    now + std::time::Duration::from_millis(1),
                );
            } else {
                terminal.set_detected_state_with_screen_signals_at(
                    Some(Agent::Claude),
                    AgentState::Working,
                    false,
                    false,
                    visible_working,
                    false,
                    false,
                    now + crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH,
                );
            }
            let screen = include_bytes!(
                "../../../tests/fixtures/agent-detection/claude-empty-prompt-ub1-wM-pJ-20260825.txt"
            );
            app.state.insert_test_runtime(
                pane_id,
                crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, screen),
            );

            let response = app.handle_agent_explain(
                case.into(),
                AgentTarget {
                    target: app.public_pane_id(0, pane_id).unwrap(),
                },
            );
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            let ResponseResult::AgentExplain { explain } = success.result else {
                panic!("expected {case} explain response");
            };
            assert_eq!(explain["effective_state"], "working", "{case}");
            assert_eq!(explain["arbitration"], expected_owner, "{case}");
            assert_eq!(
                app.agent_info(0, pane_id).unwrap().agent_status,
                AgentStatus::Working,
                "{case}"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unmatched_output_retires_hook_authority_rebaselines_same_state_report() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Idle);
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"");
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);

        for seq in [1, 2] {
            assert_eq!(
                app.handle_internal_event(crate::events::AppEvent::HookStateReported {
                    pane_id,
                    source: "herdr:claude-closing-block".into(),
                    agent_label: "claude".into(),
                    state: AgentState::Blocked,
                    message: None,
                    seq: Some(seq),
                    wait: None,
                    eta_s: None,
                    reported_at: None,
                    session_ref: None,
                }),
                Some(true)
            );
            if seq == 1 {
                app.terminal_runtimes
                    .get(&terminal_id)
                    .unwrap()
                    .test_mark_detection_content_changed();
            }
        }
        assert_eq!(
            app.terminal_runtimes
                .get(&terminal_id)
                .unwrap()
                .hook_authority_output_baseline_for_test(),
            1,
            "an accepted same-state report rearms from current content"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn closing_block_authority_is_limited_to_live_blocked_gates_quiet_report_wakes_screen() {
        for report_state in [AgentState::Working, AgentState::Idle, AgentState::Unknown] {
            let mut app = app_with_agent();
            let pane_id = app.state.workspaces[0].tabs[0].root_pane;
            let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .set_detected_state(Some(Agent::Claude), AgentState::Idle);
            let screen = include_bytes!(
                "../../../tests/fixtures/agent-detection/claude-empty-prompt-ub1-wM-pJ-20260825.txt"
            );
            let (runtime, mut detection_events) =
                crate::terminal::TerminalRuntime::test_with_live_detection_screen_bytes(
                    pane_id,
                    Agent::Claude,
                    AgentState::Idle,
                    true,
                    false,
                    false,
                    screen,
                );
            app.terminal_runtimes.insert(terminal_id, runtime);
            app.state.workspaces[0].tabs[0]
                .panes
                .get_mut(&pane_id)
                .unwrap()
                .seen = false;
            app.state.active = None;

            assert_eq!(
                app.handle_internal_event(crate::events::AppEvent::HookStateReported {
                    pane_id,
                    source: "herdr:claude-closing-block".into(),
                    agent_label: "claude".into(),
                    state: report_state,
                    message: None,
                    seq: Some(1),
                    wait: None,
                    eta_s: None,
                    reported_at: None,
                    session_ref: None,
                }),
                Some(true)
            );

            let event = tokio::time::timeout(std::time::Duration::from_secs(1), async {
                loop {
                    let event = detection_events.recv().await.expect("detector event");
                    if matches!(event, crate::events::AppEvent::StateChanged { .. }) {
                        break event;
                    }
                }
            })
            .await
            .unwrap_or_else(|_| {
                panic!("quiet {report_state:?} closing report must force a real screen scan")
            });
            let crate::events::AppEvent::StateChanged { state, .. } = &event else {
                unreachable!()
            };
            assert_eq!(*state, AgentState::Idle, "no synthetic transition");
            app.handle_internal_event(event);
            assert_eq!(
                app.agent_info(0, pane_id).unwrap().agent_status,
                AgentStatus::Done,
                "hidden pane reaches Done only from the manifest-confirmed idle screen"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn closing_block_authority_is_limited_to_live_blocked_gates_explain_matches_effective_state(
    ) {
        for (report_state, expected_status, expected_label) in [
            (AgentState::Working, AgentStatus::Working, "working"),
            (AgentState::Idle, AgentStatus::Idle, "idle"),
            (AgentState::Unknown, AgentStatus::Unknown, "unknown"),
        ] {
            let mut app = app_with_agent();
            let pane_id = app.state.workspaces[0].tabs[0].root_pane;
            let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .set_detected_state(Some(Agent::Claude), AgentState::Idle);
            let screen = include_bytes!(
                "../../../tests/fixtures/agent-detection/claude-empty-prompt-ub1-wM-pJ-20260825.txt"
            );
            app.terminal_runtimes.insert(
                terminal_id,
                crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, screen),
            );
            app.handle_internal_event(crate::events::AppEvent::HookStateReported {
                pane_id,
                source: "herdr:claude-closing-block".into(),
                agent_label: "claude".into(),
                state: report_state,
                message: None,
                seq: Some(1),
                wait: None,
                eta_s: None,
                reported_at: None,
                session_ref: None,
            });
            let target = app.public_pane_id(0, pane_id).unwrap();

            let info = app.agent_info(0, pane_id).unwrap();
            let response = app.handle_agent_explain(
                "before-refresh".into(),
                AgentTarget {
                    target: target.clone(),
                },
            );
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            let ResponseResult::AgentExplain { explain } = success.result else {
                panic!("expected agent explain response");
            };
            assert_eq!(info.agent_status, expected_status, "{report_state:?}");
            assert_eq!(explain["effective_state"], expected_label);
            assert_eq!(explain["arbitration"], "closing_block_report");

            app.handle_internal_event(crate::events::AppEvent::StateChanged {
                pane_id,
                agent: Some(Agent::Claude),
                state: AgentState::Idle,
                visible_blocker: false,
                visible_working: false,
                usage_limited: false,
                process_exited: false,
                observed_at: std::time::Instant::now(),
            });
            let info = app.agent_info(0, pane_id).unwrap();
            let response = app.handle_agent_explain("after-refresh".into(), AgentTarget { target });
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            let ResponseResult::AgentExplain { explain } = success.result else {
                panic!("expected refreshed agent explain response");
            };
            assert_eq!(info.agent_status, AgentStatus::Idle);
            assert_eq!(explain["effective_state"], "idle");
            assert_eq!(explain["arbitration"], "screen");
        }
    }

    #[test]
    fn fresh_hook_state_wins_over_non_blocker_screen_done_projection() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(Some(Agent::Kimi), AgentState::Idle);
        let session_ref = crate::agent_resume::AgentSessionRef::id("done-projection").unwrap();
        terminal.set_agent_session_ref_for_session_start(
            "herdr:kimi".into(),
            "kimi".into(),
            Some(session_ref.clone()),
            Some(1),
            Some("startup".into()),
        );
        let reported_at = std::time::Instant::now();
        terminal.set_hook_authority_at(
            "herdr:kimi".into(),
            "kimi".into(),
            AgentState::Idle,
            None,
            Some(session_ref),
            Some(2),
            reported_at,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Kimi),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            reported_at + crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH,
        );
        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .unwrap()
            .seen = false;

        let info = app.agent_info(0, pane_id).unwrap();
        assert_eq!(info.agent_status, AgentStatus::Done);
        assert!(!info.screen_detection_skipped);
    }

    #[tokio::test]
    async fn agent_prompt_sends_text_then_delays_enter() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Working);
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 0, b"", 1,
            );
        runtime.test_process_pty_bytes(b"\x1b[?2004h");
        app.state.insert_test_runtime(pane_id, runtime);

        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();
        let bracketed_started = std::time::Instant::now();
        let response = app.handle_agent_prompt(
            "req".into(),
            AgentPromptParams {
                target: public_pane_id,
                text: "A != B".into(),
                wait: None,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentPrompted { agent, .. } = success.result else {
            panic!("expected prompted response");
        };
        assert_eq!(agent.name.as_deref(), Some("reviewer"));
        assert_eq!(
            rx.try_recv().unwrap(),
            Bytes::from_static(b"\x1b[200~A != B\x1b[201~")
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .unwrap()
                .unwrap(),
            Bytes::from_static(b"\r")
        );
        assert!(bracketed_started.elapsed() >= AGENT_PROMPT_SUBMIT_DELAY);

        app.lookup_runtime_sender(0, pane_id)
            .unwrap()
            .test_process_pty_bytes(b"\x1b[?2004l");
        let raw_started = std::time::Instant::now();
        let raw = app.handle_agent_prompt(
            "req-raw".into(),
            AgentPromptParams {
                target: "reviewer".into(),
                text: "A != B".into(),
                wait: None,
            },
        );
        let raw: SuccessResponse = serde_json::from_str(&raw).unwrap();
        assert!(matches!(raw.result, ResponseResult::AgentPrompted { .. }));
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"A != B"));
        assert!(rx.try_recv().is_err());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .unwrap()
                .unwrap(),
            Bytes::from_static(b"\r")
        );
        assert!(raw_started.elapsed() >= AGENT_PROMPT_SUBMIT_DELAY);

        let rejected = app.handle_agent_prompt(
            "req-label".into(),
            AgentPromptParams {
                target: "opencode".into(),
                text: "wrong target".into(),
                wait: None,
            },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&rejected).unwrap();
        assert_eq!(error.error.code, "agent_not_found");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn agent_send_keys_validates_every_key_before_writing() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        let rejected = app.handle_agent_send_keys(
            "req-invalid".into(),
            AgentSendKeysParams {
                target: "reviewer".into(),
                keys: vec!["enter".into(), "not-a-key".into()],
            },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&rejected).unwrap();
        assert_eq!(error.error.code, "invalid_key");
        assert!(rx.try_recv().is_err());

        let sent = app.handle_agent_send_keys(
            "req-valid".into(),
            AgentSendKeysParams {
                target: "reviewer".into(),
                keys: vec!["up".into(), "enter".into()],
            },
        );
        let success: SuccessResponse = serde_json::from_str(&sent).unwrap();
        assert!(matches!(success.result, ResponseResult::Ok {}));
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"\x1b[A\r"));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_prompt_retires_blocked_hook_authority_after_forwarding() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Blocked,
            None,
            Some(1),
        );
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        let response = app.handle_agent_prompt(
            "req".into(),
            AgentPromptParams {
                target: "reviewer".into(),
                text: "continue".into(),
                wait: None,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();

        assert!(matches!(
            success.result,
            ResponseResult::AgentPrompted { .. }
        ));
        assert!(rx.try_recv().is_ok());
        assert_eq!(app.state.terminals[&terminal_id].state, AgentState::Idle);
        assert!(!app.state.terminals[&terminal_id].full_lifecycle_hook_authority_active());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_send_keys_retires_blocked_hook_authority_after_forwarding() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Blocked,
            None,
            Some(1),
        );
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        let response = app.handle_agent_send_keys(
            "req".into(),
            AgentSendKeysParams {
                target: "reviewer".into(),
                keys: vec!["enter".into()],
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();

        assert!(matches!(success.result, ResponseResult::Ok {}));
        assert!(rx.try_recv().is_ok());
        assert_eq!(app.state.terminals[&terminal_id].state, AgentState::Idle);
        assert!(!app.state.terminals[&terminal_id].full_lifecycle_hook_authority_active());
    }

    #[tokio::test]
    async fn agent_prompt_rejects_managed_agent_while_startup_is_pending() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        let now = std::time::Instant::now();
        terminal.begin_managed_agent(
            "reviewer".into(),
            Agent::OpenCode,
            now,
            std::time::Duration::from_secs(3),
            std::time::Duration::from_secs(10),
        );
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        let response = app.handle_agent_prompt(
            "req-pending".into(),
            AgentPromptParams {
                target: "reviewer".into(),
                text: "A != B".into(),
                wait: None,
            },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "agent_not_ready");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn agent_focus_marks_already_focused_done_agent_seen() {
        let mut app = app_with_agent();
        app.state.outer_terminal_focus = Some(false);

        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Pi), AgentState::Idle);
        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .unwrap()
            .seen = false;
        app.state.workspaces[0].tabs[0].layout.focus_pane(pane_id);

        let response = app.handle_agent_focus(
            "req".into(),
            AgentTarget {
                target: app.public_pane_id(0, pane_id).unwrap(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentInfo { agent } = success.result else {
            panic!("expected agent info response");
        };
        assert_eq!(agent.agent_status, AgentStatus::Idle);
    }

    #[test]
    fn agent_rename_does_not_replace_the_pane_label() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_manual_label("shell-pane".into());
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        let target = app.public_pane_id(0, pane_id).unwrap();

        for name in [Some("reviewer".to_string()), None] {
            let response = app.handle_agent_rename(
                "req".into(),
                AgentRenameParams {
                    target: target.clone(),
                    name,
                },
            );
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
            assert_eq!(
                app.state.terminals[&terminal_id].manual_label.as_deref(),
                Some("shell-pane")
            );
        }
    }
}
