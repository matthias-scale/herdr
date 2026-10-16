//! Transport-owned, read-only WAN observers. No control lease or input is sent.
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::control::{read_initial_welcome, CancellableReader, RemoteHostIdentity, SshRunner};
use super::snapshot_cache::SnapshotCache;
use crate::api::schema::AgentRef;
use crate::config::FleetHostConfig;
use crate::protocol::{self, ClientMessage, ServerMessage, MAX_FRAME_SIZE, PROTOCOL_VERSION};

struct Observer {
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    retry_after: Instant,
}
impl Drop for Observer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

#[derive(Default)]
pub(super) struct Prewarm {
    inventory: Vec<AgentRef>,
    size: (u16, u16),
    active_workspaces: Vec<AgentRef>,
    last_update: Option<Instant>,
    last_workspace: Option<AgentRef>,
    recent: VecDeque<AgentRef>,
    observers: HashMap<AgentRef, Observer>,
}
impl Prewarm {
    pub(super) fn clear(&mut self) {
        self.observers.clear();
        self.inventory.clear();
        self.active_workspaces.clear();
        self.last_update = None;
        self.recent.clear();
    }
    pub(super) fn set_size(&mut self, size: (u16, u16)) {
        let size = (size.0.max(1), size.1.max(1));
        if self.size != size {
            self.size = size;
            self.observers.clear();
            self.last_update = None;
        }
    }
    pub(super) fn due(&self, workspace: Option<&AgentRef>) -> bool {
        self.last_workspace.as_ref() != workspace
            || self
                .last_update
                .is_none_or(|last| last.elapsed() >= Duration::from_secs(1))
    }
    pub(super) fn focus(&mut self, agent: &AgentRef) {
        self.recent.retain(|previous| previous != agent);
        self.recent.push_front(agent.clone());
        self.recent.truncate(10);
        self.last_update = None;
        // Stop writing the observer's diff sequence before control starts its
        // own full baseline. The cache lock orders any in-flight final insert.
        self.observers.remove(agent);
    }
    pub(super) fn observe(&mut self, snapshot: &crate::fleet::Snapshot) {
        self.last_update = None;
        self.active_workspaces = snapshot
            .hosts
            .iter()
            .filter(|host| host.reachable)
            .filter_map(|host| {
                let workspaces = host
                    .sessions
                    .as_ref()?
                    .snapshot
                    .as_ref()?
                    .get("workspaces")?
                    .as_array()?;
                let active = workspaces.iter().find(|workspace| {
                    workspace
                        .get("focused")
                        .and_then(serde_json::Value::as_bool)
                        == Some(true)
                })?;
                let id = active.get("workspace_id")?.as_str()?;
                AgentRef::new(&host.name, format!("{id}:prewarm")).ok()
            })
            .collect();
        let mut inventory: std::collections::HashSet<_> = snapshot
            .hosts
            .iter()
            .filter(|host| host.reachable)
            .flat_map(|host| host.entries.iter())
            .filter(|row| row.source == crate::fleet::EvidenceSource::Herdr)
            .map(|row| row.agent_ref.clone())
            .collect();
        // Session inventory also includes plain shell panes without an agent row.
        for host in snapshot.hosts.iter().filter(|host| host.reachable) {
            let panes = host
                .sessions
                .as_ref()
                .and_then(|sessions| sessions.snapshot.as_ref())
                .and_then(|snapshot| snapshot.get("panes"))
                .and_then(serde_json::Value::as_array);
            for pane in panes.into_iter().flatten() {
                if let Some(agent) = pane
                    .get("pane_id")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|pane| AgentRef::new(&host.name, pane).ok())
                {
                    inventory.insert(agent);
                }
            }
        }
        self.inventory = inventory.into_iter().collect();
    }
    pub(super) fn update(
        &mut self,
        workspace: Option<&AgentRef>,
        hosts: &HashMap<String, FleetHostConfig>,
        identities: &HashMap<String, RemoteHostIdentity>,
        runner: Arc<dyn SshRunner>,
        cache: Arc<Mutex<SnapshotCache>>,
        active: &std::collections::HashSet<AgentRef>,
    ) {
        self.last_update = Some(Instant::now());
        self.last_workspace = workspace.cloned();
        let workspaces = workspace
            .map(std::slice::from_ref)
            .unwrap_or(&self.active_workspaces);
        let desired = crate::fleet::prewarm_selection(
            hosts.values(),
            &self.inventory,
            workspaces,
            &self.recent,
        );
        self.observers
            .retain(|agent, _| desired.contains(agent) && !active.contains(agent));
        for agent in desired {
            if active.contains(&agent) {
                continue;
            }
            if let Some(observer) = self.observers.get(&agent) {
                if !observer.done.load(Ordering::Acquire) || Instant::now() < observer.retry_after {
                    continue;
                }
            }
            let (Some(host), Some(_identity)) =
                (hosts.get(&agent.host), identities.get(&agent.host))
            else {
                continue;
            };
            let host = host.clone();
            let stop = Arc::new(AtomicBool::new(false));
            let done = Arc::new(AtomicBool::new(false));
            let spawn_done = Arc::clone(&done);
            self.observers.insert(
                agent.clone(),
                Observer {
                    stop: Arc::clone(&stop),
                    done: Arc::clone(&done),
                    retry_after: Instant::now() + Duration::from_secs(10),
                },
            );
            let cache = Arc::clone(&cache);
            let runner = Arc::clone(&runner);
            let size = self.size;
            if let Err(error) = std::thread::Builder::new()
                .name("herdr-wan-observe".into())
                .spawn(move || {
                    let result = observe(runner, &host, &agent, &stop, &cache, size);
                    if let Err(error) = result {
                        tracing::debug!(%agent, %error, "WAN observer ended");
                    }
                    done.store(true, Ordering::Release);
                })
            {
                spawn_done.store(true, Ordering::Release);
                tracing::debug!(%error, "cannot start WAN observer");
            }
        }
    }
}

