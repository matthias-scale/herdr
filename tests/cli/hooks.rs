use super::harness::*;

fn run_claude_hook(action: &str, hook_input: &str) -> Option<serde_json::Value> {
    run_shell_hook(
        "src/integration/assets/claude/herdr-agent-state.sh",
        &[action],
        hook_input,
    )
}

fn run_codex_hook(action: &str, hook_input: &str) -> Option<serde_json::Value> {
    run_shell_hook(
        "src/integration/assets/codex/herdr-agent-state.sh",
        &[action],
        hook_input,
    )
}

fn run_turn_title_cli(
    provider: &str,
    hook_input: &str,
    envs: &[(&str, &str)],
) -> Vec<serde_json::Value> {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket_path = base.join("herdr.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            if request["method"] == "ping" {
                write_fake_pong(
                    &mut stream,
                    &request,
                    "fixture-compatible",
                    CURRENT_PROTOCOL,
                );
            } else {
                writeln!(
                    stream,
                    "{}",
                    serde_json::json!({"id": request["id"], "result": {"type": "ok"}})
                )
                .unwrap();
                stream.flush().unwrap();
            }
            requests.push(request);
        }
        requests
    });

    let mut command = Command::new(env!("CARGO_BIN_EXE_herdr"));
    command
        .args(["agent", "turn-title", "--provider", provider])
        .env("HERDR_ENV", "1")
        .env("HERDR_SOCKET_PATH", &socket_path)
        .env("HERDR_PANE_ID", "p_fixture")
        .env_remove("CODEX_THREAD_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        command.env(key, value);
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(hook_input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "turn-title failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());

    let requests = server.join().unwrap();
    cleanup_test_base(&base);
    requests
}

fn run_copilot_hook(hook_input: &str) -> Option<serde_json::Value> {
    run_shell_hook(
        "src/integration/assets/copilot/herdr-agent-state.sh",
        &[],
        hook_input,
    )
}

fn run_devin_hook(
    action: &str,
    hook_input: &str,
    envs: &[(&str, &str)],
) -> Option<serde_json::Value> {
    run_shell_hook_with_env(
        "src/integration/assets/devin/herdr-agent-state.sh",
        &[action],
        hook_input,
        envs,
    )
}

fn run_shell_hook(asset_path: &str, args: &[&str], hook_input: &str) -> Option<serde_json::Value> {
    run_shell_hook_with_env(asset_path, args, hook_input, &[])
}

fn run_shell_hook_with_env(
    asset_path: &str,
    args: &[&str],
    hook_input: &str,
    envs: &[(&str, &str)],
) -> Option<serde_json::Value> {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket_path = base.join("herdr.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();

    let server = thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_millis(700);
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut line = String::new();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    reader.read_line(&mut line).unwrap();
                    let _ = stream.write_all(br#"{"id":"test","result":{"type":"ok"}}"#);
                    let _ = stream.write_all(b"\n");
                    let _ = stream.flush();
                    return Some(line);
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("accept failed: {err}"),
            }
        }
        None
    });

    let hook_path = Path::new(env!("CARGO_MANIFEST_DIR")).join(asset_path);
    let mut command = Command::new("bash");
    command
        .arg(hook_path)
        .args(args)
        .env("HERDR_ENV", "1")
        .env("HERDR_SOCKET_PATH", &socket_path)
        .env("HERDR_PANE_ID", "p_test")
        .env_remove("CODEX_THREAD_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        command.env(key, value);
    }
    let mut child = command.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(hook_input.as_bytes()).unwrap();
    drop(stdin);

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "hook failed: status={:?} stderr={} stdout={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );

    let request = server.join().unwrap();
    cleanup_test_base(&base);
    request.map(|line| serde_json::from_str(&line).unwrap())
}

#[test]
fn claude_hook_ignores_state_actions() {
    let subagent_input = r#"{"hook_event_name":"Notification","agent_id":"agent-abc123","agent_type":"Explore","notification_type":"permission_prompt"}"#;

    assert!(run_claude_hook("working", subagent_input).is_none());
    assert!(run_claude_hook("blocked", subagent_input).is_none());
}

#[test]
fn claude_hook_ignores_subagent_completion_reports() {
    let subagent_input =
        r#"{"hook_event_name":"SubagentStop","agent_id":"agent-abc123","agent_type":"Explore"}"#;

    assert!(run_claude_hook("working", subagent_input).is_none());
    assert!(run_claude_hook("idle", subagent_input).is_none());
    assert!(run_claude_hook("release", subagent_input).is_none());
}

#[test]
fn claude_hook_keeps_parent_agent_type_only_blocked() {
    let request = run_claude_hook(
        "blocked",
        r#"{"hook_event_name":"PermissionRequest","agent_type":"Explore"}"#,
    );

    assert!(request.is_none());
}

#[test]
fn claude_hook_reports_session_id_from_stdin() {
    let request = run_claude_hook(
        "session",
        r#"{"hook_event_name":"SessionStart","session_id":"claude-session"}"#,
    )
    .expect("session start should report session identity");

    assert_eq!(request["method"], "pane.report_agent_session");
    assert_eq!(request["params"]["agent_session_id"], "claude-session");
    assert!(request["params"].get("state").is_none());
}

