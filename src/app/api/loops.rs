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
        let history = self.state.loop_run_history.clone();
        let selected_runs = crate::loop_runs::runs_for_loop(&history, params.loop_id.as_deref());
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

    pub(super) fn refresh_loop_run_history(&mut self) -> bool {
        let Some(reader) = self.loop_history_reader.as_mut() else {
            return false;
        };
        if !reader.refresh() {
            return false;
        }
        let history = reader.history().clone();
        if self.state.loop_run_history == history {
            return false;
        }
        self.state.loop_run_history = history.clone();
        if let Some(detail) = self.state.loop_run_history_detail.as_mut() {
            detail.history = crate::loop_runs::RunHistory {
                runs: crate::loop_runs::runs_for_loop(
                    &history,
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
