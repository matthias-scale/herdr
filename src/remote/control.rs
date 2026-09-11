//! Non-interactive OpenSSH control transport for remote focus.
//
// This transport is intentionally retained as compiled/tested code while the
// production default remains the proxy-pane stub.
#![allow(dead_code)]

#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::sync::{Arc, Mutex};

use crate::api::schema::{AgentRef, ErrorBody};
#[cfg(unix)]
use crate::app::remote_focus::RemoteFocusTransition;
#[cfg(unix)]
use crate::pane::ProxyOutbound;
// The transport trait signature names this on every platform; only the Unix
// session loop uses it beyond the signature.
use crate::pane::RemoteProxyChannels;
#[cfg(unix)]
use crate::protocol::{
    self, ClientKeybindings, ClientLaunchMode, ClientMessage, RenderEncoding, ServerMessage,
    MAX_FRAME_SIZE, PROTOCOL_VERSION,
};

/// The read half of a split control stream. Owns connection diagnostics:
/// after a read failure the SSH stderr explains the loss better than the I/O
/// error does.
#[cfg(unix)]
pub(crate) trait ControlReadHalf: Read + Send {
    fn close_diagnostic(&mut self) -> Option<String> {
        None
    }
}

#[cfg(unix)]
pub(crate) trait ControlStream: Read + Write + Send {
    /// Splits the stream after the handshake so the session can read frames
    /// and write input concurrently. The writer half closes the remote
    /// bridge's stdin when dropped.
    fn split(self: Box<Self>) -> (Box<dyn ControlReadHalf>, Box<dyn Write + Send>);
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
        if target.starts_with('-') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SSH target must not begin with '-'",
            ));
        }
        let mut command = ssh_command(target);
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
fn ssh_command(target: &str) -> std::process::Command {
    let mut command = std::process::Command::new("ssh");
    command
        .arg("-T")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("RequestTTY=no")
        .arg("-o")
        .arg("ConnectTimeout=5")
        .arg("--")
        .arg(target)
        .arg("herdr")
        .arg("remote-control-bridge")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    command
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
    fn split(self: Box<Self>) -> (Box<dyn ControlReadHalf>, Box<dyn Write + Send>) {
        let this = *self;
        (
            Box::new(ProcessControlReader {
                child: this.child,
                stdout: this.stdout,
                stderr: this.stderr,
            }),
            Box::new(this.stdin),
        )
    }
}

#[cfg(unix)]
struct ProcessControlReader {
    child: std::process::Child,
    stdout: std::process::ChildStdout,
    stderr: Option<std::process::ChildStderr>,
}

#[cfg(unix)]
impl Read for ProcessControlReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.stdout.read(buffer)
    }
}

