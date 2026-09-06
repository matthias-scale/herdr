//! Background probes for the settings Providers and Integrations sections.
//!
//! Every probe shells out to a CLI, which can block for seconds when the tool
//! is a shim, needs the network, or is missing from a slow filesystem. None of
//! that may happen while a frame is being built, so the probes run once per
//! session on a worker thread and the render path only reads the cached result.

use std::time::{Duration, Instant};

use super::App;

/// How long a single probe may take before it is killed and reported as such.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Which settings section a probe belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolProbeKind {
    /// An agent CLI Herdr can launch.
    Provider,
    /// An external service Herdr reads work state from.
    Integration,
}

/// What a probe found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolProbeOutcome {
    /// The tool ran and reported a usable state.
    Ready,
    /// The tool is installed but not usable yet, typically unauthenticated.
    NeedsAttention,
    /// The tool is not on PATH.
    Missing,
    /// The tool did not answer within `PROBE_TIMEOUT`.
    TimedOut,
}

impl ToolProbeOutcome {
    pub fn marker(self) -> &'static str {
        match self {
            Self::Ready => "✓",
            Self::NeedsAttention => "!",
            Self::Missing => "–",
            Self::TimedOut => "⏱",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolProbe {
    pub label: &'static str,
    pub kind: ToolProbeKind,
    pub outcome: ToolProbeOutcome,
    /// One short line shown next to the label.
    pub detail: String,
}

/// The session-wide probe cache. Probes are not re-run on every visit: the
/// answer only changes when the operator installs or authenticates something,
/// which is not something a settings screen should poll for.
#[derive(Debug, Default)]
pub enum ToolProbeState {
    #[default]
    Idle,
    Running,
    Ready(Vec<ToolProbe>),
}

struct ProbeSpec {
    label: &'static str,
    kind: ToolProbeKind,
    program: &'static str,
    args: &'static [&'static str],
    /// Non-zero exit means installed but not ready, rather than broken.
    non_zero_needs_attention: bool,
}

const PROBE_SPECS: &[ProbeSpec] = &[
    ProbeSpec {
        label: "claude",
        kind: ToolProbeKind::Provider,
        program: "claude",
        args: &["--version"],
        non_zero_needs_attention: false,
    },
    ProbeSpec {
        label: "codex",
        kind: ToolProbeKind::Provider,
        program: "codex",
        args: &["--version"],
        non_zero_needs_attention: false,
    },
    ProbeSpec {
        label: "github",
        kind: ToolProbeKind::Integration,
        program: "gh",
        args: &["auth", "status"],
        non_zero_needs_attention: true,
    },
    ProbeSpec {
        label: "linear",
        kind: ToolProbeKind::Integration,
        program: "linearis",
        args: &["auth", "status"],
        non_zero_needs_attention: true,
    },
];

/// Collapse command output to the one line worth showing in a settings row.
pub(crate) fn probe_detail_line(output: &str) -> String {
    output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .chars()
        .take(48)
        .collect()
}

/// Run one command, killing it once `timeout` has passed.
///
/// `Command::output()` waits forever, so a hung CLI would pin the worker
/// thread for the rest of the session and the section would never leave
/// `checking…`.
fn run_with_timeout(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<std::process::Output, ProbeFailure> {
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(ProbeFailure::Missing)
        }
        Err(err) => return Err(ProbeFailure::Failed(err.to_string())),
    };

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ProbeFailure::TimedOut);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(err) => return Err(ProbeFailure::Failed(err.to_string())),
        }
    }

    child
        .wait_with_output()
        .map_err(|err| ProbeFailure::Failed(err.to_string()))
}

enum ProbeFailure {
    Missing,
    TimedOut,
    Failed(String),
}

