//! Platform-specific process and filesystem operations.
//!
//! Centralizes OS-dependent behavior behind a clean boundary so core
//! modules don't scatter `#[cfg]` branches through product logic.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundProcess {
    pub pid: u32,
    pub name: String,
    pub argv0: Option<String>,
    pub argv: Option<Vec<String>>,
    pub cmdline: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundJob {
    pub process_group_id: u32,
    pub processes: Vec<ForegroundProcess>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Hangup,
    Terminate,
    Kill,
}

pub(crate) fn detached_custom_command_process(command: &str) -> std::process::Command {
    let mut process = detached_custom_command_process_platform(command);
    configure_background_command(&mut process);
    process
}

pub(crate) fn pane_custom_command_pty_builder(command: &str) -> portable_pty::CommandBuilder {
    pane_custom_command_pty_builder_platform(command)
}

pub(crate) fn apply_pane_runtime_marker(command: &mut portable_pty::CommandBuilder) {
    apply_pane_runtime_marker_platform(command);
}

#[cfg(not(windows))]
fn apply_pane_runtime_marker_platform(_command: &mut portable_pty::CommandBuilder) {}

pub(crate) fn configure_background_command(command: &mut std::process::Command) {
    configure_background_command_platform(command);
}

#[cfg(not(windows))]
fn configure_background_command_platform(_command: &mut std::process::Command) {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlatformCapabilities {
    pub(crate) live_handoff: bool,
    pub(crate) direct_terminal_attach: bool,
    pub(crate) preserve_legacy_doubled_escape_input: bool,
}

pub(crate) const fn capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        live_handoff: cfg!(unix),
        direct_terminal_attach: cfg!(unix),
        preserve_legacy_doubled_escape_input: cfg!(target_os = "macos"),
    }
}

#[cfg(not(windows))]
pub fn launch_server_daemon_command(command: &mut std::process::Command) -> std::io::Result<u32> {
    command.spawn().map(|child| child.id())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn detach_server_daemon_command(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;

    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn current_process_is_detached_server_daemon() -> bool {
    unsafe { libc::getsid(0) == libc::getpid() }
}

/// Raised by the terminal wake-signal handler, consumed by the host resize watcher.
#[cfg(unix)]
static TERMINAL_RESIZE_SIGNALLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn record_terminal_resize_signal(_signal: libc::c_int) {
    TERMINAL_RESIZE_SIGNALLED.store(true, std::sync::atomic::Ordering::Release);
}

/// Records terminal wake signals that size polling can miss.
#[cfg(unix)]
pub(crate) fn watch_terminal_resize_signal() {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction =
        record_terminal_resize_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // Keep blocking stdin and socket reads from failing with EINTR.
    action.sa_flags = libc::SA_RESTART;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGWINCH, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGCONT, &action, std::ptr::null_mut());
    }
}

#[cfg(not(unix))]
pub(crate) fn watch_terminal_resize_signal() {}

/// Returns whether a terminal size change was signalled since the last call.
#[cfg(unix)]
pub(crate) fn take_terminal_resize_signal() -> bool {
    TERMINAL_RESIZE_SIGNALLED.swap(false, std::sync::atomic::Ordering::AcqRel)
}

/// Windows relies on size polling.
#[cfg(not(unix))]
pub(crate) fn take_terminal_resize_signal() -> bool {
    false
}

