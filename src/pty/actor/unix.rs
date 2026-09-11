use std::{
    collections::VecDeque,
    io::{Read, Write},
    os::fd::{AsRawFd, OwnedFd, RawFd},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc as std_mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};

use bytes::Bytes;
use tokio::sync::mpsc::{self, error::TryRecvError as DataTryRecvError};
use tracing::{debug, error, warn};

use crate::pty::fd;

// Actor handle methods must call wake_actor() after queuing work. The idle
// timeout is only a fallback for missed wakes; PTY and wake readiness drive
// normal responsiveness.
const ACTOR_IDLE_POLL_MS: i32 = 1000;
const ACTOR_COMMAND_BUFFER: usize = 1024;
const HANDOFF_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActorState {
    Running,
    Quiesced,
    Released,
}

pub(crate) struct PtyReadResult {
    pub terminal_responses: Vec<Bytes>,
}

impl PtyReadResult {
    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        Self {
            terminal_responses: Vec::new(),
        }
    }
}

type ReadCallback = Box<dyn FnMut(&[u8]) -> PtyReadResult + Send + 'static>;
type ReaderExitCallback = Box<dyn FnOnce() + Send + 'static>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PtyResize {
    rows: u16,
    cols: u16,
    cell_width_px: u32,
    cell_height_px: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PtyResizeRequest {
    resize: PtyResize,
    terminal_responses: Vec<Bytes>,
}

#[derive(Default)]
struct SharedPtyControls {
    resize: Option<PtyResizeRequest>,
    nudge: Option<PtyResize>,
    terminal_responses: Vec<Bytes>,
}

pub(crate) struct PtyIoActorConfig {
    pub pane_id: u32,
    pub master_fd: OwnedFd,
    pub initially_quiesced: bool,
    pub on_read: ReadCallback,
    pub on_reader_exit: Option<ReaderExitCallback>,
}

enum PtyIoDataCommand {
    WriteUserInput {
        bytes: Bytes,
        authorization: Option<PtyWriteAuthorization>,
    },
}

/// Shared invalidation state for a runtime's guarded remote writes.
///
/// The boundary mutex serializes lease invalidation with the actor's final
/// validation and syscall. The epoch is an independent check so a missed
/// invalidation site cannot make a queued authorization valid again.
#[derive(Debug, Clone)]
pub(crate) struct PtyWriteGuard {
    active: Arc<AtomicBool>,
    context_epoch: Arc<AtomicU64>,
    authorization_epoch: Arc<AtomicU64>,
    boundary: Arc<Mutex<()>>,
}

impl PtyWriteGuard {
    pub(crate) fn new() -> Self {
        Self {
            active: Arc::new(AtomicBool::new(false)),
            context_epoch: Arc::new(AtomicU64::new(0)),
            authorization_epoch: Arc::new(AtomicU64::new(0)),
            boundary: Arc::new(Mutex::new(())),
        }
    }

    pub(crate) fn lock_boundary(&self) -> std::sync::MutexGuard<'_, ()> {
        self.boundary
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn activate(&self, context_epoch: u64) {
        let _boundary = self.lock_boundary();
        self.context_epoch.store(context_epoch, Ordering::Release);
        self.authorization_epoch.fetch_add(1, Ordering::AcqRel);
        self.active.store(true, Ordering::Release);
    }

    pub(crate) fn revoke(&self) {
        let _boundary = self.lock_boundary();
        self.revoke_locked();
    }

    pub(crate) fn revoke_locked(&self) {
        if self.active.swap(false, Ordering::AcqRel) {
            self.authorization_epoch.fetch_add(1, Ordering::AcqRel);
        }
    }

    pub(crate) fn authorization(
        &self,
        expected_process_group_id: u32,
        expected_context_epoch: u64,
        on_unknown: Arc<dyn Fn() + Send + Sync>,
    ) -> PtyWriteAuthorization {
        let _boundary = self.lock_boundary();
        PtyWriteAuthorization::new_with_context_epoch(
            expected_process_group_id,
            expected_context_epoch,
            self.authorization_epoch.load(Ordering::Acquire),
            Arc::clone(&self.active),
            Arc::clone(&self.context_epoch),
            Arc::clone(&self.authorization_epoch),
            Arc::clone(&self.boundary),
            on_unknown,
        )
    }
}

/// Authorization carried with a guarded remote write until the actor writes it.
/// The actor owns the final check because the channel and pending-write queue
/// outlive the server event-loop turn that accepted the batch.
#[derive(Clone)]
pub(crate) struct PtyWriteAuthorization {
    expected_process_group_id: u32,
    expected_context_epoch: u64,
    expected_authorization_epoch: u64,
    active: Arc<AtomicBool>,
    context_epoch: Arc<AtomicU64>,
    authorization_epoch: Arc<AtomicU64>,
    boundary: Arc<Mutex<()>>,
    reported: Arc<AtomicBool>,
    on_unknown: Arc<dyn Fn() + Send + Sync>,
}

impl PtyWriteAuthorization {
    #[cfg(test)]
    pub(crate) fn new(
        expected_process_group_id: u32,
        active: Arc<AtomicBool>,
        boundary: Arc<Mutex<()>>,
        on_unknown: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self::new_with_context_epoch(
            expected_process_group_id,
            0,
            0,
            active,
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            boundary,
            on_unknown,
        )
    }

    fn new_with_context_epoch(
        expected_process_group_id: u32,
        expected_context_epoch: u64,
        expected_authorization_epoch: u64,
        active: Arc<AtomicBool>,
        context_epoch: Arc<AtomicU64>,
        authorization_epoch: Arc<AtomicU64>,
        boundary: Arc<Mutex<()>>,
        on_unknown: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            expected_process_group_id,
            expected_context_epoch,
            expected_authorization_epoch,
            active,
            context_epoch,
            authorization_epoch,
            boundary,
            reported: Arc::new(AtomicBool::new(false)),
            on_unknown,
        }
    }

    fn is_valid(&self, tty_fd: RawFd) -> bool {
        self.active.load(Ordering::Acquire)
            && self.context_epoch.load(Ordering::Acquire) == self.expected_context_epoch
            && self.authorization_epoch.load(Ordering::Acquire) == self.expected_authorization_epoch
            && crate::platform::foreground_process_group_id_for_tty_fd(tty_fd)
                == Some(self.expected_process_group_id)
    }

    fn report_unknown(&self) {
        if !self.reported.swap(true, Ordering::AcqRel) {
            (self.on_unknown)();
        }
    }
}

enum PtyIoControlCommand {
    BeginHandoff(std_mpsc::Sender<std::io::Result<()>>),
    DuplicateForHandoff(std_mpsc::Sender<std::io::Result<RawFd>>),
    ForegroundProcessGroup(std_mpsc::Sender<Option<u32>>),
    RollbackHandoff(std_mpsc::Sender<std::io::Result<()>>),
    ReleaseAfterCommit(std_mpsc::Sender<std::io::Result<()>>),
    Shutdown,
}

#[derive(Clone)]
pub(crate) struct PtyIoActorHandle {
    data_tx: mpsc::Sender<PtyIoDataCommand>,
    control_tx: std_mpsc::Sender<PtyIoControlCommand>,
    wake: fd::WakeWriter,
    user_writes: Arc<Mutex<UserWriteGate>>,
    user_writes_poisoned: Arc<AtomicBool>,
    write_guard: PtyWriteGuard,
    controls: Arc<Mutex<SharedPtyControls>>,
    response_order: Arc<Mutex<()>>,
}

#[derive(Debug)]
struct UserWriteGate {
    accepting: bool,
    remote_owner: Option<u64>,
}

impl PtyIoActorHandle {
    fn mark_user_writes_poisoned(&self) {
        if !self.user_writes_poisoned.swap(true, Ordering::AcqRel) {
            error!("PTY user-write gate was poisoned; retiring pane and refusing input");
        }
        if self.control_tx.send(PtyIoControlCommand::Shutdown).is_ok() {
            self.wake_actor();
        }
    }

