//! Non-interactive OpenSSH control transport for remote focus.

#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::sync::Arc;

use crate::api::schema::{AgentRef, ErrorBody};
#[cfg(unix)]
use crate::app::remote_focus::RemoteFocusTransition;
#[cfg(unix)]
use crate::protocol::{
    self, ClientKeybindings, ClientLaunchMode, ClientMessage, RenderEncoding, ServerMessage,
    MAX_FRAME_SIZE, PROTOCOL_VERSION,
};

#[cfg(unix)]
pub(crate) trait ControlStream: Read + Write + Send {
    fn close_diagnostic(&mut self) -> Option<String> {
        None
    }
}

#[cfg(unix)]
pub(crate) trait SshRunner: Send + Sync {
    fn connect(&self, target: &str) -> Result<Box<dyn ControlStream>, io::Error>;
}

#[cfg(unix)]
#[derive(Debug, Default)]
pub(crate) struct OpenSshRunner;

#[cfg(unix)]
impl SshRunner for OpenSshRunner {
    fn connect(&self, target: &str) -> Result<Box<dyn ControlStream>, io::Error> {
        let mut command = std::process::Command::new("ssh");
        command
            .arg("-T")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("RequestTTY=no")
            .arg("-o")
            .arg("ConnectTimeout=5")
            .arg(target)
            .arg("herdr")
            .arg("remote-control-bridge")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = command.spawn()?;
        let Some(stdin) = child.stdin.take() else {
            return Err(io::Error::other("ssh stdin was not available"));
        };
        let Some(stdout) = child.stdout.take() else {
            return Err(io::Error::other("ssh stdout was not available"));
        };
        let stderr = child.stderr.take();
        Ok(Box::new(ProcessControlStream {
            child,
            stdin,
            stdout,
            stderr,
        }))
    }
}

#[cfg(unix)]
struct ProcessControlStream {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: std::process::ChildStdout,
    stderr: Option<std::process::ChildStderr>,
}

#[cfg(unix)]
impl Read for ProcessControlStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.stdout.read(buffer)
    }
}

#[cfg(unix)]
impl Write for ProcessControlStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.stdin.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stdin.flush()
    }
}

#[cfg(unix)]
impl ControlStream for ProcessControlStream {
    fn close_diagnostic(&mut self) -> Option<String> {
        let mut output = Vec::new();
        if let Some(stderr) = &mut self.stderr {
            let _ = stderr.read_to_end(&mut output);
        }
        let _ = self.child.wait();
        (!output.is_empty()).then(|| String::from_utf8_lossy(&output).into_owned())
    }
}

#[cfg(unix)]
impl Drop for ProcessControlStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(crate) struct SshRemoteFocusTransport {
    #[cfg(unix)]
    runner: Arc<dyn SshRunner>,
    targets: std::collections::HashMap<String, String>,
}

impl SshRemoteFocusTransport {
    pub(crate) fn new(fleet: &crate::config::FleetConfig) -> Self {
        let targets = fleet
            .hosts
            .iter()
            .filter(|host| !host.local && !host.target.trim().is_empty())
            .map(|host| (host.name.clone(), host.target.clone()))
            .collect();
        Self {
            #[cfg(unix)]
            runner: Arc::new(OpenSshRunner),
            targets,
        }
    }

    #[cfg(all(test, unix))]
    pub(crate) fn with_runner(
        fleet: &crate::config::FleetConfig,
        runner: Arc<dyn SshRunner>,
    ) -> Self {
        let mut transport = Self::new(fleet);
        transport.runner = runner;
        transport
    }

    #[cfg(unix)]
    fn fail(
        event_tx: &tokio::sync::mpsc::Sender<crate::events::AppEvent>,
        operation_id: &str,
        code: &str,
        message: impl Into<String>,
    ) {
        let _ = event_tx.blocking_send(crate::events::AppEvent::RemoteFocusTransition {
            operation_id: operation_id.to_owned(),
            transition: Box::new(RemoteFocusTransition::Failed(ErrorBody {
                code: code.to_owned(),
                message: message.into(),
            })),
        });
    }
}

impl std::fmt::Debug for SshRemoteFocusTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SshRemoteFocusTransport")
            .field("targets", &self.targets)
            .finish_non_exhaustive()
    }
}

