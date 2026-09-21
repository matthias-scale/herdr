//! Account-level subscription usage for the top status bar.
//!
//! Three providers, one shape: a 5-hour window and a 7-day window per account,
//! each a used percentage plus the instant it resets. The numbers describe the
//! *account*, not the focused pane, because the quota is what every agent on
//! that profile shares.
//!
//! Every source is already on disk or already installed. Claude Code writes its
//! own rate-limit payload to the statusline cache, Codex records its limits in
//! each rollout, and `kimi-usage` reshapes the Kimi plan into the same fields.
//! Nothing here talks to a provider API, so a dead network costs the bar a dim
//! segment rather than a stalled frame.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, HashMap},
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::OnceLock,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Claude's cache is written by whichever Claude Code session last rendered its
/// statusline. Past this age the numbers describe a window that may already
/// have rolled over, so the bar dims them instead of asserting them.
pub(crate) const CLAUDE_CACHE_STALE_AFTER: Duration = Duration::from_secs(30 * 60);
const KIMI_TIMEOUT: Duration = Duration::from_secs(5);
const KIMI_OUTPUT_LIMIT: usize = 64 * 1024;
const FIVE_HOUR_MINUTES: u64 = 300;
const SEVEN_DAY_MINUTES: u64 = 10_080;
const MAX_USAGE_FILES: usize = 64;
const MAX_USAGE_DIRECTORIES: usize = 256;
const MAX_USAGE_ENTRIES: usize = 4096;
const MAX_USAGE_FILE_BYTES: u64 = 512 * 1024;
const MAX_CREDIT_BALANCE: f64 = 1_000_000.0;
const MAX_CCUSAGE_DISCOVERY_ENTRIES: usize = 256;
const MAX_CCUSAGE_OUTPUT_BYTES: usize = 512 * 1024;
const CCUSAGE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CLAUDE_USAGE_AMOUNT: f64 = 1_000_000.0;
const MAX_CLAUDE_REMAINING_MINUTES: u64 = 24 * 60;

#[derive(Debug, Clone, Default, PartialEq)]
struct CodexRateLimits {
    windows: Vec<CodexUsageWindow>,
    credits: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
struct CodexUsageWindow {
    used_percent: f64,
    window_minutes: u64,
    resets_at: i64,
}

#[derive(Debug, serde::Deserialize)]
struct RawRateLimits {
    primary: Option<RawUsageWindow>,
    secondary: Option<RawUsageWindow>,
    credits: Option<RawCredits>,
}

#[derive(Debug, serde::Deserialize)]
struct RawUsageWindow {
    used_percent: f64,
    window_minutes: u64,
    resets_at: i64,
}

#[derive(Debug, serde::Deserialize)]
struct RawCredits {
    balance: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
struct RawCcusageResponse {
    blocks: Vec<RawCcusageBlock>,
}

#[derive(Debug, serde::Deserialize)]
struct RawCcusageBlock {
    #[serde(rename = "isActive")]
    is_active: bool,
    #[serde(rename = "endTime")]
    end_time: Option<String>,
    #[serde(rename = "costUSD")]
    cost_usd: Option<f64>,
    projection: Option<RawCcusageProjection>,
}

#[derive(Debug, serde::Deserialize)]
struct RawCcusageProjection {
    #[serde(rename = "remainingMinutes")]
    remaining_minutes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ClaudeUsageDetails {
    cost_usd: f64,
    remaining_minutes: Option<u64>,
}

fn parse_codex_record(line: &str) -> Option<CodexRateLimits> {
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
        .and_then(parse_credit_balance);
    (!windows.is_empty()).then_some(CodexRateLimits { windows, credits })
}

fn parse_credit_balance(value: serde_json::Value) -> Option<f64> {
    let balance = match value {
        serde_json::Value::Number(number) => number.as_f64()?,
        serde_json::Value::String(value) => value.parse().ok()?,
        _ => return None,
    };
    (balance.is_finite() && (0.0..=MAX_CREDIT_BALANCE).contains(&balance)).then_some(balance)
}

fn parse_usage_window(raw: RawUsageWindow) -> Option<CodexUsageWindow> {
    if !raw.used_percent.is_finite()
        || !(0.0..=100.0).contains(&raw.used_percent)
        || ![FIVE_HOUR_MINUTES, SEVEN_DAY_MINUTES].contains(&raw.window_minutes)
        || raw.resets_at <= 0
    {
        return None;
    }
    Some(CodexUsageWindow {
        used_percent: raw.used_percent,
        window_minutes: raw.window_minutes,
        resets_at: raw.resets_at,
    })
}

fn usage_window(usage: &CodexRateLimits, minutes: u64) -> Option<&CodexUsageWindow> {
    usage
        .windows
        .iter()
        .find(|window| window.window_minutes == minutes)
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

fn parse_ccusage_output(output: &str, now: i64) -> Result<Option<ClaudeUsageDetails>, ()> {
    let response: RawCcusageResponse = serde_json::from_str(output).map_err(|_| ())?;
    let Some(block) = response.blocks.into_iter().find(|block| block.is_active) else {
        return Ok(None);
    };
    let cost_usd = block
        .cost_usd
        .filter(|cost| cost.is_finite() && (0.0..=MAX_CLAUDE_USAGE_AMOUNT).contains(cost))
        .ok_or(())?;
    let resets_at = parse_utc_timestamp(block.end_time.as_deref().ok_or(())?).ok_or(())?;
    if resets_at <= now {
        return Ok(None);
    }
    let remaining_minutes = block.projection.ok_or(())?.remaining_minutes;
    let remaining_minutes =
        (remaining_minutes <= MAX_CLAUDE_REMAINING_MINUTES).then_some(remaining_minutes);
    Ok(Some(ClaudeUsageDetails {
        cost_usd,
        remaining_minutes,
    }))
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

fn load_claude_usage_details(now: i64) -> Option<ClaudeUsageDetails> {
    let binary = resolve_ccusage()?;
    let mut command = crate::noninteractive_process::command(binary);
    command.args(["blocks", "--active", "--json", "--offline"]);
    let output = crate::noninteractive_process::output_with_deadline_limited(
        command,
        Instant::now() + CCUSAGE_TIMEOUT,
        MAX_CCUSAGE_OUTPUT_BYTES,
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let output = String::from_utf8(output.stdout).ok()?;
    parse_ccusage_output(&output, now).ok().flatten()
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

fn home_path(directory: &str) -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .map(|home| home.join(directory))
}

/// Cached dashboard aggregates live beside the other local Herdr state.
pub(crate) const USAGE_CACHE_FILE: &str = "usage-cache.json";
const USAGE_CACHE_VERSION: u32 = 1;

/// Provider attached to one locally observed token-usage record.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UsageProvider {
    ClaudeCode,
    Codex,
    /// Proves that consumers handle a newly collected provider without adding
    /// a provider that production does not collect yet.
    #[cfg(test)]
    TestCollected,
}

impl UsageProvider {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            #[cfg(test)]
            Self::TestCollected => "Test Collected",
        }
    }

    pub(crate) fn series_index(self) -> usize {
        match self {
            Self::ClaudeCode => 0,
            Self::Codex => 1,
            #[cfg(test)]
            Self::TestCollected => 2,
        }
    }
}

/// One billable response or turn, normalised across local provider logs.
///
/// `input_tokens` excludes cached reads. Claude reports all four buckets
/// separately. Codex reports cached reads inside `input_tokens`, so its parser
/// subtracts that bucket before storing the sample.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct UsageSample {
    pub provider: UsageProvider,
    pub session_id: String,
    pub timestamp: i64,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_read_tokens: u64,
}

impl UsageSample {
    pub(crate) fn processed_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cache_write_tokens)
            .saturating_add(self.cache_read_tokens)
    }
}

