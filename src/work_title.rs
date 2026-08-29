use std::sync::OnceLock;

use regex::Regex;
use serde::Deserialize;

use crate::api::schema::PaneReportMetadataParams;

pub(crate) const WORK_TITLE_SOURCE: &str = "herdr:work-title";
/// Metadata source for an agent-reported session rename. It is separate from
/// `WORK_TITLE_SOURCE` because a rename patches only the session name and must
/// not replace the work context the last turn established.
pub(crate) const SESSION_NAME_SOURCE: &str = "herdr:session-name";
/// Session names are authored for humans, so they are kept whole rather than
/// reduced to the derived work title's word budget.
pub(crate) const SESSION_NAME_MAX_CHARS: usize = 80;
pub(crate) const WORK_TITLE_MAX_CHARS: usize = 48;
const WORK_TITLE_MIN_WORDS: usize = 1;
const WORK_TITLE_MAX_WORDS: usize = 7;

#[derive(Debug, Deserialize)]
struct TurnStartHookInput {
    hook_event_name: Option<String>,
    session_id: Option<String>,
    prompt: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    transcript_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkTitleProvider {
    Claude,
    Codex,
}

impl WorkTitleProvider {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }

    fn agent(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    fn lifecycle_source(self) -> &'static str {
        match self {
            Self::Claude => "herdr:claude",
            Self::Codex => "herdr:codex",
        }
    }
}

pub(crate) fn request_from_turn_start(
    provider: WorkTitleProvider,
    pane_id: Option<&str>,
    input: &str,
    seq: u64,
) -> Option<PaneReportMetadataParams> {
    let pane_id = pane_id.map(str::trim).filter(|value| !value.is_empty())?;
    let input: TurnStartHookInput = serde_json::from_str(input).ok()?;
    if input.hook_event_name.as_deref() != Some("UserPromptSubmit") {
        return None;
    }
    // Claude exposes subagent identity explicitly. A native subagent has no
    // independent Herdr pane/title surface, so never let it rename its parent.
    if provider == WorkTitleProvider::Claude
        && input
            .agent_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
    {
        return None;
    }
    let session_id = input
        .session_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())?;
    let prompt = input.prompt.as_deref()?;
    let title = calculate_work_title(prompt);
    let work_context = crate::work_context::PaneWorkContext {
        ticket_ids: crate::work_context::extract_ticket_ids(prompt),
        pr_urls: crate::work_context::extract_pr_urls(prompt),
        preview_urls: crate::work_context::extract_preview_urls(prompt),
        missive_urls: crate::work_context::extract_missive_urls(prompt),
        branch: None,
        work_title: title.clone(),
        // The turn-title path never names the session; only the guarded
        // session-name report may.
        session_name: None,
    };

    Some(PaneReportMetadataParams {
        pane_id: pane_id.to_string(),
        source: WORK_TITLE_SOURCE.to_string(),
        agent: Some(provider.agent().to_string()),
        applies_to_source: Some(provider.lifecycle_source().to_string()),
        agent_session_id: Some(session_id),
        title,
        work_context: Some(work_context),
        display_agent: None,
        state_labels: std::collections::HashMap::new(),
        tokens: std::collections::HashMap::new(),
        clear_title: false,
        clear_display_agent: false,
        clear_state_labels: false,
        seq: Some(seq),
        ttl_ms: None,
    })
}

pub(crate) fn calculate_work_title(prompt: &str) -> Option<String> {
    let sanitized = sanitize_prompt(prompt);
    let words = meaningful_objective_words(&sanitized)?;
    let mut title_words = Vec::new();
    for word in words.into_iter().take(WORK_TITLE_MAX_WORDS) {
        let word = title_case_word(&word);
        let candidate = if title_words.is_empty() {
            word.clone()
        } else {
            format!("{} {word}", title_words.join(" "))
        };
        if candidate.chars().count() > WORK_TITLE_MAX_CHARS {
            break;
        }
        title_words.push(word);
    }
    if title_words.len() < WORK_TITLE_MIN_WORDS {
        return None;
    }
    Some(title_words.join(" "))
}

