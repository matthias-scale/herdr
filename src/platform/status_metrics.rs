//! Shared status-metric data and sampling policy.
//!
//! Native collection lives in the selected `platform/<os>.rs` module. This
//! module deliberately contains no process spawning, network clients, or
//! platform APIs so UI rendering can consume a plain `AppState` snapshot.

use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

pub(crate) const STATUS_METRIC_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
pub(crate) const STATUS_METRIC_STALE_AFTER: Duration = Duration::from_secs(4);
const STATUS_METRIC_REPAINT_INTERVAL: Duration = Duration::from_secs(4);
const COMPATIBLE_WAN_CACHE_TTL: Duration = Duration::from_secs(300);
const COMPATIBLE_WAN_CACHE_MAX_BYTES: u64 = 64;

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct StatusMetrics {
    pub cpu_percent: Option<u8>,
    pub mem_used_gib: Option<f32>,
    pub mem_total_gib: Option<f32>,
    pub battery_percent: Option<u8>,
    pub battery_charging: Option<bool>,
    pub local_ip: Option<String>,
    pub tailscale_ip: Option<String>,
    pub public_ip: Option<String>,
    pub net_down_kib: Option<u64>,
    pub net_up_kib: Option<u64>,
    pub net_kind: NetKind,
    pub vpn_active: bool,
    pub remote_session: bool,
    pub hostname: String,
    pub username: String,
    pub date: String,
    pub time: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
// Windows and fallback collectors currently expose network kind as unavailable.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", test)),
    allow(dead_code)
)]
pub(crate) enum NetKind {
    #[default]
    Unknown,
    Wifi,
    Ethernet,
}

#[derive(Debug, Clone)]
pub(crate) struct StatusMetricsSnapshot {
    pub metrics: StatusMetrics,
    pub sampled_at: Instant,
}

impl StatusMetricsSnapshot {
    pub(crate) fn is_stale_at(&self, now: Instant) -> bool {
        now.duration_since(self.sampled_at) >= STATUS_METRIC_STALE_AFTER
    }
}

#[derive(Debug)]
pub(crate) struct StatusMetricSampler {
    previous_cpu: Option<(u64, u64)>,
    #[cfg(any(unix, test))]
    previous_network: Option<(String, u64, u64, Instant)>,
}

impl StatusMetricSampler {
    pub(crate) fn new() -> Self {
        Self {
            previous_cpu: None,
            #[cfg(any(unix, test))]
            previous_network: None,
        }
    }

    pub(super) fn cpu_percent(&mut self, idle: u64, total: u64) -> Option<u8> {
        let result = self.previous_cpu.map(|(previous_idle, previous_total)| {
            let idle_delta = idle.saturating_sub(previous_idle);
            let total_delta = total.saturating_sub(previous_total);
            if total_delta == 0 {
                return 0;
            }
            let busy = total_delta.saturating_sub(idle_delta) as f64;
            ((busy / total_delta as f64) * 100.0)
                .round()
                .clamp(0.0, 100.0) as u8
        });
        self.previous_cpu = Some((idle, total));
        result.or(Some(0))
    }

    #[cfg(any(unix, test))]
    pub(super) fn bandwidth_kib(
        &mut self,
        interface: &str,
        rx_bytes: u64,
        tx_bytes: u64,
        now: Instant,
    ) -> Option<(u64, u64)> {
        let result = self
            .previous_network
            .as_ref()
            .and_then(
                |(previous_interface, previous_rx, previous_tx, previous_at)| {
                    if previous_interface != interface
                        || rx_bytes < *previous_rx
                        || tx_bytes < *previous_tx
                    {
                        return Some((0, 0));
                    }
                    let seconds = now.duration_since(*previous_at).as_secs_f64();
                    (seconds > 0.0).then(|| {
                        (
                            ((rx_bytes - previous_rx) as f64 / seconds / 1024.0).round() as u64,
                            ((tx_bytes - previous_tx) as f64 / seconds / 1024.0).round() as u64,
                        )
                    })
                },
            )
            .or(Some((0, 0)));
        self.previous_network = Some((interface.to_owned(), rx_bytes, tx_bytes, now));
        result
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct StatusMetricRefresh {
    next_at: Instant,
    in_flight: bool,
    last_repaint_at: Option<Instant>,
}

impl StatusMetricRefresh {
    pub(crate) fn immediate(now: Instant) -> Self {
        Self {
            next_at: now,
            in_flight: false,
            last_repaint_at: None,
        }
    }

    pub(crate) fn deadline(self) -> Option<Instant> {
        (!self.in_flight).then_some(self.next_at)
    }

    pub(crate) fn begin(&mut self, now: Instant) -> bool {
        if self.in_flight || now < self.next_at {
            return false;
        }
        self.in_flight = true;
        self.next_at = now + STATUS_METRIC_REFRESH_INTERVAL;
        true
    }

    pub(crate) fn finish(&mut self) {
        self.in_flight = false;
    }

    pub(crate) fn finish_and_should_repaint(&mut self, sampled_at: Option<Instant>) -> bool {
        self.finish();
        let Some(sampled_at) = sampled_at else {
            return false;
        };
        if self.last_repaint_at.is_some_and(|previous| {
            sampled_at.duration_since(previous) < STATUS_METRIC_REPAINT_INTERVAL
        }) {
            return false;
        }
        self.last_repaint_at = Some(sampled_at);
        true
    }

    #[cfg(test)]
    pub(crate) fn in_flight(self) -> bool {
        self.in_flight
    }
}

pub(super) fn short_hostname(hostname: &str) -> String {
    hostname.split('.').next().unwrap_or(hostname).to_string()
}

pub(super) fn remote_session_from_env() -> bool {
    ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY", "MOSH"]
        .iter()
        .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()))
}

