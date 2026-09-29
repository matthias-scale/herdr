//! Shared, deterministic evidence extraction for pane and worker watchdogs.
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

static TIMERS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"\((?:[^()]*\d+\s*(?:ms|s|m|h)\b[^()]*|[^()]*esc to interrupt[^()]*)\)",
        r"\b\d+(?:\.\d+)?\s*[kKmM]?\s*tokens?\b",
        r"\b\d+h\s*\d+m\b|\b\d+m\s*\d+s\b|\b\d+(?:\.\d+)?\s*(?:ms|s)\b",
        r"\b\d{1,2}:\d{2}(?::\d{2})?\b",
        r"\b\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}(?::\d{2})?(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?",
        r"\b\d{1,3}(?:\.\d+)?\s*%",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("static regex"))
    .collect()
});
static HEARTBEAT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\[?(?:heartbeat|keepalive|still (?:running|working)|tick)\b")
        .expect("static regex")
});
static YESNO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)[\[(]\s*(?:y\s*/\s*n|yes\s*/\s*no)\s*[\])]").expect("static regex")
});
static OPTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:[❯›>▌│|]\s*)?(?:\d[.)]|\[\d\])\s+\S").expect("static regex"));
static DIALOG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:do you want to|allow (?:this|command|the)|approve|grant permission|permission required|requires approval|choose (?:a|an|the)|select (?:a|an|the))\b").expect("static regex")
});
static HINT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:esc to cancel|enter to (?:confirm|select)|press enter|tab to amend|\(esc\)|↑/↓)",
    )
    .expect("static regex")
});
static ACCOUNT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)you've hit your usage limit|usage limit reached|hit your usage limit|please (?:log ?in|run /login)|authentication failed|session expired|change (?:your )?account|upgrade your plan").expect("static regex")
});
static PROGRESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:new turn|continuing|i am continuing|i'm continuing|working on|running |reading |writing |editing |compil|step \d|resum)").expect("static regex")
});
static RETRY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:retry(?:ing)?|reconnecting|backing off|backoff)\b[^\n]*?\bin\s+(\d+(?:\.\d+)?)\s*(ms|s|sec|secs|seconds?|m|min|minutes?)\b").expect("static regex")
});

