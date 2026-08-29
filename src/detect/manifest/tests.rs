use super::*;

fn remote_manifest(version: &str, state: &str, contains: &str) -> String {
    format!(
        r#"
id = "codex"
version = "{version}"
min_engine_version = 1
updated_at = "2026-06-10T12:00:00Z"

[[rules]]
id = "test"
state = "{state}"
contains = ["{contains}"]
"#
    )
}

fn local_manifest(state: &str, contains: &str) -> String {
    format!(
        r#"
id = "codex"

[[rules]]
id = "test"
state = "{state}"
contains = ["{contains}"]
"#
    )
}

fn rules_manifest(rules: &str) -> String {
    format!(
        r#"
id = "codex"

{rules}
"#
    )
}

fn with_manifest_dirs<T>(name: &str, f: impl FnOnce() -> T) -> T {
    with_bundled_manifests(name, f)
}

/// `explain` against the bundled manifests only. See `with_bundled_manifests`:
/// without it these assertions read whatever manifest the developer's machine
/// last downloaded.
fn bundled_explain(agent: Agent, screen: &str) -> DetectionExplain {
    with_bundled_manifests("bundled-explain", || explain(agent, screen))
}

fn write_remote_codex(content: &str) {
    let path = crate::detect::manifest_update::remote_manifest_path(Agent::Codex);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
    reload_manifests();
}

