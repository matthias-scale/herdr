use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Provider {
    Github,
    Linear,
    Missive,
}

impl Provider {
    pub(super) const fn directory(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Linear => "linear",
            Self::Missive => "missive",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct CachePolicy {
    pub(super) ttl: Duration,
    pub(super) bypass: bool,
}

#[derive(Debug)]
pub(super) struct ProviderOutput {
    pub(super) status: ExitStatus,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
}

#[derive(Clone, Debug)]
pub(super) struct CachedValue {
    pub(super) stdout: Vec<u8>,
    pub(super) fetched_at: SystemTime,
}

#[derive(Clone, Debug)]
pub(super) struct RateLimitState {
    pub(super) until: SystemTime,
    pub(super) reason: String,
}

#[derive(Clone, Debug)]
pub(super) struct ProviderCache {
    root: PathBuf,
    trace: bool,
    counts: Arc<Mutex<InvocationCounts>>,
    outcomes: Arc<Mutex<InvocationOutcomes>>,
    rate_limit_reasons: Arc<Mutex<HashMap<Provider, String>>>,
    now: Option<SystemTime>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct InvocationCounts {
    pub(super) github: usize,
    pub(super) linear: usize,
    pub(super) missive: usize,
}

#[derive(Debug, Default)]
struct InvocationOutcomes {
    successful: HashSet<Provider>,
    rate_limited: HashSet<Provider>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CacheEntry {
    fetched_at: u64,
    stdout_base64: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedRateLimit {
    until_epoch_secs: u64,
    backoff_secs: u64,
    reason: String,
}

impl ProviderCache {
    pub(super) fn new(state_dir: &Path) -> Self {
        Self {
            root: state_dir.join("work-index").join("cache"),
            trace: std::env::var_os("HERDR_WORK_INDEX_TRACE").as_deref() == Some(OsStr::new("1")),
            counts: Arc::new(Mutex::new(InvocationCounts::default())),
            outcomes: Arc::new(Mutex::new(InvocationOutcomes::default())),
            rate_limit_reasons: Arc::new(Mutex::new(HashMap::new())),
            now: None,
        }
    }

    #[cfg(test)]
    pub(super) fn at_time(state_dir: &Path, now: SystemTime) -> Self {
        let mut cache = Self::new(state_dir);
        cache.now = Some(now);
        cache
    }

    pub(super) fn current_time(&self) -> SystemTime {
        self.now.unwrap_or_else(SystemTime::now)
    }

    pub(super) fn counts(&self) -> InvocationCounts {
        self.counts
            .lock()
            .map_or_else(|_| InvocationCounts::default(), |counts| *counts)
    }

    pub(super) fn log_summary(&self) {
        if !self.trace {
            return;
        }
        let counts = self.counts();
        tracing::info!(
            github = counts.github,
            linear = counts.linear,
            missive = counts.missive,
            "work index provider refresh summary"
        );
    }

    pub(super) fn cached(&self, provider: Provider, argv: &[String]) -> Option<CachedValue> {
        read_cache_entry(&self.cache_path(provider, argv))
    }

    pub(super) fn run(
        &self,
        provider: Provider,
        program: &Path,
        argv: &[String],
        policy: CachePolicy,
        deadline: Instant,
    ) -> io::Result<ProviderOutput> {
        let path = self.cache_path(provider, argv);
        if !policy.bypass {
            if let Some(output) = self.fresh_output(&path, policy.ttl) {
                return Ok(output);
            }
        }
        if let Some(backoff) = self.rate_limit(provider) {
            if backoff.until > self.current_time() {
                return Err(io::Error::new(io::ErrorKind::WouldBlock, backoff.reason));
            }
        }

        let lock_path = path.with_extension("lock");
        let lock = if policy.bypass {
            None
        } else {
            Some(acquire_lock(&lock_path, deadline, || {
                self.fresh_output(&path, policy.ttl).is_some()
            })?)
        };
        if !policy.bypass {
            if let Some(output) = self.fresh_output(&path, policy.ttl) {
                drop(lock);
                return Ok(output);
            }
        }

        let started = Instant::now();
        let mut command = crate::noninteractive_process::command(program);
        command.args(argv);
        let result = crate::noninteractive_process::output_with_deadline(command, deadline);
        let duration = started.elapsed();
        self.record_invocation(provider);
        match result {
            Ok(output) => {
                self.trace_invocation(provider, argv, duration, output.stdout.len(), output.status);
                if output.status.success() {
                    self.record_success(provider);
                    if let Err(error) =
                        write_cache_entry(&path, self.current_time(), &output.stdout)
                    {
                        tracing::warn!(
                            provider = provider.directory(),
                            error = %error,
                            "failed to persist work index provider cache"
                        );
                    }
                } else if is_rate_limit_output(&output.stdout, &output.stderr) {
                    self.record_rate_limited(provider);
                    let reason = provider_error_reason(&output.stdout, &output.stderr);
                    if let Err(error) = self.record_rate_limit(provider, &reason) {
                        tracing::warn!(
                            provider = provider.directory(),
                            error = %error,
                            "failed to persist work index rate-limit backoff"
                        );
                    }
                }
                Ok(ProviderOutput {
                    status: output.status,
                    stdout: output.stdout,
                    stderr: output.stderr,
                })
            }
            Err(error) => {
                if self.trace {
                    tracing::info!(
                        provider = provider.directory(),
                        argv = %argv_summary(argv),
                        duration_ms = duration.as_millis(),
                        bytes = 0,
                        exit = -1,
                        error = %error,
                        "work index provider invocation"
                    );
                }
                Err(error)
            }
        }
    }

    pub(super) fn rate_limit(&self, provider: Provider) -> Option<RateLimitState> {
        let bytes = std::fs::read(self.rate_limit_path(provider)).ok()?;
        let state = serde_json::from_slice::<PersistedRateLimit>(&bytes).ok()?;
        let reason = self
            .rate_limit_reasons
            .lock()
            .ok()
            .and_then(|reasons| reasons.get(&provider).cloned())
            .unwrap_or(state.reason);
        Some(RateLimitState {
            until: UNIX_EPOCH + Duration::from_secs(state.until_epoch_secs),
            reason,
        })
    }

    pub(super) fn finish_refresh(&self) {
        let Ok(mut outcomes) = self.outcomes.lock() else {
            return;
        };
        for provider in [Provider::Github, Provider::Linear, Provider::Missive] {
            if outcomes.successful.contains(&provider) && !outcomes.rate_limited.contains(&provider)
            {
                let _ = self.clear_rate_limit(provider);
            }
        }
        outcomes.successful.clear();
        outcomes.rate_limited.clear();
    }

    fn fresh_output(&self, path: &Path, ttl: Duration) -> Option<ProviderOutput> {
        let cached = read_cache_entry(path)?;
        let age = self.current_time().duration_since(cached.fetched_at).ok()?;
        (age < ttl).then(|| ProviderOutput {
            status: success_status(),
            stdout: cached.stdout,
            stderr: Vec::new(),
        })
    }

    fn record_invocation(&self, provider: Provider) {
        let Ok(mut counts) = self.counts.lock() else {
            return;
        };
        match provider {
            Provider::Github => counts.github += 1,
            Provider::Linear => counts.linear += 1,
            Provider::Missive => counts.missive += 1,
        }
    }

    fn record_success(&self, provider: Provider) {
        if let Ok(mut outcomes) = self.outcomes.lock() {
            outcomes.successful.insert(provider);
        }
    }

    fn record_rate_limited(&self, provider: Provider) {
        if let Ok(mut outcomes) = self.outcomes.lock() {
            outcomes.rate_limited.insert(provider);
        }
    }

    fn trace_invocation(
        &self,
        provider: Provider,
        argv: &[String],
        duration: Duration,
        bytes: usize,
        status: ExitStatus,
    ) {
        if !self.trace {
            return;
        }
        tracing::info!(
            provider = provider.directory(),
            argv = %argv_summary(argv),
            duration_ms = duration.as_millis(),
            bytes,
            exit = status.code().unwrap_or(-1),
            "work index provider invocation"
        );
    }

    fn cache_path(&self, provider: Provider, argv: &[String]) -> PathBuf {
        let mut hasher = Sha256::new();
        for arg in argv {
            hasher.update(arg.len().to_le_bytes());
            hasher.update(arg.as_bytes());
        }
        self.root
            .join(provider.directory())
            .join(format!("{}.json", lower_hex(&hasher.finalize())))
    }

    fn rate_limit_path(&self, provider: Provider) -> PathBuf {
        self.root.join(provider.directory()).join("backoff.json")
    }

    fn record_rate_limit(&self, provider: Provider, reason: &str) -> io::Result<()> {
        if let Ok(mut reasons) = self.rate_limit_reasons.lock() {
            reasons.insert(provider, reason.to_string());
        }
        let path = self.rate_limit_path(provider);
        let previous = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<PersistedRateLimit>(&bytes).ok());
        let backoff_secs = previous
            .map(|state| state.backoff_secs.saturating_mul(2))
            .unwrap_or(15 * 60)
            .clamp(15 * 60, 60 * 60);
        let now = epoch_secs(self.current_time());
        let persisted_reason = if reason.to_ascii_lowercase().contains("429") {
            "HTTP 429"
        } else {
            "rate limited"
        };
        let state = PersistedRateLimit {
            until_epoch_secs: now.saturating_add(backoff_secs),
            backoff_secs,
            reason: persisted_reason.to_string(),
        };
        write_json_atomically(&path, &state)
    }

    fn clear_rate_limit(&self, provider: Provider) -> io::Result<()> {
        if let Ok(mut reasons) = self.rate_limit_reasons.lock() {
            reasons.remove(&provider);
        }
        match std::fs::remove_file(self.rate_limit_path(provider)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

struct CacheLock {
    path: PathBuf,
}

impl Drop for CacheLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn acquire_lock(
    path: &Path,
    deadline: Instant,
    cache_filled: impl Fn() -> bool,
) -> io::Result<CacheLock> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(_) => {
                return Ok(CacheLock {
                    path: path.to_path_buf(),
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if cache_filled() {
                    return Ok(CacheLock {
                        path: PathBuf::new(),
                    });
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "timed out waiting for work index cache writer",
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    }
}

fn read_cache_entry(path: &Path) -> Option<CachedValue> {
    let bytes = std::fs::read(path).ok()?;
    let entry = serde_json::from_slice::<CacheEntry>(&bytes).ok()?;
    let stdout = base64::engine::general_purpose::STANDARD
        .decode(entry.stdout_base64)
        .ok()?;
    Some(CachedValue {
        stdout,
        fetched_at: UNIX_EPOCH + Duration::from_secs(entry.fetched_at),
    })
}

fn write_cache_entry(path: &Path, fetched_at: SystemTime, stdout: &[u8]) -> io::Result<()> {
    let entry = CacheEntry {
        fetched_at: epoch_secs(fetched_at),
        stdout_base64: base64::engine::general_purpose::STANDARD.encode(stdout),
    };
    write_json_atomically(path, &entry)
}

fn write_json_atomically(path: &Path, value: &impl Serialize) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = path.with_extension(format!("tmp-{}-{nonce}", std::process::id()));
    let write = std::fs::write(&temp, bytes);
    if let Err(error) = write {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    if let Err(error) = crate::platform::replace_file_durably(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    Ok(())
}

fn argv_summary(argv: &[String]) -> String {
    let mut redacted = Vec::with_capacity(argv.len());
    let mut redact_next = false;
    for arg in argv {
        if redact_next {
            redacted.push("<redacted>".to_string());
            redact_next = false;
            continue;
        }
        if arg.eq_ignore_ascii_case("authorization:")
            || arg
                .to_ascii_lowercase()
                .starts_with("authorization: bearer")
        {
            redacted.push("Authorization: <redacted>".to_string());
            redact_next = arg.eq_ignore_ascii_case("authorization:");
        } else {
            redacted.push(arg.clone());
        }
    }
    redacted.join(" ")
}

fn is_rate_limit_output(stdout: &[u8], stderr: &[u8]) -> bool {
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(stderr)
    )
    .to_ascii_lowercase();
    combined.contains("rate limit")
        || combined.contains("rate limited")
        || combined.contains("http 429")
        || combined.contains("status 429")
        || combined.contains("status\":429")
}

fn provider_error_reason(stdout: &[u8], stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    if !stderr.is_empty() {
        return stderr;
    }
    String::from_utf8_lossy(stdout).trim().to_string()
}

fn epoch_secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(unix)]
fn success_status() -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw(0)
}

#[cfg(windows)]
fn success_status() -> ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    ExitStatus::from_raw(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "herdr-work-index-cache-{name}-{}-{}",
            std::process::id(),
            epoch_secs(SystemTime::now())
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[cfg(unix)]
    fn fixture_program(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = dir.join("provider");
        std::fs::write(
            &path,
            "#!/bin/sh\nprintf '%s\\n' run >> \"$1\"\nprintf '{\"nodes\":[]}'\n",
        )
        .expect("write fixture");
        let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("permissions");
        path
    }

    #[cfg(unix)]
    fn write_fixture_program(path: &Path, contents: &str) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::write(path, contents).expect("write fixture");
        let mut permissions = std::fs::metadata(path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("permissions");
    }

    #[test]
    #[cfg(unix)]
    fn cache_hit_miss_ttl_and_bypass_use_injected_program() {
        let dir = temp_dir("ttl");
        let calls = dir.join("calls");
        let program = fixture_program(&dir);
        let base = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let argv = vec![calls.display().to_string()];
        let policy = CachePolicy {
            ttl: Duration::from_secs(600),
            bypass: false,
        };
        let first = ProviderCache::at_time(&dir, base);
        let second_session = ProviderCache::at_time(&dir, base);
        second_session
            .run(
                Provider::Linear,
                &program,
                &argv,
                policy,
                Instant::now() + Duration::from_secs(2),
            )
            .expect("cache miss");
        first
            .run(
                Provider::Linear,
                &program,
                &argv,
                policy,
                Instant::now() + Duration::from_secs(2),
            )
            .expect("cache hit");
        let expired = ProviderCache::at_time(&dir, base + Duration::from_secs(600));
        expired
            .run(
                Provider::Linear,
                &program,
                &argv,
                policy,
                Instant::now() + Duration::from_secs(2),
            )
            .expect("expired cache");
        expired
            .run(
                Provider::Linear,
                &program,
                &argv,
                CachePolicy {
                    bypass: true,
                    ..policy
                },
                Instant::now() + Duration::from_secs(2),
            )
            .expect("bypass cache");

        let calls = std::fs::read_to_string(calls).expect("read calls");
        assert_eq!(calls.lines().count(), 3);
        assert_eq!(first.counts().linear, 0);
        assert_eq!(second_session.counts().linear, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    #[cfg(unix)]
    fn concurrent_sessions_share_one_provider_invocation() {
        let dir = temp_dir("concurrent");
        let calls = dir.join("calls");
        let program = dir.join("linearis");
        write_fixture_program(
            &program,
            &format!(
                "#!/bin/sh\nprintf 'call\\n' >> '{}'\nsleep 0.1\nprintf '{{}}'\n",
                calls.display()
            ),
        );
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let argv = vec!["--compact".to_string(), "issues".to_string()];
        let handles = (0..2)
            .map(|_| {
                let cache = ProviderCache::new(&dir);
                let program = program.clone();
                let argv = argv.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    cache
                        .run(
                            Provider::Linear,
                            &program,
                            &argv,
                            CachePolicy {
                                ttl: Duration::from_secs(60),
                                bypass: false,
                            },
                            Instant::now() + Duration::from_secs(2),
                        )
                        .expect("shared invocation")
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            assert!(handle.join().expect("session thread").status.success());
        }
        let calls = std::fs::read_to_string(calls).expect("read calls");
        assert_eq!(calls.lines().count(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_cache_entry_is_ignored_and_atomic_write_leaves_no_temp_file() {
        let dir = temp_dir("corrupt");
        let cache = ProviderCache::at_time(&dir, UNIX_EPOCH + Duration::from_secs(20));
        let argv = vec!["issues".to_string(), "list".to_string()];
        let path = cache.cache_path(Provider::Linear, &argv);
        std::fs::create_dir_all(path.parent().expect("cache parent")).expect("create parent");
        std::fs::write(&path, b"not-json").expect("write corrupt cache");
        assert!(cache.cached(Provider::Linear, &argv).is_none());

        write_cache_entry(&path, UNIX_EPOCH + Duration::from_secs(20), b"{}")
            .expect("atomic cache write");
        assert_eq!(
            cache
                .cached(Provider::Linear, &argv)
                .map(|entry| entry.stdout),
            Some(b"{}".to_vec())
        );
        let entries = std::fs::read_dir(path.parent().expect("cache parent"))
            .expect("read parent")
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1);
        assert!(std::fs::read_to_string(&path)
            .expect("read cache entry")
            .contains("\"fetched_at\""));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rate_limit_backoff_doubles_caps_and_resets_on_success() {
        let dir = temp_dir("backoff");
        let base = UNIX_EPOCH + Duration::from_secs(2_000_000);
        let cache = ProviderCache::at_time(&dir, base);
        cache
            .record_rate_limit(Provider::Linear, "Rate limit exceeded")
            .expect("first backoff");
        assert_eq!(
            cache
                .rate_limit(Provider::Linear)
                .and_then(|state| state.until.duration_since(base).ok()),
            Some(Duration::from_secs(15 * 60))
        );
        for expected in [30 * 60, 60 * 60, 60 * 60] {
            cache
                .record_rate_limit(Provider::Linear, "HTTP 429")
                .expect("next backoff");
            assert_eq!(
                cache
                    .rate_limit(Provider::Linear)
                    .and_then(|state| state.until.duration_since(base).ok()),
                Some(Duration::from_secs(expected))
            );
        }
        cache.record_success(Provider::Linear);
        cache.record_rate_limited(Provider::Linear);
        cache.finish_refresh();
        assert!(cache.rate_limit(Provider::Linear).is_some());

        cache.record_success(Provider::Linear);
        cache.finish_refresh();
        assert!(cache.rate_limit(Provider::Linear).is_none());
        cache
            .record_rate_limit(Provider::Linear, "rate limited")
            .expect("backoff after reset");
        assert_eq!(
            cache
                .rate_limit(Provider::Linear)
                .and_then(|state| state.until.duration_since(base).ok()),
            Some(Duration::from_secs(15 * 60))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn argv_summary_redacts_authorization() {
        assert_eq!(
            argv_summary(&[
                "-H".into(),
                "Authorization: Bearer secret".into(),
                "https://example.test".into(),
            ]),
            "-H Authorization: <redacted> https://example.test"
        );
    }

    #[test]
    fn cache_files_never_store_secret_argv() {
        let dir = temp_dir("secrets");
        let cache = ProviderCache::at_time(&dir, UNIX_EPOCH + Duration::from_secs(20));
        let argv = vec![
            "--header".to_string(),
            "Authorization: Bearer should-not-be-stored".to_string(),
            "https://example.test".to_string(),
        ];
        let path = cache.cache_path(Provider::Missive, &argv);
        write_cache_entry(
            &path,
            UNIX_EPOCH + Duration::from_secs(20),
            b"{\"ok\":true}",
        )
        .expect("cache response");
        let stored = std::fs::read_to_string(path).expect("stored cache");
        assert!(!stored.contains("should-not-be-stored"));
        assert!(!stored.contains("Authorization"));
        cache
            .record_rate_limit(
                Provider::Missive,
                "HTTP 429 Authorization: Bearer should-not-be-stored",
            )
            .expect("persist backoff");
        let backoff = std::fs::read_to_string(cache.rate_limit_path(Provider::Missive))
            .expect("stored backoff");
        assert!(!backoff.contains("should-not-be-stored"));
        assert!(!backoff.contains("Authorization"));
        assert!(cache
            .rate_limit(Provider::Missive)
            .is_some_and(|state| state.reason.contains("should-not-be-stored")));
        let _ = std::fs::remove_dir_all(dir);
    }
}
