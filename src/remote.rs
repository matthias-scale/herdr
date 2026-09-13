mod attach;
mod control;
#[cfg(unix)]
mod host_unix;

use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum RemoteFocusOperationPhase {
    Live = 0,
    Terminal = 1,
}

/// Shared lifecycle state for one remote focus operation.
///
/// The transport changes this state before publishing a failure event, while
/// the app uses it to reject late frames and context transitions that were
/// already queued before the connection ended.
#[derive(Debug)]
pub(crate) struct RemoteFocusOperationState {
    phase: AtomicU8,
}

impl RemoteFocusOperationState {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            phase: AtomicU8::new(RemoteFocusOperationPhase::Live as u8),
        })
    }

    pub(crate) fn terminate(&self) -> bool {
        self.phase
            .compare_exchange(
                RemoteFocusOperationPhase::Live as u8,
                RemoteFocusOperationPhase::Terminal as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.phase.load(Ordering::Acquire) == RemoteFocusOperationPhase::Terminal as u8
    }
}

pub(crate) use attach::*;
pub(crate) use control::SshRemoteFocusTransport;
#[cfg(unix)]
// Test-only transport seams are re-exported for the real socket handshake harness.
#[allow(unused_imports)]
pub(crate) use control::{ControlReadHalf, ControlStream, SshRunner, TimedRead};
#[cfg(unix)]
pub(crate) use host_unix::{run_remote_client_bridge, run_remote_control_bridge};

#[cfg(windows)]
pub(crate) fn run_remote_client_bridge() -> std::io::Result<()> {
    Err(std::io::Error::other(
        "remote Windows hosts are not supported yet",
    ))
}

#[cfg(windows)]
pub(crate) fn run_remote_control_bridge() -> std::io::Result<()> {
    Err(std::io::Error::other(
        "remote Windows hosts are not supported yet",
    ))
}

pub(crate) fn print_remote_error_hint(err: &std::io::Error, target: &str) {
    if is_remote_auth_error(err) {
        eprintln!(
            "hint: verify SSH access first with `{}`.",
            ssh_check_command(target)
        );
        eprintln!(
            "hint: if your SSH key has a passphrase, load it into ssh-agent with `ssh-add` before running `herdr --remote`."
        );
    }
}

fn is_remote_auth_error(err: &std::io::Error) -> bool {
    let message = err.to_string();
    message.contains("Permission denied")
        && (message.contains("(publickey")
            || message.contains("(keyboard-interactive")
            || message.contains("(password"))
}

fn ssh_check_command(target: &str) -> String {
    format!("ssh {}", shell_quote(target))
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(
                    ch,
                    '@' | '%' | '_' | '+' | '=' | ':' | ',' | '.' | '/' | '-'
                )
        })
    {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_auth_error_matches_ssh_auth_denied() {
        let err = std::io::Error::other(
            "remote platform detection failed: user@host: Permission denied (publickey).",
        );

        assert!(is_remote_auth_error(&err));
    }

    #[test]
    fn remote_auth_error_matches_keyboard_interactive_denied() {
        let err = std::io::Error::other(
            "remote server status failed: user@host: Permission denied (keyboard-interactive).",
        );

        assert!(is_remote_auth_error(&err));
    }

    #[test]
    fn remote_auth_error_ignores_non_auth_errors() {
        let err = std::io::Error::other("remote platform detection failed: unsupported platform");

        assert!(!is_remote_auth_error(&err));
    }

    #[test]
    fn ssh_check_command_quotes_remote_target() {
        assert_eq!(ssh_check_command("host name"), "ssh 'host name'");
    }
}
