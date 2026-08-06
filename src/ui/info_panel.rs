use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
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
const USAGE_WINDOW_MINUTES: [(u64, &str); 2] = [(300, "5h"), (10080, "week")];

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
    claude: ProviderUsage,
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
    number.is_finite().then_some(number)
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

fn recent_jsonl_files(root: &Path, max_files: usize) -> Vec<PathBuf> {
    if max_files == 0 {
        return Vec::new();
    }

    let root_mtime = fs::metadata(root)
        .and_then(|metadata| metadata.modified())
        .unwrap_or(UNIX_EPOCH);
    let mut directories = vec![(root_mtime, root.to_path_buf(), 0usize)];
    let mut visited_directories = 0usize;
    let mut visited_entries = 0usize;
    let mut files = Vec::new();

    while let Some((_, directory, depth)) = directories.pop() {
        if visited_directories >= MAX_USAGE_DIRECTORIES || visited_entries >= MAX_USAGE_ENTRIES {
            break;
        }
        visited_directories = visited_directories.saturating_add(1);

        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        let remaining_entries = MAX_USAGE_ENTRIES.saturating_sub(visited_entries);
        let mut child_directories = Vec::new();
        for entry in entries.flatten().take(remaining_entries) {
            visited_entries = visited_entries.saturating_add(1);
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
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
                files.sort_unstable_by(|left, right| right.0.cmp(&left.0));
                files.truncate(max_files);
            }
        }
        child_directories.sort_unstable_by(|left, right| right.0.cmp(&left.0));
        directories.extend(child_directories.into_iter().rev());
    }

    files.into_iter().map(|(_, path)| path).collect()
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

fn latest_codex_usage(path: &Path) -> Option<ProviderUsage> {
    let contents = read_file_tail(path)?;
    contents.lines().filter_map(parse_codex_record).last()
}

fn home_path(directory: &str) -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .map(|home| home.join(directory))
}

fn load_codex_usage() -> ProviderUsage {
    let Some(root) = home_path(".codex/sessions") else {
        return ProviderUsage::default();
    };
    recent_jsonl_files(&root, MAX_USAGE_FILES)
        .into_iter()
        .find_map(|path| latest_codex_usage(&path))
        .unwrap_or_default()
}

fn load_usage_snapshot() -> Option<UsageSnapshot> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok());
    let mut codex = load_codex_usage();
    if let Some(now) = now {
        codex.windows.retain(|window| window.resets_at > now);
    } else {
        codex.windows.clear();
    }
    Some(UsageSnapshot {
        codex,
        // Claude Code's local project logs on this machine do not contain a
        // usable quota field, and no non-credential local source is available.
        claude: ProviderUsage::default(),
    })
}

struct UsageCacheState {
    snapshot: UsageSnapshot,
    last_refresh: Option<Instant>,
    refresh_in_progress: bool,
}

struct UsageCache {
    state: Mutex<UsageCacheState>,
    refresh_interval: Duration,
}

impl UsageCache {
    fn new(refresh_interval: Duration) -> Self {
        Self {
            state: Mutex::new(UsageCacheState {
                snapshot: UsageSnapshot::default(),
                last_refresh: None,
                refresh_in_progress: false,
            }),
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
        state.refresh_in_progress = false;
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
        state.refresh_in_progress = false;
    }

    fn snapshot(&self) -> UsageSnapshot {
        self.state
            .lock()
            .map(|state| state.snapshot.clone())
            .unwrap_or_default()
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
                let Some(snapshot) = load_usage_snapshot() else {
                    cache_for_thread.cancel_refresh(now);
                    return;
                };
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
        F: FnOnce() -> UsageSnapshot,
    {
        if self.begin_refresh(now) {
            let snapshot = loader();
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
        "{window_label} {left:.0}% left · {:.0}% used",
        window.used_percent
    );
    if display_width(&compact) <= width {
        return compact;
    }

    format!("{window_label} {left:.0}% left")
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

    let providers = [("CODEX", &usage.codex), ("CLAUDE CODE", &usage.claude)];
    for (provider_index, (label, provider)) in providers.into_iter().enumerate() {
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
        for (window_index, (minutes, window_label)) in USAGE_WINDOW_MINUTES.iter().enumerate() {
            let text = usage_window(provider, *minutes)
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
        if provider_index == 0 {
            render_usage_line(
                frame,
                layout.line(provider_start.saturating_add(3)),
                provider
                    .credits
                    .as_ref()
                    .map(|balance| format!("credits: {balance:.2}"))
                    .unwrap_or_else(|| "credits: —".to_string()),
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
    fn usage_cache_does_not_rescan_within_refresh_interval() {
        let cache = UsageCache::new(Duration::from_secs(60));
        let scans = AtomicUsize::new(0);
        let started = Instant::now();
        let loader = || {
            scans.fetch_add(1, Ordering::Relaxed);
            UsageSnapshot::default()
        };

        cache.refresh_if_due(started, loader);
        cache.refresh_if_due(started + Duration::from_secs(59), || {
            scans.fetch_add(1, Ordering::Relaxed);
            UsageSnapshot::default()
        });
        assert_eq!(scans.load(Ordering::Relaxed), 1);

        cache.refresh_if_due(started + Duration::from_secs(60), || {
            scans.fetch_add(1, Ordering::Relaxed);
            UsageSnapshot::default()
        });
        assert_eq!(scans.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn usage_cache_publishes_a_snapshot_and_requests_a_render() {
        let cache = UsageCache::new(Duration::from_secs(60));
        let started = Instant::now();
        assert!(cache.begin_refresh(started));
        let render_notify = Notify::new();
        let render_dirty = RenderSignal::new();
        let snapshot = UsageSnapshot {
            codex: ProviderUsage {
                credits: Some(12.5),
                ..ProviderUsage::default()
            },
            ..UsageSnapshot::default()
        };

        cache.publish_refresh(started, snapshot.clone(), &render_notify, &render_dirty);

        assert_eq!(cache.snapshot(), snapshot);
        assert!(render_dirty.is_pending());
    }

    #[test]
    fn usage_cache_keeps_the_previous_snapshot_when_refresh_is_cancelled() {
        let cache = UsageCache::new(Duration::from_secs(60));
        let started = Instant::now();
        let snapshot = UsageSnapshot {
            codex: ProviderUsage {
                credits: Some(12.5),
                ..ProviderUsage::default()
            },
            ..UsageSnapshot::default()
        };
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
            "week 69% left · 31% used"
        );
        assert_eq!(
            usage_window_text("week", &window, full_inner),
            "week 69% left · 31% used · 11:59Z"
        );
        assert!(display_width(&usage_window_text("week", &window, minimum_inner)) <= minimum_inner);
        assert!(display_width(&usage_window_text("week", &window, full_inner)) <= full_inner);
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