#[cfg(not(windows))]
pub(crate) fn terminal_title_for_presentation(title: &str) -> &str {
    title
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardCommand {
    pub program: &'static str,
    pub args: &'static [&'static str],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardImage {
    pub bytes: Vec<u8>,
    pub extension: &'static str,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LimitedRead {
    Empty,
    Complete(Vec<u8>),
    Oversized,
}

pub(crate) fn read_limited_reader(
    mut reader: impl std::io::Read,
    max_bytes: usize,
) -> std::io::Result<LimitedRead> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];

    while bytes.len() < max_bytes {
        let remaining = max_bytes - bytes.len();
        let read_len = remaining.min(buffer.len());
        let bytes_read = match reader.read(&mut buffer[..read_len]) {
            Ok(bytes_read) => bytes_read,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        if bytes_read == 0 {
            return if bytes.is_empty() {
                Ok(LimitedRead::Empty)
            } else {
                Ok(LimitedRead::Complete(bytes))
            };
        }
        bytes.extend_from_slice(&buffer[..bytes_read]);
    }

    let mut sentinel = [0_u8; 1];
    loop {
        return match reader.read(&mut sentinel) {
            Ok(0) if bytes.is_empty() => Ok(LimitedRead::Empty),
            Ok(0) => Ok(LimitedRead::Complete(bytes)),
            Ok(_) => Ok(LimitedRead::Oversized),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => Err(err),
        };
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RemoteSshConfigPaths {
    pub(crate) user_config: Option<std::path::PathBuf>,
    pub(crate) system_config: Option<std::path::PathBuf>,
    pub(crate) multiplexing: bool,
}

#[cfg(unix)]
mod unix_common;
#[cfg(unix)]
pub(crate) use unix_common::{begin_cli_output, end_cli_output};

#[cfg(not(unix))]
pub(crate) fn begin_cli_output() {}

#[cfg(not(unix))]
pub(crate) fn end_cli_output() {}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::*;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod fallback;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub use fallback::*;

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod process_tty_tests {
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::process::CommandExt;
    #[cfg(target_os = "linux")]
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    #[test]
    fn process_tty_returns_verified_pty_when_stdin_is_redirected() {
        let marker = std::env::temp_dir().join(format!(
            "herdr-process-tty-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        let pair = portable_pty::native_pty_system()
            .openpty(portable_pty::PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open test PTY");
        let mut command = portable_pty::CommandBuilder::new("/bin/sh");
        command.arg("-c");
        command.arg(format!(
            "exec </dev/null; touch {}; exec sleep 30",
            marker.display()
        ));
        let mut child = pair.slave.spawn_command(command).expect("spawn PTY child");
        let pid = child.process_id().expect("PTY child pid");

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && !marker.exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
        let marker_created = marker.exists();
        let observed = super::process_tty(pid);
        #[cfg(target_os = "linux")]
        let stdin = std::fs::read_link(format!("/proc/{pid}/fd/0")).ok();
        #[cfg(target_os = "linux")]
        let stdout = std::fs::read_link(format!("/proc/{pid}/fd/1")).ok();
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(marker);

        assert!(
            marker_created,
            "test child did not finish redirecting stdin"
        );
        #[cfg(target_os = "linux")]
        assert_eq!(stdin.as_deref(), Some(Path::new("/dev/null")));
        let tty = observed.expect("controlling TTY");
        #[cfg(target_os = "linux")]
        assert_eq!(Some(tty.as_path()), stdout.as_deref());
        #[cfg(target_os = "linux")]
        assert!(tty.starts_with("/dev/pts"), "unexpected TTY {tty:?}");
        #[cfg(target_os = "macos")]
        assert!(
            tty.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("ttys")),
            "unexpected TTY {tty:?}"
        );
        assert!(
            std::fs::metadata(&tty)
                .expect("TTY metadata")
                .file_type()
                .is_char_device(),
            "TTY must be a character device: {tty:?}"
        );
    }

    #[test]
    fn process_tty_returns_none_without_a_controlling_terminal() {
        let mut command = Command::new("/bin/sleep");
        command
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: `setsid` is async-signal-safe and this closure performs no allocation.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        let mut child = command.spawn().expect("spawn detached child");

        let observed = super::process_tty(child.id());
        let _ = child.kill();
        let _ = child.wait();

        assert_eq!(observed, None);
    }
}

/// Returns the process job used for the sidebar foreground-process label.
///
/// Windows has no terminal foreground process group, so its implementation
/// adds a conservative plain-process fallback. Other platforms use their
/// regular foreground job unchanged.
#[cfg(not(target_os = "windows"))]
pub(crate) fn foreground_process_job(child_pid: u32) -> Option<ForegroundJob> {
    foreground_job(child_pid)
}

/// Depth cap for the pane sub-process walk.
///
/// An agent's background command sits two or three levels under the agent
/// process (wrapper shell, command, its own children). The cap only exists so a
/// pathological or cyclic parent table cannot turn a 1.5s refresh into an
/// unbounded walk.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) const DESCENDANT_SCAN_MAX_DEPTH: usize = 8;
/// Process cap for the pane sub-process walk, for the same reason.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) const DESCENDANT_SCAN_MAX_PROCESSES: usize = 64;

/// Collect the live descendants of `root_pid`, breadth-first and capped.
///
/// Only real descendants are returned: a process that daemonizes away from the
/// agent is reparented to init and drops out of this walk, so it cannot hold a
/// pane in any state forever.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn collect_descendant_processes(
    root_pid: u32,
    child_pids: impl Fn(u32) -> Vec<u32>,
    details: impl Fn(u32) -> Option<ForegroundProcess>,
) -> Vec<ForegroundProcess> {
    let mut seen = std::collections::HashSet::from([root_pid]);
    let mut processes = Vec::new();
    let mut frontier = vec![root_pid];

    for _ in 0..DESCENDANT_SCAN_MAX_DEPTH {
        let mut next = Vec::new();
        for pid in frontier {
            for child in child_pids(pid) {
                if !seen.insert(child) {
                    continue;
                }
                if let Some(process) = details(child) {
                    processes.push(process);
                }
                if processes.len() >= DESCENDANT_SCAN_MAX_PROCESSES {
                    return processes;
                }
                next.push(child);
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }

    processes
}

/// Cached native metrics for the full-width top status bar (tmux-parity).
pub(crate) mod status_metrics;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn available_pane_shell_from_job(child_pid: u32, job: ForegroundJob) -> Option<String> {
    if job.process_group_id != child_pid
        || job.processes.iter().any(|process| process.pid != child_pid)
    {
        return None;
    }
    job.processes
        .into_iter()
        .find(|process| process.pid == child_pid)
        .map(|process| process.name)
        .filter(|name| is_pane_shell_process_name(name))
}

fn normalized_process_name(name: &str) -> String {
    name.rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim_start_matches('-')
        .trim_end_matches(".exe")
        .to_ascii_lowercase()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn is_powershell_process_name(name: &str) -> bool {
    matches!(
        normalized_process_name(name).as_str(),
        "pwsh" | "powershell"
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn interactive_unix_shell_command(
    argv: &[String],
    shell_name: &str,
    quote_posix_arg: fn(&str) -> String,
) -> Option<String> {
    let quote = if is_powershell_process_name(shell_name) {
        quote_powershell_arg
    } else {
        quote_posix_arg
    };
    let mut parts = argv.iter();
    let mut command = quote(parts.next()?);
    for part in parts {
        command.push(' ');
        command.push_str(&quote(part));
    }
    Some(command)
}

pub(crate) fn quote_powershell_arg(value: &str) -> String {
    if !value.is_empty()
        && !value.starts_with('-')
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'-' | b'.' | b'/' | b':' | b'+' | b'=')
        })
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "''"))
}

/// The normalized base name of a pane shell, when the process is one.
///
/// Callers that have to tell the shells apart need the normalized name, not
/// just the yes/no answer: `-c`, `-Command` and `/c` all mean "run this one
/// command", but each belongs to a different family.
pub(crate) fn pane_shell_name(name: &str) -> Option<String> {
    let normalized = normalized_process_name(name);
    is_pane_shell_process_name(&normalized).then_some(normalized)
}

pub(crate) fn is_pane_shell_process_name(name: &str) -> bool {
    let normalized = normalized_process_name(name);
    matches!(
        normalized.as_str(),
        "sh" | "bash"
            | "dash"
            | "zsh"
            | "fish"
            | "ksh"
            | "mksh"
            | "csh"
            | "tcsh"
            | "elvish"
            | "xonsh"
            | "nu"
            | "pwsh"
            | "powershell"
            | "cmd"
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn process_agent_hint(_pid: u32) -> Option<crate::detect::Agent> {
    None
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn parse_agent_env_hint(environ: &[u8]) -> Option<crate::detect::Agent> {
    for record in environ.split(|&byte| byte == 0) {
        let Some(value) = record.strip_prefix(b"HERDR_AGENT=") else {
            continue;
        };
        return crate::detect::parse_agent_label(std::str::from_utf8(value).ok()?);
    }
    None
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[derive(Debug)]
pub(crate) struct InputSourceRestore;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn switch_to_ascii_input_source() -> Option<InputSourceRestore> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn pump_input_source_runloop() {}

/// Switches the host keyboard input source while prefix mode is active.
///
/// `App` drives this through a trait so the prefix-mode transitions can be
/// tested with a fake, without touching the real macOS APIs or leaking a
/// platform-specific restore type into `App`.
pub(crate) trait PrefixInputSource {
    /// Switch to an ASCII-capable input source for prefix commands. No-op if
    /// the current source is already ASCII-capable, the platform is
    /// unsupported, or the switch fails. Calling it again before `restore`
    /// keeps the source saved by the first call.
    fn switch_to_ascii(&mut self);

    /// Restore whatever `switch_to_ascii` saved. No-op if nothing was switched.
    fn restore(&mut self);
}

/// Production [`PrefixInputSource`] backed by the per-platform API.
#[derive(Default)]
pub(crate) struct RealPrefixInputSource {
    restore: Option<InputSourceRestore>,
}

impl PrefixInputSource for RealPrefixInputSource {
    fn switch_to_ascii(&mut self) {
        if self.restore.is_none() {
            // Drain pending input-source-change notifications so the read below is fresh (see
            // `pump_input_source_runloop`); a no-op on non-macOS.
            pump_input_source_runloop();
            self.restore = switch_to_ascii_input_source();
        }
    }

    fn restore(&mut self) {
        let _ = self.restore.take();
    }
}

/// Scale a test's subprocess budget for the host platform.
///
/// Several tests give a real spawned process a sub-second window and assert on
/// what finished inside it. Those windows were calibrated on Linux, where a
/// spawn returns in single-digit milliseconds. macOS spends materially longer
/// per spawn, so the same literal expires before the behaviour under test can
/// happen and the assertion reports a timing artifact as a defect.
///
/// Scaling keeps one budget in the source and gives the slower platform the
/// headroom it needs. This is a pure policy constant -- both branches compile on
/// every target -- so it uses `cfg!` rather than a compile gate.
#[cfg(test)]
pub(crate) fn test_spawn_budget(base: std::time::Duration) -> std::time::Duration {
    if cfg!(target_os = "macos") {
        base * 20
    } else {
        base
    }
}

/// How far a symlink chain is followed before the write is refused. Matches the
/// kernel's own `MAXSYMLINKS`, so any chain the OS can resolve resolves here.
const MAX_WRITE_TARGET_SYMLINK_HOPS: usize = 40;

/// Resolve `path` through any symlink chain so an atomic replace lands on the
/// file the symlink points at instead of replacing the link itself.
///
/// Herdr's config and session files are commonly symlinks into a dotfiles or
/// stow checkout. Renaming a temp file over the link detaches it: the managed
/// file becomes an unmanaged regular file, later dotfiles changes never reach
/// it, and a reinstall can relink over it and lose the edit.
///
/// Symlinks are followed manually rather than with `fs::canonicalize`, which
/// requires the target to exist and so excludes the dangling-link case a stow
/// user hits on the very first save. A dangling link resolves to its missing
/// target, so the write creates the managed file. A chain still unresolved
/// after [`MAX_WRITE_TARGET_SYMLINK_HOPS`] hops, or an unreadable link, is an
/// error: the caller must fail the save rather than fall back onto replacing a
/// link whose target it could not determine.
pub(crate) fn resolve_write_target(path: &Path) -> std::io::Result<PathBuf> {
    let mut current = path.to_path_buf();
    for _ in 0..MAX_WRITE_TARGET_SYMLINK_HOPS {
        let Ok(metadata) = std::fs::symlink_metadata(&current) else {
            return Ok(current);
        };
        if !metadata.file_type().is_symlink() {
            return Ok(current);
        }
        // An unreadable link is not a path to write through: fail the save
        // rather than fall back to replacing the link.
        let link = std::fs::read_link(&current)?;
        current = if link.is_absolute() {
            link
        } else {
            current
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(link)
        };
    }
    // The budget is spent, but the last hop may already have landed on a real
    // file. Only a path that is still a symlink here is unresolved, and writing
    // to any path in that chain would replace a link the operator manages.
    match std::fs::symlink_metadata(&current) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            tracing::warn!(
                path = %path.display(),
                "symlink chain deeper than the kernel resolves; refusing the write"
            );
            // ErrorKind::FilesystemLoop is still unstable, so the kind stays
            // Other and the message carries the reason.
            Err(std::io::Error::other(format!(
                "{} exceeds {MAX_WRITE_TARGET_SYMLINK_HOPS} symlink hops",
                path.display()
            )))
        }
        _ => Ok(current),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn symlink_scratch_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-write-target-{}",
            crate::config::test_unique_suffix()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn resolve_write_target_returns_a_plain_path_unchanged() {
        let dir = symlink_scratch_dir();
        let path = dir.join("config.toml");
        std::fs::write(&path, "onboarding = false\n").expect("seed");

        assert_eq!(resolve_write_target(&path).expect("resolve"), path);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_write_target_follows_a_relative_link() {
        let dir = symlink_scratch_dir();
        let target = dir.join("generated.toml");
        let link = dir.join("config.toml");
        std::fs::write(&target, "onboarding = false\n").expect("seed");
        std::os::unix::fs::symlink("generated.toml", &link).expect("symlink");

        assert_eq!(resolve_write_target(&link).expect("resolve"), target);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A dangling link resolves to its missing target, so the write recreates
    /// the managed file instead of replacing the link with a stub.
    #[test]
    fn resolve_write_target_resolves_a_dangling_link_to_its_target() {
        let dir = symlink_scratch_dir();
        let link = dir.join("config.toml");
        std::os::unix::fs::symlink("never-generated.toml", &link).expect("symlink");

        assert_eq!(
            resolve_write_target(&link).expect("resolve"),
            dir.join("never-generated.toml")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The hop budget bounds a loop, not a legitimately deep chain: a chain
    /// that ends on a real file at the last allowed hop must still resolve, or
    /// the write replaces the first link instead of its target.
    #[test]
    fn resolve_write_target_resolves_a_chain_that_ends_on_the_last_hop() {
        let dir = symlink_scratch_dir();
        let target = dir.join("generated.toml");
        std::fs::write(&target, "onboarding = false\n").expect("seed");

        let mut previous = target.clone();
        for hop in 0..MAX_WRITE_TARGET_SYMLINK_HOPS {
            let link = dir.join(format!("link-{hop}.toml"));
            std::os::unix::fs::symlink(&previous, &link).expect("symlink");
            previous = link;
        }

        assert_eq!(resolve_write_target(&previous).expect("resolve"), target);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_write_target_refuses_a_symlink_loop() {
        let dir = symlink_scratch_dir();
        let first = dir.join("config.toml");
        let second = dir.join("other.toml");
        std::os::unix::fs::symlink("other.toml", &first).expect("symlink");
        std::os::unix::fs::symlink("config.toml", &second).expect("symlink");

        let error = resolve_write_target(&first).expect_err("a loop has no write target");
        assert!(
            error.to_string().contains("symlink hops"),
            "unexpected error: {error}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn test_process(pid: u32) -> ForegroundProcess {
        ForegroundProcess {
            pid,
            name: format!("process-{pid}"),
            argv0: None,
            argv: None,
            cmdline: None,
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn descendants_are_collected_through_the_whole_tree() {
        let processes = collect_descendant_processes(
            1,
            |pid| match pid {
                1 => vec![2, 3],
                2 => vec![4],
                _ => Vec::new(),
            },
            |pid| Some(test_process(pid)),
        );

        let pids: Vec<u32> = processes.iter().map(|process| process.pid).collect();
        assert_eq!(pids, vec![2, 3, 4]);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_cyclic_or_endless_parent_table_cannot_stall_the_walk() {
        let cycle =
            collect_descendant_processes(1, |pid| vec![pid % 3 + 1], |pid| Some(test_process(pid)));
        assert!(cycle.len() < DESCENDANT_SCAN_MAX_PROCESSES);

        let unbounded = collect_descendant_processes(
            1,
            |pid| vec![pid * 2, pid * 2 + 1],
            |pid| Some(test_process(pid)),
        );
        assert_eq!(unbounded.len(), DESCENDANT_SCAN_MAX_PROCESSES);
    }

    #[test]
    fn terminal_wake_signals_are_recorded_once_per_delivery() {
        watch_terminal_resize_signal();
        assert!(!take_terminal_resize_signal());

        unsafe { libc::raise(libc::SIGWINCH) };
        assert!(take_terminal_resize_signal());
        assert!(!take_terminal_resize_signal());

        unsafe { libc::raise(libc::SIGCONT) };
        assert!(take_terminal_resize_signal());
        assert!(!take_terminal_resize_signal());
    }

    #[test]
    fn pane_shell_process_names_reject_exec_replacement_programs() {
        for shell in ["bash", "-zsh", "/bin/fish", "pwsh", "powershell.exe"] {
            assert!(is_pane_shell_process_name(shell), "{shell}");
        }
        for program in ["vim", "nvim", "cargo", "test-runner", "opencode"] {
            assert!(!is_pane_shell_process_name(program), "{program}");
        }
    }

    #[test]
    fn detached_custom_command_preserves_unix_login_shell_flag() {
        let cmd = detached_custom_command_process("echo hello");
        assert_eq!(cmd.get_program(), std::ffi::OsStr::new("/bin/sh"));
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            [
                std::ffi::OsStr::new("-lc"),
                std::ffi::OsStr::new("echo hello")
            ]
        );
    }

    #[test]
    fn pane_custom_command_builder_preserves_unix_shell_flag() {
        let expected: Vec<std::ffi::OsString> =
            vec!["/bin/sh".into(), "-c".into(), "echo hello".into()];
        assert_eq!(
            pane_custom_command_pty_builder("echo hello").get_argv(),
            &expected
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn parse_agent_env_hint_accepts_known_agents() {
        assert_eq!(
            parse_agent_env_hint(b"PATH=/bin\0HERDR_AGENT=claude\0TERM=xterm\0"),
            Some(crate::detect::Agent::Claude)
        );
        assert_eq!(
            parse_agent_env_hint(b"HERDR_AGENT=codex"),
            Some(crate::detect::Agent::Codex)
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn parse_agent_env_hint_ignores_missing_or_unknown_agents() {
        assert_eq!(parse_agent_env_hint(b"PATH=/bin\0TERM=xterm\0"), None);
        assert_eq!(parse_agent_env_hint(b"HERDR_AGENT=not-an-agent\0"), None);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn interactive_shell_command_quotes_for_posix_and_powershell() {
        let argv = vec![
            "pi".into(),
            String::new(),
            "two words".into(),
            "a'b".into(),
            "$HOME".into(),
            "semi;colon".into(),
            "@options".into(),
        ];
        assert_eq!(
            interactive_shell_command(&argv, "bash").as_deref(),
            Some("pi '' 'two words' 'a'\\''b' '$HOME' 'semi;colon' @options")
        );
        assert_eq!(
            interactive_shell_command(&argv, "pwsh").as_deref(),
            Some("pi '' 'two words' 'a''b' '$HOME' 'semi;colon' '@options'")
        );
    }

    #[test]
    fn read_limited_reader_returns_complete_data_under_limit() {
        let input = std::io::Cursor::new(b"image".to_vec());
        assert_eq!(
            read_limited_reader(input, 16).expect("limited read"),
            LimitedRead::Complete(b"image".to_vec())
        );
    }

    #[test]
    fn read_limited_reader_returns_empty_for_empty_input() {
        let input = std::io::Cursor::new(Vec::<u8>::new());
        assert_eq!(
            read_limited_reader(input, 16).expect("limited read"),
            LimitedRead::Empty
        );
    }

    #[test]
    fn read_limited_reader_accepts_data_exactly_at_limit() {
        let input = std::io::Cursor::new(b"four".to_vec());
        assert_eq!(
            read_limited_reader(input, 4).expect("limited read"),
            LimitedRead::Complete(b"four".to_vec())
        );
    }

    #[test]
    fn read_limited_reader_rejects_data_over_limit() {
        let input = std::io::Cursor::new(b"oversized".to_vec());
        assert_eq!(
            read_limited_reader(input, 4).expect("limited read"),
            LimitedRead::Oversized
        );
    }

    #[test]
    fn read_limited_reader_retries_interrupted_reads() {
        struct InterruptedOnce {
            interrupted: bool,
            inner: std::io::Cursor<Vec<u8>>,
        }

        impl std::io::Read for InterruptedOnce {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                self.inner.read(buffer)
            }
        }

        let input = InterruptedOnce {
            interrupted: false,
            inner: std::io::Cursor::new(b"image".to_vec()),
        };
        assert_eq!(
            read_limited_reader(input, 16).expect("limited read"),
            LimitedRead::Complete(b"image".to_vec())
        );
    }
}