    fn lock_user_writes(&self) -> Option<std::sync::MutexGuard<'_, UserWriteGate>> {
        if self.user_writes_poisoned.load(Ordering::Acquire) {
            return None;
        }
        match self.user_writes.lock() {
            Ok(guard) => Some(guard),
            Err(_) => {
                self.mark_user_writes_poisoned();
                None
            }
        }
    }

    pub(crate) fn remote_control_guard(&self) -> PtyWriteGuard {
        self.write_guard.clone()
    }

    pub(crate) async fn write_user_input(
        &self,
        bytes: Bytes,
    ) -> Result<(), mpsc::error::SendError<Bytes>> {
        let accepting = {
            let Some(user_writes) = self.lock_user_writes() else {
                return Err(mpsc::error::SendError(bytes));
            };
            user_writes.accepting
        };
        if !accepting {
            return Err(mpsc::error::SendError(bytes));
        }

        let permit = match self.data_tx.reserve().await {
            Ok(permit) => permit,
            Err(_) => return Err(mpsc::error::SendError(bytes)),
        };

        let allowed = {
            let Some(user_writes) = self.lock_user_writes() else {
                return Err(mpsc::error::SendError(bytes));
            };
            user_writes.accepting && user_writes.remote_owner.is_none()
        };
        if !allowed {
            return Err(mpsc::error::SendError(bytes));
        }
        permit.send(PtyIoDataCommand::WriteUserInput {
            bytes,
            authorization: None,
        });
        self.wake_actor();
        Ok(())
    }

    pub(crate) fn try_write_user_input(
        &self,
        bytes: Bytes,
    ) -> Result<(), mpsc::error::TrySendError<Bytes>> {
        let Some(user_writes) = self.lock_user_writes() else {
            return Err(mpsc::error::TrySendError::Closed(bytes));
        };
        if !user_writes.accepting {
            return Err(mpsc::error::TrySendError::Closed(bytes));
        }
        if user_writes.remote_owner.is_some() {
            return Err(mpsc::error::TrySendError::Closed(bytes));
        }
        drop(user_writes);
        match self.data_tx.try_send(PtyIoDataCommand::WriteUserInput {
            bytes,
            authorization: None,
        }) {
            Ok(()) => {
                self.wake_actor();
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(PtyIoDataCommand::WriteUserInput {
                bytes,
                authorization: None,
            })) => Err(mpsc::error::TrySendError::Full(bytes)),
            Err(mpsc::error::TrySendError::Closed(PtyIoDataCommand::WriteUserInput {
                bytes,
                authorization: None,
            })) => Err(mpsc::error::TrySendError::Closed(bytes)),
            Err(mpsc::error::TrySendError::Full(PtyIoDataCommand::WriteUserInput {
                authorization: Some(_),
                ..
            }))
            | Err(mpsc::error::TrySendError::Closed(PtyIoDataCommand::WriteUserInput {
                authorization: Some(_),
                ..
            })) => unreachable!("unguarded input cannot carry remote authorization"),
        }
    }

    pub(crate) fn acquire_remote_owner(&self, owner_id: u64) -> bool {
        let Some(mut user_writes) = self.lock_user_writes() else {
            return false;
        };
        match user_writes.remote_owner {
            None => {
                user_writes.remote_owner = Some(owner_id);
                true
            }
            Some(existing) => existing == owner_id,
        }
    }

    pub(crate) fn release_remote_owner(&self, owner_id: u64) {
        let Some(mut user_writes) = self.lock_user_writes() else {
            return;
        };
        if user_writes.remote_owner == Some(owner_id) {
            user_writes.remote_owner = None;
        }
    }

    pub(crate) fn try_write_controlled_user_input(
        &self,
        owner_id: u64,
        bytes: Bytes,
        authorization: PtyWriteAuthorization,
    ) -> Result<(), mpsc::error::TrySendError<Bytes>> {
        let Some(user_writes) = self.lock_user_writes() else {
            return Err(mpsc::error::TrySendError::Closed(bytes));
        };
        if !user_writes.accepting || user_writes.remote_owner != Some(owner_id) {
            return Err(mpsc::error::TrySendError::Closed(bytes));
        }
        drop(user_writes);
        match self.data_tx.try_send(PtyIoDataCommand::WriteUserInput {
            bytes,
            authorization: Some(authorization),
        }) {
            Ok(()) => {
                self.wake_actor();
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(PtyIoDataCommand::WriteUserInput {
                bytes,
                authorization: Some(_),
            })) => Err(mpsc::error::TrySendError::Full(bytes)),
            Err(mpsc::error::TrySendError::Closed(PtyIoDataCommand::WriteUserInput {
                bytes,
                authorization: Some(_),
            })) => Err(mpsc::error::TrySendError::Closed(bytes)),
            Err(_) => unreachable!("controlled input command lost its authorization"),
        }
    }

    pub(crate) fn write_terminal_response(&self, response: impl FnOnce() -> Option<Bytes>) {
        let _order = self
            .response_order
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(bytes) = response() else {
            return;
        };
        if !bytes.is_empty() {
            self.controls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .terminal_responses
                .push(bytes);
            self.wake_actor();
        }
    }

    pub(crate) fn resize(
        &self,
        rows: u16,
        cols: u16,
        cell_width_px: u32,
        cell_height_px: u32,
        terminal_responses: Vec<Bytes>,
    ) {
        {
            let mut controls = self
                .controls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            controls.resize = Some(PtyResizeRequest {
                resize: PtyResize {
                    rows,
                    cols,
                    cell_width_px,
                    cell_height_px,
                },
                terminal_responses,
            });
        }
        self.wake_actor();
    }

    pub(crate) fn nudge_child_redraw_after_handoff(
        &self,
        rows: u16,
        cols: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) {
        {
            let mut controls = self
                .controls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            controls.nudge = Some(PtyResize {
                rows,
                cols,
                cell_width_px,
                cell_height_px,
            });
        }
        self.wake_actor();
    }

    pub(crate) fn begin_handoff(&self, timeout: Duration) -> std::io::Result<()> {
        let (reply_tx, reply_rx) = std_mpsc::channel();
        {
            let Some(mut user_writes) = self.lock_user_writes() else {
                return Err(std::io::Error::other(
                    "PTY user-write gate was poisoned; pane is retiring",
                ));
            };
            user_writes.accepting = false;
            if self
                .control_tx
                .send(PtyIoControlCommand::BeginHandoff(reply_tx))
                .is_err()
            {
                user_writes.accepting = true;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "pty actor closed",
                ));
            }
            self.wake_actor();
        }
        match reply_rx.recv_timeout(timeout) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => {
                let _ = self.rollback_handoff();
                Err(err)
            }
            Err(_) => {
                let _ = self.rollback_handoff();
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out waiting for PTY actor to quiesce",
                ))
            }
        }
    }

    pub(crate) fn duplicate_for_handoff(&self) -> std::io::Result<RawFd> {
        let (reply_tx, reply_rx) = std_mpsc::channel();
        self.control_tx
            .send(PtyIoControlCommand::DuplicateForHandoff(reply_tx))
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pty actor closed"))?;
        self.wake_actor();
        reply_rx.recv_timeout(Duration::from_secs(1)).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out waiting for PTY handoff duplicate",
            )
        })?
    }

    pub(crate) fn foreground_process_group_id(&self) -> Option<u32> {
        let (reply_tx, reply_rx) = std_mpsc::channel();
        self.control_tx
            .send(PtyIoControlCommand::ForegroundProcessGroup(reply_tx))
            .ok()?;
        self.wake_actor();
        reply_rx.recv_timeout(Duration::from_secs(1)).ok()?
    }

    pub(crate) fn rollback_handoff(&self) -> std::io::Result<()> {
        let (reply_tx, reply_rx) = std_mpsc::channel();
        self.control_tx
            .send(PtyIoControlCommand::RollbackHandoff(reply_tx))
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pty actor closed"))?;
        self.wake_actor();
        let result = reply_rx.recv_timeout(Duration::from_secs(1)).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out waiting for PTY handoff rollback",
            )
        })?;
        if result.is_ok() {
            let Some(mut user_writes) = self.lock_user_writes() else {
                return Err(std::io::Error::other(
                    "PTY user-write gate was poisoned; pane is retiring",
                ));
            };
            user_writes.accepting = true;
        }
        result
    }

    pub(crate) fn release_after_commit(&self) -> std::io::Result<()> {
        {
            let Some(mut user_writes) = self.lock_user_writes() else {
                return Err(std::io::Error::other(
                    "PTY user-write gate was poisoned; pane is retiring",
                ));
            };
            user_writes.accepting = false;
        }
        let (reply_tx, reply_rx) = std_mpsc::channel();
        self.control_tx
            .send(PtyIoControlCommand::ReleaseAfterCommit(reply_tx))
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pty actor closed"))?;
        self.wake_actor();
        reply_rx.recv_timeout(Duration::from_secs(1)).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out waiting for PTY actor release",
            )
        })?
    }

    pub(crate) fn shutdown(&self) {
        if let Some(mut user_writes) = self.lock_user_writes() {
            user_writes.accepting = false;
        }
        if self.control_tx.send(PtyIoControlCommand::Shutdown).is_ok() {
            self.wake_actor();
        }
    }

    fn wake_actor(&self) {
        if let Err(err) = self.wake.wake() {
            debug!(err = %err, "failed to wake PTY actor");
        }
    }
}

pub(crate) struct PtyIoActor;

impl PtyIoActor {
    pub(crate) fn spawn(config: PtyIoActorConfig) -> std::io::Result<PtyIoActorHandle> {
        Self::spawn_inner(config, None)
    }

