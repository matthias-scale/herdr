use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

// Effective state arbitration is intentionally centralized here. Full lifecycle
// Herdr hook integrations are authoritative while live, except for settled
// visible working evidence that can start a new turn after hook-reported idle.
// Process-exit updates clear matching hook authority before recomputing state.

use crate::detect::{Agent, AgentState};
use crate::terminal::TerminalId;

pub(crate) const AGENT_STALE_SILENCE: Duration = Duration::from_secs(20 * 60);
pub(crate) const DECLARED_WAIT_GRACE: Duration = Duration::from_secs(30);

#[path = "metadata.rs"]
mod metadata;
pub use metadata::{AgentMetadata, AgentMetadataReport, EffectivePresentation};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookAuthority {
    pub source: String,
    pub agent_label: String,
    pub state: AgentState,
    pub message: Option<String>,
    pub reported_at: Instant,
    pub wait: Option<String>,
    pub eta_s: Option<u64>,
    pub reported_at_wire: Option<String>,
    pub session_ref: Option<crate::agent_resume::AgentSessionRef>,
    pub retired_at: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SuppressedFullLifecycleHookReport {
    agent_label: String,
    session_ref: Option<crate::agent_resume::AgentSessionRef>,
    observed_at: Instant,
    reason: FullLifecycleHookSuppressionReason,
    replacement_session_ref: Option<crate::agent_resume::AgentSessionRef>,
    pending_replacement_report: Option<PendingFullLifecycleHookReport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingFullLifecycleHookReport {
    authority: HookAuthority,
    seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FullLifecycleHookSuppressionReason {
    HookClear,
    ProcessExit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FullLifecycleHookReportRoute {
    Accept { reanchor_sequence: bool },
    Ignore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StaleFullLifecycleHookSession {
    agent_label: String,
    session_ref: crate::agent_resume::AgentSessionRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagedAgentPhase {
    Pending {
        ready_after: Option<Instant>,
        deadline: Instant,
        observed_expected: bool,
    },
    Active,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ManagedAgent {
    kind: Agent,
    phase: ManagedAgentPhase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveStateChange {
    pub previous_agent_label: Option<String>,
    pub previous_known_agent: Option<Agent>,
    pub previous_state: AgentState,
    pub previous_presentation: EffectivePresentation,
    pub agent_label: Option<String>,
    pub known_agent: Option<Agent>,
    pub state: AgentState,
    pub presentation: EffectivePresentation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct TerminalTitleChange {
    pub(crate) raw_changed: bool,
    pub(crate) stripped_changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TerminalStateMutation {
    pub effective_state_change: Option<EffectiveStateChange>,
    pub session_ref_changed: bool,
    pub session_replaced: bool,
    pub hook_work_context_changed: bool,
    pub agent_released: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentNameOwner {
    agent_label: String,
    session_ref: Option<crate::agent_resume::AgentSessionRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum AgentActivityOwner {
    Session {
        source: String,
        agent_label: String,
        kind: crate::agent_resume::AgentSessionRefKind,
        value: String,
    },
    Agent {
        agent_label: String,
        previous_session: Option<AgentActivitySession>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AgentActivitySession {
    source: String,
    kind: crate::agent_resume::AgentSessionRefKind,
    value: String,
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AgentActivityHandoffState {
    state: AgentActivityHandoffStatus,
    active_elapsed: Option<Duration>,
    last_active_elapsed: Option<Duration>,
    owner: Option<AgentActivityOwner>,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum AgentActivityHandoffStatus {
    Idle,
    Working,
    Blocked,
    Unknown,
}

#[cfg(unix)]
impl From<AgentState> for AgentActivityHandoffStatus {
    fn from(state: AgentState) -> Self {
        match state {
            AgentState::Idle => Self::Idle,
            AgentState::Working => Self::Working,
            AgentState::Blocked => Self::Blocked,
            AgentState::Unknown => Self::Unknown,
        }
    }
}

#[cfg(unix)]
impl From<AgentActivityHandoffStatus> for AgentState {
    fn from(state: AgentActivityHandoffStatus) -> Self {
        match state {
            AgentActivityHandoffStatus::Idle => Self::Idle,
            AgentActivityHandoffStatus::Working => Self::Working,
            AgentActivityHandoffStatus::Blocked => Self::Blocked,
            AgentActivityHandoffStatus::Unknown => Self::Unknown,
        }
    }
}

impl AgentActivityOwner {
    fn agent_label(&self) -> &str {
        match self {
            Self::Session { agent_label, .. } | Self::Agent { agent_label, .. } => agent_label,
        }
    }

    fn session(&self) -> Option<AgentActivitySession> {
        match self {
            Self::Session {
                source,
                kind,
                value,
                ..
            } => Some(AgentActivitySession {
                source: source.clone(),
                kind: *kind,
                value: value.clone(),
            }),
            Self::Agent {
                previous_session, ..
            } => previous_session.clone(),
        }
    }

    fn inherit_detection_lineage(&mut self, previous: Option<&Self>) {
        let Self::Agent {
            agent_label,
            previous_session,
        } = self
        else {
            return;
        };
        let Some(previous) = previous.filter(|owner| owner.agent_label() == agent_label) else {
            return;
        };
        *previous_session = previous.session();
    }

    /// Detection can identify an agent before a session hook supplies its
    /// stronger identity, and can remain after that hook clears. Those
    /// refinement transitions belong to one live activity interval. A
    /// detection fallback remembers its last authoritative session so a later
    /// different session cannot inherit that interval.
    fn continues_activity_from(&self, previous: &Self) -> bool {
        match (previous, self) {
            (Self::Session { .. }, Self::Session { .. }) => previous == self,
            (
                Self::Agent {
                    previous_session, ..
                },
                Self::Session { .. },
            ) => {
                previous.agent_label() == self.agent_label()
                    && previous_session
                        .as_ref()
                        .is_none_or(|session| Some(session.clone()) == self.session())
            }
            _ => previous.agent_label() == self.agent_label(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RecentAgentProcessExit {
    agent: Agent,
    observed_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkTitleInitialSubject {
    agent_label: String,
    lifecycle_source: String,
    session_id: String,
    title: String,
}

/// Pure state for a server-owned terminal.
///
/// During the migration this is still one-to-one with a pane-backed PTY, but
/// pane/view state no longer owns terminal identity, cwd, labels, or agent
/// metadata.
pub struct TerminalState {
    pub id: TerminalId,
    pub cwd: PathBuf,
    pub detected_agent: Option<Agent>,
    pub fallback_state: AgentState,
    fallback_visible_blocker: bool,
    fallback_visible_working: bool,
    fallback_visible_working_observed_at: Option<Instant>,
    fallback_working_observed_at: Option<Instant>,
    fallback_observed_at: Option<Instant>,
    pub hook_authority: Option<HookAuthority>,
    pub supervisor_stale: bool,
    pub agent_metadata: HashMap<String, AgentMetadata>,
    pub work_context: crate::work_context::PaneWorkContextState,
    work_title_initial_subject: Option<WorkTitleInitialSubject>,
    pub metadata_tokens: crate::metadata_tokens::MetadataTokens,
    pub closing_gates: Vec<crate::api::schema::ClosingBlockItem>,
    pub closing_items: Vec<crate::api::schema::ClosingBlockItem>,
    pub closing_decisions: Vec<crate::api::schema::ClosingBlockDecision>,
    pub persisted_agent_session: Option<crate::agent_resume::PersistedAgentSession>,
    pub terminal_title: Option<String>,
    pub manual_label: Option<String>,
    pub agent_name: Option<String>,
    agent_name_owner: Option<AgentNameOwner>,
    managed_agent: Option<ManagedAgent>,
    hook_report_sequences: HashMap<String, u64>,
    suppressed_full_lifecycle_hook_reports: HashMap<String, SuppressedFullLifecycleHookReport>,
    stale_full_lifecycle_hook_sessions: HashMap<String, Vec<StaleFullLifecycleHookSession>>,
    metadata_report_sequences: HashMap<String, u64>,
    metadata_report_agents: HashMap<String, Agent>,
    metadata_token_sequence_sources: std::collections::HashSet<String>,
    pub state: AgentState,
    /// Provider-reported background jobs owned by this agent thread. `None`
    /// means the provider does not expose a supported count.
    pub background_job_count: Option<u16>,
    /// Last background observation of the pane's distinct foreground process.
    pub(crate) foreground_process_name: Option<String>,
    foreground_process_active: bool,
    pub last_agent_state_change_seq: Option<u64>,
    agent_active_since: Option<Instant>,
    agent_last_active_at: Option<Instant>,
    agent_activity_owner: Option<AgentActivityOwner>,
    pub revision: u64,
    pub launch_argv: Option<Vec<String>>,
    pub respawn_shell_on_exit: bool,
    recent_agent_process_exit: Option<RecentAgentProcessExit>,
    pub pending_agent_resume_plan: Option<crate::agent_resume::AgentResumePlan>,
}

fn normalize_declared_wait(
    state: AgentState,
    wait: Option<String>,
    eta_s: Option<u64>,
) -> (Option<String>, Option<u64>) {
    const MAX_DECLARED_WAIT_S: u64 = 7 * 24 * 60 * 60;
    let wait = wait
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string());
    if state != AgentState::Working || wait.is_none() {
        return (None, None);
    }
    let Some(eta_s) = eta_s.filter(|eta_s| *eta_s <= MAX_DECLARED_WAIT_S) else {
        return (None, None);
    };
    (wait, Some(eta_s))
}

impl TerminalState {
    pub fn new(id: TerminalId, cwd: PathBuf) -> Self {
        Self {
            id,
            cwd,
            detected_agent: None,
            fallback_state: AgentState::Unknown,
            fallback_visible_blocker: false,
            fallback_visible_working: false,
            fallback_visible_working_observed_at: None,
            fallback_working_observed_at: None,
            fallback_observed_at: None,
            hook_authority: None,
            supervisor_stale: false,
            agent_metadata: HashMap::new(),
            work_context: crate::work_context::PaneWorkContextState::default(),
            work_title_initial_subject: None,
            metadata_tokens: crate::metadata_tokens::MetadataTokens::default(),
            closing_gates: Vec::new(),
            closing_items: Vec::new(),
            closing_decisions: Vec::new(),
            persisted_agent_session: None,
            terminal_title: None,
            manual_label: None,
            agent_name: None,
            agent_name_owner: None,
            managed_agent: None,
            hook_report_sequences: HashMap::new(),
            suppressed_full_lifecycle_hook_reports: HashMap::new(),
            stale_full_lifecycle_hook_sessions: HashMap::new(),
            metadata_report_sequences: HashMap::new(),
            metadata_report_agents: HashMap::new(),
            metadata_token_sequence_sources: std::collections::HashSet::new(),
            state: AgentState::Unknown,
            background_job_count: None,
            foreground_process_name: None,
            foreground_process_active: false,
            last_agent_state_change_seq: None,
            agent_active_since: None,
            agent_last_active_at: None,
            agent_activity_owner: None,
            revision: 0,
            launch_argv: None,
            respawn_shell_on_exit: false,
            recent_agent_process_exit: None,
            pending_agent_resume_plan: None,
        }
    }

    pub(crate) fn apply_closing_block_payload(
        &mut self,
        gates: Vec<crate::api::schema::ClosingBlockItem>,
        items: Vec<crate::api::schema::ClosingBlockItem>,
        decisions: Vec<crate::api::schema::ClosingBlockDecision>,
    ) -> bool {
        if self.closing_gates == gates
            && self.closing_items == items
            && self.closing_decisions == decisions
        {
            return false;
        }
        self.closing_gates = gates;
        self.closing_items = items;
        self.closing_decisions = decisions;
        self.revision = self.revision.saturating_add(1);
        true
    }

    pub fn effective_work_context(&self) -> &crate::work_context::PaneWorkContext {
        self.work_context.effective()
    }

    pub(crate) fn apply_manual_work_context_patch(
        &mut self,
        patch: crate::work_context::PaneWorkContextPatch,
    ) -> Result<bool, String> {
        let changed = self.work_context.apply_manual_patch(patch)?;
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        Ok(changed)
    }

    pub(crate) fn replace_hook_work_context(
        &mut self,
        context: crate::work_context::PaneWorkContext,
    ) -> Result<bool, String> {
        let changed = self.work_context.replace_hook_turn(context)?;
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        Ok(changed)
    }

    pub(crate) fn replace_git_work_context(
        &mut self,
        context: crate::work_context::PaneWorkContext,
    ) -> Result<bool, String> {
        let changed = self.work_context.replace_git_observation(context)?;
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        Ok(changed)
    }

    /// The hook tier is persisted for restore fidelity, but any accepted
    /// mutation that tears down or replaces the session identity that authorized guarded
    /// work-title reports must also drop the hook tier, so stale ticket/PR refs
    /// never outlive their session. Manual, git, and restored-fallback tiers are
    /// untouched.
    fn clear_hook_work_context(&mut self) -> bool {
        let changed = self.work_context.clear_hook_turn();
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        changed
    }

    pub(crate) fn restore_work_context(
        &mut self,
        context: crate::work_context::PaneWorkContext,
    ) -> Result<(), String> {
        self.work_context = crate::work_context::PaneWorkContextState::from_restored(context)?;
        Ok(())
    }

    pub(crate) fn restore_work_context_with_tiers(
        &mut self,
        flat: crate::work_context::PaneWorkContext,
        tiers: Option<crate::work_context::PaneWorkContextTiers>,
    ) -> Result<(), String> {
        let Some(tiers) = tiers else {
            return self.restore_work_context(flat);
        };
        self.work_context =
            crate::work_context::PaneWorkContextState::from_restored_with_tiers(flat, Some(tiers))?;
        Ok(())
    }

    pub(crate) fn set_background_job_count(&mut self, count: Option<u16>) -> bool {
        if self.background_job_count == count {
            return false;
        }
        self.background_job_count = count;
        self.revision = self.revision.wrapping_add(1);
        true
    }

    pub(crate) fn set_foreground_process(
        &mut self,
        name: Option<String>,
        active: bool,
        now: Instant,
    ) -> Option<TerminalStateMutation> {
        if self.foreground_process_name == name && self.foreground_process_active == active {
            return None;
        }
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        self.foreground_process_name = name;
        self.foreground_process_active = active;
        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            ..TerminalStateMutation::default()
        })
    }

    pub(crate) fn foreground_process_active(&self) -> bool {
        self.foreground_process_active
    }

    pub(crate) fn terminal_title_stripped(&self) -> Option<String> {
        self.terminal_title
            .as_deref()
            .and_then(super::stripped_terminal_title)
    }

    pub(crate) fn resolve_work_title_for_session(
        &mut self,
        agent_label: &str,
        lifecycle_source: &str,
        session_id: &str,
        latest_title: Option<String>,
    ) -> Option<String> {
        let same_session = self
            .work_title_initial_subject
            .as_ref()
            .is_some_and(|initial| {
                initial.agent_label == agent_label
                    && initial.lifecycle_source == lifecycle_source
                    && initial.session_id == session_id
            });
        if same_session {
            return latest_title.or_else(|| {
                self.work_title_initial_subject
                    .as_ref()
                    .map(|initial| initial.title.clone())
            });
        }
        let title = latest_title?;
        self.work_title_initial_subject = Some(WorkTitleInitialSubject {
            agent_label: agent_label.to_string(),
            lifecycle_source: lifecycle_source.to_string(),
            session_id: session_id.to_string(),
            title: title.clone(),
        });
        Some(title)
    }

    pub(crate) fn set_terminal_title(&mut self, title: Option<String>) -> TerminalTitleChange {
        if self.terminal_title == title {
            return TerminalTitleChange::default();
        }
        let previous_stripped = self.terminal_title_stripped();
        self.terminal_title = title;
        let stripped_changed = previous_stripped != self.terminal_title_stripped();
        if stripped_changed {
            self.revision = self.revision.wrapping_add(1);
        }
        TerminalTitleChange {
            raw_changed: true,
            stripped_changed,
        }
    }

    pub fn with_launch_argv(mut self, argv: Vec<String>) -> Self {
        self.launch_argv = Some(argv);
        self
    }

    pub fn with_respawn_shell_on_exit(mut self) -> Self {
        self.respawn_shell_on_exit = true;
        self
    }

    #[cfg(any(windows, test))]
    pub(crate) fn agent_process_exited_within(&self, now: Instant, max_age: Duration) -> bool {
        self.recent_agent_process_exit
            .is_some_and(|exit| now.saturating_duration_since(exit.observed_at) <= max_age)
    }

    pub fn with_pending_agent_resume_plan(
        mut self,
        plan: crate::agent_resume::AgentResumePlan,
    ) -> Self {
        self.pending_agent_resume_plan = Some(plan);
        self
    }

    #[cfg(test)]
    pub fn set_detected_state(
        &mut self,
        agent: Option<Agent>,
        fallback_state: AgentState,
    ) -> Option<EffectiveStateChange> {
        self.set_detected_state_with_visible_blocker(agent, fallback_state, false, false, false)
    }

    #[cfg(test)]
    pub fn set_detected_state_with_mutation(
        &mut self,
        agent: Option<Agent>,
        fallback_state: AgentState,
    ) -> TerminalStateMutation {
        self.set_detected_state_with_screen_signals_at(
            agent,
            fallback_state,
            false,
            false,
            false,
            false,
            Instant::now(),
        )
    }

    #[cfg(test)]
    pub fn set_detected_state_with_visible_blocker(
        &mut self,
        agent: Option<Agent>,
        fallback_state: AgentState,
        visible_blocker: bool,
        _ignored_screen_idle: bool,
        process_exited: bool,
    ) -> Option<EffectiveStateChange> {
        self.set_detected_state_with_screen_signals_at(
            agent,
            fallback_state,
            visible_blocker,
            false,
            false,
            process_exited,
            Instant::now(),
        )
        .effective_state_change
    }

    pub fn set_detected_state_with_screen_signals_at(
        &mut self,
        agent: Option<Agent>,
        fallback_state: AgentState,
        visible_blocker: bool,
        _visible_idle: bool,
        visible_working: bool,
        process_exited: bool,
        now: Instant,
    ) -> TerminalStateMutation {
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let previous_detected_agent = self.detected_agent;
        let previous_session = self.current_session_identity_for_persistence();
        let visible_working_signal = visible_working && fallback_state == AgentState::Working;
        self.fallback_visible_working = visible_working_signal;
        self.fallback_visible_working_observed_at = visible_working_signal.then_some(now);
        self.fallback_working_observed_at = (fallback_state == AgentState::Working).then_some(now);
        let newer_custom_authority = process_exited
            && self.hook_authority.as_ref().is_some_and(|authority| {
                crate::detect::parse_agent_label(&authority.agent_label) == agent
                    && !crate::agent_resume::is_official_agent_source(
                        &authority.source,
                        &authority.agent_label,
                    )
                    && authority.reported_at > now
            });
        let agent_released = process_exited
            && !newer_custom_authority
            && (previous_agent_label.is_some() || self.agent_name.is_some());
        if self.should_ignore_detected_state_under_full_lifecycle_hook(agent, process_exited) {
            if self
                .hook_authority
                .as_ref()
                .and_then(|authority| crate::detect::parse_agent_label(&authority.agent_label))
                == agent
            {
                self.detected_agent = agent;
            }
            return TerminalStateMutation {
                effective_state_change: self.recompute_effective_state(
                    previous_agent_label,
                    previous_known_agent,
                    previous_state,
                    previous_presentation,
                    now,
                ),
                session_ref_changed: previous_session
                    != self.current_session_identity_for_persistence(),
                session_replaced: previous_session.is_some()
                    && previous_session != self.current_session_identity_for_persistence(),
                hook_work_context_changed: false,
                agent_released: false,
            };
        }
        let replacement_process_detected = !process_exited
            && agent.is_some()
            && self
                .recent_agent_process_exit
                .is_some_and(|exit| Some(exit.agent) == agent && exit.observed_at < now);
        if !process_exited && self.detected_state_observed_before_release_suppression(agent, now) {
            return TerminalStateMutation {
                effective_state_change: self.recompute_effective_state(
                    previous_agent_label,
                    previous_known_agent,
                    previous_state,
                    previous_presentation,
                    now,
                ),
                session_ref_changed: previous_session
                    != self.current_session_identity_for_persistence(),
                session_replaced: previous_session.is_some()
                    && previous_session != self.current_session_identity_for_persistence(),
                hook_work_context_changed: false,
                agent_released: false,
            };
        }
        self.detected_agent = agent;
        if let Some(agent) = agent {
            let agent_label = crate::detect::agent_label(agent);
            self.reconcile_agent_name_owner(agent_label, None);
        }
        if !process_exited {
            self.clear_full_lifecycle_hook_suppression_for_detected_agent(
                if replacement_process_detected {
                    None
                } else {
                    previous_detected_agent
                },
                agent,
            );
        }
        self.fallback_state = fallback_state;
        self.fallback_visible_blocker = visible_blocker && fallback_state == AgentState::Blocked;
        self.fallback_observed_at = Some(now);
        if process_exited {
            if let Some(agent) = agent {
                self.recent_agent_process_exit = Some(RecentAgentProcessExit {
                    agent,
                    observed_at: now,
                });
            }
        } else if agent.is_some() {
            self.recent_agent_process_exit = None;
        }
        if process_exited {
            let mut reset_sources = Vec::new();
            let mut stale_sessions = Vec::new();
            for (source, suppressed) in &mut self.suppressed_full_lifecycle_hook_reports {
                if crate::detect::parse_agent_label(&suppressed.agent_label) != agent
                    || suppressed.reason == FullLifecycleHookSuppressionReason::HookClear
                {
                    continue;
                }
                let exited_session_ref = suppressed
                    .replacement_session_ref
                    .take()
                    .or_else(|| {
                        suppressed
                            .pending_replacement_report
                            .as_ref()
                            .and_then(|pending| pending.authority.session_ref.clone())
                    })
                    .or_else(|| suppressed.session_ref.clone());
                if let (Some(previous), Some(exited)) =
                    (suppressed.session_ref.as_ref(), exited_session_ref.as_ref())
                {
                    if previous != exited {
                        stale_sessions.push((
                            source.clone(),
                            suppressed.agent_label.clone(),
                            previous.clone(),
                        ));
                    }
                }
                suppressed.session_ref = exited_session_ref;
                suppressed.pending_replacement_report = None;
                suppressed.observed_at = now;
                reset_sources.push(source.clone());
            }
            for (source, agent_label, session_ref) in stale_sessions {
                self.remember_stale_full_lifecycle_hook_session(source, agent_label, session_ref);
            }
            for source in reset_sources {
                self.hook_report_sequences.remove(&source);
            }

            let official_session = self
                .hook_authority
                .as_ref()
                .filter(|authority| {
                    crate::agent_resume::is_official_agent_source(
                        &authority.source,
                        &authority.agent_label,
                    ) && crate::detect::parse_agent_label(&authority.agent_label) == agent
                })
                .map(|authority| {
                    (
                        authority.source.clone(),
                        authority.agent_label.clone(),
                        authority.session_ref.clone(),
                    )
                })
                .or_else(|| {
                    self.persisted_agent_session.as_ref().and_then(|session| {
                        (crate::agent_resume::is_official_agent_source(
                            &session.source,
                            &session.agent,
                        ) && crate::detect::parse_agent_label(&session.agent) == agent)
                            .then(|| {
                                (
                                    session.source.clone(),
                                    session.agent.clone(),
                                    Some(session.session_ref.clone()),
                                )
                            })
                    })
                });
            if let Some((source, agent_label, session_ref)) = official_session {
                self.hook_report_sequences.remove(&source);
                self.suppress_full_lifecycle_hook_report_with_session_ref(
                    source,
                    agent_label,
                    session_ref,
                    FullLifecycleHookSuppressionReason::ProcessExit,
                    now,
                );
            }
            let cleared_hook_source = self.hook_authority.as_ref().and_then(|authority| {
                (crate::detect::parse_agent_label(&authority.agent_label) == agent
                    && !newer_custom_authority)
                    .then(|| authority.source.clone())
            });
            if let Some(source) = cleared_hook_source {
                self.hook_report_sequences.remove(&source);
                self.hook_authority = None;
            }
            if !newer_custom_authority
                && self
                    .persisted_agent_session
                    .as_ref()
                    .is_some_and(|session| {
                        crate::detect::parse_agent_label(&session.agent) == agent
                    })
            {
                self.persisted_agent_session = None;
            }
            if let Some(agent) = agent {
                let agent_label = crate::detect::agent_label(agent);
                let mut cleared_metadata_sources = Vec::new();
                self.agent_metadata.retain(|source, metadata| {
                    let official_metadata = crate::agent_resume::is_official_agent_source(
                        &metadata.source,
                        agent_label,
                    ) || metadata.applies_to_source.as_deref().is_some_and(
                        |applies_to| {
                            crate::agent_resume::is_official_agent_source(applies_to, agent_label)
                        },
                    );
                    let matches_agent =
                        metadata.agent_label.as_deref() == Some(agent_label) || official_metadata;
                    let clear = matches_agent && (official_metadata || metadata.reported_at <= now);
                    if clear {
                        cleared_metadata_sources.push(source.clone());
                    }
                    !clear
                });
                for source in cleared_metadata_sources {
                    self.metadata_report_sequences.remove(&source);
                    self.metadata_report_agents.remove(&source);
                    self.metadata_token_sequence_sources.remove(&source);
                }
                let mut exited_generation_sources = Vec::new();
                self.metadata_report_agents.retain(|source, owner| {
                    if *owner == agent {
                        exited_generation_sources.push(source.clone());
                        false
                    } else {
                        true
                    }
                });
                for source in exited_generation_sources {
                    self.metadata_report_sequences.remove(&source);
                    self.metadata_token_sequence_sources.remove(&source);
                }
            }
        }
        if self.hook_authority_not_newer_than(now)
            && (self.hook_authority_conflicts_with_detected_agent(agent)
                || (previous_detected_agent.is_some()
                    && agent != previous_detected_agent
                    && self.hook_authority.as_ref().is_some_and(|authority| {
                        crate::detect::parse_agent_label(&authority.agent_label)
                            == previous_detected_agent
                    })))
        {
            let durable_session = self.hook_authority.as_ref().and_then(|authority| {
                authority.session_ref.as_ref().map(|session_ref| {
                    crate::agent_resume::PersistedAgentSession {
                        source: authority.source.clone(),
                        agent: authority.agent_label.clone(),
                        session_ref: session_ref.clone(),
                    }
                })
            });
            self.suppress_current_full_lifecycle_hook_authority(
                FullLifecycleHookSuppressionReason::HookClear,
            );
            self.hook_authority = None;
            self.persisted_agent_session = durable_session;
        }
        if agent_released {
            self.clear_agent_name();
        }
        let current_session = self.current_session_identity_for_persistence();
        let session_ref_changed = previous_session != current_session;
        let hook_work_context_changed =
            if (process_exited && !newer_custom_authority) || previous_session != current_session {
                self.clear_hook_work_context()
            } else {
                false
            };
        TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed,
            session_replaced: previous_session.is_some() && session_ref_changed,
            hook_work_context_changed,
            agent_released,
        }
    }

    #[cfg(test)]
    pub fn set_hook_authority(
        &mut self,
        source: String,
        agent_label: String,
        state: AgentState,
        message: Option<String>,
        seq: Option<u64>,
    ) -> Option<EffectiveStateChange> {
        self.set_hook_authority_at(
            source,
            agent_label,
            state,
            message,
            None,
            seq,
            Instant::now(),
        )
        .and_then(|mutation| mutation.effective_state_change)
    }

    #[cfg(test)]
    pub fn set_hook_authority_with_session_ref(
        &mut self,
        source: String,
        agent_label: String,
        state: AgentState,
        message: Option<String>,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        seq: Option<u64>,
    ) -> Option<TerminalStateMutation> {
        self.set_hook_authority_at(
            source,
            agent_label,
            state,
            message,
            session_ref,
            seq,
            Instant::now(),
        )
    }

    #[cfg(test)]
    pub fn set_hook_authority_at(
        &mut self,
        source: String,
        agent_label: String,
        state: AgentState,
        message: Option<String>,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        seq: Option<u64>,
        now: Instant,
    ) -> Option<TerminalStateMutation> {
        self.set_hook_authority_report_at(
            source,
            agent_label,
            state,
            message,
            None,
            None,
            None,
            session_ref,
            seq,
            now,
        )
    }

    pub fn set_hook_authority_report_at(
        &mut self,
        source: String,
        agent_label: String,
        state: AgentState,
        message: Option<String>,
        wait: Option<String>,
        eta_s: Option<u64>,
        reported_at_wire: Option<String>,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        seq: Option<u64>,
        now: Instant,
    ) -> Option<TerminalStateMutation> {
        if crate::detect::session_identity_only_integration(&source, &agent_label) {
            return None;
        }
        if !crate::detect::full_lifecycle_hook_authority(&source, &agent_label)
            && self.recent_agent_process_exit.is_some_and(|exit| {
                crate::detect::parse_agent_label(&agent_label) == Some(exit.agent)
            })
        {
            return None;
        }
        let reanchor_sequence = match self.route_full_lifecycle_hook_report(
            &source,
            &agent_label,
            state,
            message.as_deref(),
            &session_ref,
            seq,
            now,
        ) {
            FullLifecycleHookReportRoute::Accept { reanchor_sequence } => reanchor_sequence,
            FullLifecycleHookReportRoute::Ignore => return None,
        };
        if self.known_agent_label_conflicts_with_detected_agent(&agent_label) {
            return None;
        }
        // A closing-block source claims no resume session, so it cannot contend
        // for the pane's session identity — that stays owned by the agent's own
        // integration (`herdr:claude`). Without this exemption the status report
        // is rejected here on every pane where the real agent has already
        // announced itself, which is every pane that matters.
        let owner_conflicts = !crate::detect::is_closing_block_source(&source, &agent_label)
            && self.current_session_owner_conflicts(&source, &agent_label);
        let foreground_takeover_allowed = owner_conflicts
            && self.foreground_agent_confirms_hook_authority_takeover(
                &source,
                &agent_label,
                &session_ref,
            );
        if owner_conflicts && !foreground_takeover_allowed {
            return None;
        }
        let session_ref = session_ref.map(|session_ref| {
            if self.lifecycle_hook_report_replaces_persisted_session(
                &source,
                &agent_label,
                &session_ref,
            ) {
                session_ref
            } else {
                self.conflicting_same_owner_session_ref(&source, &agent_label, &session_ref, None)
                    .unwrap_or(session_ref)
            }
        });
        if self.live_full_lifecycle_hook_authority_conflicts_with_session(
            &source,
            &agent_label,
            &session_ref,
        ) {
            return None;
        }
        if reanchor_sequence {
            self.hook_report_sequences.remove(&source);
        }
        if !self.accept_hook_report(&source, seq) {
            return None;
        }

        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let previous_session = self.current_session_identity_for_persistence();
        self.reconcile_agent_name_owner(&agent_label, session_ref.as_ref());
        if foreground_takeover_allowed {
            self.suppress_current_full_lifecycle_hook_authority(
                FullLifecycleHookSuppressionReason::HookClear,
            );
        }
        if session_ref.is_some() || reanchor_sequence {
            if let Some(suppressed) = self.suppressed_full_lifecycle_hook_reports.remove(&source) {
                if let Some(suppressed_ref) = suppressed.session_ref {
                    self.remember_stale_full_lifecycle_hook_session(
                        source.clone(),
                        suppressed.agent_label,
                        suppressed_ref,
                    );
                }
            }
        }
        self.persisted_agent_session = None;
        let (wait, eta_s) = normalize_declared_wait(state, wait, eta_s);
        self.hook_authority = Some(HookAuthority {
            source,
            agent_label,
            state,
            message,
            reported_at: now,
            wait,
            eta_s,
            reported_at_wire: reported_at_wire.filter(|value| !value.trim().is_empty()),
            session_ref,
            retired_at: None,
        });
        self.supervisor_stale = false;
        let current_session = self.current_session_identity_for_persistence();
        let session_ref_changed = previous_session != current_session;
        let hook_work_context_changed = if previous_session != current_session {
            self.clear_hook_work_context()
        } else {
            false
        };
        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed,
            session_replaced: previous_session.is_some() && session_ref_changed,
            hook_work_context_changed,
            agent_released: false,
        })
    }

    pub fn status_report_snapshot(&self) -> (Option<String>, Option<u64>, Option<String>, bool) {
        let Some(authority) = self.hook_authority.as_ref() else {
            return (None, None, None, self.supervisor_stale);
        };
        (
            authority.wait.clone(),
            authority.eta_s,
            authority.reported_at_wire.clone(),
            self.supervisor_stale,
        )
    }

    pub fn status_reported_at(&self) -> Option<Instant> {
        self.hook_authority
            .as_ref()
            .map(|authority| authority.reported_at)
    }

    pub fn agent_status_watchdog_deadline(&self) -> Option<Instant> {
        let authority = self.hook_authority.as_ref()?;
        if self.supervisor_stale || authority.state != AgentState::Working {
            return None;
        }
        let age = authority
            .wait
            .as_ref()
            .and(authority.eta_s)
            .map(|eta_s| Duration::from_secs(eta_s).saturating_add(DECLARED_WAIT_GRACE))
            .unwrap_or(AGENT_STALE_SILENCE);
        authority.reported_at.checked_add(age)
    }

    pub fn mark_agent_status_stale_at(&mut self, now: Instant) -> Option<TerminalStateMutation> {
        if self.supervisor_stale
            || self
                .agent_status_watchdog_deadline()
                .is_none_or(|deadline| now < deadline)
        {
            return None;
        }
        self.supervisor_stale = true;
        Some(TerminalStateMutation::default())
    }

    fn hook_authority_not_newer_than(&self, observed_at: Instant) -> bool {
        self.hook_authority
            .as_ref()
            .is_none_or(|authority| authority.reported_at <= observed_at)
    }

    fn fallback_not_older_than_hook(&self) -> bool {
        self.hook_authority.as_ref().is_none_or(|authority| {
            self.fallback_observed_at
                .is_some_and(|observed_at| authority.reported_at <= observed_at)
        })
    }

    fn hook_authority_conflicts_with_detected_agent(&self, detected_agent: Option<Agent>) -> bool {
        let Some(detected_agent) = detected_agent else {
            return false;
        };
        self.hook_authority.as_ref().is_some_and(|authority| {
            crate::detect::parse_agent_label(&authority.agent_label)
                .is_some_and(|hook_agent| hook_agent != detected_agent)
        })
    }

    fn should_ignore_detected_state_under_full_lifecycle_hook(
        &self,
        detected_agent: Option<Agent>,
        process_exited: bool,
    ) -> bool {
        self.live_full_lifecycle_hook_authority()
            && !process_exited
            && !self.hook_authority_conflicts_with_detected_agent(detected_agent)
    }

    fn persisted_agent_session_matches(&self, source: &str, agent: &str) -> bool {
        self.persisted_agent_session
            .as_ref()
            .is_some_and(|session| session.source == source && session.agent == agent)
    }

    fn suppress_current_full_lifecycle_hook_authority(
        &mut self,
        reason: FullLifecycleHookSuppressionReason,
    ) {
        if let Some((source, agent_label, session_ref)) =
            self.hook_authority.as_ref().and_then(|authority| {
                crate::detect::full_lifecycle_hook_authority(
                    &authority.source,
                    &authority.agent_label,
                )
                .then(|| {
                    (
                        authority.source.clone(),
                        authority.agent_label.clone(),
                        authority.session_ref.clone(),
                    )
                })
            })
        {
            self.suppress_full_lifecycle_hook_report_with_session_ref(
                source,
                agent_label,
                session_ref,
                reason,
                Instant::now(),
            );
        }
    }

    fn suppress_full_lifecycle_hook_report(
        &mut self,
        source: &str,
        agent_label: &str,
        reason: FullLifecycleHookSuppressionReason,
    ) {
        if crate::detect::full_lifecycle_hook_authority(source, agent_label) {
            let session_ref = self
                .hook_authority
                .as_ref()
                .and_then(|authority| authority.session_ref.clone());
            self.suppress_full_lifecycle_hook_report_with_session_ref(
                source.to_string(),
                agent_label.to_string(),
                session_ref,
                reason,
                Instant::now(),
            );
        }
    }

    fn suppress_full_lifecycle_hook_report_with_session_ref(
        &mut self,
        source: String,
        agent_label: String,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        reason: FullLifecycleHookSuppressionReason,
        observed_at: Instant,
    ) {
        self.suppressed_full_lifecycle_hook_reports.insert(
            source,
            SuppressedFullLifecycleHookReport {
                agent_label,
                session_ref,
                observed_at,
                reason,
                replacement_session_ref: None,
                pending_replacement_report: None,
            },
        );
    }

    fn route_full_lifecycle_hook_report(
        &mut self,
        source: &str,
        agent_label: &str,
        state: AgentState,
        message: Option<&str>,
        session_ref: &Option<crate::agent_resume::AgentSessionRef>,
        seq: Option<u64>,
        reported_at: Instant,
    ) -> FullLifecycleHookReportRoute {
        if !crate::detect::full_lifecycle_hook_authority(source, agent_label) {
            return FullLifecycleHookReportRoute::Accept {
                reanchor_sequence: false,
            };
        }
        if self.full_lifecycle_hook_report_matches_stale_session(source, agent_label, session_ref) {
            return FullLifecycleHookReportRoute::Ignore;
        }

        let known_agent = crate::detect::parse_agent_label(agent_label);
        let process_present = known_agent.is_some()
            && self.detected_agent == known_agent
            && self.recent_agent_process_exit.is_none();
        let session_anchored = self
            .hook_authority
            .as_ref()
            .filter(|authority| authority.source == source && authority.agent_label == agent_label)
            .and_then(|authority| authority.session_ref.as_ref())
            .or_else(|| {
                self.persisted_agent_session
                    .as_ref()
                    .filter(|session| session.source == source && session.agent == agent_label)
                    .map(|session| &session.session_ref)
            })
            .is_some_and(|anchored| {
                session_ref
                    .as_ref()
                    .is_none_or(|incoming| incoming == anchored)
            });
        // A closing-block source reports what the agent just said, not where to
        // resume it, so it never carries an `AgentSessionRef`
        // (`agent_resume::session_ref_from_report` mints one only for official
        // sources). Every path below admits a report by anchoring it to a
        // session, so without this arm the report is unreachable: it falls to
        // the `session_ref` bail-out and is stored as a pending replacement that
        // nothing ever drains while the agent stays continuously detected.
        //
        // Presence of the agent's own process in the pane is the entire guard
        // this source claims (see `detect::is_closing_block_source`), and
        // `accept_hook_report` still enforces per-source sequence monotonicity
        // downstream, so accepting on presence alone loses no protection.
        if process_present && crate::detect::is_closing_block_source(source, agent_label) {
            return FullLifecycleHookReportRoute::Accept {
                reanchor_sequence: false,
            };
        }
        if let Some(suppressed) = self.suppressed_full_lifecycle_hook_reports.get(source) {
            if suppressed.agent_label != agent_label {
                return FullLifecycleHookReportRoute::Ignore;
            }
            if suppressed.reason == FullLifecycleHookSuppressionReason::HookClear {
                let reanchor_sequence = matches!(
                    (&suppressed.session_ref, session_ref),
                    (Some(previous), Some(incoming)) if previous != incoming
                );
                return if reanchor_sequence {
                    FullLifecycleHookReportRoute::Accept {
                        reanchor_sequence: true,
                    }
                } else {
                    FullLifecycleHookReportRoute::Ignore
                };
            }
        }

        if process_present
            && session_anchored
            && !self
                .suppressed_full_lifecycle_hook_reports
                .contains_key(source)
        {
            return FullLifecycleHookReportRoute::Accept {
                reanchor_sequence: self
                    .full_lifecycle_hook_report_has_fresh_session_after_stale_session(
                        source,
                        agent_label,
                        session_ref,
                    ),
            };
        }

        let Some(session_ref) = session_ref.clone() else {
            return FullLifecycleHookReportRoute::Ignore;
        };
        let Some(seq) = seq else {
            return FullLifecycleHookReportRoute::Ignore;
        };
        if self
            .hook_report_sequences
            .get(source)
            .is_some_and(|previous| seq <= *previous)
        {
            return FullLifecycleHookReportRoute::Ignore;
        }

        let previous_session_ref = self
            .persisted_agent_session
            .as_ref()
            .filter(|session| session.source == source && session.agent == agent_label)
            .map(|session| session.session_ref.clone());
        let suppressed = self
            .suppressed_full_lifecycle_hook_reports
            .entry(source.to_string())
            .or_insert_with(|| SuppressedFullLifecycleHookReport {
                agent_label: agent_label.to_string(),
                session_ref: previous_session_ref,
                observed_at: reported_at,
                reason: FullLifecycleHookSuppressionReason::ProcessExit,
                replacement_session_ref: None,
                pending_replacement_report: None,
            });
        let replace_pending = suppressed
            .pending_replacement_report
            .as_ref()
            .is_none_or(|pending| seq > pending.seq);
        if replace_pending {
            suppressed.pending_replacement_report = Some(PendingFullLifecycleHookReport {
                authority: HookAuthority {
                    source: source.to_string(),
                    agent_label: agent_label.to_string(),
                    state,
                    message: message.map(str::to_string),
                    reported_at,
                    wait: None,
                    eta_s: None,
                    reported_at_wire: None,
                    session_ref: Some(session_ref),
                    retired_at: None,
                },
                seq,
            });
        }
        FullLifecycleHookReportRoute::Ignore
    }

    fn full_lifecycle_hook_report_matches_stale_session(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &Option<crate::agent_resume::AgentSessionRef>,
    ) -> bool {
        if !crate::detect::full_lifecycle_hook_authority(source, agent_label) {
            return false;
        }
        self.stale_full_lifecycle_hook_sessions
            .get(source)
            .is_some_and(|stale_sessions| {
                session_ref.as_ref().is_some_and(|incoming_ref| {
                    stale_sessions.iter().any(|stale| {
                        stale.agent_label == agent_label && incoming_ref == &stale.session_ref
                    })
                })
            })
    }

    fn full_lifecycle_hook_report_has_fresh_session_after_stale_session(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &Option<crate::agent_resume::AgentSessionRef>,
    ) -> bool {
        if !crate::detect::full_lifecycle_hook_authority(source, agent_label) {
            return false;
        }
        self.stale_full_lifecycle_hook_sessions
            .get(source)
            .is_some_and(|stale_sessions| {
                stale_sessions
                    .iter()
                    .any(|stale| stale.agent_label == agent_label)
                    && session_ref.as_ref().is_some_and(|incoming_ref| {
                        stale_sessions.iter().all(|stale| {
                            stale.agent_label != agent_label || incoming_ref != &stale.session_ref
                        })
                    })
            })
    }

    fn live_full_lifecycle_hook_authority_conflicts_with_session(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &Option<crate::agent_resume::AgentSessionRef>,
    ) -> bool {
        let Some(authority) = self.hook_authority.as_ref() else {
            return false;
        };
        if !crate::detect::full_lifecycle_hook_authority(&authority.source, &authority.agent_label)
        {
            return false;
        }
        if authority.source != source || authority.agent_label != agent_label {
            return false;
        }
        authority
            .session_ref
            .as_ref()
            .zip(session_ref.as_ref())
            .is_some_and(|(current, incoming)| current != incoming)
    }

    fn same_owner_full_lifecycle_hook_authority_session_ref(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &crate::agent_resume::AgentSessionRef,
    ) -> Option<crate::agent_resume::AgentSessionRef> {
        let authority = self.hook_authority.as_ref()?;
        if !crate::detect::full_lifecycle_hook_authority(&authority.source, &authority.agent_label)
            || authority.source != source
            || authority.agent_label != agent_label
        {
            return None;
        }
        authority
            .session_ref
            .as_ref()
            .filter(|current| *current != session_ref)
            .cloned()
    }

    fn clear_full_lifecycle_hook_suppression_for_detected_agent(
        &mut self,
        previous_detected_agent: Option<Agent>,
        detected_agent: Option<Agent>,
    ) {
        let Some(detected_agent) = detected_agent else {
            return;
        };
        if previous_detected_agent == Some(detected_agent) {
            return;
        }
        let detected_label = crate::detect::agent_label(detected_agent);
        let mut stale_sessions = Vec::new();
        let mut validated_replacement_sessions = Vec::new();
        self.suppressed_full_lifecycle_hook_reports
            .retain(|source, suppressed| {
                let should_clear = crate::detect::parse_agent_label(&suppressed.agent_label)
                    == Some(detected_agent);
                if !should_clear {
                    return true;
                }
                if suppressed.reason == FullLifecycleHookSuppressionReason::ProcessExit {
                    if let Some(session_ref) = suppressed.replacement_session_ref.take() {
                        if let Some(exited_session_ref) = suppressed
                            .session_ref
                            .as_ref()
                            .filter(|exited_session_ref| *exited_session_ref != &session_ref)
                            .cloned()
                        {
                            stale_sessions.push((
                                source.clone(),
                                StaleFullLifecycleHookSession {
                                    agent_label: suppressed.agent_label.clone(),
                                    session_ref: exited_session_ref,
                                },
                            ));
                        }
                        let session_start_seq = self.hook_report_sequences.get(source).copied();
                        let pending =
                            suppressed
                                .pending_replacement_report
                                .take()
                                .filter(|pending| {
                                    pending.authority.session_ref.as_ref() == Some(&session_ref)
                                        && session_start_seq.is_none_or(|seq| pending.seq > seq)
                                });
                        validated_replacement_sessions.push((
                            source.clone(),
                            suppressed.agent_label.clone(),
                            session_ref,
                            pending,
                        ));
                        return false;
                    }
                    return true;
                }
                if let Some(session_ref) = suppressed.session_ref.clone() {
                    stale_sessions.push((
                        source.clone(),
                        StaleFullLifecycleHookSession {
                            agent_label: suppressed.agent_label.clone(),
                            session_ref,
                        },
                    ));
                }
                false
            });
        for (source, stale_session) in stale_sessions {
            self.remember_stale_full_lifecycle_hook_session(
                source,
                stale_session.agent_label,
                stale_session.session_ref,
            );
        }
        self.hook_report_sequences.retain(|source, _| {
            validated_replacement_sessions
                .iter()
                .any(|(validated_source, _, _, _)| validated_source == source)
                || !crate::detect::full_lifecycle_hook_authority(source, detected_label)
        });
        for (source, agent_label, session_ref, pending) in validated_replacement_sessions {
            self.forget_stale_full_lifecycle_hook_session(&source, &agent_label, &session_ref);
            self.reconcile_agent_name_owner(&agent_label, Some(&session_ref));
            self.persisted_agent_session = Some(crate::agent_resume::PersistedAgentSession {
                source: source.clone(),
                agent: agent_label,
                session_ref,
            });
            if let Some(pending) = pending {
                self.hook_report_sequences.insert(source, pending.seq);
                self.hook_authority = Some(pending.authority);
            }
        }
    }

    fn remember_stale_full_lifecycle_hook_session(
        &mut self,
        source: String,
        agent_label: String,
        session_ref: crate::agent_resume::AgentSessionRef,
    ) {
        let stale_session = StaleFullLifecycleHookSession {
            agent_label,
            session_ref,
        };
        let source_stale_sessions = self
            .stale_full_lifecycle_hook_sessions
            .entry(source)
            .or_default();
        if !source_stale_sessions
            .iter()
            .any(|existing| existing == &stale_session)
        {
            source_stale_sessions.push(stale_session);
        }
    }

    fn forget_stale_full_lifecycle_hook_session(
        &mut self,
        source: &str,
        agent_label: &str,
        session_ref: &crate::agent_resume::AgentSessionRef,
    ) {
        let remove_source = self
            .stale_full_lifecycle_hook_sessions
            .get_mut(source)
            .is_some_and(|stale_sessions| {
                stale_sessions.retain(|stale| {
                    stale.agent_label != agent_label || &stale.session_ref != session_ref
                });
                stale_sessions.is_empty()
            });
        if remove_source {
            self.stale_full_lifecycle_hook_sessions.remove(source);
        }
    }

    fn detected_state_observed_before_release_suppression(
        &self,
        detected_agent: Option<Agent>,
        observed_at: Instant,
    ) -> bool {
        let Some(detected_agent) = detected_agent else {
            return false;
        };
        self.suppressed_full_lifecycle_hook_reports
            .values()
            .any(|suppressed| {
                crate::detect::parse_agent_label(&suppressed.agent_label) == Some(detected_agent)
                    && observed_at <= suppressed.observed_at
            })
    }

    fn current_session_identity_for_persistence(
        &self,
    ) -> Option<(
        String,
        String,
        crate::agent_resume::AgentSessionRefKind,
        String,
    )> {
        if let Some(authority) = self.hook_authority.as_ref() {
            if let Some(session_ref) = authority.session_ref.as_ref() {
                return Some((
                    authority.source.clone(),
                    authority.agent_label.clone(),
                    session_ref.kind,
                    session_ref.value.clone(),
                ));
            }
        }
        self.persisted_agent_session.as_ref().map(|session| {
            (
                session.source.clone(),
                session.agent.clone(),
                session.session_ref.kind,
                session.session_ref.value.clone(),
            )
        })
    }

    fn current_agent_activity_owner(&self) -> Option<AgentActivityOwner> {
        self.current_session_identity_for_persistence()
            .map(
                |(source, agent_label, kind, value)| AgentActivityOwner::Session {
                    source,
                    agent_label,
                    kind,
                    value,
                },
            )
            .or_else(|| {
                self.effective_agent_label()
                    .map(|label| AgentActivityOwner::Agent {
                        agent_label: label.to_string(),
                        previous_session: None,
                    })
            })
    }

    pub(crate) fn agent_session_matches(
        &self,
        source: &str,
        agent_label: &str,
        session_id: &str,
    ) -> bool {
        self.current_session_identity_for_persistence().is_some_and(
            |(current_source, current_agent, current_kind, current_value)| {
                current_source == source
                    && current_agent == agent_label
                    && current_kind == crate::agent_resume::AgentSessionRefKind::Id
                    && current_value == session_id
            },
        )
    }

    fn current_session_owner_conflicts(&self, source: &str, agent_label: &str) -> bool {
        self.current_session_identity_for_persistence().is_some_and(
            |(current_source, current_agent, _, _)| {
                current_source != source || current_agent != agent_label
            },
        )
    }

    fn conflicting_same_owner_session_ref(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &crate::agent_resume::AgentSessionRef,
        session_start_source: Option<&str>,
    ) -> Option<crate::agent_resume::AgentSessionRef> {
        self.current_session_identity_for_persistence().and_then(
            |(current_source, current_agent, current_kind, current_value)| {
                (current_source == source
                    && current_agent == agent_label
                    && current_kind == crate::agent_resume::AgentSessionRefKind::Id
                    && session_ref.kind == crate::agent_resume::AgentSessionRefKind::Id
                    && current_value != session_ref.value
                    && !Self::session_report_allows_session_replacement(
                        source,
                        agent_label,
                        session_start_source,
                    ))
                .then_some(crate::agent_resume::AgentSessionRef {
                    kind: current_kind,
                    value: current_value,
                })
            },
        )
    }

    fn lifecycle_hook_report_replaces_persisted_session(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &crate::agent_resume::AgentSessionRef,
    ) -> bool {
        self.hook_authority.is_none()
            && (source, agent_label) == ("herdr:mastracode", "mastracode")
            && self
                .persisted_agent_session
                .as_ref()
                .is_some_and(|session| {
                    session.source == source
                        && session.agent == agent_label
                        && session.session_ref.kind == crate::agent_resume::AgentSessionRefKind::Id
                        && session_ref.kind == crate::agent_resume::AgentSessionRefKind::Id
                        && session.session_ref.value != session_ref.value
                })
    }

    fn session_report_allows_session_replacement(
        source: &str,
        agent_label: &str,
        session_start_source: Option<&str>,
    ) -> bool {
        matches!(
            (source, agent_label, session_start_source),
            (
                "herdr:claude",
                "claude",
                Some("clear" | "resume" | "compact")
            ) | (
                "herdr:codex",
                "codex",
                Some("startup" | "clear" | "resume" | "compact")
            ) | ("herdr:mastracode", "mastracode", Some("startup"))
                | ("herdr:hermes", "hermes", Some("startup" | "new" | "resume"))
                | ("herdr:opencode", "opencode", Some("new"))
                | ("herdr:pi", "pi", Some("new" | "resume" | "fork"))
                | (
                    "herdr:omp",
                    "omp",
                    Some("startup" | "new" | "resume" | "fork")
                )
                | ("herdr:antigravity_cli", "agy", None)
        )
    }

    fn session_start_source_is_recognized(session_start_source: Option<&str>) -> bool {
        matches!(
            session_start_source,
            Some("startup" | "clear" | "resume" | "compact" | "new" | "fork")
        )
    }

    pub fn set_persisted_agent_session(
        &mut self,
        session: crate::agent_resume::PersistedAgentSession,
    ) {
        self.persisted_agent_session = Some(session);
    }

    pub fn set_agent_session_ref(
        &mut self,
        source: String,
        agent_label: String,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        seq: Option<u64>,
    ) -> Option<TerminalStateMutation> {
        self.set_agent_session_ref_for_session_start(source, agent_label, session_ref, seq, None)
    }

    pub fn set_agent_session_ref_for_session_start(
        &mut self,
        source: String,
        agent_label: String,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        seq: Option<u64>,
        session_start_source: Option<String>,
    ) -> Option<TerminalStateMutation> {
        let session_ref = session_ref?;
        let known_agent = crate::detect::parse_agent_label(&agent_label);
        let process_present = known_agent.is_some()
            && self.detected_agent == known_agent
            && self.recent_agent_process_exit.is_none();
        let full_lifecycle_source =
            crate::detect::full_lifecycle_hook_authority(&source, &agent_label);
        let generation_gated = self
            .suppressed_full_lifecycle_hook_reports
            .get(&source)
            .is_some_and(|suppressed| {
                suppressed.agent_label == agent_label
                    && suppressed.reason != FullLifecycleHookSuppressionReason::HookClear
            });
        let session_anchored = self.hook_authority.as_ref().is_some_and(|authority| {
            authority.source == source
                && authority.agent_label == agent_label
                && authority.session_ref.is_some()
        }) || self.persisted_agent_session_matches(&source, &agent_label);
        if full_lifecycle_source && (!process_present || generation_gated || !session_anchored) {
            if !Self::session_start_source_is_recognized(session_start_source.as_deref()) {
                return None;
            }
            let seq = seq?;
            if self
                .hook_report_sequences
                .get(&source)
                .is_some_and(|previous| seq <= *previous)
            {
                return None;
            }

            let previous_agent_label = self.effective_agent_label().map(str::to_string);
            let previous_known_agent = self.effective_known_agent();
            let previous_state = self.state;
            let now = Instant::now();
            let previous_presentation =
                self.effective_presentation_for_state_at(previous_state, now);
            let previous_session = self.current_session_identity_for_persistence();
            let suppressed = self
                .suppressed_full_lifecycle_hook_reports
                .entry(source.clone())
                .or_insert_with(|| SuppressedFullLifecycleHookReport {
                    agent_label: agent_label.clone(),
                    session_ref: None,
                    observed_at: now,
                    reason: FullLifecycleHookSuppressionReason::ProcessExit,
                    replacement_session_ref: None,
                    pending_replacement_report: None,
                });
            if suppressed.replacement_session_ref.as_ref() != Some(&session_ref) {
                if suppressed
                    .pending_replacement_report
                    .as_ref()
                    .is_some_and(|pending| {
                        pending.authority.session_ref.as_ref() != Some(&session_ref)
                    })
                {
                    suppressed.pending_replacement_report = None;
                }
                suppressed.replacement_session_ref = Some(session_ref);
            }
            self.hook_report_sequences.insert(source.clone(), seq);

            if process_present {
                self.clear_full_lifecycle_hook_suppression_for_detected_agent(None, known_agent);
                let current_session = self.current_session_identity_for_persistence();
                let hook_work_context_changed = if previous_session != current_session {
                    self.clear_hook_work_context()
                } else {
                    false
                };
                return Some(TerminalStateMutation {
                    effective_state_change: self.recompute_effective_state(
                        previous_agent_label,
                        previous_known_agent,
                        previous_state,
                        previous_presentation,
                        now,
                    ),
                    session_ref_changed: previous_session != current_session,
                    session_replaced: previous_session.is_some()
                        && previous_session != current_session,
                    hook_work_context_changed,
                    agent_released: false,
                });
            }
            return None;
        }
        if !self.accept_hook_report(&source, seq) {
            return None;
        }
        if self.known_agent_label_conflicts_with_detected_agent(&agent_label) {
            return None;
        }
        let session_replacement_allowed = Self::session_report_allows_session_replacement(
            &source,
            &agent_label,
            session_start_source.as_deref(),
        );
        let replacing_identity_only_session =
            crate::detect::session_identity_only_integration(&source, &agent_label)
                && session_replacement_allowed
                && self.current_session_identity_for_persistence().is_some_and(
                    |(current_source, current_agent, current_kind, current_value)| {
                        current_source == source
                            && current_agent == agent_label
                            && current_kind == crate::agent_resume::AgentSessionRefKind::Id
                            && session_ref.kind == crate::agent_resume::AgentSessionRefKind::Id
                            && current_value != session_ref.value
                    },
                );
        if replacing_identity_only_session && !process_present {
            return None;
        }
        let owner_conflicts = self.current_session_owner_conflicts(&source, &agent_label);
        let foreground_takeover_allowed = owner_conflicts
            && self.foreground_agent_confirms_different_owner_takeover(
                &source,
                &agent_label,
                &session_ref,
                session_start_source.as_deref(),
            );
        if owner_conflicts && !foreground_takeover_allowed {
            return None;
        }
        if self
            .conflicting_same_owner_session_ref(
                &source,
                &agent_label,
                &session_ref,
                session_start_source.as_deref(),
            )
            .is_some()
        {
            return None;
        }
        let replaced_hook_session = self.same_owner_full_lifecycle_hook_authority_session_ref(
            &source,
            &agent_label,
            &session_ref,
        );
        if replaced_hook_session.is_some() && !session_replacement_allowed {
            return None;
        }

        let now = Instant::now();
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let previous_session = self.current_session_identity_for_persistence();
        if session_replacement_allowed || foreground_takeover_allowed {
            self.forget_stale_full_lifecycle_hook_session(&source, &agent_label, &session_ref);
        }
        if let Some(replaced_hook_session) = replaced_hook_session {
            self.remember_stale_full_lifecycle_hook_session(
                source.clone(),
                agent_label.clone(),
                replaced_hook_session,
            );
            self.hook_authority = None;
        } else if foreground_takeover_allowed {
            self.suppress_current_full_lifecycle_hook_authority(
                FullLifecycleHookSuppressionReason::HookClear,
            );
            self.hook_authority = None;
        }
        self.reconcile_agent_name_owner(&agent_label, Some(&session_ref));
        self.persisted_agent_session = Some(crate::agent_resume::PersistedAgentSession {
            source,
            agent: agent_label,
            session_ref,
        });
        let current_session = self.current_session_identity_for_persistence();
        let session_ref_changed = previous_session != current_session;
        let hook_work_context_changed = if previous_session != current_session {
            self.clear_hook_work_context()
        } else {
            false
        };
        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed,
            session_replaced: previous_session.is_some() && session_ref_changed,
            hook_work_context_changed,
            agent_released: false,
        })
    }

    fn known_agent_label_conflicts_with_detected_agent(&self, agent_label: &str) -> bool {
        let Some(detected_agent) = self.detected_agent else {
            return false;
        };
        crate::detect::parse_agent_label(agent_label)
            .is_some_and(|hook_agent| hook_agent != detected_agent)
    }

    fn foreground_agent_confirms_different_owner_takeover(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &crate::agent_resume::AgentSessionRef,
        session_start_source: Option<&str>,
    ) -> bool {
        Self::session_start_source_is_recognized(session_start_source)
            && self.foreground_agent_confirms_session_owner(source, agent_label, session_ref)
    }

    fn foreground_agent_confirms_hook_authority_takeover(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &Option<crate::agent_resume::AgentSessionRef>,
    ) -> bool {
        session_ref.as_ref().is_some_and(|session_ref| {
            self.foreground_agent_confirms_session_owner(source, agent_label, session_ref)
        })
    }

    fn foreground_agent_confirms_session_owner(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &crate::agent_resume::AgentSessionRef,
    ) -> bool {
        let Some(detected_agent) = self.detected_agent else {
            return false;
        };
        crate::detect::parse_agent_label(agent_label) == Some(detected_agent)
            && crate::agent_resume::plan(source, agent_label, session_ref).is_some()
    }

    fn accept_hook_report(&mut self, source: &str, seq: Option<u64>) -> bool {
        let Some(seq) = seq else {
            return !self.hook_report_sequences.contains_key(source);
        };

        if self
            .hook_report_sequences
            .get(source)
            .is_some_and(|last_seq| seq <= *last_seq)
        {
            return false;
        }

        self.hook_report_sequences.insert(source.to_string(), seq);
        true
    }

    #[cfg(test)]
    pub fn clear_hook_authority(
        &mut self,
        source: Option<&str>,
        seq: Option<u64>,
    ) -> Option<EffectiveStateChange> {
        self.clear_hook_authority_with_mutation(source, seq)
            .and_then(|mutation| mutation.effective_state_change)
    }

    pub fn clear_hook_authority_with_mutation(
        &mut self,
        source: Option<&str>,
        seq: Option<u64>,
    ) -> Option<TerminalStateMutation> {
        let sequence_source = source.map(str::to_string).or_else(|| {
            self.hook_authority
                .as_ref()
                .map(|authority| authority.source.clone())
        });
        let should_clear = self
            .hook_authority
            .as_ref()
            .is_some_and(|authority| source.is_none_or(|source| authority.source == source));
        if !should_clear {
            return None;
        }
        if let Some(source) = sequence_source.as_deref() {
            if !self.accept_hook_report(source, seq) {
                return None;
            }
        }

        let now = Instant::now();
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let previous_session = self.current_session_identity_for_persistence();
        self.suppress_current_full_lifecycle_hook_authority(
            FullLifecycleHookSuppressionReason::HookClear,
        );
        self.hook_authority = None;
        self.supervisor_stale = false;
        self.persisted_agent_session = None;
        let hook_work_context_changed = self.clear_hook_work_context();
        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed: previous_session.is_some(),
            session_replaced: previous_session.is_some(),
            hook_work_context_changed,
            agent_released: false,
        })
    }

    pub fn retire_blocked_full_lifecycle_hook_authority_at(
        &mut self,
        observed_at: Instant,
    ) -> Option<TerminalStateMutation> {
        let should_retire = self.hook_authority.as_ref().is_some_and(|authority| {
            authority.state == AgentState::Blocked
                && authority.retired_at.is_none()
                && authority.reported_at <= observed_at
                && self.hook_authority_is_effective(authority)
                && crate::detect::full_lifecycle_hook_authority(
                    &authority.source,
                    &authority.agent_label,
                )
        });
        if !should_retire {
            return None;
        }

        let now = Instant::now();
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let authority = self.hook_authority.as_mut()?;
        authority.retired_at = Some(observed_at);

        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed: false,
            session_replaced: false,
            hook_work_context_changed: false,
            agent_released: false,
        })
    }

    pub fn release_agent_with_mutation(
        &mut self,
        source: &str,
        agent_label: &str,
        seq: Option<u64>,
    ) -> Option<TerminalStateMutation> {
        if self.hook_authority.as_ref().is_some_and(|authority| {
            authority.agent_label != agent_label || authority.source != source
        }) {
            return None;
        }

        let matches_current_agent = self.effective_agent_label() == Some(agent_label);
        let matches_persisted_session = self.persisted_agent_session_matches(source, agent_label);
        if !matches_current_agent && !matches_persisted_session {
            return None;
        }
        if !self.accept_hook_report(source, seq) {
            return None;
        }
        let preserve_foreign_persisted_session = self
            .persisted_agent_session
            .as_ref()
            .is_some_and(|session| session.source != source || session.agent != agent_label);
        let process_owns_agent =
            crate::detect::parse_agent_label(agent_label).is_some_and(|agent| {
                self.detected_agent == Some(agent) && self.recent_agent_process_exit.is_none()
            });

        let now = Instant::now();
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let previous_session = self.current_session_identity_for_persistence();
        self.suppress_full_lifecycle_hook_report(
            source,
            agent_label,
            FullLifecycleHookSuppressionReason::HookClear,
        );
        if !process_owns_agent {
            self.detected_agent = None;
            self.fallback_state = AgentState::Unknown;
            self.fallback_visible_blocker = false;
            self.fallback_visible_working = false;
            self.fallback_visible_working_observed_at = None;
            self.fallback_working_observed_at = None;
            self.fallback_observed_at = None;
            self.clear_agent_name();
        }
        self.hook_authority = None;
        if !preserve_foreign_persisted_session {
            self.persisted_agent_session = None;
        }
        let current_session = self.current_session_identity_for_persistence();
        let hook_work_context_changed = self.clear_hook_work_context();
        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed: previous_session != current_session,
            session_replaced: previous_session.is_some() && previous_session != current_session,
            hook_work_context_changed,
            agent_released: !process_owns_agent,
        })
    }

    fn hook_authority_is_effective(&self, authority: &HookAuthority) -> bool {
        !crate::detect::full_lifecycle_hook_authority(&authority.source, &authority.agent_label)
            || crate::detect::parse_agent_label(&authority.agent_label).is_none_or(|agent| {
                self.detected_agent == Some(agent) && self.recent_agent_process_exit.is_none()
            })
    }

    pub fn effective_agent_label(&self) -> Option<&str> {
        self.hook_authority
            .as_ref()
            .filter(|authority| self.hook_authority_is_effective(authority))
            .map(|authority| authority.agent_label.as_str())
            .or_else(|| {
                self.recent_agent_process_exit
                    .is_none()
                    .then(|| self.detected_agent.map(crate::detect::agent_label))
                    .flatten()
            })
    }

    pub fn effective_known_agent(&self) -> Option<Agent> {
        self.effective_agent_label()
            .and_then(crate::detect::parse_agent_label)
    }

    pub(crate) fn agent_lifecycle_context(&self) -> Option<Agent> {
        self.effective_known_agent()
            .or_else(|| self.recent_agent_process_exit.map(|exit| exit.agent))
    }

    /// Authoritative runtime timestamp behind the sidebar activity-age field.
    ///
    /// Working agents expose the start of their current active interval.
    /// Non-working agents expose when their most recent active interval ended.
    pub(crate) fn agent_activity_at(&self) -> Option<Instant> {
        if self.state == AgentState::Working {
            self.agent_active_since
        } else {
            self.agent_last_active_at
        }
    }

    #[cfg(unix)]
    pub(crate) fn agent_activity_handoff_state(
        &self,
        now: Instant,
    ) -> Option<AgentActivityHandoffState> {
        if self.agent_activity_owner.is_none()
            && self.agent_active_since.is_none()
            && self.agent_last_active_at.is_none()
        {
            return None;
        }
        Some(AgentActivityHandoffState {
            state: self.state.into(),
            active_elapsed: self
                .agent_active_since
                .map(|started| now.saturating_duration_since(started)),
            last_active_elapsed: self
                .agent_last_active_at
                .map(|ended| now.saturating_duration_since(ended)),
            owner: self.agent_activity_owner.clone(),
        })
    }

    #[cfg(unix)]
    pub(crate) fn restore_agent_activity_handoff_state(
        &mut self,
        handoff: AgentActivityHandoffState,
        now: Instant,
    ) {
        self.state = handoff.state.into();
        self.fallback_state = self.state;
        self.agent_active_since = handoff
            .active_elapsed
            .and_then(|elapsed| now.checked_sub(elapsed));
        self.agent_last_active_at = handoff
            .last_active_elapsed
            .and_then(|elapsed| now.checked_sub(elapsed));
        self.agent_activity_owner = handoff.owner;
    }

    pub(crate) fn unchanged_effective_state_change_at(&self, now: Instant) -> EffectiveStateChange {
        let agent_label = self.effective_agent_label().map(str::to_string);
        let known_agent = self.effective_known_agent();
        let state = self.state;
        let presentation = self.effective_presentation_for_state_at(state, now);
        EffectiveStateChange {
            previous_agent_label: agent_label.clone(),
            previous_known_agent: known_agent,
            previous_state: state,
            previous_presentation: presentation.clone(),
            agent_label,
            known_agent,
            state,
            presentation,
        }
    }

    pub fn full_lifecycle_hook_authority_active(&self) -> bool {
        self.live_full_lifecycle_hook_authority()
    }

    fn visible_blocker_overrides_hook(&self) -> bool {
        if self.live_full_lifecycle_hook_authority() {
            return false;
        }
        self.fallback_visible_blocker
            && self.fallback_not_older_than_hook()
            && self.hook_authority.as_ref().is_some_and(|authority| {
                authority.state != AgentState::Blocked
                    && crate::detect::parse_agent_label(&authority.agent_label)
                        == self.detected_agent
            })
    }

    fn visible_working_overrides_idle_hook(&self) -> bool {
        let Some(authority) = self.hook_authority.as_ref() else {
            return false;
        };
        authority.retired_at.is_none()
            && self.hook_authority_is_effective(authority)
            && authority.state == AgentState::Idle
            && crate::detect::parse_agent_label(&authority.agent_label) == self.detected_agent
            && self.fallback_visible_working
            && self
                .fallback_visible_working_observed_at
                .is_some_and(|observed_at| {
                    // Reuse the detector's stable-signal interval so one frame
                    // left over from the hook report cannot start a new turn.
                    authority
                        .reported_at
                        .checked_add(crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH)
                        .is_some_and(|settled_at| observed_at >= settled_at)
                })
    }

    fn detected_working_overrides_idle_hook(&self) -> bool {
        let Some(authority) = self.hook_authority.as_ref() else {
            return false;
        };
        authority.retired_at.is_none()
            && self.hook_authority_is_effective(authority)
            && authority.state == AgentState::Idle
            && crate::detect::parse_agent_label(&authority.agent_label) == self.detected_agent
            && self
                .fallback_working_observed_at
                .is_some_and(|observed_at| {
                    // The same settled interval covers output-promoted working
                    // evidence, so a stale frame cannot start a new turn.
                    authority
                        .reported_at
                        .checked_add(crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH)
                        .is_some_and(|settled_at| observed_at >= settled_at)
                })
    }

    fn live_full_lifecycle_hook_authority(&self) -> bool {
        self.hook_authority.as_ref().is_some_and(|authority| {
            authority.retired_at.is_none()
                && self.hook_authority_is_effective(authority)
                && crate::detect::full_lifecycle_hook_authority(
                    &authority.source,
                    &authority.agent_label,
                )
        })
    }

    pub fn set_manual_label(&mut self, label: String) {
        let label = label.trim().to_string();
        self.manual_label = (!label.is_empty()).then_some(label);
    }

    pub fn clear_manual_label(&mut self) {
        self.manual_label = None;
    }

    pub fn set_agent_name(&mut self, name: String) {
        self.agent_name = (!name.is_empty()).then_some(name);
        self.agent_name_owner = self.agent_name.as_ref().and_then(|_| {
            self.hook_authority
                .as_ref()
                .map(|authority| AgentNameOwner {
                    agent_label: authority.agent_label.clone(),
                    session_ref: authority.session_ref.clone(),
                })
                .or_else(|| {
                    self.persisted_agent_session
                        .as_ref()
                        .map(|session| AgentNameOwner {
                            agent_label: session.agent.clone(),
                            session_ref: Some(session.session_ref.clone()),
                        })
                })
                .or_else(|| {
                    self.effective_agent_label()
                        .map(|agent_label| AgentNameOwner {
                            agent_label: agent_label.to_string(),
                            session_ref: None,
                        })
                })
        });
    }

    pub fn begin_managed_agent(
        &mut self,
        name: String,
        kind: Agent,
        now: Instant,
        settle_delay: Duration,
        timeout: Duration,
    ) {
        self.set_agent_name(name);
        self.agent_name_owner = Some(AgentNameOwner {
            agent_label: crate::detect::agent_label(kind).to_string(),
            session_ref: None,
        });
        self.managed_agent = Some(ManagedAgent {
            kind,
            phase: ManagedAgentPhase::Pending {
                ready_after: Some(now.checked_add(settle_delay).unwrap_or(now)),
                deadline: now.checked_add(timeout).unwrap_or(now),
                observed_expected: false,
            },
        });
    }

    pub fn managed_agent_launch_pending(&self) -> bool {
        self.managed_agent
            .is_some_and(|managed| matches!(managed.phase, ManagedAgentPhase::Pending { .. }))
    }

    pub fn managed_agent_interactive_ready(&self) -> bool {
        self.managed_agent
            .is_some_and(|managed| matches!(managed.phase, ManagedAgentPhase::Active))
    }

    pub fn managed_agent_kind(&self) -> Option<Agent> {
        self.managed_agent.map(|managed| managed.kind)
    }

    pub fn next_managed_agent_deadline(&self) -> Option<Instant> {
        let ManagedAgentPhase::Pending {
            ready_after,
            deadline,
            ..
        } = self.managed_agent?.phase
        else {
            return None;
        };
        Some(ready_after.unwrap_or(deadline).min(deadline))
    }

    pub fn reconcile_managed_agent_at(&mut self, now: Instant, process_exited: bool) -> bool {
        let Some(managed) = self.managed_agent else {
            return false;
        };
        let known_agent = self.effective_known_agent();
        let observed_expected = match managed.phase {
            ManagedAgentPhase::Pending {
                observed_expected, ..
            } => observed_expected || known_agent == Some(managed.kind),
            ManagedAgentPhase::Active => false,
        };
        let clear = process_exited
            || known_agent.is_some_and(|agent| agent != managed.kind)
            || matches!(managed.phase, ManagedAgentPhase::Pending { .. })
                && observed_expected
                && known_agent.is_none();
        if clear {
            self.clear_agent_name();
            return true;
        }
        if let ManagedAgentPhase::Pending {
            ready_after,
            deadline,
            observed_expected: previous_observed_expected,
        } = managed.phase
        {
            if now >= deadline {
                self.clear_agent_name();
                return true;
            }
            if ready_after.is_none_or(|ready_after| now >= ready_after) {
                if known_agent == Some(managed.kind)
                    && matches!(self.state, AgentState::Idle | AgentState::Blocked)
                {
                    self.managed_agent = Some(ManagedAgent {
                        kind: managed.kind,
                        phase: ManagedAgentPhase::Active,
                    });
                    return true;
                }
                if ready_after.is_some() {
                    self.managed_agent = Some(ManagedAgent {
                        kind: managed.kind,
                        phase: ManagedAgentPhase::Pending {
                            ready_after: None,
                            deadline,
                            observed_expected,
                        },
                    });
                    return true;
                }
            }
            if observed_expected != previous_observed_expected {
                self.managed_agent = Some(ManagedAgent {
                    kind: managed.kind,
                    phase: ManagedAgentPhase::Pending {
                        ready_after,
                        deadline,
                        observed_expected,
                    },
                });
                return true;
            }
        }
        false
    }

    pub fn restore_managed_agent(&mut self, name: String, kind: Agent) {
        self.set_agent_name(name);
        self.agent_name_owner = Some(AgentNameOwner {
            agent_label: crate::detect::agent_label(kind).to_string(),
            session_ref: None,
        });
        self.managed_agent = Some(ManagedAgent {
            kind,
            phase: ManagedAgentPhase::Active,
        });
    }

    pub fn clear_agent_name(&mut self) {
        self.agent_name = None;
        self.agent_name_owner = None;
        self.managed_agent = None;
    }

    pub fn clear_agent_runtime_identity_after_respawn(&mut self) -> bool {
        let hook_work_context_changed = self.clear_hook_work_context();
        self.detected_agent = None;
        self.fallback_state = AgentState::Unknown;
        self.fallback_visible_blocker = false;
        self.fallback_visible_working = false;
        self.fallback_visible_working_observed_at = None;
        self.fallback_working_observed_at = None;
        self.fallback_observed_at = None;
        self.hook_authority = None;
        self.supervisor_stale = false;
        self.persisted_agent_session = None;
        self.agent_metadata.clear();
        self.metadata_report_agents.clear();
        self.suppressed_full_lifecycle_hook_reports.clear();
        self.stale_full_lifecycle_hook_sessions.clear();
        self.state = AgentState::Unknown;
        self.last_agent_state_change_seq = None;
        self.agent_active_since = None;
        self.agent_last_active_at = None;
        self.agent_activity_owner = None;
        self.launch_argv = None;
        self.respawn_shell_on_exit = false;
        self.recent_agent_process_exit = None;
        self.pending_agent_resume_plan = None;
        self.clear_agent_name();
        hook_work_context_changed
    }

    pub fn is_agent_terminal(&self) -> bool {
        self.agent_name.is_some() || self.effective_agent_label().is_some()
    }

    fn reconcile_agent_name_owner(
        &mut self,
        agent_label: &str,
        session_ref: Option<&crate::agent_resume::AgentSessionRef>,
    ) {
        if self.agent_name.is_none() {
            return;
        }
        if self.managed_agent.is_some_and(|managed| {
            crate::detect::parse_agent_label(agent_label) == Some(managed.kind)
        }) {
            return;
        }
        match self.agent_name_owner.as_mut() {
            Some(owner)
                if owner.agent_label != agent_label
                    || owner
                        .session_ref
                        .as_ref()
                        .zip(session_ref)
                        .is_some_and(|(current, incoming)| current != incoming) =>
            {
                self.agent_name = None;
                self.agent_name_owner = None;
            }
            Some(owner) if owner.session_ref.is_none() && session_ref.is_some() => {
                owner.session_ref = session_ref.cloned();
            }
            None => {
                self.agent_name_owner = Some(AgentNameOwner {
                    agent_label: agent_label.to_string(),
                    session_ref: session_ref.cloned(),
                })
            }
            _ => {}
        }
    }

    pub fn border_label(&self, show_agent_labels: bool) -> Option<String> {
        self.effective_title().or_else(|| {
            self.manual_label.clone().or_else(|| {
                show_agent_labels
                    .then(|| {
                        self.effective_display_agent()
                            .or_else(|| self.effective_agent_label().map(str::to_string))
                    })
                    .flatten()
            })
        })
    }

    fn recompute_effective_state(
        &mut self,
        previous_agent_label: Option<String>,
        previous_known_agent: Option<Agent>,
        previous_state: AgentState,
        previous_presentation: EffectivePresentation,
        now: Instant,
    ) -> Option<EffectiveStateChange> {
        let detected_state = if self.visible_blocker_overrides_hook() {
            AgentState::Blocked
        } else if self.visible_working_overrides_idle_hook()
            || self.detected_working_overrides_idle_hook()
        {
            AgentState::Working
        } else {
            self.hook_authority
                .as_ref()
                .filter(|authority| {
                    authority.retired_at.is_none() && self.hook_authority_is_effective(authority)
                })
                .map(|authority| authority.state)
                .unwrap_or(self.fallback_state)
        };
        let state = if detected_state == AgentState::Idle
            && self.effective_agent_label().is_some()
            && self.foreground_process_active
        {
            AgentState::Working
        } else {
            detected_state
        };
        let agent_label = self.effective_agent_label().map(str::to_string);
        let known_agent = self.effective_known_agent();
        let mut activity_owner = self.current_agent_activity_owner();
        if let Some(owner) = activity_owner.as_mut() {
            owner.inherit_detection_lineage(self.agent_activity_owner.as_ref());
        }
        let activity_owner_changed =
            match (self.agent_activity_owner.as_ref(), activity_owner.as_ref()) {
                (Some(previous), Some(current)) => !current.continues_activity_from(previous),
                (None, None) => false,
                _ => true,
            };

        let presentation = self.effective_presentation_for_state_at(state, now);
        self.clear_expiry_pending_for_hidden_metadata();

        if activity_owner_changed {
            if state == AgentState::Working && activity_owner.is_some() {
                self.agent_active_since = Some(now);
                self.agent_last_active_at = None;
            } else {
                self.agent_active_since = None;
                self.agent_last_active_at = None;
            }
        } else if previous_state != state {
            if state == AgentState::Working {
                self.agent_active_since = Some(now);
            } else if previous_state == AgentState::Working {
                self.agent_active_since = None;
                self.agent_last_active_at = Some(now);
            }
        }
        self.agent_activity_owner = activity_owner;

        if previous_agent_label == agent_label
            && previous_state == state
            && previous_presentation == presentation
        {
            return None;
        }

        self.state = state;
        Some(EffectiveStateChange {
            previous_agent_label,
            previous_known_agent,
            previous_state,
            previous_presentation,
            agent_label,
            known_agent,
            state,
            presentation,
        })
    }
}

pub(crate) fn stabilize_agent_detection(detection: crate::detect::AgentDetection) -> AgentState {
    detection.state
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::AgentDetection;

    fn test_terminal() -> TerminalState {
        TerminalState::new(TerminalId::alloc(), "/tmp".into())
    }

    #[test]
    fn declared_wait_watchdog_uses_deadline_and_fresh_report_clears_stale() {
        let started = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal
            .set_hook_authority_report_at(
                "herdr:claude-closing-block".into(),
                "claude".into(),
                AgentState::Working,
                Some("waiting for CI".into()),
                Some("CI run 4123".into()),
                Some(120),
                Some("2026-08-10T10:00:00Z".into()),
                None,
                Some(1),
                started,
            )
            .expect("declared wait accepted");

        assert_eq!(
            terminal.status_report_snapshot(),
            (
                Some("CI run 4123".into()),
                Some(120),
                Some("2026-08-10T10:00:00Z".into()),
                false,
            )
        );
        assert!(terminal
            .mark_agent_status_stale_at(started + Duration::from_secs(149))
            .is_none());
        assert!(terminal
            .mark_agent_status_stale_at(started + Duration::from_secs(150))
            .is_some());
        assert!(terminal.status_report_snapshot().3);

        terminal
            .set_hook_authority_report_at(
                "herdr:claude-closing-block".into(),
                "claude".into(),
                AgentState::Working,
                None,
                None,
                None,
                Some("2026-08-10T10:03:00Z".into()),
                None,
                Some(2),
                started + Duration::from_secs(151),
            )
            .expect("fresh report accepted");
        assert!(!terminal.status_report_snapshot().3);
        assert_eq!(terminal.status_report_snapshot().0, None);
        assert_eq!(terminal.status_report_snapshot().1, None);
    }

    #[test]
    fn no_declared_wait_watchdog_stales_after_silence_only_while_working() {
        let started = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal
            .set_hook_authority_report_at(
                "herdr:claude-closing-block".into(),
                "claude".into(),
                AgentState::Working,
                None,
                None,
                None,
                Some("2026-08-10T10:00:00Z".into()),
                None,
                Some(1),
                started,
            )
            .expect("working report accepted");
        assert!(terminal
            .mark_agent_status_stale_at(started + AGENT_STALE_SILENCE - Duration::from_secs(1))
            .is_none());
        assert!(terminal
            .mark_agent_status_stale_at(started + AGENT_STALE_SILENCE)
            .is_some());

        let mut waiting = test_terminal();
        waiting.set_detected_state(Some(Agent::Claude), AgentState::Working);
        waiting
            .set_hook_authority_report_at(
                "herdr:claude-closing-block".into(),
                "claude".into(),
                AgentState::Working,
                None,
                Some("CI".into()),
                Some(3600),
                None,
                None,
                Some(1),
                started,
            )
            .expect("waiting report accepted");
        assert!(waiting
            .mark_agent_status_stale_at(started + AGENT_STALE_SILENCE)
            .is_none());
    }

    #[test]
    fn declared_wait_is_ignored_for_non_working_reports() {
        let started = Instant::now();
        let mut terminal = test_terminal();
        terminal
            .set_hook_authority_report_at(
                "herdr:claude-closing-block".into(),
                "claude".into(),
                AgentState::Blocked,
                None,
                Some("not a live wait".into()),
                Some(10),
                None,
                None,
                Some(1),
                started,
            )
            .expect("blocked report accepted");
        assert_eq!(terminal.status_report_snapshot().0, None);
        assert_eq!(terminal.status_report_snapshot().1, None);
        assert!(terminal.agent_status_watchdog_deadline().is_none());
    }

    #[test]
    fn idle_detected_agent_with_foreground_process_is_working() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);

        let mutation = terminal
            .set_foreground_process(Some("codex".into()), true, Instant::now())
            .expect("foreground process changed");

        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            mutation
                .effective_state_change
                .expect("effective state changed")
                .state,
            AgentState::Working
        );
    }

    #[test]
    fn idle_detected_agent_returns_idle_when_foreground_process_clears() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.set_foreground_process(Some("cargo".into()), true, Instant::now());

        terminal.set_foreground_process(None, false, Instant::now());

        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn idle_detected_agent_without_active_child_stays_idle() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);

        terminal.set_foreground_process(Some("claude".into()), false, Instant::now());

        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn foreground_process_does_not_override_non_idle_detection_states() {
        for state in [AgentState::Working, AgentState::Blocked] {
            let mut terminal = test_terminal();
            terminal.set_detected_state(Some(Agent::Claude), state);

            terminal.set_foreground_process(Some("cargo".into()), true, Instant::now());

            assert_eq!(terminal.state, state);
        }
    }

    #[test]
    fn activity_timestamp_tracks_working_interval_without_observation_jitter() {
        let started = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Working,
            false,
            false,
            true,
            false,
            started,
        );
        assert_eq!(terminal.agent_activity_at(), Some(started));

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Working,
            false,
            false,
            true,
            false,
            started + Duration::from_secs(12),
        );
        assert_eq!(terminal.agent_activity_at(), Some(started));

        let finished = started + Duration::from_secs(20);
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            finished,
        );
        assert_eq!(terminal.agent_activity_at(), Some(finished));

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            finished + Duration::from_secs(30),
        );
        assert_eq!(terminal.agent_activity_at(), Some(finished));
    }

    #[test]
    fn review_findings_activity_owner_refinement_preserves_working_interval() {
        let started = Instant::now().checked_sub(Duration::from_secs(30)).unwrap();
        let mut terminal = test_terminal();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Kimi),
            AgentState::Working,
            false,
            false,
            true,
            false,
            started,
        );

        let session_ref = crate::agent_resume::AgentSessionRef::id("kimi-root").unwrap();
        assert!(terminal
            .set_agent_session_ref_for_session_start(
                "herdr:kimi".into(),
                "kimi".into(),
                Some(session_ref.clone()),
                Some(10),
                Some("startup".into()),
            )
            .is_some());
        assert_eq!(terminal.agent_activity_at(), Some(started));

        assert!(terminal
            .set_hook_authority_with_session_ref(
                "herdr:kimi".into(),
                "kimi".into(),
                AgentState::Working,
                None,
                Some(session_ref),
                Some(11),
            )
            .is_some());
        assert_eq!(terminal.agent_activity_at(), Some(started));
    }

    #[test]
    #[cfg(unix)]
    fn review_fix_handoff_preserves_working_activity_age_and_owner() {
        let started = Instant::now().checked_sub(Duration::from_secs(45)).unwrap();
        let captured_at = Instant::now();
        let mut source = test_terminal();
        source.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Working,
            false,
            true,
            false,
            false,
            started,
        );
        let handoff = source
            .agent_activity_handoff_state(captured_at)
            .expect("working activity should transfer");

        let restored_at = captured_at + Duration::from_secs(2);
        let mut restored = test_terminal();
        restored.restore_agent_activity_handoff_state(handoff, restored_at);
        restored.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Working,
            false,
            true,
            false,
            false,
            restored_at + Duration::from_secs(1),
        );

        assert_eq!(
            restored.agent_activity_at(),
            Some(started + Duration::from_secs(2))
        );
        assert_eq!(restored.agent_activity_owner, source.agent_activity_owner);
    }

    #[test]
    fn review_findings_activity_owner_demotion_records_idle_transition() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Kimi), AgentState::Idle);
        let session_ref = crate::agent_resume::AgentSessionRef::id("kimi-root").unwrap();
        assert!(terminal
            .set_agent_session_ref_for_session_start(
                "herdr:kimi".into(),
                "kimi".into(),
                Some(session_ref.clone()),
                Some(10),
                Some("startup".into()),
            )
            .is_some());
        assert!(terminal
            .set_hook_authority_with_session_ref(
                "herdr:kimi".into(),
                "kimi".into(),
                AgentState::Working,
                None,
                Some(session_ref),
                Some(11),
            )
            .is_some());
        let working_started = terminal.agent_activity_at().unwrap();

        assert!(terminal
            .clear_hook_authority_with_mutation(Some("herdr:kimi"), Some(12))
            .is_some());

        assert_eq!(terminal.state, AgentState::Idle);
        assert!(terminal
            .agent_activity_at()
            .is_some_and(|at| at >= working_started));
    }

    #[test]
    fn review_findings_same_label_replacement_after_fallback_resets_activity() {
        let mut terminal = test_terminal();
        let started = Instant::now().checked_sub(Duration::from_secs(30)).unwrap();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Working,
            false,
            false,
            true,
            false,
            started,
        );
        let first_session =
            crate::agent_resume::AgentSessionRef::path(test_session_path("first.jsonl")).unwrap();
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            first_session.clone(),
        );
        terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(first_session),
            Some(20),
        );
        assert_eq!(terminal.agent_activity_at(), Some(started));

        terminal
            .clear_hook_authority_with_mutation(Some("herdr:pi"), Some(21))
            .expect("hook clear should be accepted");
        assert_eq!(terminal.agent_activity_at(), Some(started));

        let replacement_observed = Instant::now();
        terminal
            .set_hook_authority_at(
                "herdr:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                crate::agent_resume::AgentSessionRef::path(test_session_path("second.jsonl")),
                Some(22),
                replacement_observed,
            )
            .expect("replacement hook should be accepted");

        assert_eq!(terminal.agent_activity_at(), Some(replacement_observed));
    }

    #[test]
    fn review_findings_activity_timestamp_resets_for_replacement_session() {
        let mut terminal = test_terminal();
        let started = Instant::now().checked_sub(Duration::from_secs(30)).unwrap();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Hermes),
            AgentState::Working,
            false,
            false,
            true,
            false,
            started,
        );
        terminal.set_agent_session_ref_for_session_start(
            "herdr:hermes".into(),
            "hermes".into(),
            crate::agent_resume::AgentSessionRef::id("first"),
            Some(1),
            Some("startup".into()),
        );
        terminal.agent_active_since = Some(started);

        terminal.set_agent_session_ref_for_session_start(
            "herdr:hermes".into(),
            "hermes".into(),
            crate::agent_resume::AgentSessionRef::id("second"),
            Some(2),
            Some("resume".into()),
        );

        assert!(terminal.agent_activity_at().is_some_and(|at| at > started));
    }

    #[test]
    fn review_findings_activity_timestamp_resets_for_detected_agent_owner() {
        let mut terminal = test_terminal();
        let started = Instant::now();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Working,
            false,
            false,
            true,
            false,
            started,
        );
        let replaced = started + Duration::from_secs(10);

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            true,
            false,
            replaced,
        );

        assert_eq!(terminal.agent_activity_at(), Some(replaced));
    }

    #[test]
    fn review_findings_idle_activity_does_not_cross_session_owner() {
        let mut terminal = test_terminal();
        let started = Instant::now().checked_sub(Duration::from_secs(30)).unwrap();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Hermes),
            AgentState::Working,
            false,
            false,
            true,
            false,
            started,
        );
        terminal.set_agent_session_ref_for_session_start(
            "herdr:hermes".into(),
            "hermes".into(),
            crate::agent_resume::AgentSessionRef::id("first"),
            Some(1),
            Some("startup".into()),
        );
        let finished = Instant::now();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Hermes),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            finished,
        );
        assert_eq!(terminal.agent_activity_at(), Some(finished));

        terminal.set_agent_session_ref_for_session_start(
            "herdr:hermes".into(),
            "hermes".into(),
            crate::agent_resume::AgentSessionRef::id("second"),
            Some(2),
            Some("resume".into()),
        );

        assert_eq!(terminal.agent_activity_at(), None);
    }

    fn test_session_path(name: &str) -> String {
        std::env::current_dir()
            .unwrap()
            .join(name)
            .display()
            .to_string()
    }

    fn anchor_full_lifecycle_session(
        terminal: &mut TerminalState,
        agent: Agent,
        source: &str,
        agent_label: &str,
        session_ref: crate::agent_resume::AgentSessionRef,
    ) {
        terminal.set_detected_state(Some(agent), terminal.fallback_state);
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: source.into(),
            agent: agent_label.into(),
            session_ref,
        });
    }

    #[test]
    fn managed_agent_activates_only_after_matching_settled_detection() {
        let mut terminal = test_terminal();
        let now = Instant::now();
        terminal.begin_managed_agent(
            "reviewer".into(),
            Agent::Pi,
            now,
            Duration::from_millis(100),
            Duration::from_secs(1),
        );
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);

        assert!(terminal.managed_agent_launch_pending());
        assert!(!terminal.managed_agent_interactive_ready());
        assert!(terminal.reconcile_managed_agent_at(now + Duration::from_millis(100), false));
        assert!(!terminal.managed_agent_launch_pending());
        assert!(terminal.managed_agent_interactive_ready());
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));

        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);
        assert!(terminal.managed_agent_interactive_ready());

        terminal.set_detected_state(None, AgentState::Unknown);
        assert!(terminal.managed_agent_interactive_ready());
        assert!(!terminal.reconcile_managed_agent_at(now + Duration::from_millis(101), false));
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));
        assert!(terminal.reconcile_managed_agent_at(now + Duration::from_millis(102), true));
        assert_eq!(terminal.agent_name, None);
    }

    #[test]
    fn managed_agent_mismatch_and_timeout_release_name() {
        let now = Instant::now();
        let mut mismatch = test_terminal();
        mismatch.begin_managed_agent(
            "reviewer".into(),
            Agent::Pi,
            now,
            Duration::ZERO,
            Duration::from_secs(1),
        );
        mismatch.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        assert!(mismatch.reconcile_managed_agent_at(now, false));
        assert_eq!(mismatch.agent_name, None);
        assert_eq!(mismatch.managed_agent_kind(), None);

        let mut timed_out = test_terminal();
        timed_out.begin_managed_agent(
            "reviewer".into(),
            Agent::Pi,
            now,
            Duration::from_millis(10),
            Duration::from_millis(20),
        );
        assert!(timed_out.reconcile_managed_agent_at(now + Duration::from_millis(20), false));
        assert_eq!(timed_out.agent_name, None);
        assert_eq!(timed_out.managed_agent_kind(), None);
    }

    #[test]
    fn stabilization_uses_raw_policy_state() {
        let detection = AgentDetection {
            state: AgentState::Idle,
            skip_state_update: false,
            visible_idle: false,
            visible_blocker: false,
            visible_working: false,
        };

        assert_eq!(stabilize_agent_detection(detection), AgentState::Idle);
    }

    #[test]
    fn hook_authority_overrides_fallback_for_same_agent() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(test_session_path("root.jsonl")).unwrap(),
        );
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
        );

        assert_eq!(terminal.detected_agent, Some(Agent::Pi));
        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.effective_agent_label(), Some("pi"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn hook_authority_can_override_with_unknown_agent_label() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:custom".into(),
            "custom-agent".into(),
            AgentState::Working,
            None,
            None,
        );

        assert_eq!(terminal.detected_agent, Some(Agent::Pi));
        assert_eq!(terminal.effective_agent_label(), Some("custom-agent"));
        assert_eq!(terminal.effective_known_agent(), None);
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn omp_hook_authority_overrides_detected_fallback() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Omp), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Omp,
            "herdr:omp",
            "omp",
            crate::agent_resume::AgentSessionRef::id("omp-root").unwrap(),
        );
        terminal.set_hook_authority(
            "herdr:omp".into(),
            "omp".into(),
            AgentState::Working,
            None,
            None,
        );

        assert_eq!(terminal.detected_agent, Some(Agent::Omp));
        assert_eq!(terminal.effective_agent_label(), Some("omp"));
        assert_eq!(terminal.effective_known_agent(), Some(Agent::Omp));
        assert_eq!(terminal.state, AgentState::Working);

        let change = terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Omp),
            AgentState::Blocked,
            true,
            false,
            false,
        );

        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Working);
        assert!(change.is_none());
    }

    #[test]
    fn session_only_report_does_not_create_hook_authority() {
        for (agent, source, label, session_id) in [
            (Agent::Codex, "herdr:codex", "codex", "codex-session"),
            (Agent::Devin, "herdr:devin", "devin", "devin-session"),
        ] {
            let mut terminal = test_terminal();
            terminal.set_detected_state(Some(agent), AgentState::Idle);

            let mutation = terminal.set_agent_session_ref(
                source.into(),
                label.into(),
                crate::agent_resume::AgentSessionRef::id(session_id),
                Some(1),
            );

            assert!(mutation.is_some());
            assert!(terminal.hook_authority.is_none());
            assert!(!terminal.full_lifecycle_hook_authority_active());
            assert_eq!(terminal.state, AgentState::Idle);

            terminal.set_detected_state_with_screen_signals_at(
                Some(agent),
                AgentState::Working,
                false,
                false,
                false,
                false,
                Instant::now(),
            );

            assert_eq!(terminal.state, AgentState::Working);
        }
    }

    #[test]
    fn startup_session_claim_activates_full_lifecycle_integrations() {
        for (agent, source, label) in [
            (Agent::Kimi, "herdr:kimi", "kimi"),
            (Agent::Kilo, "herdr:kilo", "kilo"),
        ] {
            let mut terminal = test_terminal();
            terminal.set_detected_state(Some(agent), AgentState::Idle);
            let session_ref = crate::agent_resume::AgentSessionRef::id(format!("{label}-root"));

            let session = terminal.set_agent_session_ref_for_session_start(
                source.into(),
                label.into(),
                session_ref.clone(),
                Some(10),
                Some("startup".into()),
            );
            let working = terminal.set_hook_authority_with_session_ref(
                source.into(),
                label.into(),
                AgentState::Working,
                None,
                session_ref,
                Some(11),
            );

            assert!(
                session.is_some(),
                "{label} should accept its startup session"
            );
            assert!(
                working.is_some(),
                "{label} should accept state after startup"
            );
            assert_eq!(terminal.state, AgentState::Working);
        }
    }

    #[test]
    fn session_identity_claims_leave_state_to_detection() {
        for (source, label, agent, start_source, replacement_source) in [
            (
                "herdr:hermes",
                "hermes",
                Agent::Hermes,
                Some("startup"),
                Some("resume"),
            ),
            (
                "herdr:antigravity_cli",
                "agy",
                Agent::Antigravity,
                None,
                None,
            ),
        ] {
            let mut terminal = test_terminal();
            terminal.set_detected_state(Some(agent), AgentState::Idle);
            let first_ref =
                crate::agent_resume::AgentSessionRef::id(format!("{label}-root")).unwrap();
            let first = terminal.set_agent_session_ref_for_session_start(
                source.into(),
                label.into(),
                Some(first_ref.clone()),
                Some(10),
                start_source.map(str::to_string),
            );

            assert!(first.is_some(), "{label} should accept its session");
            assert!(terminal.hook_authority.is_none());
            assert_eq!(terminal.state, AgentState::Idle);
            assert_eq!(
                terminal
                    .persisted_agent_session
                    .as_ref()
                    .map(|session| &session.session_ref),
                Some(&first_ref)
            );

            terminal.set_detected_state(Some(agent), AgentState::Working);
            let replacement_ref =
                crate::agent_resume::AgentSessionRef::id(format!("{label}-replacement")).unwrap();
            let replacement = terminal.set_agent_session_ref_for_session_start(
                source.into(),
                label.into(),
                Some(replacement_ref.clone()),
                Some(11),
                start_source.map(str::to_string),
            );

            assert!(
                replacement.is_some_and(|mutation| mutation.session_ref_changed),
                "{label} should replace its detected session"
            );
            assert!(terminal.hook_authority.is_none());
            assert_eq!(terminal.state, AgentState::Working);
            assert_eq!(
                terminal
                    .persisted_agent_session
                    .as_ref()
                    .map(|session| &session.session_ref),
                Some(&replacement_ref)
            );

            let legacy_state = terminal.set_hook_authority_with_session_ref(
                source.into(),
                label.into(),
                AgentState::Blocked,
                None,
                Some(replacement_ref.clone()),
                Some(12),
            );
            assert!(legacy_state.is_none());
            assert!(terminal.hook_authority.is_none());
            assert_eq!(terminal.state, AgentState::Working);

            terminal.set_detected_state(None, AgentState::Unknown);
            let background_ref =
                crate::agent_resume::AgentSessionRef::id(format!("{label}-background")).unwrap();
            let background_replacement = terminal.set_agent_session_ref_for_session_start(
                source.into(),
                label.into(),
                Some(background_ref.clone()),
                Some(13),
                replacement_source.map(str::to_string),
            );
            assert!(
                background_replacement.is_none(),
                "{label} should reject a background replacement"
            );
            assert_eq!(
                terminal
                    .persisted_agent_session
                    .as_ref()
                    .map(|session| &session.session_ref),
                Some(&replacement_ref)
            );

            terminal.set_detected_state(Some(agent), AgentState::Idle);
            let retried_replacement = terminal.set_agent_session_ref_for_session_start(
                source.into(),
                label.into(),
                Some(background_ref.clone()),
                Some(14),
                replacement_source.map(str::to_string),
            );
            assert!(
                retried_replacement.is_some_and(|mutation| mutation.session_ref_changed),
                "{label} should replace the session once detected"
            );
            assert_eq!(
                terminal
                    .persisted_agent_session
                    .as_ref()
                    .map(|session| &session.session_ref),
                Some(&background_ref)
            );
        }
    }

    #[test]
    fn pi_session_replacement_reports_reanchor_full_lifecycle_authority() {
        for reason in ["new", "resume", "fork"] {
            let mut terminal = test_terminal();
            let old_session = test_session_path(&format!("pi-{reason}-old.jsonl"));
            let new_session = test_session_path(&format!("pi-{reason}-new.jsonl"));
            terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
            terminal.set_hook_authority_with_session_ref(
                "herdr:pi".into(),
                "pi".into(),
                AgentState::Idle,
                None,
                crate::agent_resume::AgentSessionRef::path(old_session),
                Some(10),
            );

            let session_report = terminal.set_agent_session_ref_for_session_start(
                "herdr:pi".into(),
                "pi".into(),
                crate::agent_resume::AgentSessionRef::path(new_session.clone()),
                Some(11),
                Some(reason.into()),
            );

            assert!(
                session_report.is_some(),
                "{reason} should replace the previous Pi session"
            );
            assert!(terminal.hook_authority.is_none());

            let working = terminal.set_hook_authority_with_session_ref(
                "herdr:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                crate::agent_resume::AgentSessionRef::path(new_session.clone()),
                Some(12),
            );

            assert!(
                working.is_some(),
                "{reason} should accept working for the replacement session"
            );
            assert_eq!(terminal.state, AgentState::Working);
            assert_eq!(
                terminal.hook_authority.as_ref().unwrap().session_ref,
                crate::agent_resume::AgentSessionRef::path(new_session)
            );
        }
    }

    #[test]
    fn pi_resume_reactivates_a_previously_stale_session() {
        let mut terminal = test_terminal();
        let session_a = test_session_path("pi-session-a.jsonl");
        let session_b = test_session_path("pi-session-b.jsonl");
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            crate::agent_resume::AgentSessionRef::path(session_a.clone()),
            Some(10),
        );

        terminal.set_agent_session_ref_for_session_start(
            "herdr:pi".into(),
            "pi".into(),
            crate::agent_resume::AgentSessionRef::path(session_b.clone()),
            Some(11),
            Some("new".into()),
        );
        terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            crate::agent_resume::AgentSessionRef::path(session_b.clone()),
            Some(12),
        );

        let resumed = terminal.set_agent_session_ref_for_session_start(
            "herdr:pi".into(),
            "pi".into(),
            crate::agent_resume::AgentSessionRef::path(session_a.clone()),
            Some(13),
            Some("resume".into()),
        );
        let working = terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(session_a.clone()),
            Some(14),
        );

        assert!(resumed.is_some());
        assert!(working.is_some());
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            terminal.hook_authority.as_ref().unwrap().session_ref,
            crate::agent_resume::AgentSessionRef::path(session_a)
        );

        let late_session_b = terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            crate::agent_resume::AgentSessionRef::path(session_b),
            Some(15),
        );
        assert!(late_session_b.is_none());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn pi_startup_adopts_persisted_session_without_live_authority() {
        let mut terminal = test_terminal();
        let old_session = test_session_path("pi-startup-old.jsonl");
        let new_session = test_session_path("pi-startup-new.jsonl");
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            session_ref: crate::agent_resume::AgentSessionRef::path(old_session)
                .expect("test session path should be valid"),
        });

        let startup = terminal.set_agent_session_ref_for_session_start(
            "herdr:pi".into(),
            "pi".into(),
            crate::agent_resume::AgentSessionRef::path(new_session.clone()),
            Some(11),
            Some("startup".into()),
        );

        assert!(startup.is_some());
        assert_eq!(
            terminal.current_session_identity_for_persistence(),
            Some((
                "herdr:pi".into(),
                "pi".into(),
                crate::agent_resume::AgentSessionRefKind::Path,
                new_session,
            ))
        );
    }

    #[test]
    fn pi_non_replacement_reports_preserve_full_lifecycle_authority() {
        for reason in [None, Some("reload"), Some("startup")] {
            let mut terminal = test_terminal();
            let old_session = test_session_path("pi-current.jsonl");
            let new_session = test_session_path("pi-unexpected.jsonl");
            terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
            anchor_full_lifecycle_session(
                &mut terminal,
                Agent::Pi,
                "herdr:pi",
                "pi",
                crate::agent_resume::AgentSessionRef::path(old_session.clone()).unwrap(),
            );
            terminal.set_hook_authority_with_session_ref(
                "herdr:pi".into(),
                "pi".into(),
                AgentState::Idle,
                None,
                crate::agent_resume::AgentSessionRef::path(old_session.clone()),
                Some(10),
            );

            let session_report = terminal.set_agent_session_ref_for_session_start(
                "herdr:pi".into(),
                "pi".into(),
                crate::agent_resume::AgentSessionRef::path(new_session.clone()),
                Some(11),
                reason.map(str::to_string),
            );
            let working = terminal.set_hook_authority_with_session_ref(
                "herdr:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                crate::agent_resume::AgentSessionRef::path(new_session),
                Some(12),
            );

            assert!(session_report.is_none());
            assert!(working.is_none());
            assert_eq!(terminal.state, AgentState::Idle);
            assert_eq!(
                terminal.hook_authority.as_ref().unwrap().session_ref,
                crate::agent_resume::AgentSessionRef::path(old_session),
                "{reason:?} must not replace the current Pi session"
            );
        }
    }

    #[test]
    fn omp_resume_session_report_reanchors_full_lifecycle_authority() {
        let mut terminal = test_terminal();
        let old_session = test_session_path("omp-old.jsonl");
        let new_session = test_session_path("omp-new.jsonl");
        terminal.set_detected_state(Some(Agent::Omp), AgentState::Idle);
        terminal.set_hook_authority_with_session_ref(
            "herdr:omp".into(),
            "omp".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(old_session.clone()),
            Some(10),
        );

        let session_report = terminal.set_agent_session_ref_for_session_start(
            "herdr:omp".into(),
            "omp".into(),
            crate::agent_resume::AgentSessionRef::path(new_session.clone()),
            Some(11),
            Some("resume".into()),
        );

        assert!(session_report.is_some());
        assert!(terminal.hook_authority.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .unwrap()
                .session_ref,
            crate::agent_resume::AgentSessionRef::path(new_session.clone()).unwrap()
        );

        let blocked = terminal.set_hook_authority_with_session_ref(
            "herdr:omp".into(),
            "omp".into(),
            AgentState::Blocked,
            Some("waiting".into()),
            crate::agent_resume::AgentSessionRef::path(new_session.clone()),
            Some(12),
        );

        assert!(blocked.is_some());
        assert_eq!(terminal.state, AgentState::Blocked);
        assert_eq!(
            terminal.hook_authority.as_ref().unwrap().session_ref,
            crate::agent_resume::AgentSessionRef::path(new_session)
        );

        let stale = terminal.set_hook_authority_with_session_ref(
            "herdr:omp".into(),
            "omp".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(old_session),
            Some(13),
        );

        assert!(stale.is_none());
        assert_eq!(terminal.state, AgentState::Blocked);
    }

    #[test]
    fn late_full_lifecycle_hook_with_same_session_after_process_exit_does_not_reacquire_authority()
    {
        let now = Instant::now();
        let mut terminal = test_terminal();
        let session_path = test_session_path("pi.jsonl");
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);
        terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path.clone()),
            Some(20),
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            true,
            now + Duration::from_millis(1),
        );
        let late = terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path),
            Some(21),
        );

        assert!(late.is_none());
        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn live_full_lifecycle_hook_rejects_different_session_ref_for_same_source() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(test_session_path("one.jsonl")).unwrap(),
        );
        terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(test_session_path("one.jsonl")),
            Some(20),
        );

        let mutation = terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            crate::agent_resume::AgentSessionRef::path(test_session_path("two.jsonl")),
            Some(21),
        );

        assert!(mutation.is_none());
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            terminal
                .hook_authority
                .as_ref()
                .and_then(|authority| authority.session_ref.as_ref())
                .map(|session_ref| session_ref.value.as_str()),
            Some(test_session_path("one.jsonl").as_str())
        );
    }

    #[test]
    fn fresh_detected_process_keeps_old_session_suppressed_after_process_exit() {
        let mut terminal = test_terminal();
        let old_session = test_session_path("old-process-exit.jsonl");
        let new_session = test_session_path("new-process-exit.jsonl");
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(old_session.clone()),
            Some(1000),
        );
        let process_exit_seen_at = Instant::now() + Duration::from_secs(1);
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            true,
            process_exit_seen_at,
        );

        let fresh_process_seen_at = process_exit_seen_at + Duration::from_millis(1);
        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            fresh_process_seen_at,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            fresh_process_seen_at + Duration::from_millis(1),
        );

        let late_old = terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(old_session),
            Some(500),
        );
        let fresh_new = terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(new_session.clone()),
            Some(501),
        );

        assert!(late_old.is_none());
        assert!(fresh_new.is_none());
        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:pi".into(),
                "pi".into(),
                crate::agent_resume::AgentSessionRef::path(new_session),
                Some(400),
                Some("startup".into()),
            )
            .expect("fresh session should activate the buffered report");
        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.state, AgentState::Working);
    }

    /// The closing-block reporter must outrank the screen scraper.
    ///
    /// When a Claude turn ends, `manifests/claude.toml` `live_prompt_box` sees the
    /// `❯` box and calls the pane idle. That is right about the harness and wrong
    /// about the work: agents may still be running, or a Gate may be waiting on a
    /// human. The `Stop` hook knows both, so its report has to win.
    #[test]
    fn closing_block_authority_outranks_visible_idle_prompt_box() {
        let mut terminal = test_terminal();
        let session_path = test_session_path("closing-block.jsonl");
        let now = Instant::now();

        // A real claude process is in the pane -- the precondition every
        // full-lifecycle source must satisfy.
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);

        // The pane's session identity is already owned by claude's own
        // integration, as it is on any pane running a real agent. The
        // closing-block source must report *alongside* that owner, not fight it.
        terminal.set_agent_session_ref_for_session_start(
            "herdr:claude".into(),
            "claude".into(),
            crate::agent_resume::AgentSessionRef::path(session_path),
            Some(999),
            Some("startup".into()),
        );

        // Exactly what the RPC produces: no session ref. `session_ref_from_report`
        // mints one only for official sources, so a closing-block report always
        // arrives with `None`. An earlier version of this test passed `Some(..)`
        // here and stayed green while the live path silently dropped every report.
        terminal.set_hook_authority_at(
            "herdr:claude-closing-block".into(),
            "claude".into(),
            AgentState::Blocked,
            Some("Gate 1: merge #30".into()),
            None,
            Some(1000),
            now,
        );

        // Screen scraping now reports the idle prompt box. It must not win.
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            now + Duration::from_millis(1),
        );
        assert!(terminal.full_lifecycle_hook_authority_active());
        assert_eq!(
            terminal.state,
            AgentState::Blocked,
            "visible idle prompt box must not clear a reported Gate"
        );

        // And once the process is gone, the stale authority must not outlive it.
        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            true,
            now + Duration::from_millis(2),
        );
        assert_ne!(terminal.state, AgentState::Blocked);
    }

    #[test]
    fn rapid_restart_replays_reports_that_arrive_before_process_evidence() {
        let mut terminal = test_terminal();
        let session_path = test_session_path("reports-before-process-evidence.jsonl");
        let now = Instant::now();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path.clone()),
            Some(1000),
            now,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            true,
            now + Duration::from_millis(1),
        );

        let lower_sequence = terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path.clone()),
            Some(1001),
            now + Duration::from_millis(2),
        );
        let missing_sequence = terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path.clone()),
            None,
            now + Duration::from_millis(3),
        );
        let buffered_working = terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path.clone()),
            Some(2001),
            now + Duration::from_millis(4),
        );
        let startup = terminal.set_agent_session_ref_for_session_start(
            "herdr:pi".into(),
            "pi".into(),
            crate::agent_resume::AgentSessionRef::path(session_path),
            Some(2000),
            Some("startup".into()),
        );
        assert!(startup.is_none());
        assert!(lower_sequence.is_none());
        assert!(missing_sequence.is_none());
        assert!(buffered_working.is_none());
        assert!(!terminal.full_lifecycle_hook_authority_active());

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            now + Duration::from_millis(5),
        );

        assert!(terminal.full_lifecycle_hook_authority_active());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn process_exit_discards_unclaimed_buffered_state_from_that_generation() {
        let mut terminal = test_terminal();
        let old_session = test_session_path("buffered-exit-old.jsonl");
        let shared_session = test_session_path("buffered-exit-shared.jsonl");
        let now = Instant::now();
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(old_session.clone()).unwrap(),
        );
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(old_session),
            Some(1000),
            now,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            true,
            now + Duration::from_millis(1),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            now + Duration::from_millis(2),
        );
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(shared_session.clone()),
            Some(500),
            now + Duration::from_millis(3),
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            true,
            now + Duration::from_millis(4),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            now + Duration::from_millis(5),
        );
        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:pi".into(),
                "pi".into(),
                crate::agent_resume::AgentSessionRef::path(shared_session),
                Some(100),
                Some("startup".into()),
            )
            .expect("new generation session claim");

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn queued_fresh_process_evidence_uses_process_exit_observation_time() {
        let mut terminal = test_terminal();
        let session_path = test_session_path("queued-after-process-exit.jsonl");
        let process_exit_at = Instant::now() - Duration::from_secs(1);
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path.clone()),
            Some(1000),
            process_exit_at - Duration::from_millis(1),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            true,
            process_exit_at,
        );

        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            process_exit_at + Duration::from_millis(1),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            process_exit_at + Duration::from_millis(2),
        );
        let startup = terminal.set_agent_session_ref_for_session_start(
            "herdr:pi".into(),
            "pi".into(),
            crate::agent_resume::AgentSessionRef::path(session_path),
            Some(2000),
            Some("startup".into()),
        );

        assert!(startup.is_some());
    }

    #[test]
    fn different_session_after_process_exit_waits_for_fresh_process_evidence() {
        let mut terminal = test_terminal();
        let old_session = test_session_path("old-before-process-exit.jsonl");
        let new_session = test_session_path("new-after-process-exit.jsonl");
        let now = Instant::now();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(old_session),
            Some(1000),
            now,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            true,
            now + Duration::from_millis(1),
        );

        let early_new = terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(new_session.clone()),
            Some(500),
            now + Duration::from_millis(2),
        );

        assert!(early_new.is_none());
        assert!(terminal.hook_authority.is_none());

        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(3),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(4),
        );
        let fresh_new = terminal.set_agent_session_ref_for_session_start(
            "herdr:pi".into(),
            "pi".into(),
            crate::agent_resume::AgentSessionRef::path(new_session),
            Some(400),
            Some("startup".into()),
        );

        assert!(fresh_new.is_some());
        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn missing_session_after_process_exit_waits_for_fresh_process_evidence() {
        let mut terminal = test_terminal();
        let old_session = test_session_path("old-before-nosession-process-exit.jsonl");
        let now = Instant::now();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(old_session),
            Some(1000),
            now,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            true,
            now + Duration::from_millis(1),
        );

        let early_without_session = terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            Some(500),
            now + Duration::from_millis(2),
        );

        assert!(early_without_session.is_none());
        assert!(terminal.hook_authority.is_none());

        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(3),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(4),
        );
        let fresh_without_session = terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            Some(500),
            now + Duration::from_millis(5),
        );
        assert!(fresh_without_session.is_none());
        assert!(terminal.hook_authority.is_none());

        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:pi".into(),
                "pi".into(),
                crate::agent_resume::AgentSessionRef::path(test_session_path(
                    "fresh-after-nosession-process-exit.jsonl",
                )),
                Some(600),
                Some("startup".into()),
            )
            .expect("fresh root session should claim the process generation");
        let child_update = terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            Some(601),
            now + Duration::from_millis(6),
        );

        assert!(child_update.is_some());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn mastracode_session_start_replaces_current_root_session() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Mastracode), AgentState::Idle);
        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:mastracode".into(),
                "mastracode".into(),
                crate::agent_resume::AgentSessionRef::id("mastracode-old"),
                Some(20),
                Some("startup".into()),
            )
            .expect("initial root session");

        let replacement = terminal.set_agent_session_ref_for_session_start(
            "herdr:mastracode".into(),
            "mastracode".into(),
            crate::agent_resume::AgentSessionRef::id("mastracode-new"),
            Some(21),
            Some("startup".into()),
        );

        assert!(replacement.is_some_and(|mutation| mutation.session_ref_changed));
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("mastracode-new")
        );
    }

    #[test]
    fn omp_reacquires_full_lifecycle_hook_after_process_exit_with_fresh_process_and_session_ref() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Omp), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:omp".into(),
            "omp".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::id("omp-old"),
            Some(1000),
            now,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Omp),
            AgentState::Idle,
            false,
            true,
            false,
            true,
            now + Duration::from_millis(1),
        );

        let stale = terminal.set_hook_authority_with_session_ref(
            "herdr:omp".into(),
            "omp".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::id("omp-old"),
            Some(500),
        );
        assert!(stale.is_none());
        assert!(terminal.hook_authority.is_none());

        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(2),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Omp),
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(3),
        );
        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:omp".into(),
                "omp".into(),
                crate::agent_resume::AgentSessionRef::id("omp-new"),
                Some(400),
                Some("startup".into()),
            )
            .expect("fresh process and session should claim the pane");
        let fresh = terminal.set_hook_authority_with_session_ref(
            "herdr:omp".into(),
            "omp".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::id("omp-new"),
            Some(500),
        );

        assert!(fresh.is_some());
        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn visible_blocker_overrides_non_blocked_hook_for_same_agent() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
        );

        let change = terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Blocked,
            true,
            false,
            false,
        );

        assert_eq!(terminal.fallback_state, AgentState::Blocked);
        assert_eq!(terminal.state, AgentState::Blocked);
        assert_eq!(change.unwrap().previous_state, AgentState::Working);
    }

    #[test]
    fn visible_blocker_does_not_override_full_lifecycle_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(test_session_path("root.jsonl")).unwrap(),
        );
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
        );

        let change = terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Pi),
            AgentState::Blocked,
            true,
            false,
            false,
        );

        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Working);
        assert!(change.is_none());
    }

    #[test]
    fn weak_blocked_fallback_does_not_override_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
        );

        let change = terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Blocked,
            false,
            false,
            false,
        );

        assert_eq!(terminal.fallback_state, AgentState::Blocked);
        assert_eq!(terminal.state, AgentState::Working);
        assert!(change.is_none());
    }

    #[test]
    fn hook_blocked_wins_over_visible_blocker() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Working);
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Blocked,
            None,
            None,
        );

        terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Blocked,
            true,
            false,
            false,
        );

        assert_eq!(terminal.state, AgentState::Blocked);
        assert!(terminal.hook_authority.is_some());
    }

    #[test]
    fn activity_retires_a_blocked_hook_and_returns_to_screen_state() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            observed,
        );
        terminal.set_hook_authority_at(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Blocked,
            None,
            None,
            Some(7),
            observed,
        );
        assert_eq!(terminal.state, AgentState::Blocked);

        let mutation = terminal
            .retire_blocked_full_lifecycle_hook_authority_at(
                observed + std::time::Duration::from_secs(1),
            )
            .expect("activity should retire the blocked report");

        assert_eq!(terminal.state, AgentState::Idle);
        assert!(!terminal.full_lifecycle_hook_authority_active());
        assert!(terminal.hook_authority.is_some());
        assert_eq!(
            mutation
                .effective_state_change
                .expect("retirement changes the effective state")
                .state,
            AgentState::Idle
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            true,
            false,
            observed + std::time::Duration::from_secs(2),
        );
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn a_silent_blocked_hook_remains_authoritative_without_activity() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Blocked,
            None,
            None,
            Some(7),
            observed,
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            true,
            false,
            observed + std::time::Duration::from_secs(30),
        );

        assert_eq!(terminal.state, AgentState::Blocked);
        assert!(terminal.full_lifecycle_hook_authority_active());
        assert_eq!(terminal.hook_authority.as_ref().unwrap().retired_at, None);
    }

    #[test]
    fn a_fresh_hook_report_after_activity_retirement_is_honoured() {
        let observed = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Blocked,
            None,
            None,
            Some(7),
            observed,
        );
        terminal
            .retire_blocked_full_lifecycle_hook_authority_at(
                observed + std::time::Duration::from_secs(1),
            )
            .expect("first report should retire");

        let fresh = terminal.set_hook_authority_at(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Blocked,
            None,
            None,
            Some(8),
            observed + std::time::Duration::from_secs(2),
        );

        assert!(fresh.is_some());
        assert_eq!(terminal.state, AgentState::Blocked);
        assert!(terminal.full_lifecycle_hook_authority_active());
        assert_eq!(terminal.hook_authority.as_ref().unwrap().retired_at, None);
    }

    #[test]
    fn visible_blocker_does_not_override_different_agent_hook() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(None, AgentState::Unknown);
        terminal.set_hook_authority(
            "custom:agent".into(),
            "custom-agent".into(),
            AgentState::Working,
            None,
            None,
        );

        terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Blocked,
            true,
            false,
            false,
        );

        assert_eq!(terminal.effective_agent_label(), Some("custom-agent"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn fallback_idle_does_not_override_hook_working() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal.set_hook_authority_at(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
            None,
            now,
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            now + Duration::from_secs(10),
        );

        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn fallback_idle_does_not_override_full_lifecycle_hook_working() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Working);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::OpenCode,
            "herdr:opencode",
            "opencode",
            crate::agent_resume::AgentSessionRef::id("opencode-root").unwrap(),
        );
        terminal.set_hook_authority_at(
            "herdr:opencode".into(),
            "opencode".into(),
            AgentState::Working,
            None,
            None,
            None,
            now,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::OpenCode),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            now + Duration::from_secs(10),
        );

        assert_eq!(terminal.fallback_state, AgentState::Working);
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn visible_working_does_not_override_hook_idle_for_same_agent() {
        let now = Instant::now();
        let mut terminal = test_terminal();
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

        let change = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            now + Duration::from_millis(1),
        );

        assert_eq!(terminal.fallback_state, AgentState::Working);
        assert_eq!(terminal.state, AgentState::Idle);
        assert!(change.effective_state_change.is_none());
    }

    #[test]
    fn visible_working_does_not_override_full_lifecycle_hook_idle() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Kimi), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Kimi,
            "herdr:kimi",
            "kimi",
            crate::agent_resume::AgentSessionRef::id("kimi-root").unwrap(),
        );
        terminal.set_hook_authority_at(
            "herdr:kimi".into(),
            "kimi".into(),
            AgentState::Idle,
            None,
            None,
            None,
            now,
        );

        let change = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Kimi),
            AgentState::Working,
            false,
            false,
            true,
            false,
            now + Duration::from_millis(1),
        );

        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Idle);
        assert!(change.effective_state_change.is_none());
    }

    #[test]
    fn settled_visible_working_overrides_full_lifecycle_hook_idle() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Idle,
            None,
            None,
            None,
            now,
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            true,
            false,
            now + crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH,
        );

        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn unsettled_visible_working_does_not_override_full_lifecycle_hook_idle() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Idle,
            None,
            None,
            None,
            now,
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            true,
            false,
            now + Duration::from_millis(1),
        );

        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn settled_detected_working_overrides_full_lifecycle_hook_idle() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority_at(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            AgentState::Idle,
            None,
            None,
            None,
            now,
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            false,
            false,
            now + crate::pane::STABLE_VISIBLE_SIGNAL_REFRESH,
        );

        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn detected_working_fallback_is_ignored_under_full_lifecycle_hook_authority() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Kilo), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Kilo,
            "herdr:kilo",
            "kilo",
            crate::agent_resume::AgentSessionRef::id("kilo-root").unwrap(),
        );
        terminal.set_hook_authority_at(
            "herdr:kilo".into(),
            "kilo".into(),
            AgentState::Idle,
            None,
            None,
            None,
            now,
        );

        let change = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Kilo),
            AgentState::Working,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(1),
        );

        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Idle);
        assert!(change.effective_state_change.is_none());
    }

    #[test]
    fn visible_working_does_not_hold_against_newer_claude_hook_idle() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            now,
        );

        let change = terminal.set_hook_authority_at(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            None,
            now + Duration::from_millis(100),
        );

        assert_eq!(terminal.state, AgentState::Idle);
        assert_eq!(
            change
                .unwrap()
                .effective_state_change
                .unwrap()
                .previous_state,
            AgentState::Working
        );
    }

    #[test]
    fn refreshed_visible_working_does_not_override_newer_hook_blocked() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            true,
            false,
            now,
        );
        terminal.set_hook_authority_at(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Blocked,
            None,
            None,
            None,
            now + Duration::from_millis(1201),
        );

        assert_eq!(terminal.state, AgentState::Blocked);

        let change = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            true,
            false,
            now + Duration::from_millis(2000),
        );

        assert_eq!(terminal.fallback_state, AgentState::Working);
        assert_eq!(terminal.state, AgentState::Blocked);
        assert!(change.effective_state_change.is_none());
    }

    #[test]
    fn fallback_idle_does_not_override_other_agent_hook_working() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Working);
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
        );

        let change = terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Idle,
            false,
            true,
            false,
        );

        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Working);
        assert!(change.is_none());
    }

    #[test]
    fn known_hook_authority_does_not_override_different_detected_agent() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Grok), AgentState::Working);
        let change = terminal.set_hook_authority(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
        );

        assert!(change.is_none());
        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, Some(Agent::Grok));
        assert_eq!(terminal.effective_agent_label(), Some("grok"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn detected_agent_clears_conflicting_known_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
        );

        terminal.set_detected_state(Some(Agent::Grok), AgentState::Working);

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, Some(Agent::Grok));
        assert_eq!(terminal.effective_agent_label(), Some("grok"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn border_label_prefers_manual_label_over_agent_label() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);

        assert_eq!(terminal.border_label(false), None);
        assert_eq!(terminal.border_label(true).as_deref(), Some("claude"));

        terminal.set_manual_label(" reviewer ".into());
        assert_eq!(terminal.border_label(false).as_deref(), Some("reviewer"));
        assert_eq!(terminal.border_label(true).as_deref(), Some("reviewer"));

        terminal.set_manual_label("   ".into());
        assert_eq!(terminal.border_label(true).as_deref(), Some("claude"));

        terminal.set_manual_label("reviewer".into());
        terminal.clear_manual_label();
        assert_eq!(terminal.border_label(true).as_deref(), Some("claude"));
    }

    #[test]
    fn hook_authority_survives_unrelated_detected_agent_clear() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:custom".into(),
            "custom-agent".into(),
            AgentState::Working,
            None,
            None,
        );

        terminal.set_detected_state(None, AgentState::Unknown);

        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.detected_agent, None);
        assert_eq!(terminal.effective_agent_label(), Some("custom-agent"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn full_lifecycle_hook_authority_ignores_detected_agent_clear_without_process_exit() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(test_session_path("root.jsonl")).unwrap(),
        );
        terminal.set_hook_authority_at(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            None,
            now,
        );

        let change = terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(1),
        );

        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.detected_agent, Some(Agent::Pi));
        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Working);
        assert!(change.effective_state_change.is_none());
    }

    #[test]
    fn detected_agent_clear_clears_matching_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Cursor), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:cursor".into(),
            "cursor".into(),
            AgentState::Idle,
            None,
            None,
        );

        terminal.set_detected_state(None, AgentState::Unknown);

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, None);
        assert_eq!(terminal.fallback_state, AgentState::Unknown);
        assert_eq!(terminal.effective_agent_label(), None);
        assert_eq!(terminal.state, AgentState::Unknown);
    }

    #[test]
    fn detected_agent_clear_clears_matching_working_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Working);
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
        );

        terminal.set_detected_state(None, AgentState::Unknown);

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, None);
        assert_eq!(terminal.effective_agent_label(), None);
        assert_eq!(terminal.state, AgentState::Unknown);
    }

    #[test]
    fn process_exit_clears_matching_hook_authority_before_reporting_idle() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Working);
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
        );

        terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Idle,
            false,
            false,
            true,
        );

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, Some(Agent::Codex));
        assert_eq!(terminal.effective_agent_label(), None);
        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn stale_visible_screen_signal_does_not_override_newer_hook_authority() {
        let mut terminal = test_terminal();
        let observed = Instant::now();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            observed,
        );
        terminal.set_hook_authority_at(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
            Some(1),
            observed + Duration::from_secs(1),
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            observed,
        );

        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn stale_process_exit_preserves_newer_custom_authority() {
        let mut terminal = test_terminal();
        let observed = Instant::now();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            observed,
        );
        terminal.set_hook_authority_at(
            "custom:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            Some(100),
            observed + Duration::from_secs(1),
        );

        let mutation = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            false,
            true,
            observed,
        );

        assert!(!mutation.agent_released);
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            terminal
                .hook_authority
                .as_ref()
                .map(|hook| hook.source.as_str()),
            Some("custom:pi")
        );
    }

    #[test]
    fn custom_authority_reanchors_sequence_after_process_restart() {
        let mut terminal = test_terminal();
        let observed = Instant::now();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_at(
            "custom:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            Some(100),
            observed,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            false,
            true,
            observed + Duration::from_millis(1),
        );
        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            observed + Duration::from_millis(2),
        );

        assert!(terminal
            .release_agent_with_mutation("custom:pi", "pi", Some(200))
            .is_none());
        assert!(terminal
            .clear_hook_authority_with_mutation(Some("custom:pi"), Some(201))
            .is_none());
        assert!(terminal
            .set_hook_authority(
                "custom:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                Some(1),
            )
            .is_none());
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            observed + Duration::from_millis(3),
        );
        assert!(terminal
            .set_hook_authority(
                "custom:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                Some(1),
            )
            .is_some());
    }

    #[test]
    fn process_exit_clears_newer_same_agent_hook_authority() {
        let mut terminal = test_terminal();
        let observed = Instant::now();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            false,
            false,
            observed,
        );
        terminal.set_hook_authority_at(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
            Some(1),
            observed,
        );
        terminal.set_hook_authority_at(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
            Some(2),
            observed + Duration::from_secs(1),
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Idle,
            false,
            false,
            false,
            true,
            observed,
        );

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.state, AgentState::Idle);
        assert_eq!(terminal.effective_agent_label(), None);
    }

    #[test]
    fn detected_agent_change_clears_previous_matching_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Idle,
            None,
            None,
        );

        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Working);

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, Some(Agent::OpenCode));
        assert_eq!(terminal.effective_agent_label(), Some("opencode"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn stale_hook_report_sequence_is_ignored_for_same_source() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(test_session_path("root.jsonl")).unwrap(),
        );
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );

        let change = terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            Some(19),
        );

        assert!(change.is_none());
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            terminal.hook_authority.as_ref().unwrap().state,
            AgentState::Working
        );
    }

    #[test]
    fn accepted_hook_report_stores_session_ref() {
        let mut terminal = test_terminal();
        let session_path = test_session_path("pi.jsonl");
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(session_path.clone()).unwrap(),
        );
        let mutation = terminal
            .set_hook_authority_with_session_ref(
                "herdr:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                crate::agent_resume::AgentSessionRef::path(session_path.clone()),
                Some(20),
            )
            .expect("accepted report");

        assert!(!mutation.session_ref_changed);
        assert_eq!(
            terminal
                .hook_authority
                .as_ref()
                .and_then(|authority| authority.session_ref.as_ref())
                .map(|session_ref| (&session_ref.kind, session_ref.value.as_str())),
            Some((
                &crate::agent_resume::AgentSessionRefKind::Path,
                session_path.as_str()
            ))
        );
    }

    #[test]
    fn stale_hook_report_cannot_overwrite_session_ref() {
        let mut terminal = test_terminal();
        let session_path = test_session_path("pi.jsonl");
        let new_session_path = test_session_path("new.jsonl");
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(session_path.clone()).unwrap(),
        );
        terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path.clone()),
            Some(20),
        );

        let mutation = terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(new_session_path),
            Some(19),
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal
                .hook_authority
                .as_ref()
                .and_then(|authority| authority.session_ref.as_ref())
                .map(|session_ref| session_ref.value.as_str()),
            Some(session_path.as_str())
        );
    }

    #[test]
    fn accepted_hook_report_without_session_ref_clears_previous_ref() {
        let mut terminal = test_terminal();
        let session_path = test_session_path("pi.jsonl");
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(session_path.clone()).unwrap(),
        );
        terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path),
            Some(20),
        );

        let mutation = terminal
            .set_hook_authority_with_session_ref(
                "herdr:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                Some(21),
            )
            .expect("accepted report");

        assert!(mutation.session_ref_changed);
        assert!(mutation.effective_state_change.is_none());
        assert!(terminal
            .hook_authority
            .as_ref()
            .unwrap()
            .session_ref
            .is_none());
    }

    #[test]
    fn different_same_agent_session_ref_is_ignored_until_current_session_clears() {
        let mut terminal = test_terminal();
        terminal
            .set_agent_session_ref(
                "herdr:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-session"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let mutation = terminal.set_agent_session_ref(
            "herdr:claude".into(),
            "claude".into(),
            crate::agent_resume::AgentSessionRef::id("nested-session"),
            Some(21),
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal.hook_report_sequences.get("herdr:claude"),
            Some(&21)
        );
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("claude-session")
        );
    }

    #[test]
    fn claude_startup_session_ref_does_not_replace_existing_session_ref() {
        let mut terminal = test_terminal();
        terminal
            .set_agent_session_ref(
                "herdr:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-session"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let mutation = terminal.set_agent_session_ref_for_session_start(
            "herdr:claude".into(),
            "claude".into(),
            crate::agent_resume::AgentSessionRef::id("nested-session"),
            Some(21),
            Some("startup".into()),
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("claude-session")
        );
    }

    #[test]
    fn claude_lifecycle_session_ref_replaces_existing_session_ref() {
        for session_start_source in ["clear", "resume", "compact"] {
            let mut terminal = test_terminal();
            terminal
                .set_agent_session_ref(
                    "herdr:claude".into(),
                    "claude".into(),
                    crate::agent_resume::AgentSessionRef::id("claude-session"),
                    Some(20),
                )
                .expect("initial session should be accepted");

            let next_session = format!("{session_start_source}-session");
            let mutation = terminal
                .set_agent_session_ref_for_session_start(
                    "herdr:claude".into(),
                    "claude".into(),
                    crate::agent_resume::AgentSessionRef::id(&next_session),
                    Some(21),
                    Some(session_start_source.into()),
                )
                .unwrap_or_else(|| panic!("{session_start_source} should replace the session"));

            assert!(
                mutation.session_ref_changed,
                "{session_start_source} should mark the session changed"
            );
            assert_eq!(
                terminal
                    .persisted_agent_session
                    .as_ref()
                    .map(|session| session.session_ref.value.as_str()),
                Some(next_session.as_str()),
                "{session_start_source} should store the replacement session"
            );
        }
    }

    #[test]
    fn codex_lifecycle_session_ref_replaces_existing_session_ref() {
        for session_start_source in ["startup", "clear", "resume", "compact"] {
            let mut terminal = test_terminal();
            terminal
                .set_agent_session_ref(
                    "herdr:codex".into(),
                    "codex".into(),
                    crate::agent_resume::AgentSessionRef::id("codex-session"),
                    Some(20),
                )
                .expect("initial session should be accepted");

            let next_session = format!("codex-{session_start_source}-session");
            let mutation = terminal
                .set_agent_session_ref_for_session_start(
                    "herdr:codex".into(),
                    "codex".into(),
                    crate::agent_resume::AgentSessionRef::id(&next_session),
                    Some(21),
                    Some(session_start_source.into()),
                )
                .unwrap_or_else(|| panic!("{session_start_source} should replace the session"));

            assert!(mutation.session_ref_changed);
            assert_eq!(
                terminal
                    .persisted_agent_session
                    .as_ref()
                    .map(|session| session.session_ref.value.as_str()),
                Some(next_session.as_str())
            );
        }
    }

    #[test]
    fn agent_session_replacement_preserves_unguarded_user_metadata() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal
            .set_agent_session_ref(
                "herdr:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-old"),
                Some(1),
            )
            .expect("initial session should be accepted");
        for (source, title) in [("user:title", "User title"), ("user:jj", "User note")] {
            terminal
                .set_agent_metadata(AgentMetadataReport {
                    source: source.into(),
                    agent_label: None,
                    applies_to_source: None,
                    title: Some(title.into()),
                    display_agent: None,
                    state_labels: std::collections::HashMap::new(),
                    clear_title: false,
                    clear_display_agent: false,
                    clear_state_labels: false,
                    ttl: None,
                    seq: None,
                })
                .expect("user metadata should be accepted");
        }

        let mutation = terminal
            .set_agent_session_ref_for_session_start(
                "herdr:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-new"),
                Some(2),
                Some("clear".into()),
            )
            .expect("replacement session should be accepted");

        assert!(mutation.session_ref_changed);
        assert_eq!(terminal.agent_metadata.len(), 2);
        assert_eq!(
            terminal.agent_metadata["user:jj"].title.as_deref(),
            Some("User note")
        );
        assert!(terminal.agent_metadata.contains_key("user:title"));
        assert!(terminal.agent_metadata.contains_key("user:jj"));
    }

    #[test]
    fn agent_session_replacement_preserves_surviving_metadata_watermarks() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal
            .set_agent_session_ref(
                "herdr:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-old"),
                Some(1),
            )
            .expect("initial session should be accepted");
        terminal
            .set_agent_metadata(AgentMetadataReport {
                source: "user:jj".into(),
                agent_label: None,
                applies_to_source: None,
                title: Some("Keep me".into()),
                display_agent: None,
                state_labels: std::collections::HashMap::new(),
                clear_title: false,
                clear_display_agent: false,
                clear_state_labels: false,
                ttl: None,
                seq: Some(5),
            })
            .expect("user metadata should be accepted");

        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-new"),
                Some(2),
                Some("clear".into()),
            )
            .expect("replacement session should be accepted");

        assert!(!terminal.metadata_report_sequence_is_fresh("user:jj", Some(1)));
        assert!(terminal
            .set_agent_metadata(AgentMetadataReport {
                source: "user:jj".into(),
                agent_label: Some("claude".into()),
                applies_to_source: Some("herdr:claude".into()),
                title: Some("Stale report".into()),
                display_agent: None,
                state_labels: std::collections::HashMap::new(),
                clear_title: false,
                clear_display_agent: false,
                clear_state_labels: false,
                ttl: None,
                seq: Some(1),
            })
            .is_none());
        assert_eq!(
            terminal.agent_metadata["user:jj"].title.as_deref(),
            Some("Keep me")
        );
    }

    #[test]
    fn opencode_new_session_ref_replaces_existing_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:opencode".into(),
                "opencode".into(),
                crate::agent_resume::AgentSessionRef::id("opencode-old"),
                Some(20),
                Some("new".into()),
            )
            .expect("initial session should be accepted");

        let mutation = terminal
            .set_agent_session_ref_for_session_start(
                "herdr:opencode".into(),
                "opencode".into(),
                crate::agent_resume::AgentSessionRef::id("opencode-new"),
                Some(21),
                Some("new".into()),
            )
            .expect("new should replace the session");

        assert!(mutation.session_ref_changed);
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("opencode-new")
        );
    }

    #[test]
    fn pi_session_replacement_clears_the_previous_sessions_alias() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:pi".into(),
                "pi".into(),
                crate::agent_resume::AgentSessionRef::id("pi-old"),
                Some(20),
                Some("new".into()),
            )
            .expect("initial session should be accepted");
        terminal.set_agent_name("reviewer".into());

        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:pi".into(),
                "pi".into(),
                crate::agent_resume::AgentSessionRef::id("pi-new"),
                Some(21),
                Some("new".into()),
            )
            .expect("new should replace the session");

        assert!(terminal.agent_name.is_none());
    }

    #[test]
    fn managed_agent_name_survives_native_session_replacement() {
        let mut terminal = test_terminal();
        let now = Instant::now();
        terminal.begin_managed_agent(
            "reviewer".into(),
            Agent::OpenCode,
            now,
            Duration::ZERO,
            Duration::from_secs(1),
        );
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        assert!(terminal.reconcile_managed_agent_at(now, false));

        for (sequence, session) in [(20, "opencode-old"), (21, "opencode-new")] {
            terminal
                .set_agent_session_ref_for_session_start(
                    "herdr:opencode".into(),
                    "opencode".into(),
                    crate::agent_resume::AgentSessionRef::id(session),
                    Some(sequence),
                    Some("new".into()),
                )
                .expect("managed session should be accepted");
        }

        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));
        assert!(terminal.managed_agent_interactive_ready());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("opencode-new")
        );
    }

    #[test]
    fn opencode_session_ref_without_start_source_does_not_replace_existing() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:opencode".into(),
                "opencode".into(),
                crate::agent_resume::AgentSessionRef::id("opencode-old"),
                Some(20),
                Some("new".into()),
            )
            .expect("initial session should be accepted");

        // session.updated reports carry no session_start_source, so a different
        // id must not displace the established session (cross-talk guard).
        let mutation = terminal.set_agent_session_ref_for_session_start(
            "herdr:opencode".into(),
            "opencode".into(),
            crate::agent_resume::AgentSessionRef::id("opencode-other"),
            Some(21),
            None,
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("opencode-old")
        );
    }

    #[test]
    fn different_owner_session_ref_does_not_replace_existing_session_ref() {
        let mut terminal = test_terminal();
        terminal
            .set_agent_session_ref(
                "herdr:droid".into(),
                "droid".into(),
                crate::agent_resume::AgentSessionRef::id("droid-session"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let mutation = terminal.set_agent_session_ref_for_session_start(
            "herdr:claude".into(),
            "claude".into(),
            crate::agent_resume::AgentSessionRef::id("claude-session"),
            Some(21),
            Some("resume".into()),
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal.persisted_agent_session.as_ref().map(|session| (
                session.source.as_str(),
                session.agent.as_str(),
                session.session_ref.value.as_str()
            )),
            Some(("herdr:droid", "droid", "droid-session"))
        );
    }

    #[test]
    fn foreground_agent_session_replaces_stale_different_owner_session_ref() {
        for session_start_source in ["resume", "startup"] {
            let mut terminal = test_terminal();
            terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("codex-session").unwrap(),
            });
            terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);

            let mutation = terminal
                .set_agent_session_ref_for_session_start(
                    "herdr:claude".into(),
                    "claude".into(),
                    crate::agent_resume::AgentSessionRef::id("claude-session"),
                    Some(21),
                    Some(session_start_source.into()),
                )
                .unwrap_or_else(|| {
                    panic!("{session_start_source} should replace stale codex session")
                });

            assert!(mutation.session_ref_changed);
            assert_eq!(
                terminal.persisted_agent_session.as_ref().map(|session| (
                    session.source.as_str(),
                    session.agent.as_str(),
                    session.session_ref.value.as_str()
                )),
                Some(("herdr:claude", "claude", "claude-session")),
                "{session_start_source} should store claude session"
            );
        }
    }

    #[test]
    fn foreground_agent_session_requires_lifecycle_source_to_replace_different_owner() {
        for session_start_source in [None, Some("other")] {
            let mut terminal = test_terminal();
            terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("codex-session").unwrap(),
            });
            terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);

            let mutation = terminal.set_agent_session_ref_for_session_start(
                "herdr:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-session"),
                Some(21),
                session_start_source.map(str::to_string),
            );

            assert!(
                mutation.is_none(),
                "{session_start_source:?} should not replace"
            );
            assert_eq!(
                terminal.persisted_agent_session.as_ref().map(|session| (
                    session.source.as_str(),
                    session.agent.as_str(),
                    session.session_ref.value.as_str()
                )),
                Some(("herdr:codex", "codex", "codex-session"))
            );
        }
    }

    #[test]
    fn different_owner_session_ref_requires_matching_detected_agent() {
        for session_start_source in ["startup", "resume"] {
            for detected_agent in [None, Some(Agent::Codex)] {
                let mut terminal = test_terminal();
                terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                    source: "herdr:codex".into(),
                    agent: "codex".into(),
                    session_ref: crate::agent_resume::AgentSessionRef::id("codex-session").unwrap(),
                });
                terminal.set_detected_state(detected_agent, AgentState::Idle);

                let mutation = terminal.set_agent_session_ref_for_session_start(
                    "herdr:claude".into(),
                    "claude".into(),
                    crate::agent_resume::AgentSessionRef::id("claude-session"),
                    Some(21),
                    Some(session_start_source.into()),
                );

                assert!(
                    mutation.is_none(),
                    "{session_start_source} with {detected_agent:?} should not replace"
                );
                assert_eq!(
                    terminal.persisted_agent_session.as_ref().map(|session| (
                        session.source.as_str(),
                        session.agent.as_str(),
                        session.session_ref.value.as_str()
                    )),
                    Some(("herdr:codex", "codex", "codex-session"))
                );
            }
        }
    }

    #[test]
    fn custom_session_report_does_not_replace_different_owner_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:codex".into(),
            agent: "codex".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("codex-session").unwrap(),
        });
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);

        let mutation = terminal.set_agent_session_ref_for_session_start(
            "custom:claude".into(),
            "claude".into(),
            crate::agent_resume::AgentSessionRef::id("claude-session"),
            Some(21),
            Some("resume".into()),
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal.persisted_agent_session.as_ref().map(|session| (
                session.source.as_str(),
                session.agent.as_str(),
                session.session_ref.value.as_str()
            )),
            Some(("herdr:codex", "codex", "codex-session"))
        );
    }

    #[test]
    fn foreground_agent_session_replaces_stale_different_owner_hook_authority() {
        let mut terminal = test_terminal();
        let now = std::time::Instant::now();
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::OpenCode,
            "herdr:opencode",
            "opencode",
            crate::agent_resume::AgentSessionRef::id("opencode-session").unwrap(),
        );
        terminal
            .set_hook_authority_at(
                "herdr:opencode".into(),
                "opencode".into(),
                AgentState::Working,
                None,
                crate::agent_resume::AgentSessionRef::id("opencode-session"),
                Some(20),
                now + Duration::from_millis(1),
            )
            .expect("initial hook authority should be accepted");
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            now,
        );

        let mutation = terminal
            .set_agent_session_ref_for_session_start(
                "herdr:codex".into(),
                "codex".into(),
                crate::agent_resume::AgentSessionRef::id("codex-session"),
                Some(21),
                Some("startup".into()),
            )
            .expect("foreground codex should replace stale hook authority");

        assert!(mutation.session_ref_changed);
        assert!(terminal.hook_authority.is_none());
        assert_eq!(
            terminal.current_session_identity_for_persistence(),
            Some((
                "herdr:codex".into(),
                "codex".into(),
                crate::agent_resume::AgentSessionRefKind::Id,
                "codex-session".into()
            ))
        );
        let late_old_session = terminal.set_hook_authority_with_session_ref(
            "herdr:opencode".into(),
            "opencode".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::id("opencode-session"),
            Some(22),
        );
        assert!(late_old_session.is_none());

        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        terminal
            .set_agent_session_ref_for_session_start(
                "herdr:opencode".into(),
                "opencode".into(),
                crate::agent_resume::AgentSessionRef::id("opencode-new-session"),
                Some(23),
                Some("new".into()),
            )
            .expect("fresh root session");
        let fresh_session = terminal.set_hook_authority_with_session_ref(
            "herdr:opencode".into(),
            "opencode".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::id("opencode-new-session"),
            Some(24),
        );
        assert!(fresh_session.is_some());
    }

    #[test]
    fn different_owner_full_lifecycle_hook_does_not_replace_existing_session_ref() {
        let mut terminal = test_terminal();
        terminal
            .set_agent_session_ref(
                "herdr:droid".into(),
                "droid".into(),
                crate::agent_resume::AgentSessionRef::id("droid-session"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let mutation = terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path("/tmp/pi-session.jsonl"),
            Some(21),
        );

        assert!(mutation.is_none());
        assert!(terminal.hook_authority.is_none());
        assert_eq!(
            terminal.persisted_agent_session.as_ref().map(|session| (
                session.source.as_str(),
                session.agent.as_str(),
                session.session_ref.value.as_str()
            )),
            Some(("herdr:droid", "droid", "droid-session"))
        );
    }

    #[test]
    fn repeated_same_agent_session_ref_is_accepted_without_session_change() {
        let mut terminal = test_terminal();
        terminal
            .set_agent_session_ref(
                "herdr:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-session"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let mutation = terminal
            .set_agent_session_ref(
                "herdr:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-session"),
                Some(21),
            )
            .expect("same session should be accepted");

        assert!(!mutation.session_ref_changed);
    }

    #[test]
    fn hook_authority_rejects_state_from_a_different_session() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::OpenCode,
            "herdr:opencode",
            "opencode",
            crate::agent_resume::AgentSessionRef::id("opencode-session").unwrap(),
        );
        terminal
            .set_hook_authority_with_session_ref(
                "herdr:opencode".into(),
                "opencode".into(),
                AgentState::Working,
                None,
                crate::agent_resume::AgentSessionRef::id("opencode-session"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let mutation = terminal.set_hook_authority_with_session_ref(
            "herdr:opencode".into(),
            "opencode".into(),
            AgentState::Blocked,
            Some("needs approval".into()),
            crate::agent_resume::AgentSessionRef::id("nested-session"),
            Some(21),
        );

        assert!(mutation.is_none());
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            terminal
                .hook_authority
                .as_ref()
                .and_then(|authority| authority.session_ref.as_ref())
                .map(|session_ref| session_ref.value.as_str()),
            Some("opencode-session")
        );
    }

    #[test]
    fn detected_agent_clear_does_not_clear_current_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal
            .set_agent_session_ref(
                "herdr:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-session"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let clear = terminal.set_detected_state_with_mutation(None, AgentState::Unknown);
        assert!(!clear.session_ref_changed);

        let mutation = terminal.set_agent_session_ref(
            "herdr:claude".into(),
            "claude".into(),
            crate::agent_resume::AgentSessionRef::id("new-session"),
            Some(21),
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("claude-session")
        );
    }

    #[test]
    fn clearing_hook_authority_clears_session_ref() {
        let mut terminal = test_terminal();
        let session_path = test_session_path("pi.jsonl");
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(session_path.clone()).unwrap(),
        );
        terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path),
            Some(20),
        );

        let mutation = terminal
            .clear_hook_authority_with_mutation(Some("herdr:pi"), Some(21))
            .expect("accepted clear");

        assert!(mutation.session_ref_changed);
        assert!(terminal.hook_authority.is_none());
    }

    #[test]
    fn agent_alias_survives_detection_uncertainty_and_reported_release_but_not_replacement() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);
        terminal.set_agent_name("reviewer".into());

        terminal.set_detected_state(None, AgentState::Unknown);
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));

        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        assert!(terminal.agent_name.is_none());

        terminal.set_agent_name("replacement".into());
        let mutation = terminal
            .release_agent_with_mutation("herdr:codex", "codex", None)
            .expect("detected agent release should be accepted");
        assert!(!mutation.agent_released);
        assert_eq!(terminal.agent_name.as_deref(), Some("replacement"));
        assert_eq!(terminal.detected_agent, Some(Agent::Codex));
    }

    #[test]
    fn custom_release_preserves_process_owned_agent_state() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal
            .set_hook_authority(
                "custom:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                Some(10),
            )
            .expect("custom state should be accepted");

        let mutation = terminal
            .release_agent_with_mutation("custom:pi", "pi", Some(11))
            .expect("custom release should be accepted");

        assert!(!mutation.agent_released);
        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, Some(Agent::Pi));
        assert_eq!(terminal.effective_agent_label(), Some("pi"));
        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn agent_replacement_clears_alias_owned_by_hook_identity() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            Some(20),
        );
        terminal.set_agent_name("reviewer".into());

        terminal.set_detected_state(Some(Agent::Grok), AgentState::Idle);

        assert!(terminal.agent_name.is_none());
        assert_eq!(terminal.effective_known_agent(), Some(Agent::Grok));
    }

    #[test]
    fn accepted_hook_replacement_clears_the_previous_agents_alias() {
        let mut terminal = test_terminal();
        terminal
            .set_hook_authority_at(
                "custom:agent".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                Some(20),
                Instant::now(),
            )
            .expect("initial hook should be accepted");
        terminal.set_agent_name("reviewer".into());

        terminal
            .set_hook_authority_at(
                "custom:agent".into(),
                "claude".into(),
                AgentState::Idle,
                None,
                None,
                Some(21),
                Instant::now(),
            )
            .expect("replacement hook should be accepted");

        assert!(terminal.agent_name.is_none());
        assert_eq!(terminal.effective_known_agent(), Some(Agent::Claude));
    }

    #[test]
    fn accepted_same_kind_hook_owner_replacement_clears_the_alias() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(test_session_path("first.jsonl")).unwrap(),
        );
        terminal
            .set_hook_authority_at(
                "herdr:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                crate::agent_resume::AgentSessionRef::path(test_session_path("first.jsonl")),
                Some(20),
                Instant::now(),
            )
            .expect("initial hook should be accepted");
        terminal.set_agent_name("reviewer".into());
        terminal
            .clear_hook_authority_with_mutation(Some("herdr:pi"), Some(21))
            .expect("hook clear should be accepted");
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));

        terminal
            .set_hook_authority_at(
                "herdr:pi".into(),
                "pi".into(),
                AgentState::Idle,
                None,
                crate::agent_resume::AgentSessionRef::path(test_session_path("second.jsonl")),
                Some(22),
                Instant::now(),
            )
            .expect("replacement hook should be accepted");

        assert!(terminal.agent_name.is_none());
        assert_eq!(terminal.effective_known_agent(), Some(Agent::Pi));
    }

    #[test]
    fn launch_command_alone_does_not_make_a_terminal_an_agent() {
        let terminal = test_terminal().with_launch_argv(vec!["just".into(), "dev".into()]);

        assert!(!terminal.is_agent_terminal());
    }

    #[test]
    fn release_agent_clears_matching_restored_session_ref_before_detection() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:hermes".into(),
            agent: "hermes".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("hermes-session").unwrap(),
        });

        let mutation = terminal
            .release_agent_with_mutation("herdr:hermes", "hermes", Some(21))
            .expect("accepted release");

        assert!(mutation.session_ref_changed);
        assert!(mutation.effective_state_change.is_none());
        assert!(terminal.persisted_agent_session.is_none());
    }

    #[test]
    fn release_agent_preserves_foreign_persisted_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("claude-session").unwrap(),
        });
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);

        let mutation = terminal
            .release_agent_with_mutation("herdr:pi", "pi", Some(21))
            .expect("visible agent release should be accepted");

        assert!(!mutation.session_ref_changed);
        assert_eq!(
            terminal.persisted_agent_session.as_ref().map(|session| (
                session.source.as_str(),
                session.agent.as_str(),
                session.session_ref.value.as_str()
            )),
            Some(("herdr:claude", "claude", "claude-session"))
        );
    }

    #[test]
    fn process_exit_clears_matching_persisted_session_ref() {
        let mut terminal = test_terminal();
        let session_ref =
            crate::agent_resume::AgentSessionRef::path(test_session_path("pi.jsonl")).unwrap();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            session_ref: session_ref.clone(),
        });
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);

        let mutation = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            false,
            true,
            std::time::Instant::now(),
        );

        assert!(mutation.session_ref_changed);
        assert!(terminal.persisted_agent_session.is_none());

        let delayed = terminal.set_agent_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            Some(session_ref),
            Some(21),
        );
        assert!(delayed.is_none());
        assert!(terminal.persisted_agent_session.is_none());
    }

    #[test]
    fn process_exit_preserves_foreign_persisted_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("claude-session").unwrap(),
        });
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);

        let mutation = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            false,
            true,
            std::time::Instant::now(),
        );

        assert!(!mutation.session_ref_changed);
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("claude-session")
        );
    }

    #[test]
    fn respawn_cleanup_resets_restored_agent_status() {
        let mut terminal = test_terminal();
        terminal.respawn_shell_on_exit = true;
        terminal.set_agent_name("codex".into());
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:codex".into(),
            agent: "codex".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("codex-session").unwrap(),
        });
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);

        terminal.clear_agent_runtime_identity_after_respawn();

        assert_eq!(terminal.state, AgentState::Unknown);
        assert!(terminal.detected_agent.is_none());
        assert!(terminal.agent_name.is_none());
        assert!(terminal.persisted_agent_session.is_none());
        assert!(!terminal.respawn_shell_on_exit);
    }

    #[test]
    fn agent_process_exit_tracks_recent_respawn_window() {
        let mut terminal = test_terminal();
        let now = std::time::Instant::now();

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::OpenCode),
            AgentState::Idle,
            false,
            false,
            false,
            true,
            now,
        );

        assert!(terminal.agent_process_exited_within(now, Duration::from_secs(2)));
        assert!(!terminal
            .agent_process_exited_within(now + Duration::from_secs(3), Duration::from_secs(2)));

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::OpenCode),
            AgentState::Working,
            false,
            false,
            true,
            false,
            now + Duration::from_secs(4),
        );

        assert!(!terminal
            .agent_process_exited_within(now + Duration::from_secs(4), Duration::from_secs(2)));
    }

    #[test]
    fn detected_conflict_clears_live_hook_but_preserves_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority_with_session_ref(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::id("claude-session"),
            Some(20),
        );

        let mutation =
            terminal.set_detected_state_with_mutation(Some(Agent::Grok), AgentState::Idle);

        assert!(!mutation.session_ref_changed);
        assert!(terminal.hook_authority.is_none());
        assert_eq!(
            terminal.persisted_agent_session.as_ref().map(|session| (
                session.source.as_str(),
                session.agent.as_str(),
                session.session_ref.value.as_str()
            )),
            Some(("herdr:claude", "claude", "claude-session"))
        );
    }

    #[test]
    fn detected_agent_disappearance_does_not_clear_full_lifecycle_hook_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Kimi), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Kimi,
            "herdr:kimi",
            "kimi",
            crate::agent_resume::AgentSessionRef::id("kimi-session").unwrap(),
        );
        terminal.set_hook_authority_with_session_ref(
            "herdr:kimi".into(),
            "kimi".into(),
            AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::id("kimi-session"),
            Some(20),
        );

        let mutation = terminal.set_detected_state_with_mutation(None, AgentState::Unknown);

        assert!(!mutation.session_ref_changed);
        assert!(terminal.hook_authority.is_some());
        assert!(terminal.persisted_agent_session.is_none());
        assert_eq!(terminal.effective_agent_label(), Some("kimi"));
    }

    #[test]
    fn detected_agent_disappearance_preserves_matching_persisted_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:opencode".into(),
            agent: "opencode".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("opencode-session").unwrap(),
        });

        let first =
            terminal.set_detected_state_with_mutation(Some(Agent::OpenCode), AgentState::Idle);
        assert!(!first.session_ref_changed);
        assert!(terminal.persisted_agent_session.is_some());

        let second = terminal.set_detected_state_with_mutation(None, AgentState::Unknown);
        assert!(!second.session_ref_changed);
        assert!(terminal.persisted_agent_session.is_some());
    }

    #[test]
    fn initial_unknown_detection_preserves_restored_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:hermes".into(),
            agent: "hermes".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("hermes-session").unwrap(),
        });

        let mutation = terminal.set_detected_state_with_mutation(None, AgentState::Unknown);
        assert!(!mutation.session_ref_changed);
        assert!(terminal.persisted_agent_session.is_some());
    }

    #[test]
    fn unsequenced_hook_report_is_ignored_after_source_uses_sequence() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(test_session_path("root.jsonl")).unwrap(),
        );
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );

        let change = terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            None,
        );

        assert!(change.is_none());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn stale_clear_all_sequence_is_checked_against_current_authority_source() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        anchor_full_lifecycle_session(
            &mut terminal,
            Agent::Pi,
            "herdr:pi",
            "pi",
            crate::agent_resume::AgentSessionRef::path(test_session_path("root.jsonl")).unwrap(),
        );
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );

        let change = terminal.clear_hook_authority(None, Some(19));

        assert!(change.is_none());
        assert_eq!(terminal.state, AgentState::Working);
        assert!(terminal.hook_authority.is_some());
    }

    #[test]
    fn same_sequence_from_different_sources_is_independent() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );

        terminal.set_hook_authority(
            "custom:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            Some(19),
        );

        assert_eq!(terminal.state, AgentState::Idle);
        assert_eq!(
            terminal.hook_authority.as_ref().unwrap().source,
            "custom:pi"
        );
    }
}