fn sanitize_prompt(prompt: &str) -> String {
    let without_escapes = ansi_regex().replace_all(prompt, " ");
    let without_secrets = secret_regex().replace_all(&without_escapes, " ");
    without_secrets
        .chars()
        .map(|character| {
            if character == '\n' {
                character
            } else if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn meaningful_objective_words(prompt: &str) -> Option<Vec<String>> {
    let mut paragraphs = Vec::new();
    let mut current = Vec::new();
    for line in prompt.lines() {
        let line = line.trim();
        if line.is_empty() {
            if !current.is_empty() {
                paragraphs.push(current.join(" "));
                current.clear();
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        paragraphs.push(current.join(" "));
    }
    paragraphs.into_iter().rev().find_map(|paragraph| {
        let relevant = relevant_objective_clause(&paragraph);
        let words: Vec<String> = objective_words(relevant)
            .into_iter()
            .filter(|word| !is_stopword(word))
            .take(WORK_TITLE_MAX_WORDS)
            .collect();
        (!words.is_empty()).then_some(words)
    })
}

fn relevant_objective_clause(prompt: &str) -> &str {
    let lower = prompt.to_ascii_lowercase();
    let mut offset = 0;
    let mut sentence_start = 0;
    for (index, character) in lower.char_indices() {
        if !matches!(character, '.' | '!' | '?' | ';') {
            continue;
        }
        let sentence = &lower[sentence_start..index];
        let dismissed = [
            " is unrelated",
            " are unrelated",
            " not the task",
            " not the objective",
            " not requested",
        ]
        .iter()
        .any(|marker| sentence.contains(marker));
        let candidate = index + character.len_utf8();
        if dismissed && objective_words(&prompt[candidate..]).len() >= 2 {
            offset = candidate;
        }
        sentence_start = candidate;
    }
    for marker in [" instead ", " actually ", " just ", " only "] {
        if let Some(index) = lower.rfind(marker) {
            let candidate = index + marker.len();
            if objective_words(&prompt[candidate..]).len() >= 2 && candidate > offset {
                offset = candidate;
            }
        }
    }
    &prompt[offset..]
}

fn objective_words(value: &str) -> Vec<String> {
    value
        .split(|character: char| !character.is_alphanumeric() && character != '-')
        .map(|word| word.trim_matches('-').to_lowercase())
        .filter(|word| {
            !word.is_empty()
                && !looks_sensitive(word)
                && !looks_like_identifier(word)
                && word != "redacted"
        })
        .collect()
}

fn title_case_word(word: &str) -> String {
    if word.eq_ignore_ascii_case("pr")
        || word.eq_ignore_ascii_case("ci")
        || word.chars().any(|character| character.is_ascii_digit())
            && word
                .chars()
                .any(|character| character.is_ascii_alphabetic())
    {
        return word.to_ascii_uppercase();
    }
    let mut characters = word.chars();
    let Some(first) = characters.next() else {
        return String::new();
    };
    first.to_uppercase().collect::<String>() + characters.as_str()
}

fn looks_sensitive(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "secret",
        "credential",
        "authorization",
        "api-key",
        "apikey",
        "access-token",
        "customer",
        "username",
    ]
    .contains(&lower.as_str())
}

fn looks_like_identifier(word: &str) -> bool {
    let ascii_alnum = word
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '-');
    let hex = word.len() >= 16 && word.chars().all(|character| character.is_ascii_hexdigit());
    let opaque = word.len() >= 24
        && ascii_alnum
        && word.chars().any(|character| character.is_ascii_digit())
        && word
            .chars()
            .any(|character| character.is_ascii_alphabetic());
    hex || opaque
}

fn is_stopword(word: &str) -> bool {
    matches!(
        word,
        "a" | "about"
            | "an"
            | "and"
            | "answering"
            | "are"
            | "as"
            | "at"
            | "be"
            | "been"
            | "begin"
            | "by"
            | "can"
            | "carry"
            | "claude"
            | "codex"
            | "continue"
            | "could"
            | "do"
            | "does"
            | "for"
            | "from"
            | "get"
            | "go"
            | "ahead"
            | "her"
            | "here"
            | "him"
            | "how"
            | "i"
            | "in"
            | "into"
            | "is"
            | "it"
            | "its"
            | "just"
            | "let"
            | "like"
            | "me"
            | "my"
            | "need"
            | "now"
            | "ok"
            | "of"
            | "on"
            | "onto"
            | "or"
            | "our"
            | "please"
            | "proceed"
            | "research"
            | "scope"
            | "should"
            | "start"
            | "that"
            | "thanks"
            | "the"
            | "their"
            | "them"
            | "these"
            | "this"
            | "those"
            | "to"
            | "under"
            | "us"
            | "using"
            | "very"
            | "want"
            | "was"
            | "we"
            | "what"
            | "when"
            | "which"
            | "why"
            | "with"
            | "working"
            | "would"
            | "you"
            | "your"
    )
}

fn ansi_regex() -> &'static Regex {
    static ANSI: OnceLock<Regex> = OnceLock::new();
    ANSI.get_or_init(|| {
        Regex::new(r"(?:\x1B\][^\x07]*(?:\x07|\x1B\\))|(?:\x1B\[[0-?]*[ -/]*[@-~])")
            .expect("static ANSI regex")
    })
}

fn secret_regex() -> &'static Regex {
    static SECRET: OnceLock<Regex> = OnceLock::new();
    SECRET.get_or_init(|| {
        Regex::new(
            r#"(?ix)
            \b(?:bearer)\s+\S+
            |\b(?:api[\s_-]*key|access[\s_-]*token|password|passwd|secret|authorization)\s*[:=]\s*\S+
            |\b(?:sk|ghp|github_pat|xox[baprs]|sb_secret|akia)[-_][a-z0-9_-]{4,}\b
            |\b[a-z0-9._%+-]+@[a-z0-9.-]+\.[a-z]{2,}\b
            |(?:^|\s)(?:/|~/|[a-z]:\\)\S+
            |@[a-z0-9_-]{2,}
            "#,
        )
        .expect("static secret regex")
    })
}

/// Claude records the name it gave a session as an `ai-title` entry in the
/// session transcript, and appends a fresh entry every time it renames. The
/// last entry for the reported session is therefore the current name.
pub(crate) fn latest_session_name(transcript: &str, session_id: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct AiTitleRecord {
        #[serde(rename = "type")]
        record_type: Option<String>,
        #[serde(rename = "aiTitle")]
        ai_title: Option<String>,
        #[serde(rename = "sessionId")]
        session_id: Option<String>,
    }

    let mut latest = None;
    for line in transcript.lines() {
        let line = line.trim();
        if line.is_empty() || !line.contains("ai-title") {
            continue;
        }
        let Ok(record) = serde_json::from_str::<AiTitleRecord>(line) else {
            continue;
        };
        if record.record_type.as_deref() != Some("ai-title") {
            continue;
        }
        // Transcript files are per session, but a resumed session can carry
        // entries for the session it forked from. Only the reported session
        // may rename this pane.
        if record
            .session_id
            .as_deref()
            .is_some_and(|value| value.trim() != session_id)
        {
            continue;
        }
        if let Some(title) = record.ai_title.as_deref().map(sanitize_session_name) {
            if !title.is_empty() {
                latest = Some(title);
            }
        }
    }
    latest
}

fn sanitize_session_name(name: &str) -> String {
    let sanitized = sanitize_prompt(name);
    let mut collapsed = String::new();
    for word in sanitized.split_whitespace() {
        if collapsed.chars().count() + 1 + word.chars().count() > SESSION_NAME_MAX_CHARS {
            break;
        }
        if !collapsed.is_empty() {
            collapsed.push(' ');
        }
        collapsed.push_str(word);
    }
    collapsed
}

/// Build the rename report for a hook payload and the session transcript it
/// points at. Unlike the turn-title path this accepts every hook event: Claude
/// names and renames a session outside prompt submission, so binding renames to
/// `UserPromptSubmit` is exactly what made the label go stale.
pub(crate) fn request_from_session_name(
    provider: WorkTitleProvider,
    pane_id: Option<&str>,
    input: &str,
    transcript: &str,
    seq: u64,
) -> Option<PaneReportMetadataParams> {
    // Only Claude publishes a session name today.
    if provider != WorkTitleProvider::Claude {
        return None;
    }
    let pane_id = pane_id.map(str::trim).filter(|value| !value.is_empty())?;
    let input: TurnStartHookInput = serde_json::from_str(input).ok()?;
    input.hook_event_name.as_deref()?;
    // A native subagent shares the parent pane and must never rename it.
    if input
        .agent_id
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return None;
    }
    let session_id = input
        .session_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())?;
    let session_name = latest_session_name(transcript, &session_id)?;

    Some(PaneReportMetadataParams {
        pane_id: pane_id.to_string(),
        source: SESSION_NAME_SOURCE.to_string(),
        agent: Some(provider.agent().to_string()),
        applies_to_source: Some(provider.lifecycle_source().to_string()),
        agent_session_id: Some(session_id),
        title: None,
        work_context: Some(crate::work_context::PaneWorkContext {
            session_name: Some(session_name),
            ..Default::default()
        }),
        display_agent: None,
        state_labels: std::collections::HashMap::new(),
        tokens: std::collections::HashMap::new(),
        clear_title: false,
        clear_display_agent: false,
        clear_state_labels: false,
        seq: Some(seq),
        ttl_ms: None,
    })
}

