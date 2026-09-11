//! Authoritative remote-control context checks.
//!
//! This module is deliberately independent of rendering, frame projection, and
//! fleet inventory. The server captures a fresh context on the control path and
//! validates it immediately before the PTY write.

#[cfg(unix)]
use crate::api::schema::ErrorBody;
use crate::api::schema::{AgentRef, RemoteControlContext};

#[derive(Debug, Clone)]
pub(crate) struct RemoteControlLease {
    pub(crate) agent_ref: AgentRef,
    pub(crate) context: RemoteControlContext,
    #[cfg(unix)]
    pub(crate) write_guard: crate::pty::actor::PtyWriteGuard,
}

impl PartialEq for RemoteControlLease {
    fn eq(&self, other: &Self) -> bool {
        self.agent_ref == other.agent_ref && self.context == other.context
    }
}

impl Eq for RemoteControlLease {}

impl RemoteControlLease {
    // The guarded write path exists only on Unix, but Windows test fixtures
    // still construct a lease to exercise shared protocol state.
    #[cfg(test)]
    pub(crate) fn new(agent_ref: AgentRef, context: RemoteControlContext) -> Self {
        #[cfg(unix)]
        {
            Self::new_with_guard(agent_ref, context, crate::pty::actor::PtyWriteGuard::new())
        }
        #[cfg(not(unix))]
        Self { agent_ref, context }
    }

    #[cfg(unix)]
    pub(crate) fn new_with_guard(
        agent_ref: AgentRef,
        context: RemoteControlContext,
        write_guard: crate::pty::actor::PtyWriteGuard,
    ) -> Self {
        write_guard.activate(context.context_epoch);
        Self {
            agent_ref,
            context,
            write_guard,
        }
    }

    pub(crate) fn revoke(&self) {
        #[cfg(unix)]
        self.write_guard.revoke();
    }

    #[cfg(unix)]
    pub(crate) fn write_authorization(
        &self,
        on_unknown: std::sync::Arc<dyn Fn() + Send + Sync>,
    ) -> crate::pty::actor::PtyWriteAuthorization {
        self.write_guard.authorization(
            self.context.foreground_process.process_group_id,
            self.context.context_epoch,
            on_unknown,
        )
    }
}

#[cfg(unix)]
pub(crate) trait RemoteControlContextProvider {
    fn fresh_remote_control_context(
        &self,
        agent_ref: &AgentRef,
    ) -> Result<RemoteControlContext, ErrorBody>;
}

#[cfg(unix)]
fn refusal(reason: impl Into<String>) -> ErrorBody {
    ErrorBody {
        code: "refused_for_safety".to_owned(),
        message: reason.into(),
    }
}

#[cfg(unix)]
fn required(value: &str, field: &str) -> Result<(), ErrorBody> {
    if value.trim().is_empty() {
        Err(refusal(format!(
            "required control fact {field} is unavailable"
        )))
    } else {
        Ok(())
    }
}

#[cfg(unix)]
pub(crate) fn validate_input_owner(owner: Option<u64>, client_id: u64) -> Result<(), ErrorBody> {
    if owner == Some(client_id) {
        Ok(())
    } else {
        Err(ErrorBody {
            code: "already_controlled".to_owned(),
            message: "remote control ownership was revoked".to_owned(),
        })
    }
}

/// Validate a fresh remote context and enqueue bytes without exposing a
/// check-then-write gap to callers. The caller must invoke this from the
/// server event loop while it owns the runtime mutation boundary.
#[cfg(unix)]
pub(crate) fn validate_and_enqueue(
    configured_host: &str,
    expected_user: &str,
    expected: &RemoteControlContext,
    current: &RemoteControlContext,
    data: &[u8],
    enqueue: impl FnOnce(&[u8]) -> Result<(), String>,
) -> Result<(), ErrorBody> {
    validate_context(configured_host, expected_user, expected, current)?;
    enqueue(data).map_err(|error| ErrorBody {
        code: "connection_lost".to_owned(),
        message: format!("remote PTY write failed: {error}; delivery is unknown"),
    })
}