/// Complete dashboard input produced by one filesystem scan.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct UsageSnapshot {
    pub samples: Vec<UsageSample>,
    /// Files present below the two provider roots during this scan.
    pub files_seen: usize,
    /// Files parsed rather than restored from the fingerprint cache.
    pub files_read: usize,
}

impl UsageSnapshot {
    pub(crate) fn samples_between(
        &self,
        start: i64,
        end: i64,
    ) -> impl Iterator<Item = &UsageSample> {
        self.samples
            .iter()
            .filter(move |sample| sample.timestamp >= start && sample.timestamp < end)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct UsageFileFingerprint {
    modified_seconds: u64,
    modified_nanos: u32,
    size: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CachedUsageFile {
    fingerprint: UsageFileFingerprint,
    samples: Vec<UsageSample>,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct UsageCache {
    version: u32,
    files: BTreeMap<PathBuf, CachedUsageFile>,
}

/// One quota window, normalised across providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QuotaWindow {
    pub used_percent: u8,
    /// Unix seconds. `None` when the provider reports a window without a reset,
    /// which Kimi does for a five-hour window that has not started.
    pub resets_at: Option<i64>,
}

/// A single provider account: its short label and its windows.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct AccountUsage {
    /// Short account code, e.g. `SHQ`. `None` when the account cannot be named.
    pub account: Option<String>,
    pub five_hour: Option<QuotaWindow>,
    pub seven_day: Option<QuotaWindow>,
    /// Remaining provider credits, when the provider reports a balance.
    pub credits: Option<f64>,
    /// Cost of the current Claude five-hour window, when reported by ccusage.
    pub cost_usd: Option<f64>,
    /// Minutes remaining in the current Claude five-hour window.
    pub remaining_minutes: Option<u64>,
    /// The source is older than its freshness budget. Values render dimmed.
    pub stale: bool,
}

impl AccountUsage {
    pub(crate) fn is_empty(&self) -> bool {
        self.five_hour.is_none() && self.seven_day.is_none()
    }

    /// The hottest window, which decides whether the account escalates.
    pub(crate) fn peak_percent(&self) -> Option<u8> {
        [self.five_hour, self.seven_day]
            .into_iter()
            .flatten()
            .map(|window| window.used_percent)
            .max()
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ProviderUsageSnapshot {
    pub claude: AccountUsage,
    pub codex: AccountUsage,
    pub kimi: AccountUsage,
}

/// Collects every provider. Blocking: callers run it off the render thread.
pub(crate) fn collect(now_unix: Option<i64>, now: Instant) -> ProviderUsageSnapshot {
    ProviderUsageSnapshot {
        claude: load_claude_usage(now_unix, now),
        codex: load_codex_usage(now_unix),
        kimi: load_kimi_usage(now_unix),
    }
}

/// Loads the last complete scan without touching provider logs, so opening the
/// dashboard can paint immediately while its background refresh runs.
pub(crate) fn load_cached_usage() -> Option<UsageSnapshot> {
    let cache = read_usage_cache(&crate::config::state_dir().join(USAGE_CACHE_FILE));
    if cache.version != USAGE_CACHE_VERSION {
        return None;
    }
    let mut snapshot = UsageSnapshot {
        files_seen: cache.files.len(),
        files_read: 0,
        samples: cache
            .files
            .into_values()
            .flat_map(|file| file.samples)
            .collect(),
    };
    sort_usage_samples(&mut snapshot.samples);
    Some(snapshot)
}

/// Background-job entry point for a fresh scan of both provider histories.
pub(crate) fn scan_historical_usage() -> Result<UsageSnapshot, String> {
    let home = home_path("").ok_or_else(|| "home directory is not set".to_string())?;
    Ok(collect_dashboard_usage_from(
        &home,
        &crate::config::state_dir().join(USAGE_CACHE_FILE),
    ))
}

/// Explicit-root form used by tests and isolated runtime fixtures.
pub(crate) fn collect_dashboard_usage_from(home: &Path, cache_path: &Path) -> UsageSnapshot {
    let mut discovered = Vec::new();
    for (provider, root) in [
        (UsageProvider::ClaudeCode, home.join(".claude/projects")),
        (UsageProvider::Codex, home.join(".codex/sessions")),
    ] {
        if let Err(error) = discover_jsonl_files(provider, &root, &mut discovered) {
            tracing::warn!(path = %root.display(), %error, "failed to discover provider usage logs");
        }
    }
    discovered.sort_by(|left, right| left.1.cmp(&right.1));

    let previous = read_usage_cache(cache_path);
    let mut next_files = BTreeMap::new();
    let mut snapshot = UsageSnapshot {
        files_seen: discovered.len(),
        ..UsageSnapshot::default()
    };
    for (provider, path) in discovered {
        let Ok(fingerprint) = usage_file_fingerprint(&path) else {
            continue;
        };
        let (fingerprint, samples) = if let Some(cached) = previous
            .files
            .get(&path)
            .filter(|cached| cached.fingerprint == fingerprint)
        {
            (fingerprint, cached.samples.clone())
        } else {
            snapshot.files_read = snapshot.files_read.saturating_add(1);
            let mut samples = parse_usage_file(provider, &path);
            let mut stable = usage_file_fingerprint(&path).unwrap_or(fingerprint.clone());
            if stable != fingerprint {
                samples = parse_usage_file(provider, &path);
                stable = usage_file_fingerprint(&path).unwrap_or(stable);
            }
            (stable, samples)
        };
        snapshot.samples.extend(samples.iter().cloned());
        next_files.insert(
            path,
            CachedUsageFile {
                fingerprint,
                samples,
            },
        );
    }
    sort_usage_samples(&mut snapshot.samples);

    let cache = UsageCache {
        version: USAGE_CACHE_VERSION,
        files: next_files,
    };
    if let Err(error) = write_usage_cache(cache_path, &cache) {
        tracing::warn!(path = %cache_path.display(), %error, "failed to persist provider usage cache");
    }
    snapshot
}

fn sort_usage_samples(samples: &mut [UsageSample]) {
    samples.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.provider.cmp(&right.provider))
            .then_with(|| left.session_id.cmp(&right.session_id))
            .then_with(|| left.model.cmp(&right.model))
    });
}

fn discover_jsonl_files(
    provider: UsageProvider,
    root: &Path,
    files: &mut Vec<(UsageProvider, PathBuf)>,
) -> io::Result<()> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            if let Err(error) = discover_jsonl_files(provider, &path, files) {
                tracing::warn!(path = %path.display(), %error, "failed to inspect provider usage directory");
            }
        } else if file_type.is_file() && path.extension().is_some_and(|value| value == "jsonl") {
            files.push((provider, path));
        }
    }
    Ok(())
}