    fn spawn_inner(
        config: PtyIoActorConfig,
        poll_observer: Option<std_mpsc::Sender<()>>,
    ) -> std::io::Result<PtyIoActorHandle> {
        fd::set_cloexec(config.master_fd.as_raw_fd())?;
        fd::set_nonblocking(config.master_fd.as_raw_fd())?;

        let (data_tx, data_rx) = mpsc::channel(ACTOR_COMMAND_BUFFER);
        let (control_tx, control_rx) = std_mpsc::channel();
        let wake_pipe = fd::create_wake_pipe()?;
        let user_writes = Arc::new(Mutex::new(UserWriteGate {
            accepting: !config.initially_quiesced,
            remote_owner: None,
        }));
        let user_writes_poisoned = Arc::new(AtomicBool::new(false));
        let write_guard = PtyWriteGuard::new();
        let controls = Arc::new(Mutex::new(SharedPtyControls::default()));
        let response_order = Arc::new(Mutex::new(()));
        let handle = PtyIoActorHandle {
            data_tx,
            control_tx,
            wake: wake_pipe.writer,
            user_writes: Arc::clone(&user_writes),
            user_writes_poisoned: Arc::clone(&user_writes_poisoned),
            write_guard: write_guard.clone(),
            controls: Arc::clone(&controls),
            response_order: Arc::clone(&response_order),
        };

        let mut runner = PtyIoActorRunner {
            pane_id: config.pane_id,
            file: std::fs::File::from(config.master_fd),
            data_rx,
            control_rx,
            state: if config.initially_quiesced {
                ActorState::Quiesced
            } else {
                ActorState::Running
            },
            pending_writes: VecDeque::new(),
            current_write_offset: 0,
            wake_read_fd: wake_pipe.read_fd,
            user_writes: Arc::clone(&user_writes),
            user_writes_poisoned,
            controls,
            response_order,
            on_read: config.on_read,
            on_reader_exit: config.on_reader_exit,
            poll_observer,
        };
        std::thread::Builder::new()
            .name(format!("herdr-pty-{}", config.pane_id))
            .spawn(move || runner.run())
            .map_err(|err| std::io::Error::other(err.to_string()))?;

        Ok(handle)
    }

    #[cfg(test)]
    fn spawn_with_poll_observer(
        config: PtyIoActorConfig,
        poll_observer: std_mpsc::Sender<()>,
    ) -> std::io::Result<PtyIoActorHandle> {
        Self::spawn_inner(config, Some(poll_observer))
    }
}

struct PtyIoActorRunner {
    pane_id: u32,
    file: std::fs::File,
    data_rx: mpsc::Receiver<PtyIoDataCommand>,
    control_rx: std_mpsc::Receiver<PtyIoControlCommand>,
    state: ActorState,
    pending_writes: VecDeque<(Bytes, Option<PtyWriteAuthorization>, bool)>,
    current_write_offset: usize,
    wake_read_fd: OwnedFd,
    user_writes: Arc<Mutex<UserWriteGate>>,
    user_writes_poisoned: Arc<AtomicBool>,
    controls: Arc<Mutex<SharedPtyControls>>,
    response_order: Arc<Mutex<()>>,
    on_read: ReadCallback,
    on_reader_exit: Option<ReaderExitCallback>,
    poll_observer: Option<std_mpsc::Sender<()>>,
}

impl PtyIoActorRunner {
    fn mark_user_writes_poisoned(&self) {
        if !self.user_writes_poisoned.swap(true, Ordering::AcqRel) {
            error!(
                pane = self.pane_id,
                "PTY user-write gate was poisoned; retiring pane and refusing input"
            );
        }
    }

    fn enqueue_write(&mut self, bytes: Bytes) {
        if !bytes.is_empty() {
            self.pending_writes.push_back((bytes, None, false));
        }
    }

    fn enqueue_write_with_authorization(
        &mut self,
        bytes: Bytes,
        authorization: Option<PtyWriteAuthorization>,
    ) {
        if !bytes.is_empty() {
            self.pending_writes.push_back((bytes, authorization, true));
        }
    }

    fn run(&mut self) {
        let mut should_exit = false;
        while !should_exit {
            should_exit = self.drain_commands();
            if should_exit || self.state == ActorState::Released {
                break;
            }

            self.apply_pending_controls();

            if !self.pending_writes.is_empty() {
                self.flush_pending_writes_once();
                if self.state == ActorState::Released {
                    break;
                }
            }

            if let Some(poll_observer) = &self.poll_observer {
                let _ = poll_observer.send(());
            }

            match fd::poll_pty_and_wake(
                self.file.as_raw_fd(),
                self.wake_read_fd.as_raw_fd(),
                self.state == ActorState::Running,
                !self.pending_writes.is_empty(),
                ACTOR_IDLE_POLL_MS,
            ) {
                Ok(readiness) => {
                    if readiness.wake_ready {
                        if let Err(err) = fd::drain_wake_fd(self.wake_read_fd.as_raw_fd()) {
                            debug!(pane = self.pane_id, err = %err, "PTY actor wake drain failed");
                            break;
                        }
                        continue;
                    }
                    if self.state == ActorState::Running
                        && readiness.pty_read_ready
                        && !self.read_once()
                    {
                        break;
                    }
                    if readiness.pty_write_ready && !self.pending_writes.is_empty() {
                        self.flush_pending_writes_once();
                        if self.state == ActorState::Released {
                            break;
                        }
                    }
                }
                Err(err) => {
                    debug!(pane = self.pane_id, err = %err, "PTY actor poll failed");
                    break;
                }
            }
        }

        if let Some(on_reader_exit) = self.on_reader_exit.take() {
            self.report_unknown_pending_writes();
            self.report_unknown_queued_writes();
            on_reader_exit();
        }
        debug!(pane = self.pane_id, "PTY actor exiting");
    }

    fn drain_commands(&mut self) -> bool {
        if self.drain_control_commands() {
            return true;
        }
        self.drain_data_commands()
    }

    fn drain_control_commands(&mut self) -> bool {
        let mut should_exit = false;
        loop {
            match self.control_rx.try_recv() {
                Ok(command) => {
                    if self.handle_control_command(command) {
                        should_exit = true;
                        break;
                    }
                }
                Err(std_mpsc::TryRecvError::Empty) => break,
                Err(std_mpsc::TryRecvError::Disconnected) => {
                    should_exit = true;
                    break;
                }
            }
        }
        should_exit
    }

    fn drain_data_commands(&mut self) -> bool {
        let mut should_exit = false;
        loop {
            match self.data_rx.try_recv() {
                Ok(command) => {
                    if self.handle_data_command(command) {
                        should_exit = true;
                        break;
                    }
                }
                Err(DataTryRecvError::Empty) => break,
                Err(DataTryRecvError::Disconnected) => {
                    should_exit = true;
                    break;
                }
            }
        }
        should_exit
    }

    fn handle_data_command(&mut self, command: PtyIoDataCommand) -> bool {
        match command {
            PtyIoDataCommand::WriteUserInput {
                bytes,
                authorization,
            } => {
                if self.state == ActorState::Running {
                    self.enqueue_write_with_authorization(bytes, authorization);
                } else if let Some(authorization) = authorization {
                    authorization.report_unknown();
                }
            }
        }
        false
    }

