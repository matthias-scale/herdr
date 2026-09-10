//! Server-owned state for the cross-host `agent.focus` operation.
//!
//! The transport is deliberately a small start seam. It does not expose
//! terminal bytes or a proxy pane, so the API state machine can land before
//! the wire-control work in the next slice.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::api::schema::{AgentRef, ErrorBody, RemoteControlContext, RemoteFocusState};

pub(crate) fn configured_remote_hosts(
    fleet: &crate::config::FleetConfig,
) -> std::collections::HashSet<String> {
    fleet
        .hosts
        .iter()
        .filter(|host| !host.local && !host.target.trim().is_empty())
        .map(|host| host.name.clone())
        .collect()
}

/// Terminal operation records remain queryable for five minutes.
pub(crate) const REMOTE_FOCUS_TERMINAL_TTL: Duration = Duration::from_secs(5 * 60);
/// At most sixteen operations may be connecting or active at once.
pub(crate) const REMOTE_FOCUS_MAX_CONCURRENT_OPERATIONS: usize = 16;
/// Retained terminal records also have a hard cap so an idle API cannot grow
/// the map without bound before the expiry sweep runs.
const REMOTE_FOCUS_MAX_RETAINED_OPERATIONS: usize = 32;

#[derive(Debug, Clone)]
#[allow(dead_code)] // Active and Closed are emitted by the step-3 transport seam.
pub(crate) enum RemoteFocusTransition {
    Active(Box<RemoteControlContext>),
    Failed(ErrorBody),
    Closed,
}

/// The future wire client implements this seam. Implementations must return
/// without performing blocking I/O. `Err` reports an immediate start failure;
/// after `Ok`, deliver state changes through `AppEvent::RemoteFocusTransition`.
pub(crate) trait RemoteFocusTransport: Send {
    fn start(
        &mut self,
        operation_id: &str,
        agent_ref: &AgentRef,
        proxy_pane_id: &str,
        event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    ) -> Result<(), ErrorBody>;
}

