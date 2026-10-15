//! Compact fleet-wide usage and resource rows beneath the workspace list.
//! Rendering consumes the poller's snapshots; collection never runs here.

use std::collections::BTreeMap;

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::{
    app::AppState,
    fleet::{HostObservation, HostState},
    provider_usage::{
        AccountUsage, ProviderAccountUsage, QuotaProvider, QuotaWindow, UsageUnavailable,
    },
};

use super::{display_width, truncate_end};

pub(crate) const USAGE_KEY: &str = "fleet-panel:usage";
pub(crate) const RESOURCES_KEY: &str = "fleet-panel:resources";
const FRESH_SECONDS: i64 = 120;

pub(crate) fn available(app: &AppState) -> bool {
    !app.fleet_snapshot.hosts.is_empty()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Section {
    Usage,
    Resources,
}

#[derive(Clone)]
pub(crate) struct Row {
    pub(crate) section: Option<Section>,
    pub(crate) text: String,
    pub(crate) color: Color,
    pub(crate) spans: Vec<(String, Color)>,
    pub(crate) tooltip: String,
    /// Account rows alone may scroll; notices and resource warnings stay visible.
    pinned: bool,
    /// Relative columns of compact account/machine tokens and their details.
    targets: Vec<(u16, u16, String)>,
}

impl Row {
    fn plain(text: impl Into<String>, color: Color, tooltip: impl Into<String>) -> Self {
        Self {
            section: None,
            text: text.into(),
            color,
            spans: Vec::new(),
            tooltip: tooltip.into(),
            pinned: false,
            targets: Vec::new(),
        }
    }
    fn styled(parts: Vec<(String, Color)>, tooltip: impl Into<String>) -> Self {
        let text = parts.iter().map(|(part, _)| part.as_str()).collect();
        Self {
            section: None,
            text,
            color: Color::Reset,
            spans: parts,
            tooltip: tooltip.into(),
            pinned: false,
            targets: Vec::new(),
        }
    }
}

fn provider_name(provider: QuotaProvider) -> &'static str {
    match provider {
        QuotaProvider::Claude => "Claude",
        QuotaProvider::Codex => "Codex",
        QuotaProvider::Kimi => "Kimi",
        QuotaProvider::Agy => "Antigravity",
        QuotaProvider::OpenCode => "OpenCode",
    }
}

fn provider_color(provider: QuotaProvider, app: &AppState) -> Color {
    match provider {
        QuotaProvider::Claude => crate::ui::icons::claude_color(&app.palette),
        QuotaProvider::Codex => crate::ui::icons::codex_color(&app.palette),
        QuotaProvider::Kimi => app.palette.teal,
        QuotaProvider::Agy => app.palette.blue,
        QuotaProvider::OpenCode => app.palette.mauve,
    }
}

fn valid_age(now: i64, observed: i64) -> bool {
    observed > 0 && now >= observed && now - observed <= FRESH_SECONDS
}

fn host_reachable(app: &AppState, host: &str) -> bool {
    app.fleet_snapshot
        .hosts
        .iter()
        .find(|candidate| candidate.name == host)
        .is_some_and(|candidate| candidate.state != HostState::Unreachable)
}

#[derive(Clone)]
struct Account {
    account: ProviderAccountUsage,
    source_host: String,
    source_time: i64,
    hosts: Vec<String>,
    fresh: bool,
    host_states: Vec<String>,
    five_source: Option<(QuotaWindow, String, i64)>,
    weekly_source: Option<(QuotaWindow, String, i64)>,
}

/// Only a provider-reported email is a cross-machine account identity. A
/// display label or host-local profile ID is never evidence of shared quota.
fn account_key(host: &str, account: &ProviderAccountUsage) -> (QuotaProvider, String) {
    let identity = account
        .usage
        .email
        .as_deref()
        .map(str::trim)
        .filter(|email| email.contains('@'))
        .map(|email| format!("email:{}", email.to_ascii_lowercase()))
        .unwrap_or_else(|| format!("host:{host}:{}", account.profile_id));
    (account.provider, identity)
}

fn account_has_valid_quota(usage: &AccountUsage) -> bool {
    usage.five_hour.is_some() || usage.seven_day.is_some()
}

fn account_ready(usage: &AccountUsage) -> bool {
    usage.unavailable.is_none()
        && !usage.stale
        && account_has_valid_quota(usage)
        && [usage.five_hour, usage.seven_day]
            .into_iter()
            .flatten()
            .all(|window| window.used_percent < 100)
}

fn account_exhausted(usage: &AccountUsage) -> bool {
    usage.unavailable.is_none()
        && !usage.stale
        && [usage.five_hour, usage.seven_day]
            .into_iter()
            .flatten()
            .any(|window| window.used_percent >= 100)
}

fn account_quota_unblocks_at(usage: &AccountUsage) -> Option<i64> {
    if !account_exhausted(usage) {
        return None;
    }
    [usage.five_hour, usage.seven_day]
        .into_iter()
        .flatten()
        .filter(|window| window.used_percent >= 100)
        .map(|window| window.resets_at)
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .max()
}

fn accounts(app: &AppState, now: i64) -> Vec<Account> {
    let mut by_identity = BTreeMap::<(QuotaProvider, String), Account>::new();
    let local = HostObservation {
        host: app.agent_host_name.clone(),
        sampled_unix: app.provider_usage.collected_unix.unwrap_or_default(),
        metrics: Default::default(),
        usage: app.provider_usage.clone(),
    };
    let mut sources = app.fleet_snapshot.observations.iter().collect::<Vec<_>>();
    if !app.fleet_snapshot.observations.iter().any(|observation| {
        app.fleet_snapshot
            .hosts
            .iter()
            .any(|host| host.local && host.name == observation.host)
    }) {
        sources.push(&local);
    }
    for observation in sources {
        let source_fresh = valid_age(now, observation.sampled_unix)
            && (observation.host == app.agent_host_name || host_reachable(app, &observation.host));
        for item in &observation.usage.accounts {
            let key = account_key(&observation.host, item);
            let entry = by_identity.entry(key).or_insert_with(|| Account {
                account: item.clone(),
                source_host: observation.host.clone(),
                source_time: observation.sampled_unix,
                hosts: Vec::new(),
                fresh: source_fresh,
                host_states: Vec::new(),
                five_source: None,
                weekly_source: None,
            });
            if !entry.hosts.contains(&observation.host) {
                entry.hosts.push(observation.host.clone());
            }
            let auth = if !source_fresh {
                "unknown (stale or unreachable)"
            } else if item.usage.unavailable == Some(UsageUnavailable::AuthExpired) {
                "expired"
            } else if account_ready(&item.usage) {
                "quota available; no recorded auth failure"
            } else if account_exhausted(&item.usage) {
                "quota exhausted; auth not independently verified"
            } else {
                "unknown"
            };
            entry
                .host_states
                .push(format!("{}: {auth}", observation.host));
            let old_valid = entry.fresh
                && account_has_valid_quota(&entry.account.usage)
                && !entry.account.usage.stale
                && entry.account.usage.unavailable.is_none();
            let new_valid = source_fresh
                && account_has_valid_quota(&item.usage)
                && !item.usage.stale
                && item.usage.unavailable.is_none();
            if new_valid {
                for (window, selected) in [
                    (item.usage.five_hour, &mut entry.five_source),
                    (item.usage.seven_day, &mut entry.weekly_source),
                ] {
                    if let Some(window) = window {
                        if selected
                            .as_ref()
                            .is_none_or(|(_, _, sampled)| observation.sampled_unix > *sampled)
                        {
                            *selected =
                                Some((window, observation.host.clone(), observation.sampled_unix));
                        }
                    }
                }
            }
            if (new_valid && !old_valid)
                || (new_valid == old_valid && observation.sampled_unix > entry.source_time)
            {
                entry.account = item.clone();
                entry.source_host = observation.host.clone();
                entry.source_time = observation.sampled_unix;
                entry.fresh = source_fresh;
            }
        }
    }
    for entry in by_identity.values_mut() {
        if let Some((window, _, _)) = &entry.five_source {
            entry.account.usage.five_hour = Some(*window);
        }
        if let Some((window, _, _)) = &entry.weekly_source {
            entry.account.usage.seven_day = Some(*window);
        }
    }
    by_identity.into_values().collect()
}