fn write_remote_codex_without_reload(content: &str) {
    let path = crate::detect::manifest_update::remote_manifest_path(Agent::Codex);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn write_local_codex(content: &str) {
    let path = override_path(Agent::Codex).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
    reload_manifests();
}

#[test]
fn known_agent_no_match_defaults_to_idle_fallback() {
    let explain = bundled_explain(Agent::Codex, "ordinary prompt text");

    assert_eq!(explain.state, AgentState::Idle);
    assert!(!explain.visible_idle);
    assert_eq!(
        explain.fallback_reason.as_deref(),
        Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
    );
}

#[test]
fn rule_semantics_apply_gates_priority_and_line_regex() {
    with_manifest_dirs("rule-semantics", || {
        write_local_codex(&rules_manifest(
            r#"
[[rules]]
id = "low_contains"
state = "idle"
priority = 1
contains = ["match"]

[[rules]]
id = "high_nested_gates"
state = "working"
priority = 10
contains = ["match"]
all = [
  { any = [{ regex = ["w[io]n"] }, { contains = ["fallback"] }] },
]
not = [
  { contains = ["blocked"] },
]

[[rules]]
id = "line_regex"
state = "blocked"
priority = 20
line_regex = ["^exact line$"]
"#,
        ));

        let high = explain(Agent::Codex, "match win");
        assert_eq!(high.state, AgentState::Working);
        assert_eq!(
            high.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("high_nested_gates")
        );

        let not_gate = explain(Agent::Codex, "match win blocked");
        assert_eq!(not_gate.state, AgentState::Idle);
        assert_eq!(
            not_gate.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("low_contains")
        );

        let line = explain(Agent::Codex, "before\nexact line\nafter");
        assert_eq!(line.state, AgentState::Blocked);
        assert_eq!(
            line.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("line_regex")
        );
    });
}

#[test]
fn remote_manifest_loads_between_local_override_and_bundled() {
    with_manifest_dirs("remote-source", || {
        write_remote_codex(&remote_manifest("9999.01.01.1", "blocked", "remote-ready"));

        let explain = explain(Agent::Codex, "remote-ready");

        assert_eq!(explain.state, AgentState::Blocked);
        assert!(matches!(
            explain.source,
            Some(ManifestSource::Remote { .. })
        ));
        assert_eq!(explain.manifest_version.as_deref(), Some("9999.01.01.1"));
        assert_eq!(
            explain.cached_remote_version.as_deref(),
            Some("9999.01.01.1")
        );
    });
}

#[test]
fn fallback_explain_preserves_active_manifest_version() {
    with_manifest_dirs("fallback-version", || {
        write_remote_codex(&remote_manifest("9999.01.01.1", "blocked", "remote-ready"));

        let explain = explain(Agent::Codex, "ordinary prompt text");

        assert_eq!(explain.state, AgentState::Idle);
        assert_eq!(
            explain.fallback_reason.as_deref(),
            Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
        );
        assert_eq!(explain.manifest_version.as_deref(), Some("9999.01.01.1"));
        assert!(matches!(
            explain.source,
            Some(ManifestSource::Remote { .. })
        ));
    });
}

#[test]
fn older_cached_remote_manifest_does_not_shadow_newer_bundled_manifest() {
    with_manifest_dirs("older-remote-bundled-fallback", || {
        write_remote_codex(&remote_manifest("2026.06.10.0", "blocked", "remote-ready"));

        let explain = explain(Agent::Codex, "remote-ready");

        assert_eq!(explain.state, AgentState::Idle);
        assert!(matches!(explain.source, Some(ManifestSource::Bundled)));
        assert_eq!(
            explain.cached_remote_version.as_deref(),
            Some("2026.06.10.0")
        );
        assert!(explain
            .warning
            .as_deref()
            .is_some_and(|warning| warning.contains("older than bundled")));
    });
}

#[test]
fn local_override_shadows_cached_remote_manifest() {
    with_manifest_dirs("local-shadows-remote", || {
        write_remote_codex(&remote_manifest("9999.01.01.1", "blocked", "remote-ready"));
        write_local_codex(&local_manifest("idle", "local-ready"));

        let explain = explain(Agent::Codex, "local-ready");

        assert_eq!(explain.state, AgentState::Idle);
        assert!(matches!(explain.source, Some(ManifestSource::Override(_))));
        assert!(explain.local_override_shadowing_remote);
        assert_eq!(
            explain.cached_remote_version.as_deref(),
            Some("9999.01.01.1")
        );
    });
}

#[test]
fn invalid_local_override_falls_back_to_cached_remote_manifest() {
    with_manifest_dirs("invalid-local-remote-fallback", || {
        write_remote_codex(&remote_manifest("9999.01.01.1", "blocked", "remote-ready"));
        write_local_codex("id = ");

        let explain = explain(Agent::Codex, "remote-ready");

        assert_eq!(explain.state, AgentState::Blocked);
        assert!(matches!(
            explain.source,
            Some(ManifestSource::Remote { .. })
        ));
        assert!(explain.warning.is_some());
    });
}

#[test]
fn detection_uses_cached_manifest_until_explicit_reload() {
    with_manifest_dirs("cache-boundary", || {
        write_remote_codex(&remote_manifest("9999.01.01.1", "blocked", "cached-ready"));

        let cached = explain(Agent::Codex, "cached-ready");
        assert_eq!(cached.state, AgentState::Blocked);
        assert!(matches!(cached.source, Some(ManifestSource::Remote { .. })));
        assert_eq!(
            cached.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("test")
        );

        write_remote_codex_without_reload(&remote_manifest("9999.01.01.2", "working", "new-ready"));

        let unchanged = explain(Agent::Codex, "new-ready");
        assert_eq!(unchanged.state, AgentState::Idle);
        assert_eq!(
            unchanged.fallback_reason.as_deref(),
            Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
        );
        assert_eq!(
            unchanged.cached_remote_version.as_deref(),
            Some("9999.01.01.1")
        );

        reload_manifests();

        let reloaded = explain(Agent::Codex, "new-ready");
        assert_eq!(reloaded.state, AgentState::Working);
        assert_eq!(
            reloaded.cached_remote_version.as_deref(),
            Some("9999.01.01.2")
        );
        assert_eq!(
            reloaded.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("test")
        );
    });
}

#[test]
fn all_bundled_manifests_parse_and_validate() {
    for agent in Agent::SCREEN_MANIFEST_AGENTS {
        assert!(
            bundled_manifest(agent).is_some(),
            "missing bundled manifest for {}",
            agent_label(agent)
        );
    }
}

#[test]
fn devin_manifest_detects_idle_working_and_blocked_states() {
    let idle = bundled_explain(
        Agent::Devin,
        "─────────────────────────────────────────────────────\n❭ Ask Devin to build features, fix bugs, or work on\n  your code\n─────────────────────────────────────────────────────\nSWE-1.6               Context: 16k / 200k tokens (7%)",
    );
    assert_eq!(idle.state, AgentState::Idle);
    assert!(idle.visible_idle);

    let live_footer_idle = bundled_explain(
        Agent::Devin,
        "Done.\n\n────────────────────────────────────────────────── (bypass permissions on) ─\n❭\n────────────────────────────────────────────────────────────────────────────\nClaude Opus 4.6 Thinking                                    Context: 38k / 200k tokens (18%)",
    );
    assert_eq!(live_footer_idle.state, AgentState::Idle);
    assert_eq!(
        live_footer_idle
            .matched_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("live_prompt_footer")
    );
    assert!(live_footer_idle.visible_idle);

    let welcome_footer_idle = bundled_explain(
        Agent::Devin,
        "⠀⠀⠀⠀⠀⣴⣾⣶⡄⠀⠀⠀⠀\n⠀⣴⣾⣶⡾⠛⠿⠟⠃⣴⣾⣶⡄  Devin CLI\n⠀⠛⠿⠟⠃⣴⣾⣶⡾⠛⠿⠟⠃  v2026.5.26-8\n⠀⣤⣶⣦⡄⠻⢿⠿⢷⣤⣶⣦⡄\n⠀⠻⢿⠿⢷⣤⣶⣦⡄⠻⢿⠿⠃  Hybrid\n⠀⠀⠀⠀⠀⠻⢿⠿⠃⠀⠀⠀⠀\n\n───────────────────────────\n❭ Ask Devin to build\n  features, fix bugs, or\n  work on your code\n───────────────────────────\nClaude Opus Looking for\n4.6 Thinkingplan mode? /\n            plan",
    );
    assert_eq!(welcome_footer_idle.state, AgentState::Idle);
    assert_eq!(
        welcome_footer_idle
            .matched_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("welcome_prompt_footer")
    );
    assert!(welcome_footer_idle.visible_idle);

    let working = bundled_explain(
        Agent::Devin,
        "◔ Reading shell 91b655\n  │ Timeout: 35s\n\n⠀⡆ Running tools · 27s (esc to interrupt)\n─────────────────────────────────────────────────────\n❭ Guide Devin while it works",
    );
    assert_eq!(working.state, AgentState::Working);
    assert!(working.visible_working);

    let trust_prompt = bundled_explain(
        Agent::Devin,
        "Do you trust the authors of this directory?\nFor security, devin should not be run in directories\nwith untrusted content.\n❭ 1 Yes, trust /private/tmp/devin-hook-probe\n· 2 No, exit",
    );
    assert_eq!(trust_prompt.state, AgentState::Blocked);
    assert!(trust_prompt.visible_blocker);

    let permission_prompt = bundled_explain(
        Agent::Devin,
        "⏺ Running command\n  └ $ sleep 30\n\n❭ 1 Yes  (Approve once)\n· 2 Yes, allow `sleep` commands\n· 3 Yes, always allow `sleep` commands\n· 4 No\n↑↓ select · ↵ confirm · esc cancel",
    );
    assert_eq!(permission_prompt.state, AgentState::Blocked);
    assert!(permission_prompt.visible_blocker);
}

#[test]
fn manifest_validation_rejects_unknown_fields_empty_rules_invalid_regions_and_regexes() {
    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "typo"
state = "working"
contain = ["Working"]
"#
    )
    .is_err());

    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "empty"