#[derive(Debug, Clone)]
pub(crate) struct RemoteFocusOperation {
    pub(crate) agent_ref: AgentRef,
    pub(crate) proxy_pane_id: String,
    pub(crate) state: RemoteFocusState,
    pub(crate) context: Option<RemoteControlContext>,
    pub(crate) error: Option<ErrorBody>,
    created_at: Instant,
    completed_at: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteFocusOperationStart {
    pub(crate) operation_id: String,
    pub(crate) agent_ref: AgentRef,
    pub(crate) proxy_pane_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteFocusOperationSnapshot {
    pub(crate) operation_id: String,
    pub(crate) agent_ref: AgentRef,
    pub(crate) proxy_pane_id: String,
    pub(crate) state: RemoteFocusState,
    pub(crate) context: Option<RemoteControlContext>,
    pub(crate) error: Option<ErrorBody>,
}

#[derive(Debug)]
pub(crate) struct RemoteFocusOperations {
    operations: HashMap<String, RemoteFocusOperation>,
    next_operation_id: u64,
}

impl Default for RemoteFocusOperations {
    fn default() -> Self {
        Self {
            operations: HashMap::new(),
            next_operation_id: 1,
        }
    }
}

impl RemoteFocusOperations {
    pub(crate) fn begin(
        &mut self,
        agent_ref: AgentRef,
        now: Instant,
    ) -> Result<RemoteFocusOperationStart, ErrorBody> {
        self.prune(now);

        let concurrent = self
            .operations
            .values()
            .filter(|operation| {
                matches!(
                    operation.state,
                    RemoteFocusState::Connecting | RemoteFocusState::Active
                )
            })
            .count();
        if concurrent >= REMOTE_FOCUS_MAX_CONCURRENT_OPERATIONS {
            return Err(ErrorBody {
                code: "operation_limit_reached".into(),
                message: format!(
                    "remote focus operation limit reached ({REMOTE_FOCUS_MAX_CONCURRENT_OPERATIONS})"
                ),
            });
        }

        if self.operations.len() >= REMOTE_FOCUS_MAX_RETAINED_OPERATIONS {
            let oldest_terminal = self
                .operations
                .iter()
                .filter(|(_, operation)| {
                    matches!(
                        operation.state,
                        RemoteFocusState::Failed | RemoteFocusState::Closed
                    )
                })
                .min_by_key(|(_, operation)| operation.created_at)
                .map(|(operation_id, _)| operation_id.clone());
            if let Some(operation_id) = oldest_terminal {
                self.operations.remove(&operation_id);
            } else {
                return Err(ErrorBody {
                    code: "operation_limit_reached".into(),
                    message: "remote focus operation retention limit reached".into(),
                });
            }
        }

        let operation_id = self.next_operation_id();
        let proxy_pane_id = format!("remote-focus-proxy-{operation_id}");
        self.operations.insert(
            operation_id.clone(),
            RemoteFocusOperation {
                agent_ref: agent_ref.clone(),
                proxy_pane_id: proxy_pane_id.clone(),
                state: RemoteFocusState::Connecting,
                context: None,
                error: None,
                created_at: now,
                completed_at: None,
            },
        );
        Ok(RemoteFocusOperationStart {
            operation_id,
            agent_ref,
            proxy_pane_id,
        })
    }

    pub(crate) fn snapshot(
        &mut self,
        operation_id: &str,
        now: Instant,
    ) -> Result<RemoteFocusOperationSnapshot, ErrorBody> {
        self.prune(now);
        let Some(operation) = self.operations.get(operation_id) else {
            return Err(ErrorBody {
                code: "unknown_operation".into(),
                message: format!("unknown remote focus operation: {operation_id}"),
            });
        };
        Ok(RemoteFocusOperationSnapshot {
            operation_id: operation_id.to_string(),
            agent_ref: operation.agent_ref.clone(),
            proxy_pane_id: operation.proxy_pane_id.clone(),
            state: operation.state,
            context: operation.context.clone(),
            error: operation.error.clone(),
        })
    }

    pub(crate) fn transition(
        &mut self,
        operation_id: &str,
        transition: RemoteFocusTransition,
        now: Instant,
    ) {
        let Some(operation) = self.operations.get_mut(operation_id) else {
            return;
        };
        if matches!(
            operation.state,
            RemoteFocusState::Failed | RemoteFocusState::Closed
        ) {
            return;
        }

        match transition {
            RemoteFocusTransition::Active(context) => {
                if operation.state == RemoteFocusState::Connecting {
                    operation.state = RemoteFocusState::Active;
                    operation.context = Some(*context);
                }
            }
            RemoteFocusTransition::Failed(error) => {
                operation.state = RemoteFocusState::Failed;
                operation.context = None;
                operation.error = Some(error);
                operation.completed_at = Some(now);
            }
            RemoteFocusTransition::Closed => {
                operation.state = RemoteFocusState::Closed;
                operation.context = None;
                operation.error = None;
                operation.completed_at = Some(now);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.operations.len()
    }

    fn next_operation_id(&mut self) -> String {
        loop {
            let number = self.next_operation_id;
            self.next_operation_id = self.next_operation_id.saturating_add(1);
            let operation_id = format!("remote-focus-{number}");
            if !self.operations.contains_key(&operation_id) {
                return operation_id;
            }
        }
    }

    fn prune(&mut self, now: Instant) {
        self.operations.retain(|_, operation| {
            operation.completed_at.is_none_or(|completed_at| {
                now.checked_duration_since(completed_at)
                    .is_none_or(|age| age < REMOTE_FOCUS_TERMINAL_TTL)
            })
        });
    }
}

impl crate::app::App {
    pub(crate) fn remote_control_context(
        &self,
        agent_ref: &AgentRef,
    ) -> Result<RemoteControlContext, ErrorBody> {
        #[cfg(unix)]
        {
            self.remote_control_context_unix(agent_ref)
        }
        #[cfg(not(unix))]
        {
            let _ = agent_ref;
            Err(ErrorBody {
                code: "agent_not_attachable".into(),
                message: "remote control is supported only for Unix runtimes".into(),
            })
        }
    }

    #[cfg(unix)]
    fn remote_control_context_unix(
        &self,
        agent_ref: &AgentRef,
    ) -> Result<RemoteControlContext, ErrorBody> {
        if agent_ref.host != self.state.agent_host_name {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "agent reference host does not match this server".into(),
            });
        }
        if self.state.fleet_snapshot.hosts.iter().any(|host| {
            host.entries.iter().any(|entry| {
                entry.agent_ref == *agent_ref
                    && entry.source == crate::fleet::EvidenceSource::RunState
            })
        }) {
            return Err(ErrorBody {
                code: "agent_not_attachable".into(),
                message: format!("agent {} is a windowless run", agent_ref),
            });
        }
        let resolved = self
            .resolve_agent_target(&agent_ref.agent)
            .map_err(|_| ErrorBody {
                code: "unknown_agent".into(),
                message: format!("agent {} no longer resolves to a pane", agent_ref),
            })?;
        let Some(workspace) = self.state.workspaces.get(resolved.ws_idx) else {
            return Err(ErrorBody {
                code: "unknown_agent".into(),
                message: format!("agent {} workspace no longer exists", agent_ref),
            });
        };
        let Some(tab_idx) = workspace.find_tab_index_for_pane(resolved.pane_id) else {
            return Err(ErrorBody {
                code: "unknown_agent".into(),
                message: format!("agent {} tab no longer exists", agent_ref),
            });
        };
        let Some(pane) = workspace.pane_state(resolved.pane_id) else {
            return Err(ErrorBody {
                code: "unknown_agent".into(),
                message: format!("agent {} pane no longer exists", agent_ref),
            });
        };
        let Some(terminal) = self.state.terminals.get(&pane.attached_terminal_id) else {
            return Err(ErrorBody {
                code: "agent_not_attachable".into(),
                message: format!("agent {} has no terminal runtime", agent_ref),
            });
        };
        let Some(runtime) = self.terminal_runtimes.get(&pane.attached_terminal_id) else {
            return Err(ErrorBody {
                code: "agent_not_attachable".into(),
                message: format!("agent {} is windowless", agent_ref),
            });
        };
        let Some(known_agent) = terminal.effective_known_agent() else {
            return Err(ErrorBody {
                code: "unknown_agent".into(),
                message: format!("agent {} is not detected in its pane", agent_ref),
            });
        };
        if terminal.managed_agent_launch_pending() {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "agent launch is still pending".into(),
            });
        }
        if !terminal.managed_agent_interactive_ready() {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "agent is not interactive_ready".into(),
            });
        }
        let Some(user) = std::env::var("USER")
            .ok()
            .filter(|user| !user.trim().is_empty())
        else {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "effective remote user is unavailable".into(),
            });
        };
        let Some(cwd) = terminal.cwd.to_str().filter(|cwd| !cwd.trim().is_empty()) else {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "terminal cwd is unavailable".into(),
            });
        };
        let Some(foreground_cwd) = runtime
            .foreground_cwd()
            .and_then(|cwd| cwd.to_str().map(str::to_owned))
            .filter(|cwd| !cwd.trim().is_empty())
        else {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "foreground cwd is unavailable".into(),
            });
        };
        let Some(tty) = runtime
            .tty_name()
            .and_then(|tty| tty.to_str().map(str::to_owned))
            .filter(|tty| !tty.trim().is_empty())
        else {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "terminal tty is unavailable".into(),
            });
        };
        let Some(shell_pid) = runtime.child_pid() else {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "terminal shell process is unavailable".into(),
            });
        };
        let Some(job) = crate::detect::foreground_job(shell_pid) else {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "foreground process is unavailable".into(),
            });
        };
        let Some(process) = job
            .processes
            .iter()
            .find(|process| process.pid == job.process_group_id)
            .or_else(|| job.processes.first())
        else {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "foreground process identity is unavailable".into(),
            });
        };
        let Some(process_cwd) = crate::platform::process_cwd(process.pid)
            .and_then(|cwd| cwd.to_str().map(str::to_owned))
            .filter(|cwd| !cwd.trim().is_empty())
        else {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "foreground process cwd is unavailable".into(),
            });
        };
        let Some(argv) = process.argv.clone() else {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "foreground process argv is unavailable".into(),
            });
        };
        if argv.is_empty() || process.name.trim().is_empty() {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "foreground process identity is incomplete".into(),
            });
        }
        let workspace_id = self.public_workspace_id(resolved.ws_idx);
        let Some(tab_id) = self.public_tab_id(resolved.ws_idx, tab_idx) else {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "tab identity is unavailable".into(),
            });
        };
        let Some(pane_id) = self.public_pane_id(resolved.ws_idx, resolved.pane_id) else {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "pane identity is unavailable".into(),
            });
        };
        let state_change_seq = terminal
            .last_agent_state_change_seq
            .ok_or_else(|| ErrorBody {
                code: "refused_for_safety".into(),
                message: "agent state epoch is unavailable".into(),
            })?;
        let human_draft = self
            .state
            .pending_human_drafts
            .contains_key(&resolved.pane_id);
        if human_draft {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "a human input draft is pending".into(),
            });
        }
        Ok(RemoteControlContext {
            host: self.state.agent_host_name.clone(),
            user,
            workspace_id,
            tab_id,
            pane_id,
            terminal_id: terminal.id.to_string(),
            cwd: cwd.to_owned(),
            foreground_cwd,
            tty,
            foreground_process: crate::api::schema::RemoteForegroundProcess {
                pid: process.pid,
                process_group_id: job.process_group_id,
                name: process.name.clone(),
                argv,
                cwd: process_cwd,
            },
            detected_agent: crate::detect::agent_label(known_agent).to_owned(),
            interactive_ready: true,
            human_draft: false,
            state_change_seq,
            revision: terminal.revision,
            context_epoch: terminal.revision,
        })
    }

    pub(crate) fn start_remote_focus_operation(
        &mut self,
        agent_ref: AgentRef,
    ) -> Result<RemoteFocusOperationStart, ErrorBody> {
        let started = self
            .remote_focus_operations
            .begin(agent_ref.clone(), Instant::now())?;
        let event_tx = self.event_tx.clone();
        if let Err(error) = self.remote_focus_transport.start(
            &started.operation_id,
            &agent_ref,
            &started.proxy_pane_id,
            event_tx,
        ) {
            self.apply_remote_focus_transition(
                &started.operation_id,
                RemoteFocusTransition::Failed(error),
            );
        }
        Ok(started)
    }

    pub(crate) fn remote_focus_status(
        &mut self,
        operation_id: &str,
    ) -> Result<RemoteFocusOperationSnapshot, ErrorBody> {
        self.remote_focus_operations
            .snapshot(operation_id, Instant::now())
    }

    pub(crate) fn apply_remote_focus_transition(
        &mut self,
        operation_id: &str,
        transition: RemoteFocusTransition,
    ) {
        self.remote_focus_operations
            .transition(operation_id, transition, Instant::now());
    }
}

