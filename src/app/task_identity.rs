//! Runtime-only first-task authority. All process probes happen on API events,
//! never while rendering, computing layout, or reading pane-scaled snapshots.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::agent_resume::{AgentSessionRef, AgentSessionRefKind};
use crate::platform::ProcessIdentity;
use crate::terminal::TerminalId;

pub(crate) trait ProcessInspector: Send + Sync {
    fn process(&self, pid: u32) -> Option<ProcessIdentity>;
    fn foreground(&self, child_pid: Option<u32>) -> Option<ProcessIdentity>;
}

struct KernelInspector;
impl ProcessInspector for KernelInspector {
    fn process(&self, pid: u32) -> Option<ProcessIdentity> {
        crate::platform::process_identity(pid)
    }
    fn foreground(&self, child_pid: Option<u32>) -> Option<ProcessIdentity> {
        let child_pid = child_pid?;
        let shell = self.process(child_pid)?;
        let job = crate::platform::foreground_job(child_pid)?;
        let mut agent = None;
        for process in &job.processes {
            let identity = self.process(process.pid)?;
            // Shell command strings are presentation hints, not process authority.
            if let Some(identified) = identity.agent() {
                if identified != crate::detect::Agent::Claude || agent.is_some() {
                    return None;
                }
                if identity.tty == 0
                    || identity.tty != shell.tty
                    || identity.process_group_id != job.process_group_id
                {
                    return None;
                }
                agent = Some(identity);
            }
        }
        let agent = agent?;
        // Check geometry and lifetime after the process-group enumeration too.
        if self.process(child_pid)? != shell
            || crate::platform::foreground_process_group_id(child_pid)? != job.process_group_id
            || self.process(agent.pid)? != agent
        {
            return None;
        }
        Some(agent)
    }
}

type ProcessLifetime = (u32, (u64, u64));

fn lifetime(process: &ProcessIdentity) -> ProcessLifetime {
    (process.pid, process.birth)
}

fn same_binding_process(left: &ProcessIdentity, right: &ProcessIdentity) -> bool {
    lifetime(left) == lifetime(right)
        && left.tty == right.tty
        && left.agent() == Some(crate::detect::Agent::Claude)
        && right.agent() == Some(crate::detect::Agent::Claude)
}

#[derive(Clone)]
struct Attestation {
    session: String,
    process: ProcessIdentity,
    epoch: u64,
}

pub(crate) struct TaskProcessIdentity {
    pub(crate) inspector: Arc<dyn ProcessInspector>,
    attestations: HashMap<TerminalId, Attestation>,
    burned: HashMap<TerminalId, HashSet<ProcessLifetime>>,
    retired_sessions: HashMap<TerminalId, HashSet<String>>,
}

impl Default for TaskProcessIdentity {
    fn default() -> Self {
        Self {
            inspector: Arc::new(KernelInspector),
            attestations: HashMap::new(),
            burned: HashMap::new(),
            retired_sessions: HashMap::new(),
        }
    }
}

impl TaskProcessIdentity {
    fn invalidate(&mut self, id: &TerminalId) {
        if let Some(previous) = self.attestations.remove(id) {
            self.retired_sessions
                .entry(id.clone())
                .or_default()
                .insert(previous.session.clone());
            self.burned
                .entry(id.clone())
                .or_default()
                .insert(lifetime(&previous.process));
        }
    }

    fn descendant(&self, peer: &ProcessIdentity, target: &ProcessIdentity) -> bool {
        let mut current = peer.clone();
        let mut path = Vec::new();
        for _ in 0..64 {
            if self.inspector.process(current.pid).as_ref() != Some(&current) {
                return false;
            }
            path.push(current.clone());
            if current == *target {
                // A PID recycled or a reparented intermediate process invalidates the path.
                return path.iter().all(|identity| {
                    self.inspector.process(identity.pid).as_ref() == Some(identity)
                });
            }
            if current.agent().is_some() || current.tty != target.tty || current.parent_pid == 0 {
                return false;
            }
            let Some(parent) = self.inspector.process(current.parent_pid) else {
                return false;
            };
            if parent.birth > current.birth {
                return false;
            }
            current = parent;
        }
        false
    }
}

impl super::App {
    fn task_foreground(&self, id: &TerminalId) -> Option<ProcessIdentity> {
        let child_pid = self
            .terminal_runtimes
            .get(id)
            .and_then(|runtime| runtime.child_pid());
        self.task_process_identity.inspector.foreground(child_pid)
    }