state = "working"
"#
    )
    .is_err());

    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_region"
state = "working"
region = "after_last_promt_marker"
contains = ["Working"]
"#
    )
    .is_err());

    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_regex"
state = "working"
regex = ["["]
"#
    )
    .is_err());

    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_nested_regex"
state = "working"
any = [{ line_regex = ["["] }]
"#
    )
    .is_err());
}

#[test]
fn manifest_validation_keeps_skip_rules_neutral() {
    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_skip_state"
state = "idle"
skip_state_update = true
contains = ["menu"]
"#
    )
    .is_err());

    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_skip_visible"
state = "unknown"
skip_state_update = true
visible_blocker = true
contains = ["menu"]
"#
    )
    .is_err());
}

#[test]
fn manifest_validation_rejects_excessive_rule_count() {
    let mut manifest = String::from(
        r#"
id = "codex"
"#,
    );
    for index in 0..129 {
        manifest.push_str(&format!(
            r#"
[[rules]]
id = "rule_{index}"
state = "idle"
contains = ["ready"]
"#
        ));
    }

    assert!(parse_manifest(&manifest).is_err());
}

#[test]
fn manifest_validation_rejects_excessive_gate_depth() {
    let manifest = r#"
id = "codex"

[[rules]]
id = "deep"
state = "idle"
contains = ["ready"]
all = [
  { contains = ["1"], all = [
    { contains = ["2"], all = [
      { contains = ["3"], all = [
        { contains = ["4"], all = [
          { contains = ["5"], all = [
            { contains = ["6"], all = [
              { contains = ["7"], all = [
                { contains = ["8"], all = [
                  { contains = ["9"] },
                ] },
              ] },
            ] },
          ] },
        ] },
      ] },
    ] },
  ] },
]
"#;

    assert!(parse_manifest(manifest).is_err());
}