fn severity(usage: &AccountUsage, fresh: bool) -> Option<u8> {
    if usage.stale || !fresh || usage.unavailable.is_some() {
        return None;
    }
    usage.peak_percent()
}

fn severity_dot(value: Option<u8>) -> &'static str {
    if value.is_some() {
        "●"
    } else {
        "○"
    }
}

fn severity_color(value: Option<u8>, app: &AppState) -> Color {
    match value {
        Some(90..) => app.palette.red,
        Some(80..) => app.palette.yellow,
        Some(_) => app.palette.green,
        None => app.palette.overlay0,
    }
}

fn reset_label(window: Option<QuotaWindow>, now: i64) -> String {
    let Some(reset) = window.and_then(|window| window.resets_at) else {
        return "—".into();
    };
    let seconds = reset.saturating_sub(now);
    if seconds <= 0 {
        return "now".into();
    }
    if seconds > 7 * 86_400 {
        return ">7d".into();
    }
    if seconds >= 86_400 {
        return format!("{}d", ((seconds as f64 / 86_400.0).round() as i64).max(1));
    }
    if seconds >= 3_600 {
        return format!("{}h", (seconds + 3_599) / 3_600);
    }
    format!("{}m", (seconds + 59) / 60)
}

fn reset_exact(at: i64, now: i64) -> String {
    let remaining = at.saturating_sub(now).max(0);
    let duration = format!(
        "{}d {}h {}m",
        remaining / 86_400,
        remaining % 86_400 / 3_600,
        remaining % 3_600 / 60
    );
    let local = u64::try_from(at)
        .ok()
        .and_then(crate::platform::local_datetime_at)
        .and_then(|date| {
            let seconds = date.assume_utc().unix_timestamp().saturating_sub(at);
            let offset = time::UtcOffset::from_whole_seconds(i32::try_from(seconds).ok()?).ok()?;
            date.assume_offset(offset)
                .format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_else(|| "date unavailable".into());
    format!("{duration} · {local}")
}

fn meter(percent: u8, cells: usize) -> String {
    (0..cells)
        .map(|cell| {
            let start = cell * 100 / cells;
            let end = (cell + 1) * 100 / cells;
            let value = usize::from(percent);
            if value >= end {
                '█'
            } else if value > start {
                crate::ui::status::fill_glyph(((value - start) * 100 / (end - start)) as u8)
            } else {
                '░'
            }
        })
        .collect()
}

fn window_parts(
    name: &str,
    window: Option<QuotaWindow>,
    now: i64,
    app: &AppState,
    cells: usize,
) -> (String, Color) {
    match window {
        Some(window) => (
            format!(
                " {name}{}{}",
                meter(window.used_percent, cells),
                reset_label(Some(window), now)
            ),
            severity_color(Some(window.used_percent), app),
        ),
        None => (
            format!(" {name}{}—", "░".repeat(cells)),
            app.palette.overlay0,
        ),
    }
}

fn account_tip(account: &Account, now: i64) -> String {
    let usage = &account.account.usage;
    let mut lines = vec![
        format!(
            "{} · {}",
            provider_name(account.account.provider),
            account.account.label
        ),
        format!("Profile: {}", account.account.profile_id),
    ];
    if let Some(email) = &usage.email {
        lines.push(format!("Account: {email}"));
    }
    if account.account.provider == QuotaProvider::Codex
        && usage.unavailable != Some(UsageUnavailable::AuthExpired)
    {
        lines.push("Login expiry: not reported by current Codex collector".into());
    }
    for (name, window, source) in [
        ("5h", usage.five_hour, &account.five_source),
        ("weekly", usage.seven_day, &account.weekly_source),
    ] {
        if let Some(window) = window {
            let provenance = source.as_ref().map_or_else(
                || account.source_host.clone(),
                |(_, host, sampled)| {
                    format!("{host}, sampled {}s ago", now.saturating_sub(*sampled))
                },
            );
            lines.push(format!(
                "{name}: {}% used · {}% remaining · reset {} · {provenance}",
                window.used_percent,
                100_u8.saturating_sub(window.used_percent),
                window
                    .resets_at
                    .map_or("not reported".into(), |at| reset_exact(at, now))
            ));
        } else {
            lines.push(format!("{name}: not reported"));
        }
    }
    if let Some(credits) = usage.credits {
        lines.push(format!("Credits: {credits}"));
    }
    if let Some(cost) = usage.cost_usd {
        lines.push(format!("5h cost: ${cost:.2}"));
    }
    if let Some(minutes) = usage.remaining_minutes {
        lines.push(format!("5h remaining time: {minutes}m"));
    }
    if let Some(reason) = usage.unavailable {
        lines.push(format!("Unavailable: {reason:?}"));
    }
    lines.push("Plan: not reported".into());
    lines.push(format!(
        "Source: {} · sampled {}s ago · {}",
        account.source_host,
        now.saturating_sub(account.source_time),
        if usage.stale || !account.fresh {
            "stale/unreachable"
        } else {
            "fresh"
        }
    ));
    lines.push(format!("Observed on: {}", account.hosts.join(", ")));
    lines.extend(account.host_states.iter().cloned());
    lines.join("\n")
}

fn login_rows(app: &AppState, now: i64) -> Vec<Row> {
    let mut rows = Vec::new();
    for observation in &app.fleet_snapshot.observations {
        if !valid_age(now, observation.sampled_unix) || !host_reachable(app, &observation.host) {
            continue;
        }
        let mut providers = BTreeMap::<QuotaProvider, (usize, usize, usize, Option<i64>)>::new();
        for account in &observation.usage.accounts {
            let state = providers.entry(account.provider).or_default();
            if account.usage.unavailable == Some(UsageUnavailable::AuthExpired) {
                state.0 += 1;
            } else if account_ready(&account.usage) {
                state.1 += 1;
            } else if account_exhausted(&account.usage) {
                state.2 += 1;
                if let Some(unblocks) = account_quota_unblocks_at(&account.usage) {
                    state.3 = Some(state.3.map_or(unblocks, |current| current.min(unblocks)));
                }
            }
        }
        for (provider, (expired, ready, exhausted, blocked_until)) in providers {
            if expired == 0 && exhausted == 0 {
                continue;
            }
            let stop = expired > 0 && ready == 0 && exhausted == 0;
            let mark = if stop {
                "⛔"
            } else if expired > 0 {
                "!"
            } else {
                "⌛"
            };
            let icon = crate::ui::icons::usage_label(provider, app.nerd_font);
            let color = if stop {
                app.palette.red
            } else {
                app.palette.yellow
            };
            rows.push(Row::styled(vec![(format!(" {icon}"), provider_color(provider, app)), (format!(" {mark} {}", observation.host), color)],
                format!("{} · {}\n{} expired · {} quota-available · {} quota-exhausted profile(s)\n{}", provider_name(provider), observation.host, expired, ready, exhausted, if stop { "Login required".into() } else if expired > 0 && ready > 0 { "Another profile has quota and no recorded auth failure; expired login needs attention".into() } else if expired > 0 { "Expired login needs attention; exhausted profiles may resume after quota reset".into() } else { format!("Quota exhausted; earliest reported reset {}", blocked_until.map_or("unknown".into(), |at| reset_label(Some(QuotaWindow { used_percent: 100, resets_at: Some(at) }), now))) })));
        }
    }
    rows
}

fn metric_percent(observation: &HostObservation) -> [Option<u8>; 3] {
    let metrics = &observation.metrics;
    [
        metrics.cpu_percent,
        crate::ui::status::memory_percent(metrics),
        metrics.disk_percent,
    ]
}

fn resource_tip(
    host: &str,
    observation: Option<&HostObservation>,
    now: i64,
    reachable: bool,
) -> String {
    let Some(observation) = observation else {
        return format!("{host} · no resource sample available");
    };
    let m = &observation.metrics;
    let [cpu, mem, disk] = metric_percent(observation);
    let value =
        |percent: Option<u8>| percent.map_or("unknown".into(), |percent| format!("{percent}%"));
    let gib =
        |amount: Option<f32>| amount.map_or("unknown".into(), |amount| format!("{amount:.1} GiB"));
    let partial = [cpu, mem, disk].iter().any(Option::is_none);
    let worst = [("CPU", cpu), ("memory", mem), ("disk", disk)]
        .into_iter()
        .filter_map(|(name, value)| value.map(|value| (name, value)))
        .max_by_key(|(_, value)| *value);
    let reason = match worst {
        Some((name, value)) if value >= 90 => format!("Critical: {name} at {value}%"),
        Some((name, value)) if value >= 80 => format!("Warning: {name} at {value}%"),
        Some(_) if partial => "Partial: at least one metric is unknown".into(),
        Some(_) => "Healthy: all reported metrics below 80%".into(),
        None => "Unknown: no valid metrics".into(),
    };
    format!("{host} · {}\n{reason}\nCPU: {} · {} cores\nMemory: {} · {} used / {} total · {} available\nDisk: {} · {} used / {} total · {} free · root/data volume\nSampled {}s ago · {}{}", if reachable { "reachable" } else { "unreachable" }, value(cpu), m.cpu_cores.map_or("unknown".into(), |cores| cores.to_string()), value(mem), gib(m.mem_used_gib), gib(m.mem_total_gib), gib(m.mem_total_gib.zip(m.mem_used_gib).map(|(total, used)| total - used)), value(disk), gib(m.disk_used_gib), gib(m.disk_total_gib), gib(m.disk_total_gib.zip(m.disk_used_gib).map(|(total, used)| total - used)), now.saturating_sub(observation.sampled_unix), if valid_age(now, observation.sampled_unix) { "fresh" } else { "stale" }, if partial { " · partial metrics" } else { "" })
}

pub(crate) fn rows(app: &AppState, width: u16) -> Vec<Row> {
    let now = app
        .status_now_unix
        .unwrap_or(app.view_observed_unix_s as i64);
    let mut rows = Vec::new();
    let usage_collapsed = app.collapsed_sidebar_groups.contains(USAGE_KEY);
    let resources_collapsed = app.collapsed_sidebar_groups.contains(RESOURCES_KEY);
    let accounts = accounts(app, now);
    let mut usage_header = Row::styled(
        vec![(
            format!(
                "{} {}",
                if usage_collapsed { '▸' } else { '▾' },
                if app.nerd_font { "󰓅" } else { "Usage" }
            ),
            app.palette.overlay1,
        )],
        format!(
            "Fleet usage · {} distinct account(s) · {}",
            accounts.len(),
            if usage_collapsed {
                "click to expand"
            } else {
                "click to collapse"
            }
        ),
    );
    usage_header.section = Some(Section::Usage);
    usage_header.pinned = true;
    if usage_collapsed {
        let mut compact = vec![usage_header];
        for account in &accounts {
            let value = severity(&account.account.usage, account.fresh);
            let token = format!(
                " {}{}",
                crate::ui::icons::usage_label(account.account.provider, app.nerd_font),
                severity_dot(value)
            );
            if display_width(&compact.last().unwrap().text) + display_width(&token) > width as usize
            {
                compact.push(Row::styled(
                    vec![("  ".into(), app.palette.overlay1)],
                    "Fleet usage · hover an account for details",
                ));
            }
            let line = compact.last_mut().unwrap();
            line.targets.push((
                display_width(&line.text) as u16,
                display_width(&token) as u16,
                account_tip(account, now),
            ));
            line.spans.push((
                format!(
                    " {}",
                    crate::ui::icons::usage_label(account.account.provider, app.nerd_font)
                ),
                provider_color(account.account.provider, app),
            ));
            line.spans
                .push((severity_dot(value).into(), severity_color(value, app)));
            line.text.push_str(&token);
        }
        rows.extend(compact);
    } else {
        if app.nerd_font {
            usage_header.text.push_str(" Usage");
            usage_header
                .spans
                .push((" Usage".into(), app.palette.overlay1));
        }
        rows.push(usage_header);
    }
    if !usage_collapsed {
        let mut duplicate_labels = BTreeMap::<(QuotaProvider, String), usize>::new();
        for account in &accounts {
            let count = duplicate_labels
                .entry((account.account.provider, account.account.label.clone()))
                .or_default();
            *count += 1;
            let mut label = account
                .account
                .usage
                .account
                .as_deref()
                .unwrap_or(&account.account.label)
                .to_string();
            if *count > 1 {
                label = format!("{}#{count}", truncate_end(&label, 1));
            }
            let icon = crate::ui::icons::usage_label(account.account.provider, app.nerd_font);
            let value = severity(&account.account.usage, account.fresh);
            let dual = matches!(
                account.account.provider,
                QuotaProvider::Claude | QuotaProvider::Codex
            );
            let narrow = width < 27;
            let cells = if narrow { 1 } else { 3 };
            let five = window_parts(
                if narrow { "5" } else { "5h" },
                account.account.usage.five_hour,
                now,
                app,
                cells,
            );
            let weekly = dual.then(|| {
                window_parts(
                    if narrow { "W" } else { "7d" },
                    account.account.usage.seven_day,
                    now,
                    app,
                    cells,
                )
            });
            let fixed = display_width(icon)
                + 2
                + display_width(&five.0)
                + weekly.as_ref().map_or(0, |part| display_width(&part.0));
            let label_budget = usize::from(width)
                .saturating_sub(fixed)
                .clamp(1, if narrow { 3 } else { 6 });
            let mut parts = vec![
                (
                    format!(" {icon}"),
                    provider_color(account.account.provider, app),
                ),
                (severity_dot(value).into(), severity_color(value, app)),
                (truncate_end(&label, label_budget), app.palette.text),
            ];
            parts.push(five);
            if let Some(weekly) = weekly {
                parts.push(weekly);
            }
            rows.push(Row::styled(parts, account_tip(account, now)));
        }
        if accounts.is_empty() {
            rows.push(Row::plain(
                " ○ Usage unavailable",
                app.palette.overlay0,
                "No fleet quota sample",
            ));
        }
    }
    rows.extend(login_rows(app, now).into_iter().map(|mut row| {
        row.pinned = true;
        row
    }));
    let hosts = &app.fleet_snapshot.hosts;
    let mut resource_header = Row::styled(
        vec![(
            format!(
                "{} {}",
                if resources_collapsed { '▸' } else { '▾' },
                if app.nerd_font { "󰍛" } else { "Resources" }
            ),
            app.palette.overlay1,
        )],
        format!(
            "Fleet resources · {} configured machine(s) · click to {}",
            hosts.len(),
            if resources_collapsed {
                "expand"
            } else {
                "collapse"
            }
        ),
    );
    resource_header.section = Some(Section::Resources);
    let mut resource_rows = Vec::new();
    let mut compact_resource_rows = Vec::new();
    for host in hosts {
        let fleet_observation = app
            .fleet_snapshot
            .observations
            .iter()
            .find(|sample| sample.host == host.name);
        // The local status sampler refreshes every few seconds; use its newer
        // sample while retaining the fleet poller's minute-scale remote data.
        let local_observation = if host.local {
            app.status_metrics
                .as_ref()
                .filter(|sample| !sample.is_stale_at(std::time::Instant::now()))
                .map(|sample| {
                    let mut observation =
                        fleet_observation
                            .cloned()
                            .unwrap_or_else(|| HostObservation {
                                host: host.name.clone(),
                                sampled_unix: now,
                                metrics: Default::default(),
                                usage: app.provider_usage.clone(),
                            });
                    observation.metrics = sample.metrics.clone();
                    observation.sampled_unix = now;
                    observation
                })
        } else {
            None
        };
        let observation = local_observation.as_ref().or(fleet_observation);
        let fresh = observation.is_some_and(|sample| valid_age(now, sample.sampled_unix))
            && host.state != HostState::Unreachable;
        let metrics = observation
            .filter(|_| fresh)
            .map(metric_percent)
            .unwrap_or([None; 3]);
        let severity = metrics.iter().flatten().max().copied();
        let complete = metrics.iter().all(Option::is_some);
        let known_warning = severity.is_some_and(|value| value >= 80);
        let healthy = fresh && complete;
        let color = if known_warning {
            severity_color(severity, app)
        } else if healthy {
            app.palette.green
        } else {
            app.palette.overlay0
        };
        let dot = if known_warning || healthy {
            "●"
        } else {
            "○"
        };
        let values = metrics.map(|value| value.map_or("—".into(), |v| format!("{v}%")));
        if resources_collapsed {
            let token = format!(" {dot}");
            let line = compact_resource_rows
                .last_mut()
                .unwrap_or(&mut resource_header);
            if display_width(&line.text) + display_width(&token) > width as usize {
                compact_resource_rows.push(Row::styled(
                    vec![("  ".into(), app.palette.overlay1)],
                    "Fleet resources",
                ));
            }
            let line = compact_resource_rows
                .last_mut()
                .unwrap_or(&mut resource_header);
            line.targets.push((
                display_width(&line.text) as u16,
                display_width(&token) as u16,
                resource_tip(
                    &host.name,
                    observation,
                    now,
                    host.state != HostState::Unreachable,
                ),
            ));
            line.spans.push((token.clone(), color));
            line.text.push_str(&token);
        } else {
            resource_rows.push(Row::styled(
                vec![
                    (format!(" {dot}"), color),
                    (
                        format!(
                            "{} {} {} {}{}",
                            host.name,
                            values[0],
                            values[1],
                            values[2],
                            if complete { "" } else { " ?" }
                        ),
                        app.palette.text,
                    ),
                ],
                resource_tip(
                    &host.name,
                    observation,
                    now,
                    host.state != HostState::Unreachable,
                ),
            ));
        }
    }
    if !resources_collapsed && app.nerd_font {
        resource_header.text.push_str(" Resources");
        resource_header
            .spans
            .push((" Resources".into(), app.palette.overlay1));
    }
    resource_header.pinned = true;
    rows.push(resource_header);
    rows.extend(compact_resource_rows.into_iter().map(|mut row| {
        row.pinned = true;
        row
    }));
    rows.extend(resource_rows.into_iter().map(|mut row| {
        row.pinned = true;
        row
    }));
    rows
}

/// Layout needs only cardinalities. Do not format tooltips, bars, or cloned
/// account rows while sizing the workspace list on each view pass.
fn row_count(app: &AppState, width: u16) -> usize {
    let mut identities = std::collections::BTreeSet::new();
    let has_local_observation = app.fleet_snapshot.observations.iter().any(|observation| {
        app.fleet_snapshot
            .hosts
            .iter()
            .any(|host| host.local && host.name == observation.host)
    });
    for observation in &app.fleet_snapshot.observations {
        for account in &observation.usage.accounts {
            identities.insert(account_key(&observation.host, account));
        }
    }
    if !has_local_observation {
        for account in &app.provider_usage.accounts {
            identities.insert(account_key(&app.agent_host_name, account));
        }
    }
    let now = app
        .status_now_unix
        .unwrap_or(app.view_observed_unix_s as i64);
    let notices = app
        .fleet_snapshot
        .observations
        .iter()
        .filter(|observation| {
            valid_age(now, observation.sampled_unix) && host_reachable(app, &observation.host)
        })
        .map(|observation| {
            observation
                .usage
                .accounts
                .iter()
                .filter(|account| {
                    account.usage.unavailable == Some(UsageUnavailable::AuthExpired)
                        || account_exhausted(&account.usage)
                })
                .map(|account| account.provider)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
        })
        .sum::<usize>();
    let usage_extra = if app.collapsed_sidebar_groups.contains(USAGE_KEY) {
        let mut lines = 0;
        let mut used = display_width(if app.nerd_font {
            "▸ 󰓅"
        } else {
            "▸ Usage"
        });
        for (provider, _) in &identities {
            let token =
                1 + display_width(crate::ui::icons::usage_label(*provider, app.nerd_font)) + 1;
            if used + token > width as usize {
                lines += 1;
                used = 2;
            }
            used += token;
        }
        lines
    } else {
        identities.len().max(1)
    };
    2 + notices + usage_extra + resource_extra(app, width)
}

fn resource_extra(app: &AppState, width: u16) -> usize {
    if app.collapsed_sidebar_groups.contains(RESOURCES_KEY) {
        let mut lines = 0;
        let mut used = display_width(if app.nerd_font {
            "▸ 󰍛"
        } else {
            "▸ Resources"
        });
        for _ in &app.fleet_snapshot.hosts {
            if used + 2 > width as usize {
                lines += 1;
                used = 2;
            }
            used += 2;
        }
        lines
    } else {
        app.fleet_snapshot.hosts.len()
    }
}

pub(crate) fn resources_visible(app: &AppState, sidebar: Rect) -> bool {
    if !available(app) {
        return false;
    }
    let panel = area(app, sidebar);
    usize::from(panel.height) >= 2 + resource_extra(app, panel.width)
}

fn visible_rows(app: &AppState, width: u16, height: u16) -> Vec<Row> {
    let all = rows(app, width);
    let height = usize::from(height);
    if height == 0 {
        return Vec::new();
    }
    if all.len() <= height {
        return all;
    }
    let pinned = all.iter().filter(|row| row.pinned).count();
    if pinned >= height {
        // Very small terminals cannot hold every section. Keep the fleet
        // header and the trailing resource rows rather than hiding resources
        // behind account rows.
        let mut fixed = all.into_iter().filter(|row| row.pinned).collect::<Vec<_>>();
        let first = fixed.remove(0);
        let tail = height.saturating_sub(1);
        return std::iter::once(first)
            .chain(
                fixed
                    .into_iter()
                    .rev()
                    .take(tail)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev(),
            )
            .collect();
    }
    let account_slots = height - pinned;
    let account_count = all.len() - pinned;
    let scroll = app.fleet_usage_scroll.min(account_count - account_slots);
    let mut account_index = 0;
    let mut visible = all
        .into_iter()
        .filter(|row| {
            if row.pinned {
                true
            } else {
                let visible = (scroll..scroll + account_slots).contains(&account_index);
                account_index += 1;
                visible
            }
        })
        .collect::<Vec<_>>();
    if let Some(header) = visible.first_mut() {
        header.tooltip.push_str(&format!(
            "\nWheel over this panel to view all {account_count} account row(s)"
        ));
    }
    visible
}

pub(crate) fn scroll(app: &mut AppState, col: u16, row: u16, delta: i8) -> bool {
    if row_at(app, col, row).is_none() {
        return false;
    }
    let area = area(app, app.view.sidebar_rect);
    let all = rows(app, area.width);
    let pinned = all.iter().filter(|item| item.pinned).count();
    let account_slots = usize::from(area.height).saturating_sub(pinned);
    let account_count = all.len().saturating_sub(pinned);
    let max_scroll = account_count.saturating_sub(account_slots);
    app.fleet_usage_scroll = if delta < 0 {
        app.fleet_usage_scroll.saturating_sub(1)
    } else {
        app.fleet_usage_scroll.saturating_add(1).min(max_scroll)
    };
    true
}

pub(crate) fn area(app: &AppState, sidebar: Rect) -> Rect {
    if app.sidebar_collapsed || !available(app) {
        return Rect::default();
    }
    let content = super::expanded_sidebar_content(sidebar);
    let host_rows = u16::from(super::sidebar_hosts_rect(app, sidebar).width > 0);
    let available = content
        .height
        .saturating_sub(host_rows)
        .saturating_sub(super::sidebar_workspace_floor(app));
    let height = u16::try_from(row_count(app, content.width))
        .unwrap_or(u16::MAX)
        .min(available);
    Rect::new(
        content.x,
        content.bottom().saturating_sub(host_rows + height),
        content.width,
        height,
    )
}

pub(crate) fn render(app: &AppState, frame: &mut Frame, sidebar: Rect) {
    let area = area(app, sidebar);
    for (index, row) in visible_rows(app, area.width, area.height)
        .into_iter()
        .take(area.height as usize)
        .enumerate()
    {
        let rect = Rect::new(area.x, area.y + index as u16, area.width, 1);
        if row.spans.is_empty() {
            let text = truncate_end(&row.text, rect.width as usize);
            frame.render_widget(
                Paragraph::new(text).style(Style::default().fg(row.color)),
                rect,
            );
        } else {
            let mut remaining = rect.width as usize;
            let mut spans = Vec::new();
            for (part, color) in row.spans {
                if remaining == 0 {
                    break;
                }
                let clipped = truncate_end(&part, remaining);
                remaining = remaining.saturating_sub(display_width(&clipped));
                spans.push(Span::styled(clipped, Style::default().fg(color)));
            }
            frame.render_widget(Paragraph::new(Line::from(spans)), rect);
        }
    }
}

pub(crate) fn row_at(app: &AppState, col: u16, row: u16) -> Option<usize> {
    let area = area(app, app.view.sidebar_rect);
    if col >= area.x && col < area.right() && row >= area.y && row < area.bottom() {
        Some(usize::from(row - area.y))
    } else {
        None
    }
}

pub(crate) fn token_at(app: &AppState, row_index: usize, col: u16) -> Option<usize> {
    let area = area(app, app.view.sidebar_rect);
    let relative = col.checked_sub(area.x)?;
    visible_rows(app, area.width, area.height)
        .get(row_index)?
        .targets
        .iter()
        .position(|(start, width, _)| relative >= *start && relative < start.saturating_add(*width))
}

pub(crate) fn token_target(
    app: &AppState,
    row_index: usize,
    token_index: usize,
) -> Option<(Rect, String)> {
    let area = area(app, app.view.sidebar_rect);
    if row_index >= area.height as usize {
        return None;
    }
    let row = visible_rows(app, area.width, area.height)
        .into_iter()
        .nth(row_index)?;
    let (start, width, tooltip) = row.targets.get(token_index)?;
    Some((
        Rect::new(area.x + *start, area.y + row_index as u16, *width, 1),
        tooltip.clone(),
    ))
}

pub(crate) fn row_target(app: &AppState, index: usize) -> Option<(Rect, String)> {
    let area = area(app, app.view.sidebar_rect);
    if index >= area.height as usize {
        return None;
    }
    let row = visible_rows(app, area.width, area.height)
        .into_iter()
        .nth(index)?;
    Some((
        Rect::new(area.x, area.y + index as u16, area.width, 1),
        row.tooltip,
    ))
}

pub(crate) fn toggle(app: &mut AppState, index: usize) -> bool {
    let panel = area(app, app.view.sidebar_rect);
    let key = match visible_rows(app, panel.width, panel.height)
        .get(index)
        .and_then(|row| row.section)
    {
        Some(Section::Usage) => USAGE_KEY,
        Some(Section::Resources) => RESOURCES_KEY,
        None => return false,
    };
    let collapsed = if app.collapsed_sidebar_groups.remove(key) {
        false
    } else {
        app.collapsed_sidebar_groups.insert(key.into());
        true
    };
    app.sidebar_group_collapsed_persistence_request = Some((key.into(), collapsed));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(percent: u8, now: i64) -> AccountUsage {
        AccountUsage {
            five_hour: Some(QuotaWindow {
                used_percent: percent,
                resets_at: Some(now + 3_600),
            }),
            seven_day: Some(QuotaWindow {
                used_percent: percent / 2,
                resets_at: Some(now + 5 * 86_400),
            }),
            ..Default::default()
        }
    }

    fn account(provider: QuotaProvider, label: &str, usage: AccountUsage) -> ProviderAccountUsage {
        ProviderAccountUsage {
            provider,
            profile_id: label.into(),
            label: label.into(),
            usage,
        }
    }

    fn host(name: &str, state: HostState) -> crate::fleet::HostSnapshot {
        crate::fleet::HostSnapshot {
            name: name.into(),
            target: name.into(),
            local: false,
            session: None,
            socket: None,
            state,
            version: None,
            protocol: None,
            error: None,
            remote_identity: None,
            sessions: None,
            reachable: state != HostState::Unreachable,
            last_seen_unix_ms: None,
            entries: Vec::new(),
        }
    }

    fn observation(host: &str, now: i64, accounts: Vec<ProviderAccountUsage>) -> HostObservation {
        let mut snapshot = crate::provider_usage::ProviderUsageSnapshot::with_primary_accounts(
            AccountUsage::default(),
            AccountUsage::default(),
            AccountUsage::default(),
        );
        snapshot.accounts = accounts;
        snapshot.collected_unix = Some(now);
        HostObservation {
            host: host.into(),
            sampled_unix: now,
            metrics: Default::default(),
            usage: snapshot,
        }
    }

    #[test]
    fn same_label_without_email_never_deduplicates_across_hosts() {
        let item = ProviderAccountUsage {
            provider: QuotaProvider::Claude,
            profile_id: "default".into(),
            label: "Max".into(),
            usage: AccountUsage::default(),
        };
        assert_ne!(account_key("ub1", &item), account_key("ub2", &item));
        let mut shared = item.clone();
        shared.usage.email = Some("A@Example.com".into());
        let mut same = shared.clone();
        same.usage.email = Some("a@example.com".into());
        assert_eq!(account_key("ub1", &shared), account_key("ub2", &same));
    }

    #[test]
    fn shared_account_merges_fresh_windows_independently_with_sources() {
        let now = 1_000_000;
        let mut app = AppState::test_new();
        app.status_now_unix = Some(now);
        for name in ["ub1", "ub2"] {
            app.fleet_snapshot
                .hosts
                .push(host(name, HostState::Reachable));
        }
        let mut five = AccountUsage {
            email: Some("shared@example.test".into()),
            ..Default::default()
        };
        five.five_hour = Some(QuotaWindow {
            used_percent: 84,
            resets_at: Some(now + 3_600),
        });
        let mut weekly = AccountUsage {
            email: Some("shared@example.test".into()),
            ..Default::default()
        };
        weekly.seven_day = Some(QuotaWindow {
            used_percent: 22,
            resets_at: Some(now + 86_400),
        });
        app.fleet_snapshot.observations.push(observation(
            "ub1",
            now - 20,
            vec![account(QuotaProvider::Claude, "one", five)],
        ));
        app.fleet_snapshot.observations.push(observation(
            "ub2",
            now - 10,
            vec![account(QuotaProvider::Claude, "two", weekly)],
        ));
        let merged = accounts(&app, now);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].account.usage.five_hour.unwrap().used_percent, 84);
        assert_eq!(merged[0].account.usage.seven_day.unwrap().used_percent, 22);
        let tip = account_tip(&merged[0], now);
        assert!(tip.contains("5h: 84% used"));
        assert!(tip.contains("weekly: 22% used"));
        assert!(tip.contains("ub1, sampled 20s ago"));
        assert!(tip.contains("ub2, sampled 10s ago"));
    }

    #[test]
    fn reset_rounding_and_missing_window() {
        let window = |at| {
            Some(QuotaWindow {
                used_percent: 50,
                resets_at: Some(at),
            })
        };
        assert_eq!(reset_label(None, 0), "—");
        assert_eq!(reset_label(window(59), 0), "1m");
        assert_eq!(reset_label(window(3601), 0), "2h");
        assert_eq!(reset_label(window(86_400), 0), "1d");
        assert_eq!(reset_label(window(8 * 86_400), 0), ">7d");
        assert_eq!(reset_label(window(7 * 86_400 + 1), 0), ">7d");
        assert!(reset_exact(3_600, 0).contains("1h 0m"));
        assert!(!reset_exact(3_600, 0).contains("Unix"));
    }

    #[test]
    fn compact_is_one_strip_per_section_and_expanded_accounts_have_both_bars() {
        let now = 1_000_000;
        let mut app = AppState::test_new();
        app.status_now_unix = Some(now);
        app.provider_usage = crate::provider_usage::ProviderUsageSnapshot::with_primary_accounts(
            usage(24, now),
            usage(82, now),
            usage(44, now),
        );
        app.provider_usage.collected_unix = Some(now);
        app.collapsed_sidebar_groups.insert(USAGE_KEY.into());
        app.collapsed_sidebar_groups.insert(RESOURCES_KEY.into());
        let compact = rows(&app, 40);
        assert_eq!(compact.len(), 2);
        assert_eq!(compact[0].section, Some(Section::Usage));
        assert_eq!(compact[1].section, Some(Section::Resources));
        assert_eq!(
            compact[0]
                .spans
                .iter()
                .filter(|(part, _)| part == "●")
                .count(),
            3
        );
        app.collapsed_sidebar_groups.remove(USAGE_KEY);
        let expanded = rows(&app, 40);
        assert_eq!(expanded.len(), 5);
        let claude = &expanded[1];
        assert!(claude.text.contains("5h"), "{}", claude.text);
        assert!(claude.text.contains("7d"), "{}", claude.text);
        assert!(claude
            .text
            .chars()
            .any(|ch| matches!(ch, '█' | '▁' | '▂' | '▃' | '▄' | '▅' | '▆' | '▇')));
        assert_eq!(
            claude.spans[0].1,
            provider_color(QuotaProvider::Claude, &app)
        );
        assert_eq!(claude.spans[1].1, app.palette.green);
        let narrow = rows(&app, 18);
        assert!(display_width(&narrow[1].text) <= 18, "{}", narrow[1].text);
    }

    #[test]
    fn stale_account_identity_remains_with_unknown_severity() {
        let now = 1_000_000;
        let mut app = AppState::test_new();
        app.status_now_unix = Some(now);
        app.fleet_snapshot
            .hosts
            .push(host("ub2", HostState::Unreachable));
        app.fleet_snapshot.observations.push(observation(
            "ub2",
            now - 300,
            vec![account(QuotaProvider::Claude, "Max", usage(90, now))],
        ));
        let result = rows(&app, 30);
        assert!(result
            .iter()
            .any(|row| row.text.contains("Max") && row.text.contains('○')));
        assert!(result
            .iter()
            .any(|row| row.tooltip.contains("stale/unreachable")));
    }

    #[test]
    fn login_notices_distinguish_expired_fallback_and_quota_reset() {
        let now = 1_000_000;
        let mut app = AppState::test_new();
        app.status_now_unix = Some(now);
        app.fleet_snapshot
            .hosts
            .push(host("ub2", HostState::Reachable));
        let expired = AccountUsage {
            unavailable: Some(UsageUnavailable::AuthExpired),
            ..Default::default()
        };
        let ready = usage(20, now);
        app.fleet_snapshot.observations.push(observation(
            "ub2",
            now,
            vec![
                account(QuotaProvider::Claude, "old", expired.clone()),
                account(QuotaProvider::Claude, "good", ready),
                account(QuotaProvider::Codex, "old", expired),
                account(
                    QuotaProvider::Kimi,
                    "limited",
                    AccountUsage {
                        five_hour: Some(QuotaWindow {
                            used_percent: 100,
                            resets_at: Some(now + 3_600),
                        }),
                        seven_day: Some(QuotaWindow {
                            used_percent: 100,
                            resets_at: Some(now + 8_000),
                        }),
                        ..Default::default()
                    },
                ),
            ],
        ));
        let notices = login_rows(&app, now);
        assert_eq!(notices.len(), 3);
        assert!(notices
            .iter()
            .any(|row| row.text.contains('!') && row.tooltip.contains("fallback has quota")));
        assert!(notices
            .iter()
            .any(|row| row.text.contains('⛔') && row.tooltip.contains("Login required")));
        assert!(notices
            .iter()
            .any(|row| row.text.contains('⌛') && row.tooltip.contains("Quota exhausted")));
        let limited = &app.fleet_snapshot.observations[0].usage.accounts[3].usage;
        assert_eq!(account_quota_unblocks_at(limited), Some(now + 8_000));
        assert!(!account_ready(limited));
    }

    #[test]
    fn partial_resource_warning_survives_unknown_metrics_and_stale_turns_gray() {
        let now = 1_000_000;
        let mut app = AppState::test_new();
        app.status_now_unix = Some(now);
        app.fleet_snapshot
            .hosts
            .push(host("ub2", HostState::Reachable));
        let mut sample = observation("ub2", now, Vec::new());
        sample.metrics.mem_used_gib = Some(8.4);
        sample.metrics.mem_total_gib = Some(10.0);
        app.fleet_snapshot.observations.push(sample);
        let result = rows(&app, 40);
        let resource = result
            .iter()
            .find(|row| row.text.contains("ub2") && row.tooltip.contains("CPU:"))
            .unwrap();
        assert_eq!(resource.spans[0].1, app.palette.yellow);
        assert!(resource.tooltip.contains("Warning: memory at 84%"));
        assert!(resource.tooltip.contains("partial metrics"));
        app.fleet_snapshot.observations[0].sampled_unix = now - 300;
        let stale = rows(&app, 40);
        let resource = stale
            .iter()
            .find(|row| row.text.contains("ub2") && row.tooltip.contains("CPU:"))
            .unwrap();
        assert_eq!(resource.spans[0].1, app.palette.overlay0);
        assert!(resource.text.contains('○'));
    }

    #[test]
    fn section_hit_targets_and_persisted_folds_fit_eighteen_columns() {
        let mut app = AppState::test_new();
        app.fleet_snapshot
            .hosts
            .push(host("ub1", HostState::Reachable));
        app.view.sidebar_rect = Rect::new(0, 0, 18, 30);
        app.collapsed_sidebar_groups.insert(USAGE_KEY.into());
        app.collapsed_sidebar_groups.insert(RESOURCES_KEY.into());
        let panel = area(&app, app.view.sidebar_rect);
        assert_eq!(panel.height, 2);
        assert_eq!(row_at(&app, 2, panel.y), Some(0));
        assert_eq!(row_at(&app, 2, panel.y.saturating_sub(1)), None);
        assert_eq!(row_at(&app, 2, panel.bottom()), None);
        assert!(row_target(&app, 0).unwrap().1.contains("Fleet usage"));
        assert!(toggle(&mut app, 0));
        assert_eq!(
            app.sidebar_group_collapsed_persistence_request,
            Some((USAGE_KEY.into(), false))
        );
        assert!(!app.collapsed_sidebar_groups.contains(USAGE_KEY));
    }

    #[test]
    fn compact_sections_wrap_many_accounts_and_hosts_without_losing_tooltips() {
        let now = 1_000_000;
        let mut app = AppState::test_new();
        app.status_now_unix = Some(now);
        app.collapsed_sidebar_groups.insert(USAGE_KEY.into());
        app.collapsed_sidebar_groups.insert(RESOURCES_KEY.into());
        for index in 0..9 {
            let name = format!("ub{index}");
            app.fleet_snapshot
                .hosts
                .push(host(&name, HostState::Reachable));
            app.fleet_snapshot.observations.push(observation(
                &name,
                now,
                vec![account(
                    QuotaProvider::Claude,
                    &format!("profile{index}"),
                    usage(20, now),
                )],
            ));
        }
        let compact = rows(&app, 17);
        assert!(compact.len() > 2);
        assert_eq!(compact.len(), row_count(&app, 17));
        assert!(compact.iter().all(|row| display_width(&row.text) <= 17));
        assert_eq!(
            compact
                .iter()
                .flat_map(|row| &row.targets)
                .filter(|(_, _, tip)| tip.contains("Profile: profile"))
                .count(),
            9
        );
        assert!(compact
            .iter()
            .flat_map(|row| &row.targets)
            .any(|(_, _, tip)| tip.contains("ub8 · reachable")));
    }

    #[test]
    fn short_viewport_scrolls_accounts_while_notices_and_machine_warnings_stay_visible() {
        let now = 1_000_000;
        let mut app = AppState::test_new();
        app.status_now_unix = Some(now);
        app.view.sidebar_rect = Rect::new(0, 0, 80, 24);
        app.fleet_snapshot
            .hosts
            .push(host("ub1", HostState::Reachable));
        app.fleet_snapshot
            .hosts
            .push(host("ub2", HostState::Reachable));
        let mut accounts = (0..17)
            .map(|index| {
                account(
                    QuotaProvider::Claude,
                    &format!("profile{index:02}"),
                    usage(20, now),
                )
            })
            .collect::<Vec<_>>();
        accounts[0].usage.unavailable = Some(UsageUnavailable::AuthExpired);
        app.fleet_snapshot
            .observations
            .push(observation("ub1", now, accounts));
        let mut remote = observation("ub2", now, Vec::new());
        remote.metrics.cpu_percent = Some(82);
        app.fleet_snapshot.observations.push(remote);

        let panel = area(&app, app.view.sidebar_rect);
        let before = visible_rows(&app, panel.width, panel.height);
        assert!(before.len() <= usize::from(panel.height));
        assert!(before[0].tooltip.contains("Wheel over this panel"));
        assert!(before
            .iter()
            .any(|row| row.tooltip.contains("Profile: profile00")));
        assert!(!before
            .iter()
            .any(|row| row.tooltip.contains("Profile: profile16")));
        assert!(before.iter().any(|row| row.text.contains("! ub1")));
        assert!(before
            .iter()
            .any(|row| row.tooltip.contains("ub2 · reachable") && row.text.contains("82%")));
        for _ in 0..20 {
            assert!(scroll(&mut app, panel.x, panel.y, 1));
        }
        let after = visible_rows(&app, panel.width, panel.height);
        assert!(after
            .iter()
            .any(|row| row.tooltip.contains("Profile: profile16")));
        assert!(after.iter().any(|row| row.text.contains("! ub1")));
        assert!(after
            .iter()
            .any(|row| row.tooltip.contains("ub2 · reachable") && row.text.contains("82%")));
        let last = after
            .iter()
            .position(|row| row.tooltip.contains("Profile: profile16"))
            .unwrap();
        let (hit, tip) = row_target(&app, last).unwrap();
        assert_eq!(hit.y, panel.y + last as u16);
        assert!(tip.contains("Profile: profile16"));
        let control = crate::ui::tooltip::hovered_control_at(&app, hit.x, hit.y).unwrap();
        assert_eq!(control, crate::app::state::ControlId::SidebarFleetRow(last));
        assert_eq!(
            crate::ui::tooltip::tooltip_target(&app, control).unwrap().1,
            tip
        );
    }

    #[test]
    fn collapsed_tokens_render_and_hover_individually_in_terminal_viewport() {
        use ratatui::{backend::TestBackend, Terminal};

        let now = 1_000_000;
        let mut app = AppState::test_new();
        app.status_now_unix = Some(now);
        app.view.sidebar_rect = Rect::new(0, 0, 25, 40);
        app.collapsed_sidebar_groups.insert(USAGE_KEY.into());
        app.collapsed_sidebar_groups.insert(RESOURCES_KEY.into());
        app.provider_usage.accounts = (0..7)
            .map(|index| {
                account(
                    QuotaProvider::Claude,
                    &format!("profile{index}"),
                    usage(20, now),
                )
            })
            .collect();
        app.provider_usage.collected_unix = Some(now);
        for index in 0..7 {
            let name = format!("ub{index}");
            app.fleet_snapshot
                .hosts
                .push(host(&name, HostState::Reachable));
            app.fleet_snapshot
                .observations
                .push(observation(&name, now, Vec::new()));
        }

        let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
        terminal
            .draw(|frame| render(&app, frame, app.view.sidebar_rect))
            .unwrap();
        let panel_rows = rows(&app, area(&app, app.view.sidebar_rect).width);
        let mut account_count = 0;
        let mut machine_count = 0;
        for (row_index, row) in panel_rows.iter().enumerate() {
            for (token_index, _) in row.targets.iter().enumerate() {
                let (anchor, expected) = token_target(&app, row_index, token_index).unwrap();
                let control =
                    crate::ui::tooltip::hovered_control_at(&app, anchor.x, anchor.y).unwrap();
                assert_eq!(
                    control,
                    crate::app::state::ControlId::SidebarFleetToken(row_index, token_index)
                );
                let (hit, tip) = crate::ui::tooltip::tooltip_target(&app, control).unwrap();
                assert_eq!(hit, anchor);
                assert_eq!(tip, expected);
                assert!(
                    crate::ui::tooltip::tooltip_rect(hit, &tip, Rect::new(0, 0, 100, 40)).is_some(),
                    "tooltip did not fit: {tip}"
                );
                if tip.contains("Profile: profile") {
                    account_count += 1;
                }
                if tip.contains("CPU:") {
                    machine_count += 1;
                }
            }
        }
        assert_eq!(account_count, 7);
        assert_eq!(machine_count, 7);
        assert!(row_target(&app, 0).unwrap().1.len() < 100);
    }

    #[test]
    fn legacy_usage_notepad_does_not_duplicate_fleet_panel() {
        let mut app = AppState::test_new();
        app.fleet_snapshot
            .hosts
            .push(host("ub1", HostState::Reachable));
        app.notepad.usage_tab = true;
        app.notepad.visible_tabs = vec!["usage".into()];
        let sidebar = Rect::new(0, 0, 25, 30);
        assert_eq!(
            app.notepad.active_tab_target(),
            Some(crate::notepad::NotepadTabTarget::Usage)
        );
        assert_eq!(
            super::super::sidebar_notepad_rect(&app, sidebar),
            Rect::default()
        );
        assert!(area(&app, sidebar).height > 0);
    }
}
