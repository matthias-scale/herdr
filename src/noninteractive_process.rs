use std::{
    ffi::OsStr,
    io::{self, Read},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Extra time granted to pipe readers after the subprocess tree has been killed:
/// enough for the kernel to deliver EOF, short enough to stay bounded.
const READER_JOIN_GRACE: Duration = Duration::from_millis(250);

/// Builds a subprocess whose stdio is controlled by the caller and which never opens a Windows console.
pub(crate) fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    crate::platform::configure_background_command(&mut command);
    command
}

pub(crate) fn curl_command() -> Command {
    command("curl")
}

/// Captures a subprocess's output, killing and reaping its whole process tree when
/// `deadline` is reached.
///
/// On Unix the child is spawned as the leader of its own process group so expiry can
/// signal every descendant, not just the direct child. On Windows there is no job-object
/// integration yet: only the direct child is terminated, and descendants that inherit
/// the output pipes are covered solely by the bounded reader joins, which detach rather
/// than block past the deadline.
pub(crate) fn output_with_deadline(mut command: Command, deadline: Instant) -> io::Result<Output> {
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "subprocess deadline elapsed before spawn",
        ));
    }

    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group (pgid == child pid) so deadline expiry can terminate the
        // entire subprocess tree with one negative-pid signal.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let mut child = command.spawn()?;
    let process_tree = ProcessTree::for_child(&child);
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("subprocess stdout pipe was unavailable after spawn"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("subprocess stderr pipe was unavailable after spawn"))?;
    let stdout_reader = thread::spawn(move || read_all(stdout));
    let stderr_reader = thread::spawn(move || read_all(stderr));

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = terminate_and_reap(&mut child, process_tree);
                let _ = drain_readers(
                    stdout_reader,
                    stderr_reader,
                    &mut child,
                    process_tree,
                    Instant::now(),
                );
                return Err(error);
            }
        }
        let now = Instant::now();
        if now >= deadline {
            let terminate_result = terminate_and_reap(&mut child, process_tree);
            let _ = drain_readers(
                stdout_reader,
                stderr_reader,
                &mut child,
                process_tree,
                Instant::now(),
            );
            terminate_result?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "subprocess exceeded its deadline",
            ));
        }
        thread::sleep(PROCESS_POLL_INTERVAL.min(deadline.saturating_duration_since(now)));
    };

    // The child exited in time, but a surviving descendant may still hold the output
    // pipes open; reader completion stays bounded by the same deadline.
    let (stdout, stderr) = drain_readers(
        stdout_reader,
        stderr_reader,
        &mut child,
        process_tree,
        deadline,
    )?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

#[derive(Clone, Copy)]
struct ProcessTree {
    #[cfg(unix)]
    pgid: libc::pid_t,
}

impl ProcessTree {
    fn for_child(child: &std::process::Child) -> Self {
        Self {
            #[cfg(unix)]
            pgid: child.id() as libc::pid_t,
        }
    }

    fn kill(self, child: &mut std::process::Child) -> io::Result<()> {
        #[cfg(unix)]
        {
            // The child is the process-group leader, so a negative pgid signal reaches
            // every descendant that inherited the refresh process group.
            let group_result = unsafe { libc::kill(-self.pgid, libc::SIGKILL) };
            if group_result == 0 {
                return Ok(());
            }
            let group_error = io::Error::last_os_error();
            // The group may already be empty (for example after a successful child
            // wait); fall back to the direct child so unrelated errors still surface.
            return match child.kill() {
                Ok(()) => Ok(()),
                Err(_) if group_error.raw_os_error() == Some(libc::ESRCH) => Ok(()),
                Err(kill_error) => Err(kill_error),
            };
        }

        #[cfg(not(unix))]
        {
            // Windows scope: without job objects only the direct child can be
            // terminated. The bounded reader drain still prevents a descendant that
            // inherits a pipe from blocking the refresh worker past its deadline.
            child.kill()
        }
    }
}

fn terminate_and_reap(
    child: &mut std::process::Child,
    process_tree: ProcessTree,
) -> io::Result<()> {
    let kill_result = process_tree.kill(child);
    if let Err(kill_error) = kill_result {
        return match child.try_wait()? {
            Some(_) => Ok(()),
            None => Err(kill_error),
        };
    }
    child.wait().map(|_| ())
}

/// Joins both pipe-reader threads without ever blocking past the deadline. If a
/// descendant still holds a pipe open, kill the process group and allow a short grace
/// period for EOF; a reader that remains blocked is detached instead of joined.
fn drain_readers(
    stdout_reader: thread::JoinHandle<io::Result<Vec<u8>>>,
    stderr_reader: thread::JoinHandle<io::Result<Vec<u8>>>,
    child: &mut std::process::Child,
    process_tree: ProcessTree,
    deadline: Instant,
) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let mut reader_deadline = deadline;
    if !readers_finished(&stdout_reader, &stderr_reader, reader_deadline) {
        let _ = process_tree.kill(child);
        reader_deadline = Instant::now() + READER_JOIN_GRACE;
        if !readers_finished(&stdout_reader, &stderr_reader, reader_deadline) {
            drop(stdout_reader);
            drop(stderr_reader);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "subprocess output pipe was held open past the deadline",
            ));
        }
    }

    Ok((join_reader(stdout_reader)?, join_reader(stderr_reader)?))
}

fn wait_for_reader(reader: &thread::JoinHandle<io::Result<Vec<u8>>>, deadline: Instant) -> bool {
    while !reader.is_finished() {
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        thread::sleep(PROCESS_POLL_INTERVAL.min(deadline.saturating_duration_since(now)));
    }
    true
}