#[test]
fn manifest_validation_rejects_excessive_matchers() {
    let matchers = (0..33)
        .map(|index| format!(r#""m{index}""#))
        .collect::<Vec<_>>()
        .join(", ");
    let manifest = format!(
        r#"
id = "codex"

[[rules]]
id = "many"
state = "idle"
contains = [{matchers}]
"#
    );

    assert!(parse_manifest(&manifest).is_err());
}

#[test]
fn bottom_non_empty_lines_uses_bottom_occurrence_for_repeated_text() {
    let content = "marker\nold\n\nmiddle\nmarker\nnew\n";

    assert_eq!(
        region(
            DetectionInput {
                screen: content,
                osc_title: "",
                osc_progress: "",
            },
            "bottom_non_empty_lines(2)"
        ),
        "marker\nnew\n"
    );
}

#[test]
fn top_non_empty_lines_uses_top_occurrence_for_repeated_text() {
    let content = "\nmarker\nold\n\nmiddle\nmarker\nnew\n";

    assert_eq!(
        region(
            DetectionInput {
                screen: content,
                osc_title: "",
                osc_progress: "",
            },
            "top_non_empty_lines(2)"
        ),
        "\nmarker\nold\n"
    );
}

#[test]
fn top_non_empty_lines_requires_a_canonical_positive_bounded_count() {
    let name = "top_non_empty_lines";
    assert!(validate_region_name(&format!("{name}(1)")).is_ok());
    assert!(validate_region_name(&format!("{name}({})", u16::MAX)).is_ok());
    for count in ["0", "01", "+1", "65536", "999999999999999999999999"] {
        assert!(
            validate_region_name(&format!("{name}({count})")).is_err(),
            "{name} accepted invalid count {count}"
        );
    }
}

#[test]
fn top_non_empty_lines_requires_engine_three_when_declared() {
    let manifest = r#"
id = "grok"
version = "1"
min_engine_version = 2

[[rules]]
id = "background"
state = "working"
region = " top_non_empty_lines(1) "
contains = ["active"]
"#;

    assert!(parse_manifest(manifest).is_err());
}

// ---------------------------------------------------------------------------
// OSC rule tests — exercise the new osc_title / osc_progress regions against
// the bundled Claude and Codex manifests.
// ---------------------------------------------------------------------------

fn osc_explain(
    agent: Agent,
    screen: &str,
    osc_title: &str,
    osc_progress: &str,
) -> DetectionExplain {
    with_bundled_manifests("bundled-osc-explain", || {
        explain_with_input(
            agent,
            DetectionInput {
                screen,
                osc_title,
                osc_progress,
            },
        )
    })
}

// --- Claude OSC rules ---

#[test]
fn claude_osc_title_braille_prefix_is_working() {
    // "⠂" is U+2802, in the braille block U+2800-U+28FF
    let result = osc_explain(Agent::Claude, "", "⠂ project", "");
    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_working")
    );
    assert!(result.visible_working);
}

#[test]
fn claude_osc_title_static_prefix_is_idle() {
    // "✳" is U+2733, static prefix when Claude is not working
    let result = osc_explain(Agent::Claude, "", "✳ Claude Code", "");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
    assert!(result.visible_idle);
}

#[test]
fn claude_osc_progress_4_3_alone_does_not_force_working() {
    // Claude leaves progress stuck at 4;3 while waiting for permission, so
    // 4;3 must not be a working signal on its own. With no other evidence it
    // falls back to idle; blocked screen rules can win when present.
    let result = osc_explain(Agent::Claude, "", "", "4;3;");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.fallback_reason.as_deref(),
        Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
    );
    assert!(!result.visible_working);
}

#[test]
fn claude_blocker_screen_outranks_stale_osc_progress() {
    // Regression: progress 4;3 persists during permission prompts. The
    // blocked form on screen must win because no rule treats 4;3 as working.
    let blocker_screen =
        "──────────\n  1. Yes\n  2. No\n\nEnter to select · ↑/↓ to navigate · Esc to cancel\n";
    let result = osc_explain(Agent::Claude, blocker_screen, "✳ Task title", "4;3;");
    assert_eq!(result.state, AgentState::Blocked);
    assert!(result.visible_blocker);
}

#[test]
fn claude_wrapped_blocker_footer_is_blocked() {
    // Captured reporter panel at 48 columns: the footer wraps once between
    // "to" and "cancel" without inserting a blank line.
    let screen = "────────────────────────────────────────────────\n\
  4. Chat about this\n\n\
Enter to select · ↑/↓ to navigate · Esc to\n\
cancel\n";
    let result = osc_explain(Agent::Claude, screen, "", "");
    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("live_blocked_form")
    );
    assert!(result.visible_blocker);
}

#[test]
fn claude_selection_dialog_is_blocked_at_every_wrap_width() {
    let cases = [
        ("50 columns", "──────────────────────────────────────────────────\n  4. Chat about this\n\nEnter to select · ↑/↓ to navigate · Esc to cancel\n"),
        ("48 columns", "────────────────────────────────────────────────\n  4. Chat about this\n\nEnter to select · ↑/↓ to navigate · Esc to\ncancel\n"),
        ("40 columns", "────────────────────────────────────────\n  4. Chat about this\n\nEnter to select · ↑/↓ to navigate · Esc\nto cancel\n"),
        ("34 columns", "──────────────────────────────────\n  4. Chat about this\n\nEnter to select · ↑/↓ to\nnavigate · Esc to cancel\n"),
        ("24 columns", "────────────────────────\n  4. Chat about this\n\nEnter to\nselect · ↑/↓ to\nnavigate · Esc\nto cancel\n"),
        ("indented continuation", "────────────────────────\n  4. Chat about this\n\n  Enter to select · ↑/↓ to navigate · Esc to\n   cancel\n"),
        ("Enter to confirm wrap", "────────────────────────\n  Do you trust this folder?\n\nEnter to\nconfirm · Esc to cancel\n"),
    ];
    for (label, screen) in cases {
        let result = osc_explain(Agent::Claude, screen, "", "");
        assert_eq!(result.state, AgentState::Blocked, "{label}");
        assert_eq!(
            result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("live_blocked_form"),
            "{label}"
        );
        assert!(result.visible_blocker, "{label}");
    }
}

