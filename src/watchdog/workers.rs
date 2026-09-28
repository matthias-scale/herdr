use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkerObservation {
    pub source: String,
    pub worker_id: String,
    pub parent_session: String,
    pub last_activity: u64,
    pub finished: bool,
}

impl WorkerObservation {
    pub(crate) fn key(&self) -> String {
        format!("{}:{}", self.source, self.worker_id)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct WorkerMemory {
    notified_activity: HashMap<String, u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StallAction {
    WouldNotify,
    Notified,
    AlreadyNotified,
    NotifyFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct WorkerStallDecision {
    pub worker_id: String,
    pub parent_session: String,
    pub age_secs: u64,
    pub action: StallAction,
    pub error: Option<String>,
}

/// Notify once for each distinct stale activity timestamp. Never terminates a worker.
pub(crate) fn process_stalls<N, L>(
    observations: &[WorkerObservation],
    memory: &mut WorkerMemory,
    now: u64,
    stall_after_secs: u64,
    dry_run: bool,
    mut notify_parent: N,
    mut log_stall: L,
) -> Vec<WorkerStallDecision>
where
    N: FnMut(&WorkerObservation, u64) -> std::io::Result<()>,
    L: FnMut(&WorkerObservation, u64) -> std::io::Result<()>,
{
    let live_keys = observations
        .iter()
        .filter(|worker| !worker.finished)
        .map(WorkerObservation::key)
        .collect::<HashSet<_>>();
    memory
        .notified_activity
        .retain(|worker_key, _| live_keys.contains(worker_key));

    let mut decisions = Vec::new();
    for worker in observations {
        if worker.finished || worker.last_activity == 0 {
            continue;
        }
        let age_secs = now.saturating_sub(worker.last_activity);
        if age_secs < stall_after_secs {
            continue;
        }
        let key = worker.key();
        if memory.notified_activity.get(&key) == Some(&worker.last_activity) {
            decisions.push(WorkerStallDecision {
                worker_id: worker.worker_id.clone(),
                parent_session: worker.parent_session.clone(),
                age_secs,
                action: StallAction::AlreadyNotified,
                error: None,
            });
            continue;
        }
        if dry_run {
            decisions.push(WorkerStallDecision {
                worker_id: worker.worker_id.clone(),
                parent_session: worker.parent_session.clone(),
                age_secs,
                action: StallAction::WouldNotify,
                error: None,
            });
            continue;
        }

        match notify_parent(worker, age_secs) {
            Ok(()) => {
                memory.notified_activity.insert(key, worker.last_activity);
                let error = log_stall(worker, age_secs)
                    .err()
                    .map(|error| format!("notification sent; stall log failed: {error}"));
                decisions.push(WorkerStallDecision {
                    worker_id: worker.worker_id.clone(),
                    parent_session: worker.parent_session.clone(),
                    age_secs,
                    action: StallAction::Notified,
                    error,
                });
            }
            Err(error) => decisions.push(WorkerStallDecision {
                worker_id: worker.worker_id.clone(),
                parent_session: worker.parent_session.clone(),
                age_secs,
                action: StallAction::NotifyFailed,
                error: Some(error.to_string()),
            }),
        }
    }
    decisions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker(last_activity: u64, finished: bool) -> WorkerObservation {
        WorkerObservation {
            source: "codex".into(),
            worker_id: "run-1".into(),
            parent_session: "parent-1".into(),
            last_activity,
            finished,
        }
    }

    #[test]
    fn recent_and_finished_workers_are_not_notified() {
        let observations = [worker(950, false), worker(100, true)];
        let mut memory = WorkerMemory::default();
        let mut notifications = 0;
        let decisions = process_stalls(
            &observations,
            &mut memory,
            1_000,
            300,
            false,
            |_, _| {
                notifications += 1;
                Ok(())
            },
            |_, _| Ok(()),
        );
        assert!(decisions.is_empty());
        assert_eq!(notifications, 0);
    }

    #[test]
    fn worker_is_notified_once_per_stalled_activity_timestamp() {
        let mut memory = WorkerMemory::default();
        let stale = worker(100, false);
        let mut notifications = 0;
        let mut logs = 0;
        let first = process_stalls(
            std::slice::from_ref(&stale),
            &mut memory,
            500,
            300,
            false,
            |_, _| {
                notifications += 1;
                Ok(())
            },
            |_, _| {
                logs += 1;
                Ok(())
            },
        );
        let second = process_stalls(
            std::slice::from_ref(&stale),
            &mut memory,
            700,
            300,
            false,
            |_, _| {
                notifications += 1;
                Ok(())
            },
            |_, _| {
                logs += 1;
                Ok(())
            },
        );
        assert_eq!(first[0].action, StallAction::Notified);
        assert_eq!(second[0].action, StallAction::AlreadyNotified);
        assert_eq!(notifications, 1);
        assert_eq!(logs, 1);

        let resumed = worker(600, false);
        let after_new_stall = process_stalls(
            &[resumed],
            &mut memory,
            1_000,
            300,
            false,
            |_, _| {
                notifications += 1;
                Ok(())
            },
            |_, _| Ok(()),
        );
        assert_eq!(after_new_stall[0].action, StallAction::Notified);
        assert_eq!(notifications, 2);
    }

    #[test]
    fn dry_run_reports_stalls_without_notifying_logging_or_deduplicating() {
        let stale = worker(100, false);
        let mut memory = WorkerMemory::default();
        let mut notifications = 0;
        let mut logs = 0;
        let decisions = process_stalls(
            &[stale],
            &mut memory,
            500,
            300,
            true,
            |_, _| {
                notifications += 1;
                Ok(())
            },
            |_, _| {
                logs += 1;
                Ok(())
            },
        );
        assert_eq!(decisions[0].action, StallAction::WouldNotify);
        assert_eq!(notifications, 0);
        assert_eq!(logs, 0);
        assert!(memory.notified_activity.is_empty());
    }
}
