use crate::api::schema::{
    EventData, EventEnvelope, EventKind, LoopRunHistoryParams, ResponseResult,
};
use crate::app::App;

use super::responses::encode_success;

impl App {
    pub(super) fn handle_loop_list(&mut self, id: String) -> String {
        let registry = crate::loop_runs::read_default_registry();
        self.state.loop_registry = registry.clone();
        encode_success(
            id,
            ResponseResult::LoopList {
                loops: registry
                    .loops
                    .iter()
                    .map(crate::loop_runs::loop_info)
                    .collect(),
            },
        )
    }

    pub(super) fn handle_loop_run_history(
        &mut self,
        id: String,
        params: LoopRunHistoryParams,
    ) -> String {
        let history = &self.state.loop_run_history;
        let selected_runs = crate::loop_runs::runs_for_loop(history, params.loop_id.as_deref());
        let runs = selected_runs
            .iter()
            .map(crate::loop_runs::run_info)
            .collect::<Vec<_>>();

        encode_success(
            id,
            ResponseResult::LoopRunHistory {
                loop_id: params.loop_id,
                runs,
                skipped_lines: history.skipped_lines,
            },
        )
    }

    /// Pending aloop findings from the producer host (MAT-159). Read-only:
    /// answers from the latest fleet poll and never starts an agent.
    pub(super) fn handle_loop_findings(&mut self, id: String) -> String {
        let projection = crate::aloop::project(&self.state.fleet_snapshot);
        let (host, reachable, findings) = match projection {
            Some(projection) => (
                projection.host,
                projection.reachable,
                projection
                    .findings
                    .iter()
                    .map(|finding| crate::api::schema::LoopFindingInfo {
                        loop_name: finding.loop_name.clone(),
                        source: finding.source.clone(),
                        stable_id: finding.stable_id.clone(),
                        title: finding.title.clone(),
                        url: finding.url.clone(),
                        evidence: finding.evidence.clone(),
                        prompt: finding.prompt.clone(),
                        created_at: finding.created_at.clone(),
                    })
                    .collect(),
            ),
            None => (self.fleet_poller_config.aloop_host(), false, Vec::new()),
        };

        encode_success(
            id,
            ResponseResult::LoopFindings {
                host,
                reachable,
                findings,
            },
        )
    }

    pub(crate) fn refresh_loop_run_history(&mut self) -> bool {
        let Some(reader) = self.loop_history_reader.as_mut() else {
            return false;
        };
        if !reader.refresh() {
            return false;
        }
        if self.state.loop_run_history == *reader.history() {
            return false;
        }
        self.state.loop_run_history = reader.history().clone();
        let history = &self.state.loop_run_history;
        if let Some(detail) = self.state.loop_run_history_detail.as_mut() {
            detail.history = crate::loop_runs::RunHistory {
                runs: crate::loop_runs::runs_for_loop(
                    history,
                    (detail.loop_id != crate::loop_runs::ALL_LOOPS_ID)
                        .then_some(detail.loop_id.as_str()),
                ),
                skipped_lines: history.skipped_lines,
            };
            detail.observed_at = std::time::SystemTime::now();
        }
        self.emit_event(EventEnvelope {
            event: EventKind::LoopRunHistoryUpdated,
            data: EventData::LoopRunHistoryUpdated {
                loop_id: None,
                runs: history
                    .runs
                    .iter()
                    .map(crate::loop_runs::run_info)
                    .collect(),
                skipped_lines: history.skipped_lines,
            },
        });
        true
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn loop_findings_reports_pending_findings_from_the_producer_snapshot() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.fleet_snapshot = crate::fleet::Snapshot {
            polled: true,
            aloop: Some(crate::aloop::ProducerSnapshot::read(
                "ub2".to_string(),
                crate::aloop::HostData {
                    findings: vec![
                        std::sync::Arc::new(crate::aloop::Finding {
                            loop_name: "nightly".to_string(),
                            source: "sentry".to_string(),
                            stable_id: "abc-123".to_string(),
                            title: "worker crashed".to_string(),
                            url: None,
                            evidence: "stacktrace".to_string(),
                            prompt: "fix it".to_string(),
                            created_at: "2026-09-18T09:50:00Z".to_string(),
                            created_at_unix_s: crate::fleet::parse_utc_timestamp(
                                "2026-09-18T09:50:00Z",
                            )
                            .expect("timestamp"),
                            status: crate::aloop::FindingStatus::Pending,
                        }),
                        std::sync::Arc::new(crate::aloop::Finding {
                            loop_name: "nightly".to_string(),
                            source: "sentry".to_string(),
                            stable_id: "done-1".to_string(),
                            title: "already launched".to_string(),
                            url: None,
                            evidence: String::new(),
                            prompt: "fix it".to_string(),
                            created_at: "2026-09-18T09:51:00Z".to_string(),
                            created_at_unix_s: crate::fleet::parse_utc_timestamp(
                                "2026-09-18T09:51:00Z",
                            )
                            .expect("timestamp"),
                            status: crate::aloop::FindingStatus::Launched,
                        }),
                    ],
                    ..Default::default()
                },
            )),
            ..Default::default()
        };

        let response = app.dispatch_api_request(
            "read",
            crate::api::schema::Method::LoopFindings(crate::api::schema::EmptyParams {}),
        );
        let parsed: serde_json::Value = serde_json::from_str(&response).expect("json response");

        assert_eq!(parsed["result"]["type"], "loop_findings");
        assert_eq!(parsed["result"]["host"], "ub2");
        assert_eq!(parsed["result"]["reachable"], true);
        let findings = parsed["result"]["findings"].as_array().expect("findings");
        // Only the pending finding is listed (AC2).
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["stable_id"], "abc-123");
        assert_eq!(findings[0]["loop"], "nightly");
        assert_eq!(findings[0]["title"], "worker crashed");
    }

    #[test]
    fn loop_findings_without_a_poll_reports_unreachable_and_empty() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );

        let response = app.dispatch_api_request(
            "read",
            crate::api::schema::Method::LoopFindings(crate::api::schema::EmptyParams {}),
        );
        let parsed: serde_json::Value = serde_json::from_str(&response).expect("json response");

        assert_eq!(parsed["result"]["type"], "loop_findings");
        assert_eq!(parsed["result"]["host"], "ub2");
        assert_eq!(parsed["result"]["reachable"], false);
        assert_eq!(parsed["result"]["findings"], serde_json::json!([]));
    }
}