pub(crate) fn stable_hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf29ce484222325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    })
}
fn spinner(c: char) -> bool {
    matches!(
        c,
        '\u{2800}'
            ..='\u{28ff}'
                | '✻'
                | '✶'
                | '✳'
                | '✢'
                | '✽'
                | '·'
                | '◐'
                | '◓'
                | '◑'
                | '◒'
                | '⏺'
                | '●'
                | '○'
                | '◌'
    )
}
pub(crate) fn normalize_line(line: &str) -> String {
    let mut s: String = line.chars().filter(|c| !spinner(*c)).collect();
    for p in TIMERS.iter() {
        s = p.replace_all(&s, "").into_owned();
    }
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if HEARTBEAT.is_match(&s) {
        return String::new();
    }
    s.trim_matches(|c: char| c.is_whitespace() || matches!(c, '|' | '/' | '-' | '\\' | '.' | '…'))
        .to_owned()
}
pub(crate) fn semantic_lines(text: &str) -> Vec<String> {
    let v: Vec<_> = text
        .lines()
        .map(normalize_line)
        .filter(|s| !s.is_empty())
        .collect();
    v[v.len().saturating_sub(40)..].to_vec()
}
pub(crate) fn semantic_hash(text: &str) -> u64 {
    stable_hash(&semantic_lines(text).join("\n"))
}
fn bottom(text: &str, n: usize) -> Vec<String> {
    let v: Vec<_> = text
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    v[v.len().saturating_sub(n)..].to_vec()
}
fn quoted(s: &str) -> bool {
    let s = s.trim();
    s.starts_with('>')
        || s.starts_with("//")
        || ['"', '\'', '`']
            .iter()
            .any(|q| s.starts_with(*q) && s.ends_with(*q))
        || (s.starts_with('‘') && s.ends_with('’'))
        || (s.starts_with('“') && s.ends_with('”'))
        || (s.contains(": \"") && s.ends_with('"'))
        || (s.contains(": ‘") && s.ends_with('’'))
        || (s.contains(": “") && s.ends_with('”'))
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PromptKind {
    YesNo,
    Dialog,
    AccountAction,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivePrompt {
    pub kind: PromptKind,
    pub line: String,
}
pub(crate) fn active_prompt(text: &str) -> Option<ActivePrompt> {
    let w = bottom(text, 6);
    if w.is_empty() {
        return None;
    }
    let last = w.len() - 1;
    for (i, l) in w.iter().enumerate().rev() {
        if !quoted(l)
            && i + 2 > last
            && YESNO.is_match(l)
            && !w[i + 1..].iter().any(|line| PROGRESS.is_match(line))
        {
            return Some(ActivePrompt {
                kind: PromptKind::YesNo,
                line: l.clone(),
            });
        }
    }
    for (i, l) in w.iter().enumerate() {
        if quoted(l) || !DIALOG.is_match(l) {
            continue;
        }
        let rest = &w[i + 1..];
        if rest.iter().filter(|s| OPTION.is_match(s)).count() >= 2
            && rest
                .iter()
                .all(|s| OPTION.is_match(s) || HINT.is_match(s) || s.len() < 3)
        {
            return Some(ActivePrompt {
                kind: PromptKind::Dialog,
                line: l.clone(),
            });
        }
    }
    for (i, l) in w.iter().enumerate() {
        if !quoted(l) && ACCOUNT.is_match(l) && !w[i + 1..].iter().any(|s| PROGRESS.is_match(s)) {
            return Some(ActivePrompt {
                kind: PromptKind::AccountAction,
                line: l.clone(),
            });
        }
    }
    None
}
pub(crate) fn prose_question(text: &str) -> Option<String> {
    bottom(text, 2).into_iter().rev().find(|l| {
        !quoted(l)
            && (l.ends_with('?')
                || [
                    "let me know",
                    "please confirm",
                    "which option",
                    "would you like",
                ]
                .iter()
                .any(|m| l.to_ascii_lowercase().contains(m)))
    })
}
pub(crate) fn scheduled_retry_secs(text: &str) -> Option<u64> {
    bottom(text, 3)
        .iter()
        .rev()
        .filter(|l| !quoted(l))
        .find_map(|l| {
            let c = RETRY.captures(l)?;
            let n = c[1].parse::<f64>().ok()?;
            let unit = c[2].to_ascii_lowercase();
            Some(
                (if unit == "ms" {
                    n / 1000.
                } else if unit.starts_with('m') {
                    n * 60.
                } else {
                    n
                })
                .ceil()
                .max(1.) as u64,
            )
        })
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProcSample {
    pub pid: u32,
    pub ppid: u32,
    pub pgid: u32,
    pub state: char,
    pub elapsed_secs: u64,
    pub cpu_ms: u64,
    pub name: String,
}
fn duration(s: &str) -> Option<u64> {
    let (d, c) = match s.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, s),
    };
    let p: Vec<_> = c.split(':').collect();
    let (h, m, t): (u64, u64, &str) = match p.as_slice() {
        [m, s] => (0, m.parse().ok()?, *s),
        [h, m, s] => (h.parse().ok()?, m.parse().ok()?, *s),
        _ => return None,
    };
    Some(((d * 24 + h) * 3600 + m * 60) * 1000 + (t.parse::<f64>().ok()? * 1000.).round() as u64)
}
pub(crate) fn parse_ps_rows(text: &str) -> Vec<ProcSample> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let pid = f.next()?.parse().ok()?;
            let ppid = f.next()?.parse().ok()?;
            let pgid = f.next()?.parse().ok()?;
            let state = f.next()?.chars().next()?;
            let elapsed_secs = duration(f.next()?)? / 1000;
            let cpu_ms = duration(f.next()?)?;
            let name = f.collect::<Vec<_>>().join(" ");
            (!name.is_empty()).then_some(ProcSample {
                pid,
                ppid,
                pgid,
                state,
                elapsed_secs,
                cpu_ms,
                name,
            })
        })
        .collect()
}
pub(crate) fn descendants(table: &[ProcSample], root: u32) -> Vec<ProcSample> {
    let mut out = Vec::new();
    let mut todo = vec![root];
    while let Some(parent) = todo.pop() {
        for p in table.iter().filter(|p| p.ppid == parent) {
            if p.pid != root && !out.iter().any(|x: &ProcSample| x.pid == p.pid) {
                todo.push(p.pid);
                out.push(p.clone())
            }
        }
    }
    out
}
fn base(name: &str) -> String {
    name.rsplit('/')
        .next()
        .unwrap_or(name)
        .trim_start_matches('-')
        .to_ascii_lowercase()
}
pub(crate) fn current_tool_processes(
    ps: &[ProcSample],
    exclude: Option<u32>,
    progress_age_secs: u64,
) -> Vec<&ProcSample> {
    ps.iter()
        .filter(|p| {
            Some(p.pid) != exclude
                && p.state != 'Z'
                && !matches!(
                    base(&p.name).as_str(),
                    "claude" | "codex" | "gemini" | "opencode" | "amp" | "droid"
                )
                && (!matches!(
                    base(&p.name).as_str(),
                    "sh" | "bash" | "zsh" | "dash" | "fish" | "login"
                ) || p.state == 'T')
                && p.elapsed_secs <= progress_age_secs.saturating_add(120)
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalization_ignores_animation_counters_and_heartbeat() {
        assert_eq!(
            semantic_hash("Compiling A\n⠋ Working… (12s · 1.2k tokens)\nheartbeat 10s"),
            semantic_hash("Compiling A\n⠙ Working… (47s · 3.9k tokens)\nheartbeat 20s")
        );
    }
    #[test]
    fn prompt_and_history_detection() {
        assert_eq!(
            active_prompt("Running tests\nOverwrite? [y/n]")
                .unwrap()
                .kind,
            PromptKind::YesNo
        );
        assert!(active_prompt("> Do you want to continue? [y/n]\nNew turn started").is_none());
        assert_eq!(
            active_prompt("Do you want to proceed?\n❯ 1. Yes\n2. No\n(esc)")
                .unwrap()
                .kind,
            PromptKind::Dialog
        );
        assert_eq!(
            active_prompt("Usage limit reached; change account")
                .unwrap()
                .kind,
            PromptKind::AccountAction
        );
    }
    #[test]
    fn retries_and_ps_formats() {
        assert_eq!(scheduled_retry_secs("retry in 1500ms"), Some(2));
        assert_eq!(scheduled_retry_secs("backoff in 2 min"), Some(120));
        let p = parse_ps_rows(
            "101 1 101 Ss 01-02:03:04 00:00:01 bash\n202 101 101 R+ 00:07 0:00.52 cargo test",
        );
        assert_eq!(p[0].elapsed_secs, 93784);
        assert_eq!(p[1].cpu_ms, 520);
    }

    #[test]
    fn quoted_questions_and_old_prompts_are_not_active() {
        assert!(active_prompt("> overwrite? [y/n]\nNew turn started").is_none());
        assert!(active_prompt("‘Should I deploy?’\nContinuing with tests").is_none());
        assert!(active_prompt("Usage limit reached\nRunning tests").is_none());
        assert_eq!(
            active_prompt("Overwrite? [Y/n]").map(|p| p.kind),
            Some(PromptKind::YesNo)
        );
    }

    #[test]
    fn tool_process_selection_excludes_agents_shells_and_old_helpers() {
        let samples = parse_ps_rows(
            "1 0 1 R 00:05 0:00.01 codex\n2 1 1 S 00:04 0:00.01 bash\n3 1 1 R 00:03 0:00.52 cargo test\n4 1 1 S 10:00 0:00.00 mcp-server\n5 1 1 Z 00:01 0:00.00 rustc",
        );
        let tools = current_tool_processes(&samples, Some(1), 10);
        assert_eq!(tools.iter().map(|p| p.pid).collect::<Vec<_>>(), [3]);
    }
}
