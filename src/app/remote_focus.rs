//! Server-owned state for the cross-host `agent.focus` operation.
//!
//! A remote focus opens one local proxy pane whose screen is fed by the wire
//! transport through `AppEvent::RemoteFocusFrame`. User input stays disabled
//! until the first complete frame and `ControlReady` have both arrived; the
//! proxy owns the remote terminal's dimensions while attached; closing the
//! pane detaches the lease and closes the operation.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::api::schema::{AgentRef, ErrorBody, RemoteControlContext, RemoteFocusState};
use crate::layout::PaneId;
use crate::pane::RemoteProxyChannels;
use crate::terminal::TerminalId;

/// The production transport is intentionally inert until the proxy pane is
/// proven against a real remote server. `SshRemoteFocusTransport` stays
/// compiled and tested; this stub remains the default.
#[derive(Debug, Default)]
pub(crate) struct StubRemoteFocusTransport;

impl RemoteFocusTransport for StubRemoteFocusTransport {
    fn start(
        &mut self,
        _operation_id: &str,
        _agent_ref: &AgentRef,
        _proxy_pane_id: &str,
        _channels: RemoteProxyChannels,
        _event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    ) -> Result<(), ErrorBody> {
        Err(ErrorBody {
            code: "host_unreachable".to_owned(),
            message: "remote focus transport is not available yet".to_owned(),
        })
    }
}

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
pub(crate) enum RemoteFocusTransition {
    // Constructed by the Unix wire transport and tests; on Windows no code
    // path reaches Active yet.
    #[cfg_attr(not(unix), allow(dead_code))]
    Active(Box<RemoteControlContext>),
    Failed(ErrorBody),
    Closed,
}

/// The wire client implements this seam. Implementations must return without
/// performing blocking I/O. `Err` reports an immediate start failure; after
/// `Ok`, deliver state changes through `AppEvent::RemoteFocusTransition` and
/// terminal frames through `AppEvent::RemoteFocusFrame`. The transport owns
/// the outbound channel: user input, resize markers (latest dimensions live
/// in the resize slot), and detach. `detach` must not block; it releases the
/// remote lease on a live session and is a no-op otherwise.
pub(crate) trait RemoteFocusTransport: Send {
    fn start(
        &mut self,
        operation_id: &str,
        agent_ref: &AgentRef,
        proxy_pane_id: &str,
        channels: RemoteProxyChannels,
        event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    ) -> Result<(), ErrorBody>;

    fn detach(&mut self, _operation_id: &str) {}
}

#[derive(Debug, Clone)]
pub(crate) struct RemoteFocusOperation {
    pub(crate) agent_ref: AgentRef,
    pub(crate) proxy_pane_id: String,
    pub(crate) state: RemoteFocusState,
    pub(crate) context: Option<RemoteControlContext>,
    pub(crate) error: Option<ErrorBody>,
    /// Local proxy pane and its terminal, once the pane exists.
    proxy: Option<(PaneId, TerminalId)>,
    first_frame_processed: bool,
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
    /// Reverse index from a proxy pane's terminal to its operation, so the
    /// terminal shutdown path can release the remote lease.
    proxy_by_terminal: HashMap<TerminalId, String>,
    next_operation_id: u64,
}