impl crate::app::remote_focus::RemoteFocusTransport for SshRemoteFocusTransport {
    fn start(
        &mut self,
        operation_id: &str,
        agent_ref: &AgentRef,
        _proxy_pane_id: &str,
        event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    ) -> Result<(), ErrorBody> {
        #[cfg(not(unix))]
        {
            let _ = (operation_id, agent_ref, event_tx);
            Err(ErrorBody {
                code: "agent_not_attachable".to_owned(),
                message: "remote focus requires a Unix client and Unix server".to_owned(),
            })
        }
        #[cfg(unix)]
        {
            let Some(target) = self.targets.get(&agent_ref.host).cloned() else {
                return Err(ErrorBody {
                    code: "host_unreachable".to_owned(),
                    message: format!("remote host alias {} is not configured", agent_ref.host),
                });
            };
            let operation_id = operation_id.to_owned();
            let agent_ref = agent_ref.clone();
            let runner = Arc::clone(&self.runner);
            std::thread::Builder::new()
                .name(format!("herdr-remote-focus-{operation_id}"))
                .spawn(move || {
                    run_control_session(runner, target, operation_id, agent_ref, event_tx)
                })
                .map_err(|error| ErrorBody {
                    code: "host_unreachable".to_owned(),
                    message: format!("failed to start remote focus connection: {error}"),
                })?;
            Ok(())
        }
    }
}