#[test]
fn claude_wrap_tolerant_blocker_leaves_neighbouring_screens_alone() {
    for (label, screen, rule) in [
        (
            "streaming",
            "✽ Cooking… (6s · ↓ 174 tokens · thinking)\n",
            "live_turn_working",
        ),
        (
            "mode line",
            "⏵⏵ accept edits on · esc to interrupt\n",
            "live_turn_working",
        ),
        (
            "overlay",
            "  /btw\n  a note\n\nesc to close\n",
            "btw_overlay_working",
        ),
    ] {
        let result = osc_explain(Agent::Claude, screen, "", "");
        assert_eq!(result.state, AgentState::Working, "{label}");
        assert_eq!(
            result
                .matched_rule
                .as_ref()
                .map(|matched| matched.id.as_str()),
            Some(rule),
            "{label}"
        );
    }
    let transcript = osc_explain(
        Agent::Claude,
        "Showing detailed transcript · Ctrl+O to toggle\n",
        "",
        "",
    );
    assert_eq!(
        transcript
            .matched_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("transcript_viewer")
    );
    assert!(transcript.skip_state_update);
}

#[test]
fn claude_blocker_footer_separator_spans_one_wrap_only() {
    for (label, screen) in [
        ("blank line", "────────────────────────\nEnter to confirm\n\nsomething esc to\n\ncancel\n"),
        ("NBSP", "────────────────────────\n  4. Chat about this\n\nEnter to select · ↑/↓ to navigate · Esc\u{a0}to\u{a0}cancel\n"),
        ("no whitespace", "────────────────────────\n  4. Chat about this\n\nEntertoselect · ↑/↓tonavigate · Esctocancel\n"),
    ] {
        let result = osc_explain(Agent::Claude, screen, "", "");
        assert_ne!(result.matched_rule.as_ref().map(|rule| rule.id.as_str()), Some("live_blocked_form"), "{label}");
    }
}

#[test]
fn claude_osc_progress_4_0_is_idle() {
    let result = osc_explain(Agent::Claude, "", "", "4;0;");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_progress_idle")
    );
}

#[test]
fn claude_blocker_screen_outranks_osc_idle_title() {
    // When the OSC title shows ✳ (idle) but the screen has a bash permission
    // prompt, the blocked rule at priority 1190 beats osc_title_idle at 250.
    let blocker_screen = "do you want to proceed?\n\
        bash command: rm -rf /tmp/test\n\
        ❯ 1. Yes\n   2. No\n\n\
        Esc to cancel · Tab to amend · ctrl+e to explain\n";
    let result = osc_explain(Agent::Claude, blocker_screen, "✳ Claude Code", "");
    assert_eq!(result.state, AgentState::Blocked);
    assert!(result.visible_blocker);
}

#[test]
fn claude_empty_osc_empty_screen_is_idle_fallback() {
    // No OSC data, no matching screen rule → fallback idle (unchanged V3 behavior)
    let result = osc_explain(Agent::Claude, "", "", "");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.fallback_reason.as_deref(),
        Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
    );
    assert!(!result.visible_idle);
}

#[test]
fn claude_live_prompt_box_remains_a_positive_idle_observation() {
    let screen = "──────────\n❯\n──────────\n";
    let result = osc_explain(Agent::Claude, screen, "", "");

    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("live_prompt_box")
    );
    assert!(result.visible_idle);
}