/// Read the transcript a hook payload points at, if any.
pub(crate) fn transcript_path_from_hook_input(input: &str) -> Option<String> {
    let input: TurnStartHookInput = serde_json::from_str(input).ok()?;
    input
        .transcript_path
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculates_concrete_titles_for_representative_turns() {
        for (prompt, expected) in [
            (
                "Please audit the local CI evidence",
                "Audit Local CI Evidence",
            ),
            (
                "Merge PR 672 skill metadata safely",
                "Merge PR 672 Skill Metadata Safely",
            ),
            (
                "Measure fleet token savings over twenty turns",
                "Measure Fleet Token Savings Over Twenty Turns",
            ),
            (
                "Fix the billing retry regression",
                "Fix Billing Retry Regression",
            ),
            (
                "Review auth migration safety",
                "Review Auth Migration Safety",
            ),
        ] {
            assert_eq!(calculate_work_title(prompt).as_deref(), Some(expected));
        }
    }

    #[test]
    fn short_first_turn_uses_the_prompt_without_filler() {
        assert_eq!(
            calculate_work_title("write a poem").as_deref(),
            Some("Write Poem")
        );
    }

    #[test]
    fn correction_uses_the_latest_objective_clause() {
        assert_eq!(
            calculate_work_title(
                "wezterm doesn't need to get updated, just the agent title in herdr"
            )
            .as_deref(),
            Some("Agent Title Herdr")
        );
    }

    #[test]
    fn long_title_is_compacted_to_the_contract() {
        let title = calculate_work_title(
            "Implement automatic recalculation of descriptive work session titles for every managed agent across all providers",
        )
        .unwrap();
        assert!((WORK_TITLE_MIN_WORDS..=WORK_TITLE_MAX_WORDS)
            .contains(&title.split_whitespace().count()));
        assert!(title.chars().count() <= WORK_TITLE_MAX_CHARS);
    }

    #[test]
    fn secrets_paths_users_and_control_sequences_never_reach_titles() {
        let title = calculate_work_title(
            "Fix \u{1b}[31mbilling\u{1b}[0m for jane@example.com using api_key=sk-live-abcdef /Users/jane/private",
        )
        .unwrap();
        assert_eq!(title, "Fix Billing");
        for forbidden in ["jane", "example", "sk", "users", "\u{1b}"] {
            assert!(!title.to_ascii_lowercase().contains(forbidden));
        }
    }

    #[test]
    fn terse_continuation_has_no_standalone_title() {
        assert_eq!(calculate_work_title("please do it now"), None);
    }

    #[test]
    fn latest_non_empty_paragraph_owns_the_title_subject() {
        let prompt = "Real-time policy management is not the task.\n\n\
            Implement briefing-derived tab titles for every window.";
        assert_eq!(
            calculate_work_title(prompt).as_deref(),
            Some("Implement Briefing-derived Tab Titles Every")
        );
        assert!(!calculate_work_title(prompt)
            .unwrap()
            .to_ascii_lowercase()
            .contains("real-time policy management"));
    }

    #[test]
    fn initial_briefing_ignores_a_trailing_meta_paragraph() {
        assert_eq!(
            calculate_work_title("Implement sidebar lifecycle assertions\n\nplease start")
                .as_deref(),
            Some("Implement Sidebar Lifecycle Assertions")
        );
    }

    #[test]
    fn dismissed_objective_in_the_same_paragraph_is_not_selected() {
        assert_eq!(
            calculate_work_title(
                "Real-time policy management is unrelated. Implement sidebar lifecycle assertions"
            )
            .as_deref(),
            Some("Implement Sidebar Lifecycle Assertions")
        );
    }

    #[test]
    fn provider_fixtures_build_session_guarded_requests() {
        let payload = include_str!("../tests/fixtures/work-titles/codex-user-prompt-submit.json");
        let request =
            request_from_turn_start(WorkTitleProvider::Codex, Some("w1:p2"), payload, 42).unwrap();
        assert_eq!(request.pane_id, "w1:p2");
        assert_eq!(request.agent.as_deref(), Some("codex"));
        assert_eq!(request.applies_to_source.as_deref(), Some("herdr:codex"));
        assert_eq!(
            request.agent_session_id.as_deref(),
            Some("fixture-codex-session")
        );
        assert_eq!(
            request.title.as_deref(),
            Some("Fix Billing Retry Regression")
        );
        assert_eq!(request.seq, Some(42));

        let claude = request_from_turn_start(
            WorkTitleProvider::Claude,
            Some("w1:p3"),
            include_str!("../tests/fixtures/work-titles/claude-user-prompt-submit.json"),
            43,
        )
        .unwrap();
        assert_eq!(claude.agent.as_deref(), Some("claude"));
        assert_eq!(
            claude.agent_session_id.as_deref(),
            Some("fixture-claude-session")
        );
        assert_eq!(
            claude.title.as_deref(),
            Some("Review Auth Migration Safety")
        );
    }

    #[test]
    fn ac1_ac2_ac3_provider_fixtures_carry_prompt_derived_work_context() {
        let codex = request_from_turn_start(
            WorkTitleProvider::Codex,
            Some("w1:p2"),
            include_str!(
                "../tests/fixtures/work-titles/codex-work-context-user-prompt-submit.json"
            ),
            44,
        )
        .unwrap();
        let codex_context = codex.work_context.expect("derived Codex work context");
        assert_eq!(codex_context.ticket_ids, vec!["MAT-7", "SCA-9"]);
        assert_eq!(
            codex_context.pr_urls,
            vec!["https://github.com/scalable-so/herdr/pull/21"]
        );
        assert_eq!(
            codex_context.preview_urls,
            vec!["https://codex-preview.vercel.app"]
        );

        let claude = request_from_turn_start(
            WorkTitleProvider::Claude,
            Some("w1:p3"),
            include_str!(
                "../tests/fixtures/work-titles/claude-work-context-user-prompt-submit.json"
            ),
            45,
        )
        .unwrap();
        let claude_context = claude.work_context.expect("derived Claude work context");
        assert_eq!(claude_context.ticket_ids, vec!["SCA-42"]);
        assert_eq!(
            claude_context.pr_urls,
            vec!["https://github.com/scalable-so/herdr/pull/17"]
        );
        assert_eq!(
            claude_context.preview_urls,
            vec!["https://claude-preview.vercel.app"]
        );
    }

    #[test]
    fn ac25_turn_hook_extracts_only_valid_preview_urls() {
        let request = request_from_turn_start(
            WorkTitleProvider::Codex,
            Some("w1:p1"),
            r#"{
                "hook_event_name":"UserPromptSubmit",
                "session_id":"session-preview",
                "prompt":"Ship https://Demo-Preview.Vercel.App and https://evil.example.test, then https://second.vercel.app?token=secret"
            }"#,
            46,
        )
        .expect("guarded turn hook should produce metadata");

        assert_eq!(
            request.work_context.expect("work context").preview_urls,
            vec!["https://demo-preview.vercel.app"]
        );
    }

    #[test]
    fn turn_hook_extracts_missive_conversation_links_from_the_prompt() {
        let request = request_from_turn_start(
            WorkTitleProvider::Codex,
            Some("w1:p1"),
            r#"{
                "hook_event_name":"UserPromptSubmit",
                "session_id":"session-missive",
                "prompt":"Reply in https://mail.missiveapp.com/#inbox/conversations/abc123 and ignore https://mail.missiveapp.com"
            }"#,
            47,
        )
        .expect("guarded turn hook should produce metadata");

        assert_eq!(
            request.work_context.expect("work context").missive_urls,
            vec!["https://mail.missiveapp.com/#inbox/conversations/abc123"]
        );
    }

    #[test]
    fn missing_support_and_claude_subagents_are_safe_noops() {
        let root_payload = r#"{
            "hook_event_name":"UserPromptSubmit",
            "session_id":"session-1",
            "prompt":"Review auth migration safety"
        }"#;
        assert!(
            request_from_turn_start(WorkTitleProvider::Claude, None, root_payload, 1).is_none()
        );

        let subagent_payload = r#"{
            "hook_event_name":"UserPromptSubmit",
            "session_id":"subagent-1",
            "agent_id":"worker-1",
            "prompt":"Review auth migration safety"
        }"#;
        assert!(request_from_turn_start(
            WorkTitleProvider::Claude,
            Some("w1:p1"),
            subagent_payload,
            2
        )
        .is_none());
    }

    #[test]
    fn latest_session_name_takes_the_most_recent_rename() {
        let transcript = concat!(
            r#"{"type":"user","sessionId":"s1"}"#,
            "\n",
            r#"{"type":"ai-title","aiTitle":"First name","sessionId":"s1"}"#,
            "\n",
            r#"{"type":"assistant","sessionId":"s1"}"#,
            "\n",
            r#"{"type":"ai-title","aiTitle":"Renamed after the rename","sessionId":"s1"}"#,
            "\n",
        );
        assert_eq!(
            latest_session_name(transcript, "s1").as_deref(),
            Some("Renamed after the rename")
        );
    }

    #[test]
    fn latest_session_name_ignores_other_sessions_and_blank_names() {
        let transcript = concat!(
            r#"{"type":"ai-title","aiTitle":"Mine","sessionId":"s1"}"#,
            "\n",
            r#"{"type":"ai-title","aiTitle":"Someone else","sessionId":"s2"}"#,
            "\n",
            r#"{"type":"ai-title","aiTitle":"   ","sessionId":"s1"}"#,
            "\n",
            "not json at all",
            "\n",
        );
        assert_eq!(
            latest_session_name(transcript, "s1").as_deref(),
            Some("Mine")
        );
        assert_eq!(latest_session_name("", "s1"), None);
    }

    #[test]
    fn latest_session_name_caps_and_normalizes_whitespace() {
        let long = "word ".repeat(40);
        let transcript = format!(r#"{{"type":"ai-title","aiTitle":"{long}","sessionId":"s1"}}"#);
        let name = latest_session_name(&transcript, "s1").expect("session name");
        assert!(name.chars().count() <= SESSION_NAME_MAX_CHARS, "{name:?}");
        assert!(!name.contains("  "), "{name:?}");
    }

    #[test]
    fn session_name_request_is_guarded_and_carries_only_the_name() {
        let input =
            r#"{"hook_event_name":"Stop","session_id":"s1","transcript_path":"/tmp/s1.jsonl"}"#;
        let transcript = r#"{"type":"ai-title","aiTitle":"Live name","sessionId":"s1"}"#;
        let request = request_from_session_name(
            WorkTitleProvider::Claude,
            Some("w1:pA"),
            input,
            transcript,
            7,
        )
        .expect("session name request");
        assert_eq!(request.source, SESSION_NAME_SOURCE);
        assert_eq!(request.agent.as_deref(), Some("claude"));
        assert_eq!(request.applies_to_source.as_deref(), Some("herdr:claude"));
        assert_eq!(request.agent_session_id.as_deref(), Some("s1"));
        assert_eq!(request.title, None);
        let context = request.work_context.expect("work context");
        assert_eq!(context.session_name.as_deref(), Some("Live name"));
        assert_eq!(context.work_title, None);
        assert!(context.ticket_ids.is_empty());
    }

    #[test]
    fn session_name_request_rejects_subagents_and_missing_evidence() {
        let transcript = r#"{"type":"ai-title","aiTitle":"Live name","sessionId":"s1"}"#;
        let subagent = r#"{"hook_event_name":"Stop","session_id":"s1","agent_id":"sub"}"#;
        assert!(request_from_session_name(
            WorkTitleProvider::Claude,
            Some("w1:pA"),
            subagent,
            transcript,
            7
        )
        .is_none());

        let ok = r#"{"hook_event_name":"Stop","session_id":"s1"}"#;
        // Codex has no session-name channel.
        assert!(request_from_session_name(
            WorkTitleProvider::Codex,
            Some("w1:pA"),
            ok,
            transcript,
            7
        )
        .is_none());
        // No pane, no report.
        assert!(
            request_from_session_name(WorkTitleProvider::Claude, None, ok, transcript, 7).is_none()
        );
        // No named session yet, no report.
        assert!(
            request_from_session_name(WorkTitleProvider::Claude, Some("w1:pA"), ok, "", 7)
                .is_none()
        );
    }

    #[test]
    fn transcript_path_is_read_from_the_hook_payload() {
        assert_eq!(
            transcript_path_from_hook_input(
                r#"{"hook_event_name":"Stop","transcript_path":" /tmp/a.jsonl "}"#
            )
            .as_deref(),
            Some("/tmp/a.jsonl")
        );
        assert_eq!(
            transcript_path_from_hook_input(r#"{"hook_event_name":"Stop"}"#),
            None
        );
    }
}