#[test]
fn codex_hook_reports_persisted_root_session_and_ignores_ephemeral_or_nested_sessions() {
    let request = run_codex_hook(
        "session",
        r#"{"hook_event_name":"SessionStart","session_id":"codex-session","transcript_path":"/tmp/codex-session.jsonl"}"#,
    )
    .expect("codex hook should report session identity");

    assert_eq!(request["method"], "pane.report_agent_session");
    assert_eq!(request["params"]["agent_session_id"], "codex-session");
    assert!(request["params"].get("state").is_none());

    let matching_request = run_shell_hook_with_env(
        "src/integration/assets/codex/herdr-agent-state.sh",
        &["session"],
        r#"{"hook_event_name":"SessionStart","session_id":"codex-session","transcript_path":"/tmp/codex-session.jsonl"}"#,
        &[("CODEX_THREAD_ID", "codex-session")],
    )
    .expect("matching inherited session should still report");
    assert_eq!(
        matching_request["params"]["agent_session_id"],
        "codex-session"
    );

    assert!(run_codex_hook(
        "session",
        r#"{"hook_event_name":"SessionStart","session_id":"side-session","transcript_path":null}"#,
    )
    .is_none());

    assert!(run_shell_hook_with_env(
        "src/integration/assets/codex/herdr-agent-state.sh",
        &["session"],
        r#"{"hook_event_name":"SessionStart","session_id":"nested-session","transcript_path":"/tmp/nested-session.jsonl"}"#,
        &[("CODEX_THREAD_ID", "parent-session")],
    )
    .is_none());
}

#[test]
fn claude_and_codex_turn_title_fixtures_reach_guarded_metadata() {
    for (provider, fixture, expected_session, expected_source, expected_title, envs) in [
        (
            "claude",
            include_str!("../fixtures/work-titles/claude-user-prompt-submit.json"),
            "fixture-claude-session",
            "herdr:claude",
            "Review Auth Migration Safety",
            Vec::new(),
        ),
        (
            "codex",
            include_str!("../fixtures/work-titles/codex-user-prompt-submit.json"),
            "fixture-codex-session",
            "herdr:codex",
            "Fix Billing Retry Regression",
            vec![("CODEX_THREAD_ID", "fixture-codex-session")],
        ),
        (
            "codex",
            include_str!("../fixtures/work-titles/codex-short-user-prompt-submit.json"),
            "fixture-codex-short-session",
            "herdr:codex",
            "Write Poem",
            vec![("CODEX_THREAD_ID", "fixture-codex-short-session")],
        ),
    ] {
        let requests = run_turn_title_cli(provider, fixture, &envs);
        assert_eq!(requests[0]["method"], "ping");
        assert_eq!(requests[1]["method"], "pane.report_agent_session");
        assert_eq!(requests[1]["params"]["agent_session_id"], expected_session);
        assert_eq!(requests[2]["method"], "pane.report_metadata");
        assert_eq!(requests[2]["params"]["agent_session_id"], expected_session);
        assert_eq!(requests[2]["params"]["applies_to_source"], expected_source);
        assert_eq!(requests[2]["params"]["title"], expected_title);
        assert_eq!(requests[1]["params"]["seq"], requests[2]["params"]["seq"]);
    }
}

#[test]
fn ac1_ac2_ac3_turn_hooks_forward_derived_work_context_without_asset_changes() {
    for (provider, fixture, envs, tickets, pr_url) in [
        (
            "claude",
            include_str!("../fixtures/work-titles/claude-work-context-user-prompt-submit.json"),
            Vec::new(),
            vec!["SCA-42"],
            "https://github.com/scalable-so/herdr/pull/17",
        ),
        (
            "codex",
            include_str!("../fixtures/work-titles/codex-work-context-user-prompt-submit.json"),
            vec![("CODEX_THREAD_ID", "fixture-codex-context-session")],
            vec!["MAT-7", "SCA-9"],
            "https://github.com/scalable-so/herdr/pull/21",
        ),
    ] {
        let requests = run_turn_title_cli(provider, fixture, &envs);
        assert_eq!(requests[2]["method"], "pane.report_metadata");
        assert_eq!(
            requests[2]["params"]["work_context"]["ticket_ids"],
            serde_json::json!(tickets)
        );
        assert_eq!(
            requests[2]["params"]["work_context"]["pr_urls"],
            serde_json::json!([pr_url])
        );
    }
}

#[test]
fn copilot_hook_reports_session_id_from_stdin() {
    let request = run_copilot_hook(
        r#"{"hook_event_name":"SessionStart","session_id":"copilot-session","source":"resume"}"#,
    )
    .expect("copilot session start should report session identity");

    assert_eq!(request["method"], "pane.report_agent_session");
    assert_eq!(request["params"]["agent"], "copilot");
    assert_eq!(request["params"]["agent_session_id"], "copilot-session");
    assert!(request["params"].get("state").is_none());

    let camel = run_copilot_hook(
        r#"{"sessionId":"copilot-camel-session","source":"new","initialPrompt":"run tests"}"#,
    )
    .expect("copilot camelCase session start should report session identity");

    assert_eq!(camel["method"], "pane.report_agent_session");
    assert_eq!(camel["params"]["agent_session_id"], "copilot-camel-session");
    assert!(camel["params"].get("state").is_none());
}