fn usage_file_fingerprint(path: &Path) -> io::Result<UsageFileFingerprint> {
    let metadata = fs::metadata(path)?;
    let modified = metadata
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    Ok(UsageFileFingerprint {
        modified_seconds: modified.as_secs(),
        modified_nanos: modified.subsec_nanos(),
        size: metadata.len(),
    })
}

fn read_usage_cache(path: &Path) -> UsageCache {
    let Ok(contents) = fs::read(path) else {
        return UsageCache::default();
    };
    let Ok(cache) = serde_json::from_slice::<UsageCache>(&contents) else {
        return UsageCache::default();
    };
    if cache.version == USAGE_CACHE_VERSION {
        cache
    } else {
        UsageCache::default()
    }
}

fn write_usage_cache(path: &Path, cache: &UsageCache) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("usage cache path has no parent"))?;
    fs::create_dir_all(parent)?;
    let contents = serde_json::to_vec_pretty(cache).map_err(io::Error::other)?;
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = path.with_extension(format!("json.{}.{}.tmp", std::process::id(), unique));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    if let Err(error) = file.write_all(&contents).and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    drop(file);
    if let Err(error) = crate::platform::replace_file_durably(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
}

fn parse_usage_file(provider: UsageProvider, path: &Path) -> Vec<UsageSample> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let session_id = usage_session_id(path);
    match provider {
        UsageProvider::ClaudeCode => parse_claude_usage_samples(&contents, &session_id),
        UsageProvider::Codex => parse_codex_usage_samples(&contents, &session_id),
        #[cfg(test)]
        UsageProvider::TestCollected => Vec::new(),
    }
}

fn usage_session_id(path: &Path) -> String {
    let claude_parent_session = path
        .parent()
        .filter(|parent| parent.file_name().is_some_and(|name| name == "subagents"))
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty());
    if let Some(session_id) = claude_parent_session {
        return session_id.to_owned();
    }
    path.file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

fn json_u64(value: Option<&serde_json::Value>) -> u64 {
    value.and_then(serde_json::Value::as_u64).unwrap_or(0)
}

fn usage_timestamp(value: Option<&serde_json::Value>) -> Option<i64> {
    let value = value?;
    if let Some(timestamp) = value.as_i64() {
        return Some(if timestamp > 10_000_000_000 {
            timestamp / 1_000
        } else {
            timestamp
        });
    }
    value.as_str().and_then(parse_utc_timestamp)
}

fn nonempty_sample(sample: &UsageSample) -> bool {
    sample.processed_tokens() > 0
}

/// Parses Claude Code assistant records. The fields used are
/// `message.model`, `message.usage.{input_tokens,output_tokens,
/// cache_creation_input_tokens,cache_read_input_tokens}`, and top-level
/// `timestamp`. Session identity comes from the JSONL path.
pub(crate) fn parse_claude_usage_samples(contents: &str, session_id: &str) -> Vec<UsageSample> {
    contents
        .lines()
        .filter_map(|line| {
            let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
            let message = value.get("message")?;
            let usage = message.get("usage")?;
            let sample = UsageSample {
                provider: UsageProvider::ClaudeCode,
                session_id: session_id.to_owned(),
                timestamp: usage_timestamp(value.get("timestamp"))?,
                model: message
                    .get("model")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned(),
                input_tokens: json_u64(usage.get("input_tokens")),
                output_tokens: json_u64(usage.get("output_tokens")),
                cache_write_tokens: json_u64(usage.get("cache_creation_input_tokens")),
                cache_read_tokens: json_u64(usage.get("cache_read_input_tokens")),
            };
            nonempty_sample(&sample).then_some(sample)
        })
        .collect()
}

