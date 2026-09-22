use std::process::Command;

const SERVER_CONTEXT_ENV: &str = "HERDR_MACOS_SERVER_CONTEXT";
const USER_CONTEXT: &str = "user";

pub(crate) fn configure_server_daemon_context(command: &mut Command) {
    // Handoff inherits the source context, including intentionally direct launches.
    // Older replacements also cannot consume this marker and would leak it.
    if command.get_args().any(|arg| arg == "--handoff-import") {
        command.env_remove(SERVER_CONTEXT_ENV);
    } else {
        command.env(SERVER_CONTEXT_ENV, USER_CONTEXT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn handoff_commands_remove_even_an_inherited_launch_marker() {
        let mut command = Command::new("herdr");
        command.args(["server", "--handoff-import", "socket", "token"]);
        command.env(SERVER_CONTEXT_ENV, USER_CONTEXT);
        crate::platform::detach_server_daemon_command(&mut command);
        assert!(command
            .get_envs()
            .any(|(key, value)| key == SERVER_CONTEXT_ENV && value.is_none()));
    }

    #[test]
    fn only_detached_server_commands_request_user_context() {
        let direct = Command::new("herdr");
        assert!(!direct.get_envs().any(|(key, _)| key == SERVER_CONTEXT_ENV));
        let mut detached = Command::new("herdr");
        crate::platform::detach_server_daemon_command(&mut detached);
        assert!(detached.get_envs().any(|(key, value)| {
            key == SERVER_CONTEXT_ENV && value == Some(OsStr::new(USER_CONTEXT))
        }));
    }
}