#[cfg(unix)]
pub(crate) fn validate_context(
    configured_host: &str,
    expected_user: &str,
    expected: &RemoteControlContext,
    current: &RemoteControlContext,
) -> Result<(), ErrorBody> {
    for (value, field) in [
        (configured_host, "configured host"),
        (expected_user, "configured user"),
        (current.host.as_str(), "host"),
        (current.user.as_str(), "user"),
        (current.workspace_id.as_str(), "workspace_id"),
        (current.tab_id.as_str(), "tab_id"),
        (current.pane_id.as_str(), "pane_id"),
        (current.terminal_id.as_str(), "terminal_id"),
        (current.cwd.as_str(), "cwd"),
        (current.foreground_cwd.as_str(), "foreground_cwd"),
        (current.tty.as_str(), "tty"),
        (
            current.foreground_process.name.as_str(),
            "foreground_process.name",
        ),
        (
            current.foreground_process.cwd.as_str(),
            "foreground_process.cwd",
        ),
        (current.detected_agent.as_str(), "detected_agent"),
    ] {
        required(value, field)?;
    }
    if current.foreground_process.pid == 0 {
        return Err(refusal("foreground_process.pid is unavailable"));
    }
    if current.foreground_process.process_group_id == 0 {
        return Err(refusal(
            "foreground_process.process_group_id is unavailable",
        ));
    }
    if current.foreground_process.argv.is_empty() {
        return Err(refusal("foreground_process.argv is unavailable"));
    }
    if !current.interactive_ready {
        return Err(refusal("agent is not interactive_ready"));
    }
    if current.human_draft {
        return Err(refusal("a human input draft is pending"));
    }
    if current.host != configured_host {
        return Err(refusal("remote host does not match configured alias"));
    }
    if current.user != expected_user {
        return Err(refusal("remote user does not match configured user"));
    }
    if expected != current {
        return Err(refusal("remote control context changed"));
    }
    if expected.human_draft {
        return Err(refusal("expected context contains a human input draft"));
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    type ContextMutation = (&'static str, Box<dyn Fn(&mut RemoteControlContext)>);

    fn context() -> RemoteControlContext {
        RemoteControlContext {
            host: "buildbox".into(),
            user: "operator".into(),
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
            pane_id: "w1:p3".into(),
            terminal_id: "terminal-id".into(),
            cwd: "/work/repo".into(),
            foreground_cwd: "/work/repo".into(),
            tty: "/dev/pts/4".into(),
            foreground_process: crate::api::schema::RemoteForegroundProcess {
                pid: 1234,
                process_group_id: 1234,
                name: "agent".into(),
                argv: vec!["agent".into(), "run".into()],
                cwd: "/work/repo".into(),
            },
            detected_agent: "agent".into(),
            interactive_ready: true,
            human_draft: false,
            state_change_seq: 9,
            revision: 41,
            context_epoch: 12,
        }
    }

    fn assert_refused(mutated: impl FnOnce(&mut RemoteControlContext)) {
        let expected = context();
        let mut current = expected.clone();
        mutated(&mut current);
        let writes = std::cell::Cell::new(0_u8);
        let result = validate_and_enqueue(
            "buildbox",
            "operator",
            &expected,
            &current,
            b"answer",
            |_| {
                writes.set(writes.get().saturating_add(1));
                Ok(())
            },
        );
        assert_eq!(
            result.as_ref().err().map(|error| error.code.as_str()),
            Some("refused_for_safety")
        );
        assert_eq!(writes.get(), 0);
    }

    fn assert_refused_for_configured_fact(configured_host: &str, expected_user: &str) {
        let expected = context();
        let writes = std::cell::Cell::new(0_u8);
        let result = validate_and_enqueue(
            configured_host,
            expected_user,
            &expected,
            &expected,
            b"answer",
            |_| {
                writes.set(writes.get().saturating_add(1));
                Ok(())
            },
        );
        assert_eq!(
            result.as_ref().err().map(|error| error.code.as_str()),
            Some("refused_for_safety")
        );
        assert_eq!(writes.get(), 0);
    }

    #[test]
    fn configured_host_and_user_are_authoritative() {
        assert_refused_for_configured_fact("other-host", "operator");
        assert_refused_for_configured_fact("buildbox", "other-user");
    }

    #[test]
    fn mismatched_reported_user_is_refused_against_effective_uid_fact() {
        let expected = context();
        let mut current = expected.clone();
        current.user = "spoofed-user".into();
        let effective_user = crate::platform::effective_user_name().expect("effective user");
        let result = validate_context("buildbox", &effective_user, &expected, &current);
        assert_eq!(
            result.as_ref().err().map(|error| error.code.as_str()),
            Some("refused_for_safety")
        );
    }

    #[test]
    fn every_identity_and_runtime_fact_is_fail_closed() {
        let mutations: [ContextMutation; 19] = [
            ("host", Box::new(|context| context.host = "other".into())),
            ("user", Box::new(|context| context.user = "other".into())),
            (
                "workspace",
                Box::new(|context| context.workspace_id = "w2".into()),
            ),
            ("tab", Box::new(|context| context.tab_id = "w1:t2".into())),
            ("pane", Box::new(|context| context.pane_id = "w1:p4".into())),
            (
                "terminal",
                Box::new(|context| context.terminal_id = "other-terminal".into()),
            ),
            ("cwd", Box::new(|context| context.cwd = "/other".into())),
            (
                "foreground cwd",
                Box::new(|context| context.foreground_cwd = "/other".into()),
            ),
            ("tty", Box::new(|context| context.tty = "/dev/pts/5".into())),
            (
                "process group",
                Box::new(|context| context.foreground_process.process_group_id += 1),
            ),
            (
                "pid",
                Box::new(|context| context.foreground_process.pid += 1),
            ),
            (
                "argv",
                Box::new(|context| context.foreground_process.argv = vec!["other".into()]),
            ),
            (
                "process name",
                Box::new(|context| context.foreground_process.name = "other".into()),
            ),
            (
                "process cwd",
                Box::new(|context| context.foreground_process.cwd = "/other".into()),
            ),
            (
                "detected agent",
                Box::new(|context| context.detected_agent = "other-agent".into()),
            ),
            (
                "agent readiness",
                Box::new(|context| context.interactive_ready = false),
            ),
            (
                "state epoch",
                Box::new(|context| context.state_change_seq += 1),
            ),
            ("state revision", Box::new(|context| context.revision += 1)),
            (
                "context epoch",
                Box::new(|context| context.context_epoch += 1),
            ),
        ];
        for (field, mutation) in mutations {
            assert_refused(|context| mutation(context));
            assert!(!field.is_empty());
        }
    }

    #[test]
    fn missing_facts_and_drafts_are_refused_without_writes() {
        for mutation in [
            Box::new(|context: &mut RemoteControlContext| context.tty.clear())
                as Box<dyn Fn(&mut RemoteControlContext)>,
            Box::new(|context| context.cwd.clear()),
            Box::new(|context| context.foreground_process.argv.clear()),
            Box::new(|context| context.human_draft = true),
        ] {
            assert_refused(mutation);
        }
    }

    #[test]
    fn draft_bytes_are_not_touched_when_control_is_refused() {
        let expected = context();
        let mut current = expected.clone();
        current.human_draft = true;
        let draft = b"human typed draft, keep every byte".to_vec();
        let before = draft.clone();
        let writes = std::cell::Cell::new(0_u8);
        let result =
            validate_and_enqueue("buildbox", "operator", &expected, &current, &draft, |_| {
                writes.set(writes.get().saturating_add(1));
                Ok(())
            });
        assert_eq!(
            result.as_ref().err().map(|error| error.code.as_str()),
            Some("refused_for_safety")
        );
        assert_eq!(draft, before);
        assert_eq!(writes.get(), 0);
    }

    #[test]
    fn revalidation_revokes_before_the_next_write() {
        let expected = context();
        let mut current = expected.clone();
        let writes = std::cell::Cell::new(0_u8);
        assert!(validate_and_enqueue(
            "buildbox",
            "operator",
            &expected,
            &current,
            b"first",
            |_| {
                writes.set(writes.get().saturating_add(1));
                Ok(())
            },
        )
        .is_ok());
        current.revision += 1;
        let result = validate_and_enqueue(
            "buildbox",
            "operator",
            &expected,
            &current,
            b"second",
            |_| {
                writes.set(writes.get().saturating_add(1));
                Ok(())
            },
        );
        assert_eq!(
            result.as_ref().err().map(|error| error.code.as_str()),
            Some("refused_for_safety")
        );
        assert_eq!(writes.get(), 1);
    }

    #[test]
    fn blocked_agent_is_allowed_when_other_facts_match() {
        let expected = context();
        let writes = std::cell::Cell::new(0_u8);
        let result = validate_and_enqueue(
            "buildbox",
            "operator",
            &expected,
            &expected,
            b"answer",
            |_| {
                writes.set(1);
                Ok(())
            },
        );
        assert!(result.is_ok());
        assert_eq!(writes.get(), 1);
    }

    #[test]
    fn failed_pty_enqueue_reports_unknown_delivery() {
        let expected = context();
        let result = validate_and_enqueue(
            "buildbox",
            "operator",
            &expected,
            &expected,
            b"answer",
            |_| Err("runtime disconnected".to_owned()),
        );
        assert_eq!(
            result.expect_err("enqueue must fail").code,
            "connection_lost"
        );
    }

    #[test]
    fn revoked_or_competing_controller_is_rejected_before_input() {
        assert!(validate_input_owner(Some(7), 7).is_ok());
        for owner in [None, Some(8)] {
            assert_eq!(
                validate_input_owner(owner, 7)
                    .expect_err("non-owner must be rejected")
                    .code,
                "already_controlled"
            );
        }
    }
}