fn codex_sample(
    usage: &serde_json::Value,
    timestamp: i64,
    model: &str,
    session_id: &str,
) -> UsageSample {
    let reported_input = json_u64(usage.get("input_tokens"));
    let cache_read_tokens = json_u64(usage.get("cached_input_tokens"));
    let cache_write_tokens = json_u64(usage.get("cache_write_input_tokens"));
    UsageSample {
        provider: UsageProvider::Codex,
        session_id: session_id.to_owned(),
        timestamp,
        model: model.to_owned(),
        input_tokens: reported_input.saturating_sub(cache_read_tokens),
        output_tokens: json_u64(usage.get("output_tokens")),
        cache_write_tokens,
        cache_read_tokens,
    }
}

/// Parses Codex rollout records. Modern logs use incremental
/// `token_usage_record.payload.usage` values joined to
/// `turn_context.payload.model` by turn id. Older logs use
/// `event_msg/token_count.info.last_token_usage`; those are read only when a
/// file has no modern records because current rollouts contain both forms.
pub(crate) fn parse_codex_usage_samples(contents: &str, session_id: &str) -> Vec<UsageSample> {
    let mut models_by_turn = HashMap::new();
    let mut current_model: Option<String> = None;
    let mut modern = Vec::new();
    let mut legacy = Vec::new();

    for line in contents.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let record_type = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let payload = value.get("payload");
        if record_type == "turn_context" {
            let turn_id = payload
                .and_then(|payload| payload.get("turn_id"))
                .and_then(serde_json::Value::as_str);
            let model = payload
                .and_then(|payload| payload.get("model"))
                .and_then(serde_json::Value::as_str);
            if let Some(model) = model {
                current_model = Some(model.to_owned());
                if let Some(turn_id) = turn_id {
                    models_by_turn.insert(turn_id.to_owned(), model.to_owned());
                }
            }
            continue;
        }

        let Some(timestamp) = usage_timestamp(value.get("timestamp")) else {
            continue;
        };
        if record_type == "token_usage_record" {
            let Some(payload) = payload else {
                continue;
            };
            let Some(usage) = payload.get("usage") else {
                continue;
            };
            let model = payload
                .get("turn_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|turn_id| models_by_turn.get(turn_id))
                .or(current_model.as_ref())
                .map(String::as_str)
                .unwrap_or("unknown");
            let sample = codex_sample(usage, timestamp, model, session_id);
            if nonempty_sample(&sample) {
                modern.push(sample);
            }
            continue;
        }

        let legacy_usage = (record_type == "event_msg")
            .then_some(payload)
            .flatten()
            .filter(|payload| {
                payload.get("type").and_then(serde_json::Value::as_str) == Some("token_count")
            })
            .and_then(|payload| payload.get("info"))
            .and_then(|info| info.get("last_token_usage"));
        if let Some(usage) = legacy_usage {
            let model = current_model.as_deref().unwrap_or("unknown");
            let sample = codex_sample(usage, timestamp, model, session_id);
            if nonempty_sample(&sample) {
                legacy.push(sample);
            }
        }
    }

    if modern.is_empty() {
        legacy
    } else {
        modern
    }
}

fn statusline_cache_dir() -> PathBuf {
    std::env::var_os("CLAUDE_STATUSLINE_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/claude-statusline"))
}

/// Basename of the active config directory, which is how both CLIs name a
/// profile. The default directory carries no profile, so it yields `None`.
fn active_profile(env_var: &str, default_dir: &str) -> Option<String> {
    let dir = std::env::var_os(env_var).map(PathBuf::from)?;
    let name = dir.file_name()?.to_string_lossy().into_owned();
    (name != default_dir).then_some(name)
}

/// `matthias@scalablehq.com` → `scalablehq.com` → `SHQ`.
///
/// The map is the readable part: an unmapped domain still gets a code rather
/// than disappearing, so a new account is legible the day it is added.
pub(crate) fn account_code(domain: &str) -> Option<String> {
    let domain = domain.trim().trim_start_matches("DOMAIN=").trim();
    if domain.is_empty() {
        return None;
    }
    let known = [
        ("scalablehq.com", "SHQ"),
        ("scalable.so", "SSO"),
        ("machete-ventures.com", "MV"),
        ("mable.ai", "MA"),
    ];
    if let Some((_, code)) = known.iter().find(|(known, _)| *known == domain) {
        return Some((*code).to_string());
    }
    let stem = domain.split('.').next().unwrap_or(domain);
    let code: String = stem
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(3)
        .collect();
    (!code.is_empty()).then(|| code.to_uppercase())
}

/// Largest `.claude.json` worth reading for one display label.
const MAX_CLAUDE_CONFIG_BYTES: u64 = 4 * 1024 * 1024;

/// The Claude account label: the statusline's own domain cache when a profile
/// is active, the signed-in account otherwise.
///
/// The fallback matters more than it looks. Agents are launched with per-pane
/// profile directories, so the server process itself usually has no
/// `CLAUDE_CONFIG_DIR`, and without it the row would show a quota with no
/// indication of whose it is.
fn claude_account_code(profile: Option<&str>) -> Option<String> {
    if let Some(profile) = profile {
        let path = statusline_cache_dir().join(format!("acctdom-claude-{profile}.env"));
        if let Ok(contents) = std::fs::read_to_string(path) {
            if let Some(code) = contents
                .lines()
                .find_map(|line| line.strip_prefix("DOMAIN=").and_then(account_code))
            {
                return Some(code);
            }
        }
    }
    signed_in_claude_account_code()
}