#[test]
fn claude_kimi_real_screen_fixtures_classify_idle_and_blocked() {
    let claude_idle = include_str!(
        "../../../tests/fixtures/agent-detection/claude-empty-prompt-ub1-wM-pJ-20260825.txt"
    );
    let kimi_through_claude = include_str!(
        "../../../tests/fixtures/agent-detection/kimi-through-claude-empty-prompt-ub1-wM-pK-20260825.txt"
    );
    let native_permission = include_str!(
        "../../../tests/fixtures/agent-detection/claude-native-bash-permission-20260825.txt"
    );
    let ask = include_str!(
        "../../../tests/fixtures/agent-detection/claude-native-ask-user-question-20260825.txt"
    );
    let narrow_asks = [
        (
            50,
            include_str!(
                "../../../tests/fixtures/agent-detection/claude-native-ask-user-question-width-50-20260825.txt"
            ),
        ),
        (
            48,
            include_str!(
                "../../../tests/fixtures/agent-detection/claude-native-ask-user-question-width-48-20260825.txt"
            ),
        ),
        (
            40,
            include_str!(
                "../../../tests/fixtures/agent-detection/claude-native-ask-user-question-width-40-20260825.txt"
            ),
        ),
        (
            34,
            include_str!(
                "../../../tests/fixtures/agent-detection/claude-native-ask-user-question-width-34-20260825.txt"
            ),
        ),
        (
            24,
            include_str!(
                "../../../tests/fixtures/agent-detection/claude-native-ask-user-question-width-24-20260825.txt"
            ),
        ),
    ];
    let narrow_layouts = include_str!(
        "../../../tests/fixtures/agent-detection/claude-native-ask-user-question-widths-20260825.layout.ndjson"
    )
    .lines()
    .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
    .collect::<Vec<_>>();
    let trust =
        include_str!("../../../tests/fixtures/agent-detection/claude-trust-folder-20260825.txt");
    let narrow_trust = include_str!(
        "../../../tests/fixtures/agent-detection/claude-trust-folder-narrow-20260825.txt"
    );
    let login =
        include_str!("../../../tests/fixtures/agent-detection/claude-login-method-20260825.txt");
    let quoted = include_str!(
        "../../../tests/fixtures/agent-detection/claude-quoted-blocker-live-prompt-20260825.txt"
    );
    let model_picker =
        include_str!("../../../tests/fixtures/agent-detection/claude-model-picker-20260825.txt");

    for screen in [claude_idle, kimi_through_claude] {
        let result = bundled_explain(Agent::Claude, screen);
        assert_eq!(result.state, AgentState::Idle);
        assert_eq!(
            result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("live_prompt_box")
        );
        assert!(result.visible_idle);
    }

    for (label, screen) in [
        ("bash permission", native_permission),
        ("AskUserQuestion", ask),
        ("trust folder", trust),
        ("wrapped trust folder", narrow_trust),
        ("login method", login),
    ] {
        let result = osc_explain(Agent::Claude, screen, "", "4;3;");
        assert_eq!(result.state, AgentState::Blocked, "{label}");
        assert!(result.visible_blocker, "{label}");
        assert!(
            !screen.contains("closing-block") && !screen.contains("Gate "),
            "{label} must not depend on fleet tokens"
        );
    }

    for ((width, screen), layout) in narrow_asks.into_iter().zip(narrow_layouts) {
        assert_eq!(
            layout["result"]["layout"]["area"]["width"].as_u64(),
            Some(width as u64),
            "capture must retain exact physical pane width {width}"
        );
        assert_eq!(
            layout["result"]["layout"]["panes"][0]["rect"]["width"].as_u64(),
            Some(width as u64),
            "captured pane must match layout area at width {width}"
        );
        let top_border = screen.lines().find(|line| !line.is_empty()).unwrap();
        assert_eq!(
            top_border.chars().count(),
            width - 1,
            "Claude must paint its observed width-minus-one border at width {width}"
        );
        assert!(top_border.starts_with('╭') && top_border.ends_with('╮'));
        let result = osc_explain(Agent::Claude, screen, "", "4;3;");
        assert_eq!(result.state, AgentState::Blocked, "width {width}");
        assert!(result.visible_blocker, "width {width}");
        assert!(
            !screen.contains("closing-block") && !screen.contains("Gate "),
            "width {width} must not depend on fleet tokens"
        );
    }

    for (label, screen) in [("quoted history", quoted), ("model picker", model_picker)] {
        let result = bundled_explain(Agent::Claude, screen);
        assert!(!result.visible_blocker, "{label}");
        assert_ne!(result.state, AgentState::Blocked, "{label}");
    }
}

#[test]
fn visible_blocker_overrides_fresh_hook_authority_priority_1100_working() {
    let native_permission = include_str!(
        "../../../tests/fixtures/agent-detection/claude-native-bash-permission-20260825.txt"
    );
    let result = osc_explain(Agent::Claude, native_permission, "⠋ Claude Code", "4;3;");
    assert_eq!(result.state, AgentState::Blocked);
    assert!(result.visible_blocker);
    assert!(result.matched_rule.as_ref().unwrap().priority > 1100);
}

#[test]
fn bundled_manifest_versions_cover_deployed_and_upstream_floors() {
    let claude: toml::Value = toml::from_str(include_str!("../manifests/claude.toml")).unwrap();
    let kimi: toml::Value = toml::from_str(include_str!("../manifests/kimi.toml")).unwrap();
    assert_eq!(claude["version"].as_str(), Some("2026.08.26.1001"));
    assert!(kimi["version"]
        .as_str()
        .is_some_and(|version| version > "2026.06.10.1"));
    let claude_text = include_str!("../manifests/claude.toml");
    for working_rule in [
        "live_turn_working",
        "background_shell_working",
        "background_agents_working",
        "background_mcp_task_working",
    ] {
        assert!(
            claude_text.contains(working_rule),
            "missing deployed working rule {working_rule}"
        );
    }
}

// --- Codex OSC rules ---

#[test]
fn codex_osc_title_braille_spinner_is_working() {
    // "⠋" is U+280B, in the braille block
    let result = osc_explain(Agent::Codex, "", "⠋ llm-proxy", "");
    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_working")
    );
    assert!(result.visible_working);
}

