use std::collections::BTreeMap;
use std::fmt;

use base64::Engine;
use serde::{Deserialize, Serialize};

const AUTHORITY_ID_BYTES: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct AuthorityId(String);

impl<'de> Deserialize<'de> for AuthorityId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

impl AuthorityId {
    pub(crate) fn from_random_bytes(bytes: [u8; AUTHORITY_ID_BYTES]) -> Self {
        Self(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
    }

    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| "authority id is not canonical base64url".to_string())?;
        let bytes: [u8; AUTHORITY_ID_BYTES] = bytes
            .try_into()
            .map_err(|_| "authority id must encode exactly 16 bytes".to_string())?;
        let parsed = Self::from_random_bytes(bytes);
        if parsed.0 != value {
            return Err("authority id is not canonical base64url".to_string());
        }
        Ok(parsed)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AuthorityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct GroupId {
    pub owner: AuthorityId,
    pub local: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum GroupState {
    Active { name: String },
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GroupRecord {
    pub id: GroupId,
    pub revision: u64,
    #[serde(flatten)]
    pub state: GroupState,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneGroupMembership {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<GroupId>,
    #[serde(default)]
    pub revision: u64,
}

impl PaneGroupMembership {
    pub(crate) fn is_default(&self) -> bool {
        self.group_id.is_none() && self.revision == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OwnedPaneMembership {
    pub pane_id: String,
    pub membership: PaneGroupMembership,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GroupAuthoritySnapshot {
    pub authority_id: AuthorityId,
    pub revision: u64,
    pub groups: Vec<GroupRecord>,
    pub memberships: Vec<OwnedPaneMembership>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MutationError {
    Deleted,
    ForeignOwner,
    InvalidName,
    NotFound,
    RevisionConflict { expected: u64, actual: u64 },
    RevisionExhausted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthorityState {
    authority_id: AuthorityId,
    revision: u64,
    next_group_id: u64,
    records: BTreeMap<u64, GroupRecord>,
}

impl AuthorityState {
    pub(crate) fn empty(authority_id: AuthorityId) -> Self {
        Self {
            authority_id,
            revision: 0,
            next_group_id: 1,
            records: BTreeMap::new(),
        }
    }

    pub(crate) fn from_persisted(
        authority_id: AuthorityId,
        revision: u64,
        next_group_id: u64,
        records: Vec<GroupRecord>,
    ) -> Result<Self, String> {
        if next_group_id == 0 {
            return Err("next group id must be positive".to_string());
        }
        let mut by_local = BTreeMap::new();
        for record in records {
            AuthorityId::parse(record.id.owner.as_str())?;
            if record.id.owner != authority_id {
                return Err("group record is owned by another authority".to_string());
            }
            if record.id.local == 0 {
                return Err("group local id must be positive".to_string());
            }
            if record.revision == 0 || record.revision > revision {
                return Err("group record revision is outside the store revision".to_string());
            }
            if let GroupState::Active { name } = &record.state {
                if normalize_name(name).as_deref() != Some(name.as_str()) {
                    return Err("active group has an invalid name".to_string());
                }
            }
            let local = record.id.local;
            if by_local.insert(local, record).is_some() {
                return Err("group store contains a duplicate group id".to_string());
            }
        }
        let mut expected_local = 1_u64;
        for local in by_local.keys() {
            if *local != expected_local {
                return Err("group store does not account for every allocated id".to_string());
            }
            expected_local = expected_local
                .checked_add(1)
                .ok_or_else(|| "group local id range is exhausted".to_string())?;
        }
        if expected_local != next_group_id {
            return Err("group store does not account for every allocated id".to_string());
        }
        if revision != 0 && by_local.is_empty() {
            return Err("group store revision has no allocated identity".to_string());
        }
        Ok(Self {
            authority_id,
            revision,
            next_group_id,
            records: by_local,
        })
    }

    pub(crate) fn authority_id(&self) -> &AuthorityId {
        &self.authority_id
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn next_group_id(&self) -> u64 {
        self.next_group_id
    }

    pub(crate) fn records(&self) -> Vec<GroupRecord> {
        self.records.values().cloned().collect()
    }

    pub(crate) fn record(&self, id: &GroupId) -> Result<&GroupRecord, MutationError> {
        if id.owner != self.authority_id {
            return Err(MutationError::ForeignOwner);
        }
        self.records.get(&id.local).ok_or(MutationError::NotFound)
    }

    pub(crate) fn create(
        &self,
        name: &str,
        expected_revision: u64,
    ) -> Result<(Self, GroupRecord), MutationError> {
        if expected_revision != self.revision {
            return Err(MutationError::RevisionConflict {
                expected: expected_revision,
                actual: self.revision,
            });
        }
        let name = normalize_name(name).ok_or(MutationError::InvalidName)?;
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(MutationError::RevisionExhausted)?;
        let next_group_id = self
            .next_group_id
            .checked_add(1)
            .ok_or(MutationError::RevisionExhausted)?;
        let record = GroupRecord {
            id: GroupId {
                owner: self.authority_id.clone(),
                local: self.next_group_id,
            },
            revision,
            state: GroupState::Active { name },
        };
        let mut candidate = self.clone();
        candidate.revision = revision;
        candidate.next_group_id = next_group_id;
        candidate.records.insert(record.id.local, record.clone());
        Ok((candidate, record))
    }

    pub(crate) fn rename(
        &self,
        id: &GroupId,
        name: &str,
        expected_revision: u64,
    ) -> Result<(Self, GroupRecord), MutationError> {
        let current = self.record(id)?;
        if current.revision != expected_revision {
            return Err(MutationError::RevisionConflict {
                expected: expected_revision,
                actual: current.revision,
            });
        }
        if matches!(current.state, GroupState::Deleted) {
            return Err(MutationError::Deleted);
        }
        let name = normalize_name(name).ok_or(MutationError::InvalidName)?;
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(MutationError::RevisionExhausted)?;
        let record = GroupRecord {
            id: id.clone(),
            revision,
            state: GroupState::Active { name },
        };
        let mut candidate = self.clone();
        candidate.revision = revision;
        candidate.records.insert(id.local, record.clone());
        Ok((candidate, record))
    }

    pub(crate) fn delete(
        &self,
        id: &GroupId,
        expected_revision: u64,
    ) -> Result<(Self, GroupRecord), MutationError> {
        let current = self.record(id)?;
        if current.revision != expected_revision {
            return Err(MutationError::RevisionConflict {
                expected: expected_revision,
                actual: current.revision,
            });
        }
        if matches!(current.state, GroupState::Deleted) {
            return Err(MutationError::Deleted);
        }
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(MutationError::RevisionExhausted)?;
        let record = GroupRecord {
            id: id.clone(),
            revision,
            state: GroupState::Deleted,
        };
        let mut candidate = self.clone();
        candidate.revision = revision;
        candidate.records.insert(id.local, record.clone());
        Ok((candidate, record))
    }
}

fn normalize_name(name: &str) -> Option<String> {
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authority(seed: u8) -> AuthorityId {
        AuthorityId::from_random_bytes([seed; AUTHORITY_ID_BYTES])
    }

    #[test]
    fn same_name_on_distinct_authorities_produces_distinct_group_ids() {
        let (_, first) = AuthorityState::empty(authority(1))
            .create("Work", 0)
            .unwrap();
        let (_, second) = AuthorityState::empty(authority(2))
            .create("Work", 0)
            .unwrap();

        assert_ne!(first.id, second.id);
        assert_eq!(first.state, second.state);
    }

    #[test]
    fn authority_id_deserialization_rejects_noncanonical_text() {
        assert!(serde_json::from_str::<AuthorityId>(r#""not-an-authority""#).is_err());
    }

    #[test]
    fn rename_preserves_identity_and_uses_record_compare_and_set() {
        let (state, created) = AuthorityState::empty(authority(1))
            .create("Work", 0)
            .unwrap();
        let (renamed_state, renamed) = state.rename(&created.id, "Focus", 1).unwrap();

        assert_eq!(renamed.id, created.id);
        assert_eq!(renamed.revision, 2);
        assert!(matches!(
            renamed_state.rename(&created.id, "Later", 1),
            Err(MutationError::RevisionConflict {
                expected: 1,
                actual: 2
            })
        ));
    }

    #[test]
    fn tombstone_is_terminal() {
        let (state, created) = AuthorityState::empty(authority(1))
            .create("Work", 0)
            .unwrap();
        let (deleted_state, deleted) = state.delete(&created.id, 1).unwrap();

        assert!(matches!(deleted.state, GroupState::Deleted));
        assert_eq!(
            deleted_state.rename(&created.id, "Revived", deleted.revision),
            Err(MutationError::Deleted)
        );
        assert_eq!(
            deleted_state.delete(&created.id, deleted.revision),
            Err(MutationError::Deleted)
        );
    }

    #[test]
    fn two_creates_from_one_revision_have_one_winner() {
        let state = AuthorityState::empty(authority(1));
        let (winner, _) = state.create("One", 0).unwrap();

        assert!(matches!(
            winner.create("Two", 0),
            Err(MutationError::RevisionConflict {
                expected: 0,
                actual: 1
            })
        ));
    }

    #[test]
    fn persisted_state_rejects_an_unaccounted_allocated_identity() {
        let authority_id = authority(1);
        let record = GroupRecord {
            id: GroupId {
                owner: authority_id.clone(),
                local: 2,
            },
            revision: 2,
            state: GroupState::Deleted,
        };

        assert!(AuthorityState::from_persisted(authority_id, 2, 3, vec![record]).is_err());
    }

    #[test]
    fn persisted_state_rejects_revision_history_without_an_identity() {
        assert!(AuthorityState::from_persisted(authority(1), 2, 1, Vec::new()).is_err());
    }
}