impl crate::server::remote_control::RemoteControlContextProvider for crate::app::App {
    fn fresh_remote_control_context(
        &self,
        agent_ref: &AgentRef,
    ) -> Result<RemoteControlContext, ErrorBody> {
        self.remote_control_context(agent_ref)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_remote_hosts_excludes_local_and_incomplete_entries() {
        let fleet = crate::config::FleetConfig {
            hosts: vec![
                crate::config::FleetHostConfig {
                    name: "buildbox".into(),
                    target: "buildbox".into(),
                    ..Default::default()
                },
                crate::config::FleetHostConfig {
                    name: "laptop".into(),
                    local: true,
                    ..Default::default()
                },
                crate::config::FleetHostConfig {
                    name: "incomplete".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(
            configured_remote_hosts(&fleet),
            std::iter::once("buildbox".to_string()).collect()
        );
    }

    fn agent_ref() -> AgentRef {
        AgentRef::new("buildbox", "w1:p3").expect("valid agent reference")
    }

    fn context() -> RemoteControlContext {
        RemoteControlContext {
            host: "buildbox".into(),
            user: "operator".into(),
            workspace_id: "w1".into(),
            tab_id: "t2".into(),
            pane_id: "w1:p3".into(),
            terminal_id: "terminal-id".into(),
            cwd: "/work/repo".into(),
            foreground_cwd: "/work/repo".into(),
            tty: "/dev/pts/4".into(),
            foreground_process: crate::api::schema::RemoteForegroundProcess {
                pid: 1234,
                process_group_id: 1234,
                name: "agent".into(),
                argv: vec!["agent".into(), "run".into()],
                cwd: "/work/repo".into(),
            },
            detected_agent: "claude".into(),
            interactive_ready: true,
            human_draft: false,
            state_change_seq: 9,
            revision: 41,
            context_epoch: 12,
        }
    }

    #[test]
    fn connecting_active_closed_and_failed_transitions_are_terminally_stable() {
        let now = Instant::now();
        let mut operations = RemoteFocusOperations::default();
        let started = operations
            .begin(agent_ref(), now)
            .expect("operation starts");

        operations.transition(
            &started.operation_id,
            RemoteFocusTransition::Active(Box::new(context())),
            now,
        );
        assert_eq!(
            operations
                .snapshot(&started.operation_id, now)
                .expect("active operation")
                .state,
            RemoteFocusState::Active
        );
        operations.transition(&started.operation_id, RemoteFocusTransition::Closed, now);
        operations.transition(
            &started.operation_id,
            RemoteFocusTransition::Failed(ErrorBody {
                code: "connection_lost".into(),
                message: "late failure".into(),
            }),
            now,
        );
        assert_eq!(
            operations
                .snapshot(&started.operation_id, now)
                .expect("closed operation")
                .state,
            RemoteFocusState::Closed
        );

        let failed = operations
            .begin(
                AgentRef::new("buildbox", "w1:p4").expect("valid agent reference"),
                now,
            )
            .expect("second operation starts");
        operations.transition(
            &failed.operation_id,
            RemoteFocusTransition::Failed(ErrorBody {
                code: "host_unreachable".into(),
                message: "not implemented".into(),
            }),
            now,
        );
        operations.transition(
            &failed.operation_id,
            RemoteFocusTransition::Active(Box::new(context())),
            now,
        );
        let failed_snapshot = operations
            .snapshot(&failed.operation_id, now)
            .expect("failed operation");
        assert_eq!(failed_snapshot.state, RemoteFocusState::Failed);
        assert_eq!(
            failed_snapshot
                .error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("host_unreachable")
        );
    }

    #[test]
    fn concurrent_cap_and_terminal_expiry_bound_the_operation_map() {
        let now = Instant::now();
        let mut operations = RemoteFocusOperations::default();
        for index in 0..REMOTE_FOCUS_MAX_CONCURRENT_OPERATIONS {
            operations
                .begin(
                    AgentRef::new("buildbox", format!("w1:p{index}"))
                        .expect("valid agent reference"),
                    now,
                )
                .expect("operation stays under concurrent cap");
        }
        let error = operations
            .begin(agent_ref(), now)
            .expect_err("concurrent cap must reject another operation");
        assert_eq!(error.code, "operation_limit_reached");
        assert_eq!(operations.len(), REMOTE_FOCUS_MAX_CONCURRENT_OPERATIONS);

        let first = "remote-focus-1";
        operations.transition(
            first,
            RemoteFocusTransition::Failed(ErrorBody {
                code: "host_unreachable".into(),
                message: "stub".into(),
            }),
            now,
        );
        operations
            .operations
            .get_mut(first)
            .expect("first operation")
            .completed_at = now.checked_sub(REMOTE_FOCUS_TERMINAL_TTL + Duration::from_secs(1));
        let expired = operations
            .snapshot(first, now)
            .expect_err("expired operation is no longer queryable");
        assert_eq!(expired.code, "unknown_operation");
        assert_eq!(operations.len(), REMOTE_FOCUS_MAX_CONCURRENT_OPERATIONS - 1);

        let mut retained = RemoteFocusOperations::default();
        for index in 0..REMOTE_FOCUS_MAX_RETAINED_OPERATIONS {
            let operation_now = now + Duration::from_secs(index as u64);
            let started = retained
                .begin(
                    AgentRef::new("buildbox", format!("w1:retained-{index}"))
                        .expect("valid agent reference"),
                    operation_now,
                )
                .expect("terminal operation starts");
            retained.transition(
                &started.operation_id,
                RemoteFocusTransition::Failed(ErrorBody {
                    code: "host_unreachable".into(),
                    message: "stub".into(),
                }),
                operation_now,
            );
        }
        assert_eq!(retained.len(), REMOTE_FOCUS_MAX_RETAINED_OPERATIONS);
        retained
            .begin(
                agent_ref(),
                now + Duration::from_secs(REMOTE_FOCUS_MAX_RETAINED_OPERATIONS as u64),
            )
            .expect("terminal record is evicted at the retention cap");
        assert_eq!(retained.len(), REMOTE_FOCUS_MAX_RETAINED_OPERATIONS);
        let unknown = retained
            .snapshot("remote-focus-1", now)
            .expect_err("evicted operation is no longer queryable");
        assert_eq!(unknown.code, "unknown_operation");
    }
}