impl Default for RemoteFocusOperations {
    fn default() -> Self {
        Self {
            operations: HashMap::new(),
            proxy_by_terminal: HashMap::new(),
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
                self.proxy_by_terminal
                    .retain(|_, candidate| candidate != &operation_id);
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
                proxy: None,
                first_frame_processed: false,
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

    /// Binds the local proxy pane to the operation and publishes the real
    /// public pane id in place of the placeholder.
    pub(crate) fn attach_proxy(
        &mut self,
        operation_id: &str,
        pane_id: PaneId,
        terminal_id: TerminalId,
        public_pane_id: String,
    ) {
        let Some(operation) = self.operations.get_mut(operation_id) else {
            return;
        };
        operation.proxy = Some((pane_id, terminal_id.clone()));
        operation.proxy_pane_id = public_pane_id;
        self.proxy_by_terminal
            .insert(terminal_id, operation_id.to_string());
    }

    pub(crate) fn proxy_location(&self, operation_id: &str) -> Option<(PaneId, TerminalId)> {
        self.operations
            .get(operation_id)
            .and_then(|operation| operation.proxy.clone())
    }

    /// Removes the reverse index entry for a shutting-down proxy terminal and
    /// returns its operation.
    pub(crate) fn take_proxy_terminal(&mut self, terminal_id: &TerminalId) -> Option<String> {
        self.proxy_by_terminal.remove(terminal_id)
    }

    /// Records that one complete frame reached the proxy screen. Returns true
    /// when input may now be enabled: the operation is active (ControlReady
    /// arrived) and a complete frame was processed.
    pub(crate) fn mark_frame_processed(
        &mut self,
        operation_id: &str,
        frame_complete: bool,
    ) -> bool {
        let Some(operation) = self.operations.get_mut(operation_id) else {
            return false;
        };
        if frame_complete {
            operation.first_frame_processed = true;
        }
        operation.state == RemoteFocusState::Active && operation.first_frame_processed
    }

    /// Whether the operation is active and at least one complete frame has
    /// been processed, the two halves of the input gate.
    pub(crate) fn input_gate_open(&self, operation_id: &str) -> bool {
        self.operations.get(operation_id).is_some_and(|operation| {
            operation.state == RemoteFocusState::Active && operation.first_frame_processed
        })
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
        self.proxy_by_terminal
            .retain(|_, operation_id| self.operations.contains_key(operation_id));
    }
}

/// The compact identity line a proxy pane shows once `ControlReady` arrives:
/// `host::agent`, remote user, cwd, tty, and foreground process.
pub(crate) fn remote_proxy_identity_line(context: &RemoteControlContext) -> String {
    format!(
        "{}::{} · {} · {} · {} · {}",
        context.host,
        context.pane_id,
        context.user,
        context.cwd,
        context.tty,
        context.foreground_process.name
    )
}

impl crate::app::App {
    #[cfg(unix)]
    pub(crate) fn remote_control_context(
        &self,
        agent_ref: &AgentRef,
    ) -> Result<RemoteControlContext, ErrorBody> {
        self.remote_control_context_unix(agent_ref)
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
        if self.fleet_marks_windowless(agent_ref) {
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
        if terminal.managed_agent_launch_pending() && !terminal.managed_agent_control_ready() {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "agent launch is still pending".into(),
            });
        }
        if !terminal.managed_agent_control_ready() {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "agent is not interactive_ready".into(),
            });
        }
        let Some(user) = crate::platform::effective_user_name() else {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "effective remote user is unavailable".into(),
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
        let Some(cwd) = crate::platform::process_cwd(shell_pid)
            .and_then(|cwd| cwd.to_str().map(str::to_owned))
            .filter(|cwd| !cwd.trim().is_empty())
        else {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "terminal cwd is unavailable".into(),
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
        if !crate::app::agents::runtime_hosts_agent_in_job(&job, known_agent) {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: format!(
                    "agent {} is no longer the live foreground process",
                    agent_ref
                ),
            });
        }
        let Some(foreground_cwd) = crate::platform::process_cwd(process.pid)
            .and_then(|cwd| cwd.to_str().map(str::to_owned))
            .filter(|cwd| !cwd.trim().is_empty())
        else {
            return Err(ErrorBody {
                code: "refused_for_safety".into(),
                message: "foreground cwd is unavailable".into(),
            });
        };
        let process_cwd = foreground_cwd.clone();
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

    pub(crate) fn fleet_marks_windowless(&self, agent_ref: &AgentRef) -> bool {
        self.state.fleet_snapshot.hosts.iter().any(|host| {
            host.entries.iter().any(|entry| {
                entry.agent_ref == *agent_ref
                    && entry.source == crate::fleet::EvidenceSource::RunState
            })
        })
    }