#[cfg(unix)]
impl ControlReadHalf for ProcessControlReader {
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
impl Drop for ProcessControlReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One live control session. `detached` distinguishes an intentional local
/// detach (quiet close) from a connection loss (ambiguous delivery).
#[cfg(unix)]
#[derive(Clone)]
struct SessionHandle {
    outbound_tx: tokio::sync::mpsc::Sender<ProxyOutbound>,
    detached: Arc<AtomicBool>,
    detach_requested: Arc<AtomicBool>,
}

pub(crate) struct SshRemoteFocusTransport {
    #[cfg(unix)]
    runner: Arc<dyn SshRunner>,
    #[cfg(unix)]
    sessions: Arc<Mutex<std::collections::HashMap<String, SessionHandle>>>,
    targets: std::collections::HashMap<String, String>,
}

impl SshRemoteFocusTransport {
    pub(crate) fn new(fleet: &crate::config::FleetConfig) -> Self {
        let admitted_hosts = crate::fleet::select_hosts(fleet, None).unwrap_or_default();
        let targets = admitted_hosts
            .iter()
            .filter(|host| !host.local && !host.target.trim().is_empty())
            .map(|host| (host.name.clone(), host.target.clone()))
            .collect();
        Self {
            #[cfg(unix)]
            runner: Arc::new(OpenSshRunner),
            #[cfg(unix)]
            sessions: Arc::new(Mutex::new(std::collections::HashMap::new())),
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
    #[allow(clippy::too_many_arguments)]
    fn start_with_options(
        &mut self,
        operation_id: &str,
        agent_ref: &AgentRef,
        version: u32,
        build_version: String,
        expected_context: Option<Box<crate::api::schema::RemoteControlContext>>,
        channels: RemoteProxyChannels,
        event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    ) -> Result<(), ErrorBody> {
        let Some(target) = self.targets.get(&agent_ref.host).cloned() else {
            return Err(ErrorBody {
                code: "host_unreachable".to_owned(),
                message: format!("remote host alias {} is not configured", agent_ref.host),
            });
        };
        let operation_id = operation_id.to_owned();
        let agent_ref = agent_ref.clone();
        let runner = Arc::clone(&self.runner);
        let detached = Arc::new(AtomicBool::new(false));
        let detach_requested = Arc::new(AtomicBool::new(false));
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                operation_id.clone(),
                SessionHandle {
                    outbound_tx: channels.detach_tx.clone(),
                    detached: Arc::clone(&detached),
                    detach_requested: Arc::clone(&detach_requested),
                },
            );
        let sessions = Arc::clone(&self.sessions);
        std::thread::Builder::new()
            .name(format!("herdr-remote-focus-{operation_id}"))
            .spawn(move || {
                // A panic anywhere in the session must not bypass teardown:
                // the registration, the detach flag, and the app-side proxy
                // operation would otherwise stay leased forever.
                let cleanup_sessions = Arc::clone(&sessions);
                let cleanup_detached = Arc::clone(&detached);
                let cleanup_operation_id = operation_id.clone();
                let cleanup_event_tx = event_tx.clone();
                let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_control_session(
                        runner,
                        target,
                        operation_id,
                        agent_ref,
                        version,
                        build_version,
                        expected_context,
                        channels,
                        sessions,
                        detached,
                        detach_requested,
                        event_tx,
                    )
                }));
                if let Err(payload) = panicked {
                    SshRemoteFocusTransport::fail(
                        &cleanup_event_tx,
                        &cleanup_operation_id,
                        "connection_lost",
                        "remote control session panicked; delivery of the last accepted batch is unknown",
                    );
                    finish_control_session(
                        &cleanup_sessions,
                        &cleanup_operation_id,
                        &cleanup_detached,
                    );
                    std::panic::resume_unwind(payload);
                }
            })
            .map_err(|error| ErrorBody {
                code: "host_unreachable".to_owned(),
                message: format!("failed to start remote focus connection: {error}"),
            })?;
        Ok(())
    }

    #[cfg(all(test, unix))]
    pub(crate) fn start_with_expected_context_and_version_for_test(
        &mut self,
        operation_id: &str,
        agent_ref: &AgentRef,
        expected_context: Option<crate::api::schema::RemoteControlContext>,
        version: u32,
        channels: RemoteProxyChannels,
        event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    ) -> Result<(), ErrorBody> {
        self.start_with_options(
            operation_id,
            agent_ref,
            version,
            crate::build_info::version().to_owned(),
            expected_context.map(Box::new),
            channels,
            event_tx,
        )
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
        channels: RemoteProxyChannels,
        event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    ) -> Result<(), ErrorBody> {
        #[cfg(not(unix))]
        {
            let _ = (operation_id, agent_ref, channels, event_tx);
            Err(ErrorBody {
                code: "agent_not_attachable".to_owned(),
                message: "remote focus requires a Unix client and Unix server".to_owned(),
            })
        }
        #[cfg(unix)]
        {
            self.start_with_options(
                operation_id,
                agent_ref,
                PROTOCOL_VERSION,
                crate::build_info::version().to_owned(),
                None,
                channels,
                event_tx,
            )
        }
    }

    fn detach(&mut self, operation_id: &str) {
        #[cfg(unix)]
        {
            let handle = self
                .sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(operation_id)
                .cloned();
            let Some(handle) = handle else {
                return;
            };
            handle.detach_requested.store(true, Ordering::Release);
            handle.detached.store(true, Ordering::Release);
            // The flag is the urgent side channel. If the normal queue is
            // full, the writer drops queued input and writes Detach directly
            // instead of waiting behind it.
            let _ = handle.outbound_tx.try_send(ProxyOutbound::Detach);
        }
        #[cfg(not(unix))]
        {
            let _ = operation_id;
        }
    }
}

