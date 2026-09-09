use crate::api::schema::{
    FleetAgentInfo, FleetHostInfo, FleetHostStateInfo, FleetSnapshotInfo, ResponseResult,
};
use crate::app::App;
use crate::fleet::{EvidenceSource, HostState};

use super::responses::encode_success;

impl App {
    pub(super) fn handle_fleet_list(&self, id: String) -> String {
        let snapshot = &self.state.fleet_snapshot;
        let hosts = snapshot
            .hosts
            .iter()
            .map(|host| {
                let agents = host
                    .entries
                    .iter()
                    .filter(|entry| entry.source != EvidenceSource::Host)
                    .map(|entry| FleetAgentInfo {
                        agent_ref: entry.agent_ref.clone(),
                        name: entry.name.clone().unwrap_or_else(|| entry.handle.clone()),
                        agent: entry.agent.clone(),
                        state: entry.state.clone(),
                        source: entry.source.table_label().to_string(),
                    })
                    .collect::<Vec<_>>();
                FleetHostInfo {
                    name: host.name.clone(),
                    target: host.target.clone(),
                    local: host.local,
                    session: host.session.clone(),
                    state: match host.state {
                        HostState::Reachable => FleetHostStateInfo::Reachable,
                        HostState::Unreachable => FleetHostStateInfo::Unreachable,
                        HostState::VersionSkew => FleetHostStateInfo::VersionSkew,
                    },
                    agent_count: agents.len(),
                    version: host.version.clone(),
                    protocol: host.protocol,
                    error: host.error.clone(),
                    agents,
                }
            })
            .collect();
        encode_success(
            id,
            ResponseResult::FleetList {
                snapshot: FleetSnapshotInfo {
                    polled: snapshot.polled,
                    refreshed_at_unix_ms: snapshot.refreshed_at_unix_ms,
                    hosts,
                },
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fleet_api_returns_cached_host_and_agent_inventory() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.fleet_snapshot = crate::fleet::Snapshot {
            polled: true,
            refreshed_at: None,
            refreshed_at_unix_ms: Some(42),
            configured_hosts: vec!["ub1".to_string(), "ub2".to_string()],
            hosts: ["ub1", "ub2"]
                .into_iter()
                .map(|host| crate::fleet::HostSnapshot {
                    name: host.to_string(),
                    target: host.to_string(),
                    local: false,
                    session: Some("agents".to_string()),
                    state: HostState::Reachable,
                    version: Some("0.8.2".to_string()),
                    protocol: Some(crate::protocol::PROTOCOL_VERSION),
                    error: None,
                    entries: vec![crate::fleet::FleetRow::test_agent_row_with_id(
                        host, "reviewer", "w1:p1",
                    )],
                })
                .collect(),
        };

        let value: serde_json::Value =
            serde_json::from_str(&app.handle_fleet_list("x".into())).expect("fleet response");
        let snapshot = &value["result"]["snapshot"];
        assert_eq!(snapshot["refreshed_at_unix_ms"], 42);
        assert_eq!(snapshot["hosts"][0]["agent_count"], 1);
        assert_eq!(snapshot["hosts"][0]["agents"][0]["agent_ref"], "ub1::w1:p1");
        assert_eq!(snapshot["hosts"][1]["agents"][0]["agent_ref"], "ub2::w1:p1");
        assert_ne!(
            snapshot["hosts"][0]["agents"][0]["agent_ref"],
            snapshot["hosts"][1]["agents"][0]["agent_ref"]
        );
    }
}