fn signed_in_claude_account_code() -> Option<String> {
    let path = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .map(|dir| dir.join(".claude.json"))
        .or_else(|| home_path(".claude.json"))?;
    if std::fs::metadata(&path).ok()?.len() > MAX_CLAUDE_CONFIG_BYTES {
        return None;
    }
    let contents = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&contents).ok()?;
    let email = value
        .get("oauthAccount")?
        .get("emailAddress")?
        .as_str()?
        .to_owned();
    account_code(email.split_once('@')?.1)
}

/// Parses the `KEY=value` snapshot the statusline writes after every render.
pub(crate) fn parse_claude_rate_limits(
    contents: &str,
    now_unix: Option<i64>,
    age: Option<Duration>,
) -> AccountUsage {
    let mut fields = std::collections::HashMap::new();
    for line in contents.lines() {
        if let Some((key, value)) = line.split_once('=') {
            fields.insert(key.trim(), value.trim());
        }
    }
    let percent = |key: &str| {
        fields
            .get(key)
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| (0..=100).contains(value))
            .map(|value| value as u8)
    };
    let reset = |key: &str| fields.get(key).and_then(|value| value.parse::<i64>().ok());
    let window = |percent_key: &str, reset_key: &str| {
        let used_percent = percent(percent_key)?;
        let resets_at = reset(reset_key).filter(|resets_at| *resets_at > 0);
        // An elapsed reset means the window rolled over and nobody has rendered
        // a statusline since; the percentage belongs to a window that is gone.
        if let (Some(resets_at), Some(now)) = (resets_at, now_unix) {
            if resets_at <= now {
                return None;
            }
        }
        Some(QuotaWindow {
            used_percent,
            resets_at,
        })
    };

    AccountUsage {
        account: None,
        five_hour: window("R5", "R5_RST"),
        seven_day: window("R7", "R7_RST"),
        credits: None,
        stale: age.is_some_and(|age| age >= CLAUDE_CACHE_STALE_AFTER),
        ..AccountUsage::default()
    }
}

fn load_claude_usage(now_unix: Option<i64>, now: Instant) -> AccountUsage {
    let path = statusline_cache_dir().join("rate-limits.env");
    let mut usage = std::fs::read_to_string(&path).map_or_else(
        |_| AccountUsage::default(),
        |contents| parse_claude_rate_limits(&contents, now_unix, file_age(&path, now)),
    );
    usage.account = claude_account_code(active_profile("CLAUDE_CONFIG_DIR", ".claude").as_deref());
    if let Some(details) = now_unix.and_then(load_claude_usage_details) {
        usage.cost_usd = Some(details.cost_usd);
        usage.remaining_minutes = details.remaining_minutes;
    }
    usage
}

fn file_age(path: &Path, _now: Instant) -> Option<Duration> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
}

