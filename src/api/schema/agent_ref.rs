use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

const AGENT_REF_SEPARATOR: &str = "::";

/// Cross-host identity for a pane-backed agent or windowless run.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, schemars::JsonSchema)]
#[schemars(with = "String")]
pub struct AgentRef {
    pub host: String,
    pub agent: String,
}

impl AgentRef {
    pub fn new(
        host: impl Into<String>,
        agent: impl Into<String>,
    ) -> Result<Self, ParseAgentRefError> {
        let host = host.into();
        let agent = agent.into();
        if host.is_empty() || host.contains(AGENT_REF_SEPARATOR) || agent.is_empty() {
            return Err(ParseAgentRefError);
        }
        Ok(Self { host, agent })
    }
}

impl fmt::Display for AgentRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}{}{}",
            self.host, AGENT_REF_SEPARATOR, self.agent
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseAgentRefError;

impl fmt::Display for ParseAgentRefError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("agent reference must be host::agent with both parts non-empty")
    }
}

impl std::error::Error for ParseAgentRefError {}

impl FromStr for AgentRef {
    type Err = ParseAgentRefError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (host, agent) = value
            .split_once(AGENT_REF_SEPARATOR)
            .filter(|(host, agent)| !host.is_empty() && !agent.is_empty())
            .ok_or(ParseAgentRefError)?;
        Self::new(host, agent)
    }
}

impl Serialize for AgentRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for AgentRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_parse_and_json_round_trip_paths_with_slashes() {
        let agent_ref =
            AgentRef::new("ssh/user@workbox", "workspace/reviewer").expect("valid agent reference");

        let displayed = agent_ref.to_string();
        assert_eq!(displayed, "ssh/user@workbox::workspace/reviewer");
        assert_eq!(displayed.parse::<AgentRef>(), Ok(agent_ref.clone()));

        let json = serde_json::to_string(&agent_ref).expect("serialize agent ref");
        assert_eq!(json, r#""ssh/user@workbox::workspace/reviewer""#);
        assert_eq!(
            serde_json::from_str::<AgentRef>(&json).expect("deserialize agent ref"),
            agent_ref
        );
    }

    #[test]
    fn parse_rejects_missing_or_empty_components() {
        for invalid in ["agent", "::agent", "host::"] {
            assert!(invalid.parse::<AgentRef>().is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn construction_rejects_values_that_cannot_round_trip() {
        for (host, agent) in [("a::b", "id"), ("", "id"), ("host", "")] {
            assert!(
                AgentRef::new(host, agent).is_err(),
                "accepted {host:?} {agent:?}"
            );
        }
    }

    #[test]
    fn construction_allows_slashes_and_separators_in_agent_ids() {
        for (host, agent) in [
            ("ssh/user@workbox", "workspace/reviewer"),
            ("host", "run::child"),
        ] {
            let agent_ref = AgentRef::new(host, agent).expect("valid agent reference");
            assert_eq!(agent_ref.to_string().parse::<AgentRef>(), Ok(agent_ref));
        }
    }
}