fn observe(
    runner: Arc<dyn SshRunner>,
    host: &FleetHostConfig,
    agent: &AgentRef,
    stop: &AtomicBool,
    cache: &Mutex<SnapshotCache>,
    size: (u16, u16),
) -> Result<(), protocol::FramingError> {
    let mut stream = runner.connect(host)?;
    if let Err(error) = negotiate_observer(&mut *stream, agent, size) {
        // Before split, Child has no drop guard. Reap a failed handshake too.
        let _ = stream.close_diagnostic();
        return Err(error);
    }
    let (mut reader, _writer) = stream.split();
    while !stop.load(Ordering::Acquire) {
        let message = protocol::read_message::<_, ServerMessage>(
            &mut CancellableReader {
                reader: &mut *reader,
                detached: stop,
            },
            MAX_FRAME_SIZE,
        )?;
        match message {
            ServerMessage::Terminal(frame) => {
                let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
                if stop.load(Ordering::Acquire) {
                    break;
                }
                cache.insert(agent, frame);
            }
            ServerMessage::ControlError { .. } | ServerMessage::ServerShutdown { .. } => break,
            _ => {}
        }
    }
    Ok(())
}

fn negotiate_observer(
    mut stream: &mut dyn super::control::ControlStream,
    agent: &AgentRef,
    size: (u16, u16),
) -> Result<(), protocol::FramingError> {
    protocol::write_message(
        &mut stream,
        &ClientMessage::Hello {
            version: PROTOCOL_VERSION,
            build_version: crate::build_info::version().to_owned(),
            cols: size.1,
            rows: size.0,
            cell_width_px: 0,
            cell_height_px: 0,
            requested_encoding: protocol::RenderEncoding::TerminalAnsi,
            keybindings: protocol::ClientKeybindings::Server,
            launch_mode: protocol::ClientLaunchMode::TerminalAttach,
        },
    )?;
    match read_initial_welcome(stream, Instant::now() + Duration::from_secs(5))? {
        ServerMessage::Welcome {
            version,
            build_version,
            error,
            encoding: protocol::RenderEncoding::TerminalAnsi,
        } if version == PROTOCOL_VERSION
            && build_version == crate::build_info::version()
            && error.is_none() => {}
        _ => return Err(std::io::Error::other("incompatible observer handshake").into()),
    }
    protocol::write_message(
        &mut stream,
        &ClientMessage::ObserveTerminal {
            target: agent.agent.clone(),
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::control::{ControlReadHalf, ControlStream, TimedRead};
    use super::*;
    use std::io::{self, Cursor, Read, Write};

    struct FakeStream {
        input: Cursor<Vec<u8>>,
        output: Arc<Mutex<Vec<u8>>>,
    }
    impl Read for FakeStream {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.input.read(bytes)
        }
    }
    impl TimedRead for FakeStream {
        fn read_with_timeout(&mut self, bytes: &mut [u8], _: Duration) -> io::Result<usize> {
            self.read(bytes)
        }
    }
    impl Write for FakeStream {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.output.lock().expect("output").extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl ControlReadHalf for FakeStream {}
    impl ControlStream for FakeStream {
        fn split(self: Box<Self>) -> (Box<dyn ControlReadHalf>, Box<dyn Write + Send>) {
            let output = Arc::clone(&self.output);
            (
                self,
                Box::new(FakeStream {
                    input: Cursor::new(Vec::new()),
                    output,
                }),
            )
        }
    }
    struct Runner(Mutex<Option<FakeStream>>);
    impl SshRunner for Runner {
        fn connect(&self, _: &FleetHostConfig) -> io::Result<Box<dyn ControlStream>> {
            Ok(Box::new(
                self.0
                    .lock()
                    .expect("runner")
                    .take()
                    .expect("single stream"),
            ))
        }
    }
    #[test]
    fn active_workspace_inventory_includes_plain_shell_panes() {
        let mut prewarm = Prewarm::default();
        prewarm.observe(&crate::fleet::Snapshot {
            hosts: vec![crate::fleet::HostSnapshot {
                name: "wan".into(),
                target: "remote".into(),
                local: false,
                session: None,
                socket: None,
                state: crate::fleet::HostState::Reachable,
                version: None,
                protocol: None,
                error: None,
                remote_identity: Some("remote".into()),
                sessions: Some(crate::fleet::HostSessionInventory {
                    snapshot: Some(serde_json::json!({
                        "workspaces": [{"workspace_id": "w2", "focused": true}],
                        "panes": [{"pane_id": "w2:p1"}, {"pane_id": "w3:p2"}]
                    })),
                    sections: Vec::new(),
                    runs: Vec::new(),
                    loops: Vec::new(),
                }),
                reachable: true,
                last_seen_unix_ms: None,
                entries: Vec::new(),
            }],
            ..Default::default()
        });
        let host = FleetHostConfig {
            name: "wan".into(),
            wan: true,
            ..Default::default()
        };
        let selected = crate::fleet::prewarm_selection(
            std::iter::once(&host),
            &prewarm.inventory,
            &prewarm.active_workspaces,
            &prewarm.recent,
        );
        assert_eq!(
            selected,
            [AgentRef::new("wan", "w2:p1").expect("ref")]
                .into_iter()
                .collect()
        );
        assert!(prewarm.due(None));
    }

    #[test]
    fn prewarm_observes_and_caches_without_control_resize_or_input() {
        let mut input = Vec::new();
        protocol::write_message(
            &mut input,
            &ServerMessage::Welcome {
                version: PROTOCOL_VERSION,
                build_version: crate::build_info::version().to_owned(),
                error: None,
                encoding: protocol::RenderEncoding::TerminalAnsi,
            },
        )
        .expect("welcome");
        protocol::write_message(
            &mut input,
            &ServerMessage::Terminal(protocol::TerminalFrame {
                seq: 1,
                width: 80,
                height: 24,
                full: true,
                bytes: b"screen".to_vec(),
            }),
        )
        .expect("frame");
        let output = Arc::new(Mutex::new(Vec::new()));
        let runner = Arc::new(Runner(Mutex::new(Some(FakeStream {
            input: Cursor::new(input),
            output: Arc::clone(&output),
        }))));
        let cache = Mutex::new(SnapshotCache::default());
        let agent = AgentRef::new("alias", "w1:p1").expect("ref");
        let _eof = observe(
            runner,
            &FleetHostConfig::default(),
            &agent,
            &AtomicBool::new(false),
            &cache,
            (24, 80),
        );
        assert!(cache
            .lock()
            .expect("cache")
            .replay(&agent, |frame| assert_eq!(frame.bytes, b"screen")));
        let bytes = output.lock().expect("output");
        let mut cursor = Cursor::new(bytes.as_slice());
        assert!(matches!(
            protocol::read_message::<_, ClientMessage>(&mut cursor, MAX_FRAME_SIZE).expect("hello"),
            ClientMessage::Hello { .. }
        ));
        assert!(
            matches!(protocol::read_message::<_, ClientMessage>(&mut cursor, MAX_FRAME_SIZE).expect("observe"), ClientMessage::ObserveTerminal { target } if target == "w1:p1")
        );
        assert_eq!(cursor.position() as usize, bytes.len());
    }
}
