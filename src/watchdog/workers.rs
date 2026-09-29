use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkerObservation {
    pub source: String,
    pub worker_id: String,
    pub parent_session: String,
    pub last_activity: u64,
    pub finished: bool,
    pub parent_scope_local: bool,
    pub parent_state: ParentState,
}

impl WorkerObservation {
    pub(crate) fn key(&self) -> String {
        format!("{}:{}", self.source, self.worker_id)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ParentState {
    Present,
    Absent,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct WorkerMemory {
    #[serde(default)]
    recorded_activity: HashMap<String, u64>,
    #[serde(default)]
    notified_activity: HashMap<String, u64>,
    #[serde(default)]
    parent_absence: HashMap<String, ParentAbsence>,
    #[serde(default)]
    orphaned_notified: HashSet<String>,
    #[serde(default)]
    orphaned_logged: HashSet<String>,
    #[serde(default)]
    stale_candidates: HashMap<String, StaleCandidate>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct ParentAbsence {
    first_seen: u64,
    last_seen: u64,
    samples: u8,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct StaleCandidate {
    activity: u64,
    first_seen: u64,
    last_seen: u64,
    samples: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StallAction {
    ParentChecking,
    ParentUnknown,
    Orphaned,
    Confirming,
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
    pub parent_state: ParentState,
}

/// Persist one incident per stale progress episode, then retry delivery until it succeeds.
/// This path only notifies; it never types into, resumes, or terminates a worker.
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
    memory
        .recorded_activity
        .retain(|worker_key, _| live_keys.contains(worker_key));
    memory
        .parent_absence
        .retain(|key, _| live_keys.contains(key));
    memory
        .orphaned_notified
        .retain(|key| live_keys.contains(key));
    memory.orphaned_logged.retain(|key| live_keys.contains(key));
    memory
        .stale_candidates
        .retain(|key, _| live_keys.contains(key));

    let mut decisions = Vec::new();
    for worker in observations {
        if worker.finished {
            continue;
        }
        let key = worker.key();
        match worker.parent_state {
            ParentState::Unknown => {
                memory.parent_absence.remove(&key);
                memory.orphaned_notified.remove(&key);
                memory.orphaned_logged.remove(&key);
                decisions.push(WorkerStallDecision {
                    worker_id: worker.worker_id.clone(),
                    parent_session: worker.parent_session.clone(),
                    age_secs: 0,
                    action: StallAction::ParentUnknown,
                    error: None,
                    parent_state: ParentState::Unknown,
                });
                continue;
            }
            ParentState::Absent => {
                let absence = memory
                    .parent_absence
                    .entry(key.clone())
                    .or_insert(ParentAbsence {
                        first_seen: now,
                        last_seen: now,
                        samples: 0,
                    });
                if absence.samples == 0 {
                    absence.samples = 1;
                    absence.last_seen = now;
                    decisions.push(parent_decision(worker, 0, StallAction::ParentChecking));
                    continue;
                }
                absence.samples = 2;
                absence.last_seen = now;
                if memory.orphaned_notified.contains(&key) {
                    decisions.push(parent_decision(
                        worker,
                        now.saturating_sub(absence.first_seen),
                        StallAction::Orphaned,
                    ));
                    continue;
                }
                if dry_run {
                    decisions.push(parent_decision(
                        worker,
                        now.saturating_sub(absence.first_seen),
                        StallAction::Orphaned,
                    ));
                    continue;
                }
                let age = now.saturating_sub(absence.first_seen);
                if !memory.orphaned_logged.contains(&key) {
                    if let Err(error) = log_stall(worker, age) {
                        decisions.push(WorkerStallDecision {
                            worker_id: worker.worker_id.clone(),
                            parent_session: worker.parent_session.clone(),
                            age_secs: age,
                            action: StallAction::NotifyFailed,
                            error: Some(format!("orphan incident log failed: {error}")),
                            parent_state: ParentState::Absent,
                        });
                        continue;
                    }
                    memory.orphaned_logged.insert(key.clone());
                }
                match notify_parent(worker, age) {
                    Ok(()) => {
                        memory.orphaned_notified.insert(key);
                        decisions.push(parent_decision(worker, age, StallAction::Orphaned));
                    }
                    Err(error) => decisions.push(WorkerStallDecision {
                        worker_id: worker.worker_id.clone(),
                        parent_session: worker.parent_session.clone(),
                        age_secs: age,
                        action: StallAction::NotifyFailed,
                        error: Some(error.to_string()),
                        parent_state: ParentState::Absent,
                    }),
                }
                continue;
            }
            ParentState::Present => {
                memory.parent_absence.remove(&key);
                memory.orphaned_notified.remove(&key);
                memory.orphaned_logged.remove(&key);
            }
        }
        if worker.last_activity == 0 {
            continue;
        }
        let age_secs = now.saturating_sub(worker.last_activity);
        if age_secs < stall_after_secs {
            memory.stale_candidates.remove(&key);
            continue;
        }
        let candidate = memory.stale_candidates.entry(key.clone()).or_default();
        if candidate.activity != worker.last_activity {
            *candidate = StaleCandidate {
                activity: worker.last_activity,
                first_seen: now,
                last_seen: now,
                samples: 1,
            };
            decisions.push(parent_decision(worker, age_secs, StallAction::Confirming));
            continue;
        }
        if candidate.samples < 2 {
            candidate.samples = 2;
            candidate.last_seen = now;
        }
        if dry_run {
            decisions.push(WorkerStallDecision {
                worker_id: worker.worker_id.clone(),
                parent_session: worker.parent_session.clone(),
                age_secs,
                action: StallAction::WouldNotify,
                error: None,
                parent_state: worker.parent_state,
            });
            continue;
        }

        if memory.recorded_activity.get(&key) != Some(&worker.last_activity) {
            if let Err(error) = log_stall(worker, age_secs) {
                decisions.push(WorkerStallDecision {
                    worker_id: worker.worker_id.clone(),
                    parent_session: worker.parent_session.clone(),
                    age_secs,
                    action: StallAction::NotifyFailed,
                    error: Some(format!("incident log failed: {error}")),
                    parent_state: worker.parent_state,
                });
                continue;
            }
            memory
                .recorded_activity
                .insert(key.clone(), worker.last_activity);
        }

        if memory.notified_activity.get(&key) == Some(&worker.last_activity) {
            decisions.push(WorkerStallDecision {
                worker_id: worker.worker_id.clone(),
                parent_session: worker.parent_session.clone(),
                age_secs,
                action: StallAction::AlreadyNotified,
                error: None,
                parent_state: worker.parent_state,
            });
            continue;
        }

        match notify_parent(worker, age_secs) {
            Ok(()) => {
                memory.notified_activity.insert(key, worker.last_activity);
                decisions.push(WorkerStallDecision {
                    worker_id: worker.worker_id.clone(),
                    parent_session: worker.parent_session.clone(),
                    age_secs,
                    action: StallAction::Notified,
                    error: None,
                    parent_state: worker.parent_state,
                });
            }
            Err(error) => decisions.push(WorkerStallDecision {
                worker_id: worker.worker_id.clone(),
                parent_session: worker.parent_session.clone(),
                age_secs,
                action: StallAction::NotifyFailed,
                error: Some(error.to_string()),
                parent_state: worker.parent_state,
            }),
        }
    }
    decisions
}

fn parent_decision(
    worker: &WorkerObservation,
    age_secs: u64,
    action: StallAction,
) -> WorkerStallDecision {
    WorkerStallDecision {
        worker_id: worker.worker_id.clone(),
        parent_session: worker.parent_session.clone(),
        age_secs,
        action,
        error: None,
        parent_state: worker.parent_state,
    }
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
            parent_scope_local: true,
            parent_state: ParentState::Present,
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
        assert_eq!(first[0].action, StallAction::Confirming);
        assert_eq!(second[0].action, StallAction::Notified);
        assert_eq!(notifications, 1);
        assert_eq!(logs, 1);

        let already = process_stalls(
            std::slice::from_ref(&stale),
            &mut memory,
            730,
            300,
            false,
            |_, _| panic!("the same incident is not delivered twice"),
            |_, _| panic!("the same incident is not logged twice"),
        );
        assert_eq!(already[0].action, StallAction::AlreadyNotified);

        let resumed = worker(600, false);
        let _ = process_stalls(
            std::slice::from_ref(&resumed),
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
        let after_new_stall = process_stalls(
            &[resumed],
            &mut memory,
            1_030,
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
        let _ = process_stalls(
            std::slice::from_ref(&stale),
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
        let decisions = process_stalls(
            &[stale],
            &mut memory,
            530,
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

    #[test]
    fn incident_is_logged_before_delivery_and_delivery_retries_without_duplicate_log() {
        let stale = worker(100, false);
        let mut memory = WorkerMemory::default();
        let events = std::cell::RefCell::new(Vec::new());
        let _ = process_stalls(
            std::slice::from_ref(&stale),
            &mut memory,
            500,
            300,
            false,
            |_, _| panic!("first stale sample only starts confirmation"),
            |_, _| panic!("confirmation must precede incident persistence"),
        );
        let first = process_stalls(
            std::slice::from_ref(&stale),
            &mut memory,
            530,
            300,
            false,
            |_, _| {
                events.borrow_mut().push("notify");
                Err(std::io::Error::other("temporarily unavailable"))
            },
            |_, _| {
                events.borrow_mut().push("log");
                Ok(())
            },
        );
        assert_eq!(first[0].action, StallAction::NotifyFailed);
        assert_eq!(*events.borrow(), ["log", "notify"]);

        let second = process_stalls(
            std::slice::from_ref(&stale),
            &mut memory,
            560,
            300,
            false,
            |_, _| {
                events.borrow_mut().push("notify");
                Ok(())
            },
            |_, _| panic!("the persisted incident must not be logged twice"),
        );
        assert_eq!(second[0].action, StallAction::Notified);
        assert_eq!(*events.borrow(), ["log", "notify", "notify"]);

        let third = process_stalls(
            &[stale],
            &mut memory,
            590,
            300,
            false,
            |_, _| panic!("successful delivery must be deduplicated"),
            |_, _| panic!("the incident must remain deduplicated"),
        );
        assert_eq!(third[0].action, StallAction::AlreadyNotified);
    }
}