fn readers_finished(
    stdout_reader: &thread::JoinHandle<io::Result<Vec<u8>>>,
    stderr_reader: &thread::JoinHandle<io::Result<Vec<u8>>>,
    deadline: Instant,
) -> bool {
    wait_for_reader(stdout_reader, deadline) && wait_for_reader(stderr_reader, deadline)
}

fn read_all(mut pipe: impl Read) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    pipe.read_to_end(&mut output)?;
    Ok(output)
}

fn join_reader(reader: thread::JoinHandle<io::Result<Vec<u8>>>) -> io::Result<Vec<u8>> {
    reader
        .join()
        .map_err(|_| io::Error::other("subprocess output reader panicked"))?
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use super::*;

    fn fixture_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create fixture dir");
        dir
    }

    fn write_executable(path: &Path, contents: &str) {
        std::fs::write(path, contents).expect("write fixture script");
        let mut permissions = std::fs::metadata(path)
            .expect("fixture script metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("make fixture script executable");
    }

    fn process_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn pid_is_dead(pid: &str) -> bool {
        !std::process::Command::new("kill")
            .args(["-0", pid])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("probe pid")
            .success()
    }

    fn assert_pid_dies(pid: &str, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if pid_is_dead(pid) {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("{what} (pid {pid}) still exists after the deadline");
    }

    // ac1: a refresh-path subprocess that exceeds its hard deadline is killed and reaped.
    #[test]
    fn deadline_kills_and_reaps_slow_process() {
        let _guard = process_test_lock().lock().expect("process test lock");
        let fixture_dir = fixture_dir("slow-git");
        let fake_git = fixture_dir.join("git");
        let pid_file = fixture_dir.join("pid");
        write_executable(
            &fake_git,
            &format!(
                "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nexec sleep 30\n",
                pid_file.display()
            ),
        );

        let started_at = Instant::now();
        let error =
            output_with_deadline(command(&fake_git), started_at + Duration::from_millis(500))
                .expect_err("slow fake git should time out");

        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(started_at.elapsed() < Duration::from_secs(2));
        let pid = std::fs::read_to_string(&pid_file).expect("fake git recorded pid");
        assert_pid_dies(pid.trim(), "timed-out fake git process");

        std::fs::remove_dir_all(fixture_dir).expect("remove fake git fixture");
    }

    // ac2: a subprocess whose descendant holds the output pipes cannot block the caller
    // past the deadline; the whole process tree is killed, including the descendant.
    #[test]
    fn deadline_kills_descendants_and_unblocks_pipe_readers() {
        let _guard = process_test_lock().lock().expect("process test lock");
        let fixture_dir = fixture_dir("descendant-git");
        let fake_git = fixture_dir.join("git");
        let pid_file = fixture_dir.join("pid");
        let descendant_pid_file = fixture_dir.join("descendant-pid");
        // Spawns a descendant (no exec): the shell stays alive in `wait` while the
        // background sleep inherits the output pipes.
        write_executable(
            &fake_git,
            &format!(
                "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nsleep 30 &\nprintf '%s' \"$!\" > '{}'\nwait\n",
                pid_file.display(),
                descendant_pid_file.display()
            ),
        );

        let started_at = Instant::now();
        let error =
            output_with_deadline(command(&fake_git), started_at + Duration::from_millis(500))
                .expect_err("fake git with hung descendant should time out");

        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(
            started_at.elapsed() < Duration::from_secs(2),
            "reader joins must stay bounded despite the descendant's open pipes"
        );
        let shell_pid = std::fs::read_to_string(&pid_file).expect("fake git recorded pid");
        assert_pid_dies(shell_pid.trim(), "timed-out fake git shell");
        let descendant_pid = std::fs::read_to_string(&descendant_pid_file)
            .expect("fake git recorded descendant pid");
        assert_pid_dies(descendant_pid.trim(), "orphaned fake git descendant");

        std::fs::remove_dir_all(fixture_dir).expect("remove fake git fixture");
    }

    // ac2: a subprocess that exits before the deadline but leaks a pipe-holding descendant
    // must not block the caller forever; the leak is detected, killed, and drained.
    #[test]
    fn successful_exit_with_pipe_holding_descendant_stays_bounded() {
        let _guard = process_test_lock().lock().expect("process test lock");
        let fixture_dir = fixture_dir("leaky-git");
        let fake_git = fixture_dir.join("git");
        let descendant_pid_file = fixture_dir.join("descendant-pid");
        // Exits by signal after spawning a descendant; the shell cannot wait for the
        // background job, but the descendant still inherits both output pipes.
        write_executable(
            &fake_git,
            &format!(
                "#!/bin/sh\nsleep 30 &\nprintf '%s' \"$!\" > '{}'\nkill -TERM $$\n",
                descendant_pid_file.display()
            ),
        );

        let started_at = Instant::now();
        let output =
            output_with_deadline(command(&fake_git), started_at + Duration::from_millis(500))
                .expect("collected output once the leaked descendant is killed");

        assert_eq!(
            output.status.code(),
            None,
            "fixture shell should be signaled"
        );
        assert!(
            started_at.elapsed() < Duration::from_secs(2),
            "reader joins must stay bounded despite the descendant's open pipes"
        );
        let descendant_pid = std::fs::read_to_string(&descendant_pid_file)
            .expect("fake git recorded descendant pid");
        assert_pid_dies(descendant_pid.trim(), "leaked fake git descendant");

        std::fs::remove_dir_all(fixture_dir).expect("remove fake git fixture");
    }
}
