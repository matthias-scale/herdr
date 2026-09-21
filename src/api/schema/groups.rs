use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GroupCreateParams {
    pub name: String,
    pub expected_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GroupRenameParams {
    pub group_id: crate::groups::GroupId,
    pub name: String,
    pub expected_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GroupDeleteParams {
    pub group_id: crate::groups::GroupId,
    pub expected_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneGroupSetParams {
    pub pane_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<crate::groups::GroupId>,
    pub expected_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AuthorityMutationParams {
    pub expected_authority: crate::groups::AuthorityId,
    /// Receiver-only marker. The receiving server rejects false and never
    /// forwards this method, which bounds routing to one hop.
    pub forwarded: bool,
    #[serde(flatten)]
    pub mutation: AuthorityMutation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum AuthorityMutation {
    Rename(GroupRenameParams),
    Delete(GroupDeleteParams),
    PaneGroupSet(PaneGroupSetParams),
}
