use std::time::{Duration, Instant};

use crate::layout::PaneId;
use crate::platform::{ForegroundJob, ForegroundProcess};

pub(crate) const FOREGROUND_PROCESS_REFRESH_INTERVAL: Duration = Duration::from_millis(1500);
pub(crate) const FOREGROUND_PROCESS_REFRESH_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ForegroundProcessRefreshInFlight {
    pub(crate) generation: u64,
    pub(crate) deadline: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ForegroundProcessTarget {
    pub(crate) pane_id: PaneId,
    pub(crate) shell_pid: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ForegroundProcessObservation {
    pub(crate) pane_id: PaneId,
    pub(crate) shell_pid: Option<u32>,
    pub(crate) process_name: Option<String>,
}

pub(crate) fn process_name_for_job(shell_pid: u32, job: &ForegroundJob) -> Option<String> {
    if job.process_group_id == shell_pid
        || job.processes.iter().any(|process| process.pid == shell_pid)
    {
        return None;
    }

    let process = job
        .processes
        .iter()
        .find(|process| process.pid == job.process_group_id)
        .or_else(|| job.processes.first())?;
    process_name(process)
}

fn process_name(process: &ForegroundProcess) -> Option<String> {
    let name = process.name.trim();
    if !name.is_empty() {
        return Some(name.to_string());
    }

    process.argv0.as_deref().and_then(|argv0| {
        argv0
            .rsplit(['/', '\\'])
            .find(|component| !component.is_empty())
            .filter(|name| !name.is_empty())
            .map(str::to_string)
    })
}

pub(crate) fn refresh_foreground_processes<F>(
    targets: &[ForegroundProcessTarget],
    deadline: Instant,
    lookup: F,
) -> Vec<ForegroundProcessObservation>
where
    F: Fn(u32) -> Option<ForegroundJob>,
{
    let mut observations = Vec::with_capacity(targets.len());
    for target in targets {
        let process_name = match target.shell_pid {
            None => None,
            Some(_) if Instant::now() >= deadline => break,
            Some(shell_pid) => {
                lookup(shell_pid).and_then(|job| process_name_for_job(shell_pid, &job))
            }
        };
        observations.push(ForegroundProcessObservation {
            pane_id: target.pane_id,
            shell_pid: target.shell_pid,
            process_name,
        });
    }
    observations
}

impl crate::app::App {
    pub(crate) fn foreground_process_refresh_deadline(&self) -> Option<Instant> {
        if let Some(refresh) = self.foreground_process_refresh_in_flight.as_ref() {
            return Some(refresh.deadline);
        }
        (!self.state.workspaces.is_empty()).then_some(self.next_foreground_process_refresh)
    }

    pub(crate) fn start_foreground_process_refresh_if_due(&mut self, now: Instant) {
        if self
            .foreground_process_refresh_in_flight
            .as_ref()
            .is_some_and(|refresh| now >= refresh.deadline)
        {
            self.foreground_process_refresh_in_flight = None;
        }

        if self.foreground_process_refresh_in_flight.is_some()
            || now < self.next_foreground_process_refresh
        {
            return;
        }

        self.next_foreground_process_refresh = now + FOREGROUND_PROCESS_REFRESH_INTERVAL;
        let targets = self.foreground_process_targets();
        if targets.is_empty() {
            return;
        }

        self.last_foreground_process_refresh_generation = self
            .last_foreground_process_refresh_generation
            .wrapping_add(1);
        let generation = self.last_foreground_process_refresh_generation;
        let deadline = now + FOREGROUND_PROCESS_REFRESH_TIMEOUT;
        self.foreground_process_refresh_in_flight = Some(ForegroundProcessRefreshInFlight {
            generation,
            deadline,
        });

        let event_tx = self.event_tx.clone();
        let _ = std::thread::Builder::new()
            .name("herdr-foreground-process".into())
            .spawn(move || {
                let observations = refresh_foreground_processes(
                    &targets,
                    deadline,
                    crate::detect::foreground_process_job,
                );
                let _ =
                    event_tx.blocking_send(crate::events::AppEvent::ForegroundProcessesRefreshed {
                        generation,
                        observations,
                    });
            });
    }

    fn foreground_process_targets(&self) -> Vec<ForegroundProcessTarget> {
        let mut targets = Vec::new();
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for tab in &workspace.tabs {
                for pane_id in tab.layout.pane_ids() {
                    let shell_pid = self
                        .state
                        .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
                        .and_then(|runtime| runtime.child_pid());
                    targets.push(ForegroundProcessTarget { pane_id, shell_pid });
                }
            }
        }
        targets
    }

    pub(crate) fn handle_foreground_processes_refreshed(
        &mut self,
        generation: u64,
        observations: Vec<ForegroundProcessObservation>,
    ) -> bool {
        let Some(refresh) = self.foreground_process_refresh_in_flight.as_ref() else {
            return false;
        };
        if refresh.generation != generation {
            return false;
        }

        let now = Instant::now();
        let deadline = refresh.deadline;
        self.foreground_process_refresh_in_flight = None;
        if now >= deadline {
            self.next_foreground_process_refresh = now + FOREGROUND_PROCESS_REFRESH_INTERVAL;
            return false;
        }

        let mut changed = false;
        for observation in observations {
            let Some((ws_idx, terminal_id)) =
                self.state
                    .workspaces
                    .iter()
                    .enumerate()
                    .find_map(|(ws_idx, workspace)| {
                        workspace
                            .terminal_id(observation.pane_id)
                            .cloned()
                            .map(|terminal_id| (ws_idx, terminal_id))
                    })
            else {
                continue;
            };
            let current_shell_pid = self
                .state
                .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, observation.pane_id)
                .and_then(|runtime| runtime.child_pid());
            if current_shell_pid != observation.shell_pid {
                continue;
            }
            let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
                continue;
            };
            changed |= terminal.set_foreground_process_name(observation.process_name);
        }

        if changed {
            self.render_dirty.request_generic();
            self.render_notify.notify_one();
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, name: &str) -> ForegroundProcess {
        ForegroundProcess {
            pid,
            name: name.into(),
            argv0: None,
            argv: None,
            cmdline: None,
        }
    }

    fn job(process_group_id: u32, processes: Vec<ForegroundProcess>) -> ForegroundJob {
        ForegroundJob {
            process_group_id,
            processes,
        }
    }

    #[test]
    fn distinct_foreground_process_uses_group_leader_name() {
        let result = process_name_for_job(
            10,
            &job(20, vec![process(21, "child"), process(20, "cargo")]),
        );

        assert_eq!(result.as_deref(), Some("cargo"));
    }

    #[test]
    fn shell_foreground_group_is_suppressed() {
        assert_eq!(
            process_name_for_job(10, &job(10, vec![process(10, "zsh")])),
            None
        );
        assert_eq!(
            process_name_for_job(10, &job(20, vec![process(10, "zsh")])),
            None
        );
    }

    #[test]
    fn missing_shell_or_lookup_result_is_silent() {
        let targets = [
            ForegroundProcessTarget {
                pane_id: PaneId::from_raw(1),
                shell_pid: None,
            },
            ForegroundProcessTarget {
                pane_id: PaneId::from_raw(2),
                shell_pid: Some(10),
            },
        ];
        let observations = refresh_foreground_processes(
            &targets,
            Instant::now() + Duration::from_secs(1),
            |_pid| None,
        );

        assert_eq!(observations.len(), 2);
        assert!(observations
            .iter()
            .all(|observation| observation.process_name.is_none()));
    }
}
