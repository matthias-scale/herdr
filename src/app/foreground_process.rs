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
    pub(crate) process_active: bool,
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

fn process_is_active(shell_pid: u32, job: &ForegroundJob) -> bool {
    if job.process_group_id == shell_pid
        || job.processes.iter().any(|process| process.pid == shell_pid)
    {
        return false;
    }

    let Some((agent, _)) = crate::detect::identify_agent_in_job(job) else {
        return true;
    };
    let selected_agent_pid = job.processes.iter().find_map(|process| {
        let single_process_job = ForegroundJob {
            process_group_id: process.pid,
            processes: vec![process.clone()],
        };
        crate::detect::identify_agent_in_job(&single_process_job)
            .is_some_and(|(process_agent, _)| process_agent == agent)
            .then_some(process.pid)
    });

    job.processes.iter().any(|process| {
        let single_process_job = ForegroundJob {
            process_group_id: process.pid,
            processes: vec![process.clone()],
        };
        match crate::detect::identify_agent_in_job(&single_process_job) {
            Some(_) => Some(process.pid) != selected_agent_pid,
            None => {
                !crate::platform::is_pane_shell_process_name(&process.name)
                    && !is_idle_agent_runtime(&process.name)
            }
        }
    })
}

fn is_idle_agent_runtime(name: &str) -> bool {
    let name = name
        .rsplit(['/', '\\'])
        .find(|component| !component.is_empty())
        .unwrap_or(name)
        .trim_end_matches(".exe")
        .to_ascii_lowercase();
    name == "node"
        || name == "bun"
        || name == "python"
        || name.strip_prefix("python").is_some_and(|version| {
            !version.is_empty()
                && version
                    .split('.')
                    .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()))
        })
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
        let (process_name, process_active) = match target.shell_pid {
            None => (None, false),
            Some(_) if Instant::now() >= deadline => continue,
            Some(shell_pid) => lookup(shell_pid)
                .map(|job| {
                    (
                        process_name_for_job(shell_pid, &job),
                        process_is_active(shell_pid, &job),
                    )
                })
                .unwrap_or((None, false)),
        };
        observations.push(ForegroundProcessObservation {
            pane_id: target.pane_id,
            shell_pid: target.shell_pid,
            process_name,
            process_active,
        });
    }
    observations
}