/// Read only the existing tmux-powerline WAN cache. Collection never starts a
/// public-IP request and never writes or refreshes this file.
pub(super) fn compatible_public_ip() -> Option<String> {
    read_compatible_public_ip(Path::new("/tmp/tmux-powerline-wan-ip"), SystemTime::now())
}

fn read_compatible_public_ip(path: &Path, now: SystemTime) -> Option<String> {
    let path_metadata = std::fs::symlink_metadata(path).ok()?;
    if !path_metadata.file_type().is_file() || path_metadata.len() > COMPATIBLE_WAN_CACHE_MAX_BYTES
    {
        return None;
    }

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.file_type().is_file() || metadata.len() > COMPATIBLE_WAN_CACHE_MAX_BYTES {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
            return None;
        }
    }

    let modified = metadata.modified().ok()?;
    if now.duration_since(modified).ok()? > COMPATIBLE_WAN_CACHE_TTL {
        return None;
    }
    let mut bytes = Vec::with_capacity(COMPATIBLE_WAN_CACHE_MAX_BYTES as usize + 1);
    file.take(COMPATIBLE_WAN_CACHE_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > COMPATIBLE_WAN_CACHE_MAX_BYTES {
        return None;
    }
    let text = std::str::from_utf8(&bytes).ok()?;
    let ip = text.trim();
    plausible_ipv4(ip).then(|| ip.to_string())
}

fn plausible_ipv4(value: &str) -> bool {
    let parts: Vec<_> = value.split('.').collect();
    parts.len() == 4 && parts.iter().all(|part| part.parse::<u8>().is_ok())
}

