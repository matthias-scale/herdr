use std::{
    cmp::Reverse,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use serde::Deserialize;
use tokio::sync::Notify;

use super::text::display_width;
use super::widgets::render_panel_shell;
use crate::{
    app::{state::InfoPanelLinkRow, AppState},
    render_signal::RenderSignal,
    terminal::{TerminalId, TerminalState},
    work_context::{work_link_candidates, WorkLinkKind},
};

pub(crate) const INFO_PANEL_MIN_WIDTH: u16 = 26;
const INFO_PANEL_WIDTH: u16 = 36;
const INFO_PANEL_MIN_MAIN_WIDTH: u16 = 44;
const USAGE_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
const MAX_USAGE_FILES: usize = 64;
const MAX_USAGE_DIRECTORIES: usize = 256;
const MAX_USAGE_ENTRIES: usize = 4096;
const MAX_USAGE_FILE_BYTES: u64 = 512 * 1024;
const MAX_CCUSAGE_DISCOVERY_ENTRIES: usize = 256;
const MAX_CCUSAGE_OUTPUT_BYTES: usize = 512 * 1024;
const CCUSAGE_TIMEOUT: Duration = Duration::from_secs(10);
const USAGE_WINDOW_MINUTES: [(u64, &str); 2] = [(300, "5h"), (10080, "week")];
const MAX_USAGE_AMOUNT: f64 = 1_000_000.0;
const MAX_CLAUDE_REMAINING_MINUTES: u64 = 24 * 60;
const MIN_USAGE_WAKEUP_DELAY: Duration = Duration::from_secs(1);

pub(crate) fn panel_width_for_main(main_width: u16) -> Option<u16> {
    let available = main_width.saturating_sub(INFO_PANEL_MIN_MAIN_WIDTH);
    (available >= INFO_PANEL_MIN_WIDTH).then_some(INFO_PANEL_WIDTH.min(available))
}

fn focused_terminal(app: &AppState) -> Option<&TerminalState> {
    let workspace = app.active.and_then(|ws_idx| app.workspaces.get(ws_idx))?;
    let pane_id = workspace.focused_pane_id()?;
    let terminal_id: &TerminalId = workspace.terminal_id(pane_id)?;
    app.terminals.get(terminal_id)
}

fn visible_candidates(terminal: &TerminalState) -> Vec<crate::work_context::WorkLinkCandidate> {
    let mut preview_seen = false;
    work_link_candidates(terminal.effective_work_context())
        .into_iter()
        .filter(|candidate| {
            if candidate.kind != WorkLinkKind::Preview {
                return true;
            }
            if preview_seen {
                false
            } else {
                preview_seen = true;
                true
            }
        })
        .take(9)
        .collect()
}

fn info_panel_layout_line(inner: Rect, index: usize) -> Option<Rect> {
    let offset = u16::try_from(index).ok()?;
    let y = inner.y.checked_add(offset)?;
    (y < inner.y.saturating_add(inner.height)).then_some(Rect::new(inner.x, y, inner.width, 1))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InfoPanelLayout {
    inner: Rect,
    link_rows: Vec<Rect>,
}

impl InfoPanelLayout {
    fn line(&self, index: usize) -> Option<Rect> {
        info_panel_layout_line(self.inner, index)
    }
}

fn info_panel_layout(area: Rect, link_count: usize) -> Option<InfoPanelLayout> {
    if area.width < 2 || area.height < 3 {
        return None;
    }
    let inner = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .inner(area);
    let link_rows = (0..link_count)
        .filter_map(|index| info_panel_layout_line(inner, 2usize.saturating_add(index)))
        .collect();
    Some(InfoPanelLayout { inner, link_rows })
}

pub(crate) fn compute_link_rows(app: &AppState, area: Rect) -> Vec<InfoPanelLinkRow> {
    let Some(terminal) = focused_terminal(app) else {
        return Vec::new();
    };
    let candidates = visible_candidates(terminal);
    let Some(layout) = info_panel_layout(area, candidates.len()) else {
        return Vec::new();
    };
    candidates
        .into_iter()
        .zip(layout.link_rows)
        .map(|(candidate, rect)| InfoPanelLinkRow {
            rect,
            copy_value: candidate.copy_value,
        })
        .collect()
}

fn state_label(terminal: &TerminalState) -> String {
    let agent = terminal
        .effective_display_agent()
        .or_else(|| terminal.effective_agent_label().map(str::to_string))
        .unwrap_or_else(|| "terminal".to_string());
    format!(
        "{agent} · {}",
        super::status::state_label(terminal.state, true)
    )
}

fn field_line(label: &str, value: &str, p: &crate::app::state::Palette) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label}: "),
            Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
        ),
        Span::styled(value.to_string(), Style::default().fg(p.text)),
    ])
}