    pub(crate) fn start_remote_focus_operation(
        &mut self,
        agent_ref: AgentRef,
    ) -> Result<RemoteFocusOperationStart, ErrorBody> {
        // Windowless runs have no remote PTY to display or control.
        if self.fleet_marks_windowless(&agent_ref) {
            return Err(ErrorBody {
                code: "agent_not_attachable".into(),
                message: format!("agent {agent_ref} is a windowless run"),
            });
        }
        let mut started = self
            .remote_focus_operations
            .begin(agent_ref.clone(), Instant::now())?;
        let channels = match self.create_remote_proxy_pane(&agent_ref) {
            Ok((pane_id, terminal_id, public_pane_id, channels)) => {
                started.proxy_pane_id = public_pane_id.clone();
                self.remote_focus_operations.attach_proxy(
                    &started.operation_id,
                    pane_id,
                    terminal_id,
                    public_pane_id,
                );
                channels
            }
            Err(error) => {
                self.apply_remote_focus_transition(
                    &started.operation_id,
                    RemoteFocusTransition::Failed(error),
                );
                return Ok(started);
            }
        };
        let event_tx = self.event_tx.clone();
        if let Err(error) = self.remote_focus_transport.start(
            &started.operation_id,
            &agent_ref,
            &started.proxy_pane_id,
            channels,
            event_tx,
        ) {
            self.apply_remote_focus_transition(
                &started.operation_id,
                RemoteFocusTransition::Failed(error),
            );
        }
        Ok(started)
    }