    pub(crate) fn observe_task_session_report(
        &mut self,
        id: &TerminalId,
        source: &str,
        agent: &str,
        session: Option<&AgentSessionRef>,
        start: Option<&str>,
        accepted: bool,
    ) {
        let foreground = self.task_foreground(id);
        // Failed/rejected replacement reports must never leave A authoritative under B.
        if self
            .task_process_identity
            .attestations
            .get(id)
            .is_some_and(|a| {
                foreground
                    .as_ref()
                    .is_none_or(|process| !same_binding_process(process, &a.process))
                    || self
                        .state
                        .terminals
                        .get(id)
                        .is_none_or(|t| t.native_binding_epoch != a.epoch)
            })
        {
            self.task_process_identity.invalidate(id);
        }
        let Some(process) = foreground else {
            return;
        };
        if source == "herdr:claude"
            && agent == "claude"
            && (start != Some("startup") || session.is_none())
        {
            self.task_process_identity.invalidate(id);
            self.task_process_identity
                .burned
                .entry(id.clone())
                .or_default()
                .insert(lifetime(&process));
            return;
        }
        let Some(terminal) = self.state.terminals.get(id) else {
            return;
        };
        if !accepted || source != "herdr:claude" || agent != "claude" || start != Some("startup") {
            return;
        }
        let Some(session) = session.filter(|s| s.kind == AgentSessionRefKind::Id) else {
            return;
        };
        if self
            .task_process_identity
            .retired_sessions
            .get(id)
            .is_some_and(|sessions| sessions.contains(&session.value))
            || !native_matches(terminal, &session.value)
        {
            return;
        }
        let Some(peer) = self.api_peer_process.as_ref() else {
            return;
        };
        if !self.task_process_identity.descendant(peer, &process)
            || self.task_foreground(id).as_ref() != Some(&process)
            || self
                .task_process_identity
                .burned
                .get(id)
                .is_some_and(|set| set.contains(&lifetime(&process)))
        {
            return;
        }
        self.task_process_identity.attestations.insert(
            id.clone(),
            Attestation {
                session: session.value.clone(),
                process,
                epoch: terminal.native_binding_epoch,
            },
        );
    }

    pub(crate) fn task_session_identity_matches(&mut self, id: &TerminalId, session: &str) -> bool {
        let Some(attestation) = self.task_process_identity.attestations.get(id).cloned() else {
            return false;
        };
        let valid = self.state.terminals.get(id).is_some_and(|terminal| {
            native_matches(terminal, &attestation.session)
                && terminal.native_binding_epoch == attestation.epoch
        }) && self
            .task_foreground(id)
            .as_ref()
            .is_some_and(|process| same_binding_process(process, &attestation.process));
        if !valid {
            self.task_process_identity.invalidate(id);
            return false;
        }
        // A typo in a bind request does not burn a valid runtime attestation.
        attestation.session == session
    }
}

fn native_matches(terminal: &crate::terminal::TerminalState, session: &str) -> bool {
    terminal
        .persisted_agent_session
        .as_ref()
        .is_some_and(|native| {
            native.source == "herdr:claude"
                && native.agent == "claude"
                && native.session_ref.kind == AgentSessionRefKind::Id
                && native.session_ref.value == session
        })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;

    pub(crate) struct FakeInspector {
        pub processes: Mutex<HashMap<u32, ProcessIdentity>>,
        pub foreground: Mutex<Option<ProcessIdentity>>,
        pub foreground_reads: std::sync::atomic::AtomicUsize,
        pub fail_foreground_from: Mutex<Option<usize>>,
    }
    impl ProcessInspector for FakeInspector {
        fn process(&self, pid: u32) -> Option<ProcessIdentity> {
            self.processes.lock().unwrap().get(&pid).cloned()
        }
        fn foreground(&self, _: Option<u32>) -> Option<ProcessIdentity> {
            let read = self
                .foreground_reads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            if self
                .fail_foreground_from
                .lock()
                .unwrap()
                .is_some_and(|from| read >= from)
            {
                return None;
            }
            let process = self.foreground.lock().unwrap().clone()?;
            (self.process(process.pid).as_ref() == Some(&process)).then_some(process)
        }
    }
    pub(crate) fn fixture() -> (Arc<FakeInspector>, ProcessIdentity) {
        let claude = ProcessIdentity {
            pid: 100,
            parent_pid: 1,
            process_group_id: 100,
            birth: (10, 0),
            tty: 42,
            name: "claude".into(),
        };
        let peer = ProcessIdentity {
            pid: 102,
            parent_pid: 101,
            process_group_id: 100,
            birth: (12, 0),
            tty: 42,
            name: "python3".into(),
        };
        let shell = ProcessIdentity {
            pid: 101,
            parent_pid: 100,
            process_group_id: 100,
            birth: (11, 0),
            tty: 42,
            name: "sh".into(),
        };
        let inspector = Arc::new(FakeInspector {
            processes: Mutex::new(
                [(100, claude.clone()), (101, shell), (102, peer.clone())].into(),
            ),
            foreground: Mutex::new(Some(claude)),
            foreground_reads: std::sync::atomic::AtomicUsize::new(0),
            fail_foreground_from: Mutex::new(None),
        });
        (inspector, peer)
    }

    #[test]
    fn process_bound_ancestry_fails_closed_on_nested_agents_reuse_and_errors() {
        let (inspector, peer) = fixture();
        let identity = TaskProcessIdentity {
            inspector: inspector.clone(),
            ..Default::default()
        };
        let target = inspector.foreground(None).unwrap();
        assert!(identity.descendant(&peer, &target));
        inspector
            .processes
            .lock()
            .unwrap()
            .get_mut(&101)
            .unwrap()
            .name = "codex".into();
        assert!(!identity.descendant(&peer, &target));
        inspector
            .processes
            .lock()
            .unwrap()
            .get_mut(&101)
            .unwrap()
            .name = "sh".into();
        inspector
            .processes
            .lock()
            .unwrap()
            .get_mut(&102)
            .unwrap()
            .birth
            .0 += 1;
        assert!(!identity.descendant(&peer, &target));
        inspector.processes.lock().unwrap().remove(&102);
        assert!(!identity.descendant(&peer, &target));
    }
}
