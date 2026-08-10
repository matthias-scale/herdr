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
        let history = crate::loop_runs::read_default_receipts();
        let selected_runs = crate::loop_runs::runs_for_loop(&history, params.loop_id.as_deref());
        let runs = selected_runs
            .iter()
            .map(crate::loop_runs::run_info)
            .collect::<Vec<_>>();
        let changed = self.state.loop_run_history != history;
        self.state.loop_run_history = history.clone();

        if let Some(loop_id) = params.loop_id.as_ref() {
            self.state.show_loop_run_history(
                loop_id.clone(),
                crate::loop_runs::RunHistory {
                    runs: selected_runs,
                    skipped_lines: history.skipped_lines,
                },
                std::time::SystemTime::now(),
            );
        } else {
            self.state.clear_loop_run_history();
        }

        if changed {
            self.emit_event(EventEnvelope {
                event: EventKind::LoopRunHistoryUpdated,
                data: EventData::LoopRunHistoryUpdated {
                    loop_id: params.loop_id.clone(),
                    runs: runs.clone(),
                    skipped_lines: history.skipped_lines,
                },
            });
        }

        encode_success(
            id,
            ResponseResult::LoopRunHistory {
                loop_id: params.loop_id,
                runs,
                skipped_lines: history.skipped_lines,
            },
        )
    }
}