#[test]
fn copilot_hook_does_not_report_lifecycle_state() {
    for payload in [
        r#"{"hook_event_name":"UserPromptSubmit","session_id":"copilot-session","prompt":"run tests"}"#,
        r#"{"hook_event_name":"PreToolUse","session_id":"copilot-session","tool_name":"ask_user"}"#,
        r#"{"hook_event_name":"notification","session_id":"copilot-session","notification_type":"permission_prompt"}"#,
        r#"{"hook_event_name":"agentStop","session_id":"copilot-session","stop_reason":"end_turn"}"#,
        r#"{"hook_event_name":"SessionEnd","session_id":"copilot-session","reason":"user_exit"}"#,
    ] {
        assert!(
            run_copilot_hook(payload).is_none(),
            "copilot session-only hook should ignore lifecycle payload {payload}"
        );
    }
}

#[test]
fn devin_hook_ignores_prompt_session_list_fallback() {
    let request = run_devin_hook(
        "session",
        r#"{"hook_event_name":"UserPromptSubmit","prompt":"run tests"}"#,
        &[
            ("DEVIN_PROJECT_DIR", "/tmp/project"),
            (
                "HERDR_DEVIN_LIST_JSON",
                r#"[{"id":"older-session","working_directory":"/tmp/other"},{"id":"devin-session","working_directory":"/tmp/project"}]"#,
            ),
        ],
    );

    assert!(request.is_none());
}

#[test]
fn devin_hook_reports_session_id_from_stdin_without_state() {
    let request = run_devin_hook(
        "session",
        r#"{"hook_event_name":"SessionStart","session_id":"devin-session","source":"startup"}"#,
        &[("HERDR_DEVIN_LIST_JSON", r#"[{"id":"older-session"}]"#)],
    )
    .expect("devin session start should report session identity");

    assert_eq!(request["method"], "pane.report_agent_session");
    assert_eq!(request["params"]["agent"], "devin");
    assert_eq!(request["params"]["agent_session_id"], "devin-session");
    assert!(request["params"].get("state").is_none());
}

#[test]
fn devin_hook_prefers_hook_session_id_over_list() {
    let request = run_devin_hook(
        "session",
        r#"{"hook_event_name":"PreToolUse","sessionId":"fresh-session","tool_name":"exec"}"#,
        &[
            ("DEVIN_PROJECT_DIR", "/tmp/project"),
            (
                "HERDR_DEVIN_LIST_JSON",
                r#"[{"id":"older-session","working_directory":"/tmp/project"}]"#,
            ),
        ],
    )
    .expect("devin tool hook should report session identity");

    assert_eq!(request["method"], "pane.report_agent_session");
    assert_eq!(request["params"]["agent_session_id"], "fresh-session");
    assert!(request["params"].get("state").is_none());
}

#[test]
fn devin_hook_reports_tool_session_from_list_without_state() {
    let request = run_devin_hook(
        "session",
        r#"{"hook_event_name":"PreToolUse","tool_name":"exec"}"#,
        &[
            ("DEVIN_PROJECT_DIR", "/tmp/project"),
            (
                "HERDR_DEVIN_LIST_JSON",
                r#"[{"id":"older-session","working_directory":"/tmp/other"},{"id":"devin-session","working_directory":"/tmp/project"}]"#,
            ),
        ],
    )
    .expect("devin tool hook should report session identity");

    assert_eq!(request["method"], "pane.report_agent_session");
    assert_eq!(request["params"]["agent"], "devin");
    assert_eq!(request["params"]["agent_session_id"], "devin-session");
    assert!(request["params"].get("state").is_none());
}

#[test]
fn devin_hook_ignores_startup_session_list_fallback() {
    let request = run_devin_hook(
        "session",
        r#"{"hook_event_name":"SessionStart","source":"startup"}"#,
        &[
            ("DEVIN_PROJECT_DIR", "/tmp/project"),
            (
                "HERDR_DEVIN_LIST_JSON",
                r#"[{"id":"stale-session","working_directory":"/tmp/project"}]"#,
            ),
        ],
    );

    assert!(request.is_none());
}

#[test]
fn devin_hook_ignores_non_matching_session_list_entries() {
    let request = run_devin_hook(
        "session",
        r#"{"hook_event_name":"PreToolUse","tool_name":"exec"}"#,
        &[
            ("DEVIN_PROJECT_DIR", "/tmp/project"),
            (
                "HERDR_DEVIN_LIST_JSON",
                r#"[{"id":"other-session","working_directory":"/tmp/other"}]"#,
            ),
        ],
    );

    assert!(request.is_none());
}
