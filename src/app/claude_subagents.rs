use std::collections::HashSet;
use std::fs::{File, Metadata, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::time::{Instant, SystemTime};

use serde_json::Value;

use crate::agent_resume::{AgentSessionRef, AgentSessionRefKind};

pub(crate) const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1500);
pub(crate) const WORKER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
pub(crate) const PER_TARGET_READ_QUANTUM: usize = 128 * 1024;
pub(crate) const BATCH_READ_BUDGET: usize = 2 * 1024 * 1024;
const MAX_TOTAL_CARRY: usize = 2 * 1024 * 1024;
const MAX_BUFFERED_JSON_ROW: usize = 256 * 1024;
const MAX_ACTIVE_IDS: usize = 4096;
const MAX_SUBAGENT_ID_BYTES: usize = 512;
const MAX_TRANSCRIPT_PATH_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
enum TranscriptEvent {
    Started(String),
    Finished(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TranscriptCursor {
    pub(crate) scan_offset: u64,
    pub(crate) committed_offset: u64,
    line_buffer: Vec<u8>,
    line_overflowed: bool,
    pub(crate) active_ids: HashSet<String>,
    pub(crate) caught_up_once: bool,
    pub(crate) trustworthy: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ParseStats {
    pub(crate) bytes_read: u64,
    pub(crate) lines_parsed: u64,
    pub(crate) partial_rows: u64,
    pub(crate) oversized_rows: u64,
    pub(crate) malformed_rows: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TranscriptTracker {
    pub(crate) session_id: String,
    pub(crate) path: PathBuf,
    pub(crate) target_generation: u64,
    pub(crate) cursor: TranscriptCursor,
    file_identity: Option<FileIdentity>,
    last_len: Option<u64>,
    last_modified: Option<SystemTime>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ScanStats {
    pub(crate) parse: ParseStats,
    pub(crate) files_opened: u64,
    pub(crate) metadata_hits: u64,
    pub(crate) identity_resets: u64,
    pub(crate) open_failures: u64,
    pub(crate) deadline_exhausted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RefreshInFlight {
    pub(crate) generation: u64,
    pub(crate) deadline: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TargetIdentity {
    pub(crate) terminal_id: crate::terminal::TerminalId,
    pub(crate) source: String,
    pub(crate) session_id: String,
    pub(crate) path: PathBuf,
    pub(crate) target_generation: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct RefreshObservation {
    pub(crate) target: TargetIdentity,
    pub(crate) tracker: TranscriptTracker,
    pub(crate) count: Option<u32>,
    pub(crate) stats: ScanStats,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BatchStats {
    pub(crate) targets_total: u64,
    pub(crate) targets_attempted: u64,
    pub(crate) files_opened: u64,
    pub(crate) metadata_hits: u64,
    pub(crate) bytes_read: u64,
    pub(crate) lines_parsed: u64,
    pub(crate) partial_rows: u64,
    pub(crate) oversized_rows: u64,
    pub(crate) malformed_rows: u64,
    pub(crate) identity_resets: u64,
    pub(crate) open_failures: u64,
    pub(crate) targets_caught_up: u64,
    pub(crate) deadline_exhausted: bool,
    pub(crate) elapsed_us: u64,
    pub(crate) max_target_us: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct RefreshWorkItem {
    pub(crate) target: TargetIdentity,
    pub(crate) tracker: TranscriptTracker,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FileIdentity {
    #[cfg(unix)]
    Unix { device: u64, inode: u64 },
    #[cfg(not(unix))]
    Portable { created: Option<SystemTime> },
}

impl TranscriptCursor {
    pub(crate) fn new() -> Self {
        Self {
            trustworthy: true,
            ..Self::default()
        }
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::new();
    }

    pub(crate) fn count(&self) -> Option<u32> {
        (self.caught_up_once && self.trustworthy)
            .then(|| u32::try_from(self.active_ids.len()).unwrap_or(u32::MAX))
    }

    fn apply_event(&mut self, event: TranscriptEvent) {
        match event {
            TranscriptEvent::Started(id) => {
                if self.active_ids.len() >= MAX_ACTIVE_IDS && !self.active_ids.contains(&id) {
                    self.trustworthy = false;
                } else {
                    self.active_ids.insert(id);
                }
            }
            TranscriptEvent::Finished(id) => {
                self.active_ids.remove(&id);
            }
        }
    }

    /// Consume bytes from the current file offset. The caller owns seeking and
    /// file-identity checks; this parser never retains more than one capped row.
    pub(crate) fn ingest(&mut self, bytes: &[u8], reached_eof: bool) -> ParseStats {
        let mut stats = ParseStats {
            bytes_read: bytes.len() as u64,
            ..ParseStats::default()
        };

        for &byte in bytes {
            self.scan_offset = self.scan_offset.saturating_add(1);
            if byte == b'\n' {
                if self.line_overflowed {
                    self.trustworthy = false;
                    stats.oversized_rows = stats.oversized_rows.saturating_add(1);
                } else {
                    let line = self
                        .line_buffer
                        .strip_suffix(b"\r")
                        .unwrap_or(&self.line_buffer);
                    if !line.is_empty() {
                        match parse_transcript_event(line) {
                            Ok(event) => {
                                if let Some(event) = event {
                                    self.apply_event(event);
                                }
                                stats.lines_parsed = stats.lines_parsed.saturating_add(1);
                            }
                            Err(()) => {
                                self.trustworthy = false;
                                stats.malformed_rows = stats.malformed_rows.saturating_add(1);
                            }
                        }
                    }
                }
                self.line_buffer.clear();
                self.line_overflowed = false;
                self.committed_offset = self.scan_offset;
            } else if self.line_overflowed {
                continue;
            } else if self.line_buffer.len() < MAX_BUFFERED_JSON_ROW {
                self.line_buffer.push(byte);
            } else {
                self.line_buffer.clear();
                self.line_overflowed = true;
            }
        }

        if reached_eof {
            if self.line_buffer.is_empty() && !self.line_overflowed {
                self.caught_up_once = true;
            } else {
                stats.partial_rows = 1;
            }
        }
        stats
    }
}

impl TranscriptTracker {
    pub(crate) fn new(session_id: String, path: PathBuf, target_generation: u64) -> Self {
        Self {
            session_id,
            path,
            target_generation,
            cursor: TranscriptCursor::new(),
            file_identity: None,
            last_len: None,
            last_modified: None,
        }
    }

    pub(crate) fn count(&self) -> Option<u32> {
        self.cursor.count()
    }

    fn retained_bytes(&self) -> usize {
        self.cursor.line_buffer.len()
    }

    fn invalidate_partial_row(&mut self) {
        self.cursor.trustworthy = false;
        self.cursor.line_buffer.clear();
        self.cursor.line_overflowed = true;
    }

    pub(crate) fn scan(&mut self, byte_budget: usize, deadline: Instant) -> ScanStats {
        let mut stats = ScanStats::default();
        let Ok((mut file, metadata, identity)) = open_regular_transcript(&self.path) else {
            stats.open_failures = 1;
            return stats;
        };
        stats.files_opened = 1;

        let replaced = self
            .file_identity
            .as_ref()
            .is_some_and(|previous| previous != &identity)
            || metadata.len() < self.cursor.scan_offset;
        if replaced {
            self.cursor.reset();
            stats.identity_resets = 1;
        }
        self.file_identity = Some(identity);

        let modified = metadata.modified().ok();
        if self.last_len == Some(metadata.len())
            && self.last_modified == modified
            && self.cursor.scan_offset == metadata.len()
            && self.cursor.line_buffer.is_empty()
            && !self.cursor.line_overflowed
        {
            stats.metadata_hits = 1;
            return stats;
        }

        if file.seek(SeekFrom::Start(self.cursor.scan_offset)).is_err() {
            stats.open_failures = 1;
            return stats;
        }
        let mut remaining = byte_budget;
        let mut buffer = [0_u8; 8192];
        let mut reached_eof = false;
        while remaining > 0 {
            if Instant::now() >= deadline {
                stats.deadline_exhausted = true;
                break;
            }
            let limit = remaining.min(buffer.len());
            match file.read(&mut buffer[..limit]) {
                Ok(0) => {
                    reached_eof = true;
                    break;
                }
                Ok(read) => {
                    remaining -= read;
                    let parsed = self.cursor.ingest(&buffer[..read], false);
                    merge_parse_stats(&mut stats.parse, parsed);
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    stats.open_failures = 1;
                    break;
                }
            }
        }
        if reached_eof {
            merge_parse_stats(&mut stats.parse, self.cursor.ingest(&[], true));
        }
        self.last_len = Some(metadata.len().max(self.cursor.scan_offset));
        self.last_modified = modified;
        stats
    }
}

pub(crate) fn refresh_trackers(
    work: Vec<RefreshWorkItem>,
    deadline: Instant,
) -> (Vec<RefreshObservation>, BatchStats) {
    let started = Instant::now();
    let mut budget = BATCH_READ_BUDGET;
    let mut observations = Vec::new();
    let mut batch = BatchStats {
        targets_total: work.len() as u64,
        ..BatchStats::default()
    };
    let mut total_carry = 0usize;

    for mut item in work {
        if budget == 0 || Instant::now() >= deadline {
            batch.deadline_exhausted = Instant::now() >= deadline;
            break;
        }
        let target_started = Instant::now();
        let stats = item
            .tracker
            .scan(budget.min(PER_TARGET_READ_QUANTUM), deadline);
        let target_us = target_started.elapsed().as_micros().min(u64::MAX as u128) as u64;
        batch.max_target_us = batch.max_target_us.max(target_us);
        let bytes_read = usize::try_from(stats.parse.bytes_read).unwrap_or(usize::MAX);
        budget = budget.saturating_sub(bytes_read);
        batch.targets_attempted = batch.targets_attempted.saturating_add(1);
        batch.files_opened = batch.files_opened.saturating_add(stats.files_opened);
        batch.metadata_hits = batch.metadata_hits.saturating_add(stats.metadata_hits);
        batch.bytes_read = batch.bytes_read.saturating_add(stats.parse.bytes_read);
        batch.lines_parsed = batch.lines_parsed.saturating_add(stats.parse.lines_parsed);
        batch.partial_rows = batch.partial_rows.saturating_add(stats.parse.partial_rows);
        batch.oversized_rows = batch
            .oversized_rows
            .saturating_add(stats.parse.oversized_rows);
        batch.malformed_rows = batch
            .malformed_rows
            .saturating_add(stats.parse.malformed_rows);
        batch.identity_resets = batch.identity_resets.saturating_add(stats.identity_resets);
        batch.open_failures = batch.open_failures.saturating_add(stats.open_failures);
        batch.deadline_exhausted |= stats.deadline_exhausted;
        batch.targets_caught_up = batch
            .targets_caught_up
            .saturating_add(u64::from(item.tracker.cursor.caught_up_once));

        let retained = item.tracker.retained_bytes();
        if total_carry.saturating_add(retained) > MAX_TOTAL_CARRY {
            item.tracker.invalidate_partial_row();
        } else {
            total_carry = total_carry.saturating_add(retained);
        }
        let count = item.tracker.count();
        observations.push(RefreshObservation {
            target: item.target,
            tracker: item.tracker,
            count,
            stats,
        });
    }
    batch.elapsed_us = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
    (observations, batch)
}

impl crate::app::App {
    pub(crate) fn claude_subagent_refresh_deadline(&self) -> Option<Instant> {
        if let Some(refresh) = self.claude_subagent_refresh_in_flight.as_ref() {
            return Some(refresh.deadline);
        }
        (!self.state.terminals.is_empty()).then_some(self.next_claude_subagent_refresh)
    }

    pub(crate) fn start_claude_subagent_refresh_if_due(&mut self, now: Instant) {
        if self
            .claude_subagent_refresh_in_flight
            .as_ref()
            .is_some_and(|refresh| now >= refresh.deadline)
        {
            self.claude_subagent_refresh_in_flight = None;
        }
        if self.claude_subagent_refresh_in_flight.is_some()
            || now < self.next_claude_subagent_refresh
        {
            return;
        }
        self.next_claude_subagent_refresh = now + POLL_INTERVAL;

        let mut raw_targets = self
            .state
            .terminals
            .iter()
            .filter_map(|(terminal_id, terminal)| {
                Some((
                    terminal_id.clone(),
                    "herdr:claude".to_string(),
                    terminal.claude_transcript_session_id.clone()?,
                    terminal.claude_transcript_path.clone()?,
                ))
            })
            .collect::<Vec<_>>();
        raw_targets.sort_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
        if raw_targets.is_empty() {
            self.claude_subagent_trackers.clear();
            return;
        }

        let current_ids = raw_targets
            .iter()
            .map(|target| target.0.clone())
            .collect::<HashSet<_>>();
        self.claude_subagent_trackers
            .retain(|terminal_id, _| current_ids.contains(terminal_id));

        let mut work = Vec::with_capacity(raw_targets.len());
        for (terminal_id, source, session_id, path) in raw_targets {
            let replace = self
                .claude_subagent_trackers
                .get(&terminal_id)
                .is_none_or(|tracker| tracker.session_id != session_id || tracker.path != path);
            if replace {
                self.next_claude_subagent_target_generation =
                    self.next_claude_subagent_target_generation.wrapping_add(1);
                self.claude_subagent_trackers.insert(
                    terminal_id.clone(),
                    TranscriptTracker::new(
                        session_id.clone(),
                        path.clone(),
                        self.next_claude_subagent_target_generation,
                    ),
                );
            }
            let tracker = self.claude_subagent_trackers[&terminal_id].clone();
            work.push(RefreshWorkItem {
                target: TargetIdentity {
                    terminal_id,
                    source,
                    session_id,
                    path,
                    target_generation: tracker.target_generation,
                },
                tracker,
            });
        }

        self.claude_subagent_refresh_rotation =
            rotate_refresh_work(&mut work, self.claude_subagent_refresh_rotation);

        self.last_claude_subagent_refresh_generation =
            self.last_claude_subagent_refresh_generation.wrapping_add(1);
        let generation = self.last_claude_subagent_refresh_generation;
        let deadline = now + WORKER_TIMEOUT;
        self.claude_subagent_refresh_in_flight = Some(RefreshInFlight {
            generation,
            deadline,
        });
        let event_tx = self.event_tx.clone();
        let _ = std::thread::Builder::new()
            .name("herdr-claude-subagents".into())
            .spawn(move || {
                let (observations, stats) = refresh_trackers(work, deadline);
                let _ = event_tx.blocking_send(crate::events::AppEvent::ClaudeSubagentsRefreshed {
                    generation,
                    observations,
                    stats,
                });
            });
    }

    pub(crate) fn handle_claude_subagents_refreshed(
        &mut self,
        generation: u64,
        observations: Vec<RefreshObservation>,
        stats: BatchStats,
    ) -> bool {
        if generation != self.last_claude_subagent_refresh_generation
            || generation <= self.last_applied_claude_subagent_refresh_generation
        {
            return false;
        }
        if self
            .claude_subagent_refresh_in_flight
            .as_ref()
            .is_none_or(|refresh| refresh.generation != generation)
        {
            return false;
        }
        if self
            .claude_subagent_refresh_in_flight
            .as_ref()
            .is_some_and(|refresh| Instant::now() >= refresh.deadline)
        {
            self.claude_subagent_refresh_in_flight = None;
            self.next_claude_subagent_refresh = Instant::now() + POLL_INTERVAL;
            return false;
        }
        self.claude_subagent_refresh_in_flight = None;
        self.last_applied_claude_subagent_refresh_generation = generation;

        let mut updated_terminal_ids = Vec::new();
        for observation in observations {
            let current_matches = self
                .state
                .terminals
                .get(&observation.target.terminal_id)
                .is_some_and(|terminal| {
                    observation.target.source == "herdr:claude"
                        && terminal.claude_transcript_session_id.as_deref()
                            == Some(observation.target.session_id.as_str())
                        && terminal.claude_transcript_path.as_ref()
                            == Some(&observation.target.path)
                        && self
                            .claude_subagent_trackers
                            .get(&observation.target.terminal_id)
                            .is_some_and(|tracker| {
                                tracker.target_generation == observation.target.target_generation
                            })
                });
            if !current_matches {
                continue;
            }
            self.claude_subagent_trackers
                .insert(observation.target.terminal_id.clone(), observation.tracker);
            updated_terminal_ids.push(observation.target.terminal_id);
            let _ = observation.count;
            let _ = observation.stats;
        }

        // A partial JSONL row is the only variable-size cursor state. Enforce
        // the process-wide cap after merging worker results, not only per batch.
        let mut tracker_ids = self
            .claude_subagent_trackers
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        tracker_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        let mut total_carry = 0usize;
        for terminal_id in tracker_ids {
            let Some(tracker) = self.claude_subagent_trackers.get_mut(&terminal_id) else {
                continue;
            };
            let retained = tracker.retained_bytes();
            if total_carry.saturating_add(retained) > MAX_TOTAL_CARRY {
                tracker.invalidate_partial_row();
                updated_terminal_ids.push(terminal_id);
            } else {
                total_carry = total_carry.saturating_add(retained);
            }
        }

        updated_terminal_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        updated_terminal_ids.dedup();
        let mut counts_changed = 0_u64;
        let mut changed_panes = Vec::new();
        for terminal_id in updated_terminal_ids {
            let count = self
                .claude_subagent_trackers
                .get(&terminal_id)
                .and_then(TranscriptTracker::count);
            let changed = self
                .state
                .terminals
                .get_mut(&terminal_id)
                .is_some_and(|terminal| terminal.set_active_subagents(count));
            if !changed {
                continue;
            }
            counts_changed = counts_changed.saturating_add(1);
            if let Some(location) =
                self.state
                    .workspaces
                    .iter()
                    .enumerate()
                    .find_map(|(ws_idx, workspace)| {
                        workspace.tabs.iter().find_map(|tab| {
                            tab.panes.iter().find_map(|(pane_id, pane)| {
                                (pane.attached_terminal_id == terminal_id)
                                    .then_some((ws_idx, *pane_id))
                            })
                        })
                    })
            {
                changed_panes.push(location);
            }
        }
        for (ws_idx, pane_id) in changed_panes {
            self.emit_pane_updated(ws_idx, pane_id);
        }
        let active_subagents_total = self
            .claude_subagent_trackers
            .values()
            .filter_map(TranscriptTracker::count)
            .fold(0_u64, |total, count| total.saturating_add(u64::from(count)));

        tracing::debug!(
            generation,
            targets_total = stats.targets_total,
            targets_attempted = stats.targets_attempted,
            files_opened = stats.files_opened,
            metadata_hits = stats.metadata_hits,
            bytes_read = stats.bytes_read,
            lines_parsed = stats.lines_parsed,
            partial_rows = stats.partial_rows,
            oversized_rows = stats.oversized_rows,
            malformed_rows = stats.malformed_rows,
            identity_resets = stats.identity_resets,
            open_failures = stats.open_failures,
            targets_caught_up = stats.targets_caught_up,
            deadline_exhausted = stats.deadline_exhausted,
            elapsed_us = stats.elapsed_us,
            max_target_us = stats.max_target_us,
            counts_changed,
            active_subagents_total,
            "refreshed Claude subagent transcripts"
        );
        counts_changed > 0
    }
}

fn rotate_refresh_work(work: &mut [RefreshWorkItem], rotation: usize) -> usize {
    if work.len() <= 1 {
        return 0;
    }
    let start = rotation % work.len();
    work.rotate_left(start);
    let full_quanta = (BATCH_READ_BUDGET / PER_TARGET_READ_QUANTUM).max(1);
    (start + full_quanta.min(work.len())) % work.len()
}

fn merge_parse_stats(total: &mut ParseStats, next: ParseStats) {
    total.bytes_read = total.bytes_read.saturating_add(next.bytes_read);
    total.lines_parsed = total.lines_parsed.saturating_add(next.lines_parsed);
    total.partial_rows = total.partial_rows.saturating_add(next.partial_rows);
    total.oversized_rows = total.oversized_rows.saturating_add(next.oversized_rows);
    total.malformed_rows = total.malformed_rows.saturating_add(next.malformed_rows);
}

fn open_regular_transcript(path: &Path) -> std::io::Result<(File, Metadata, FileIdentity)> {
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Claude transcript must not be a symlink",
        ));
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Claude transcript must be a regular file",
        ));
    }
    let identity = file_identity(&metadata);
    Ok((file, metadata, identity))
}

#[cfg(unix)]
fn file_identity(metadata: &Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;
    FileIdentity::Unix {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[cfg(not(unix))]
fn file_identity(metadata: &Metadata) -> FileIdentity {
    FileIdentity::Portable {
        created: metadata.created().ok(),
    }
}

pub(crate) fn validated_transcript_path(
    source: &str,
    agent_label: &str,
    session_ref: Option<&AgentSessionRef>,
    raw_path: Option<&str>,
) -> Option<PathBuf> {
    if source != "herdr:claude" || agent_label != "claude" {
        return None;
    }
    let session_ref = session_ref?;
    if session_ref.kind != AgentSessionRefKind::Id {
        return None;
    }
    let raw_path = raw_path?;
    if raw_path.is_empty()
        || raw_path.len() > MAX_TRANSCRIPT_PATH_BYTES
        || raw_path.chars().any(char::is_control)
    {
        return None;
    }

    let path = Path::new(raw_path);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return None;
    }
    let expected_file = format!("{}.jsonl", session_ref.value);
    let mut components = path.components().rev();
    let file = components.next()?.as_os_str();
    let project_slug = components.next()?;
    let projects = components.next()?.as_os_str();
    if file != expected_file.as_str()
        || projects != "projects"
        || !matches!(project_slug, Component::Normal(value) if !value.is_empty())
    {
        return None;
    }
    Some(path.to_path_buf())
}

fn parse_transcript_event(line: &[u8]) -> Result<Option<TranscriptEvent>, ()> {
    let value: Value = serde_json::from_slice(line).map_err(|_| ())?;
    if let Some(result) = value.get("toolUseResult") {
        if result.get("status").and_then(Value::as_str) == Some("async_launched")
            && result.get("isAsync").and_then(Value::as_bool) == Some(true)
        {
            return Ok(result
                .get("agentId")
                .and_then(Value::as_str)
                .and_then(valid_subagent_id)
                .map(TranscriptEvent::Started));
        }
        if result.get("success").and_then(Value::as_bool) == Some(true) {
            return Ok(result
                .get("resumedAgentId")
                .and_then(Value::as_str)
                .and_then(valid_subagent_id)
                .map(TranscriptEvent::Started));
        }
    }

    if value.get("type").and_then(Value::as_str) != Some("queue-operation")
        || value.get("operation").and_then(Value::as_str) != Some("enqueue")
    {
        return Ok(None);
    }
    let Some(content) = value.get("content").and_then(Value::as_str) else {
        return Ok(None);
    };
    if !content.starts_with("<task-notification>")
        || !content.contains("<status>completed</status>")
    {
        return Ok(None);
    }
    Ok(tag_value(content, "task-id")
        .and_then(valid_subagent_id)
        .map(TranscriptEvent::Finished))
}

fn tag_value<'a>(content: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = content.find(&open)? + open.len();
    let end = content[start..].find(&close)? + start;
    Some(&content[start..end])
}

fn valid_subagent_id(value: &str) -> Option<String> {
    (!value.is_empty()
        && value.len() <= MAX_SUBAGENT_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
    .then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    const SESSION_ID: &str = "6be5e8e1-cce2-4c1e-b04c-62e3e38eb75a";
    const AGENT_A: &str = "a5f724db00c0e2f2b";
    const AGENT_B: &str = "a2e17f250c84a35b2";

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let path = std::env::temp_dir().join(format!(
                "herdr-claude-subagents-{}-{label}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn transcript(&self) -> PathBuf {
            let projects = self.0.join("projects").join("-tmp-repro");
            std::fs::create_dir_all(&projects).unwrap();
            projects.join(format!("{SESSION_ID}.jsonl"))
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn line(value: Value) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(&value).unwrap();
        bytes.push(b'\n');
        bytes
    }

    fn launch(agent_id: &str) -> Vec<u8> {
        line(serde_json::json!({
            "type": "user",
            "toolUseResult": {
                "isAsync": true,
                "status": "async_launched",
                "agentId": agent_id,
                "description": "bounded fixture"
            }
        }))
    }

    fn completion(agent_id: &str) -> Vec<u8> {
        line(serde_json::json!({
            "type": "queue-operation",
            "operation": "enqueue",
            "sessionId": SESSION_ID,
            "content": format!(
                "<task-notification>\n<task-id>{agent_id}</task-id>\n<status>completed</status>\n</task-notification>"
            )
        }))
    }

    #[test]
    fn claude_async_launch_and_completion_fixture_uses_one_proven_identity() {
        let mut cursor = TranscriptCursor::new();
        let mut fixture = launch(AGENT_A);
        fixture.extend(launch(AGENT_B));
        cursor.ingest(&fixture, true);
        assert_eq!(cursor.count(), Some(2));

        cursor.ingest(&completion(AGENT_A), true);
        assert_eq!(cursor.count(), Some(1));
        assert!(cursor.active_ids.contains(AGENT_B));
    }

    #[test]
    fn successful_resume_readds_only_the_reported_agent() {
        let mut cursor = TranscriptCursor::new();
        cursor.ingest(&launch(AGENT_A), true);
        cursor.ingest(&completion(AGENT_A), true);
        cursor.ingest(
            &line(serde_json::json!({
                "type": "user",
                "toolUseResult": {
                    "success": true,
                    "message": "Resuming agent",
                    "resumedAgentId": AGENT_A
                }
            })),
            true,
        );
        assert_eq!(cursor.count(), Some(1));
    }

    #[test]
    fn duplicate_launch_and_completion_are_idempotent() {
        let mut cursor = TranscriptCursor::new();
        cursor.ingest(&launch(AGENT_A), true);
        cursor.ingest(&launch(AGENT_A), true);
        assert_eq!(cursor.count(), Some(1));
        cursor.ingest(&completion(AGENT_A), true);
        cursor.ingest(&completion(AGENT_A), true);
        assert_eq!(cursor.count(), Some(0));
    }

    #[test]
    fn unrelated_background_task_notification_does_not_remove_agent() {
        let mut cursor = TranscriptCursor::new();
        cursor.ingest(&launch(AGENT_A), true);
        cursor.ingest(&completion("bhqhdnqhg"), true);
        assert_eq!(cursor.count(), Some(1));
    }

    #[test]
    fn torn_final_record_is_retried_without_double_counting() {
        let launch = launch(AGENT_A);
        for split in 1..launch.len() {
            let mut cursor = TranscriptCursor::new();
            cursor.ingest(&launch[..split], true);
            assert_eq!(cursor.count(), None);
            cursor.ingest(&launch[split..], true);
            assert_eq!(cursor.count(), Some(1), "split at {split}");
        }
    }

    #[test]
    fn malformed_or_oversized_complete_row_invalidates_authority() {
        let mut malformed = TranscriptCursor::new();
        malformed.ingest(b"not-json\n", true);
        assert_eq!(malformed.count(), None);

        let mut oversized = TranscriptCursor::new();
        let mut row = vec![b'x'; MAX_BUFFERED_JSON_ROW + 1];
        row.push(b'\n');
        let stats = oversized.ingest(&row, true);
        assert_eq!(stats.oversized_rows, 1);
        assert_eq!(oversized.count(), None);
    }

    #[test]
    fn count_stays_unknown_until_initial_replay_reaches_complete_eof() {
        let launch = launch(AGENT_A);
        let mut cursor = TranscriptCursor::new();
        cursor.ingest(&launch[..launch.len() - 1], true);
        assert_eq!(cursor.count(), None);
        cursor.ingest(&launch[launch.len() - 1..], true);
        assert_eq!(cursor.count(), Some(1));
    }

    #[test]
    fn claude_transcript_path_binds_profile_path_to_session_id() {
        let session = AgentSessionRef::id(SESSION_ID).unwrap();
        let default = format!("/home/user/.claude/projects/-tmp-repro/{SESSION_ID}.jsonl");
        let profile = format!("/profiles/team-a/projects/-tmp-repro/{SESSION_ID}.jsonl");
        assert_eq!(
            validated_transcript_path("herdr:claude", "claude", Some(&session), Some(&default)),
            Some(PathBuf::from(default))
        );
        assert_eq!(
            validated_transcript_path("herdr:claude", "claude", Some(&session), Some(&profile)),
            Some(PathBuf::from(profile))
        );
    }

    #[test]
    fn spoofed_or_mismatched_transcript_path_is_rejected() {
        let session = AgentSessionRef::id(SESSION_ID).unwrap();
        let other = "/tmp/projects/-tmp-repro/other.jsonl";
        let traversal = format!("/tmp/projects/../-tmp-repro/{SESSION_ID}.jsonl");
        assert!(
            validated_transcript_path("custom:claude", "claude", Some(&session), Some(other))
                .is_none()
        );
        assert!(
            validated_transcript_path("herdr:claude", "claude", Some(&session), Some(other))
                .is_none()
        );
        assert!(validated_transcript_path(
            "herdr:claude",
            "claude",
            Some(&session),
            Some(&traversal)
        )
        .is_none());
    }

    #[test]
    fn tracker_reads_only_appended_bytes_after_initial_replay() {
        let dir = TestDir::new("append");
        let path = dir.transcript();
        let mut fixture = launch(AGENT_A);
        fixture.extend(launch(AGENT_B));
        std::fs::write(&path, &fixture).unwrap();
        let mut tracker = TranscriptTracker::new(SESSION_ID.into(), path.clone(), 1);
        let first = tracker.scan(PER_TARGET_READ_QUANTUM, Instant::now() + WORKER_TIMEOUT);
        assert_eq!(tracker.count(), Some(2));
        assert_eq!(first.parse.bytes_read as usize, fixture.len());

        let completed = completion(AGENT_A);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&completed)
            .unwrap();
        let second = tracker.scan(PER_TARGET_READ_QUANTUM, Instant::now() + WORKER_TIMEOUT);
        assert_eq!(tracker.count(), Some(1));
        assert_eq!(second.parse.bytes_read as usize, completed.len());

        let unchanged = tracker.scan(PER_TARGET_READ_QUANTUM, Instant::now() + WORKER_TIMEOUT);
        assert_eq!(unchanged.metadata_hits, 1);
        assert_eq!(unchanged.parse.bytes_read, 0);
    }

    #[test]
    fn tracker_bounds_initial_replay_and_does_not_publish_partial_count() {
        let dir = TestDir::new("bounded");
        let path = dir.transcript();
        let mut fixture = Vec::new();
        for index in 0..800 {
            fixture.extend(line(serde_json::json!({
                "type": "attachment",
                "index": index,
                "padding": "x".repeat(256)
            })));
        }
        fixture.extend(launch(AGENT_A));
        std::fs::write(&path, fixture).unwrap();
        let mut tracker = TranscriptTracker::new(SESSION_ID.into(), path, 1);
        let first = tracker.scan(32 * 1024, Instant::now() + WORKER_TIMEOUT);
        assert!(first.parse.bytes_read <= 32 * 1024);
        assert_eq!(tracker.count(), None);

        for _ in 0..16 {
            tracker.scan(32 * 1024, Instant::now() + WORKER_TIMEOUT);
            if tracker.count().is_some() {
                break;
            }
        }
        assert_eq!(tracker.count(), Some(1));
    }

    #[test]
    fn batch_read_budget_bounds_large_claude_fleets() {
        let dir = TestDir::new("fleet-budget");
        let mut fixture = Vec::new();
        while fixture.len() <= PER_TARGET_READ_QUANTUM {
            fixture.extend(line(serde_json::json!({
                "type": "attachment",
                "padding": "x".repeat(512)
            })));
        }

        let work = (0..41)
            .map(|index| {
                let path = dir.0.join(format!("target-{index}.jsonl"));
                std::fs::write(&path, &fixture).unwrap();
                let terminal_id = crate::terminal::TerminalId::alloc();
                RefreshWorkItem {
                    target: TargetIdentity {
                        terminal_id,
                        source: "herdr:claude".into(),
                        session_id: SESSION_ID.into(),
                        path: path.clone(),
                        target_generation: 1,
                    },
                    tracker: TranscriptTracker::new(SESSION_ID.into(), path, 1),
                }
            })
            .collect();

        let (observations, stats) =
            refresh_trackers(work, Instant::now() + std::time::Duration::from_secs(5));
        assert_eq!(stats.targets_total, 41);
        assert_eq!(stats.targets_attempted, 16);
        assert_eq!(observations.len(), 16);
        assert_eq!(stats.bytes_read as usize, BATCH_READ_BUDGET);
        assert!(observations
            .iter()
            .all(|observation| observation.count.is_none()));
    }

    #[test]
    fn unchanged_41_target_fleet_reads_zero_transcript_bytes() {
        let dir = TestDir::new("fleet-steady-state");
        let fixture = line(serde_json::json!({ "type": "attachment" }));
        let work = (0..41)
            .map(|index| {
                let path = dir.0.join(format!("target-{index}.jsonl"));
                std::fs::write(&path, &fixture).unwrap();
                RefreshWorkItem {
                    target: TargetIdentity {
                        terminal_id: crate::terminal::TerminalId::alloc(),
                        source: "herdr:claude".into(),
                        session_id: SESSION_ID.into(),
                        path: path.clone(),
                        target_generation: 1,
                    },
                    tracker: TranscriptTracker::new(SESSION_ID.into(), path, 1),
                }
            })
            .collect();
        let (initial, initial_stats) =
            refresh_trackers(work, Instant::now() + std::time::Duration::from_secs(5));
        let steady_work = initial
            .into_iter()
            .map(|observation| RefreshWorkItem {
                target: observation.target,
                tracker: observation.tracker,
            })
            .collect();
        let (_, steady_stats) = refresh_trackers(
            steady_work,
            Instant::now() + std::time::Duration::from_secs(5),
        );
        assert_eq!(initial_stats.targets_attempted, 41);
        assert_eq!(steady_stats.targets_attempted, 41);
        assert_eq!(steady_stats.metadata_hits, 41);
        assert_eq!(steady_stats.bytes_read, 0);
    }

    #[test]
    fn refresh_rotation_reaches_every_target_in_a_41_pane_fleet() {
        let dir = TestDir::new("rotation");
        let mut work = (0..41)
            .map(|index| {
                let path = dir.0.join(format!("target-{index}.jsonl"));
                RefreshWorkItem {
                    target: TargetIdentity {
                        terminal_id: crate::terminal::TerminalId::alloc(),
                        source: "herdr:claude".into(),
                        session_id: SESSION_ID.into(),
                        path: path.clone(),
                        target_generation: index,
                    },
                    tracker: TranscriptTracker::new(SESSION_ID.into(), path, index),
                }
            })
            .collect::<Vec<_>>();
        let mut rotation = 0;
        let mut visited = HashSet::new();

        for _ in 0..3 {
            rotation = rotate_refresh_work(&mut work, rotation);
            visited.extend(
                work.iter()
                    .take(BATCH_READ_BUDGET / PER_TARGET_READ_QUANTUM)
                    .map(|item| item.target.target_generation),
            );
            work.sort_by_key(|item| item.target.target_generation);
        }

        assert_eq!(visited.len(), 41);
        assert_eq!(rotation, 7);
    }

    fn app_with_claude_target(path: PathBuf) -> (crate::app::App, crate::terminal::TerminalId) {
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("claude")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .unwrap()
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: AgentSessionRef::id(SESSION_ID).unwrap(),
        });
        terminal.set_claude_transcript_target(Some(SESSION_ID.into()), Some(path));
        (app, terminal_id)
    }

    fn observation(
        terminal_id: crate::terminal::TerminalId,
        path: PathBuf,
        target_generation: u64,
        active_id: &str,
    ) -> RefreshObservation {
        let mut tracker =
            TranscriptTracker::new(SESSION_ID.into(), path.clone(), target_generation);
        tracker.cursor.ingest(&launch(active_id), true);
        RefreshObservation {
            target: TargetIdentity {
                terminal_id,
                source: "herdr:claude".into(),
                session_id: SESSION_ID.into(),
                path,
                target_generation,
            },
            count: tracker.count(),
            tracker,
            stats: ScanStats::default(),
        }
    }

    #[test]
    fn refresh_applies_live_count_and_rejects_stale_session_result() {
        let dir = TestDir::new("stale-result");
        let path = dir.transcript();
        let (mut app, terminal_id) = app_with_claude_target(path.clone());
        app.claude_subagent_trackers.insert(
            terminal_id.clone(),
            TranscriptTracker::new(SESSION_ID.into(), path.clone(), 7),
        );
        app.last_claude_subagent_refresh_generation = 1;
        app.claude_subagent_refresh_in_flight = Some(RefreshInFlight {
            generation: 1,
            deadline: Instant::now() + WORKER_TIMEOUT,
        });

        assert!(app.handle_claude_subagents_refreshed(
            1,
            vec![observation(terminal_id.clone(), path.clone(), 7, AGENT_A)],
            BatchStats::default(),
        ));
        assert_eq!(app.state.terminals[&terminal_id].active_subagents, Some(1));

        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_claude_transcript_target(None, None);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_active_subagents(None);
        app.last_claude_subagent_refresh_generation = 2;
        app.claude_subagent_refresh_in_flight = Some(RefreshInFlight {
            generation: 2,
            deadline: Instant::now() + WORKER_TIMEOUT,
        });

        assert!(!app.handle_claude_subagents_refreshed(
            2,
            vec![observation(terminal_id.clone(), path, 7, AGENT_B)],
            BatchStats::default(),
        ));
        assert_eq!(app.state.terminals[&terminal_id].active_subagents, None);
    }

    #[test]
    fn refresh_result_after_worker_deadline_is_ignored() {
        let dir = TestDir::new("expired-result");
        let path = dir.transcript();
        let (mut app, terminal_id) = app_with_claude_target(path.clone());
        app.claude_subagent_trackers.insert(
            terminal_id.clone(),
            TranscriptTracker::new(SESSION_ID.into(), path.clone(), 3),
        );
        app.last_claude_subagent_refresh_generation = 1;
        app.claude_subagent_refresh_in_flight = Some(RefreshInFlight {
            generation: 1,
            deadline: Instant::now() - std::time::Duration::from_millis(1),
        });

        assert!(!app.handle_claude_subagents_refreshed(
            1,
            vec![observation(terminal_id.clone(), path, 3, AGENT_A)],
            BatchStats::default(),
        ));
        assert_eq!(app.state.terminals[&terminal_id].active_subagents, None);
    }

    #[test]
    fn transcript_truncation_clears_and_rebuilds_active_set() {
        let dir = TestDir::new("truncate");
        let path = dir.transcript();
        std::fs::write(&path, launch(AGENT_A)).unwrap();
        let mut tracker = TranscriptTracker::new(SESSION_ID.into(), path.clone(), 1);
        tracker.scan(PER_TARGET_READ_QUANTUM, Instant::now() + WORKER_TIMEOUT);
        assert_eq!(tracker.count(), Some(1));

        std::fs::write(&path, []).unwrap();
        let reset = tracker.scan(PER_TARGET_READ_QUANTUM, Instant::now() + WORKER_TIMEOUT);
        assert_eq!(reset.identity_resets, 1);
        assert_eq!(tracker.count(), Some(0));
    }

    #[test]
    fn transcript_replacement_discards_old_active_ids() {
        let dir = TestDir::new("replace");
        let path = dir.transcript();
        let old_path = path.with_extension("old");
        std::fs::write(&path, launch(AGENT_A)).unwrap();
        let mut tracker = TranscriptTracker::new(SESSION_ID.into(), path.clone(), 1);
        tracker.scan(PER_TARGET_READ_QUANTUM, Instant::now() + WORKER_TIMEOUT);
        assert_eq!(tracker.count(), Some(1));

        std::fs::rename(&path, &old_path).unwrap();
        std::fs::write(&path, launch(AGENT_B)).unwrap();
        let reset = tracker.scan(PER_TARGET_READ_QUANTUM, Instant::now() + WORKER_TIMEOUT);
        assert_eq!(reset.identity_resets, 1);
        assert_eq!(tracker.count(), Some(1));
        assert!(tracker.cursor.active_ids.contains(AGENT_B));
        assert!(!tracker.cursor.active_ids.contains(AGENT_A));
    }

    #[cfg(unix)]
    #[test]
    fn tracker_rejects_symlink_and_non_regular_targets() {
        use std::os::unix::fs::symlink;

        let dir = TestDir::new("unsafe-targets");
        let regular = dir.0.join("regular.jsonl");
        let linked = dir.transcript();
        std::fs::write(&regular, launch(AGENT_A)).unwrap();
        symlink(&regular, &linked).unwrap();
        let mut linked_tracker = TranscriptTracker::new(SESSION_ID.into(), linked, 1);
        let linked_stats =
            linked_tracker.scan(PER_TARGET_READ_QUANTUM, Instant::now() + WORKER_TIMEOUT);
        assert_eq!(linked_stats.open_failures, 1);
        assert_eq!(linked_tracker.count(), None);

        let mut directory_tracker =
            TranscriptTracker::new(SESSION_ID.into(), dir.0.join("projects"), 1);
        let directory_stats =
            directory_tracker.scan(PER_TARGET_READ_QUANTUM, Instant::now() + WORKER_TIMEOUT);
        assert_eq!(directory_stats.open_failures, 1);
        assert_eq!(directory_tracker.count(), None);
    }
}