fn rotate_targets_for_generation(targets: &mut [ForegroundProcessTarget], generation: u64) {
    let target_count = targets.len();
    if target_count > 1 {
        targets.rotate_left(generation as usize % target_count);
    }
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
        let mut targets = self.foreground_process_targets();
        if targets.is_empty() {
            return;
        }

        self.last_foreground_process_refresh_generation = self
            .last_foreground_process_refresh_generation
            .wrapping_add(1);
        let generation = self.last_foreground_process_refresh_generation;
        rotate_targets_for_generation(&mut targets, generation);
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
        if generation <= self.last_applied_foreground_process_refresh_generation
            || generation != self.last_foreground_process_refresh_generation
        {
            return false;
        }

        let now = Instant::now();
        if self
            .foreground_process_refresh_in_flight
            .as_ref()
            .is_some_and(|refresh| refresh.generation == generation)
        {
            let overran_deadline = self
                .foreground_process_refresh_in_flight
                .as_ref()
                .is_some_and(|refresh| now >= refresh.deadline);
            self.foreground_process_refresh_in_flight = None;
            if overran_deadline {
                self.next_foreground_process_refresh = now + FOREGROUND_PROCESS_REFRESH_INTERVAL;
            }
        }
        self.last_applied_foreground_process_refresh_generation = generation;

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
            let process_changed = self
                .state
                .terminals
                .get(&terminal_id)
                .is_some_and(|terminal| {
                    terminal.foreground_process_name != observation.process_name
                        || terminal.foreground_process_active() != observation.process_active
                });
            let update = self
                .state
                .update_terminal_state(observation.pane_id, |terminal| {
                    terminal.set_foreground_process(
                        observation.process_name,
                        observation.process_active,
                        now,
                    )
                });
            if let Some(update) = update {
                self.emit_pane_state_update(&update);
            }
            changed |= process_changed;
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

    fn process_with_argv(pid: u32, name: &str, argv: &[&str]) -> ForegroundProcess {
        ForegroundProcess {
            pid,
            name: name.into(),
            argv0: argv.first().map(|value| (*value).to_string()),
            argv: Some(argv.iter().map(|value| (*value).to_string()).collect()),
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
    fn resting_agent_process_is_not_active() {
        assert!(!process_is_active(
            10,
            &job(20, vec![process(20, "claude")])
        ));
    }

    #[test]
    fn agent_child_process_is_active() {
        assert!(process_is_active(
            10,
            &job(20, vec![process(20, "claude"), process(21, "codex")]),
        ));
    }

    #[test]
    fn same_agent_nested_child_is_active() {
        assert!(process_is_active(
            10,
            &job(20, vec![process(20, "claude"), process(21, "claude")]),
        ));
    }

    #[test]
    fn wrapped_resting_agent_processes_are_not_active() {
        assert!(!process_is_active(
            10,
            &job(
                20,
                vec![
                    process_with_argv(20, "node", &["node", "/path/to/bin/codex"]),
                    process(21, "bash"),
                ],
            ),
        ));
    }

    #[test]
    fn agent_with_plain_running_command_is_active() {
        assert!(process_is_active(
            10,
            &job(20, vec![process(20, "claude"), process(21, "cargo")]),
        ));
    }

    #[test]
    fn plain_foreground_command_is_active() {
        assert!(process_is_active(10, &job(20, vec![process(20, "cargo")])));
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

    #[test]
    fn timed_out_refresh_omits_unobserved_targets() {
        let targets = [
            ForegroundProcessTarget {
                pane_id: PaneId::from_raw(1),
                shell_pid: Some(10),
            },
            ForegroundProcessTarget {
                pane_id: PaneId::from_raw(2),
                shell_pid: Some(20),
            },
        ];

        for _ in 0..2 {
            let observations = refresh_foreground_processes(
                &targets,
                Instant::now() - Duration::from_millis(1),
                |_pid| panic!("timed-out targets must not be inspected"),
            );
            assert!(observations.is_empty());
        }
    }

    #[test]
    fn refresh_target_order_rotates_between_generations() {
        let targets = vec![
            ForegroundProcessTarget {
                pane_id: PaneId::from_raw(1),
                shell_pid: Some(10),
            },
            ForegroundProcessTarget {
                pane_id: PaneId::from_raw(2),
                shell_pid: Some(20),
            },
            ForegroundProcessTarget {
                pane_id: PaneId::from_raw(3),
                shell_pid: Some(30),
            },
        ];

        let mut first_generation = targets.clone();
        rotate_targets_for_generation(&mut first_generation, 1);
        assert_eq!(first_generation[0].pane_id, PaneId::from_raw(2));

        let mut second_generation = targets;
        rotate_targets_for_generation(&mut second_generation, 2);
        assert_eq!(second_generation[0].pane_id, PaneId::from_raw(3));
    }

    fn app_with_test_pane(name: &str) -> (crate::app::App, PaneId, crate::terminal::TerminalId) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let workspace = crate::workspace::Workspace::test_new(name);
        let pane_id = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        let terminal_id = app.state.workspaces[0].tabs[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("test pane terminal");
        (app, pane_id, terminal_id)
    }

    #[test]
    fn unobserved_refresh_leaves_process_state_and_events_unchanged() {
        let (mut app, pane_id, terminal_id) = app_with_test_pane("unobserved-foreground");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test pane terminal")
            .set_foreground_process(Some("cargo".into()), true, Instant::now());
        let event_count = app.event_hub.events_after(0).len();
        app.last_foreground_process_refresh_generation = 1;

        assert!(!app.handle_foreground_processes_refreshed(1, Vec::new()));

        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(terminal.foreground_process_name.as_deref(), Some("cargo"));
        assert!(terminal.foreground_process_active());
        assert_eq!(app.event_hub.events_after(0).len(), event_count);
        assert_eq!(app.state.workspaces[0].tabs[0].root_pane, pane_id);
    }

    #[test]
    fn late_foreground_process_refresh_applies_process_name() {
        let (mut app, pane_id, terminal_id) = app_with_test_pane("late-foreground");
        app.last_foreground_process_refresh_generation = 1;
        app.foreground_process_refresh_in_flight = Some(ForegroundProcessRefreshInFlight {
            generation: 1,
            deadline: Instant::now() - Duration::from_millis(1),
        });

        app.handle_foreground_processes_refreshed(
            1,
            vec![ForegroundProcessObservation {
                pane_id,
                shell_pid: None,
                process_name: Some("cargo".into()),
                process_active: true,
            }],
        );

        assert_eq!(
            app.state.terminals[&terminal_id]
                .foreground_process_name
                .as_deref(),
            Some("cargo")
        );
        assert!(app.foreground_process_refresh_in_flight.is_none());
        assert!(app.next_foreground_process_refresh > Instant::now());
    }

    #[test]
    fn foreground_process_refresh_promotes_idle_agent_until_process_clears() {
        let (mut app, pane_id, terminal_id) = app_with_test_pane("active-agent-child");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test pane terminal")
            .set_detected_state(
                Some(crate::detect::Agent::Claude),
                crate::detect::AgentState::Idle,
            );

        app.last_foreground_process_refresh_generation = 1;
        assert!(app.handle_foreground_processes_refreshed(
            1,
            vec![ForegroundProcessObservation {
                pane_id,
                shell_pid: None,
                process_name: Some("codex".into()),
                process_active: true,
            }],
        ));
        assert_eq!(
            app.state.terminals[&terminal_id].state,
            crate::detect::AgentState::Working
        );

        app.last_foreground_process_refresh_generation = 2;
        assert!(app.handle_foreground_processes_refreshed(
            2,
            vec![ForegroundProcessObservation {
                pane_id,
                shell_pid: None,
                process_name: None,
                process_active: false,
            }],
        ));
        assert_eq!(
            app.state.terminals[&terminal_id].state,
            crate::detect::AgentState::Idle
        );
    }

    #[test]
    fn same_process_name_activity_flip_requests_render_and_emits_status_event() {
        let (mut app, pane_id, terminal_id) = app_with_test_pane("same-name-activity");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test pane terminal")
            .set_detected_state(
                Some(crate::detect::Agent::Claude),
                crate::detect::AgentState::Idle,
            );
        app.last_foreground_process_refresh_generation = 1;
        app.handle_foreground_processes_refreshed(
            1,
            vec![ForegroundProcessObservation {
                pane_id,
                shell_pid: None,
                process_name: Some("claude".into()),
                process_active: false,
            }],
        );
        let _ = app.render_dirty.take();
        let event_count = app.event_hub.events_after(0).len();

        app.last_foreground_process_refresh_generation = 2;
        assert!(app.handle_foreground_processes_refreshed(
            2,
            vec![ForegroundProcessObservation {
                pane_id,
                shell_pid: None,
                process_name: Some("claude".into()),
                process_active: true,
            }],
        ));

        assert!(app.render_dirty.is_pending());
        assert!(
            app.event_hub.events_after(0)[event_count..]
                .iter()
                .any(|(_, event)| event.event
                    == crate::api::schema::EventKind::PaneAgentStatusChanged)
        );
    }

    #[test]
    fn scheduler_expired_foreground_refresh_accepts_late_result_and_rejects_older_generation() {
        let (mut app, pane_id, terminal_id) = app_with_test_pane("scheduled-late-foreground");
        let first_now = Instant::now();
        app.next_foreground_process_refresh = first_now;
        app.start_foreground_process_refresh_if_due(first_now);
        let first_refresh = app
            .foreground_process_refresh_in_flight
            .clone()
            .expect("first foreground refresh in flight");

        // Push the schedule out so the deadline sweep clears the in-flight record
        // without issuing a successor. That is the interleaving the late-result
        // repair exists for: the record is gone, but nothing has superseded the
        // generation, so its results are still the freshest data available.
        app.next_foreground_process_refresh = first_refresh.deadline + Duration::from_secs(60);
        app.start_foreground_process_refresh_if_due(
            first_refresh.deadline + Duration::from_millis(1),
        );
        assert!(app.foreground_process_refresh_in_flight.is_none());
        assert_eq!(
            app.last_foreground_process_refresh_generation,
            first_refresh.generation
        );

        assert!(app.handle_foreground_processes_refreshed(
            first_refresh.generation,
            vec![ForegroundProcessObservation {
                pane_id,
                shell_pid: None,
                process_name: Some("cargo".into()),
                process_active: true,
            }],
        ));
        assert_eq!(
            app.state.terminals[&terminal_id]
                .foreground_process_name
                .as_deref(),
            Some("cargo")
        );

        assert!(!app.handle_foreground_processes_refreshed(
            first_refresh.generation,
            vec![ForegroundProcessObservation {
                pane_id,
                shell_pid: None,
                process_name: Some("stale-process".into()),
                process_active: true,
            }],
        ));
        assert_eq!(
            app.state.terminals[&terminal_id]
                .foreground_process_name
                .as_deref(),
            Some("cargo")
        );
    }

    #[test]
    fn superseded_foreground_refresh_drops_result_before_successor() {
        let (mut app, pane_id, terminal_id) = app_with_test_pane("superseded-foreground");
        let first_now = Instant::now();
        app.next_foreground_process_refresh = first_now;
        app.start_foreground_process_refresh_if_due(first_now);
        let first_refresh = app
            .foreground_process_refresh_in_flight
            .clone()
            .expect("first foreground refresh in flight");

        let successor_now = first_refresh.deadline + Duration::from_millis(1);
        app.next_foreground_process_refresh = successor_now;
        app.start_foreground_process_refresh_if_due(successor_now);
        let successor_generation = app
            .foreground_process_refresh_in_flight
            .as_ref()
            .expect("successor foreground refresh in flight")
            .generation;
        assert_eq!(successor_generation, first_refresh.generation + 1);

        assert!(!app.handle_foreground_processes_refreshed(
            first_refresh.generation,
            vec![ForegroundProcessObservation {
                pane_id,
                shell_pid: None,
                process_name: Some("stale-process".into()),
                process_active: true,
            }],
        ));
        assert_eq!(
            app.state.terminals[&terminal_id].foreground_process_name,
            None
        );
        assert_eq!(
            app.foreground_process_refresh_in_flight
                .as_ref()
                .map(|refresh| refresh.generation),
            Some(successor_generation)
        );

        assert!(!app.handle_foreground_processes_refreshed(successor_generation, Vec::new(),));
        assert_eq!(
            app.last_applied_foreground_process_refresh_generation,
            successor_generation
        );
        assert!(app.foreground_process_refresh_in_flight.is_none());
    }

    #[test]
    fn foreground_process_generation_mismatch_drops_observations() {
        let (mut app, pane_id, terminal_id) = app_with_test_pane("stale-foreground");
        app.foreground_process_refresh_in_flight = Some(ForegroundProcessRefreshInFlight {
            generation: 2,
            deadline: Instant::now() - Duration::from_millis(1),
        });
        app.last_applied_foreground_process_refresh_generation = 1;

        app.handle_foreground_processes_refreshed(
            1,
            vec![ForegroundProcessObservation {
                pane_id,
                shell_pid: None,
                process_name: Some("stale-process".into()),
                process_active: true,
            }],
        );

        assert_eq!(
            app.state.terminals[&terminal_id].foreground_process_name,
            None
        );
        assert_eq!(
            app.foreground_process_refresh_in_flight
                .map(|refresh| refresh.generation),
            Some(2)
        );
    }
}
