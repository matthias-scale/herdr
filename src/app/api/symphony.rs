use crate::api::schema::{ResponseResult, SymphonyWorkflowInfo};
use crate::app::App;

use super::responses::encode_success;

impl App {
    pub(super) fn handle_symphony_list(&self, id: String) -> String {
        let snapshot = &self.state.symphony_snapshot;
        encode_success(
            id,
            ResponseResult::SymphonyList {
                workflows: snapshot
                    .workflows
                    .iter()
                    .map(|workflow| SymphonyWorkflowInfo {
                        workflow_id: workflow.workflow_id.clone(),
                        run_id: workflow.run_id.clone(),
                        name: workflow.name.clone(),
                        phase: workflow.phase.clone(),
                        wait: workflow.wait.clone(),
                        started_at: workflow.started_at.clone(),
                        ticket: workflow.ticket.clone(),
                        repo: workflow.repo.clone(),
                        pr: workflow.pr.clone(),
                        receipts: workflow.receipts.clone(),
                    })
                    .collect(),
                unavailable: snapshot.unavailable.clone(),
            },
        )
    }
}
