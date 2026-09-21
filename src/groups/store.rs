use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::model::{AuthorityState, GroupId, GroupRecord, MutationError};
use super::AuthorityId;

const STORE_SCHEMA_VERSION: u32 = 1;
const AUTHORITY_FILE_NAME: &str = "group-authority.json";
const GROUPS_FILE_NAME: &str = "groups.json";

#[derive(Debug, Serialize, Deserialize)]
struct AuthorityFile {
    schema_version: u32,
    authority_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct GroupStoreFile {
    schema_version: u32,
    authority_id: String,
    revision: u64,
    next_group_id: u64,
    groups: Vec<GroupRecord>,
}

#[derive(Debug)]
enum RuntimeState {
    Ready(AuthorityState),
    Unavailable(String),
}

#[derive(Debug)]
pub(crate) struct Runtime {
    groups_path: PathBuf,
    state: RuntimeState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeError {
    Unavailable(String),
    Mutation(MutationError),
    Persistence(String),
}

impl Runtime {
    #[cfg(not(test))]
    pub(crate) fn load_default() -> Self {
        Self::load(&crate::session::data_dir())
    }

    pub(crate) fn load(data_dir: &Path) -> Self {
        let groups_path = data_dir.join(GROUPS_FILE_NAME);
        match load_or_initialize(data_dir) {
            Ok(state) => Self {
                groups_path,
                state: RuntimeState::Ready(state),
            },
            Err(error) => {
                tracing::warn!(%error, "group authority is unavailable");
                Self {
                    groups_path,
                    state: RuntimeState::Unavailable(error),
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn unavailable_for_tests() -> Self {
        Self {
            groups_path: PathBuf::new(),
            state: RuntimeState::Unavailable("group test runtime not configured".to_string()),
        }
    }

    pub(crate) fn authority(&self) -> Result<&AuthorityState, RuntimeError> {
        match &self.state {
            RuntimeState::Ready(state) => Ok(state),
            RuntimeState::Unavailable(error) => Err(RuntimeError::Unavailable(error.clone())),
        }
    }

    pub(crate) fn create(
        &mut self,
        name: &str,
        expected_revision: u64,
    ) -> Result<(GroupRecord, u64), RuntimeError> {
        let (candidate, record) = self
            .authority()?
            .create(name, expected_revision)
            .map_err(RuntimeError::Mutation)?;
        self.commit(candidate)?;
        let revision = self.authority()?.revision();
        Ok((record, revision))
    }

    pub(crate) fn rename(
        &mut self,
        id: &GroupId,
        name: &str,
        expected_revision: u64,
    ) -> Result<(GroupRecord, u64), RuntimeError> {
        let (candidate, record) = self
            .authority()?
            .rename(id, name, expected_revision)
            .map_err(RuntimeError::Mutation)?;
        self.commit(candidate)?;
        let revision = self.authority()?.revision();
        Ok((record, revision))
    }

    pub(crate) fn delete(
        &mut self,
        id: &GroupId,
        expected_revision: u64,
    ) -> Result<(GroupRecord, u64), RuntimeError> {
        let (candidate, record) = self
            .authority()?
            .delete(id, expected_revision)
            .map_err(RuntimeError::Mutation)?;
        self.commit(candidate)?;
        let revision = self.authority()?.revision();
        Ok((record, revision))
    }

    fn commit(&mut self, candidate: AuthorityState) -> Result<(), RuntimeError> {
        save_store(&self.groups_path, &candidate)
            .map_err(|error| RuntimeError::Persistence(error.to_string()))?;
        self.state = RuntimeState::Ready(candidate);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_groups_path_for_test(&mut self, path: PathBuf) {
        self.groups_path = path;
    }
}

fn load_or_initialize(data_dir: &Path) -> Result<AuthorityState, String> {
    let authority_path = data_dir.join(AUTHORITY_FILE_NAME);
    let groups_path = data_dir.join(GROUPS_FILE_NAME);
    let authority_exists = authority_path
        .try_exists()
        .map_err(|error| format!("cannot inspect authority file: {error}"))?;
    let groups_exist = groups_path
        .try_exists()
        .map_err(|error| format!("cannot inspect group store: {error}"))?;

    match (authority_exists, groups_exist) {
        (true, true) => {
            let authority_id = load_authority(&authority_path)?;
            load_store(&groups_path, &authority_id)
        }
        (true, false) => Err("group store is missing while the authority file exists".to_string()),
        (false, true) => Err("authority file is missing while the group store exists".to_string()),
        (false, false) => {
            let authority_id = generate_authority_id()?;
            save_authority(&authority_path, &authority_id)
                .map_err(|error| format!("cannot persist new authority: {error}"))?;
            let state = AuthorityState::empty(authority_id);
            save_store(&groups_path, &state)
                .map_err(|error| format!("cannot persist empty group store: {error}"))?;
            Ok(state)
        }
    }
}

fn generate_authority_id() -> Result<AuthorityId, String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| format!("operating system randomness failed: {error}"))?;
    Ok(AuthorityId::from_random_bytes(bytes))
}

fn load_authority(path: &Path) -> Result<AuthorityId, String> {
    let json = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read authority file: {error}"))?;
    let file: AuthorityFile = serde_json::from_str(&json)
        .map_err(|error| format!("cannot parse authority file: {error}"))?;
    if file.schema_version != STORE_SCHEMA_VERSION {
        return Err(format!(
            "unsupported authority schema version {}",
            file.schema_version
        ));
    }
    AuthorityId::parse(&file.authority_id)
}

fn save_authority(path: &Path, authority_id: &AuthorityId) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(&AuthorityFile {
        schema_version: STORE_SCHEMA_VERSION,
        authority_id: authority_id.as_str().to_string(),
    })?;
    crate::persist::commit_json_to_path(path, &json)
}

fn load_store(path: &Path, authority_id: &AuthorityId) -> Result<AuthorityState, String> {
    let json = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read group store: {error}"))?;
    let file: GroupStoreFile = serde_json::from_str(&json)
        .map_err(|error| format!("cannot parse group store: {error}"))?;
    if file.schema_version != STORE_SCHEMA_VERSION {
        return Err(format!(
            "unsupported group store schema version {}",
            file.schema_version
        ));
    }
    let stored_authority = AuthorityId::parse(&file.authority_id)?;
    if &stored_authority != authority_id {
        return Err("group store authority does not match the identity file".to_string());
    }
    AuthorityState::from_persisted(
        stored_authority,
        file.revision,
        file.next_group_id,
        file.groups,
    )
}

fn save_store(path: &Path, state: &AuthorityState) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(&GroupStoreFile {
        schema_version: STORE_SCHEMA_VERSION,
        authority_id: state.authority_id().as_str().to_string(),
        revision: state.revision(),
        next_group_id: state.next_group_id(),
        groups: state.records(),
    })?;
    crate::persist::commit_json_to_path(path, &json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::groups::GroupState;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let unique = format!(
                "herdr-group-store-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            );
            Self(std::env::temp_dir().join(unique))
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_authority_is_created_once_and_reused() {
        let dir = TestDir::new("identity");
        let first = Runtime::load(&dir.0);
        let first_id = first.authority().unwrap().authority_id().clone();
        let second = Runtime::load(&dir.0);

        assert_eq!(second.authority().unwrap().authority_id(), &first_id);
    }

    #[test]
    fn invalid_authority_fails_closed_without_replacement() {
        let dir = TestDir::new("invalid-identity");
        std::fs::create_dir_all(&dir.0).unwrap();
        let identity_path = dir.0.join(AUTHORITY_FILE_NAME);
        std::fs::write(&identity_path, "not json").unwrap();

        let runtime = Runtime::load(&dir.0);

        assert!(matches!(
            runtime.authority(),
            Err(RuntimeError::Unavailable(_))
        ));
        assert_eq!(std::fs::read_to_string(identity_path).unwrap(), "not json");
        assert!(!dir.0.join(GROUPS_FILE_NAME).exists());
    }

    #[test]
    fn unreadable_authority_fails_closed_without_replacement() {
        let dir = TestDir::new("unreadable-identity");
        std::fs::create_dir_all(dir.0.join(AUTHORITY_FILE_NAME)).unwrap();

        let runtime = Runtime::load(&dir.0);

        assert!(matches!(
            runtime.authority(),
            Err(RuntimeError::Unavailable(_))
        ));
        assert!(dir.0.join(AUTHORITY_FILE_NAME).is_dir());
        assert!(!dir.0.join(GROUPS_FILE_NAME).exists());
    }

    #[test]
    fn missing_authority_never_relabels_an_existing_group_store() {
        let dir = TestDir::new("missing-identity");
        std::fs::create_dir_all(&dir.0).unwrap();
        std::fs::write(dir.0.join(GROUPS_FILE_NAME), "{}").unwrap();

        let runtime = Runtime::load(&dir.0);

        assert!(matches!(
            runtime.authority(),
            Err(RuntimeError::Unavailable(_))
        ));
        assert!(!dir.0.join(AUTHORITY_FILE_NAME).exists());
    }

    #[test]
    fn missing_group_store_never_reuses_a_tombstoned_identity() {
        let dir = TestDir::new("missing-store");
        let mut runtime = Runtime::load(&dir.0);
        let (created, _) = runtime.create("Deleted", 0).unwrap();
        runtime.delete(&created.id, created.revision).unwrap();
        let authority_json = std::fs::read_to_string(dir.0.join(AUTHORITY_FILE_NAME)).unwrap();
        std::fs::remove_file(dir.0.join(GROUPS_FILE_NAME)).unwrap();

        let mut reloaded = Runtime::load(&dir.0);

        assert!(matches!(
            reloaded.authority(),
            Err(RuntimeError::Unavailable(_))
        ));
        assert!(matches!(
            reloaded.create("Replacement", 0),
            Err(RuntimeError::Unavailable(_))
        ));
        assert_eq!(
            std::fs::read_to_string(dir.0.join(AUTHORITY_FILE_NAME)).unwrap(),
            authority_json
        );
        assert!(!dir.0.join(GROUPS_FILE_NAME).exists());
    }

    #[test]
    fn active_group_survives_reload_after_the_session_is_cleared() {
        let dir = TestDir::new("empty-group-restart");
        let mut runtime = Runtime::load(&dir.0);
        let (created, _) = runtime.create("Empty", 0).unwrap();
        let session_path = dir.0.join("session.json");
        std::fs::write(&session_path, "discarded session").unwrap();
        std::fs::remove_file(session_path).unwrap();

        let reloaded = Runtime::load(&dir.0);

        assert_eq!(reloaded.authority().unwrap().records(), vec![created]);
    }

    #[test]
    fn tombstone_survives_reload() {
        let dir = TestDir::new("tombstone-restart");
        let mut runtime = Runtime::load(&dir.0);
        let (created, _) = runtime.create("Deleted", 0).unwrap();
        let (deleted, _) = runtime.delete(&created.id, created.revision).unwrap();
        let reloaded = Runtime::load(&dir.0);

        let records = reloaded.authority().unwrap().records();
        assert_eq!(records, vec![deleted]);
        assert!(matches!(records[0].state, GroupState::Deleted));
    }

    #[test]
    fn failed_commit_keeps_previous_memory_and_file() {
        let dir = TestDir::new("failed-write");
        let mut runtime = Runtime::load(&dir.0);
        let blocker = dir.0.join("not-a-directory");
        std::fs::write(&blocker, "block").unwrap();
        runtime.set_groups_path_for_test(blocker.join(GROUPS_FILE_NAME));

        assert!(matches!(
            runtime.create("Uncommitted", 0),
            Err(RuntimeError::Persistence(_))
        ));
        assert_eq!(runtime.authority().unwrap().revision(), 0);
        assert!(runtime.authority().unwrap().records().is_empty());
        assert!(Runtime::load(&dir.0)
            .authority()
            .unwrap()
            .records()
            .is_empty());
    }

    #[test]
    fn duplicate_active_and_deleted_records_fail_closed() {
        let dir = TestDir::new("duplicate-tombstone");
        let runtime = Runtime::load(&dir.0);
        let authority_id = runtime.authority().unwrap().authority_id().clone();
        let id = GroupId {
            owner: authority_id.clone(),
            local: 1,
        };
        let file = GroupStoreFile {
            schema_version: STORE_SCHEMA_VERSION,
            authority_id: authority_id.as_str().to_string(),
            revision: 2,
            next_group_id: 2,
            groups: vec![
                GroupRecord {
                    id: id.clone(),
                    revision: 1,
                    state: GroupState::Active {
                        name: "Work".into(),
                    },
                },
                GroupRecord {
                    id,
                    revision: 2,
                    state: GroupState::Deleted,
                },
            ],
        };
        std::fs::write(
            dir.0.join(GROUPS_FILE_NAME),
            serde_json::to_string_pretty(&file).unwrap(),
        )
        .unwrap();

        let reloaded = Runtime::load(&dir.0);

        assert!(matches!(
            reloaded.authority(),
            Err(RuntimeError::Unavailable(_))
        ));
    }
}