/// The Codex account behind the active `CODEX_HOME`, read from the `id_token`
/// the CLI already stores. No network call and no token is ever logged.
fn codex_account_code() -> Option<String> {
    let root = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| home_path(".codex"))?;
    let contents = std::fs::read_to_string(root.join("auth.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&contents).ok()?;
    let id_token = value.get("tokens")?.get("id_token")?.as_str()?;
    let claims = decode_jwt_claims(id_token)?;
    let email = claims.get("email")?.as_str()?;
    account_code(email.split_once('@')?.1)
}

/// Decodes the payload segment of a JWT. Signature verification is pointless
/// here: the file is already trusted local state, and the only field read is a
/// display label.
fn decode_jwt_claims(token: &str) -> Option<serde_json::Value> {
    let payload = token.split('.').nth(1)?;
    let decoded = base64_url_decode(payload)?;
    serde_json::from_slice(&decoded).ok()
}

fn base64_url_decode(input: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut buffer = 0u32;
    let mut bits = 0u32;
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    for byte in input.bytes() {
        if byte == b'=' {
            break;
        }
        let value = TABLE.iter().position(|candidate| *candidate == byte)? as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

fn load_codex_usage(now_unix: Option<i64>) -> AccountUsage {
    let Some(root) = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .map(|home| home.join("sessions"))
        .or_else(|| home_path(".codex/sessions"))
    else {
        return AccountUsage::default();
    };
    let Ok(files) = recent_jsonl_files(&root, MAX_USAGE_FILES) else {
        return AccountUsage::default();
    };

    let mut usage = AccountUsage {
        account: codex_account_code(),
        ..AccountUsage::default()
    };
    for path in files {
        let Some(contents) = read_file_tail(&path) else {
            continue;
        };
        let Some(record) = contents.lines().filter_map(parse_codex_record).next_back() else {
            continue;
        };
        let window = |minutes: u64| {
            // A reset already in the past means this rollout is the newest
            // record and still describes a window that has since rolled over.
            // Report it as stale rather than as current truth.
            usage_window(&record, minutes).map(|window| QuotaWindow {
                used_percent: window.used_percent.round().clamp(0.0, 100.0) as u8,
                resets_at: Some(window.resets_at),
            })
        };
        usage.five_hour = window(FIVE_HOUR_MINUTES);
        usage.seven_day = window(SEVEN_DAY_MINUTES);
        usage.credits = record.credits;
        usage.stale = [usage.five_hour, usage.seven_day]
            .into_iter()
            .flatten()
            .all(|window| {
                matches!((window.resets_at, now_unix), (Some(resets_at), Some(now)) if resets_at <= now)
            });
        if !usage.is_empty() {
            break;
        }
    }
    usage
}

#[derive(Debug, serde::Deserialize)]
struct RawKimiWindow {
    used_percentage: Option<f64>,
    resets_at: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct RawKimiUsage {
    five_hour: Option<RawKimiWindow>,
    seven_day: Option<RawKimiWindow>,
}

pub(crate) fn parse_kimi_usage(output: &str, now_unix: Option<i64>) -> AccountUsage {
    let Ok(raw) = serde_json::from_str::<RawKimiUsage>(output) else {
        return AccountUsage::default();
    };
    let window = |raw: Option<RawKimiWindow>| {
        let raw = raw?;
        let used_percent = raw
            .used_percentage
            .filter(|percent| percent.is_finite() && (0.0..=100.0).contains(percent))?
            .round() as u8;
        let resets_at = raw
            .resets_at
            .as_deref()
            .filter(|value| !value.is_empty())
            .and_then(parse_utc_timestamp);
        if let (Some(resets_at), Some(now)) = (resets_at, now_unix) {
            if resets_at <= now {
                return None;
            }
        }
        Some(QuotaWindow {
            used_percent,
            resets_at,
        })
    };
    AccountUsage {
        account: None,
        five_hour: window(raw.five_hour),
        seven_day: window(raw.seven_day),
        credits: None,
        stale: false,
        ..AccountUsage::default()
    }
}

/// Kimi has no on-disk cache of its own, so this shells out to the same
/// `kimi-usage` the statusline uses. Absent binary, non-zero exit, or garbage
/// output all mean the same thing to the bar: no Kimi segment at all.
fn load_kimi_usage(now_unix: Option<i64>) -> AccountUsage {
    let Some(binary) = resolve_kimi_usage() else {
        return AccountUsage::default();
    };
    let Ok(mut child) = Command::new(binary)
        .arg("--rate-limits")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return AccountUsage::default();
    };

    let deadline = Instant::now() + KIMI_TIMEOUT;
    let output = loop {
        match child.try_wait() {
            Ok(Some(_)) => break child.wait_with_output().ok(),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => break None,
        }
    };

    let Some(output) = output.filter(|output| output.status.success()) else {
        return AccountUsage::default();
    };
    if output.stdout.len() > KIMI_OUTPUT_LIMIT {
        return AccountUsage::default();
    }
    parse_kimi_usage(&String::from_utf8_lossy(&output.stdout), now_unix)
}

fn resolve_kimi_usage() -> Option<PathBuf> {
    for candidate in [".local/bin/kimi-usage", "bin/kimi-usage"] {
        if let Some(path) = home_path(candidate) {
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

/// Human reset distance: `2h45`, `3d15h`, `12m`.
pub(crate) fn reset_label(resets_at: i64, now_unix: i64) -> Option<String> {
    let remaining = resets_at.checked_sub(now_unix)?;
    if remaining <= 0 {
        return None;
    }
    let minutes = remaining / 60;
    let (hours, minutes) = (minutes / 60, minutes % 60);
    Some(match hours {
        0 => format!("{minutes}m"),
        hours if hours < 48 => format!("{hours}h{minutes:02}"),
        hours => format!("{}d{}h", hours / 24, hours % 24),
    })
}

/// Refresh cadence. Quota windows move in minutes, not seconds, and the Kimi
/// read costs a process spawn, so a minute is the right price.
pub(crate) const PROVIDER_USAGE_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

pub(crate) fn snapshot_is_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|last| {
        now.checked_duration_since(last)
            .is_some_and(|elapsed| elapsed >= PROVIDER_USAGE_REFRESH_INTERVAL)
    })
}

/// Convenience for callers that only have wall-clock time.
pub(crate) fn now_unix() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_787_992_841;

    #[test]
    fn codex_rate_limit_record_keeps_both_supported_windows() {
        let usage = parse_codex_record(
            r#"{"payload":{"rate_limits":{"primary":{"used_percent":31.0,"window_minutes":300,"resets_at":1788003000},"secondary":{"used_percent":52.0,"window_minutes":10080,"resets_at":1788307200},"credits":{"balance":"1927.95"}}}}"#,
        )
        .expect("rate limits");

        assert_eq!(
            usage_window(&usage, FIVE_HOUR_MINUTES)
                .unwrap()
                .used_percent,
            31.0
        );
        assert_eq!(
            usage_window(&usage, SEVEN_DAY_MINUTES)
                .unwrap()
                .used_percent,
            52.0
        );
        assert_eq!(usage.credits, Some(1927.95));
    }

    #[test]
    fn parses_ccusage_cost_and_remaining_minutes_from_active_block() {
        let usage = parse_ccusage_output(
            r#"{"blocks":[{"isActive":true,"endTime":"2026-09-21T14:00:00.000Z","costUSD":56.68,"projection":{"remainingMinutes":66}}]}"#,
            parse_utc_timestamp("2026-09-21T12:00:00Z").expect("valid test timestamp"),
        )
        .expect("valid ccusage JSON")
        .expect("active Claude block");

        assert_eq!(usage.cost_usd, 56.68);
        assert_eq!(usage.remaining_minutes, Some(66));
    }

    #[test]
    fn ccusage_missing_active_block_keeps_claude_details_unavailable() {
        assert_eq!(
            parse_ccusage_output(r#"{"blocks":[{"isActive":false}]}"#, NOW,)
                .expect("valid ccusage JSON"),
            None
        );
    }

    struct UsageFixture {
        root: PathBuf,
        cache_path: PathBuf,
        claude_path: PathBuf,
        codex_path: PathBuf,
    }

    impl UsageFixture {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "herdr-usage-{name}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("test clock after epoch")
                    .as_nanos()
            ));
            let claude_path = root
                .join(".claude/projects/project-a/nested")
                .join("claude-session.jsonl");
            let codex_path = root
                .join(".codex/sessions/2026/09/06")
                .join("rollout-codex-session.jsonl");
            fs::create_dir_all(claude_path.parent().expect("Claude fixture parent"))
                .expect("create Claude fixture tree");
            fs::create_dir_all(codex_path.parent().expect("Codex fixture parent"))
                .expect("create Codex fixture tree");
            fs::write(
                &claude_path,
                concat!(
                    "{\"type\":\"user\",\"timestamp\":\"2026-09-05T10:00:00Z\"}\n",
                    "{\"type\":\"assistant\",\"timestamp\":\"2026-09-05T10:01:00.123Z\",",
                    "\"message\":{\"model\":\"claude-sonnet-5\",\"usage\":{",
                    "\"input_tokens\":10,\"output_tokens\":5,",
                    "\"cache_creation_input_tokens\":20,\"cache_read_input_tokens\":30}}}\n"
                ),
            )
            .expect("write Claude fixture");
            fs::write(
                &codex_path,
                concat!(
                    "{\"timestamp\":\"2026-09-05T11:00:00Z\",\"type\":\"turn_context\",",
                    "\"payload\":{\"turn_id\":\"turn-1\",\"model\":\"gpt-5.6-sol\"}}\n",
                    "{\"timestamp\":\"2026-09-05T11:01:00Z\",\"type\":\"token_usage_record\",",
                    "\"payload\":{\"turn_id\":\"turn-1\",\"session_id\":\"ignored-payload-session\",",
                    "\"usage\":{\"input_tokens\":100,\"cached_input_tokens\":60,",
                    "\"cache_write_input_tokens\":10,\"output_tokens\":7,",
                    "\"reasoning_output_tokens\":99,\"total_tokens\":107}}}\n",
                    "{\"timestamp\":\"2026-09-05T11:01:00Z\",\"type\":\"event_msg\",",
                    "\"payload\":{\"type\":\"token_count\",\"info\":{\"last_token_usage\":{",
                    "\"input_tokens\":100,\"cached_input_tokens\":60,",
                    "\"cache_write_input_tokens\":10,\"output_tokens\":7}}}}\n"
                ),
            )
            .expect("write Codex fixture");
            let cache_path = root.join("state/herdr-dev").join(USAGE_CACHE_FILE);
            Self {
                root,
                cache_path,
                claude_path,
                codex_path,
            }
        }

        fn scan(&self) -> UsageSnapshot {
            collect_dashboard_usage_from(&self.root, &self.cache_path)
        }
    }

    impl Drop for UsageFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn claude_cache_yields_both_windows_and_drops_a_rolled_over_one() {
        let contents = "R5=6\nR7=56\nR5_RST=1788003000\nR7_RST=1788307200\nTS=1787992841\n";
        let usage = parse_claude_rate_limits(contents, Some(NOW), Some(Duration::from_secs(60)));
        assert_eq!(
            usage.five_hour,
            Some(QuotaWindow {
                used_percent: 6,
                resets_at: Some(1_788_003_000)
            })
        );
        assert_eq!(usage.seven_day.map(|window| window.used_percent), Some(56));
        assert!(!usage.stale);

        // Nobody has rendered a statusline since the window rolled over, so the
        // percentage describes a window that no longer exists.
        let expired = parse_claude_rate_limits(contents, Some(1_788_400_000), None);
        assert!(expired.five_hour.is_none());
        assert!(expired.seven_day.is_none());
    }

    #[test]
    fn a_cache_older_than_its_budget_is_reported_stale_not_dropped() {
        let contents = "R5=6\nR7=56\nR5_RST=1788003000\nR7_RST=1788307200\n";
        let usage = parse_claude_rate_limits(contents, Some(NOW), Some(CLAUDE_CACHE_STALE_AFTER));
        assert!(usage.stale);
        assert!(usage.five_hour.is_some(), "stale still renders, dimmed");
    }

    #[test]
    fn unset_percentages_read_as_absent_rather_than_zero() {
        // The statusline writes -1 when Claude Code sent no rate limits at all.
        // Zero would draw a column claiming a fresh quota.
        let usage = parse_claude_rate_limits("R5=-1\nR7=-1\n", Some(NOW), None);
        assert!(usage.is_empty());
    }

    #[test]
    fn kimi_reshaped_output_parses_into_the_same_shape() {
        let output = r#"{"five_hour":{"used_percentage":0,"resets_at":""},
            "seven_day":{"used_percentage":24,"resets_at":"2026-08-31T06:02:04Z"}}"#;
        let usage = parse_kimi_usage(output, Some(NOW));
        assert_eq!(usage.five_hour.map(|window| window.used_percent), Some(0));
        assert_eq!(
            usage.five_hour.and_then(|window| window.resets_at),
            None,
            "an empty reset string is not a reset time"
        );
        assert_eq!(usage.seven_day.map(|window| window.used_percent), Some(24));
    }

    #[test]
    fn garbage_from_kimi_is_no_kimi_segment_rather_than_a_zero_one() {
        assert!(parse_kimi_usage("not json", Some(NOW)).is_empty());
        assert!(parse_kimi_usage(r#"{"five_hour":{"used_percentage":250}}"#, Some(NOW)).is_empty());
    }

    #[test]
    fn account_codes_map_known_domains_and_still_name_unknown_ones() {
        assert_eq!(account_code("scalablehq.com").as_deref(), Some("SHQ"));
        assert_eq!(account_code("DOMAIN=scalable.so").as_deref(), Some("SSO"));
        assert_eq!(account_code("machete-ventures.com").as_deref(), Some("MV"));
        assert_eq!(account_code("newcompany.io").as_deref(), Some("NEW"));
        assert_eq!(account_code(""), None);
    }

    #[test]
    fn a_jwt_payload_decodes_to_the_account_email_domain() {
        // {"email":"matthias@scalablehq.com"}
        let token = "header.eyJlbWFpbCI6Im1hdHRoaWFzQHNjYWxhYmxlaHEuY29tIn0.signature";
        let claims = decode_jwt_claims(token).expect("payload");
        assert_eq!(
            claims.get("email").and_then(|value| value.as_str()),
            Some("matthias@scalablehq.com")
        );
    }

    #[test]
    fn reset_labels_shorten_as_the_distance_grows() {
        assert_eq!(reset_label(NOW + 720, NOW).as_deref(), Some("12m"));
        assert_eq!(reset_label(NOW + 9_900, NOW).as_deref(), Some("2h45"));
        assert_eq!(reset_label(NOW + 313_200, NOW).as_deref(), Some("3d15h"));
        assert_eq!(reset_label(NOW - 1, NOW), None);
    }

    #[test]
    fn peak_percent_reports_the_window_closest_to_its_ceiling() {
        let usage = AccountUsage {
            five_hour: Some(QuotaWindow {
                used_percent: 6,
                resets_at: None,
            }),
            seven_day: Some(QuotaWindow {
                used_percent: 91,
                resets_at: None,
            }),
            ..AccountUsage::default()
        };
        assert_eq!(usage.peak_percent(), Some(91));
    }

    #[test]
    fn refresh_is_due_once_the_interval_has_passed_and_not_before() {
        let start = Instant::now();
        assert!(snapshot_is_due(None, start));
        assert!(!snapshot_is_due(
            Some(start),
            start + PROVIDER_USAGE_REFRESH_INTERVAL - Duration::from_millis(1)
        ));
        assert!(snapshot_is_due(
            Some(start),
            start + PROVIDER_USAGE_REFRESH_INTERVAL
        ));
    }

    #[test]
    fn dashboard_scan_reads_recursive_claude_and_codex_fixtures_without_double_counting() {
        let fixture = UsageFixture::new("both-providers");

        let snapshot = fixture.scan();

        assert_eq!(snapshot.files_seen, 2);
        assert_eq!(snapshot.files_read, 2);
        assert_eq!(snapshot.samples.len(), 2);
        assert_eq!(
            snapshot.samples[0],
            UsageSample {
                provider: UsageProvider::ClaudeCode,
                session_id: "claude-session".into(),
                timestamp: 1_788_602_460,
                model: "claude-sonnet-5".into(),
                input_tokens: 10,
                output_tokens: 5,
                cache_write_tokens: 20,
                cache_read_tokens: 30,
            }
        );
        assert_eq!(snapshot.samples[0].processed_tokens(), 65);
        assert_eq!(snapshot.samples[1].provider, UsageProvider::Codex);
        assert_eq!(snapshot.samples[1].session_id, "rollout-codex-session");
        assert_eq!(snapshot.samples[1].model, "gpt-5.6-sol");
        assert_eq!(snapshot.samples[1].input_tokens, 40);
        assert_eq!(snapshot.samples[1].cache_read_tokens, 60);
        assert_eq!(snapshot.samples[1].cache_write_tokens, 10);
        assert_eq!(snapshot.samples[1].output_tokens, 7);
        assert_eq!(
            snapshot
                .samples_between(1_788_602_000, 1_788_607_000)
                .count(),
            2
        );
    }

    #[test]
    fn dashboard_cache_reuses_unchanged_files_and_reads_only_the_changed_file() {
        let fixture = UsageFixture::new("changed-cache-file");
        let first = fixture.scan();
        assert_eq!(first.files_read, 2);
        assert!(fixture.cache_path.is_file());

        let cached = fixture.scan();
        assert_eq!(cached.files_seen, 2);
        assert_eq!(cached.files_read, 0);
        assert_eq!(cached.samples, first.samples);

        let mut claude = fs::read_to_string(&fixture.claude_path).expect("read Claude fixture");
        claude.push_str(concat!(
            "{\"type\":\"assistant\",\"timestamp\":\"2026-09-06T10:01:00Z\",",
            "\"message\":{\"model\":\"claude-opus-5\",\"usage\":{",
            "\"input_tokens\":1,\"output_tokens\":2,",
            "\"cache_creation_input_tokens\":3,\"cache_read_input_tokens\":4}}}\n"
        ));
        fs::write(&fixture.claude_path, claude).expect("change Claude fixture");

        let changed = fixture.scan();
        assert_eq!(changed.files_seen, 2);
        assert_eq!(changed.files_read, 1);
        assert_eq!(changed.samples.len(), 3);
        assert_eq!(
            changed
                .samples
                .iter()
                .filter(|sample| sample.provider == UsageProvider::Codex)
                .count(),
            1,
            "unchanged Codex file is restored once from cache"
        );
        let cache: UsageCache =
            serde_json::from_slice(&fs::read(&fixture.cache_path).expect("read persisted cache"))
                .expect("parse persisted cache");
        assert_eq!(cache.version, USAGE_CACHE_VERSION);
        assert_eq!(cache.files.len(), 2);
        assert!(cache.files.contains_key(&fixture.codex_path));
    }

    #[test]
    fn legacy_codex_token_count_is_used_when_incremental_records_are_absent() {
        let samples = parse_codex_usage_samples(
            concat!(
                "{\"timestamp\":\"2026-09-05T11:00:00Z\",\"type\":\"turn_context\",",
                "\"payload\":{\"turn_id\":\"turn-1\",\"model\":\"gpt-5.3-codex-spark\"}}\n",
                "{\"timestamp\":\"2026-09-05T11:01:00Z\",\"type\":\"event_msg\",",
                "\"payload\":{\"type\":\"token_count\",\"info\":{\"last_token_usage\":{",
                "\"input_tokens\":80,\"cached_input_tokens\":20,",
                "\"cache_write_input_tokens\":5,\"output_tokens\":4}}}}\n"
            ),
            "legacy-session",
        );

        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].model, "gpt-5.3-codex-spark");
        assert_eq!(samples[0].input_tokens, 60);
        assert_eq!(samples[0].output_tokens, 4);
    }

    #[test]
    fn claude_subagent_path_uses_the_parent_session_id() {
        assert_eq!(
            usage_session_id(Path::new(
                "/home/.claude/projects/project/session-123/subagents/agent-456.jsonl"
            )),
            "session-123"
        );
        assert_eq!(
            usage_session_id(Path::new("/home/.codex/sessions/rollout-session-789.jsonl")),
            "rollout-session-789"
        );
    }
}