    /// Opens one local tab holding a single proxy pane in the active
    /// workspace and focuses it. No SSH, wire I/O, or process work happens
    /// here; the pane is a local terminal surface until the transport proves
    /// the remote control lease.
    fn create_remote_proxy_pane(
        &mut self,
        agent_ref: &AgentRef,
    ) -> Result<(PaneId, TerminalId, String, RemoteProxyChannels), ErrorBody> {
        let workspace_count = self.state.workspaces.len();
        let ws_idx = self
            .state
            .active
            .filter(|idx| *idx < workspace_count)
            .or_else(|| (self.state.selected < workspace_count).then_some(self.state.selected))
            .ok_or_else(|| ErrorBody {
                code: "host_unreachable".into(),
                message: "no local workspace is available for the remote focus proxy pane".into(),
            })?;
        let (rows, cols) = self.state.estimate_pane_size();
        let pane_id = PaneId::alloc();
        let terminal_id = TerminalId::alloc();
        let (runtime, channels) = crate::terminal::TerminalRuntime::spawn_remote_proxy(
            pane_id,
            rows,
            cols,
            self.state.pane_scrollback_limit_bytes,
            self.render_notify.clone(),
            self.render_dirty.clone(),
        )
        .map_err(|error| ErrorBody {
            code: "host_unreachable".into(),
            message: format!("failed to create remote focus proxy pane: {error}"),
        })?;
        let cwd = self.state.workspaces[ws_idx].identity_cwd.clone();
        let mut terminal = crate::terminal::TerminalState::new(terminal_id.clone(), cwd);
        terminal.remote_proxy = true;
        terminal.manual_label = Some(agent_ref.to_string());
        self.state.terminals.insert(terminal_id.clone(), terminal);
        self.terminal_runtimes.insert(terminal_id.clone(), runtime);
        let workspace = &mut self.state.workspaces[ws_idx];
        let tab_idx = workspace.create_tab_from_existing_pane(
            crate::workspace::MovedPane {
                pane_id,
                pane_state: crate::pane::PaneState::new(terminal_id.clone()),
            },
            Some(agent_ref.to_string()),
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
        );
        workspace.switch_tab(tab_idx);
        self.state.focus_pane_in_workspace(ws_idx, pane_id);
        self.state.mark_session_dirty();
        let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) else {
            return Err(ErrorBody {
                code: "host_unreachable".into(),
                message: "remote focus proxy pane has no public identity".into(),
            });
        };
        Ok((pane_id, terminal_id, public_pane_id, channels))
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
        let activation = match &transition {
            RemoteFocusTransition::Active(context) => Some((**context).clone()),
            _ => None,
        };
        self.remote_focus_operations
            .transition(operation_id, transition, Instant::now());
        let state = self
            .remote_focus_operations
            .snapshot(operation_id, Instant::now())
            .map(|snapshot| snapshot.state);
        match (state, activation) {
            (Ok(RemoteFocusState::Active), Some(context)) => {
                self.activate_remote_proxy(operation_id, &context);
            }
            (Ok(RemoteFocusState::Failed | RemoteFocusState::Closed), _) => {
                self.close_remote_proxy_pane(operation_id);
            }
            _ => {}
        }
    }

    /// Applies the authoritative control context to the proxy pane: the
    /// identity line becomes the pane label, and the input gate opens when
    /// the first complete frame has also arrived.
    fn activate_remote_proxy(&mut self, operation_id: &str, context: &RemoteControlContext) {
        let Some((_pane_id, terminal_id)) =
            self.remote_focus_operations.proxy_location(operation_id)
        else {
            return;
        };
        let identity = remote_proxy_identity_line(context);
        if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
            terminal.manual_label = Some(identity);
            terminal.cwd = std::path::PathBuf::from(&context.cwd);
        }
        if self.remote_focus_operations.input_gate_open(operation_id) {
            if let Some(runtime) = self.terminal_runtimes.get(&terminal_id) {
                runtime.set_remote_proxy_input_enabled(true);
            }
        }
    }

    /// Feeds one complete remote terminal frame into the proxy pane. Returns
    /// true when the frame painted new content and the client should redraw.
    pub(crate) fn apply_remote_focus_frame(
        &mut self,
        operation_id: &str,
        frame: &crate::protocol::TerminalFrame,
    ) -> bool {
        let Some((_pane_id, terminal_id)) =
            self.remote_focus_operations.proxy_location(operation_id)
        else {
            return false;
        };
        let Some(runtime) = self.terminal_runtimes.get(&terminal_id) else {
            return false;
        };
        let painted = runtime.process_remote_frame(&frame.bytes);
        if self
            .remote_focus_operations
            .mark_frame_processed(operation_id, frame.full)
        {
            runtime.set_remote_proxy_input_enabled(true);
        }
        painted
    }

    /// Closes the proxy pane for an operation that failed or closed. Closing
    /// the pane shuts its runtime down, which releases the remote lease
    /// through `detach_remote_proxy_for_terminal`.
    fn close_remote_proxy_pane(&mut self, operation_id: &str) {
        let Some((pane_id, terminal_id)) =
            self.remote_focus_operations.proxy_location(operation_id)
        else {
            return;
        };
        let Some((ws_idx, _)) = self.find_pane(pane_id) else {
            return;
        };
        let should_close_workspace = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .is_some_and(|workspace| workspace.remove_pane(pane_id));
        self.state.mark_session_dirty();
        if should_close_workspace {
            self.state.selected = ws_idx;
            self.state.close_selected_workspace();
        } else {
            self.state.remove_unattached_terminal_ids([terminal_id]);
        }
        self.shutdown_detached_terminal_runtimes();
    }

    /// Releases the remote lease when a proxy pane's terminal shuts down,
    /// whatever local close path got there (pane, tab, workspace close, or
    /// operation failure). The remote server also rejects a second controller
    /// for the terminal, so the lease must go back exactly once.
    pub(crate) fn detach_remote_proxy_for_terminal(&mut self, terminal_id: &TerminalId) {
        let Some(operation_id) = self
            .remote_focus_operations
            .take_proxy_terminal(terminal_id)
        else {
            return;
        };
        self.remote_focus_transport.detach(&operation_id);
        self.remote_focus_operations.transition(
            &operation_id,
            RemoteFocusTransition::Closed,
            Instant::now(),
        );
    }
}

