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
pub(crate) enum SnapshotAdmissionError {
    AuthorityMismatch,
    InvalidRecord {
        local: u64,
    },
    DuplicateRecord {
        local: u64,
    },
    DuplicateMembership {
        pane_id: String,
    },
    SnapshotRollback {
        retained_revision: u64,
        incoming_revision: u64,
    },
    SnapshotConflict {
        revision: u64,
    },
    MissingRecord {
        local: u64,
    },
    MissingObservedTombstone {
        local: u64,
    },
    RecordRollback {
        local: u64,
        retained_revision: u64,
        incoming_revision: u64,
    },
    RecordConflict {
        local: u64,
        revision: u64,
    },
    TombstoneRevival {
        local: u64,
    },
}

impl fmt::Display for SnapshotAdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthorityMismatch => f.write_str("snapshot contains a foreign group record"),
            Self::InvalidRecord { local } => write!(f, "group {local} has an invalid revision"),
            Self::DuplicateRecord { local } => {
                write!(f, "snapshot contains duplicate group {local}")
            }
            Self::DuplicateMembership { pane_id } => {
                write!(f, "snapshot contains duplicate pane membership {pane_id}")
            }
            Self::SnapshotRollback {
                retained_revision,
                incoming_revision,
            } => write!(
                f,
                "snapshot revision rolled back from {retained_revision} to {incoming_revision}"
            ),
            Self::SnapshotConflict { revision } => {
                write!(f, "snapshot conflicts at revision {revision}")
            }
            Self::MissingRecord { local } => write!(f, "snapshot lost observed group {local}"),
            Self::MissingObservedTombstone { local } => {
                write!(f, "snapshot lost observed tombstone {local}")
            }
            Self::RecordRollback {
                local,
                retained_revision,
                incoming_revision,
            } => write!(
                f,
                "group {local} revision rolled back from {retained_revision} to {incoming_revision}"
            ),
            Self::RecordConflict { local, revision } => {
                write!(f, "group {local} conflicts at revision {revision}")
            }
            Self::TombstoneRevival { local } => {
                write!(f, "group {local} attempts to replace an observed tombstone")
            }
        }
    }
}

