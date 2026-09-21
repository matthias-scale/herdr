use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::model::{AuthorityState, GroupId, GroupRecord, MutationError};
use super::AuthorityId;

const AUTHORITY_SCHEMA_VERSION: u32 = 2;
const GROUP_STORE_SCHEMA_VERSION: u32 = 1;
const AUTHORITY_FILE_NAME: &str = "group-authority.json";
const GROUPS_FILE_NAME: &str = "groups.json";

#[derive(Debug, Serialize, Deserialize)]
struct AuthorityFile {
    schema_version: u32,
    authority_id: String,
    next_group_id: u64,
    retired_groups: Vec<RetiredGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RetiredGroup {
    local: u64,
    revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AuthorityLedger {
    authority_id: AuthorityId,
    next_group_id: u64,
    retired_groups: BTreeMap<u64, u64>,
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
    authority_path: PathBuf,
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
        let authority_path = data_dir.join(AUTHORITY_FILE_NAME);
        let groups_path = data_dir.join(GROUPS_FILE_NAME);
        match load_or_initialize(data_dir) {
            Ok(state) => Self {
                authority_path,
                groups_path,
                state: RuntimeState::Ready(state),
            },
            Err(error) => {
                tracing::warn!(%error, "group authority is unavailable");
                Self {
                    authority_path,
                    groups_path,
                    state: RuntimeState::Unavailable(error),
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn unavailable_for_tests() -> Self {
        Self {
            authority_path: PathBuf::new(),
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
        let current_ledger = AuthorityLedger::from_state(self.authority()?);
        let candidate_ledger = AuthorityLedger::from_state(&candidate);
        let ledger_changed = candidate_ledger != current_ledger;
        save_store(&self.groups_path, &candidate)
            .map_err(|error| RuntimeError::Persistence(error.to_string()))?;
        if ledger_changed {
            if let Err(error) = save_authority(&self.authority_path, &candidate_ledger) {
                self.state = RuntimeState::Unavailable(
                    "group identity ledger persistence failed after the record store advanced"
                        .to_string(),
                );
                return Err(RuntimeError::Persistence(error.to_string()));
            }
        }
        self.state = RuntimeState::Ready(candidate);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_groups_path_for_test(&mut self, path: PathBuf) {
        self.groups_path = path;
    }

    #[cfg(test)]
    pub(crate) fn set_authority_path_for_test(&mut self, path: PathBuf) {
        self.authority_path = path;
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
            let authority = load_authority(&authority_path)?;
            load_store(&groups_path, &authority)
        }
        (true, false) => Err("group store is missing while the authority file exists".to_string()),
        (false, true) => Err("authority file is missing while the group store exists".to_string()),
        (false, false) => {
            let authority_id = generate_authority_id()?;
            let state = AuthorityState::empty(authority_id);
            let authority = AuthorityLedger::from_state(&state);
            save_authority(&authority_path, &authority)
                .map_err(|error| format!("cannot persist new authority: {error}"))?;
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

impl AuthorityLedger {
    fn from_state(state: &AuthorityState) -> Self {
        let retired_groups = state
            .records()
            .into_iter()
            .filter_map(|record| {
                matches!(record.state, super::GroupState::Deleted)
                    .then_some((record.id.local, record.revision))
            })
            .collect();
        Self {
            authority_id: state.authority_id().clone(),
            next_group_id: state.next_group_id(),
            retired_groups,
        }
    }
}

fn load_authority(path: &Path) -> Result<AuthorityLedger, String> {
    let json = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read authority file: {error}"))?;
    let file: AuthorityFile = serde_json::from_str(&json)
        .map_err(|error| format!("cannot parse authority file: {error}"))?;
    if file.schema_version != AUTHORITY_SCHEMA_VERSION {
        return Err(format!(
            "unsupported authority schema version {}",
            file.schema_version
        ));
    }
    let authority_id = AuthorityId::parse(&file.authority_id)?;
    if file.next_group_id == 0 {
        return Err("authority allocation floor must be positive".to_string());
    }
    let mut retired_groups = BTreeMap::new();
    for retired in file.retired_groups {
        if retired.local == 0 || retired.local >= file.next_group_id {
            return Err("retired group id is outside the authority allocation floor".to_string());
        }
        if retired.revision == 0 {
            return Err("retired group revision must be positive".to_string());
        }
        if retired_groups
            .insert(retired.local, retired.revision)
            .is_some()
        {
            return Err("authority file contains a duplicate retired group id".to_string());
        }
    }
    Ok(AuthorityLedger {
        authority_id,
        next_group_id: file.next_group_id,
        retired_groups,
    })
}

fn save_authority(path: &Path, authority: &AuthorityLedger) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(&AuthorityFile {
        schema_version: AUTHORITY_SCHEMA_VERSION,
        authority_id: authority.authority_id.as_str().to_string(),
        next_group_id: authority.next_group_id,
        retired_groups: authority
            .retired_groups
            .iter()
            .map(|(&local, &revision)| RetiredGroup { local, revision })
            .collect(),
    })?;
    crate::persist::commit_json_to_path(path, &json)
}

fn load_store(path: &Path, authority: &AuthorityLedger) -> Result<AuthorityState, String> {
    let json = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read group store: {error}"))?;
    let file: GroupStoreFile = serde_json::from_str(&json)
        .map_err(|error| format!("cannot parse group store: {error}"))?;
    if file.schema_version != GROUP_STORE_SCHEMA_VERSION {
        return Err(format!(
            "unsupported group store schema version {}",
            file.schema_version
        ));
    }
    let stored_authority = AuthorityId::parse(&file.authority_id)?;
    if stored_authority != authority.authority_id {
        return Err("group store authority does not match the identity file".to_string());
    }
    let stored = AuthorityState::from_persisted(
        stored_authority.clone(),
        file.revision,
        file.next_group_id,
        file.groups,
    )?;
    if stored.next_group_id() > authority.next_group_id {
        return Err("group store allocation floor exceeds the authority ledger".to_string());
    }

    let mut records: BTreeMap<_, _> = stored
        .records()
        .into_iter()
        .map(|record| (record.id.local, record))
        .collect();
    for record in records.values() {
        if matches!(record.state, super::GroupState::Deleted)
            && authority.retired_groups.get(&record.id.local) != Some(&record.revision)
        {
            return Err("group store tombstone is absent from the authority ledger".to_string());
        }
    }
    for (&local, &revision) in &authority.retired_groups {
        records.insert(
            local,
            GroupRecord {
                id: GroupId {
                    owner: stored_authority.clone(),
                    local,
                },
                revision,
                state: super::GroupState::Deleted,
            },
        );
    }
    let revision = authority
        .retired_groups
        .values()
        .copied()
        .fold(stored.revision(), u64::max);
    let reconciled = AuthorityState::from_persisted(
        stored_authority,
        revision,
        authority.next_group_id,
        records.into_values().collect(),
    )?;
    if reconciled != stored {
        save_store(path, &reconciled)
            .map_err(|error| format!("cannot repair rolled-back group store: {error}"))?;
    }
    Ok(reconciled)
}

fn save_store(path: &Path, state: &AuthorityState) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(&GroupStoreFile {
        schema_version: GROUP_STORE_SCHEMA_VERSION,
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
    fn legacy_authority_without_identity_ledger_fails_closed() {
        let dir = TestDir::new("legacy-authority");
        let runtime = Runtime::load(&dir.0);
        let authority_id = runtime.authority().unwrap().authority_id().clone();
        let authority_path = dir.0.join(AUTHORITY_FILE_NAME);
        let legacy = serde_json::json!({
            "schema_version": 1,
            "authority_id": authority_id.as_str(),
        });
        std::fs::write(
            &authority_path,
            serde_json::to_string_pretty(&legacy).unwrap(),
        )
        .unwrap();

        let reloaded = Runtime::load(&dir.0);

        assert!(matches!(
            reloaded.authority(),
            Err(RuntimeError::Unavailable(_))
        ));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &std::fs::read_to_string(authority_path).unwrap()
            )
            .unwrap(),
            legacy
        );
    }

    #[test]
    fn new_authority_uses_the_ledger_schema() {
        let dir = TestDir::new("ledger-schema");
        Runtime::load(&dir.0);

        let authority: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.0.join(AUTHORITY_FILE_NAME)).unwrap(),
        )
        .unwrap();

        assert_eq!(authority["schema_version"], AUTHORITY_SCHEMA_VERSION);
        assert_eq!(authority["next_group_id"], 1);
        assert_eq!(authority["retired_groups"], serde_json::json!([]));
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
    fn rolled_back_group_store_reconstructs_a_retired_identity() {
        let dir = TestDir::new("rolled-back-store");
        let mut runtime = Runtime::load(&dir.0);
        let (first, _) = runtime.create("First", 0).unwrap();
        let before_second_group = std::fs::read_to_string(dir.0.join(GROUPS_FILE_NAME)).unwrap();
        let (second, _) = runtime.create("Second", 1).unwrap();
        let (deleted, _) = runtime.delete(&second.id, second.revision).unwrap();
        std::fs::write(dir.0.join(GROUPS_FILE_NAME), before_second_group).unwrap();

        let mut reloaded = Runtime::load(&dir.0);
        let records = reloaded.authority().unwrap().records();
        let (replacement, _) = reloaded.create("Replacement", 3).unwrap();

        assert_eq!(records, vec![first, deleted]);
        assert_eq!(replacement.id.owner, second.id.owner);
        assert_eq!(replacement.id.local, 3);
    }

    #[test]
    fn authority_tombstone_replaces_a_rolled_back_active_record() {
        let dir = TestDir::new("rolled-back-delete");
        let mut runtime = Runtime::load(&dir.0);
        runtime.create("First", 0).unwrap();
        let (second, _) = runtime.create("Second", 1).unwrap();
        let before_delete = std::fs::read_to_string(dir.0.join(GROUPS_FILE_NAME)).unwrap();
        let (deleted, _) = runtime.delete(&second.id, second.revision).unwrap();
        std::fs::write(dir.0.join(GROUPS_FILE_NAME), before_delete).unwrap();

        let reloaded = Runtime::load(&dir.0);
        let restored = reloaded.authority().unwrap().record(&second.id).unwrap();

        assert_eq!(restored, &deleted);
        assert!(matches!(restored.state, GroupState::Deleted));
    }

    #[test]
    fn rolled_back_group_store_with_an_unretired_gap_fails_closed() {
        let dir = TestDir::new("rolled-back-active-store");
        let mut runtime = Runtime::load(&dir.0);
        runtime.create("First", 0).unwrap();
        let before_second_group = std::fs::read_to_string(dir.0.join(GROUPS_FILE_NAME)).unwrap();
        runtime.create("Second", 1).unwrap();
        std::fs::write(dir.0.join(GROUPS_FILE_NAME), before_second_group).unwrap();

        let mut reloaded = Runtime::load(&dir.0);

        assert!(matches!(
            reloaded.authority(),
            Err(RuntimeError::Unavailable(_))
        ));
        assert!(matches!(
            reloaded.create("Replacement", 1),
            Err(RuntimeError::Unavailable(_))
        ));
    }

    #[test]
    fn failed_record_commit_keeps_previous_memory_and_files() {
        let dir = TestDir::new("failed-write");
        let mut runtime = Runtime::load(&dir.0);
        let authority_json = std::fs::read_to_string(dir.0.join(AUTHORITY_FILE_NAME)).unwrap();
        let blocker = dir.0.join("not-a-directory");
        std::fs::write(&blocker, "block").unwrap();
        runtime.set_groups_path_for_test(blocker.join(GROUPS_FILE_NAME));

        assert!(matches!(
            runtime.create("Uncommitted", 0),
            Err(RuntimeError::Persistence(_))
        ));
        assert_eq!(runtime.authority().unwrap().revision(), 0);
        assert!(runtime.authority().unwrap().records().is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.0.join(AUTHORITY_FILE_NAME)).unwrap(),
            authority_json
        );
        assert!(Runtime::load(&dir.0)
            .authority()
            .unwrap()
            .records()
            .is_empty());
    }

    #[test]
    fn failed_authority_commit_makes_the_advanced_store_unavailable() {
        let dir = TestDir::new("failed-authority-write");
        let mut runtime = Runtime::load(&dir.0);
        let blocker = dir.0.join("not-a-directory");
        std::fs::write(&blocker, "block").unwrap();
        runtime.set_authority_path_for_test(blocker.join(AUTHORITY_FILE_NAME));

        assert!(matches!(
            runtime.create("Uncommitted", 0),
            Err(RuntimeError::Persistence(_))
        ));
        assert!(matches!(
            runtime.authority(),
            Err(RuntimeError::Unavailable(_))
        ));
        assert!(matches!(
            Runtime::load(&dir.0).authority(),
            Err(RuntimeError::Unavailable(_))
        ));
    }

    #[test]
    fn failed_authority_commit_after_delete_makes_the_tombstone_store_unavailable() {
        let dir = TestDir::new("failed-delete-authority-write");
        let mut runtime = Runtime::load(&dir.0);
        let (created, _) = runtime.create("Deleted", 0).unwrap();
        let blocker = dir.0.join("not-a-directory");
        std::fs::write(&blocker, "block").unwrap();
        runtime.set_authority_path_for_test(blocker.join(AUTHORITY_FILE_NAME));

        assert!(matches!(
            runtime.delete(&created.id, created.revision),
            Err(RuntimeError::Persistence(_))
        ));
        assert!(matches!(
            runtime.authority(),
            Err(RuntimeError::Unavailable(_))
        ));
        assert!(matches!(
            Runtime::load(&dir.0).authority(),
            Err(RuntimeError::Unavailable(_))
        ));
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
            schema_version: GROUP_STORE_SCHEMA_VERSION,
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