fn link_prefix(kind: WorkLinkKind) -> &'static str {
    match kind {
        WorkLinkKind::Ticket => "ticket",
        WorkLinkKind::PullRequest => "pr",
        WorkLinkKind::Preview => "preview",
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
struct UsageSnapshot {
    codex: ProviderUsage,
    claude: ClaudeUsage,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct ProviderUsage {
    windows: Vec<UsageWindow>,
    credits: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
struct UsageWindow {
    used_percent: f64,
    window_minutes: u64,
    resets_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct ClaudeUsage {
    five_hour: Option<ClaudeUsageWindow>,
}

#[derive(Debug, Clone, PartialEq)]
struct ClaudeUsageWindow {
    cost_usd: f64,
    remaining_minutes: Option<u64>,
    resets_at: i64,
}

#[derive(Debug, Deserialize)]
struct RawRateLimits {
    primary: Option<RawUsageWindow>,
    secondary: Option<RawUsageWindow>,
    credits: Option<RawCredits>,
}

#[derive(Debug, Deserialize)]
struct RawUsageWindow {
    used_percent: f64,
    window_minutes: u64,
    resets_at: i64,
}

#[derive(Debug, Deserialize)]
struct RawCredits {
    balance: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct RawCcusageResponse {
    blocks: Vec<RawCcusageBlock>,
}

#[derive(Debug, Deserialize)]
struct RawCcusageBlock {
    #[serde(rename = "isActive")]
    is_active: bool,
    #[serde(rename = "endTime")]
    end_time: Option<String>,
    #[serde(rename = "costUSD")]
    cost_usd: Option<f64>,
    projection: Option<RawCcusageProjection>,
}

#[derive(Debug, Deserialize)]
struct RawCcusageProjection {
    #[serde(rename = "remainingMinutes")]
    remaining_minutes: u64,
}

fn parse_codex_record(line: &str) -> Option<ProviderUsage> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let rate_limits = value
        .get("payload")
        .and_then(|payload| payload.get("rate_limits"))
        .or_else(|| value.get("rate_limits"))?;
    let raw: RawRateLimits = serde_json::from_value(rate_limits.clone()).ok()?;

    let mut windows = [raw.primary, raw.secondary]
        .into_iter()
        .filter_map(|raw| raw.and_then(parse_usage_window))
        .collect::<Vec<_>>();
    windows.sort_unstable_by_key(|window| window.window_minutes);
    windows.dedup_by_key(|window| window.window_minutes);

    let credits = raw
        .credits
        .and_then(|credits| credits.balance)
        .and_then(parse_numeric_value);

    if windows.is_empty() {
        None
    } else {
        Some(ProviderUsage { windows, credits })
    }
}

fn parse_numeric_value(value: serde_json::Value) -> Option<f64> {
    let number = match value {
        serde_json::Value::Number(number) => number.as_f64()?,
        serde_json::Value::String(value) => value.parse().ok()?,
        _ => return None,
    };
    (number.is_finite() && (0.0..=MAX_USAGE_AMOUNT).contains(&number)).then_some(number)
}

fn parse_ccusage_output(output: &str, now: i64) -> Result<Option<ClaudeUsage>, ()> {
    let response: RawCcusageResponse = serde_json::from_str(output).map_err(|_| ())?;
    let Some(block) = response.blocks.into_iter().find(|block| block.is_active) else {
        return Ok(None);
    };
    let cost_usd = block
        .cost_usd
        .filter(|cost| cost.is_finite() && (0.0..=MAX_USAGE_AMOUNT).contains(cost))
        .ok_or(())?;
    let resets_at = parse_utc_timestamp(block.end_time.as_deref().ok_or(())?).ok_or(())?;
    if resets_at <= now {
        return Ok(Some(ClaudeUsage::default()));
    }
    let remaining_minutes = block.projection.ok_or(())?.remaining_minutes;
    let remaining_minutes =
        (remaining_minutes <= MAX_CLAUDE_REMAINING_MINUTES).then_some(remaining_minutes);
    Ok(Some(ClaudeUsage {
        five_hour: Some(ClaudeUsageWindow {
            cost_usd,
            remaining_minutes,
            resets_at,
        }),
    }))
}

fn parse_utc_timestamp(value: &str) -> Option<i64> {
    let value = value.strip_suffix('Z')?;
    let (date, time) = value.split_once('T')?;
    let mut date_parts = date.split('-');
    let year = date_parts.next()?.parse::<i64>().ok()?;
    let month = date_parts.next()?.parse::<i64>().ok()?;
    let day = date_parts.next()?.parse::<i64>().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) {
        return None;
    }
    let seconds = time.split(':').collect::<Vec<_>>();
    if seconds.len() != 3 {
        return None;
    }
    let hour = seconds[0].parse::<i64>().ok()?;
    let minute = seconds[1].parse::<i64>().ok()?;
    let second = seconds[2]
        .split_once('.')
        .map_or(seconds[2], |(whole, _)| whole)
        .parse::<i64>()
        .ok()?;
    if !(0..=23).contains(&hour) || !(0..=59).contains(&minute) || !(0..=59).contains(&second) {
        return None;
    }
    let days_in_month = match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(1..=days_in_month).contains(&day) {
        return None;
    }
    let days = days_from_civil(year, month, day)?;
    days.checked_mul(86_400)?.checked_add(
        hour.checked_mul(3_600)?
            .checked_add(minute.checked_mul(60)?)?
            .checked_add(second)?,
    )
}

fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
    let adjusted_year = year.checked_sub(i64::from(month <= 2))?;
    let era = if adjusted_year >= 0 {
        adjusted_year / 400
    } else {
        adjusted_year.checked_sub(399)? / 400
    };
    let year_of_era = adjusted_year.checked_sub(era.checked_mul(400)?)?;
    let month_prime = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era.checked_mul(146_097)?
        .checked_add(day_of_era)?
        .checked_sub(719_468)
}

fn parse_usage_window(raw: RawUsageWindow) -> Option<UsageWindow> {
    if !raw.used_percent.is_finite()
        || !(0.0..=100.0).contains(&raw.used_percent)
        || !USAGE_WINDOW_MINUTES
            .iter()
            .any(|(minutes, _)| *minutes == raw.window_minutes)
        || raw.resets_at <= 0
    {
        return None;
    }
    Some(UsageWindow {
        used_percent: raw.used_percent,
        window_minutes: raw.window_minutes,
        resets_at: raw.resets_at,
    })
}

fn usage_window(usage: &ProviderUsage, minutes: u64) -> Option<&UsageWindow> {
    usage
        .windows
        .iter()
        .find(|window| window.window_minutes == minutes)
}

fn recent_jsonl_files(root: &Path, max_files: usize) -> Result<Vec<PathBuf>, ()> {
    if max_files == 0 {
        return Ok(Vec::new());
    }

    let root_metadata = fs::metadata(root).map_err(|_| ())?;
    if !root_metadata.is_dir() {
        return Err(());
    }
    let root_mtime = root_metadata.modified().unwrap_or(UNIX_EPOCH);
    let mut directories = vec![(root_mtime, root.to_path_buf(), 0usize)];
    let mut visited_directories = 0usize;
    let mut visited_entries = 0usize;
    let mut files = Vec::new();

    while let Some((_, directory, depth)) = directories.pop() {
        if visited_directories >= MAX_USAGE_DIRECTORIES || visited_entries >= MAX_USAGE_ENTRIES {
            break;
        }
        visited_directories = visited_directories.saturating_add(1);

        let entries = fs::read_dir(directory).map_err(|_| ())?;
        let remaining_entries = MAX_USAGE_ENTRIES.saturating_sub(visited_entries);
        let mut child_directories = Vec::new();
        for entry in entries.take(remaining_entries) {
            let entry = entry.map_err(|_| ())?;
            visited_entries = visited_entries.saturating_add(1);
            let path = entry.path();
            let file_type = entry.file_type().map_err(|_| ())?;
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(UNIX_EPOCH);
            if file_type.is_dir() && depth < 4 {
                child_directories.push((modified, path, depth.saturating_add(1)));
            } else if file_type.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension == "jsonl")
            {
                files.push((modified, path));
                files.sort_unstable_by_key(|entry| Reverse(entry.0));
                files.truncate(max_files);
            }
        }
        child_directories.sort_unstable_by_key(|entry| Reverse(entry.0));
        directories.extend(child_directories.into_iter().rev());
    }

    Ok(files.into_iter().map(|(_, path)| path).collect())
}

fn read_file_tail(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    let start = length.saturating_sub(MAX_USAGE_FILE_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_USAGE_FILE_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    let text = String::from_utf8_lossy(&bytes);
    if start == 0 {
        return Some(text.into_owned());
    }
    text.find('\n')
        .map(|newline| text[newline.saturating_add(1)..].to_owned())
}

fn latest_codex_usage(path: &Path) -> Result<Option<ProviderUsage>, ()> {
    let contents = read_file_tail(path).ok_or(())?;
    Ok(contents.lines().filter_map(parse_codex_record).next_back())
}

fn home_path(directory: &str) -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .map(|home| home.join(directory))
}

fn load_codex_usage_from_root(root: &Path) -> ProviderRefresh<ProviderUsage> {
    let files = match recent_jsonl_files(root, MAX_USAGE_FILES) {
        Ok(files) => files,
        Err(()) => return ProviderRefresh::Failed,
    };
    for path in files {
        match latest_codex_usage(&path) {
            Ok(Some(usage)) => return ProviderRefresh::Fresh(usage),
            Ok(None) => {}
            Err(()) => return ProviderRefresh::Failed,
        }
    }
    ProviderRefresh::Empty
}

fn load_codex_usage() -> ProviderRefresh<ProviderUsage> {
    let Some(root) = home_path(".codex/sessions") else {
        return ProviderRefresh::Failed;
    };
    load_codex_usage_from_root(&root)
}

fn filter_expired_codex_usage(mut usage: ProviderUsage, now: Option<i64>) -> ProviderUsage {
    if let Some(now) = now {
        usage.windows.retain(|window| window.resets_at > now);
    } else {
        usage.windows.clear();
    }
    if usage.windows.is_empty() {
        usage.credits = None;
    }
    usage
}

fn filter_expired_claude_usage(mut usage: ClaudeUsage, now: Option<i64>) -> ClaudeUsage {
    if usage
        .five_hour
        .as_ref()
        .is_none_or(|window| now.is_none_or(|now| window.resets_at <= now))
    {
        usage.five_hour = None;
    }
    usage
}

fn filter_expired_usage_snapshot(mut snapshot: UsageSnapshot, now: Option<i64>) -> UsageSnapshot {
    snapshot.codex = filter_expired_codex_usage(snapshot.codex, now);
    snapshot.claude = filter_expired_claude_usage(snapshot.claude, now);
    snapshot
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn discover_ccusage() -> Option<PathBuf> {
    let path_candidate = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join("ccusage"))
        .find(|path| is_executable_file(path));
    if path_candidate.is_some() {
        return path_candidate;
    }

    if let Some(path) = home_path(".local/bin/ccusage") {
        if is_executable_file(&path) {
            return Some(path);
        }
    }

    let root = home_path(".local/state/fnm_multishells")?;
    let Ok(entries) = fs::read_dir(root) else {
        return None;
    };
    for (index, entry) in entries.enumerate() {
        if index >= MAX_CCUSAGE_DISCOVERY_ENTRIES {
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        let Some(candidate) = entry
            .file_type()
            .ok()
            .is_some_and(|file_type| file_type.is_dir())
            .then(|| entry.path().join("bin/ccusage"))
        else {
            continue;
        };
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn resolve_ccusage() -> Option<PathBuf> {
    static CCUSAGE_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    CCUSAGE_PATH.get_or_init(discover_ccusage).clone()
}

fn load_claude_usage() -> ProviderRefresh<ClaudeUsage> {
    let Some(binary) = resolve_ccusage() else {
        return ProviderRefresh::Failed;
    };
    let mut command = crate::noninteractive_process::command(binary);
    command.args(["blocks", "--active", "--json", "--offline"]);
    let output = match crate::noninteractive_process::output_with_deadline_limited(
        command,
        Instant::now() + CCUSAGE_TIMEOUT,
        MAX_CCUSAGE_OUTPUT_BYTES,
    ) {
        Ok(output) => output,
        Err(_) => return ProviderRefresh::Failed,
    };
    if !output.status.success() {
        return ProviderRefresh::Failed;
    }
    let output = match String::from_utf8(output.stdout) {
        Ok(output) => output,
        Err(_) => return ProviderRefresh::Failed,
    };
    let Some(now) = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
    else {
        return ProviderRefresh::Failed;
    };
    match parse_ccusage_output(&output, now) {
        Ok(Some(usage)) => ProviderRefresh::Fresh(usage),
        Ok(None) => ProviderRefresh::Empty,
        Err(()) => ProviderRefresh::Failed,
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
enum ProviderRefresh<T> {
    Fresh(T),
    Empty,
    #[default]
    Failed,
}

#[derive(Debug, Default)]
struct UsageRefresh {
    codex: ProviderRefresh<ProviderUsage>,
    claude: ProviderRefresh<ClaudeUsage>,
}

fn merge_usage_refresh(
    previous: &UsageSnapshot,
    refresh: UsageRefresh,
    now: Option<i64>,
) -> UsageSnapshot {
    UsageSnapshot {
        codex: match refresh.codex {
            ProviderRefresh::Fresh(usage) => usage,
            ProviderRefresh::Empty => ProviderUsage::default(),
            ProviderRefresh::Failed => filter_expired_codex_usage(previous.codex.clone(), now),
        },
        claude: match refresh.claude {
            ProviderRefresh::Fresh(usage) => usage,
            ProviderRefresh::Empty => ClaudeUsage::default(),
            ProviderRefresh::Failed => filter_expired_claude_usage(previous.claude.clone(), now),
        },
    }
}

fn current_unix_timestamp() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
}

fn load_usage_snapshot(previous: &UsageSnapshot) -> UsageSnapshot {
    let now = current_unix_timestamp();
    let codex = match load_codex_usage() {
        ProviderRefresh::Fresh(usage) => {
            ProviderRefresh::Fresh(filter_expired_codex_usage(usage, now))
        }
        refresh => refresh,
    };
    let claude = load_claude_usage();
    merge_usage_refresh(previous, UsageRefresh { codex, claude }, now)
}

struct UsageCacheState {
    snapshot: UsageSnapshot,
    last_refresh: Option<Instant>,
    last_wakeup: Option<Instant>,
    refresh_in_progress: bool,
}

struct UsageCache {
    state: Mutex<UsageCacheState>,
    wakeup: Condvar,
    wakeup_started: Mutex<bool>,
    refresh_interval: Duration,
}

impl UsageCache {
    fn new(refresh_interval: Duration) -> Self {
        Self {
            state: Mutex::new(UsageCacheState {
                snapshot: UsageSnapshot::default(),
                last_refresh: None,
                last_wakeup: None,
                refresh_in_progress: false,
            }),
            wakeup: Condvar::new(),
            wakeup_started: Mutex::new(false),
            refresh_interval,
        }
    }

    fn begin_refresh(&self, now: Instant) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.refresh_in_progress
            || state.last_refresh.is_some_and(|last| {
                now.checked_duration_since(last)
                    .is_some_and(|elapsed| elapsed < self.refresh_interval)
            })
        {
            return false;
        }
        state.refresh_in_progress = true;
        true
    }

    fn finish_refresh(&self, now: Instant, snapshot: UsageSnapshot) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.snapshot = snapshot;
        state.last_refresh = Some(now);
        state.last_wakeup = None;
        state.refresh_in_progress = false;
        drop(state);
        self.wakeup.notify_all();
    }

    fn publish_refresh(
        &self,
        now: Instant,
        snapshot: UsageSnapshot,
        render_notify: &Notify,
        render_dirty: &RenderSignal,
    ) {
        self.finish_refresh(now, snapshot);
        render_dirty.request_generic();
        render_notify.notify_one();
    }

    fn cancel_refresh(&self, now: Instant) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.last_refresh = Some(now);
        state.last_wakeup = None;
        state.refresh_in_progress = false;
        drop(state);
        self.wakeup.notify_all();
    }

    fn snapshot(&self) -> UsageSnapshot {
        self.snapshot_at(current_unix_timestamp())
    }

    fn purge_expired_snapshot(&self, now: Option<i64>) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let snapshot = filter_expired_usage_snapshot(state.snapshot.clone(), now);
        let changed = snapshot != state.snapshot;
        if changed {
            state.snapshot = snapshot;
            state.last_wakeup = None;
        }
        drop(state);
        if changed {
            self.wakeup.notify_all();
        }
        changed
    }

    fn snapshot_at(&self, now: Option<i64>) -> UsageSnapshot {
        self.purge_expired_snapshot(now);
        let Ok(state) = self.state.lock() else {
            return UsageSnapshot::default();
        };
        state.snapshot.clone()
    }

    fn next_wakeup_delay(&self, now: Instant) -> Duration {
        let unix_now = current_unix_timestamp();
        self.purge_expired_snapshot(unix_now);
        let Ok(state) = self.state.lock() else {
            return self.refresh_interval.max(MIN_USAGE_WAKEUP_DELAY);
        };
        let last_activity = match (state.last_refresh, state.last_wakeup) {
            (Some(last_refresh), Some(last_wakeup)) => Some(last_refresh.max(last_wakeup)),
            (Some(last), None) | (None, Some(last)) => Some(last),
            (None, None) => None,
        };
        let mut delay = last_activity.map_or(self.refresh_interval, |last| {
            self.refresh_interval
                .saturating_sub(now.saturating_duration_since(last))
        });
        if let Some(unix_now) = unix_now {
            for resets_at in state
                .snapshot
                .codex
                .windows
                .iter()
                .map(|window| window.resets_at)
                .chain(
                    state
                        .snapshot
                        .claude
                        .five_hour
                        .iter()
                        .map(|window| window.resets_at),
                )
            {
                let expiry = resets_at.saturating_sub(unix_now);
                let expiry = if expiry <= 0 {
                    Duration::ZERO
                } else {
                    Duration::from_secs(expiry as u64)
                };
                delay = delay.min(expiry);
            }
        }
        delay.max(MIN_USAGE_WAKEUP_DELAY)
    }

    fn ensure_wakeup(
        cache: &Arc<Self>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
    ) {
        let Ok(mut started) = cache.wakeup_started.lock() else {
            return;
        };
        if *started {
            return;
        }
        *started = true;
        let cache_for_thread = Arc::clone(cache);
        if std::thread::Builder::new()
            .name("herdr-usage-wakeup".to_string())
            .spawn(move || loop {
                let delay = cache_for_thread.next_wakeup_delay(Instant::now());
                let Ok(state) = cache_for_thread.state.lock() else {
                    return;
                };
                let (mut state, wait_result) =
                    match cache_for_thread.wakeup.wait_timeout(state, delay) {
                        Ok(result) => result,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                if !wait_result.timed_out() {
                    continue;
                }
                state.last_wakeup = Some(Instant::now());
                drop(state);
                render_dirty.request_generic();
                render_notify.notify_one();
            })
            .is_err()
        {
            *started = false;
        }
    }

    fn request_refresh(
        cache: &Arc<Self>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
    ) {
        if !cache.begin_refresh(Instant::now()) {
            return;
        }
        let cache_for_thread = Arc::clone(cache);
        if std::thread::Builder::new()
            .name("herdr-usage-refresh".to_string())
            .spawn(move || {
                let now = Instant::now();
                let previous = cache_for_thread.snapshot();
                let snapshot = load_usage_snapshot(&previous);
                cache_for_thread.publish_refresh(now, snapshot, &render_notify, &render_dirty);
            })
            .is_err()
        {
            cache.cancel_refresh(Instant::now());
        }
    }

    #[cfg(test)]
    fn refresh_if_due<F>(&self, now: Instant, loader: F) -> UsageSnapshot
    where
        F: FnOnce() -> UsageRefresh,
    {
        if self.begin_refresh(now) {
            let previous = self.snapshot();
            let snapshot = merge_usage_refresh(&previous, loader(), current_unix_timestamp());
            self.finish_refresh(now, snapshot.clone());
            snapshot
        } else {
            self.snapshot()
        }
    }
}

fn usage_snapshot(render_handles: Option<(&Arc<Notify>, &Arc<RenderSignal>)>) -> UsageSnapshot {
    static CACHE: OnceLock<Arc<UsageCache>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Arc::new(UsageCache::new(USAGE_REFRESH_INTERVAL)));
    if let Some((render_notify, render_dirty)) = render_handles {
        UsageCache::ensure_wakeup(cache, Arc::clone(render_notify), Arc::clone(render_dirty));
        UsageCache::request_refresh(cache, Arc::clone(render_notify), Arc::clone(render_dirty));
    }
    cache.snapshot()
}

fn usage_reset_label(resets_at: i64) -> String {
    let seconds = resets_at.rem_euclid(86_400);
    format!("{:02}:{:02}Z", seconds / 3_600, (seconds % 3_600) / 60)
}

fn render_usage_line(
    frame: &mut Frame,
    row: Option<Rect>,
    text: String,
    palette: &crate::app::state::Palette,
) {
    if let Some(row) = row {
        frame.render_widget(
            Paragraph::new(Span::styled(text, Style::default().fg(palette.text))),
            row,
        );
    }
}

fn usage_window_text(window_label: &str, window: &UsageWindow, width: usize) -> String {
    let left = (100.0 - window.used_percent).max(0.0);
    let reset = usage_reset_label(window.resets_at);
    let full = format!(
        "{window_label} {left:.0}% left · {:.0}% used · {reset}",
        window.used_percent
    );
    if display_width(&full) <= width {
        return full;
    }

    let compact = format!(
        "{window_label} {left:.0}% · {:.0}% · {reset}",
        window.used_percent
    );
    if display_width(&compact) <= width {
        return compact;
    }

    let tight = format!("{left:.0}% · {:.0}% · {reset}", window.used_percent);
    if display_width(&tight) <= width {
        return tight;
    }

    let left_only = format!("{left:.0}%");
    if display_width(&left_only) <= width {
        return left_only;
    }

    format!("{left:.0}%")
}

fn claude_remaining_text(minutes: Option<u64>) -> String {
    let Some(minutes) = minutes else {
        return "—".to_string();
    };
    if minutes >= 24 * 60 {
        format!("{}d", minutes / (24 * 60))
    } else if minutes >= 60 {
        format!("{}h", minutes / 60)
    } else {
        format!("{minutes}m")
    }
}

fn claude_usage_text(window: &ClaudeUsageWindow, width: usize) -> String {
    let reset = usage_reset_label(window.resets_at);
    let remaining_minutes = window
        .remaining_minutes
        .map_or_else(|| "—".to_string(), |minutes| format!("{minutes}m"));
    let remaining_with_suffix = if window.remaining_minutes.is_some() {
        format!("{remaining_minutes} left")
    } else {
        remaining_minutes.clone()
    };
    let full = format!(
        "5h ${:.2} · {remaining_with_suffix} · {reset}",
        window.cost_usd
    );
    if display_width(&full) <= width {
        return full;
    }

    let compact = format!("5h ${:.2} · {remaining_minutes} · {reset}", window.cost_usd);
    if display_width(&compact) <= width {
        return compact;
    }

    let tight = format!("${:.2} · {remaining_minutes} · {reset}", window.cost_usd);
    if display_width(&tight) <= width {
        return tight;
    }

    let rounded_cost = format!("${:.0}", window.cost_usd);
    let remaining = claude_remaining_text(window.remaining_minutes);
    let rounded = format!("5h {rounded_cost} · {remaining} · {reset}");
    if display_width(&rounded) <= width {
        return rounded;
    }

    let compact = format!("{rounded_cost} · {remaining} · {reset}");
    if display_width(&compact) <= width {
        return compact;
    }

    let quota = format!("{rounded_cost} · {remaining}");
    if display_width(&quota) <= width {
        return quota;
    }

    if display_width(&rounded_cost) <= width {
        return rounded_cost;
    }
    if display_width(&remaining) <= width {
        return remaining;
    }

    quota
}

fn credits_text(balance: f64, width: usize) -> String {
    let full = format!("credits: {balance:.2}");
    if display_width(&full) <= width {
        return full;
    }

    let compact = format!("credits: {balance:.0}");
    if display_width(&compact) <= width {
        return compact;
    }

    "credits: —".to_string()
}

fn render_subscription_usage(
    app: &AppState,
    frame: &mut Frame,
    layout: &InfoPanelLayout,
    start: usize,
    render_handles: Option<(&Arc<Notify>, &Arc<RenderSignal>)>,
) {
    let usage = usage_snapshot(render_handles);
    if let Some(row) = layout.line(start) {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "SUBSCRIPTION USAGE",
                Style::default()
                    .fg(app.palette.text)
                    .add_modifier(Modifier::BOLD),
            ))),
            row,
        );
    }

    for (provider_index, label) in ["CODEX", "CLAUDE CODE"].into_iter().enumerate() {
        let provider_start = start.saturating_add(1 + provider_index.saturating_mul(4));
        if let Some(row) = layout.line(provider_start) {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    label,
                    Style::default()
                        .fg(app.palette.accent)
                        .add_modifier(Modifier::BOLD),
                ))),
                row,
            );
        }
        if provider_index == 0 {
            for (window_index, (minutes, window_label)) in USAGE_WINDOW_MINUTES.iter().enumerate() {
                let text = usage_window(&usage.codex, *minutes)
                    .map(|window| {
                        let width = layout
                            .line(provider_start.saturating_add(1 + window_index))
                            .map_or(0, |row| usize::from(row.width));
                        usage_window_text(window_label, window, width)
                    })
                    .unwrap_or_else(|| format!("{window_label} — · —"));
                render_usage_line(
                    frame,
                    layout.line(provider_start.saturating_add(1 + window_index)),
                    text,
                    &app.palette,
                );
            }
            render_usage_line(
                frame,
                layout.line(provider_start.saturating_add(3)),
                usage.codex.credits.as_ref().map_or_else(
                    || "credits: —".to_string(),
                    |balance| {
                        let width = layout
                            .line(provider_start.saturating_add(3))
                            .map_or(0, |row| usize::from(row.width));
                        credits_text(*balance, width)
                    },
                ),
                &app.palette,
            );
        } else {
            let width = layout
                .line(provider_start.saturating_add(1))
                .map_or(0, |row| usize::from(row.width));
            let text = usage
                .claude
                .five_hour
                .as_ref()
                .map(|window| claude_usage_text(window, width))
                .unwrap_or_else(|| "5h — · —".to_string());
            render_usage_line(
                frame,
                layout.line(provider_start.saturating_add(1)),
                text,
                &app.palette,
            );
            render_usage_line(
                frame,
                layout.line(provider_start.saturating_add(2)),
                "week — · —".to_string(),
                &app.palette,
            );
        }
    }
}