/// Validate a complete owner snapshot against the last snapshot accepted from
/// that authority. Record revisions establish identity continuity; the
/// authority revision is only an additional rollback check.
pub(crate) fn admit_authority_snapshot(
    retained: Option<&GroupAuthoritySnapshot>,
    incoming: &GroupAuthoritySnapshot,
) -> Result<(), SnapshotAdmissionError> {
    let mut incoming_by_local = BTreeMap::new();
    for record in &incoming.groups {
        if record.id.owner != incoming.authority_id {
            return Err(SnapshotAdmissionError::AuthorityMismatch);
        }
        if record.id.local == 0 || record.revision == 0 || record.revision > incoming.revision {
            return Err(SnapshotAdmissionError::InvalidRecord {
                local: record.id.local,
            });
        }
        if incoming_by_local.insert(record.id.local, record).is_some() {
            return Err(SnapshotAdmissionError::DuplicateRecord {
                local: record.id.local,
            });
        }
    }
    let mut incoming_memberships = BTreeMap::new();
    for membership in &incoming.memberships {
        if incoming_memberships
            .insert(membership.pane_id.as_str(), membership)
            .is_some()
        {
            return Err(SnapshotAdmissionError::DuplicateMembership {
                pane_id: membership.pane_id.clone(),
            });
        }
    }

    let Some(retained) = retained else {
        return Ok(());
    };
    if retained.authority_id != incoming.authority_id {
        return Err(SnapshotAdmissionError::AuthorityMismatch);
    }
    if incoming.revision < retained.revision {
        return Err(SnapshotAdmissionError::SnapshotRollback {
            retained_revision: retained.revision,
            incoming_revision: incoming.revision,
        });
    }

    for previous in &retained.groups {
        let Some(next) = incoming_by_local.get(&previous.id.local).copied() else {
            return Err(if matches!(previous.state, GroupState::Deleted) {
                SnapshotAdmissionError::MissingObservedTombstone {
                    local: previous.id.local,
                }
            } else {
                SnapshotAdmissionError::MissingRecord {
                    local: previous.id.local,
                }
            });
        };
        if next.revision < previous.revision {
            return Err(SnapshotAdmissionError::RecordRollback {
                local: previous.id.local,
                retained_revision: previous.revision,
                incoming_revision: next.revision,
            });
        }
        if next.revision == previous.revision && next != previous {
            return Err(SnapshotAdmissionError::RecordConflict {
                local: previous.id.local,
                revision: next.revision,
            });
        }
        if matches!(previous.state, GroupState::Deleted) && next != previous {
            return Err(SnapshotAdmissionError::TombstoneRevival {
                local: previous.id.local,
            });
        }
    }
    if incoming.revision == retained.revision
        && (incoming_by_local.len() != retained.groups.len()
            || retained.groups.iter().any(|previous| {
                incoming_by_local.get(&previous.id.local).copied() != Some(previous)
            }))
    {
        return Err(SnapshotAdmissionError::SnapshotConflict {
            revision: incoming.revision,
        });
    }
    Ok(())
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

    fn snapshot(revision: u64, records: Vec<GroupRecord>) -> GroupAuthoritySnapshot {
        GroupAuthoritySnapshot {
            authority_id: authority(1),
            revision,
            groups: records,
            memberships: Vec::new(),
        }
    }

    fn record(local: u64, revision: u64, state: GroupState) -> GroupRecord {
        GroupRecord {
            id: GroupId {
                owner: authority(1),
                local,
            },
            revision,
            state,
        }
    }

    fn membership(pane_id: &str, revision: u64, group_id: Option<GroupId>) -> OwnedPaneMembership {
        OwnedPaneMembership {
            pane_id: pane_id.into(),
            membership: PaneGroupMembership { group_id, revision },
        }
    }

    #[test]
    fn admission_rejects_a_newer_snapshot_that_lost_an_observed_tombstone() {
        let retained = snapshot(2, vec![record(1, 2, GroupState::Deleted)]);
        let incoming = snapshot(3, Vec::new());

        assert_eq!(
            admit_authority_snapshot(Some(&retained), &incoming),
            Err(SnapshotAdmissionError::MissingObservedTombstone { local: 1 })
        );
    }

    #[test]
    fn admission_uses_record_revisions_and_rejects_equal_revision_conflicts() {
        let retained = snapshot(
            4,
            vec![record(
                1,
                3,
                GroupState::Active {
                    name: "Work".into(),
                },
            )],
        );
        let incoming = snapshot(
            4,
            vec![record(
                1,
                3,
                GroupState::Active {
                    name: "Focus".into(),
                },
            )],
        );

        assert_eq!(
            admit_authority_snapshot(Some(&retained), &incoming),
            Err(SnapshotAdmissionError::RecordConflict {
                local: 1,
                revision: 3,
            })
        );
    }

    #[test]
    fn admission_rejects_per_record_rollback_even_when_authority_revision_advances() {
        let retained = snapshot(
            4,
            vec![record(
                1,
                4,
                GroupState::Active {
                    name: "Focus".into(),
                },
            )],
        );
        let incoming = snapshot(
            5,
            vec![record(
                1,
                3,
                GroupState::Active {
                    name: "Work".into(),
                },
            )],
        );

        assert_eq!(
            admit_authority_snapshot(Some(&retained), &incoming),
            Err(SnapshotAdmissionError::RecordRollback {
                local: 1,
                retained_revision: 4,
                incoming_revision: 3,
            })
        );
    }

    #[test]
    fn admission_accepts_an_identical_snapshot_idempotently() {
        let retained = snapshot(
            2,
            vec![record(
                1,
                2,
                GroupState::Active {
                    name: "Work".into(),
                },
            )],
        );

        assert_eq!(admit_authority_snapshot(Some(&retained), &retained), Ok(()));
    }

    #[test]
    fn admission_rejects_new_record_content_at_the_same_snapshot_revision() {
        let retained = snapshot(
            2,
            vec![record(
                1,
                1,
                GroupState::Active {
                    name: "Work".into(),
                },
            )],
        );
        let incoming = snapshot(
            2,
            vec![
                retained.groups[0].clone(),
                record(
                    2,
                    2,
                    GroupState::Active {
                        name: "Focus".into(),
                    },
                ),
            ],
        );

        assert_eq!(
            admit_authority_snapshot(Some(&retained), &incoming),
            Err(SnapshotAdmissionError::SnapshotConflict { revision: 2 })
        );
    }

    #[test]
    fn admission_reports_snapshot_rollback_before_missing_tombstones() {
        let retained = snapshot(3, vec![record(1, 2, GroupState::Deleted)]);
        let incoming = snapshot(1, Vec::new());

        assert_eq!(
            admit_authority_snapshot(Some(&retained), &incoming),
            Err(SnapshotAdmissionError::SnapshotRollback {
                retained_revision: 3,
                incoming_revision: 1,
            })
        );
    }

    #[test]
    fn admission_rejects_duplicate_pane_memberships() {
        let mut incoming = snapshot(0, Vec::new());
        incoming.memberships = vec![membership("w1:p1", 0, None), membership("w1:p1", 0, None)];

        assert_eq!(
            admit_authority_snapshot(None, &incoming),
            Err(SnapshotAdmissionError::DuplicateMembership {
                pane_id: "w1:p1".into(),
            })
        );
    }

    #[test]
    fn admission_treats_reused_pane_addresses_as_new_membership_observations() {
        let group_a = GroupId {
            owner: authority(1),
            local: 1,
        };
        let group_b = GroupId {
            owner: authority(1),
            local: 2,
        };
        let mut retained = snapshot(2, Vec::new());
        retained.memberships = vec![membership("w1:p1", 5, Some(group_a.clone()))];
        let mut rollback = retained.clone();
        rollback.memberships = vec![membership("w1:p1", 4, Some(group_a))];
        let mut conflict = retained.clone();
        conflict.memberships = vec![membership("w1:p1", 5, Some(group_b))];

        assert_eq!(admit_authority_snapshot(Some(&retained), &rollback), Ok(()));
        assert_eq!(admit_authority_snapshot(Some(&retained), &conflict), Ok(()));
    }

    #[test]
    fn admission_accepts_membership_advance_without_authority_revision_change() {
        let mut retained = snapshot(2, Vec::new());
        retained.memberships = vec![membership("w1:p1", 5, None)];
        let mut incoming = retained.clone();
        incoming.memberships = vec![membership("w1:p1", 6, None)];
        incoming.memberships.push(membership("w1:p2", 1, None));

        assert_eq!(admit_authority_snapshot(Some(&retained), &incoming), Ok(()));

        incoming.memberships.remove(0);
        assert_eq!(admit_authority_snapshot(Some(&retained), &incoming), Ok(()));
    }
}
