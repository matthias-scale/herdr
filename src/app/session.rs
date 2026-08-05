use std::time::{Duration, Instant};

use super::{App, SESSION_SAVE_DEBOUNCE};

const SESSION_SAVE_COMPLETION_POLL: Duration = Duration::from_millis(100);
const SESSION_SAVE_RETRY_BASE: Duration = Duration::from_millis(250);
const SESSION_SAVE_RETRY_MAX: Duration = Duration::from_secs(30);

enum SessionSaveJob {
    Clear,
    Save {
        snapshot: crate::persist::SessionSnapshot,
        history: Option<crate::persist::SessionHistorySnapshot>,
    },
}

pub(crate) struct SessionSaveResult {
    pub(super) revision: u64,
    pub(super) result: std::io::Result<()>,
}

impl App {
    pub(super) fn schedule_session_save(&mut self) {
        if self.no_session {
            return;
        }

        self.state.mark_session_dirty();
        if self.session_save_retry_deadline.is_none() {
            self.session_save_deadline = Some(Instant::now() + SESSION_SAVE_DEBOUNCE);
            self.session_save_scheduled_revision = Some(self.state.session_dirty_revision);
        }
    }

    pub(crate) fn sync_session_save_schedule(&mut self) {
        self.reap_finished_session_save();
        if let Some(retry_at) = self.session_save_retry_deadline {
            self.session_save_deadline = Some(retry_at);
            return;
        }
        if self.state.session_dirty
            && self.session_save_thread.is_none()
            && (self.session_save_deadline.is_none()
                || self.session_save_scheduled_revision != Some(self.state.session_dirty_revision))
        {
            self.schedule_session_save();
        }
    }

    pub(super) fn reap_finished_session_save(&mut self) {
        if !self
            .session_save_thread
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            return;
        }

        let Some(thread) = self.session_save_thread.take() else {
            return;
        };
        match thread.join() {
            Ok(result) => self.apply_session_save_result(result),
            Err(_) => self.apply_session_save_result(SessionSaveResult {
                revision: self.state.session_dirty_revision,
                result: Err(std::io::Error::other("session save thread panicked")),
            }),
        }
    }

    pub(super) fn apply_session_save_result(&mut self, result: SessionSaveResult) {
        match result.result {
            Ok(()) => {
                self.session_save_failures = 0;
                self.session_save_retry_deadline = None;
                if self.state.session_dirty_revision == result.revision {
                    self.state.session_dirty = false;
                    self.session_save_deadline = None;
                }
            }
            Err(err) => {
                self.state.session_dirty = true;
                self.session_save_failures = self.session_save_failures.saturating_add(1);
                let delay = session_save_retry_delay(self.session_save_failures);
                let retry_at = Instant::now() + delay;
                self.session_save_retry_deadline = Some(retry_at);
                self.session_save_deadline = Some(retry_at);
                tracing::warn!(
                    err = %err,
                    attempt = self.session_save_failures,
                    retry_ms = delay.as_millis(),
                    "session save failed; retry scheduled"
                );
            }
        }
    }

    fn capture_session_save_job(&self) -> SessionSaveJob {
        if self.state.workspaces.is_empty() {
            SessionSaveJob::Clear
        } else {
            let snapshot = crate::persist::capture(
                &self.state.workspaces,
                &self.state.terminals,
                &self.terminal_runtimes,
                self.state.active,
                self.state.selected,
                self.state.sidebar_width,
                self.state.sidebar_section_split,
                self.state.collapsed_space_keys.clone(),
                self.state.prio_panel_collapsed,
            );
            let history = self.persist_pane_history.then(|| {
                crate::persist::capture_history(&self.state.workspaces, &self.terminal_runtimes)
            });
            SessionSaveJob::Save { snapshot, history }
        }
    }

    pub(crate) fn start_background_session_save(&mut self) {
        if self.no_session {
            self.session_save_deadline = None;
            self.session_save_scheduled_revision = None;
            self.session_save_retry_deadline = None;
            return;
        }

        self.reap_finished_session_save();
        if self.session_save_thread.is_some() {
            self.session_save_deadline = Some(Instant::now() + SESSION_SAVE_COMPLETION_POLL);
            return;
        }
        if self
            .session_save_retry_deadline
            .is_some_and(|retry_at| Instant::now() < retry_at)
        {
            self.session_save_deadline = self.session_save_retry_deadline;
            return;
        }
        if !self.state.session_dirty {
            self.session_save_deadline = None;
            return;
        }

        let job = self.capture_session_save_job();
        let revision = self.state.session_dirty_revision;
        self.session_save_scheduled_revision = Some(revision);
        self.session_save_retry_deadline = None;
        self.session_save_deadline = Some(Instant::now() + SESSION_SAVE_COMPLETION_POLL);
        match std::thread::Builder::new()
            .name("herdr-session-save".into())
            .spawn(move || run_session_save_job(job, revision))
        {
            Ok(thread) => self.session_save_thread = Some(thread),
            Err(err) => {
                tracing::warn!(err = %err, "failed to spawn session save thread; saving inline");
                let result = run_session_save_job(self.capture_session_save_job(), revision);
                self.apply_session_save_result(result);
            }
        }
    }

    pub(crate) fn save_session_now(&mut self) {
        if let Some(thread) = self.session_save_thread.take() {
            match thread.join() {
                Ok(result) => self.apply_session_save_result(result),
                Err(_) => self.apply_session_save_result(SessionSaveResult {
                    revision: self.state.session_dirty_revision,
                    result: Err(std::io::Error::other("session save thread panicked")),
                }),
            }
        }

        if self.no_session {
            self.session_save_deadline = None;
            self.session_save_scheduled_revision = None;
            self.session_save_retry_deadline = None;
            return;
        }

        let revision = self.state.session_dirty_revision;
        let result = run_session_save_job(self.capture_session_save_job(), revision);
        self.apply_session_save_result(result);
    }
}

fn session_save_retry_delay(failures: u32) -> Duration {
    let shift = failures.saturating_sub(1).min(16);
    SESSION_SAVE_RETRY_BASE
        .saturating_mul(1_u32 << shift)
        .min(SESSION_SAVE_RETRY_MAX)
}

fn run_session_save_job(job: SessionSaveJob, revision: u64) -> SessionSaveResult {
    let result = match job {
        SessionSaveJob::Clear => crate::persist::clear(),
        SessionSaveJob::Save { snapshot, history } => {
            crate::persist::save(&snapshot, history.as_ref())
        }
    };
    SessionSaveResult { revision, result }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac1_retry_backoff_is_bounded() {
        assert_eq!(session_save_retry_delay(1), Duration::from_millis(250));
        assert_eq!(session_save_retry_delay(2), Duration::from_millis(500));
        assert_eq!(session_save_retry_delay(u32::MAX), SESSION_SAVE_RETRY_MAX);
    }
}