pub(super) fn render_info_panel(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    render_handles: Option<(&Arc<Notify>, &Arc<RenderSignal>)>,
) {
    let Some(inner) = render_panel_shell(frame, area, app.palette.accent, app.palette.panel_bg)
    else {
        return;
    };
    let Some(terminal) = focused_terminal(app) else {
        frame.render_widget(Paragraph::new("no focused pane"), inner);
        return;
    };

    let context = terminal.effective_work_context();
    let candidates = visible_candidates(terminal);
    let Some(layout) = info_panel_layout(area, candidates.len()) else {
        return;
    };
    debug_assert_eq!(inner, layout.inner);
    let title = context.work_title.as_deref().unwrap_or("untitled");
    let branch = context.branch.as_deref().unwrap_or("—");

    if let Some(row) = layout.line(0) {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "WORK CONTEXT",
                Style::default()
                    .fg(app.palette.text)
                    .add_modifier(Modifier::BOLD),
            ))),
            row,
        );
    }
    if let Some(row) = layout.line(1) {
        frame.render_widget(
            Paragraph::new(field_line("title", title, &app.palette)),
            row,
        );
    }
    for (index, candidate) in candidates.iter().enumerate() {
        let Some(row) = layout.link_rows.get(index).copied() else {
            break;
        };
        let number = if index < 9 {
            format!("{} ", index + 1)
        } else {
            "  ".to_string()
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(number, Style::default().fg(app.palette.accent)),
                Span::styled(
                    format!("{}: ", link_prefix(candidate.kind)),
                    Style::default().fg(app.palette.overlay0),
                ),
                Span::styled(
                    candidate.label.clone(),
                    Style::default().fg(app.palette.text),
                ),
            ])),
            row,
        );
    }
    if candidates.is_empty() {
        if let Some(row) = layout.line(2) {
            frame.render_widget(Paragraph::new(field_line("links", "—", &app.palette)), row);
        }
    }
    let footer_start = 2usize.saturating_add(candidates.len().max(1));
    if let Some(row) = layout.line(footer_start) {
        frame.render_widget(
            Paragraph::new(field_line("branch", branch, &app.palette)),
            row,
        );
    }
    if let Some(row) = layout.line(footer_start.saturating_add(1)) {
        frame.render_widget(
            Paragraph::new(field_line("agent", &state_label(terminal), &app.palette)),
            row,
        );
    }

    if app.show_subscription_usage {
        render_subscription_usage(
            app,
            frame,
            &layout,
            footer_start.saturating_add(2),
            render_handles,
        );
    }

    // Keep each link as one screen row so its hit target matches the rendered
    // numbering even when a URL is longer than the desktop panel.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::state::AppState, work_context::PaneWorkContextPatch};
    use ratatui::{backend::TestBackend, Terminal};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn codex_record(rate_limits: &str) -> String {
        format!(r#"{{"payload":{{"rate_limits":{rate_limits}}}}}"#)
    }

    fn ccusage_now() -> i64 {
        parse_utc_timestamp("2026-08-06T12:00:00Z").expect("valid test timestamp")
    }

    fn populated_usage_snapshot(resets_at: i64) -> UsageSnapshot {
        UsageSnapshot {
            codex: ProviderUsage {
                windows: vec![UsageWindow {
                    used_percent: 31.0,
                    window_minutes: 10080,
                    resets_at,
                }],
                credits: Some(12.5),
            },
            claude: ClaudeUsage {
                five_hour: Some(ClaudeUsageWindow {
                    cost_usd: 56.68,
                    remaining_minutes: Some(66),
                    resets_at,
                }),
            },
        }
    }

    fn codex_snapshot_with_credits() -> UsageSnapshot {
        UsageSnapshot {
            codex: ProviderUsage {
                windows: vec![UsageWindow {
                    used_percent: 31.0,
                    window_minutes: 10080,
                    resets_at: current_unix_timestamp().expect("current test timestamp") + 3600,
                }],
                credits: Some(12.5),
            },
            ..UsageSnapshot::default()
        }
    }

    fn temp_codex_root() -> PathBuf {
        static NEXT_ROOT: AtomicUsize = AtomicUsize::new(0);
        std::env::temp_dir().join(format!(
            "herdr-info-panel-codex-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn parses_valid_codex_record_and_credit_balance() {
        let usage = parse_codex_record(&codex_record(
            r#"{"primary":{"used_percent":31.0,"window_minutes":10080,"resets_at":1786537158},"credits":{"has_credits":true,"unlimited":false,"balance":"1927.95"}}"#,
        ))
        .expect("valid Codex usage record");

        assert_eq!(usage.windows.len(), 1);
        assert_eq!(usage.windows[0].window_minutes, 10080);
        assert_eq!(usage.windows[0].used_percent, 31.0);
        assert_eq!(usage.credits, Some(1927.95));
    }

    #[test]
    fn rejects_non_numeric_credit_balance() {
        let usage = parse_codex_record(&codex_record(
            r#"{"primary":{"used_percent":31.0,"window_minutes":10080,"resets_at":1786537158},"credits":{"balance":"private quota"}}"#,
        ))
        .expect("valid Codex usage record");

        assert_eq!(usage.credits, None);
    }

    #[test]
    fn rejects_negative_and_implausibly_large_credit_balances() {
        for balance in ["-1.0", "1000001.0"] {
            let usage = parse_codex_record(&codex_record(&format!(
                r#"{{"primary":{{"used_percent":31.0,"window_minutes":10080,"resets_at":1786537158}},"credits":{{"balance":"{balance}"}}}}"#
            )))
            .expect("valid Codex usage record");

            assert_eq!(usage.credits, None, "balance {balance} should be rejected");
        }
    }

    #[test]
    fn ignores_codex_record_without_rate_limits() {
        assert_eq!(
            parse_codex_record(r#"{"payload":{"type":"token_count"}}"#),
            None
        );
    }

    #[test]
    fn ignores_malformed_codex_record() {
        assert_eq!(parse_codex_record("not json"), None);
    }

    #[test]
    fn readable_codex_sessions_without_rate_limits_clear_stale_usage() {
        let root = temp_codex_root();
        fs::create_dir_all(&root).expect("create temporary Codex sessions root");
        fs::write(
            root.join("session.jsonl"),
            r#"{"payload":{"type":"token_count"}}"#,
        )
        .expect("write non-rate-limit Codex record");

        let now = ccusage_now();
        let previous = populated_usage_snapshot(now + 3600);
        let refresh = load_codex_usage_from_root(&root);
        assert_eq!(refresh, ProviderRefresh::Empty);

        let refreshed = merge_usage_refresh(
            &previous,
            UsageRefresh {
                codex: refresh,
                ..UsageRefresh::default()
            },
            Some(now),
        );
        assert_eq!(refreshed.codex, ProviderUsage::default());

        fs::remove_dir_all(root).expect("remove temporary Codex sessions root");
    }

    #[test]
    fn parses_ccusage_active_block() {
        let usage = parse_ccusage_output(
            r#"{
                "blocks": [
                    {"isActive": false},
                    {
                        "isActive": true,
                        "endTime": "2026-08-06T14:00:00.000Z",
                        "totalTokens": 71580953,
                        "costUSD": 56.68,
                        "burnRate": {"costPerHour": 15.06},
                        "projection": {
                            "totalTokens": 92373740,
                            "totalCost": 73.15,
                            "remainingMinutes": 66
                        }
                    }
            ]
            }"#,
            ccusage_now(),
        )
        .expect("valid ccusage JSON")
        .expect("active ccusage block");
        let window = usage.five_hour.expect("five-hour usage");

        assert_eq!(window.cost_usd, 56.68);
        assert_eq!(window.remaining_minutes, Some(66));
        assert_eq!(usage_reset_label(window.resets_at), "14:00Z");
    }

    #[test]
    fn ccusage_bounds_implausible_remaining_minutes_to_dash() {
        let usage = parse_ccusage_output(
            r#"{"blocks":[{"isActive":true,"endTime":"2026-08-06T14:00:00.000Z","costUSD":1000000,"projection":{"remainingMinutes":18446744073709551615}}]}"#,
            ccusage_now(),
        )
        .expect("valid ccusage JSON")
        .expect("active ccusage block");
        let window = usage.five_hour.expect("five-hour usage");

        assert_eq!(window.cost_usd, MAX_USAGE_AMOUNT);
        assert_eq!(window.remaining_minutes, None);
        let rendered = claude_usage_text(&window, 24);
        assert!(rendered.contains("$1000000"), "{rendered}");
        assert!(rendered.contains('—'), "{rendered}");
    }

    #[test]
    fn ccusage_expired_active_block_degrades_to_no_current_window() {
        let usage = parse_ccusage_output(
            r#"{"blocks":[{"isActive":true,"endTime":"2026-08-06T14:00:00.000Z","costUSD":56.68,"projection":{"remainingMinutes":66}}]}"#,
            parse_utc_timestamp("2026-08-06T15:00:00Z").expect("valid test timestamp"),
        )
        .expect("valid ccusage JSON")
        .expect("expired block is a parsed refresh");

        assert_eq!(usage.five_hour, None);
    }

    #[test]
    fn ccusage_without_an_active_block_degrades_to_none() {
        assert_eq!(
            parse_ccusage_output(r#"{"blocks":[{"isActive":false}]}"#, ccusage_now())
                .expect("valid ccusage JSON"),
            None
        );
    }

    #[test]
    fn ccusage_empty_blocks_degrade_to_none() {
        assert_eq!(
            parse_ccusage_output(r#"{"blocks":[]}"#, ccusage_now()).expect("valid ccusage JSON"),
            None
        );
    }

    #[test]
    fn rejects_malformed_ccusage_json() {
        assert!(parse_ccusage_output("not json", ccusage_now()).is_err());
    }

    #[test]
    fn rejects_ccusage_active_block_without_projection() {
        assert!(parse_ccusage_output(
            r#"{"blocks":[{"isActive":true,"endTime":"2026-08-06T14:00:00.000Z","costUSD":56.68}]}"#,
            ccusage_now(),
        )
        .is_err());
    }

    #[test]
    fn parses_codex_five_hour_and_weekly_windows() {
        let usage = parse_codex_record(&codex_record(
            r#"{"primary":{"used_percent":12.5,"window_minutes":300,"resets_at":1786537158},"secondary":{"used_percent":87.0,"window_minutes":10080,"resets_at":1786617158}}"#,
        ))
        .expect("valid Codex usage record");

        assert_eq!(
            usage
                .windows
                .iter()
                .map(|window| window.window_minutes)
                .collect::<Vec<_>>(),
            vec![300, 10080]
        );
    }

    #[test]
    fn parses_codex_record_without_credits() {
        let usage = parse_codex_record(&codex_record(
            r#"{"primary":{"used_percent":12.5,"window_minutes":300,"resets_at":1786537158}}"#,
        ))
        .expect("valid Codex usage record");

        assert_eq!(usage.credits, None);
    }

    #[test]
    fn expiry_filter_removes_codex_credits_with_expired_windows() {
        let usage = ProviderUsage {
            windows: vec![UsageWindow {
                used_percent: 31.0,
                window_minutes: 10080,
                resets_at: ccusage_now() - 1,
            }],
            credits: Some(12.5),
        };

        let filtered = filter_expired_codex_usage(usage, Some(ccusage_now()));

        assert!(filtered.windows.is_empty());
        assert_eq!(filtered.credits, None);
    }

    #[test]
    fn usage_cache_does_not_rescan_within_refresh_interval() {
        let cache = UsageCache::new(Duration::from_secs(60));
        let scans = AtomicUsize::new(0);
        let started = Instant::now();
        let loader = || {
            scans.fetch_add(1, Ordering::Relaxed);
            UsageRefresh::default()
        };

        cache.refresh_if_due(started, loader);
        cache.refresh_if_due(started + Duration::from_secs(59), || {
            scans.fetch_add(1, Ordering::Relaxed);
            UsageRefresh::default()
        });
        assert_eq!(scans.load(Ordering::Relaxed), 1);

        cache.refresh_if_due(started + Duration::from_secs(60), || {
            scans.fetch_add(1, Ordering::Relaxed);
            UsageRefresh::default()
        });
        assert_eq!(scans.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn usage_refresh_replaces_fresh_data() {
        let now = ccusage_now();
        let previous = populated_usage_snapshot(now + 3600);
        let refreshed = merge_usage_refresh(
            &previous,
            UsageRefresh {
                codex: ProviderRefresh::Fresh(ProviderUsage {
                    windows: vec![UsageWindow {
                        used_percent: 12.0,
                        window_minutes: 300,
                        resets_at: now + 7200,
                    }],
                    credits: Some(8.5),
                }),
                claude: ProviderRefresh::Fresh(ClaudeUsage {
                    five_hour: Some(ClaudeUsageWindow {
                        cost_usd: 1.23,
                        remaining_minutes: Some(240),
                        resets_at: now + 7200,
                    }),
                }),
            },
            Some(now),
        );

        assert_eq!(refreshed.codex.windows[0].used_percent, 12.0);
        assert_eq!(refreshed.codex.credits, Some(8.5));
        assert_eq!(refreshed.claude.five_hour.unwrap().cost_usd, 1.23);
    }

    #[test]
    fn usage_refresh_clears_each_provider_when_refresh_succeeds_without_active_data() {
        let now = ccusage_now();
        let previous = populated_usage_snapshot(now + 3600);

        let refreshed = merge_usage_refresh(
            &previous,
            UsageRefresh {
                codex: ProviderRefresh::Empty,
                claude: ProviderRefresh::Empty,
            },
            Some(now),
        );

        assert_eq!(refreshed, UsageSnapshot::default());
    }

    #[test]
    fn usage_refresh_retains_failed_provider_data_until_its_deadline() {
        let now = ccusage_now();
        let previous = populated_usage_snapshot(now + 3600);

        let refreshed = merge_usage_refresh(
            &previous,
            UsageRefresh {
                codex: ProviderRefresh::Failed,
                claude: ProviderRefresh::Failed,
            },
            Some(now),
        );

        assert_eq!(refreshed, previous);
    }

    #[test]
    fn usage_refresh_drops_failed_provider_data_after_its_deadline() {
        let now = ccusage_now();
        let previous = populated_usage_snapshot(now - 1);

        let refreshed = merge_usage_refresh(
            &previous,
            UsageRefresh {
                codex: ProviderRefresh::Failed,
                claude: ProviderRefresh::Failed,
            },
            Some(now),
        );

        assert_eq!(refreshed, UsageSnapshot::default());
    }

    #[test]
    fn usage_cache_filters_expired_retained_data_on_idle_read() {
        let cache = UsageCache::new(Duration::from_secs(60));
        let now = ccusage_now();
        let started = Instant::now();
        assert!(cache.begin_refresh(started));
        cache.finish_refresh(started, populated_usage_snapshot(now - 1));

        assert_eq!(cache.snapshot_at(Some(now)), UsageSnapshot::default());
    }

    #[test]
    fn usage_cache_purges_expired_snapshot_before_computing_wakeup_delay() {
        let cache = UsageCache::new(Duration::from_secs(60));
        let now = current_unix_timestamp().expect("current test timestamp");
        let started = Instant::now();
        assert!(cache.begin_refresh(started));
        cache.finish_refresh(started, populated_usage_snapshot(now - 1));

        let delay = cache.next_wakeup_delay(started);

        let state = cache.state.lock().expect("usage cache state");
        assert_eq!(state.snapshot, UsageSnapshot::default());
        assert!(delay >= MIN_USAGE_WAKEUP_DELAY);
    }

    #[test]
    fn usage_cache_publishes_a_snapshot_and_requests_a_render() {
        let cache = UsageCache::new(Duration::from_secs(60));
        let started = Instant::now();
        assert!(cache.begin_refresh(started));
        let render_notify = Notify::new();
        let render_dirty = RenderSignal::new();
        let snapshot = codex_snapshot_with_credits();

        cache.publish_refresh(started, snapshot.clone(), &render_notify, &render_dirty);

        assert_eq!(cache.snapshot(), snapshot);
        assert!(render_dirty.is_pending());
    }

    #[test]
    fn usage_cache_keeps_the_previous_snapshot_when_refresh_is_cancelled() {
        let cache = UsageCache::new(Duration::from_secs(60));
        let started = Instant::now();
        let snapshot = codex_snapshot_with_credits();
        assert!(cache.begin_refresh(started));
        cache.finish_refresh(started, snapshot.clone());
        assert!(cache.begin_refresh(started + Duration::from_secs(60)));
        cache.cancel_refresh(started + Duration::from_secs(60));

        assert_eq!(cache.snapshot(), snapshot);
    }

    #[test]
    fn usage_rows_fit_minimum_and_full_panel_widths() {
        let window = UsageWindow {
            used_percent: 31.0,
            window_minutes: 10080,
            resets_at: 11 * 3_600 + 59 * 60,
        };
        let minimum_panel = panel_width_for_main(INFO_PANEL_MIN_MAIN_WIDTH + INFO_PANEL_MIN_WIDTH)
            .expect("minimum panel width");
        let full_panel = panel_width_for_main(INFO_PANEL_MIN_MAIN_WIDTH + INFO_PANEL_WIDTH)
            .expect("full panel width");
        let minimum_inner = usize::from(
            ratatui::widgets::Block::default()
                .borders(ratatui::widgets::Borders::ALL)
                .inner(Rect::new(0, 0, minimum_panel, 1))
                .width,
        );
        let full_inner = usize::from(
            ratatui::widgets::Block::default()
                .borders(ratatui::widgets::Borders::ALL)
                .inner(Rect::new(0, 0, full_panel, 1))
                .width,
        );

        assert_eq!(minimum_panel, INFO_PANEL_MIN_WIDTH);
        assert_eq!(full_panel, INFO_PANEL_WIDTH);
        assert_eq!(
            usage_window_text("week", &window, minimum_inner),
            "week 69% · 31% · 11:59Z"
        );
        assert_eq!(
            usage_window_text("week", &window, full_inner),
            "week 69% left · 31% used · 11:59Z"
        );
        assert!(display_width(&usage_window_text("week", &window, minimum_inner)) <= minimum_inner);
        assert!(display_width(&usage_window_text("week", &window, full_inner)) <= full_inner);

        let claude_window = ClaudeUsageWindow {
            cost_usd: 56.68,
            remaining_minutes: Some(66),
            resets_at: 14 * 3_600,
        };
        assert_eq!(
            claude_usage_text(&claude_window, minimum_inner),
            "5h $56.68 · 66m · 14:00Z"
        );
        assert_eq!(
            claude_usage_text(&claude_window, full_inner),
            "5h $56.68 · 66m left · 14:00Z"
        );
        assert!(display_width(&claude_usage_text(&claude_window, minimum_inner)) <= minimum_inner);
        assert!(display_width(&claude_usage_text(&claude_window, full_inner)) <= full_inner);
        assert_eq!(credits_text(1927.95, minimum_inner), "credits: 1927.95");
        assert_eq!(credits_text(1_000_000.0, 12), "credits: —");
    }

    #[test]
    fn usage_rows_keep_quota_figures_at_narrow_realistic_widths() {
        let claude_window = ClaudeUsageWindow {
            cost_usd: 1234.56,
            remaining_minutes: Some(MAX_CLAUDE_REMAINING_MINUTES),
            resets_at: parse_utc_timestamp("2026-08-06T23:59:00Z").expect("valid reset"),
        };
        let codex_window = UsageWindow {
            used_percent: 99.6,
            window_minutes: 10_080,
            resets_at: claude_window.resets_at,
        };

        assert_eq!(
            claude_remaining_text(Some(MAX_CLAUDE_REMAINING_MINUTES)),
            "1d"
        );
        assert_eq!(claude_remaining_text(Some(960)), "16h");
        for panel_width in [26usize, 36] {
            let inner_width = panel_width - 2;
            let claude = claude_usage_text(&claude_window, inner_width);
            assert!(
                display_width(&claude) <= inner_width,
                "{panel_width}: {claude}"
            );
            assert!(claude.contains("$1235") || claude.contains("$1234.56"));
            assert!(claude.contains("1d") || claude.contains("1440m"));

            let codex = usage_window_text("week", &codex_window, inner_width);
            assert!(
                display_width(&codex) <= inner_width,
                "{panel_width}: {codex}"
            );
            assert!(codex.contains("100%"), "{panel_width}: {codex}");
            assert!(codex.contains("23:59Z"), "{panel_width}: {codex}");
        }
    }

    #[test]
    fn claude_usage_width_fallback_preserves_quota_for_adversarial_numbers() {
        let invalid_remaining = parse_ccusage_output(
            r#"{"blocks":[{"isActive":true,"endTime":"2026-08-06T14:00:00.000Z","costUSD":1000000,"projection":{"remainingMinutes":18446744073709551615}}]}"#,
            ccusage_now(),
        )
        .expect("valid ccusage JSON")
        .expect("active ccusage block")
        .five_hour
        .expect("five-hour usage");
        let largest_plausible = ClaudeUsageWindow {
            cost_usd: MAX_USAGE_AMOUNT,
            remaining_minutes: Some(MAX_CLAUDE_REMAINING_MINUTES),
            resets_at: parse_utc_timestamp("2026-08-06T23:59:00Z").expect("valid reset"),
        };

        for width in [24, 34] {
            for window in [&invalid_remaining, &largest_plausible] {
                let rendered = claude_usage_text(window, width);
                assert!(
                    display_width(&rendered) <= width,
                    "width {width}: {rendered}"
                );
                assert!(rendered.contains("$1000000"), "width {width}: {rendered}");
                if window.remaining_minutes.is_some() {
                    assert!(
                        rendered.contains("1d") || rendered.contains("1440m"),
                        "width {width}: {rendered}"
                    );
                } else {
                    assert!(rendered.contains('—'), "width {width}: {rendered}");
                }
            }
        }
    }

    #[test]
    fn info_panel_link_rows_follow_shared_candidate_order() {
        let mut app = AppState::test_new();
        app.workspaces
            .push(crate::workspace::Workspace::test_new("one"));
        app.active = Some(0);
        app.ensure_test_terminals();
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].terminal_id(pane_id).cloned().unwrap();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .apply_manual_work_context_patch(PaneWorkContextPatch {
                ticket_ids: Some(vec!["MAT-1".into()]),
                pr_urls: Some(vec!["https://github.com/o/r/pull/2".into()]),
                ..Default::default()
            })
            .unwrap();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .replace_hook_work_context(crate::work_context::PaneWorkContext {
                preview_urls: vec!["https://preview.vercel.app".into()],
                ..Default::default()
            })
            .unwrap();

        let area = Rect::new(0, 0, 50, 12);
        let rows = compute_link_rows(&app, area);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].copy_value, "MAT-1");
        assert_eq!(rows[1].copy_value, "https://github.com/o/r/pull/2");
        assert_eq!(rows[2].copy_value, "https://preview.vercel.app");

        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_info_panel(&app, frame, area, None))
            .unwrap();
        for (row, label) in rows.iter().zip([
            "MAT-1",
            "https://github.com/o/r/pull/2",
            "https://preview.vercel.app",
        ]) {
            let rendered = (area.x..area.x + area.width)
                .map(|x| terminal.backend().buffer()[(x, row.rect.y)].symbol())
                .collect::<String>();
            assert!(rendered.contains(label), "row {}: {rendered}", row.rect.y);
        }
    }
}