#[cfg(unix)]
fn read_initial_welcome(
    stream: &mut dyn ControlStream,
) -> Result<ServerMessage, protocol::FramingError> {
    let payload = protocol::read_frame(stream, MAX_FRAME_SIZE)?;
    protocol::decode_frame(&payload)
        .or_else(|error| protocol::decode_legacy_server_welcome(&payload).ok_or(error))
}

/// Ends the session: marks it detached so a late local detach is a no-op and
/// unregisters it. The writer exits on its own: a queued detach, a write
/// failure, or the channel closing once the app and this handle drop their
/// senders. Nothing more is written here — after a connection loss no bytes
/// may reach the wire.
#[cfg(unix)]
fn finish_control_session(
    sessions: &Mutex<std::collections::HashMap<String, SessionHandle>>,
    operation_id: &str,
    detached: &AtomicBool,
) {
    detached.store(true, Ordering::Release);
    sessions
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(operation_id);
}

/// Forwards proxy outbound messages to the wire. Input preserves order
/// relative to resizes; a resize marker reads the latest dimensions from the
/// shared slot. Ends on detach, on a closed channel (best-effort detach so
/// the remote lease is still released), or on a write failure (the reader
/// reports the loss).
#[cfg(unix)]
fn run_control_writer(
    mut writer: Box<dyn Write + Send>,
    mut outbound_rx: tokio::sync::mpsc::Receiver<ProxyOutbound>,
    resize_slot: Arc<Mutex<(u16, u16, u32, u32)>>,
    detached: Arc<AtomicBool>,
    detach_requested: Arc<AtomicBool>,
) {
    while let Some(message) = outbound_rx.blocking_recv() {
        if detach_requested.load(Ordering::Acquire) {
            let _ = protocol::write_message(&mut writer, &ClientMessage::Detach);
            return;
        }
        if detached.load(Ordering::Acquire) {
            return;
        }
        let is_detach = matches!(message, ProxyOutbound::Detach);
        let wire = match message {
            ProxyOutbound::Input(bytes) => ClientMessage::Input {
                data: bytes.to_vec(),
            },
            ProxyOutbound::SyncResize => {
                let (rows, cols, cell_width_px, cell_height_px) =
                    resize_slot.lock().map(|slot| *slot).unwrap_or((0, 0, 0, 0));
                ClientMessage::Resize {
                    cols,
                    rows,
                    cell_width_px,
                    cell_height_px,
                }
            }
            ProxyOutbound::Detach => ClientMessage::Detach,
        };
        if protocol::write_message(&mut writer, &wire).is_err() {
            return;
        }
        if is_detach {
            return;
        }
    }
    if detach_requested.load(Ordering::Acquire) || !detached.load(Ordering::Acquire) {
        let _ = protocol::write_message(&mut writer, &ClientMessage::Detach);
    }
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn run_control_session(
    runner: Arc<dyn SshRunner>,
    target: String,
    operation_id: String,
    agent_ref: AgentRef,
    version: u32,
    build_version: String,
    expected_context: Option<Box<crate::api::schema::RemoteControlContext>>,
    channels: RemoteProxyChannels,
    sessions: Arc<Mutex<std::collections::HashMap<String, SessionHandle>>>,
    detached: Arc<AtomicBool>,
    detach_requested: Arc<AtomicBool>,
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
) {
    let RemoteProxyChannels {
        outbound_rx,
        resize_slot,
        ..
    } = channels;
    let mut stream = match runner.connect(&target) {
        Ok(stream) => stream,
        Err(error) => {
            let code = if error.to_string().contains("Permission denied") {
                "auth_failed"
            } else {
                "host_unreachable"
            };
            SshRemoteFocusTransport::fail(&event_tx, &operation_id, code, error.to_string());
            finish_control_session(&sessions, &operation_id, &detached);
            return;
        }
    };
    let (hello_rows, hello_cols, hello_cell_width_px, hello_cell_height_px) = resize_slot
        .lock()
        .map(|slot| *slot)
        .unwrap_or((40, 120, 0, 0));
    let hello = ClientMessage::Hello {
        version,
        build_version,
        cols: hello_cols.max(2),
        rows: hello_rows.max(1),
        cell_width_px: hello_cell_width_px,
        cell_height_px: hello_cell_height_px,
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
        finish_control_session(&sessions, &operation_id, &detached);
        return;
    }
    let welcome: ServerMessage = match read_initial_welcome(&mut *stream) {
        Ok(message) => message,
        Err(error) => {
            SshRemoteFocusTransport::fail(
                &event_tx,
                &operation_id,
                "host_unreachable",
                format!("remote control handshake read failed: {error}"),
            );
            finish_control_session(&sessions, &operation_id, &detached);
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
        finish_control_session(&sessions, &operation_id, &detached);
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
        finish_control_session(&sessions, &operation_id, &detached);
        return;
    }
    if let Err(error) = protocol::write_message(
        &mut stream,
        &ClientMessage::ControlTerminal {
            target: agent_ref.to_string(),
            agent_ref: Some(agent_ref),
            expected_context,
            takeover: false,
        },
    ) {
        SshRemoteFocusTransport::fail(
            &event_tx,
            &operation_id,
            "host_unreachable",
            format!("remote control request write failed: {error}"),
        );
        finish_control_session(&sessions, &operation_id, &detached);
        return;
    }
    let (mut reader, writer) = stream.split();
    let writer_detached = Arc::clone(&detached);
    let writer_detach_requested = Arc::clone(&detach_requested);
    let writer_resize_slot = Arc::clone(&resize_slot);
    let writer_thread = std::thread::Builder::new()
        .name(format!("herdr-remote-focus-writer-{operation_id}"))
        .spawn(move || {
            run_control_writer(
                writer,
                outbound_rx,
                writer_resize_slot,
                writer_detached,
                writer_detach_requested,
            )
        });
    if let Err(error) = writer_thread {
        SshRemoteFocusTransport::fail(
            &event_tx,
            &operation_id,
            "host_unreachable",
            format!("failed to start remote focus writer: {error}"),
        );
        finish_control_session(&sessions, &operation_id, &detached);
        return;
    }
    let mut active = false;
    loop {
        if detached.load(Ordering::Acquire) {
            break;
        }
        let message: ServerMessage = match protocol::read_message(&mut reader, MAX_FRAME_SIZE) {
            Ok(message) => message,
            Err(error) => {
                if detached.load(Ordering::Acquire) {
                    break;
                }
                let diagnostic = reader.close_diagnostic();
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
                break;
            }
        };
        match message {
            ServerMessage::ControlReady { context } => {
                active = true;
                if event_tx
                    .blocking_send(crate::events::AppEvent::RemoteFocusTransition {
                        operation_id: operation_id.clone(),
                        transition: Box::new(RemoteFocusTransition::Active(context)),
                    })
                    .is_err()
                {
                    break;
                }
            }
            ServerMessage::ControlContext { context } => {
                if active
                    && event_tx
                        .blocking_send(crate::events::AppEvent::RemoteFocusTransition {
                            operation_id: operation_id.clone(),
                            transition: Box::new(RemoteFocusTransition::ContextUpdated(context)),
                        })
                        .is_err()
                {
                    break;
                }
            }
            ServerMessage::Terminal(frame) => {
                // Frames are an ordered diff stream: block on a full event
                // channel rather than drop one and corrupt later diffs.
                if event_tx
                    .blocking_send(crate::events::AppEvent::RemoteFocusFrame {
                        operation_id: operation_id.clone(),
                        frame: Box::new(frame),
                    })
                    .is_err()
                {
                    break;
                }
            }
            ServerMessage::ControlError { code, message } => {
                if !detached.load(Ordering::Acquire) {
                    SshRemoteFocusTransport::fail(&event_tx, &operation_id, &code, message);
                }
                break;
            }
            ServerMessage::ServerShutdown { reason } => {
                if !detached.load(Ordering::Acquire) {
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
                }
                break;
            }
            ServerMessage::Graphics { .. } => {}
            _ => {}
        }
    }
    finish_control_session(&sessions, &operation_id, &detached);
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
        panic_at: Option<u64>,
        read_gate: Option<Arc<AtomicBool>>,
    }

    impl Read for FakeStream {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            // The handshake reads through the whole stream before the split;
            // the gate only holds the post-split reader.
            read_fake_input(&mut self.input, self.fail_at, None, None, buffer)
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

    impl ControlStream for FakeStream {
        fn split(self: Box<Self>) -> (Box<dyn ControlReadHalf>, Box<dyn Write + Send>) {
            let this = *self;
            (
                Box::new(FakeReader {
                    input: this.input,
                    fail_at: this.fail_at,
                    panic_at: this.panic_at,
                    read_gate: this.read_gate,
                }),
                Box::new(FakeWriter {
                    output: this.output,
                }),
            )
        }
    }

    fn read_fake_input(
        input: &mut Cursor<Vec<u8>>,
        fail_at: Option<u64>,
        panic_at: Option<u64>,
        read_gate: Option<&AtomicBool>,
        buffer: &mut [u8],
    ) -> io::Result<usize> {
        // A closed gate holds the reader only once the scripted bytes are
        // consumed, so a test can order a detach before the stream ends
        // without blocking scripted messages.
        while read_gate.is_some_and(|gate| !gate.load(Ordering::Acquire))
            && input.position() >= input.get_ref().len() as u64
        {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        if panic_at.is_some_and(|offset| input.position() >= offset) {
            panic!("injected session panic");
        }
        if fail_at.is_some_and(|offset| input.position() >= offset) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "mid-stream failure",
            ));
        }
        input.read(buffer)
    }

    struct FakeReader {
        input: Cursor<Vec<u8>>,
        fail_at: Option<u64>,
        panic_at: Option<u64>,
        read_gate: Option<Arc<AtomicBool>>,
    }

    impl Read for FakeReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            read_fake_input(
                &mut self.input,
                self.fail_at,
                self.panic_at,
                self.read_gate.as_deref(),
                buffer,
            )
        }
    }

    impl ControlReadHalf for FakeReader {}

    struct FakeWriter {
        output: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for FakeWriter {
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

    fn test_channels() -> (
        RemoteProxyChannels,
        tokio::sync::mpsc::Sender<ProxyOutbound>,
    ) {
        let (outbound_tx, outbound_rx) = tokio::sync::mpsc::channel(8);
        (
            RemoteProxyChannels {
                outbound_rx,
                detach_tx: outbound_tx.clone(),
                resize_slot: Arc::new(Mutex::new((24, 80, 0, 0))),
            },
            outbound_tx,
        )
    }

    fn transport_with(
        input: Vec<u8>,
        output: Arc<Mutex<Vec<u8>>>,
        fail_at: Option<u64>,
    ) -> SshRemoteFocusTransport {
        transport_with_read_gate(input, output, fail_at, None)
    }

    fn transport_with_panic_at(
        input: Vec<u8>,
        output: Arc<Mutex<Vec<u8>>>,
        panic_at: u64,
    ) -> SshRemoteFocusTransport {
        transport_with_fake_stream(input, output, None, Some(panic_at), None)
    }

    fn transport_with_read_gate(
        input: Vec<u8>,
        output: Arc<Mutex<Vec<u8>>>,
        fail_at: Option<u64>,
        read_gate: Option<Arc<AtomicBool>>,
    ) -> SshRemoteFocusTransport {
        transport_with_fake_stream(input, output, fail_at, None, read_gate)
    }

    fn transport_with_fake_stream(
        input: Vec<u8>,
        output: Arc<Mutex<Vec<u8>>>,
        fail_at: Option<u64>,
        panic_at: Option<u64>,
        read_gate: Option<Arc<AtomicBool>>,
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
                    panic_at,
                    read_gate,
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

    fn agent_ref() -> AgentRef {
        AgentRef {
            host: "buildbox".into(),
            agent: "claude".into(),
        }
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

    fn welcome_bytes() -> Vec<u8> {
        framed(&ServerMessage::Welcome {
            version: PROTOCOL_VERSION,
            build_version: crate::build_info::version(),
            encoding: RenderEncoding::TerminalAnsi,
            error: None,
        })
    }

    fn wire_messages(written: &[u8]) -> Vec<ClientMessage> {
        let mut messages = Vec::new();
        let mut input = written;
        while !input.is_empty() {
            let message: ClientMessage =
                protocol::read_message(&mut input, MAX_FRAME_SIZE).expect("wire message parses");
            messages.push(message);
        }
        messages
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
        let (channels, _outbound_tx) = test_channels();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
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
            let (channels, _outbound_tx) = test_channels();
            let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
            transport
                .start("operation", &agent_ref(), "proxy", channels, event_tx)
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
    fn legacy_v21_welcome_is_classified_as_version_skew() {
        #[derive(serde::Serialize)]
        enum LegacyServerMessageV21 {
            Welcome {
                version: u32,
                encoding: RenderEncoding,
                error: Option<String>,
            },
        }

        let output = Arc::new(Mutex::new(Vec::new()));
        let input = framed(&LegacyServerMessageV21::Welcome {
            version: PROTOCOL_VERSION - 1,
            encoding: RenderEncoding::TerminalAnsi,
            error: None,
        });
        let mut transport = transport_with(input, Arc::clone(&output), None);
        let (channels, _outbound_tx) = test_channels();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts");
        assert_eq!(receive_failure(&mut event_rx).code, "version_skew");
    }

    #[test]
    fn ssh_auth_failure_is_not_forwarded_to_the_remote_agent() {
        let mut transport = transport_with_connect_error("Permission denied (publickey)");
        let (channels, _outbound_tx) = test_channels();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts");
        assert_eq!(receive_failure(&mut event_rx).code, "auth_failed");
    }

    #[test]
    fn hostile_ssh_target_is_rejected_before_any_process_spawn() {
        let result = OpenSshRunner.connect("-oProxyCommand=touch /tmp/herdr-owned");
        assert!(matches!(
            result,
            Err(error) if error.kind() == io::ErrorKind::InvalidInput
        ));
    }

    #[test]
    // AC7: the control SSH command is non-interactive and separates the target from the bridge command.
    fn control_ssh_argv_has_batch_mode_and_command_separator() {
        let command = ssh_command("buildbox");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args.windows(2).any(|pair| pair == ["-o", "BatchMode=yes"]));
        let separator = args
            .iter()
            .position(|arg| arg == "--")
            .expect("SSH target separator");
        assert_eq!(
            args.get(separator + 1).map(String::as_str),
            Some("buildbox")
        );
        assert_eq!(
            &args[separator + 2..],
            ["herdr".to_owned(), "remote-control-bridge".to_owned()]
        );
    }

    #[test]
    // AC7: remote focus only resolves the admitted hosts returned by select_hosts.
    fn remote_focus_transport_resolves_hosts_through_select_hosts() {
        let fleet = crate::config::FleetConfig {
            hosts: vec![
                crate::config::FleetHostConfig {
                    name: "laptop".into(),
                    local: true,
                    ..Default::default()
                },
                crate::config::FleetHostConfig {
                    name: "buildbox".into(),
                    target: "operator@buildbox".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let transport = SshRemoteFocusTransport::new(&fleet);
        assert_eq!(
            transport.targets.get("buildbox").map(String::as_str),
            Some("operator@buildbox")
        );
        assert!(!transport.targets.contains_key("laptop"));

        let invalid = crate::config::FleetConfig {
            hosts: vec![
                crate::config::FleetHostConfig {
                    name: "duplicate".into(),
                    target: "one".into(),
                    ..Default::default()
                },
                crate::config::FleetHostConfig {
                    name: "duplicate".into(),
                    target: "two".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert!(SshRemoteFocusTransport::new(&invalid).targets.is_empty());
    }

    #[test]
    // AC6: a typed server ControlError becomes the same typed client failure as other transport errors.
    fn typed_control_error_is_classified_by_the_client() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let welcome = framed(&ServerMessage::Welcome {
            version: PROTOCOL_VERSION,
            build_version: crate::build_info::version(),
            encoding: RenderEncoding::TerminalAnsi,
            error: None,
        });
        let control_error = framed(&ServerMessage::ControlError {
            code: "refused_for_safety".into(),
            message: "controlled PTY write gate refused the batch".into(),
        });
        let mut input = welcome;
        input.extend(control_error);
        let mut transport = transport_with(input, Arc::clone(&output), None);
        let (channels, _outbound_tx) = test_channels();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts");
        let error = receive_failure(&mut event_rx);
        assert_eq!(error.code, "refused_for_safety");
        assert_eq!(error.message, "controlled PTY write gate refused the batch");
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
        let (channels, _outbound_tx) = test_channels();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(2);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
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

    #[test]
    // The proxy pane's screen is the remote terminal: every complete Terminal
    // frame becomes one ordered frame event for the app.
    fn terminal_frames_are_forwarded_as_ordered_frame_events() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut input = welcome_bytes();
        input.extend(framed(&ServerMessage::ControlReady {
            context: Box::new(control_context()),
        }));
        let frame = crate::protocol::TerminalFrame {
            seq: 1,
            width: 80,
            height: 24,
            full: true,
            bytes: b"\x1b[1;1Hremote screen".to_vec(),
        };
        input.extend(framed(&ServerMessage::Terminal(frame.clone())));
        let mut transport = transport_with(input, Arc::clone(&output), None);
        let (channels, _outbound_tx) = test_channels();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(4);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts");
        let first = event_rx.blocking_recv().expect("active event");
        assert!(matches!(
            first,
            crate::events::AppEvent::RemoteFocusTransition { transition, .. }
                if matches!(*transition, RemoteFocusTransition::Active(_))
        ));
        let second = event_rx.blocking_recv().expect("frame event");
        let crate::events::AppEvent::RemoteFocusFrame {
            operation_id,
            frame: received,
        } = second
        else {
            panic!("expected remote focus frame, got {second:?}");
        };
        assert_eq!(operation_id, "operation");
        assert_eq!(*received, frame);
    }

    #[test]
    // Input, the latest resize, and detach reach the wire in order after the
    // control request.
    fn input_resize_and_detach_are_written_in_order() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut input = welcome_bytes();
        input.extend(framed(&ServerMessage::ControlReady {
            context: Box::new(control_context()),
        }));
        // Hold the reader at the stream's end so the outbound messages land
        // before the EOF the remote close would produce.
        let read_gate = Arc::new(AtomicBool::new(false));
        let mut transport = transport_with_read_gate(
            input,
            Arc::clone(&output),
            None,
            Some(Arc::clone(&read_gate)),
        );
        let (channels, outbound_tx) = test_channels();
        let resize_slot = Arc::clone(&channels.resize_slot);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(4);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts");
        let first = event_rx.blocking_recv().expect("active event");
        assert!(matches!(
            first,
            crate::events::AppEvent::RemoteFocusTransition { transition, .. }
                if matches!(*transition, RemoteFocusTransition::Active(_))
        ));
        outbound_tx
            .blocking_send(ProxyOutbound::Input(bytes::Bytes::from_static(b"answer")))
            .expect("input queued");
        *resize_slot.lock().expect("resize slot lock") = (30, 100, 9, 18);
        outbound_tx
            .blocking_send(ProxyOutbound::SyncResize)
            .expect("resize queued");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if wire_messages(&output.lock().expect("fake output lock")).len() >= 4 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "input and resize did not reach the wire before detach"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        transport.detach("operation");
        read_gate.store(true, Ordering::Release);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            {
                let written = output.lock().expect("fake output lock");
                let messages = wire_messages(&written);
                if messages.len() >= 5 {
                    assert!(matches!(messages[0], ClientMessage::Hello { .. }));
                    assert!(matches!(messages[1], ClientMessage::ControlTerminal { .. }));
                    assert!(
                        matches!(&messages[2], ClientMessage::Input { data } if data == b"answer")
                    );
                    assert!(matches!(
                        messages[3],
                        ClientMessage::Resize {
                            cols: 100,
                            rows: 30,
                            cell_width_px: 9,
                            cell_height_px: 18,
                        }
                    ));
                    assert!(matches!(messages[4], ClientMessage::Detach));
                    break;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "input, resize, and detach did not reach the wire: {:?}",
                output.lock().expect("fake output lock").len()
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn detach_preempts_pending_outbound_input() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut input = welcome_bytes();
        input.extend(framed(&ServerMessage::ControlReady {
            context: Box::new(control_context()),
        }));
        let read_gate = Arc::new(AtomicBool::new(false));
        let mut transport = transport_with_read_gate(
            input,
            Arc::clone(&output),
            None,
            Some(Arc::clone(&read_gate)),
        );
        let (outbound_tx, outbound_rx) = tokio::sync::mpsc::channel(1);
        let channels = RemoteProxyChannels {
            outbound_rx,
            detach_tx: outbound_tx.clone(),
            resize_slot: Arc::new(Mutex::new((24, 80, 0, 0))),
        };
        outbound_tx
            .try_send(ProxyOutbound::Input(bytes::Bytes::from_static(b"stale")))
            .expect("pending input queued");
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(4);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts");
        transport.detach("operation");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let messages = wire_messages(&output.lock().expect("fake output lock"));
            if messages.len() >= 3 {
                assert!(matches!(messages[0], ClientMessage::Hello { .. }));
                assert!(matches!(messages[1], ClientMessage::ControlTerminal { .. }));
                assert!(matches!(messages[2], ClientMessage::Detach));
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "detach did not preempt pending input: {messages:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        read_gate.store(true, Ordering::Release);
    }

    #[test]
    // An intentional detach closes quietly: the stream ending afterwards is
    // not a connection loss and no failure reaches the app.
    fn detach_after_activation_is_quiet() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut input = welcome_bytes();
        input.extend(framed(&ServerMessage::ControlReady {
            context: Box::new(control_context()),
        }));
        // Hold the reader before the stream's end so the detach lands first,
        // then release it into the EOF the remote close would produce.
        let read_gate = Arc::new(AtomicBool::new(false));
        let mut transport = transport_with_read_gate(
            input,
            Arc::clone(&output),
            None,
            Some(Arc::clone(&read_gate)),
        );
        let (channels, _outbound_tx) = test_channels();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(4);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts");
        let first = event_rx.blocking_recv().expect("active event");
        assert!(matches!(
            first,
            crate::events::AppEvent::RemoteFocusTransition { transition, .. }
                if matches!(*transition, RemoteFocusTransition::Active(_))
        ));
        transport.detach("operation");
        read_gate.store(true, Ordering::Release);

        // The reader now sees the EOF the remote close produced; an
        // intentional detach must not surface as a failure. The session's
        // event sender drops when it ends, closing the channel quietly.
        let unexpected = event_rx.blocking_recv();
        assert!(
            unexpected.is_none(),
            "detach must not produce failure events: {unexpected:?}"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            {
                let written = output.lock().expect("fake output lock");
                let messages = wire_messages(&written);
                if messages
                    .last()
                    .is_some_and(|message| matches!(message, ClientMessage::Detach))
                {
                    break;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "detach did not reach the wire"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    // A panic inside the session thread must still run teardown: the app
    // hears a failure (closing the proxy operation) and the session
    // registration is removed instead of staying leased forever.
    fn a_session_panic_still_tears_down_registration_and_operation() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut input = welcome_bytes();
        input.extend(framed(&ServerMessage::ControlReady {
            context: Box::new(control_context()),
        }));
        let panic_at = input.len() as u64;
        let mut transport = transport_with_panic_at(input, Arc::clone(&output), panic_at);
        let (channels, _outbound_tx) = test_channels();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(4);
        transport
            .start("operation", &agent_ref(), "proxy", channels, event_tx)
            .expect("thread starts");
        let first = event_rx.blocking_recv().expect("active event");
        assert!(matches!(
            first,
            crate::events::AppEvent::RemoteFocusTransition { transition, .. }
                if matches!(*transition, RemoteFocusTransition::Active(_))
        ));
        let error = receive_failure(&mut event_rx);
        assert_eq!(error.code, "connection_lost");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if transport.sessions.lock().expect("sessions lock").is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the panic left the session registered"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}
