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
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use crate::ui::info_panel::{
    current_unix_timestamp, home_path, parse_codex_record, parse_utc_timestamp, read_file_tail,
    recent_jsonl_files, usage_window, MAX_USAGE_FILES,
};

/// Claude's cache is written by whichever Claude Code session last rendered its
/// statusline. Past this age the numbers describe a window that may already
/// have rolled over, so the bar dims them instead of asserting them.
pub(crate) const CLAUDE_CACHE_STALE_AFTER: Duration = Duration::from_secs(30 * 60);
const KIMI_TIMEOUT: Duration = Duration::from_secs(5);
const KIMI_OUTPUT_LIMIT: usize = 64 * 1024;
const FIVE_HOUR_MINUTES: u64 = 300;
const SEVEN_DAY_MINUTES: u64 = 10_080;

/// One quota window, normalised across providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QuotaWindow {
    pub used_percent: u8,
    /// Unix seconds. `None` when the provider reports a window without a reset,
    /// which Kimi does for a five-hour window that has not started.
    pub resets_at: Option<i64>,
}

/// A single provider account: its short label and its windows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AccountUsage {
    /// Short account code, e.g. `SHQ`. `None` when the account cannot be named.
    pub account: Option<String>,
    pub five_hour: Option<QuotaWindow>,
    pub seven_day: Option<QuotaWindow>,
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
        stale: age.is_some_and(|age| age >= CLAUDE_CACHE_STALE_AFTER),
    }
}

fn load_claude_usage(now_unix: Option<i64>, now: Instant) -> AccountUsage {
    let path = statusline_cache_dir().join("rate-limits.env");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return AccountUsage::default();
    };
    let age = file_age(&path, now);
    let mut usage = parse_claude_rate_limits(&contents, now_unix, age);
    usage.account = claude_account_code(active_profile("CLAUDE_CONFIG_DIR", ".claude").as_deref());
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
        stale: false,
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
    current_unix_timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_787_992_841;

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
}