#[cfg(unix)]
fn run_control_session(
    runner: Arc<dyn SshRunner>,
    target: String,
    operation_id: String,
    agent_ref: AgentRef,
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
) {
    let mut stream = match runner.connect(&target) {
        Ok(stream) => stream,
        Err(error) => {
            let code = if error.to_string().contains("Permission denied") {
                "auth_failed"
            } else {
                "host_unreachable"
            };
            SshRemoteFocusTransport::fail(&event_tx, &operation_id, code, error.to_string());
            return;
        }
    };
    let hello = ClientMessage::Hello {
        version: PROTOCOL_VERSION,
        build_version: crate::build_info::version(),
        cols: 120,
        rows: 40,
        cell_width_px: 0,
        cell_height_px: 0,
        requested_encoding: RenderEncoding::TerminalAnsi,
        keybindings: ClientKeybindings::Server,
        launch_mode: ClientLaunchMode::TerminalAttach,
    };
    if let Err(error) = protocol::write_message(&mut stream, &hello) {
        SshRemoteFocusTransport::fail(
            &event_tx,
            &operation_id,
            "host_unreachable",
            format!("remote control handshake write failed: {error}"),
        );
        return;
    }
    let welcome: ServerMessage = match protocol::read_message(&mut stream, MAX_FRAME_SIZE) {
        Ok(message) => message,
        Err(error) => {
            SshRemoteFocusTransport::fail(
                &event_tx,
                &operation_id,
                "host_unreachable",
                format!("remote control handshake read failed: {error}"),
            );
            return;
        }
    };
    let ServerMessage::Welcome {
        version,
        build_version,
        error,
        ..
    } = welcome
    else {
        SshRemoteFocusTransport::fail(
            &event_tx,
            &operation_id,
            "version_skew",
            "remote control did not receive a Welcome message",
        );
        return;
    };
    if error.is_some()
        || version != PROTOCOL_VERSION
        || build_version.is_empty()
        || build_version != crate::build_info::version()
    {
        SshRemoteFocusTransport::fail(
            &event_tx,
            &operation_id,
            "version_skew",
            format!(
                "remote control requires protocol {} and build {}, got protocol {} and build {}{}",
                PROTOCOL_VERSION,
                crate::build_info::version(),
                version,
                if build_version.is_empty() {
                    "<missing>"
                } else {
                    &build_version
                },
                error
                    .as_deref()
                    .map_or(String::new(), |error| format!(" ({error})")),
            ),
        );
        return;
    }
    if let Err(error) = protocol::write_message(
        &mut stream,
        &ClientMessage::ControlTerminal {
            target: agent_ref.to_string(),
            agent_ref: Some(agent_ref),
            expected_context: None,
            takeover: false,
        },
    ) {
        SshRemoteFocusTransport::fail(
            &event_tx,
            &operation_id,
            "host_unreachable",
            format!("remote control request write failed: {error}"),
        );
        return;
    }
    let mut active = false;
    loop {
        let message: ServerMessage = match protocol::read_message(&mut stream, MAX_FRAME_SIZE) {
            Ok(message) => message,
            Err(error) => {
                let diagnostic = stream.close_diagnostic();
                let detail = diagnostic
                    .filter(|message| !message.trim().is_empty())
                    .unwrap_or_else(|| error.to_string());
                SshRemoteFocusTransport::fail(
                    &event_tx,
                    &operation_id,
                    if active {
                        "connection_lost"
                    } else if detail.contains("Permission denied") {
                        "auth_failed"
                    } else {
                        "host_unreachable"
                    },
                    if active {
                        format!("remote control connection lost; delivery of the last accepted batch is unknown: {detail}")
                    } else {
                        format!("remote control request read failed: {detail}")
                    },
                );
                return;
            }
        };
        match message {
            ServerMessage::ControlReady { context } => {
                active = true;
                let _ = event_tx.blocking_send(crate::events::AppEvent::RemoteFocusTransition {
                    operation_id: operation_id.clone(),
                    transition: Box::new(RemoteFocusTransition::Active(context)),
                });
            }
            ServerMessage::ControlError { code, message } => {
                SshRemoteFocusTransport::fail(&event_tx, &operation_id, &code, message);
                return;
            }
            ServerMessage::ServerShutdown { reason } => {
                SshRemoteFocusTransport::fail(
                    &event_tx,
                    &operation_id,
                    if active {
                        "connection_lost"
                    } else {
                        "host_unreachable"
                    },
                    reason.unwrap_or_else(|| {
                        "remote control server closed the connection".to_owned()
                    }),
                );
                return;
            }
            ServerMessage::Terminal(_) | ServerMessage::Graphics { .. } => {}
            _ => {}
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::app::remote_focus::RemoteFocusTransport;
    use std::io::Cursor;
    use std::sync::Mutex;

    struct FakeStream {
        input: Cursor<Vec<u8>>,
        output: Arc<Mutex<Vec<u8>>>,
        fail_at: Option<u64>,
    }

    impl Read for FakeStream {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self
                .fail_at
                .is_some_and(|offset| self.input.position() >= offset)
            {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "mid-stream failure",
                ));
            }
            self.input.read(buffer)
        }
    }

    impl Write for FakeStream {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.output
                .lock()
                .expect("fake output lock")
                .extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl ControlStream for FakeStream {}

    struct FakeRunner {
        stream: Mutex<Option<FakeStream>>,
        connect_error: Option<String>,
    }

    impl SshRunner for FakeRunner {
        fn connect(&self, _target: &str) -> Result<Box<dyn ControlStream>, io::Error> {
            if let Some(error) = &self.connect_error {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    error.clone(),
                ));
            }
            self.stream
                .lock()
                .expect("fake runner lock")
                .take()
                .map(|stream| Box::new(stream) as Box<dyn ControlStream>)
                .ok_or_else(|| io::Error::other("no fake connection"))
        }
    }

    fn framed(message: &impl serde::Serialize) -> Vec<u8> {
        let mut output = Vec::new();
        protocol::write_message(&mut output, message).expect("frame fake message");
        output
    }

    fn transport_with(
        input: Vec<u8>,
        output: Arc<Mutex<Vec<u8>>>,
        fail_at: Option<u64>,
    ) -> SshRemoteFocusTransport {
        let fleet = crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "buildbox".into(),
                target: "buildbox".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        SshRemoteFocusTransport::with_runner(
            &fleet,
            Arc::new(FakeRunner {
                stream: Mutex::new(Some(FakeStream {
                    input: Cursor::new(input),
                    output,
                    fail_at,
                })),
                connect_error: None,
            }),
        )
    }

    fn transport_with_connect_error(error: &str) -> SshRemoteFocusTransport {
        let fleet = crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "buildbox".into(),
                target: "buildbox".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        SshRemoteFocusTransport::with_runner(
            &fleet,
            Arc::new(FakeRunner {
                stream: Mutex::new(None),
                connect_error: Some(error.to_owned()),
            }),
        )
    }

    fn receive_failure(
        event_rx: &mut tokio::sync::mpsc::Receiver<crate::events::AppEvent>,
    ) -> ErrorBody {
        let event = event_rx.blocking_recv().expect("failure event");
        let crate::events::AppEvent::RemoteFocusTransition { transition, .. } = event else {
            panic!("expected remote focus transition");
        };
        let RemoteFocusTransition::Failed(error) = *transition else {
            panic!("expected failed remote focus transition");
        };
        error
    }

    fn control_context() -> crate::api::schema::RemoteControlContext {
        crate::api::schema::RemoteControlContext {
            host: "buildbox".into(),
            user: "operator".into(),
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
            pane_id: "w1:p3".into(),
            terminal_id: "terminal-id".into(),
            cwd: "/work/repo".into(),
            foreground_cwd: "/work/repo".into(),
            tty: "/dev/pts/4".into(),
            foreground_process: crate::api::schema::RemoteForegroundProcess {
                pid: 1234,
                process_group_id: 1234,
                name: "agent".into(),
                argv: vec!["agent".into(), "run".into()],
                cwd: "/work/repo".into(),
            },
            detected_agent: "agent".into(),
            interactive_ready: true,
            human_draft: false,
            state_change_seq: 9,
            revision: 41,
            context_epoch: 12,
        }
    }

    #[test]
    fn version_skew_is_reported_before_control_terminal_is_written() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let input = framed(&ServerMessage::Welcome {
            version: PROTOCOL_VERSION - 1,
            build_version: crate::build_info::version(),
            encoding: RenderEncoding::TerminalAnsi,
            error: None,
        });
        let mut transport = transport_with(input, Arc::clone(&output), None);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
        transport
            .start(
                "operation",
                &AgentRef {
                    host: "buildbox".into(),
                    agent: "claude".into(),
                },
                "proxy",
                event_tx,
            )
            .expect("thread starts");
        assert_eq!(receive_failure(&mut event_rx).code, "version_skew");
        let written = output.lock().expect("fake output lock");
        let hello: ClientMessage = protocol::read_message(&mut written.as_slice(), MAX_FRAME_SIZE)
            .expect("hello was written");
        assert!(matches!(hello, ClientMessage::Hello { .. }));
        assert_eq!(written.len(), framed(&hello).len());
    }

    #[test]
    fn build_skew_and_missing_identity_are_rejected_before_control_terminal() {
        for build_version in ["other-build", ""] {
            let output = Arc::new(Mutex::new(Vec::new()));
            let input = framed(&ServerMessage::Welcome {
                version: PROTOCOL_VERSION,
                build_version: build_version.to_owned(),
                encoding: RenderEncoding::TerminalAnsi,
                error: None,
            });
            let mut transport = transport_with(input, Arc::clone(&output), None);
            let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
            transport
                .start(
                    "operation",
                    &AgentRef {
                        host: "buildbox".into(),
                        agent: "claude".into(),
                    },
                    "proxy",
                    event_tx,
                )
                .expect("thread starts");
            assert_eq!(receive_failure(&mut event_rx).code, "version_skew");
            let written = output.lock().expect("fake output lock");
            let mut input = written.as_slice();
            let _: ClientMessage =
                protocol::read_message(&mut input, MAX_FRAME_SIZE).expect("hello was written");
            assert!(
                input.is_empty(),
                "control request must not cross version skew"
            );
        }
    }

    #[test]
    fn ssh_auth_failure_is_not_forwarded_to_the_remote_agent() {
        let mut transport = transport_with_connect_error("Permission denied (publickey)");
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
        transport
            .start(
                "operation",
                &AgentRef {
                    host: "buildbox".into(),
                    agent: "claude".into(),
                },
                "proxy",
                event_tx,
            )
            .expect("thread starts");
        assert_eq!(receive_failure(&mut event_rx).code, "auth_failed");
    }

    #[test]
    fn mid_stream_loss_reports_unknown_delivery_without_replay() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let welcome = framed(&ServerMessage::Welcome {
            version: PROTOCOL_VERSION,
            build_version: crate::build_info::version(),
            encoding: RenderEncoding::TerminalAnsi,
            error: None,
        });
        let ready = framed(&ServerMessage::ControlReady {
            context: Box::new(control_context()),
        });
        let fail_at = (welcome.len() + ready.len()) as u64;
        let mut input = welcome;
        input.extend(ready);
        let mut transport = transport_with(input, Arc::clone(&output), Some(fail_at));
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
        transport
            .start(
                "operation",
                &AgentRef {
                    host: "buildbox".into(),
                    agent: "claude".into(),
                },
                "proxy",
                event_tx,
            )
            .expect("thread starts");
        let first = event_rx.blocking_recv().expect("active event");
        assert!(matches!(
            first,
            crate::events::AppEvent::RemoteFocusTransition { transition, .. }
                if matches!(*transition, RemoteFocusTransition::Active(_))
        ));
        let second = event_rx.blocking_recv().expect("loss event");
        assert!(matches!(
            second,
            crate::events::AppEvent::RemoteFocusTransition { transition, .. }
                if matches!(*transition, RemoteFocusTransition::Failed(ref error)
                    if error.code == "connection_lost" && error.message.contains("delivery")
                )
        ));
        let written = output.lock().expect("fake output lock");
        let mut frames = written.as_slice();
        let _: ClientMessage =
            protocol::read_message(&mut frames, MAX_FRAME_SIZE).expect("hello was written");
        let _: ClientMessage = protocol::read_message(&mut frames, MAX_FRAME_SIZE)
            .expect("control request was written");
        assert!(frames.is_empty(), "loss must not cause a replay");
    }
}
