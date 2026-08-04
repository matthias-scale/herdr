use std::{
    ffi::OsStr,
    io::{self, Read},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Builds a subprocess whose stdio is controlled by the caller and which never opens a Windows console.
pub(crate) fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    crate::platform::configure_background_command(&mut command);
    command
}

pub(crate) fn curl_command() -> Command {
    command("curl")
}

/// Captures a subprocess's output, killing and reaping it when `deadline` is reached.
pub(crate) fn output_with_deadline(mut command: Command, deadline: Instant) -> io::Result<Output> {
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "subprocess deadline elapsed before spawn",
        ));
    }

    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
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
                let _ = terminate_and_reap(&mut child);
                let _ = join_reader(stdout_reader);
                let _ = join_reader(stderr_reader);
                return Err(error);
            }
        }
        let now = Instant::now();
        if now >= deadline {
            let terminate_result = terminate_and_reap(&mut child);
            let _ = join_reader(stdout_reader);
            let _ = join_reader(stderr_reader);
            terminate_result?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "subprocess exceeded its deadline",
            ));
        }
        thread::sleep(PROCESS_POLL_INTERVAL.min(deadline.saturating_duration_since(now)));
    };

    Ok(Output {
        status,
        stdout: join_reader(stdout_reader)?,
        stderr: join_reader(stderr_reader)?,
    })
}

fn terminate_and_reap(child: &mut std::process::Child) -> io::Result<()> {
    match child.kill() {
        Ok(()) => {
            child.wait()?;
            Ok(())
        }
        Err(kill_error) => match child.try_wait()? {
            Some(_) => Ok(()),
            None => Err(kill_error),
        },
    }
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
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use super::*;

    // ac1: a refresh-path subprocess that exceeds its hard deadline is killed and reaped.
    #[test]
    fn deadline_kills_and_reaps_slow_process() {
        let fixture_dir = std::env::temp_dir().join(format!(
            "herdr-slow-git-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&fixture_dir).expect("create fake git fixture");
        let fake_git = fixture_dir.join("git");
        let pid_file = fixture_dir.join("pid");
        std::fs::write(
            &fake_git,
            format!(
                "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nexec sleep 30\n",
                pid_file.display()
            ),
        )
        .expect("write fake git");
        let mut permissions = std::fs::metadata(&fake_git)
            .expect("fake git metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_git, permissions).expect("make fake git executable");

        let started_at = Instant::now();
        let error =
            output_with_deadline(command(&fake_git), started_at + Duration::from_millis(500))
                .expect_err("slow fake git should time out");

        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(started_at.elapsed() < Duration::from_secs(2));
        let pid = std::fs::read_to_string(&pid_file).expect("fake git recorded pid");
        let still_running = std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("probe fake git pid")
            .success();
        assert!(!still_running, "timed-out fake git process still exists");

        std::fs::remove_dir_all(fixture_dir).expect("remove fake git fixture");
    }
}
