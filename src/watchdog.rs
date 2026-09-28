//! Deterministic classification and scan decisions for the blocked-agent watchdog.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::api::schema::AgentStatus;

pub(crate) const WATCHDOG_SOURCE: &str = "watchdog";
const PROMPT_WINDOW_LINES: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    Blocked(String),
    NotBlocked,
    Ambiguous(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PaneMemory {
    pub hash: u64,
    pub since: u64,
}

pub(crate) type Memory = HashMap<String, PaneMemory>;

#[derive(Debug, Clone)]
pub(crate) struct PaneSample {
    pub pane_id: String,
    pub agent: Option<String>,
    pub status: AgentStatus,
    pub tail: String,
    pub read_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DecisionStatus {
    Blocked,
    Ambiguous,
    Ok,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PaneDecision {
    pub pane_id: String,
    pub agent: String,
    pub agent_status: AgentStatus,
    pub status: DecisionStatus,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write_error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ScanResult {
    pub decisions: Vec<PaneDecision>,
    pub model_calls: usize,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ScanOptions {
    pub stall_secs: u64,
    pub max_model_calls: usize,
    pub no_model: bool,
    pub dry_run: bool,
}

pub(crate) fn status_in_scope(status: AgentStatus) -> bool {
    matches!(
        status,
        AgentStatus::Working | AgentStatus::Idle | AgentStatus::Unknown | AgentStatus::Stale
    )
}

const PERMISSION_MARKERS: &[&str] = &[
    "(y/n)",
    "[y/n]",
    "(yes/no)",
    "[yes/no]",
    "do you want to proceed",
    "do you want to make this edit",
    "do you want to create",
    "do you want to allow",
    "allow this command",
    "allow command?",
    "approve this",
    "requires approval",
    "waiting for approval",
    "permission required",
    "grant permission",
    "press enter to continue",
    "press enter to confirm",
    "waiting for input",
    "waiting for your input",
    "waiting for user input",
    "awaiting your input",
    "enter your choice",
    "select an option",
];

const ERROR_MARKERS: &[&str] = &[
    "you've hit your usage limit",
    "usage limit reached",
    "rate limit exceeded",
    "authentication failed",
    "please log in",
    "please run /login",
    "session expired",
    "fatal error",
    "panicked at",
];

const SOFT_MARKERS: &[&str] = &[
    "error:",
    "failed",
    "should i ",
    "would you like",
    "do you want",
    "let me know",
    "which option",
    "please confirm",
];

fn tail_window(tail: &str) -> Vec<String> {
    let lines: Vec<&str> = tail
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let start = lines.len().saturating_sub(PROMPT_WINDOW_LINES);
    lines[start..]
        .iter()
        .map(|line| line.trim().to_lowercase())
        .collect()
}

pub(crate) fn classify_text(tail: &str) -> Verdict {
    let window = tail_window(tail);
    if window.is_empty() {
        return Verdict::NotBlocked;
    }
    for line in &window {
        if let Some(marker) = PERMISSION_MARKERS
            .iter()
            .find(|marker| line.contains(**marker))
        {
            return Verdict::Blocked(format!("prompt: {marker}"));
        }
    }
    for line in &window {
        if let Some(marker) = ERROR_MARKERS.iter().find(|marker| line.contains(**marker)) {
            return Verdict::Blocked(format!("error: {marker}"));
        }
    }
    let recent = &window[window.len().saturating_sub(4)..];
    if recent.iter().any(|line| line.ends_with('?')) {
        return Verdict::Ambiguous("question near tail".into());
    }
    if let Some(marker) = recent
        .iter()
        .find_map(|line| SOFT_MARKERS.iter().find(|marker| line.contains(**marker)))
    {
        return Verdict::Ambiguous(format!("soft marker: {}", marker.trim()));
    }
    Verdict::NotBlocked
}

/// Stable FNV-1a hash so fingerprints survive rebuilds.
pub(crate) fn tail_hash(tail: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in tail.trim_end().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Update remembered fingerprint; return seconds the tail has been unchanged.
pub(crate) fn observe(memory: &mut Memory, pane_id: &str, hash: u64, now: u64) -> u64 {
    let entry = memory
        .entry(pane_id.to_string())
        .or_insert(PaneMemory { hash, since: now });
    if entry.hash != hash {
        *entry = PaneMemory { hash, since: now };
    }
    now.saturating_sub(entry.since)
}

pub(crate) fn stage1(
    status: AgentStatus,
    tail: &str,
    unchanged_secs: u64,
    stall_secs: u64,
) -> Verdict {
    match classify_text(tail) {
        Verdict::NotBlocked => {}
        other => return other,
    }
    if status == AgentStatus::Working && unchanged_secs >= stall_secs {
        return Verdict::Blocked(format!(
            "stalled: tail unchanged {}m while working",
            unchanged_secs / 60
        ));
    }
    Verdict::NotBlocked
}

pub(crate) fn classifier_prompt(agent: &str, tail: &str) -> String {
    let lines: Vec<&str> = tail.lines().collect();
    let start = lines.len().saturating_sub(40);
    let mut excerpt = lines[start..].join("\n");
    if excerpt.len() > 4000 {
        let mut cut = excerpt.len() - 4000;
        while !excerpt.is_char_boundary(cut) {
            cut += 1;
        }
        excerpt = excerpt[cut..].to_string();
    }
    format!(
        "You classify a terminal running the coding agent `{agent}`. Do not run tools. \
Decide whether the agent is BLOCKED: it cannot continue without a human (a permission or \
yes/no prompt, a question to the user, an unrecoverable error, or it is stuck). It is NOT \
blocked if it is working, finished its turn with a report, or idles at an empty prompt with \
nothing asked. Answer on one line, exactly `BLOCKED: <short reason>` or `NOT_BLOCKED`.\n\n\
<tail>\n{excerpt}\n</tail>\n"
    )
}

pub(crate) fn parse_classifier_reply(reply: &str) -> Verdict {
    for line in reply.lines().rev() {
        let line = line.trim().trim_matches('`');
        if line == "NOT_BLOCKED" || line.starts_with("NOT_BLOCKED ") {
            return Verdict::NotBlocked;
        }
        if line == "BLOCKED" || line.starts_with("BLOCKED:") {
            let rest = line.strip_prefix("BLOCKED").unwrap_or_default();
            let reason = rest.trim_start_matches(':').trim();
            let reason = if reason.is_empty() { "model" } else { reason };
            let reason: String = reason.chars().take(80).collect();
            return Verdict::Blocked(format!("model: {reason}"));
        }
    }
    Verdict::NotBlocked
}

/// Resolve a scan while keeping model execution and blocked reports injectable.
/// The write callback is the only path that can report a blocked pane.
pub(crate) fn scan_decisions<M, W>(
    listed_pane_ids: &[String],
    samples: &[PaneSample],
    memory: &mut Memory,
    now: u64,
    options: ScanOptions,
    mut classify_model: M,
    mut write_blocked: W,
) -> ScanResult
where
    M: FnMut(&str, &str) -> Result<Verdict, String>,
    W: FnMut(&str, &str, &str) -> std::io::Result<()>,
{
    let listed: HashSet<&str> = listed_pane_ids.iter().map(String::as_str).collect();
    memory.retain(|pane_id, _| listed.contains(pane_id.as_str()));

    let mut result = ScanResult::default();
    for sample in samples {
        if !status_in_scope(sample.status) {
            continue;
        }
        let Some(agent) = sample.agent.as_deref() else {
            continue;
        };

        let mut verdict = if let Some(error) = &sample.read_error {
            Verdict::Ambiguous(format!("pane read failed: {error}"))
        } else {
            let unchanged_secs = observe(memory, &sample.pane_id, tail_hash(&sample.tail), now);
            stage1(
                sample.status,
                &sample.tail,
                unchanged_secs,
                options.stall_secs,
            )
        };

        if sample.read_error.is_none() {
            if let Verdict::Ambiguous(reason) = &verdict {
                if options.no_model {
                    verdict = Verdict::Ambiguous(format!("{reason}; model disabled"));
                } else if result.model_calls >= options.max_model_calls {
                    verdict = Verdict::Ambiguous(format!("{reason}; model-call cap reached"));
                } else {
                    result.model_calls += 1;
                    verdict = classify_model(agent, &sample.tail).unwrap_or_else(|error| {
                        Verdict::Ambiguous(format!("model failed: {error}"))
                    });
                }
            }
        }

        let (status, reason) = match verdict {
            Verdict::Blocked(reason) => (DecisionStatus::Blocked, reason),
            Verdict::NotBlocked => (DecisionStatus::Ok, "no blocking evidence".into()),
            Verdict::Ambiguous(reason) => (DecisionStatus::Ambiguous, reason),
        };
        let mut decision = PaneDecision {
            pane_id: sample.pane_id.clone(),
            agent: agent.to_string(),
            agent_status: sample.status,
            status,
            reason: reason.replace('\n', " ").replace('\r', " "),
            write_error: None,
        };
        if status == DecisionStatus::Blocked && !options.dry_run {
            if let Err(error) = write_blocked(&sample.pane_id, agent, &decision.reason) {
                decision.write_error = Some(error.to_string());
            }
        }
        result.decisions.push(decision);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchdog_permission_prompt_is_blocked() {
        let tail = "Bash command\n  rm -rf build\nDo you want to proceed?\n❯ 1. Yes\n  2. No\n";
        assert!(
            matches!(classify_text(tail), Verdict::Blocked(reason) if reason.contains("proceed"))
        );
    }

    #[test]
    fn watchdog_yn_prompt_is_blocked() {
        assert!(matches!(
            classify_text("Overwrite file? [y/N]"),
            Verdict::Blocked(_)
        ));
    }

    #[test]
    fn watchdog_usage_limit_error_is_blocked() {
        let tail = "ERROR: You've hit your usage limit. Try again later.";
        assert!(
            matches!(classify_text(tail), Verdict::Blocked(reason) if reason.starts_with("error"))
        );
    }

    #[test]
    fn watchdog_question_at_tail_is_ambiguous() {
        let tail = "I found two options.\nShould I go with the smaller patch?";
        assert!(matches!(classify_text(tail), Verdict::Ambiguous(_)));
    }

    #[test]
    fn watchdog_soft_marker_near_tail_is_ambiguous() {
        assert!(matches!(
            classify_text("The command finished.\nPlease confirm the next step."),
            Verdict::Ambiguous(reason) if reason.contains("please confirm")
        ));
    }

    #[test]
    fn watchdog_plain_progress_is_not_blocked() {
        let tail = "Reading src/main.rs\nRunning cargo test\n✻ Working… (12s)";
        assert_eq!(classify_text(tail), Verdict::NotBlocked);
        assert_eq!(classify_text("   \n\n"), Verdict::NotBlocked);
    }

    #[test]
    fn watchdog_old_question_scrolled_away_is_not_ambiguous() {
        let mut tail = String::from("Should I continue?\n");
        for i in 0..6 {
            tail.push_str(&format!("step {i} done\n"));
        }
        assert_eq!(classify_text(&tail), Verdict::NotBlocked);
    }

    #[test]
    fn watchdog_stall_only_counts_while_working() {
        assert!(
            matches!(stage1(AgentStatus::Working, "compiling", 900, 600), Verdict::Blocked(reason) if reason.starts_with("stalled"))
        );
        assert_eq!(
            stage1(AgentStatus::Working, "compiling", 300, 600),
            Verdict::NotBlocked
        );
        assert_eq!(
            stage1(AgentStatus::Idle, "compiling", 900, 600),
            Verdict::NotBlocked
        );
    }

    #[test]
    fn watchdog_memory_tracks_unchanged_duration_and_resets() {
        let mut memory = Memory::new();
        assert_eq!(observe(&mut memory, "p1", 1, 100), 0);
        assert_eq!(observe(&mut memory, "p1", 1, 400), 300);
        assert_eq!(observe(&mut memory, "p1", 2, 500), 0);
        assert_eq!(observe(&mut memory, "p1", 2, 560), 60);
    }

    #[test]
    fn watchdog_tail_hash_ignores_trailing_whitespace() {
        assert_eq!(tail_hash("abc\n\n"), tail_hash("abc"));
        assert_ne!(tail_hash("abc"), tail_hash("abd"));
    }

    #[test]
    fn watchdog_scope_excludes_blocked_and_done() {
        assert!(status_in_scope(AgentStatus::Working));
        assert!(!status_in_scope(AgentStatus::Blocked));
        assert!(!status_in_scope(AgentStatus::Done));
    }

    #[test]
    fn watchdog_classifier_reply_parsing() {
        assert_eq!(parse_classifier_reply("NOT_BLOCKED"), Verdict::NotBlocked);
        assert_eq!(
            parse_classifier_reply("thinking...\nBLOCKED: asks which branch"),
            Verdict::Blocked("model: asks which branch".into())
        );
        assert_eq!(parse_classifier_reply("garbage"), Verdict::NotBlocked);
    }

    #[test]
    fn watchdog_classifier_prompt_keeps_only_the_tail() {
        let tail: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let prompt = classifier_prompt("claude", &tail);
        assert!(prompt.contains("line 99"));
        assert!(!prompt.contains("line 10\n"));
    }

    #[test]
    fn watchdog_scan_calls_capped_classifier_and_dry_run_never_writes() {
        let samples = (0..3)
            .map(|i| PaneSample {
                pane_id: format!("p{i}"),
                agent: Some("claude".into()),
                status: AgentStatus::Working,
                tail: "Should I continue?".into(),
                read_error: None,
            })
            .collect::<Vec<_>>();
        let listed = samples
            .iter()
            .map(|sample| sample.pane_id.clone())
            .collect::<Vec<_>>();
        let mut memory = Memory::new();
        let mut classifier_calls = 0;
        let mut writes = 0;
        let result = scan_decisions(
            &listed,
            &samples,
            &mut memory,
            100,
            ScanOptions {
                stall_secs: 600,
                max_model_calls: 1,
                no_model: false,
                dry_run: true,
            },
            |_, _| {
                classifier_calls += 1;
                Ok(Verdict::Blocked("model found prompt".into()))
            },
            |_, _, _| {
                writes += 1;
                Ok(())
            },
        );

        assert_eq!(result.model_calls, 1);
        assert_eq!(classifier_calls, 1);
        assert_eq!(writes, 0);
        assert_eq!(result.decisions[0].status, DecisionStatus::Blocked);
        assert_eq!(result.decisions[1].status, DecisionStatus::Ambiguous);
        assert!(result.decisions[1].reason.contains("cap reached"));
    }

    #[test]
    fn watchdog_scan_keeps_read_failures_ambiguous_without_model_calls() {
        let sample = PaneSample {
            pane_id: "p1".into(),
            agent: Some("claude".into()),
            status: AgentStatus::Working,
            tail: String::new(),
            read_error: Some("unavailable".into()),
        };
        let mut memory = Memory::new();
        let mut classifier_calls = 0;
        let result = scan_decisions(
            &["p1".into()],
            &[sample],
            &mut memory,
            100,
            ScanOptions {
                stall_secs: 600,
                max_model_calls: 5,
                no_model: false,
                dry_run: true,
            },
            |_, _| {
                classifier_calls += 1;
                Ok(Verdict::NotBlocked)
            },
            |_, _, _| Ok(()),
        );

        assert_eq!(classifier_calls, 0);
        assert_eq!(result.model_calls, 0);
        assert_eq!(result.decisions[0].status, DecisionStatus::Ambiguous);
        assert!(result.decisions[0].reason.contains("read failed"));
    }

    #[test]
    fn watchdog_scan_prunes_memory_and_writes_blocked_once() {
        let sample = PaneSample {
            pane_id: "p1".into(),
            agent: Some("claude".into()),
            status: AgentStatus::Idle,
            tail: "Do you want to proceed?".into(),
            read_error: None,
        };
        let mut memory = HashMap::from([
            ("p1".into(), PaneMemory { hash: 1, since: 10 }),
            ("gone".into(), PaneMemory { hash: 2, since: 10 }),
        ]);
        let mut writes = Vec::new();
        let result = scan_decisions(
            &["p1".into()],
            &[sample],
            &mut memory,
            100,
            ScanOptions {
                stall_secs: 600,
                max_model_calls: 0,
                no_model: false,
                dry_run: false,
            },
            |_, _| Err("must not run".into()),
            |pane_id, agent, reason| {
                writes.push((pane_id.to_string(), agent.to_string(), reason.to_string()));
                Ok(())
            },
        );

        assert_eq!(result.decisions[0].status, DecisionStatus::Blocked);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0, "p1");
        assert!(!memory.contains_key("gone"));
    }
}