#[cfg(unix)]
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
    use crate::pane::ProxyOutbound;

    #[derive(Debug, Default)]
    struct RecordingState {
        started: Vec<(String, AgentRef)>,
        detached: Vec<String>,
        outbound: Option<tokio::sync::mpsc::Receiver<ProxyOutbound>>,
    }

    #[derive(Debug, Clone, Default)]
    struct RecordingTransport {
        state: std::sync::Arc<std::sync::Mutex<RecordingState>>,
    }

    impl RemoteFocusTransport for RecordingTransport {
        fn start(
            &mut self,
            operation_id: &str,
            agent_ref: &AgentRef,
            _proxy_pane_id: &str,
            channels: RemoteProxyChannels,
            _event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
        ) -> Result<(), ErrorBody> {
            let mut state = self.state.lock().expect("recording state lock");
            state
                .started
                .push((operation_id.to_string(), agent_ref.clone()));
            state.outbound = Some(channels.outbound_rx);
            Ok(())
        }

        fn detach(&mut self, operation_id: &str) {
            self.state
                .lock()
                .expect("recording state lock")
                .detached
                .push(operation_id.to_string());
        }
    }

    fn proxy_app() -> (
        crate::app::App,
        std::sync::Arc<std::sync::Mutex<RecordingState>>,
    ) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("local")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = crate::app::Mode::Terminal;
        app.state.agent_host_name = "local".into();
        let recording = std::sync::Arc::new(std::sync::Mutex::new(RecordingState::default()));
        app.remote_focus_transport = Box::new(RecordingTransport {
            state: std::sync::Arc::clone(&recording),
        });
        (app, recording)
    }

    fn proxy_terminal_id(app: &crate::app::App, operation_id: &str) -> TerminalId {
        app.remote_focus_operations
            .proxy_location(operation_id)
            .expect("proxy location")
            .1
    }

    fn full_frame(bytes: &[u8]) -> crate::protocol::TerminalFrame {
        crate::protocol::TerminalFrame {
            seq: 1,
            width: 80,
            height: 24,
            full: true,
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn start_opens_one_focused_proxy_tab_with_a_real_public_pane_id() {
        let (mut app, recording) = proxy_app();
        let started = app
            .start_remote_focus_operation(agent_ref())
            .expect("operation starts");

        assert_eq!(recording.lock().expect("lock").started.len(), 1);
        let workspace = &app.state.workspaces[0];
        assert_eq!(workspace.tabs.len(), 2);
        let proxy_tab = &workspace.tabs[workspace.active_tab_index()];
        assert_eq!(proxy_tab.custom_name.as_deref(), Some("buildbox::w1:p3"));
        let (pane_id, terminal_id) = app
            .remote_focus_operations
            .proxy_location(&started.operation_id)
            .expect("proxy bound to operation");
        assert!(proxy_tab.panes.contains_key(&pane_id));
        assert_eq!(proxy_tab.layout.focused(), pane_id);
        let terminal = app
            .state
            .terminals
            .get(&terminal_id)
            .expect("proxy terminal state");
        assert!(terminal.remote_proxy);
        assert_eq!(
            started.proxy_pane_id,
            app.public_pane_id(0, pane_id).expect("public pane id")
        );
        assert!(
            started.proxy_pane_id.contains(":p"),
            "the placeholder id is replaced by the real public pane id: {}",
            started.proxy_pane_id
        );

        // Input stays refused until the first frame and ControlReady.
        let runtime = app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("proxy runtime");
        assert!(runtime.is_remote_proxy());
        assert!(
            runtime
                .try_send_bytes(bytes::Bytes::from_static(b"early"))
                .is_err(),
            "keystrokes before ControlReady are refused, not buffered"
        );
        let mut recording = recording.lock().expect("lock");
        assert!(
            recording
                .outbound
                .as_mut()
                .expect("outbound channel")
                .try_recv()
                .is_err(),
            "refused keystrokes never reach the transport"
        );
    }

    #[test]
    fn input_gate_opens_only_after_control_ready_and_first_frame() {
        let (mut app, recording) = proxy_app();
        let started = app
            .start_remote_focus_operation(agent_ref())
            .expect("operation starts");
        let terminal_id = proxy_terminal_id(&app, &started.operation_id);

        // A frame before ControlReady paints but does not open the gate.
        assert!(app.apply_remote_focus_frame(
            &started.operation_id,
            &full_frame(b"\x1b[1;1Hremote question"),
        ));
        {
            let runtime = app
                .terminal_runtimes
                .get(&terminal_id)
                .expect("proxy runtime");
            assert!(runtime.visible_text().contains("remote question"));
            assert!(
                runtime
                    .try_send_bytes(bytes::Bytes::from_static(b"too early"))
                    .is_err(),
                "a frame without ControlReady keeps input disabled"
            );
        }

        app.apply_remote_focus_transition(
            &started.operation_id,
            RemoteFocusTransition::Active(Box::new(context())),
        );
        assert!(app
            .remote_focus_operations
            .input_gate_open(&started.operation_id));
        app.terminal_runtimes
            .get(&terminal_id)
            .expect("proxy runtime")
            .try_send_bytes(bytes::Bytes::from_static(b"answer"))
            .expect("input flows after ControlReady and first frame");
        let mut recording = recording.lock().expect("lock");
        assert_eq!(
            recording
                .outbound
                .as_mut()
                .expect("outbound channel")
                .try_recv(),
            Ok(ProxyOutbound::Input(bytes::Bytes::from_static(b"answer")))
        );

        // The authoritative context becomes the compact identity line.
        let terminal = app
            .state
            .terminals
            .get(&terminal_id)
            .expect("proxy terminal");
        assert_eq!(
            terminal.manual_label.as_deref(),
            Some("buildbox::w1:p3 · operator · /work/repo · /dev/pts/4 · agent")
        );
    }

    #[test]
    fn control_ready_before_first_frame_keeps_the_gate_closed() {
        let (mut app, _recording) = proxy_app();
        let started = app
            .start_remote_focus_operation(agent_ref())
            .expect("operation starts");
        let terminal_id = proxy_terminal_id(&app, &started.operation_id);

        app.apply_remote_focus_transition(
            &started.operation_id,
            RemoteFocusTransition::Active(Box::new(context())),
        );
        {
            let runtime = app
                .terminal_runtimes
                .get(&terminal_id)
                .expect("proxy runtime");
            assert!(
                runtime
                    .try_send_bytes(bytes::Bytes::from_static(b"too early"))
                    .is_err(),
                "ControlReady without a complete frame keeps input disabled"
            );
        }

        assert!(app
            .apply_remote_focus_frame(&started.operation_id, &full_frame(b"\x1b[1;1Hlate frame"),));
        app.terminal_runtimes
            .get(&terminal_id)
            .expect("proxy runtime")
            .try_send_bytes(bytes::Bytes::from_static(b"answer"))
            .expect("the first complete frame after ControlReady opens the gate");
    }

    #[test]
    fn partial_first_frame_does_not_open_the_input_gate() {
        let (mut app, _recording) = proxy_app();
        let started = app
            .start_remote_focus_operation(agent_ref())
            .expect("operation starts");
        let terminal_id = proxy_terminal_id(&app, &started.operation_id);

        let mut partial = full_frame(b"partial frame");
        partial.full = false;
        assert!(app.apply_remote_focus_frame(&started.operation_id, &partial));
        app.apply_remote_focus_transition(
            &started.operation_id,
            RemoteFocusTransition::Active(Box::new(context())),
        );

        assert!(!app
            .remote_focus_operations
            .input_gate_open(&started.operation_id));
        assert!(app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("proxy runtime")
            .try_send_bytes(bytes::Bytes::from_static(b"too early"))
            .is_err());
    }

    #[test]
    fn refused_proxy_press_drops_a_later_held_key_repeat() {
        let (mut app, recording) = proxy_app();
        let started = app
            .start_remote_focus_operation(agent_ref())
            .expect("operation starts");
        let terminal_id = proxy_terminal_id(&app, &started.operation_id);
        let key = crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Char('a'),
            crossterm::event::KeyModifiers::empty(),
        )
        .with_windows_record(crate::input::WindowsKeyRecord {
            key_down: true,
            repeat_count: 1,
            virtual_key_code: 65,
            virtual_scan_code: 30,
            unicode: b'a' as u16,
            control_key_state: 0,
        });

        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Key(key.clone())],
            false,
        );
        app.apply_remote_focus_transition(
            &started.operation_id,
            RemoteFocusTransition::Active(Box::new(context())),
        );
        app.apply_remote_focus_frame(&started.operation_id, &full_frame(b"ready"));
        assert!(app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("proxy runtime")
            .is_remote_proxy());

        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Key(
                key.with_kind(crossterm::event::KeyEventKind::Repeat),
            )],
            false,
        );

        assert!(recording
            .lock()
            .expect("lock")
            .outbound
            .as_mut()
            .expect("outbound channel")
            .try_recv()
            .is_err());
    }

    #[test]
    fn failure_closes_the_proxy_pane_and_releases_the_lease() {
        let (mut app, recording) = proxy_app();
        let started = app
            .start_remote_focus_operation(agent_ref())
            .expect("operation starts");
        let operation_id = started.operation_id.clone();
        let terminal_id = proxy_terminal_id(&app, &operation_id);
        assert_eq!(app.state.workspaces[0].tabs.len(), 2);

        app.apply_remote_focus_transition(
            &operation_id,
            RemoteFocusTransition::Failed(ErrorBody {
                code: "connection_lost".into(),
                message: "stream ended".into(),
            }),
        );

        assert_eq!(
            app.state.workspaces[0].tabs.len(),
            1,
            "a failed focus closes its proxy pane"
        );
        assert!(
            app.terminal_runtimes.get(&terminal_id).is_none(),
            "the proxy runtime shuts down with the pane"
        );
        assert_eq!(
            recording.lock().expect("lock").detached,
            vec![operation_id.clone()]
        );
        let snapshot = app
            .remote_focus_status(&operation_id)
            .expect("operation stays queryable");
        assert_eq!(snapshot.state, RemoteFocusState::Failed);
        assert_eq!(
            snapshot.error.map(|error| error.code),
            Some("connection_lost".to_string())
        );
    }

    #[test]
    fn closing_the_proxy_pane_detaches_and_closes_the_operation() {
        let (mut app, recording) = proxy_app();
        let started = app
            .start_remote_focus_operation(agent_ref())
            .expect("operation starts");
        let operation_id = started.operation_id.clone();
        let (pane_id, terminal_id) = app
            .remote_focus_operations
            .proxy_location(&operation_id)
            .expect("proxy location");
        app.apply_remote_focus_transition(
            &operation_id,
            RemoteFocusTransition::Active(Box::new(context())),
        );
        app.apply_remote_focus_frame(&operation_id, &full_frame(b"\x1b[1;1Hlive"));

        // The user closes the pane: the workspace close path queues the
        // terminal for shutdown, and the shutdown releases the lease.
        let should_close = app.state.workspaces[0].remove_pane(pane_id);
        assert!(!should_close);
        app.state.remove_unattached_terminal_ids([terminal_id]);
        app.shutdown_detached_terminal_runtimes();

        assert_eq!(
            recording.lock().expect("lock").detached,
            vec![operation_id.clone()]
        );
        let snapshot = app
            .remote_focus_status(&operation_id)
            .expect("operation stays queryable");
        assert_eq!(snapshot.state, RemoteFocusState::Closed);
    }

    #[test]
    fn windowless_run_rows_are_rejected_before_a_pane_opens() {
        let (mut app, recording) = proxy_app();
        app.state.fleet_snapshot = crate::fleet::Snapshot {
            hosts: vec![crate::fleet::HostSnapshot {
                name: "buildbox".into(),
                target: "buildbox".into(),
                local: false,
                session: None,
                socket: None,
                state: crate::fleet::HostState::Reachable,
                version: None,
                protocol: None,
                error: None,
                entries: vec![crate::fleet::FleetRow::test_run_row(
                    "buildbox", "w1:p3", false,
                )],
            }],
            ..crate::fleet::Snapshot::default()
        };

        let error = app
            .start_remote_focus_operation(agent_ref())
            .expect_err("windowless runs are not attachable");
        assert_eq!(error.code, "agent_not_attachable");
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert!(recording.lock().expect("lock").started.is_empty());
    }

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
