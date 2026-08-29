//! Internal app events delivered via channel.
//!
//! Background tasks (PTY child watchers, future hook listeners, etc.) send
//! events to the main loop through this channel. No polling needed.

use std::time::Instant;

use crate::detect::{Agent, AgentState};
use crate::layout::PaneId;
use crate::workspace::{GitStatusCacheEntry, WorkspaceGitStatus};

#[derive(Debug)]
pub struct ApiWorktreeAddRequest {
    pub id: String,
    pub operation_id: u64,
    pub checkout_key: std::path::PathBuf,
    pub source_workspace_id: Option<String>,
    pub source_existing_membership: Option<crate::workspace::WorktreeSpaceMembership>,
    pub source_checkout_path: std::path::PathBuf,
    pub source_repo_root: std::path::PathBuf,
    pub repo_key: String,
    pub repo_name: String,
    pub label: Option<String>,
    pub focus: bool,
    pub respond_to: std::sync::mpsc::Sender<String>,
}

#[derive(Debug)]
pub struct WorktreeAddResult {
    pub path: std::path::PathBuf,
    pub api_request: Option<ApiWorktreeAddRequest>,
    pub result: Result<(), String>,
}

#[derive(Debug)]
pub struct ApiWorktreeRemoveRequest {
    pub id: String,
    pub operation_id: u64,
    pub checkout_key: std::path::PathBuf,
    pub respond_to: std::sync::mpsc::Sender<String>,
}

#[derive(Debug)]
pub struct WorktreeRemoveResult {
    pub workspace_id: String,
    pub path: std::path::PathBuf,
    pub workspace: Option<Box<crate::api::schema::WorkspaceInfo>>,
    pub worktree: Option<Box<crate::api::schema::WorktreeInfo>>,
    pub forced: bool,
    pub api_request: Option<ApiWorktreeRemoveRequest>,
    pub result: Result<(), String>,
}

/// An event from a background task to the main loop.
#[derive(Debug)]
pub enum AppEvent {
    /// A read-only Symphony Temporal workflow poll completed.
    SymphonyWorkflowsRefreshed { snapshot: crate::symphony::Snapshot },
    /// The focused repository's scratchpad file changed on disk; reload it.
    ScratchpadChanged,
    /// The append-only loop receipt source changed; refresh its cursor.
    LoopRunHistoryChanged,
    /// A bounded background native-metric sample completed.
    StatusMetricsRefreshed {
        snapshot: Option<Box<crate::platform::status_metrics::StatusMetricsSnapshot>>,
    },
    /// A bounded background provider-quota read completed.
    ProviderUsageRefreshed {
        snapshot: Box<crate::provider_usage::ProviderUsageSnapshot>,
    },
    /// A bounded background reachability probe completed.
    ConnectivityProbed { reachable: bool },
    /// A pane's child process exited.
    PaneDied { pane_id: PaneId },
    /// Fallback detector state changed in a pane.
    StateChanged {
        pane_id: PaneId,
        agent: Option<Agent>,
        state: AgentState,
        visible_blocker: bool,
        visible_working: bool,
        usage_limited: bool,
        process_exited: bool,
        observed_at: Instant,
    },
    /// The process composition below a pane changed.
    PaneProcessStateChanged {
        pane_id: PaneId,
        holds_shell: bool,
        stale_resolution: Option<(AgentState, bool)>,
    },
    /// Hook-authoritative agent state was reported for a pane.
    HookStateReported {
        pane_id: PaneId,
        source: String,
        agent_label: String,
        state: AgentState,
        message: Option<String>,
        seq: Option<u64>,
        wait: Option<String>,
        eta_s: Option<u64>,
        reported_at: Option<String>,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
    },
    /// Agent session identity was reported without state authority.
    AgentSessionReported {
        pane_id: PaneId,
        source: String,
        agent_label: String,
        seq: Option<u64>,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        claude_transcript_path: Option<std::path::PathBuf>,
        session_start_source: Option<String>,
    },
    /// Display-only agent metadata was reported for a pane.
    HookMetadataReported {
        pane_id: PaneId,
        source: String,
        agent_label: Option<String>,
        applies_to_source: Option<String>,
        title: Option<String>,
        display_agent: Option<String>,
        state_labels: std::collections::HashMap<String, String>,
        clear_title: bool,
        clear_display_agent: bool,
        clear_state_labels: bool,
        seq: Option<u64>,
        ttl: Option<std::time::Duration>,
    },
    /// Hook authority was explicitly cleared for a pane.
    HookAuthorityCleared {
        pane_id: PaneId,
        source: Option<String>,
        seq: Option<u64>,
    },
    /// Activity after a blocked full-lifecycle report made that report stale.
    HookAuthorityRetired {
        pane_id: PaneId,
        observed_at: Instant,
    },
    /// The current detected agent gracefully released this pane back to the shell.
    HookAgentReleased {
        pane_id: PaneId,
        source: String,
        agent_label: String,
        known_agent: Option<Agent>,
        seq: Option<u64>,
    },
    /// A new version is available through the active installation manager.
    UpdateReady {
        version: String,
        install_command: String,
    },
    /// Remote agent detection manifest update check finished.
    AgentDetectionManifestsUpdated {
        updated: Vec<crate::detect::manifest_update::ManifestUpdateCommit>,
        status: crate::detect::manifest_update::ManifestUpdateStatus,
    },
    /// A pane child emitted a valid OSC 52 clipboard write. The main loop
    /// re-emits it through herdr's own clipboard writer.
    ClipboardWrite { content: Vec<u8> },
    /// Prefix-mode ASCII input-source request, emitted on entering/leaving the ASCII input
    /// realm. The foreground process applies the host-local TIS switch (`active = true`) /
    /// restore (`active = false`): the client in server mode (via server forwarding), the
    /// app itself in monolithic mode.
    PrefixInputSource { active: bool },
    /// A pane child reported its shell current directory through terminal
    /// metadata such as OSC 7.
    TerminalCwdReported {
        pane_id: PaneId,
        cwd: std::path::PathBuf,
    },
    /// Background git status refresh completed for workspaces.
    GitStatusRefreshed {
        generation: u64,
        results: Vec<WorkspaceGitStatus>,
        cache_updates: Vec<(std::path::PathBuf, GitStatusCacheEntry)>,
    },
    /// Background git/gh work-context observations completed for live panes.
    GitWorkContextRefreshed {
        generation: u64,
        observations: Vec<crate::app::work_context_git::GitWorkContextObservation>,
        cache_updates: Vec<(
            crate::app::work_context_git::GitWorkContextCacheKey,
            crate::app::work_context_git::GitWorkContextCacheEntry,
        )>,
    },
    /// Background foreground-process observations completed for live panes.
    ForegroundProcessesRefreshed {
        generation: u64,
        observations: Vec<crate::app::foreground_process::ForegroundProcessObservation>,
    },
    /// Bounded incremental observations from Claude session transcripts.
    ClaudeSubagentsRefreshed {
        generation: u64,
        observations: Vec<crate::app::claude_subagents::RefreshObservation>,
        stats: crate::app::claude_subagents::BatchStats,
    },
    /// A plugin action or event command finished.
    PluginCommandFinished {
        log_id: String,
        finished_unix_ms: u64,
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
        error: Option<String>,
    },
    /// Background `git worktree add` completed.
    WorktreeAddFinished(Box<WorktreeAddResult>),
    /// Background `git worktree remove` completed.
    WorktreeRemoveFinished(Box<WorktreeRemoveResult>),
}
