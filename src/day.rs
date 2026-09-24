use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DayItemKind {
    #[default]
    Task,
    Noticed,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DayItemSource {
    #[default]
    Manual,
    Ticket,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DayBinding {
    /// Which server owns the bound pane. Pane ids restart at 1 in every server
    /// process, so two named sessions on one host both own `w1:p1`. `None` is a
    /// binding written before this field existed; it stays host-scoped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    pub pane_id: String,
    pub bound_at: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DayLinks {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tickets: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prs: Vec<String>,
}

impl DayLinks {
    pub fn is_empty(&self) -> bool {
        self.tickets.is_empty() && self.prs.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DayItem {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub kind: DayItemKind,
    #[serde(default)]
    pub source: DayItemSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default)]
    pub links: DayLinks,
    #[serde(default)]
    pub bindings: BTreeMap<String, DayBinding>,
    pub added_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_at: Option<u64>,
    #[serde(default)]
    pub dismissed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_after_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DayItemFrontMatter {
    id: String,
    #[serde(default)]
    kind: DayItemKind,
    #[serde(default)]
    source: DayItemSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(default)]
    links: DayLinks,
    #[serde(default)]
    bindings: BTreeMap<String, DayBinding>,
    added_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    done_at: Option<u64>,
    #[serde(default)]
    dismissed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stale_after_seconds: Option<u64>,
}

impl DayItemFrontMatter {
    fn from_item(item: &DayItem) -> Self {
        Self {
            id: item.id.clone(),
            kind: item.kind,
            source: item.source,
            note: item.note.clone(),
            links: item.links.clone(),
            bindings: item.bindings.clone(),
            added_at: item.added_at,
            done_at: item.done_at,
            dismissed: item.dismissed,
            stale_after_seconds: item.stale_after_seconds,
        }
    }

    fn into_item(self, title: String) -> DayItem {
        DayItem {
            id: self.id,
            title,
            kind: self.kind,
            source: self.source,
            note: self.note,
            links: self.links,
            bindings: self.bindings,
            added_at: self.added_at,
            done_at: self.done_at,
            dismissed: self.dismissed,
            stale_after_seconds: self.stale_after_seconds,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayItemLoadError {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DayBoard {
    pub items: BTreeMap<String, DayItem>,
    pub load_errors: Vec<DayItemLoadError>,
}

impl DayBoard {
    pub fn links(&self) -> DayLinks {
        let mut links = DayLinks {
            tickets: self
                .items
                .values()
                .filter(|item| item.done_at.is_none() && !item.dismissed)
                .flat_map(|item| item.links.tickets.iter().cloned())
                .collect(),
            prs: self
                .items
                .values()
                .filter(|item| item.done_at.is_none() && !item.dismissed)
                .flat_map(|item| item.links.prs.iter().cloned())
                .collect(),
        };
        links.tickets.sort();
        links.tickets.dedup();
        links.prs.sort();
        links.prs.dedup();
        links
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum DayItemUpdateError {
    NotFound,
    Store(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DayColumn {
    Todo,
    Working,
    Blocked,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DayPaneEvidence {
    pub state: crate::detect::AgentState,
    pub inactive_for: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DerivedDayItem {
    #[serde(flatten)]
    pub item: DayItem,
    pub column: DayColumn,
    pub stale: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

/// Names one server among those sharing the host-global store.
///
/// A binding records a pane id, and pane ids restart at 1 in every server
/// process, so the id is only meaningful next to the server that issued it.
/// Two facts decide whether a pane id still means what it meant: the api socket
/// path, because that is what routes a client to a server, and the data
/// directory, because `persist::io::session_path` restores the workspaces and
/// panes from there. `HERDR_SOCKET_PATH` can vary one while `HERDR_SESSION`
/// varies the other, so either alone lets two unrelated servers claim each
/// other's pane ids, and neither alone is stable across a restart of the same
/// server. Both paths are home directories, so hash them rather than write one
/// into an item file that syncs to other machines.
pub fn server_id_for(data_dir: &Path, socket_path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(path_identity_key(data_dir).as_encoded_bytes());
    hasher.update([0]);
    hasher.update(path_identity_key(socket_path).as_encoded_bytes());
    format!("srv-{:.12}", format!("{:x}", hasher.finalize()))
}

/// Two spellings of one path are one server, and a relative or symlinked
/// spelling would otherwise orphan the bindings a previous start wrote. A
/// relative spelling is worse than that: `herdr.sock` names a different socket
/// in every working directory, so leaving it relative would give two unrelated
/// servers one identity. Anchor it to the working directory first, then resolve
/// it, falling back to the parent because a socket file usually does not exist
/// yet when identity is computed.
fn path_identity_key(path: &Path) -> std::ffi::OsString {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => path.to_path_buf(),
        }
    };
    if let Ok(resolved) = fs::canonicalize(&absolute) {
        return resolved.into_os_string();
    }
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => fs::canonicalize(parent)
            .map(|dir| dir.join(name))
            .unwrap_or(absolute),
        _ => absolute,
    }
    .into_os_string()
}

pub fn default_root() -> PathBuf {
    crate::config::state_dir().join("day-board")
}

impl crate::app::state::AppState {
    pub(crate) fn derive_day_item(
        &self,
        item: &DayItem,
        stale_after: Duration,
        links_closed: bool,
        observed_at: Instant,
    ) -> DerivedDayItem {
        derive(
            item,
            &self.agent_host_name,
            &self.day_server_id,
            stale_after,
            links_closed,
            |public_id| {
                self.workspaces.iter().find_map(|workspace| {
                    let pane_id =
                        workspace
                            .public_pane_numbers
                            .iter()
                            .find_map(|(pane_id, number)| {
                                (crate::workspace::public_pane_id_for_number(
                                    &workspace.id,
                                    *number,
                                ) == public_id)
                                    .then_some(*pane_id)
                            })?;
                    let pane = workspace.pane_state(pane_id)?;
                    let terminal = self.terminals.get(&pane.attached_terminal_id)?;
                    Some(DayPaneEvidence {
                        state: pane.agent_projection(terminal).state,
                        inactive_for: pane.activity.inactive_for(observed_at),
                    })
                })
            },
        )
    }
}

pub fn load(root: &Path) -> DayBoard {
    let items_dir = root.join("items");
    let mut board = DayBoard::default();
    let entries = match fs::read_dir(&items_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return board,
        Err(error) => {
            board.load_errors.push(DayItemLoadError {
                path: items_dir,
                message: error.to_string(),
            });
            return board;
        }
    };

    let mut paths = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) if entry.path().extension().is_some_and(|ext| ext == "md") => {
                paths.push(entry.path());
            }
            Ok(_) => {}
            Err(error) => board.load_errors.push(DayItemLoadError {
                path: items_dir.clone(),
                message: error.to_string(),
            }),
        }
    }
    paths.sort();

    for path in paths {
        match fs::read_to_string(&path)
            .map_err(|error| error.to_string())
            .and_then(|contents| parse_item_file(&path, &contents))
        {
            Ok(item) => {
                if board.items.insert(item.id.clone(), item).is_some() {
                    board.load_errors.push(DayItemLoadError {
                        path,
                        message: "duplicate day item id".to_string(),
                    });
                }
            }
            Err(message) => board.load_errors.push(DayItemLoadError { path, message }),
        }
    }
    board
}

pub fn write_item(root: &Path, item: &DayItem) -> Result<(), String> {
    let _lock = lock_store(root)?;
    write_item_unlocked(root, item)
}

fn write_item_unlocked(root: &Path, item: &DayItem) -> Result<(), String> {
    validate_item(item)?;
    let items_dir = root.join("items");
    fs::create_dir_all(&items_dir).map_err(|error| {
        format!(
            "cannot create day item directory {}: {error}",
            items_dir.display()
        )
    })?;
    let target = items_dir.join(format!("{}.md", item.id));
    let temporary = items_dir.join(format!(
        ".{}.tmp-{}-{}",
        item.id,
        std::process::id(),
        unix_nanos()
    ));
    let contents = format_item_file(item)?;
    let write_result = (|| -> Result<(), String> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("cannot create {}: {error}", temporary.display()))?;
        file.write_all(contents.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("cannot write {}: {error}", temporary.display()))?;
        crate::platform::replace_file_durably(&temporary, &target)
            .map_err(|error| format!("cannot replace {}: {error}", target.display()))
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

fn lock_store(root: &Path) -> Result<fs::File, String> {
    fs::create_dir_all(root)
        .map_err(|error| format!("cannot create day item store {}: {error}", root.display()))?;
    let lock_path = root.join(".items.lock");
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| format!("cannot open {}: {error}", lock_path.display()))?;
    lock.lock()
        .map_err(|error| format!("cannot lock {}: {error}", lock_path.display()))?;
    Ok(lock)
}

/// Serialize cross-process read-modify-write operations through one store lock.
/// The item is loaded after the lock is held, so two named servers cannot write
/// mutations built from different cached copies of the same file.
pub fn update_item(
    root: &Path,
    id: &str,
    update: impl FnOnce(&mut DayItem),
) -> Result<DayBoard, DayItemUpdateError> {
    let _lock = lock_store(root).map_err(DayItemUpdateError::Store)?;

    let mut board = load(root);
    let item = board
        .items
        .get_mut(id)
        .ok_or(DayItemUpdateError::NotFound)?;
    update(item);
    write_item_unlocked(root, item).map_err(DayItemUpdateError::Store)?;
    Ok(board)
}

fn format_item_file(item: &DayItem) -> Result<String, String> {
    let front_matter = serde_json::to_string_pretty(&DayItemFrontMatter::from_item(item))
        .map_err(|error| format!("cannot encode day item {}: {error}", item.id))?;
    Ok(format!("---\n{front_matter}\n---\n{}\n", item.title))
}

fn parse_item_file(path: &Path, contents: &str) -> Result<DayItem, String> {
    let Some(contents) = contents.strip_prefix("---\n") else {
        return Err("missing JSON front matter opening delimiter".to_string());
    };
    let Some((front_matter, body)) = contents.split_once("\n---\n") else {
        return Err("missing JSON front matter closing delimiter".to_string());
    };
    let metadata: DayItemFrontMatter = serde_json::from_str(front_matter)
        .map_err(|error| format!("invalid JSON front matter: {error}"))?;
    let title = body.strip_suffix('\n').unwrap_or(body).to_string();
    let item = metadata.into_item(title);
    validate_item(&item)?;
    let file_id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| "day item filename is not valid UTF-8".to_string())?;
    if file_id != item.id {
        return Err(format!(
            "day item id {} does not match filename {file_id}",
            item.id
        ));
    }
    Ok(item)
}

fn validate_item(item: &DayItem) -> Result<(), String> {
    if item.id.is_empty()
        || !item
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err("day item id must contain only ASCII letters, digits, '-' or '_'".to_string());
    }
    if item.title.trim().is_empty() || item.title.contains(['\n', '\r']) {
        return Err("day item title must be one non-empty line".to_string());
    }
    if item
        .bindings
        .iter()
        .any(|(host, binding)| host.trim().is_empty() || binding.pane_id.trim().is_empty())
    {
        return Err("day item bindings require a host and pane id".to_string());
    }
    Ok(())
}

fn unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

pub fn unix_seconds_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn new_id(now_ms: u64) -> Result<String, String> {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut random = [0_u8; 6];
    getrandom::fill(&mut random)
        .map_err(|error| format!("cannot generate day item id: {error}"))?;
    let mut value = (u128::from(now_ms) << 48)
        | random
            .into_iter()
            .fold(0_u128, |value, byte| (value << 8) | u128::from(byte));
    let mut encoded = [b'0'; 20];
    for byte in encoded.iter_mut().rev() {
        *byte = ALPHABET[(value & 31) as usize];
        value >>= 5;
    }
    String::from_utf8(encoded.to_vec()).map_err(|error| error.to_string())
}

pub fn derive(
    item: &DayItem,
    local_host: &str,
    local_server: &str,
    stale_after: Duration,
    links_closed: bool,
    pane: impl Fn(&str) -> Option<DayPaneEvidence>,
) -> DerivedDayItem {
    if item.done_at.is_some() || (!item.links.is_empty() && links_closed) {
        return DerivedDayItem {
            item: item.clone(),
            column: DayColumn::Done,
            stale: false,
            notice: None,
        };
    }

    let Some(binding) = item.bindings.get(local_host) else {
        return DerivedDayItem {
            item: item.clone(),
            column: DayColumn::Todo,
            stale: false,
            notice: None,
        };
    };
    if binding
        .server_id
        .as_deref()
        .is_some_and(|server| server != local_server)
    {
        // Another server's pane id means nothing here, and resolving it against
        // this server's panes would report a stranger's column.
        return DerivedDayItem {
            item: item.clone(),
            column: DayColumn::Todo,
            stale: false,
            notice: None,
        };
    }
    let Some(evidence) = pane(&binding.pane_id) else {
        return DerivedDayItem {
            item: item.clone(),
            column: DayColumn::Todo,
            stale: false,
            notice: Some(format!(
                "bound pane {} no longer exists on {local_host}",
                binding.pane_id
            )),
        };
    };
    let column = match evidence.state {
        crate::detect::AgentState::Working => DayColumn::Working,
        crate::detect::AgentState::Blocked => DayColumn::Blocked,
        crate::detect::AgentState::Idle | crate::detect::AgentState::Unknown => DayColumn::Todo,
    };
    DerivedDayItem {
        item: item.clone(),
        column,
        stale: column == DayColumn::Working
            && evidence.inactive_for
                > Duration::from_secs(item.stale_after_seconds.unwrap_or(stale_after.as_secs())),
        notice: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "herdr-day-{label}-{}",
            crate::config::test_unique_suffix()
        ))
    }

    fn item(id: &str, title: &str) -> DayItem {
        DayItem {
            id: id.to_string(),
            title: title.to_string(),
            kind: DayItemKind::Task,
            source: DayItemSource::Manual,
            note: Some("after standup".to_string()),
            links: DayLinks {
                tickets: vec!["SCA-7".to_string()],
                prs: vec!["https://github.com/acme/app/pull/9".to_string()],
            },
            bindings: BTreeMap::from([(
                "ub1".to_string(),
                DayBinding {
                    server_id: Some("default".to_string()),
                    pane_id: "w_main:p1".to_string(),
                    bound_at: 1_790_000_000,
                },
            )]),
            added_at: 1_789_000_000,
            done_at: None,
            dismissed: false,
            stale_after_seconds: Some(900),
        }
    }

    #[test]
    fn item_file_round_trips_json_front_matter_and_title_body() {
        let root = temp_root("round-trip");
        let expected = item("01K5DAYITEM", "Reply to refund thread");

        write_item(&root, &expected).expect("write item");
        let loaded = load(&root);

        assert_eq!(loaded.load_errors, Vec::new());
        assert_eq!(loaded.items.get(&expected.id), Some(&expected));
        let body =
            std::fs::read_to_string(root.join("items/01K5DAYITEM.md")).expect("read written item");
        assert!(body.starts_with("---\n"));
        assert!(body.ends_with("---\nReply to refund thread\n"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rewriting_one_item_does_not_rewrite_a_sibling_file() {
        let root = temp_root("isolated-mutation");
        let mut first = item("01K5FIRST", "First");
        let second = item("01K5SECOND", "Second");
        write_item(&root, &first).expect("write first item");
        write_item(&root, &second).expect("write second item");
        let second_path = root.join("items/01K5SECOND.md");
        let before = std::fs::read(&second_path).expect("read sibling before mutation");

        first.note = Some("changed".to_string());
        write_item(&root, &first).expect("rewrite first item");

        assert_eq!(
            std::fs::read(second_path).expect("read sibling after mutation"),
            before
        );
        assert_eq!(load(&root).items.len(), 2);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn board_links_only_include_unresolved_visible_items() {
        let active = item("01K5ACTIVE", "Active");
        let mut done = item("01K5DONE", "Done");
        done.done_at = Some(1_800_000_000);
        done.links.tickets = vec!["SCA-8".into()];
        done.links.prs = vec!["https://github.com/acme/app/pull/8".into()];
        let mut dismissed = item("01K5DISMISSED", "Dismissed");
        dismissed.dismissed = true;
        dismissed.links.tickets = vec!["SCA-9".into()];
        dismissed.links.prs = vec!["https://github.com/acme/app/pull/10".into()];
        let board = DayBoard {
            items: BTreeMap::from([
                (active.id.clone(), active),
                (done.id.clone(), done),
                (dismissed.id.clone(), dismissed),
            ]),
            load_errors: Vec::new(),
        };

        assert_eq!(
            board.links(),
            DayLinks {
                tickets: vec!["SCA-7".into()],
                prs: vec!["https://github.com/acme/app/pull/9".into()],
            }
        );
    }

    #[test]
    fn malformed_item_remains_visible_as_a_load_error() {
        let root = temp_root("load-error");
        let items = root.join("items");
        std::fs::create_dir_all(&items).expect("create items directory");
        std::fs::write(items.join("broken.md"), "not front matter\n")
            .expect("write malformed item");

        let loaded = load(&root);

        assert!(loaded.items.is_empty());
        assert_eq!(loaded.load_errors.len(), 1);
        assert_eq!(loaded.load_errors[0].path, items.join("broken.md"));
        assert!(loaded.load_errors[0]
            .message
            .contains("missing JSON front matter opening delimiter"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn column_is_derived_from_completion_then_local_live_binding() {
        let mut candidate = item("01K5DERIVE", "Derive me");
        let stale_after = Duration::from_secs(600);
        let evidence = |state| {
            move |pane_id: &str| {
                (pane_id == "w_main:p1").then_some(DayPaneEvidence {
                    state,
                    inactive_for: Duration::from_secs(5),
                })
            }
        };

        candidate.done_at = Some(1_800_000_000);
        assert_eq!(
            derive(
                &candidate,
                "ub1",
                "default",
                stale_after,
                false,
                evidence(crate::detect::AgentState::Working)
            )
            .column,
            DayColumn::Done
        );

        candidate.done_at = None;
        assert_eq!(
            derive(
                &candidate,
                "ub1",
                "default",
                stale_after,
                true,
                evidence(crate::detect::AgentState::Blocked)
            )
            .column,
            DayColumn::Done
        );
        assert_eq!(
            derive(
                &candidate,
                "ub1",
                "default",
                stale_after,
                false,
                evidence(crate::detect::AgentState::Working)
            )
            .column,
            DayColumn::Working
        );
        assert_eq!(
            derive(
                &candidate,
                "ub1",
                "default",
                stale_after,
                false,
                evidence(crate::detect::AgentState::Blocked)
            )
            .column,
            DayColumn::Blocked
        );

        candidate.bindings.clear();
        assert_eq!(
            derive(
                &candidate,
                "ub1",
                "default",
                stale_after,
                false,
                evidence(crate::detect::AgentState::Working)
            )
            .column,
            DayColumn::Todo
        );
    }

    #[test]
    fn stale_starts_after_the_quiet_threshold_and_only_while_working() {
        let mut candidate = item("01K5STALE", "Quiet work");
        candidate.stale_after_seconds = None;
        let at = |state, seconds| {
            derive(
                &candidate,
                "ub1",
                "default",
                Duration::from_secs(600),
                false,
                move |_| {
                    Some(DayPaneEvidence {
                        state,
                        inactive_for: Duration::from_secs(seconds),
                    })
                },
            )
        };

        assert!(!at(crate::detect::AgentState::Working, 600).stale);
        assert!(at(crate::detect::AgentState::Working, 601).stale);
        assert!(!at(crate::detect::AgentState::Blocked, 601).stale);
    }

    #[test]
    fn binding_resolves_only_on_the_server_that_created_it() {
        let mut bound_server = crate::app::state::AppState::test_with_adversarial_identity_state();
        let mut other_server = crate::app::state::AppState::test_with_adversarial_identity_state();
        other_server.workspaces[0].id = bound_server.workspaces[0].id.clone();
        bound_server.day_server_id = "session-a".to_string();
        other_server.day_server_id = "session-b".to_string();
        bound_server.assert_invariants_for_test();
        other_server.assert_invariants_for_test();

        let pane_id = bound_server.workspaces[0].tabs[0].root_pane;
        let public_pane_id = crate::workspace::public_pane_id_for_number(
            &bound_server.workspaces[0].id,
            bound_server.workspaces[0]
                .public_pane_number(pane_id)
                .expect("public pane number"),
        );
        let terminal_id = bound_server.workspaces[0]
            .terminal_id(pane_id)
            .expect("terminal id")
            .clone();
        bound_server
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal")
            .set_raw_agent_state_for_test(crate::detect::AgentState::Working);
        let other_pane_id = other_server.workspaces[0].tabs[0].root_pane;
        let other_terminal_id = other_server.workspaces[0]
            .terminal_id(other_pane_id)
            .expect("terminal id")
            .clone();
        other_server
            .terminals
            .get_mut(&other_terminal_id)
            .expect("terminal")
            .set_raw_agent_state_for_test(crate::detect::AgentState::Blocked);

        let mut candidate = item("01K5SERVER", "Stay on one server");
        candidate.bindings = BTreeMap::from([(
            bound_server.agent_host_name.clone(),
            DayBinding {
                server_id: Some(bound_server.day_server_id.clone()),
                pane_id: public_pane_id,
                bound_at: 1_790_000_000,
            },
        )]);

        let bound = bound_server.derive_day_item(
            &candidate,
            Duration::from_secs(600),
            false,
            Instant::now(),
        );
        let mismatched = other_server.derive_day_item(
            &candidate,
            Duration::from_secs(600),
            false,
            Instant::now(),
        );

        assert_eq!(bound.column, DayColumn::Working);
        assert_eq!(mismatched.column, DayColumn::Todo);
        assert_eq!(mismatched.notice, None);
        bound_server.assert_invariants_for_test();
        other_server.assert_invariants_for_test();
    }

    #[test]
    fn adversarial_state_keeps_missing_pane_binding_as_todo_with_notice() {
        let mut state = crate::app::state::AppState::test_with_adversarial_identity_state();
        let mut candidate = item("01K5MISSING", "Keep me visible");
        candidate.bindings = BTreeMap::from([(
            state.agent_host_name.clone(),
            DayBinding {
                server_id: Some(state.day_server_id.clone()),
                pane_id: "w_missing:p9".to_string(),
                bound_at: 1_790_000_000,
            },
        )]);
        state
            .day_board
            .items
            .insert(candidate.id.clone(), candidate.clone());
        state.assert_invariants_for_test();

        let derived =
            state.derive_day_item(&candidate, Duration::from_secs(600), false, Instant::now());

        assert_eq!(derived.column, DayColumn::Todo);
        assert!(derived.notice.as_deref().is_some_and(|notice| {
            notice.contains("w_missing:p9") && notice.contains(&state.agent_host_name)
        }));
        assert_eq!(state.day_board.items.get(&candidate.id), Some(&candidate));
        state.assert_invariants_for_test();
    }

    #[test]
    fn server_id_separates_servers_reached_through_different_socket_paths() {
        let root = temp_root("server-id-socket");
        let data = root.join("data");
        fs::create_dir_all(root.join("one")).expect("one");
        fs::create_dir_all(root.join("two")).expect("two");
        fs::create_dir_all(&data).expect("data");

        // `HERDR_SOCKET_PATH` decides which server a client reaches, so these are
        // two servers however their sessions are named.
        let left = server_id_for(&data, &root.join("one/herdr.sock"));
        let right = server_id_for(&data, &root.join("two/herdr.sock"));

        assert_ne!(left, right);
        assert!(left.starts_with("srv-"), "{left}");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn server_id_separates_servers_restoring_different_panes_on_one_socket() {
        let root = temp_root("server-id-data");
        let socket = root.join("herdr.sock");
        let plain = root.join("data");
        let named = root.join("data/sessions/work");
        fs::create_dir_all(&plain).expect("plain");
        fs::create_dir_all(&named).expect("named");

        // `HERDR_SESSION` moves the data directory that `persist::io` restores
        // workspaces and panes from, so the same socket path serves a different
        // set of panes and its pane ids mean something else.
        let left = server_id_for(&plain, &socket);
        let right = server_id_for(&named, &socket);

        assert_ne!(left, right);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn server_id_is_stable_when_one_server_restarts() {
        let root = temp_root("server-id-restart");
        let data = root.join("data");
        fs::create_dir_all(&data).expect("data");

        // Nothing but the two paths may enter the value, or a restart would
        // rename the server and silently orphan every binding it wrote.
        assert_eq!(
            server_id_for(&data, &root.join("herdr.sock")),
            server_id_for(&data, &root.join("herdr.sock"))
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn server_id_is_stable_across_spellings_of_one_path() {
        let root = temp_root("server-id-spelling");
        let dir = root.join("nested");
        fs::create_dir_all(&dir).expect("dir");

        let plain = server_id_for(&dir, &dir.join("herdr.sock"));
        let indirect = server_id_for(
            &root.join("nested/../nested"),
            &root.join("nested/../nested/herdr.sock"),
        );

        assert_eq!(plain, indirect);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_relative_socket_path_is_anchored_to_the_working_directory() {
        // `HERDR_SOCKET_PATH=herdr.sock` names a different socket in every
        // working directory, so the bare string cannot be the identity. Anchored,
        // it is the absolute path, which the test above shows differs per
        // directory. Asserting the equality avoids moving the process cwd.
        let cwd = std::env::current_dir().expect("cwd");

        assert_eq!(
            server_id_for(&cwd, Path::new("herdr.sock")),
            server_id_for(&cwd, &cwd.join("herdr.sock"))
        );
    }

    #[test]
    fn server_id_never_carries_the_paths_themselves() {
        let root = temp_root("server-id-secret");
        fs::create_dir_all(&root).expect("root");

        let id = server_id_for(&root, &root.join("herdr.sock"));

        // Item files sync to other machines, so the home directory must not ride
        // along in one.
        assert!(!id.contains(&root.to_string_lossy().to_string()), "{id}");
        assert!(!id.contains("herdr.sock"), "{id}");

        fs::remove_dir_all(&root).ok();
    }
}
