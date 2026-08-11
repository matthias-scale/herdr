use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LoopRunHistoryParams {
    /// Optional during the receipt-schema migration. When omitted, the API
    /// returns every observed run; when present, records with loop identity
    /// are filtered to this id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LoopInfo {
    pub loop_id: String,
    pub title: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_runs: Vec<LoopRecentRun>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LoopRecentRun {
    pub run_id: String,
    pub stable_id: Option<String>,
    pub outcome: String,
    pub epoch: Option<u64>,
    pub at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LoopGateInfo {
    pub kind: String,
    pub defaulted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommendation_matched: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LoopRunInfo {
    pub run_id: String,
    pub skill: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_id: Option<String>,
    pub start: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_min: Option<f64>,
    pub gates: Vec<LoopGateInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_touches: Option<u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub touches_by_type: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted_focus: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_rounds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out_tokens: Option<u64>,
    pub outcome: String,
}