#[test]
fn codex_osc_title_action_required_is_blocked() {
    let result = osc_explain(Agent::Codex, "", "[ . ] Action Required | llm-proxy", "");
    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_blocked")
    );
    assert!(result.visible_blocker);
}

#[test]
fn codex_osc_title_plain_is_idle() {
    let result = osc_explain(Agent::Codex, "", "llm-proxy", "");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
    assert!(!result.visible_idle);
}

#[test]
fn codex_background_terminal_screen_does_not_override_osc_idle() {
    // Background terminal tasks can be long-lived helpers such as dev servers.
    // They should not make Codex look busy once the foreground turn is idle.
    let screen = "background terminal running · /ps to view · /stop to close\n";
    let result = osc_explain(Agent::Codex, screen, "llm-proxy", "");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
    assert!(!result.visible_idle);
}

#[test]
fn codex_screen_working_fallback_handles_static_osc_title_and_progress_decorations() {
    for progress_line in [
        "◦ Working (1m 16s • esc to interrupt) · 1 background…",
        "Working (1m 16s • esc to interrupt)",
    ] {
        let screen = format!(
            "• I’ll run it and wait for completion.\n\n{progress_line}\n\n\
             › Use /skills to list available skills\n\n\
             gpt-5.6-sol default · /work\n\
             footer detail\n"
        );
        let result = osc_explain(Agent::Codex, &screen, "project", "");

        assert_eq!(result.state, AgentState::Working, "{progress_line}");
        assert_eq!(
            result.matched_rule.as_ref().map(|r| r.id.as_str()),
            Some("screen_working_fallback"),
            "{progress_line}"
        );
        assert_eq!(
            result
                .matched_rule
                .as_ref()
                .map(|rule| rule.region.as_str()),
            Some("bottom_non_empty_lines(6)"),
            "{progress_line}"
        );
        assert!(result.visible_working, "{progress_line}");
    }
}

/// Verbatim from a codex-cli 0.147.0 pane read back through `agent.read` while
/// it ran a shell command. Note what is *not* here: no bullet, no spinner, and
/// an OSC title that never leaves the cwd. This screen is the reason the rule
/// cannot anchor itself to a block marker.
const CODEX_TOOL_CALL_SCREEN: &str = "\
› Run exactly this shell command and nothing else, then report the exit code: sleep 45

Working (19s • esc to interrupt) · 1 background terminal running · /ps to view

› Write tests for @filename

  gpt-5.6-luna medium · Context 97% left · 8.69K used · ~/workspaces/personal
";

/// The same pane a few seconds later, streaming its answer. Codex removes the
/// progress line while it writes, so nothing on screen separates this from a
/// finished turn -- only the bytes still arriving do.
const CODEX_STREAMING_SCREEN: &str = "\
› Explain in detail, step by step, how the Rust borrow checker works.

• Rust's borrow checker enforces one central rule:

  let value = String::from(\"hello\");
  let reference = &value;

› Use /skills to list available skills

  gpt-5.6-luna medium · Context 100% left · ~/workspaces/personal
";

#[test]
fn a_codex_pane_running_a_tool_is_working_even_though_it_printed_no_block_marker() {
    let result = osc_explain(Agent::Codex, CODEX_TOOL_CALL_SCREEN, "personal", "");

    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("screen_working_fallback")
    );
    assert!(result.visible_working);
}

#[test]
fn a_streaming_codex_pane_shows_no_working_evidence_on_screen_at_all() {
    let result = osc_explain(Agent::Codex, CODEX_STREAMING_SCREEN, "personal", "");

    // Idle by elimination, and the flag says so: nobody observed an idle pane,
    // the manifest simply ran out of rules. Recent output is what rescues this
    // screen, and it lives outside the manifest.
    assert_eq!(result.state, AgentState::Idle);
    assert!(!result.visible_working);
    assert!(!result.visible_idle);
}

#[test]
fn codex_osc_working_remains_preferred_over_screen_fallback() {
    let screen = "• Working (4s • esc to interrupt)\n\n\
        › Use /skills to list available skills\n\n\
        gpt-5.6-sol default · /work\n";
    let result = osc_explain(Agent::Codex, screen, "⠸ project", "");

    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_working")
    );
    assert!(result.visible_working);
}

#[test]
fn codex_screen_blocker_outranks_working_fallback() {
    let screen = "• Working (4s • esc to interrupt)\n\
        › 1. Yes, proceed\n\
        Press enter to confirm or esc to cancel\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("live_strong_blocker")
    );
    assert!(result.visible_blocker);
    assert!(!result.visible_working);
}

#[test]
fn codex_weak_blocker_outranks_working_fallback() {
    let screen = "• Working (4s • esc to interrupt)\n\
        do you want to continue? [y/n]\n\
        › Use /skills to list available skills\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("weak_blocker")
    );
    assert!(!result.visible_working);
}