    fn handle_control_command(&mut self, command: PtyIoControlCommand) -> bool {
        match command {
            PtyIoControlCommand::BeginHandoff(reply) => {
                let result = self.begin_handoff();
                let _ = reply.send(result);
            }
            PtyIoControlCommand::DuplicateForHandoff(reply) => {
                let result = if self.state == ActorState::Quiesced {
                    fd::duplicate_cloexec_fd(self.file.as_raw_fd())
                } else {
                    Err(std::io::Error::other(
                        "PTY actor must be quiesced before handoff duplication",
                    ))
                };
                let _ = reply.send(result);
            }
            PtyIoControlCommand::ForegroundProcessGroup(reply) => {
                let result =
                    crate::platform::foreground_process_group_id_for_tty_fd(self.file.as_raw_fd());
                let _ = reply.send(result);
            }
            PtyIoControlCommand::RollbackHandoff(reply) => {
                let result = if self.state == ActorState::Released {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "PTY actor was released before handoff rollback",
                    ))
                } else {
                    self.state = ActorState::Running;
                    Ok(())
                };
                let _ = reply.send(result);
            }
            PtyIoControlCommand::ReleaseAfterCommit(reply) => {
                self.state = ActorState::Released;
                self.report_unknown_pending_writes();
                self.report_unknown_queued_writes();
                self.pending_writes.clear();
                let _ = reply.send(Ok(()));
                return true;
            }
            PtyIoControlCommand::Shutdown => return true,
        }
        false
    }

    fn begin_handoff(&mut self) -> std::io::Result<()> {
        self.drain_pre_quiesce_commands();
        self.apply_pending_controls();
        if self.state == ActorState::Released {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "PTY actor was released before handoff quiesce",
            ));
        }
        let deadline = Instant::now() + HANDOFF_DRAIN_TIMEOUT;
        self.flush_pending_writes_once();
        while !self.pending_writes.is_empty() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out draining PTY writes before handoff",
                ));
            }
            let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
            let readiness = fd::poll_pty_and_wake(
                self.file.as_raw_fd(),
                self.wake_read_fd.as_raw_fd(),
                true,
                true,
                timeout_ms,
            )?;
            if readiness.wake_ready {
                fd::drain_wake_fd(self.wake_read_fd.as_raw_fd())?;
            }
            if readiness.pty_read_ready && !self.read_once() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "PTY closed while draining writes before handoff",
                ));
            }
            if readiness.pty_write_ready {
                self.flush_pending_writes_once();
            }
        }
        self.state = ActorState::Quiesced;
        Ok(())
    }

    fn drain_pre_quiesce_commands(&mut self) {
        while let Ok(PtyIoDataCommand::WriteUserInput {
            bytes,
            authorization,
        }) = self.data_rx.try_recv()
        {
            if self.state != ActorState::Released {
                self.enqueue_write_with_authorization(bytes, authorization);
            } else if let Some(authorization) = authorization {
                authorization.report_unknown();
            }
        }
    }

    fn apply_pending_controls(&mut self) {
        let (resize, nudge, terminal_responses) = {
            let mut controls = self
                .controls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (
                controls.resize.take(),
                controls.nudge.take(),
                std::mem::take(&mut controls.terminal_responses),
            )
        };
        if self.state == ActorState::Released {
            return;
        }
        if let Some(request) = resize {
            self.resize(request.resize);
            self.enqueue_terminal_responses(request.terminal_responses);
        }
        if let Some(nudge) = nudge {
            self.nudge(nudge);
        }
        self.enqueue_terminal_responses(terminal_responses);
    }

    fn read_once(&mut self) -> bool {
        let mut buf = [0u8; 8192];
        match self.file.read(&mut buf) {
            Ok(0) => false,
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => true,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => true,
            Err(err) => {
                debug!(pane = self.pane_id, err = %err, "PTY actor read failed");
                false
            }
            Ok(n) => {
                let response_order = Arc::clone(&self.response_order);
                let _order = response_order
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let result = (self.on_read)(&buf[..n]);
                self.controls
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .terminal_responses
                    .extend(result.terminal_responses);
                drop(_order);
                let terminal_responses = std::mem::take(
                    &mut self
                        .controls
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .terminal_responses,
                );
                self.enqueue_terminal_responses(terminal_responses);
                true
            }
        }
    }

    fn enqueue_terminal_responses(&mut self, terminal_responses: Vec<Bytes>) {
        if self.state == ActorState::Released {
            return;
        }
        for bytes in terminal_responses {
            self.enqueue_write(bytes);
        }
    }

    fn flush_pending_writes_once(&mut self) {
        while let Some((bytes, authorization, user_input)) = self.pending_writes.front() {
            let bytes = bytes.clone();
            let authorization = authorization.clone();
            let user_input = *user_input;
            let authorization_boundary = authorization.as_ref().map(|authorization| {
                authorization
                    .boundary
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
            });
            let user_writes_mutex = Arc::clone(&self.user_writes);
            let user_writes = match user_writes_mutex.lock() {
                Ok(user_writes) => user_writes,
                Err(_) => {
                    self.mark_user_writes_poisoned();
                    self.state = ActorState::Released;
                    self.report_unknown_pending_writes();
                    self.pending_writes.clear();
                    self.current_write_offset = 0;
                    return;
                }
            };
            if user_input && authorization.is_none() && user_writes.remote_owner.is_some() {
                drop(user_writes);
                drop(authorization_boundary);
                self.pending_writes.pop_front();
                self.current_write_offset = 0;
                continue;
            }
            if let Some(authorization) = &authorization {
                if !authorization.is_valid(self.file.as_raw_fd()) {
                    drop(user_writes);
                    drop(authorization_boundary);
                    authorization.report_unknown();
                    self.pending_writes.pop_front();
                    self.current_write_offset = 0;
                    continue;
                }
            }
            // The foreground-process-group check and write(2) are not one atomic
            // kernel operation. The kernel can change the tty's foreground group
            // between the tcgetpgrp(2) performed by is_valid and write(2); no
            // userspace mutex can close that window because it does not serialize
            // with the kernel's tty state. The window is bounded to this single
            // check-to-write syscall sequence, and every retry repeats the full check.
            let chunk = &bytes[self.current_write_offset..];
            let write_result = self.file.write(chunk);
            drop(user_writes);
            drop(authorization_boundary);
            match write_result {
                Ok(0) => {
                    warn!(pane = self.pane_id, "PTY actor write returned zero bytes");
                    return;
                }
                Ok(written) => {
                    self.current_write_offset += written;
                    if self.current_write_offset >= bytes.len() {
                        self.pending_writes.pop_front();
                        self.current_write_offset = 0;
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => return,
                Err(err) => {
                    warn!(pane = self.pane_id, err = %err, "PTY actor write failed");
                    self.report_unknown_pending_writes();
                    self.pending_writes.clear();
                    self.current_write_offset = 0;
                    return;
                }
            }
        }
        let _ = self.file.flush();
    }

    fn report_unknown_pending_writes(&self) {
        for (_, authorization, _) in &self.pending_writes {
            if let Some(authorization) = authorization {
                authorization.report_unknown();
            }
        }
    }

    fn report_unknown_queued_writes(&mut self) {
        while let Ok(PtyIoDataCommand::WriteUserInput { authorization, .. }) =
            self.data_rx.try_recv()
        {
            if let Some(authorization) = authorization {
                authorization.report_unknown();
            }
        }
    }

    fn resize(&self, resize: PtyResize) {
        self.log_resize_result(fd::resize_pty_fd(
            self.file.as_raw_fd(),
            resize.rows,
            resize.cols,
            resize.cell_width_px,
            resize.cell_height_px,
        ));
    }

    fn nudge(&mut self, resize: PtyResize) {
        if self.state == ActorState::Released {
            return;
        }
        let nudge = if resize.rows > 2 {
            (
                resize.rows - 1,
                resize.cols,
                resize.cell_width_px,
                resize.cell_height_px,
            )
        } else {
            (
                resize.rows,
                resize.cols.saturating_sub(1).max(4),
                resize.cell_width_px,
                resize.cell_height_px,
            )
        };
        if nudge
            == (
                resize.rows,
                resize.cols,
                resize.cell_width_px,
                resize.cell_height_px,
            )
        {
            return;
        }
        self.log_resize_result(fd::resize_pty_fd(
            self.file.as_raw_fd(),
            nudge.0,
            nudge.1,
            nudge.2,
            nudge.3,
        ));
        std::thread::sleep(Duration::from_millis(30));
        self.log_resize_result(fd::resize_pty_fd(
            self.file.as_raw_fd(),
            resize.rows,
            resize.cols,
            resize.cell_width_px,
            resize.cell_height_px,
        ));
    }

    fn log_resize_result(&self, result: std::io::Result<()>) {
        if let Err(err) = result {
            debug!(pane = self.pane_id, err = %err, "PTY resize failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::File,
        io::{Read, Write},
        os::fd::{AsRawFd, FromRawFd, IntoRawFd},
        os::unix::net::UnixStream,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    fn test_wake_pair() -> (fd::WakeWriter, OwnedFd) {
        let pipe = fd::create_wake_pipe().expect("wake pipe");
        (pipe.writer, pipe.read_fd)
    }

    fn actor_with_socket_pair(
        initially_quiesced: bool,
    ) -> (PtyIoActorHandle, UnixStream, std_mpsc::Receiver<Bytes>) {
        actor_with_socket_pair_and_poll_observer(initially_quiesced, None)
    }

    fn actor_with_socket_pair_and_poll_observer(
        initially_quiesced: bool,
        poll_observer: Option<std_mpsc::Sender<()>>,
    ) -> (PtyIoActorHandle, UnixStream, std_mpsc::Receiver<Bytes>) {
        let (actor_socket, peer) = UnixStream::pair().expect("socket pair");
        actor_socket
            .set_nonblocking(true)
            .expect("actor socket nonblocking");
        peer.set_read_timeout(Some(Duration::from_secs(1)))
            .expect("peer timeout");
        let owned = unsafe { OwnedFd::from_raw_fd(actor_socket.into_raw_fd()) };
        let (read_tx, read_rx) = std_mpsc::channel();
        let config = PtyIoActorConfig {
            pane_id: 1,
            master_fd: owned,
            initially_quiesced,
            on_read: Box::new(move |bytes| {
                read_tx
                    .send(Bytes::copy_from_slice(bytes))
                    .expect("read callback receiver alive");
                PtyReadResult::empty()
            }),
            on_reader_exit: None,
        };
        let handle = if let Some(poll_observer) = poll_observer {
            PtyIoActor::spawn_with_poll_observer(config, poll_observer)
        } else {
            PtyIoActor::spawn(config)
        }
        .expect("actor spawn");
        (handle, peer, read_rx)
    }

    fn actor_runner_for_unit_test() -> (PtyIoActorRunner, UnixStream) {
        let (actor_socket, peer) = UnixStream::pair().expect("socket pair");
        actor_socket
            .set_nonblocking(true)
            .expect("actor socket nonblocking");
        let owned = unsafe { OwnedFd::from_raw_fd(actor_socket.into_raw_fd()) };
        let (_data_tx, data_rx) = mpsc::channel(ACTOR_COMMAND_BUFFER);
        let (_control_tx, control_rx) = std_mpsc::channel();
        let wake_pipe = fd::create_wake_pipe().expect("wake pipe");
        let runner = PtyIoActorRunner {
            pane_id: 1,
            file: std::fs::File::from(owned),
            data_rx,
            control_rx,
            state: ActorState::Running,
            pending_writes: VecDeque::new(),
            current_write_offset: 0,
            wake_read_fd: wake_pipe.read_fd,
            user_writes: Arc::new(Mutex::new(UserWriteGate {
                accepting: true,
                remote_owner: None,
            })),
            user_writes_poisoned: Arc::new(AtomicBool::new(false)),
            controls: Arc::new(Mutex::new(SharedPtyControls::default())),
            response_order: Arc::new(Mutex::new(())),
            on_read: Box::new(|_| PtyReadResult::empty()),
            on_reader_exit: None,
            poll_observer: None,
        };
        (runner, peer)
    }

    #[cfg(target_os = "linux")]
    fn actor_runner_for_real_pty() -> (PtyIoActorRunner, File, libc::pid_t, u32) {
        let mut master = -1;
        let mut slave_name = [0 as libc::c_char; 128];
        // SAFETY: forkpty initializes a real PTY pair and returns the child pid
        // in the parent. The child immediately execs below.
        let child = unsafe {
            libc::forkpty(
                &mut master,
                slave_name.as_mut_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert!(
            child >= 0,
            "forkpty failed: {}",
            std::io::Error::last_os_error()
        );
        if child == 0 {
            // SAFETY: the forkpty child owns stdin/stdout/stderr and execs now.
            unsafe {
                libc::execl(
                    c"/bin/sleep".as_ptr(),
                    c"sleep".as_ptr(),
                    c"60".as_ptr(),
                    std::ptr::null::<libc::c_char>(),
                );
                libc::_exit(127);
            }
        }
        let actor_fd = fd::duplicate_cloexec_fd(master).expect("duplicate PTY master");
        // SAFETY: forkpty returned a NUL-terminated slave path in `slave_name`.
        let peer_fd = unsafe {
            libc::open(
                slave_name.as_ptr(),
                libc::O_RDWR | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        assert!(
            peer_fd >= 0,
            "open PTY slave failed: {}",
            std::io::Error::last_os_error()
        );

        let mut foreground_group = None;
        for _ in 0..100 {
            if let Some(group) = crate::platform::foreground_process_group_id_for_tty_fd(master) {
                foreground_group = Some(group);
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let foreground_group = foreground_group.expect("child did not become PTY foreground");
        // SAFETY: fcntl changes only the duplicated descriptors' flags.
        let flags = unsafe { libc::fcntl(actor_fd, libc::F_GETFL) };
        assert!(flags >= 0, "fcntl(F_GETFL) failed");
        assert_eq!(
            unsafe { libc::fcntl(actor_fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0,
            "fcntl(F_SETFL) failed"
        );
        // SAFETY: the observer descriptor is open and remains owned by `peer`.
        let peer = unsafe { File::from_raw_fd(peer_fd) };
        let mut termios = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::tcgetattr(peer.as_raw_fd(), &mut termios) },
            0,
            "PTY slave tcgetattr failed"
        );
        unsafe {
            libc::cfmakeraw(&mut termios);
            libc::tcsetattr(peer.as_raw_fd(), libc::TCSANOW, &termios);
        }
        // SAFETY: ownership of the duplicated actor descriptor is transferred once.
        let master_fd = unsafe { OwnedFd::from_raw_fd(actor_fd) };
        let (_data_tx, data_rx) = mpsc::channel(ACTOR_COMMAND_BUFFER);
        let (_control_tx, control_rx) = std_mpsc::channel();
        let wake_pipe = fd::create_wake_pipe().expect("wake pipe");
        let runner = PtyIoActorRunner {
            pane_id: 1,
            file: File::from(master_fd),
            data_rx,
            control_rx,
            state: ActorState::Running,
            pending_writes: VecDeque::new(),
            current_write_offset: 0,
            wake_read_fd: wake_pipe.read_fd,
            user_writes: Arc::new(Mutex::new(UserWriteGate {
                accepting: true,
                remote_owner: None,
            })),
            user_writes_poisoned: Arc::new(AtomicBool::new(false)),
            controls: Arc::new(Mutex::new(SharedPtyControls::default())),
            response_order: Arc::new(Mutex::new(())),
            on_read: Box::new(|_| PtyReadResult::empty()),
            on_reader_exit: None,
            poll_observer: None,
        };
        (runner, peer, child, foreground_group)
    }

    #[cfg(target_os = "linux")]
    fn reap_test_pty(child: libc::pid_t) {
        // SAFETY: child is the process created by actor_runner_for_real_pty.
        unsafe {
            libc::kill(child, libc::SIGKILL);
            libc::waitpid(child, std::ptr::null_mut(), 0);
        }
    }

    fn authorization_for(
        guard: &PtyWriteGuard,
        process_group_id: u32,
        context_epoch: u64,
        unknown: &Arc<AtomicUsize>,
    ) -> PtyWriteAuthorization {
        let unknown = Arc::clone(unknown);
        guard.authorization(
            process_group_id,
            context_epoch,
            Arc::new(move || {
                unknown.fetch_add(1, Ordering::AcqRel);
            }),
        )
    }

    #[test]
    fn actor_ignores_empty_user_input_write() {
        let (mut runner, _peer) = actor_runner_for_unit_test();

        assert!(
            !runner.handle_data_command(PtyIoDataCommand::WriteUserInput {
                bytes: Bytes::new(),
                authorization: None,
            })
        );

        assert!(runner.pending_writes.is_empty());
    }

    #[test]
    fn controlled_write_is_dropped_at_flush_after_authorization_is_revoked() {
        let (mut runner, mut peer) = actor_runner_for_unit_test();
        peer.set_read_timeout(Some(Duration::from_millis(100)))
            .expect("peer timeout");
        let active = Arc::new(AtomicBool::new(false));
        let unknown = Arc::new(AtomicUsize::new(0));
        let unknown_for_callback = Arc::clone(&unknown);
        let authorization = PtyWriteAuthorization::new(
            1234,
            active,
            Arc::new(Mutex::new(())),
            Arc::new(move || {
                unknown_for_callback.fetch_add(1, Ordering::AcqRel);
            }),
        );

        assert!(
            !runner.handle_data_command(PtyIoDataCommand::WriteUserInput {
                bytes: Bytes::from_static(b"must-not-reach-pty"),
                authorization: Some(authorization),
            })
        );
        runner.flush_pending_writes_once();

        let mut received = [0u8; 32];
        assert!(matches!(
            peer.read(&mut received),
            Err(error) if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            )
        ));
        assert_eq!(unknown.load(Ordering::Acquire), 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn context_epoch_change_after_enqueue_writes_zero_bytes() {
        let (mut runner, mut peer, child, process_group_id) = actor_runner_for_real_pty();
        let guard = PtyWriteGuard::new();
        guard.activate(41);
        let unknown = Arc::new(AtomicUsize::new(0));
        let authorization = authorization_for(&guard, process_group_id, 41, &unknown);
        assert!(
            !runner.handle_data_command(PtyIoDataCommand::WriteUserInput {
                bytes: Bytes::from_static(b"stale-context"),
                authorization: Some(authorization),
            })
        );

        // This models a server-observed context revision update even if a
        // future call site forgets to revoke the active lease.
        guard.context_epoch.store(42, Ordering::Release);
        runner.flush_pending_writes_once();

        let mut received = [0u8; 32];
        assert!(matches!(
            peer.read(&mut received),
            Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)
        ));
        assert_eq!(unknown.load(Ordering::Acquire), 1);
        reap_test_pty(child);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn foreground_agent_change_with_same_process_group_writes_zero_bytes() {
        let (mut runner, mut peer, child, process_group_id) = actor_runner_for_real_pty();
        let guard = PtyWriteGuard::new();
        guard.activate(41);
        let unknown = Arc::new(AtomicUsize::new(0));
        let authorization = authorization_for(&guard, process_group_id, 41, &unknown);
        assert!(
            !runner.handle_data_command(PtyIoDataCommand::WriteUserInput {
                bytes: Bytes::from_static(b"stale-agent"),
                authorization: Some(authorization),
            })
        );

        // The observed foreground agent changed while its process group stayed
        // constant. The observation revokes the lease before this flush. Even
        // reactivating the same context cannot resurrect the queued batch.
        guard.revoke();
        guard.activate(41);
        runner.flush_pending_writes_once();

        let mut received = [0u8; 32];
        assert!(matches!(
            peer.read(&mut received),
            Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)
        ));
        assert_eq!(unknown.load(Ordering::Acquire), 1);
        reap_test_pty(child);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn partial_write_then_revoke_does_not_retry_remainder() {
        let (mut runner, mut peer, child, process_group_id) = actor_runner_for_real_pty();
        let guard = PtyWriteGuard::new();
        guard.activate(41);
        let unknown = Arc::new(AtomicUsize::new(0));
        let authorization = authorization_for(&guard, process_group_id, 41, &unknown);
        let bytes = Bytes::from(vec![b'x'; 1024 * 1024]);
        assert!(
            !runner.handle_data_command(PtyIoDataCommand::WriteUserInput {
                bytes: bytes.clone(),
                authorization: Some(authorization),
            })
        );
        runner.flush_pending_writes_once();
        let written = runner.current_write_offset;
        assert!(written > 0, "PTY write should make a partial write first");
        let mut received = vec![0; written];
        peer.read_exact(&mut received)
            .expect("PTY slave receives the partial write");
        assert!(received.iter().all(|byte| *byte == b'x'));
        guard.revoke();
        runner.flush_pending_writes_once();

        assert!(runner.pending_writes.is_empty());
        assert_eq!(runner.current_write_offset, 0);
        assert_eq!(unknown.load(Ordering::Acquire), 1);
        let mut remainder = [0u8; 1];
        assert!(matches!(
            peer.read(&mut remainder),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        reap_test_pty(child);
        drop(peer);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn eagain_then_revoke_does_not_write_controlled_input() {
        let (mut runner, mut peer, child, process_group_id) = actor_runner_for_real_pty();
        let mut filler = vec![b'f'; 1024 * 1024];
        while !filler.is_empty() {
            match runner.file.write(&filler) {
                Ok(written) => {
                    filler.drain(..written);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("failed to fill PTY input: {error}"),
            }
        }
        let guard = PtyWriteGuard::new();
        guard.activate(41);
        let unknown = Arc::new(AtomicUsize::new(0));
        let authorization = authorization_for(&guard, process_group_id, 41, &unknown);
        let marker = Bytes::from_static(b"controlled-after-eagain");
        assert!(
            !runner.handle_data_command(PtyIoDataCommand::WriteUserInput {
                bytes: marker.clone(),
                authorization: Some(authorization),
            })
        );
        runner.flush_pending_writes_once();
        assert_eq!(runner.current_write_offset, 0);
        guard.revoke();
        runner.flush_pending_writes_once();

        assert!(runner.pending_writes.is_empty());
        assert_eq!(unknown.load(Ordering::Acquire), 1);
        let mut observed = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            match peer.read(&mut buffer) {
                Ok(0) => break,
                Ok(length) => observed.extend_from_slice(&buffer[..length]),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break
                }
                Err(error) => panic!("failed to inspect PTY input: {error}"),
            }
        }
        assert!(!observed
            .windows(marker.len())
            .any(|window| window == marker.as_ref()));
        reap_test_pty(child);
    }

    #[test]
    fn poisoned_user_write_gate_refuses_local_and_controlled_writes() {
        let (handle, _peer, _read_rx) = actor_with_socket_pair(false);
        assert!(handle.acquire_remote_owner(7));
        let user_writes = Arc::clone(&handle.user_writes);
        let poison = std::thread::spawn(move || {
            let _guard = user_writes.lock().expect("poisoning lock is healthy");
            panic!("intentional test panic");
        });
        assert!(poison.join().is_err());

        assert!(handle
            .try_write_user_input(Bytes::from_static(b"local"))
            .is_err());
        let unknown = Arc::new(AtomicUsize::new(0));
        let authorization = PtyWriteAuthorization::new(
            0,
            Arc::new(AtomicBool::new(true)),
            Arc::new(Mutex::new(())),
            Arc::new({
                let unknown = Arc::clone(&unknown);
                move || {
                    unknown.fetch_add(1, Ordering::AcqRel);
                }
            }),
        );
        assert!(handle
            .try_write_controlled_user_input(7, Bytes::from_static(b"controlled"), authorization)
            .is_err());
        assert!(handle.user_writes_poisoned.load(Ordering::Acquire));
        handle.shutdown();
    }

    #[test]
    fn remote_owner_excludes_local_and_api_writes_until_release() {
        let (handle, mut peer, _read_rx) = actor_with_socket_pair(false);

        assert!(handle.acquire_remote_owner(7));
        assert!(handle
            .try_write_user_input(Bytes::from_static(b"local-or-api"))
            .is_err());
        handle.release_remote_owner(7);
        handle
            .try_write_user_input(Bytes::from_static(b"after-release"))
            .expect("writes resume after lease release");

        let mut received = [0u8; 13];
        peer.read_exact(&mut received)
            .expect("post-release write reaches the actor fd");
        assert_eq!(&received, b"after-release");
        handle.shutdown();
    }

    #[test]
    fn actor_writes_user_input_to_owned_fd() {
        let (handle, mut peer, _read_rx) = actor_with_socket_pair(false);

        handle
            .try_write_user_input(Bytes::from_static(b"hello"))
            .expect("write command accepted");

        let mut buf = [0u8; 5];
        peer.read_exact(&mut buf).expect("peer receives write");
        assert_eq!(&buf, b"hello");
        handle.shutdown();
    }

    #[test]
    fn actor_wakes_idle_poll_for_user_input() {
        let (poll_tx, poll_rx) = std_mpsc::channel();
        let (handle, mut peer, _read_rx) =
            actor_with_socket_pair_and_poll_observer(false, Some(poll_tx));
        peer.set_read_timeout(Some(Duration::from_millis(500)))
            .expect("peer timeout");
        poll_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("actor entered idle poll");

        let start = Instant::now();
        handle
            .try_write_user_input(Bytes::from_static(b"x"))
            .expect("write command accepted");

        let mut buf = [0u8; 1];
        peer.read_exact(&mut buf)
            .expect("peer receives write without waiting for actor poll timeout");
        assert_eq!(&buf, b"x");
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "actor write should be driven by wake fd, not the idle poll timeout"
        );
        handle.shutdown();
    }

    #[test]
    fn actor_reads_output_while_input_is_backpressured() {
        let (mut actor_socket, mut peer) = UnixStream::pair().expect("socket pair");
        actor_socket
            .set_nonblocking(true)
            .expect("actor socket nonblocking");
        peer.set_read_timeout(Some(Duration::from_secs(1)))
            .expect("peer timeout");

        let fill = [0xAA; 8192];
        let mut prefilled = 0;
        loop {
            match actor_socket.write(&fill) {
                Ok(written) => prefilled += written,
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(err) => panic!("failed to fill actor write buffer: {err}"),
            }
        }
        assert!(prefilled > 0, "actor write buffer should accept some bytes");

        let owned = unsafe { OwnedFd::from_raw_fd(actor_socket.into_raw_fd()) };
        let (read_tx, read_rx) = std_mpsc::channel();
        let handle = PtyIoActor::spawn(PtyIoActorConfig {
            pane_id: 1,
            master_fd: owned,
            initially_quiesced: false,
            on_read: Box::new(move |bytes| {
                read_tx
                    .send(Bytes::copy_from_slice(bytes))
                    .expect("read callback receiver alive");
                PtyReadResult::empty()
            }),
            on_reader_exit: None,
        })
        .expect("actor spawn");

        let marker = Bytes::from_static(b"queued-input");
        handle
            .try_write_user_input(marker.clone())
            .expect("write command accepted");

        const OUTPUT_LEN: usize = 128 * 1024;
        let mut peer_writer = peer.try_clone().expect("clone peer writer");
        let output_writer = std::thread::spawn(move || {
            peer_writer
                .write_all(&vec![0xBB; OUTPUT_LEN])
                .expect("peer writes sustained output");
        });
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut output_len = 0;
        while output_len < OUTPUT_LEN {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "actor did not keep reading blocked peer output"
            );
            let output = read_rx
                .recv_timeout(remaining)
                .expect("actor keeps reading while input remains blocked");
            assert!(output.iter().all(|byte| *byte == 0xBB));
            output_len += output.len();
        }
        assert_eq!(output_len, OUTPUT_LEN);
        output_writer.join().expect("output writer joins");

        let mut received_input = vec![0; prefilled + marker.len()];
        peer.read_exact(&mut received_input)
            .expect("peer receives prefill and queued input");
        assert!(received_input[..prefilled].iter().all(|byte| *byte == 0xAA));
        assert_eq!(&received_input[prefilled..], marker.as_ref());
        handle.shutdown();
    }

    #[test]
    fn actor_wakes_idle_poll_for_handoff_control() {
        let (poll_tx, poll_rx) = std_mpsc::channel();
        let (handle, _peer, _read_rx) =
            actor_with_socket_pair_and_poll_observer(false, Some(poll_tx));
        poll_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("actor entered idle poll");

        let start = Instant::now();
        let handoff_handle = handle.clone();
        let handoff =
            std::thread::spawn(move || handoff_handle.begin_handoff(Duration::from_secs(1)));

        handoff
            .join()
            .expect("handoff thread joins")
            .expect("handoff control should wake idle actor");
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "handoff control should be driven by wake fd, not the idle poll timeout"
        );
        handle.shutdown();
    }

    #[test]
    fn poll_ignores_pty_hup_without_pty_interest() {
        let (actor_socket, peer) = UnixStream::pair().expect("socket pair");
        actor_socket
            .set_nonblocking(true)
            .expect("actor socket nonblocking");
        drop(peer);
        let wake_pipe = fd::create_wake_pipe().expect("wake pipe");

        let readiness = fd::poll_pty_and_wake(
            actor_socket.as_raw_fd(),
            wake_pipe.read_fd.as_raw_fd(),
            false,
            false,
            10,
        )
        .expect("poll succeeds");

        assert!(!readiness.pty_read_ready);
        assert!(!readiness.pty_write_ready);
        assert!(!readiness.wake_ready);
    }

    #[test]
    fn actor_delivers_fd_reads_to_callback() {
        let (handle, mut peer, read_rx) = actor_with_socket_pair(false);

        peer.write_all(b"from-peer").expect("peer write");

        let read = read_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("actor read callback");
        assert_eq!(read, Bytes::from_static(b"from-peer"));
        handle.shutdown();
    }

    #[test]
    fn begin_handoff_stops_reads_and_rejects_user_writes_until_rollback() {
        let (handle, mut peer, read_rx) = actor_with_socket_pair(false);

        handle
            .begin_handoff(Duration::from_secs(1))
            .expect("handoff quiesced");
        assert!(handle
            .try_write_user_input(Bytes::from_static(b"blocked"))
            .is_err());

        peer.write_all(b"held").expect("peer write during quiesce");
        assert!(
            read_rx.recv_timeout(Duration::from_millis(150)).is_err(),
            "actor must not read while quiesced"
        );

        handle.rollback_handoff().expect("rollback resumes actor");
        let read = read_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("actor reads held bytes after rollback");
        assert_eq!(read, Bytes::from_static(b"held"));

        handle
            .try_write_user_input(Bytes::from_static(b"after"))
            .expect("write accepted after rollback");
        let mut buf = [0u8; 5];
        peer.read_exact(&mut buf).expect("peer receives after");
        assert_eq!(&buf, b"after");
        handle.shutdown();
    }

    #[test]
    fn duplicate_for_handoff_requires_quiesced_actor() {
        let (handle, mut peer, read_rx) = actor_with_socket_pair(false);

        assert!(handle.duplicate_for_handoff().is_err());
        handle
            .begin_handoff(Duration::from_secs(1))
            .expect("handoff quiesced");
        let duplicate = handle
            .duplicate_for_handoff()
            .expect("handoff duplicate created");
        assert!(duplicate >= 0);
        unsafe {
            libc::close(duplicate);
        }
        handle.rollback_handoff().expect("rollback resumes actor");

        peer.write_all(b"still-live").expect("peer write");
        let read = read_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("actor still reads after duplicate closes");
        assert_eq!(read, Bytes::from_static(b"still-live"));
        handle.shutdown();
    }

    #[test]
    fn resize_and_nudge_keep_latest_request_when_command_queue_is_full() {
        let (data_tx, _data_rx) = mpsc::channel(1);
        let (control_tx, _control_rx) = std_mpsc::channel();
        data_tx
            .try_send(PtyIoDataCommand::WriteUserInput {
                bytes: Bytes::from_static(b"fill"),
                authorization: None,
            })
            .expect("fill command queue");
        let controls = Arc::new(Mutex::new(SharedPtyControls::default()));
        let (wake, _wake_read_fd) = test_wake_pair();
        let handle = PtyIoActorHandle {
            data_tx,
            control_tx,
            wake,
            user_writes: Arc::new(Mutex::new(UserWriteGate {
                accepting: true,
                remote_owner: None,
            })),
            user_writes_poisoned: Arc::new(AtomicBool::new(false)),
            write_guard: PtyWriteGuard::new(),
            controls: Arc::clone(&controls),
            response_order: Arc::new(Mutex::new(())),
        };

        handle.resize(20, 80, 8, 16, vec![Bytes::from_static(b"old")]);
        handle.resize(40, 120, 9, 18, vec![Bytes::from_static(b"new")]);
        handle.nudge_child_redraw_after_handoff(41, 121, 10, 20);
        handle.write_terminal_response(|| Some(Bytes::from_static(b"response")));

        let controls = controls.lock().expect("controls lock");
        assert_eq!(
            controls.resize,
            Some(PtyResizeRequest {
                resize: PtyResize {
                    rows: 40,
                    cols: 120,
                    cell_width_px: 9,
                    cell_height_px: 18,
                },
                terminal_responses: vec![Bytes::from_static(b"new")],
            })
        );
        assert_eq!(
            controls.nudge,
            Some(PtyResize {
                rows: 41,
                cols: 121,
                cell_width_px: 10,
                cell_height_px: 20,
            })
        );
        assert_eq!(
            controls.terminal_responses,
            vec![Bytes::from_static(b"response")]
        );
    }

    #[test]
    fn appearance_transition_report_precedes_query_of_new_scheme() {
        let (actor_socket, mut peer) = UnixStream::pair().expect("socket pair");
        actor_socket
            .set_nonblocking(true)
            .expect("actor socket nonblocking");
        let owned = unsafe { OwnedFd::from_raw_fd(actor_socket.into_raw_fd()) };
        let (data_tx, data_rx) = mpsc::channel(ACTOR_COMMAND_BUFFER);
        let (control_tx, control_rx) = std_mpsc::channel();
        let wake_pipe = fd::create_wake_pipe().expect("wake pipe");
        let controls = Arc::new(Mutex::new(SharedPtyControls::default()));
        let response_order = Arc::new(Mutex::new(()));
        let light = Arc::new(AtomicBool::new(false));
        let query_light = Arc::clone(&light);
        let runner = PtyIoActorRunner {
            pane_id: 1,
            file: std::fs::File::from(owned),
            data_rx,
            control_rx,
            state: ActorState::Running,
            pending_writes: VecDeque::new(),
            current_write_offset: 0,
            wake_read_fd: wake_pipe.read_fd,
            user_writes: Arc::new(Mutex::new(UserWriteGate {
                accepting: true,
                remote_owner: None,
            })),
            user_writes_poisoned: Arc::new(AtomicBool::new(false)),
            controls: Arc::clone(&controls),
            response_order: Arc::clone(&response_order),
            on_read: Box::new(move |_| PtyReadResult {
                terminal_responses: vec![if query_light.load(Ordering::Acquire) {
                    Bytes::from_static(b"query-light")
                } else {
                    Bytes::from_static(b"query-dark")
                }],
            }),
            on_reader_exit: None,
            poll_observer: None,
        };
        let handle = PtyIoActorHandle {
            data_tx,
            control_tx,
            wake: wake_pipe.writer,
            user_writes: Arc::new(Mutex::new(UserWriteGate {
                accepting: true,
                remote_owner: None,
            })),
            user_writes_poisoned: Arc::new(AtomicBool::new(false)),
            write_guard: PtyWriteGuard::new(),
            controls,
            response_order,
        };
        let (changed_tx, changed_rx) = std_mpsc::channel();
        let (continue_tx, continue_rx) = std_mpsc::channel();

        let appearance = std::thread::spawn(move || {
            handle.write_terminal_response(|| {
                light.store(true, Ordering::Release);
                changed_tx.send(()).expect("notify appearance change");
                continue_rx.recv().expect("continue appearance report");
                Some(Bytes::from_static(b"live-light"))
            });
        });
        changed_rx.recv().expect("appearance changed");
        peer.write_all(b"query").expect("write query");
        let reader = std::thread::spawn(move || {
            let mut runner = runner;
            assert!(runner.read_once());
            runner
        });
        continue_tx.send(()).expect("release appearance report");
        appearance.join().expect("appearance thread joins");
        let runner = reader.join().expect("reader thread joins");

        assert_eq!(runner.pending_writes.len(), 2);
        assert_eq!(
            runner.pending_writes[0].0,
            Bytes::from_static(b"live-light")
        );
        assert_eq!(
            runner.pending_writes[1].0,
            Bytes::from_static(b"query-light")
        );
        assert!(runner
            .pending_writes
            .iter()
            .all(|(_, authorization, _)| { authorization.is_none() }));
    }

    #[test]
    fn resize_writes_terminal_responses_after_applying_resize() {
        let (handle, mut peer, _read_rx) = actor_with_socket_pair(false);
        let response = Bytes::from_static(b"\x1B[48;40;100;720;900t");

        handle.resize(40, 100, 9, 18, vec![response.clone()]);

        let mut buf = vec![0; response.len()];
        peer.read_exact(&mut buf)
            .expect("peer receives resize response");
        assert_eq!(Bytes::from(buf), response);
        handle.shutdown();
    }

    #[tokio::test]
    async fn async_user_input_waits_for_queue_capacity() {
        let (data_tx, mut data_rx) = mpsc::channel(1);
        let (control_tx, _control_rx) = std_mpsc::channel();
        data_tx
            .try_send(PtyIoDataCommand::WriteUserInput {
                bytes: Bytes::from_static(b"fill"),
                authorization: None,
            })
            .expect("fill data queue");
        let (wake, _wake_read_fd) = test_wake_pair();
        let handle = PtyIoActorHandle {
            data_tx,
            control_tx,
            wake,
            user_writes: Arc::new(Mutex::new(UserWriteGate {
                accepting: true,
                remote_owner: None,
            })),
            user_writes_poisoned: Arc::new(AtomicBool::new(false)),
            write_guard: PtyWriteGuard::new(),
            controls: Arc::new(Mutex::new(SharedPtyControls::default())),
            response_order: Arc::new(Mutex::new(())),
        };

        let write = tokio::spawn(async move {
            handle
                .write_user_input(Bytes::from_static(b"wait-for-capacity"))
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !write.is_finished(),
            "async input should wait for queue capacity"
        );

        assert!(matches!(
            data_rx.recv().await,
            Some(PtyIoDataCommand::WriteUserInput { .. })
        ));
        write
            .await
            .expect("write task joins")
            .expect("write succeeds after capacity opens");
        match data_rx.recv().await {
            Some(PtyIoDataCommand::WriteUserInput { bytes, .. }) => {
                assert_eq!(bytes, Bytes::from_static(b"wait-for-capacity"));
            }
            _ => panic!("expected queued user input"),
        }
    }

    #[tokio::test]
    async fn async_user_input_waiting_for_capacity_is_rejected_after_handoff_begins() {
        let (data_tx, mut data_rx) = mpsc::channel(1);
        let (control_tx, control_rx) = std_mpsc::channel();
        data_tx
            .try_send(PtyIoDataCommand::WriteUserInput {
                bytes: Bytes::from_static(b"fill"),
                authorization: None,
            })
            .expect("fill data queue");
        let (wake, _wake_read_fd) = test_wake_pair();
        let handle = PtyIoActorHandle {
            data_tx,
            control_tx,
            wake,
            user_writes: Arc::new(Mutex::new(UserWriteGate {
                accepting: true,
                remote_owner: None,
            })),
            user_writes_poisoned: Arc::new(AtomicBool::new(false)),
            write_guard: PtyWriteGuard::new(),
            controls: Arc::new(Mutex::new(SharedPtyControls::default())),
            response_order: Arc::new(Mutex::new(())),
        };
        let write_handle = handle.clone();
        let write = tokio::spawn(async move {
            write_handle
                .write_user_input(Bytes::from_static(b"after-handoff-start"))
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let handoff = std::thread::spawn(move || handle.begin_handoff(Duration::from_secs(1)));
        match control_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("handoff control command")
        {
            PtyIoControlCommand::BeginHandoff(reply) => {
                reply.send(Ok(())).expect("handoff waiter alive");
            }
            _ => panic!("expected begin handoff command"),
        }
        handoff
            .join()
            .expect("handoff thread joins")
            .expect("handoff succeeds");
        assert!(matches!(
            data_rx.recv().await,
            Some(PtyIoDataCommand::WriteUserInput { .. })
        ));

        let err = write.await.expect("write task joins").expect_err(
            "write waiting for capacity must be rejected after handoff closes the input gate",
        );
        assert_eq!(err.0, Bytes::from_static(b"after-handoff-start"));
        match tokio::time::timeout(Duration::from_millis(50), data_rx.recv()).await {
            Err(_) | Ok(None) => {}
            Ok(Some(_)) => panic!("rejected write must not be queued"),
        }
    }

    #[test]
    fn handoff_control_is_not_blocked_by_full_data_queue() {
        let (data_tx, _data_rx) = mpsc::channel(1);
        let (control_tx, control_rx) = std_mpsc::channel();
        data_tx
            .try_send(PtyIoDataCommand::WriteUserInput {
                bytes: Bytes::from_static(b"fill"),
                authorization: None,
            })
            .expect("fill data queue");
        let (wake, _wake_read_fd) = test_wake_pair();
        let handle = PtyIoActorHandle {
            data_tx,
            control_tx,
            wake,
            user_writes: Arc::new(Mutex::new(UserWriteGate {
                accepting: true,
                remote_owner: None,
            })),
            user_writes_poisoned: Arc::new(AtomicBool::new(false)),
            write_guard: PtyWriteGuard::new(),
            controls: Arc::new(Mutex::new(SharedPtyControls::default())),
            response_order: Arc::new(Mutex::new(())),
        };

        let handoff = std::thread::spawn(move || handle.begin_handoff(Duration::from_secs(1)));
        match control_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("handoff control command")
        {
            PtyIoControlCommand::BeginHandoff(reply) => {
                reply.send(Ok(())).expect("handoff waiter alive");
            }
            _ => panic!("expected begin handoff command"),
        }

        handoff
            .join()
            .expect("handoff thread joins")
            .expect("handoff succeeds despite full data queue");
    }

    #[test]
    fn begin_handoff_drains_user_writes_already_in_command_queue() {
        let (actor_socket, mut peer) = UnixStream::pair().expect("socket pair");
        actor_socket
            .set_nonblocking(true)
            .expect("actor socket nonblocking");
        peer.set_read_timeout(Some(Duration::from_secs(1)))
            .expect("peer timeout");
        let (data_tx, data_rx) = mpsc::channel(ACTOR_COMMAND_BUFFER);
        let (_control_tx, control_rx) = std_mpsc::channel();
        data_tx
            .try_send(PtyIoDataCommand::WriteUserInput {
                bytes: Bytes::from_static(b"queued-before-ack"),
                authorization: None,
            })
            .expect("queued write");
        let mut runner = PtyIoActorRunner {
            pane_id: 1,
            file: std::fs::File::from(unsafe { OwnedFd::from_raw_fd(actor_socket.into_raw_fd()) }),
            data_rx,
            control_rx,
            state: ActorState::Running,
            pending_writes: VecDeque::new(),
            current_write_offset: 0,
            wake_read_fd: fd::create_wake_pipe().expect("wake pipe").read_fd,
            user_writes: Arc::new(Mutex::new(UserWriteGate {
                accepting: true,
                remote_owner: None,
            })),
            user_writes_poisoned: Arc::new(AtomicBool::new(false)),
            controls: Arc::new(Mutex::new(SharedPtyControls::default())),
            response_order: Arc::new(Mutex::new(())),
            on_read: Box::new(|_| PtyReadResult::empty()),
            on_reader_exit: None,
            poll_observer: None,
        };

        runner.begin_handoff().expect("handoff drains queued write");

        let mut buf = [0u8; 17];
        peer.read_exact(&mut buf)
            .expect("queued write reaches peer before quiesce ack");
        assert_eq!(&buf, b"queued-before-ack");
        assert_eq!(runner.state, ActorState::Quiesced);
    }

    #[test]
    fn release_after_commit_prevents_further_io() {
        let (handle, mut peer, read_rx) = actor_with_socket_pair(false);

        handle.release_after_commit().expect("actor released");
        assert!(handle
            .try_write_user_input(Bytes::from_static(b"blocked"))
            .is_err());

        let _ = peer.write_all(b"ignored");
        assert!(read_rx.recv_timeout(Duration::from_millis(150)).is_err());
    }
}