#[cfg(test)]
pub(crate) fn status_metrics_fixture() -> StatusMetrics {
    StatusMetrics {
        cpu_percent: Some(12),
        mem_used_gib: Some(8.0),
        mem_total_gib: Some(16.0),
        battery_percent: Some(88),
        battery_charging: Some(false),
        local_ip: Some("10.0.0.2".into()),
        tailscale_ip: Some("100.64.0.1".into()),
        public_ip: Some("203.0.113.10".into()),
        net_down_kib: Some(120),
        net_up_kib: Some(34),
        net_kind: NetKind::Wifi,
        vpn_active: true,
        remote_session: false,
        hostname: "testhost".into(),
        username: "testuser".into(),
        date: "2026-01-02".into(),
        time: "03:04".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wan_fixture_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("herdr-status-wan-{}-{label}", std::process::id()))
    }

    #[test]
    fn metric_refresh_cadence_is_immediate_bounded_and_retries() {
        // AC5: immediate first start, 2s cadence, max one in flight, and retry.
        let start = Instant::now();
        let mut refresh = StatusMetricRefresh::immediate(start);
        assert!(refresh.begin(start));
        assert!(refresh.in_flight());
        assert!(!refresh.begin(start + Duration::from_secs(2)));
        refresh.finish();
        assert!(!refresh.begin(start + Duration::from_millis(1999)));
        assert!(refresh.begin(start + Duration::from_secs(2)));
    }

    #[test]
    fn metric_refresh_bounds_repaints_without_delaying_samples() {
        // AC5: 2s samples remain bounded while idle full-frame repaints are capped at 4s.
        let start = Instant::now();
        let mut refresh = StatusMetricRefresh::immediate(start);
        assert!(refresh.begin(start));
        assert!(refresh.finish_and_should_repaint(Some(start)));
        assert!(refresh.begin(start + STATUS_METRIC_REFRESH_INTERVAL));
        assert!(!refresh.finish_and_should_repaint(Some(start + STATUS_METRIC_REFRESH_INTERVAL)));
        assert!(refresh.begin(start + STATUS_METRIC_STALE_AFTER));
        assert!(refresh.finish_and_should_repaint(Some(start + STATUS_METRIC_STALE_AFTER)));
    }

    #[test]
    fn metric_stale_boundary_is_injected_and_exact() {
        // AC5: snapshot availability expires deterministically at the 4s boundary.
        let sampled_at = Instant::now();
        let snapshot = StatusMetricsSnapshot {
            metrics: status_metrics_fixture(),
            sampled_at,
        };
        assert!(
            !snapshot.is_stale_at(sampled_at + STATUS_METRIC_STALE_AFTER - Duration::from_nanos(1))
        );
        assert!(snapshot.is_stale_at(sampled_at + STATUS_METRIC_STALE_AFTER));
    }

    #[test]
    fn metric_privacy_cache_accepts_only_fresh_local_values() {
        // AC5/AC6: the shared policy has no HTTP/process path; WAN is read-only cache data.
        let path = wan_fixture_path("fresh");
        std::fs::write(&path, "203.0.113.9\n").expect("write fixture");
        let modified = std::fs::metadata(&path)
            .expect("fixture metadata")
            .modified()
            .expect("fixture timestamp");
        assert_eq!(
            read_compatible_public_ip(&path, modified).as_deref(),
            Some("203.0.113.9")
        );
        assert_eq!(
            read_compatible_public_ip(
                &path,
                modified + COMPATIBLE_WAN_CACHE_TTL + Duration::from_secs(1)
            ),
            None
        );
        std::fs::remove_file(path).expect("remove fixture");
    }

    #[test]
    fn metric_privacy_cache_rejects_oversized_values() {
        let path = wan_fixture_path("oversized");
        std::fs::write(
            &path,
            vec![b'1'; COMPATIBLE_WAN_CACHE_MAX_BYTES as usize + 1],
        )
        .expect("write oversized fixture");
        let modified = std::fs::metadata(&path)
            .expect("fixture metadata")
            .modified()
            .expect("fixture timestamp");

        assert_eq!(read_compatible_public_ip(&path, modified), None);
        std::fs::remove_file(path).expect("remove fixture");
    }

    #[cfg(unix)]
    #[test]
    fn metric_privacy_cache_rejects_symlinks() {
        let target = wan_fixture_path("symlink-target");
        let link = wan_fixture_path("symlink");
        std::fs::write(&target, "203.0.113.9\n").expect("write target");
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");
        let modified = std::fs::metadata(&target)
            .expect("target metadata")
            .modified()
            .expect("target timestamp");

        assert_eq!(read_compatible_public_ip(&link, modified), None);
        std::fs::remove_file(link).expect("remove symlink");
        std::fs::remove_file(target).expect("remove target");
    }

    #[cfg(unix)]
    #[test]
    fn metric_privacy_cache_rejects_fifo_without_blocking() {
        use std::os::unix::ffi::OsStrExt;

        let path = wan_fixture_path("fifo");
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("valid path");
        // SAFETY: `c_path` is a valid NUL-terminated filesystem path.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);

        assert_eq!(read_compatible_public_ip(&path, SystemTime::now()), None);
        std::fs::remove_file(path).expect("remove fifo");
    }

    #[test]
    fn metric_unavailable_and_rate_fallbacks_are_deterministic() {
        // AC3/AC5: first CPU/network samples establish a stable visible baseline.
        let mut sampler = StatusMetricSampler::new();
        let now = Instant::now();
        assert_eq!(sampler.cpu_percent(20, 100), Some(0));
        assert_eq!(sampler.cpu_percent(30, 140), Some(75));
        assert_eq!(sampler.bandwidth_kib("eth0", 1000, 2000, now), Some((0, 0)));
        assert_eq!(
            sampler.bandwidth_kib("eth0", 2024, 3024, now + Duration::from_secs(1)),
            Some((1, 1))
        );
    }

    #[test]
    fn network_interface_change_resets_rate_baseline() {
        let mut sampler = StatusMetricSampler::new();
        let now = Instant::now();
        assert_eq!(
            sampler.bandwidth_kib("eth0", 1_000, 2_000, now),
            Some((0, 0))
        );
        assert_eq!(
            sampler.bandwidth_kib("wlan0", 9_000_000, 8_000_000, now + Duration::from_secs(1)),
            Some((0, 0))
        );
        assert_eq!(
            sampler.bandwidth_kib("wlan0", 9_001_024, 8_002_048, now + Duration::from_secs(2)),
            Some((1, 2))
        );
    }
}