#[test]
fn codex_transcript_viewer_outranks_working_fallback() {
    let screen = "• Working (4s • esc to interrupt)\n\
        › transcript\n\
        ↑/↓ to scroll · pgup/pgdn to move · home/end to jump · q to quit · esc to edit prev\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Unknown);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("transcript_viewer")
    );
    assert!(result.skip_state_update);
    assert!(!result.visible_working);
}

#[test]
fn codex_screen_working_fallback_ignores_stale_and_prompt_text() {
    let screens = [
        "◦ Working (1m 16s • esc to interrupt)\n\
         ■ Conversation interrupted\n\
         › Use /skills to list available skills\n\
         gpt-5.6-sol default · /work\n",
        "› Explain the text ◦ Working (1m 16s • esc to interrupt)\n\
         gpt-5.6-sol default · /work\n",
        "  ◦ Working (1m 16s • esc to interrupt)\n\
         › Use /skills to list available skills\n\
         gpt-5.6-sol default · /work\n",
        // A stale progress line from an *earlier* turn is deliberately not
        // tested here, because codex erases that line the moment the turn ends
        // -- the same erase that leaves a streaming pane with no working
        // evidence at all (see CODEX_STREAMING_SCREEN). A copy can only survive
        // in scrollback the pane has already scrolled past, which puts it far
        // above the bottom lines this rule reads. Narrowing the region to
        // exclude a screen that cannot occur would cost the margin the observed
        // tool-call screen needs.
    ];

    for screen in screens {
        let result = osc_explain(Agent::Codex, screen, "project", "");
        assert_eq!(result.state, AgentState::Idle);
        assert_eq!(
            result.matched_rule.as_ref().map(|r| r.id.as_str()),
            Some("osc_title_idle")
        );
        assert!(!result.visible_idle);
        assert!(!result.visible_working);
    }
}

#[test]
fn codex_screen_working_fallback_ignores_interrupted_short_terminal() {
    let screen = "◦ Working (1m 16s • esc to interrupt)\n\
        ■ Conversation interrupted\n\
        ›\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
    assert!(!result.visible_idle);
    assert!(!result.visible_working);
}

#[test]
fn codex_osc_working_beats_weak_blocker_screen() {
    // A stale [y/n] on screen triggers weak_blocker at priority 600, but an
    // active braille spinner in the OSC title is priority 1050 — OSC wins.
    let screen = "do you want to continue? [y/n]\n";
    let result = osc_explain(Agent::Codex, screen, "⠋ llm-proxy", "");
    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_working")
    );
}

#[test]
fn claude_usage_limit_screen_reads_as_a_usage_blocker() {
    // The plan-limit banner sits right above the live prompt box, so the idle
    // rule (950) also matches; the usage rule at 960 has to win.
    let screen = "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\n\
        You've hit your 5-hour limit resets 3pm\n\
        /upgrade to increase your usage limit.\n\
        \u{256d}\u{2500}\u{2500}\u{256e}\n\
        \u{2502} \u{276f}   \u{2502}\n\
        \u{2570}\u{2500}\u{2500}\u{256f}\n";
    let result = osc_explain(Agent::Claude, screen, "", "");

    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("usage_limit_reached")
    );
    assert!(result.visible_blocker);
    assert!(result.usage_limited);
}

#[test]
fn claude_usage_limit_clears_as_soon_as_the_agent_works() {
    // Live screen only: the banner may still be on screen, but a working OSC
    // title (1100) outranks it, so nothing latches.
    let screen = "You've hit your 5-hour limit resets 3pm\n\
        /upgrade to increase your usage limit.\n";
    let result = osc_explain(Agent::Claude, screen, "\u{2807} scalable", "");

    assert_eq!(result.state, AgentState::Working);
    assert!(!result.usage_limited);
}

#[test]
fn codex_usage_limit_screen_reads_as_a_usage_blocker() {
    let screen = "You've hit your usage limit for gpt-5.6-sol.\n\
        Try again at 4:15pm.\n";
    let result = osc_explain(Agent::Codex, screen, "", "");

    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("usage_limit_reached")
    );
    assert!(result.visible_blocker);
    assert!(result.usage_limited);
}

#[test]
fn codex_usage_limit_clears_as_soon_as_the_agent_works() {
    let screen = "You've hit your usage limit for gpt-5.6-sol.\n\
        Try again later.\n";
    let result = osc_explain(Agent::Codex, screen, "\u{2839} llm-proxy", "");

    assert_eq!(result.state, AgentState::Working);
    assert!(!result.usage_limited);
}

#[test]
fn an_ordinary_claude_question_is_not_a_usage_blocker() {
    let screen = "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\n\
        Do you want to proceed?\n\
        \u{276f} 1. Yes\n\
        enter to select \u{00b7} esc to cancel \u{00b7} arrow keys to navigate\n";
    let result = osc_explain(Agent::Claude, screen, "", "");

    assert_eq!(result.state, AgentState::Blocked);
    assert!(result.visible_blocker);
    assert!(!result.usage_limited);
}