fn run_probe(spec: &ProbeSpec) -> ToolProbe {
    let (outcome, detail) = match run_with_timeout(spec.program, spec.args, PROBE_TIMEOUT) {
        Ok(output) => {
            let text = if output.stdout.is_empty() {
                String::from_utf8_lossy(&output.stderr).into_owned()
            } else {
                String::from_utf8_lossy(&output.stdout).into_owned()
            };
            let detail = probe_detail_line(&text);
            if output.status.success() {
                (ToolProbeOutcome::Ready, detail)
            } else if spec.non_zero_needs_attention {
                (
                    ToolProbeOutcome::NeedsAttention,
                    if detail.is_empty() {
                        "not authenticated".to_string()
                    } else {
                        detail
                    },
                )
            } else {
                (ToolProbeOutcome::NeedsAttention, detail)
            }
        }
        Err(ProbeFailure::Missing) => (
            ToolProbeOutcome::Missing,
            format!("{} not on PATH", spec.program),
        ),
        Err(ProbeFailure::TimedOut) => (
            ToolProbeOutcome::TimedOut,
            format!("{} did not answer in 3s", spec.program),
        ),
        Err(ProbeFailure::Failed(err)) => (ToolProbeOutcome::NeedsAttention, probe_detail_line(&err)),
    };
    ToolProbe {
        label: spec.label,
        kind: spec.kind,
        outcome,
        detail,
    }
}

impl App {
    /// Start the probes once per session. Cheap and idempotent, so section
    /// entry can call it unconditionally.
    pub(crate) fn start_tool_probes_if_needed(&mut self) {
        if !matches!(self.state.tool_probes, ToolProbeState::Idle) {
            return;
        }
        self.state.tool_probes = ToolProbeState::Running;
        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let probes = PROBE_SPECS.iter().map(run_probe).collect::<Vec<_>>();
            let _ = event_tx.blocking_send(crate::events::AppEvent::ToolProbesFinished { probes });
        });
    }

    pub(crate) fn handle_tool_probes_finished(&mut self, probes: Vec<ToolProbe>) -> bool {
        self.state.tool_probes = ToolProbeState::Ready(probes);
        true
    }
}

impl crate::app::state::AppState {
    /// Probes for one settings section, in declaration order.
    pub(crate) fn tool_probes_for(&self, kind: ToolProbeKind) -> Vec<&ToolProbe> {
        match &self.tool_probes {
            ToolProbeState::Ready(probes) => {
                probes.iter().filter(|probe| probe.kind == kind).collect()
            }
            ToolProbeState::Idle | ToolProbeState::Running => Vec::new(),
        }
    }

    pub(crate) fn tool_probes_pending(&self) -> bool {
        matches!(
            self.tool_probes,
            ToolProbeState::Idle | ToolProbeState::Running
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_detail_line_takes_the_first_non_empty_line() {
        assert_eq!(probe_detail_line("\n\n  2.1.4 (Claude Code)\nmore\n"), "2.1.4 (Claude Code)");
        assert_eq!(probe_detail_line(""), "");
    }

    #[test]
    fn a_missing_program_is_reported_as_missing_not_as_a_failure() {
        let probe = run_probe(&ProbeSpec {
            label: "nothing",
            kind: ToolProbeKind::Provider,
            program: "herdr-probe-that-does-not-exist",
            args: &[],
            non_zero_needs_attention: false,
        });
        assert_eq!(probe.outcome, ToolProbeOutcome::Missing);
    }

    #[test]
    fn a_hanging_program_is_killed_and_reported_as_timed_out() {
        let started = Instant::now();
        let result = run_with_timeout("sleep", &["30"], Duration::from_millis(150));
        assert!(matches!(result, Err(ProbeFailure::TimedOut)));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn probes_are_split_by_section_and_pending_until_ready() {
        let mut state = crate::app::state::AppState::test_new();
        assert!(state.tool_probes_pending());
        assert!(state.tool_probes_for(ToolProbeKind::Provider).is_empty());

        state.tool_probes = ToolProbeState::Ready(vec![
            ToolProbe {
                label: "claude",
                kind: ToolProbeKind::Provider,
                outcome: ToolProbeOutcome::Ready,
                detail: "2.1.4".into(),
            },
            ToolProbe {
                label: "github",
                kind: ToolProbeKind::Integration,
                outcome: ToolProbeOutcome::NeedsAttention,
                detail: "not authenticated".into(),
            },
        ]);

        assert!(!state.tool_probes_pending());
        assert_eq!(state.tool_probes_for(ToolProbeKind::Provider).len(), 1);
        assert_eq!(
            state.tool_probes_for(ToolProbeKind::Integration)[0].label,
            "github"
        );
    }
}
