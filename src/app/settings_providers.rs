//! The Providers section of the settings screen.
//!
//! A provider row is the probe result plus the flags Herdr would pass that
//! agent. The flags come from the dispatch argv builder, not from a second
//! description of it, so the row cannot advertise a launch Herdr does not make.

use crate::{app::state::AppState, detect::Agent};

/// The flag line for one probe label, for example `claude`.
pub(crate) fn provider_flags_label(app: &AppState, label: &str) -> String {
    let Some(agent) = crate::detect::parse_agent_label(label) else {
        return "not launchable from herdr".to_string();
    };
    let executable = crate::detect::interactive_agent_executable(agent);
    match launch_flags(app, agent) {
        None => "not launchable from herdr".to_string(),
        Some(flags) if flags.is_empty() => format!("{executable}   (no default flags)"),
        Some(flags) => format!("{executable} {}", flags.join(" ")),
    }
}

/// The flags for `agent`: the composer's current selection when it is the
/// selected agent, otherwise the agent's own defaults.
fn launch_flags(app: &AppState, agent: Agent) -> Option<Vec<String>> {
    if let Some(home) = app.home.as_ref() {
        if home.agent == agent {
            return home.launch_flags();
        }
    }
    matches!(agent, Agent::Claude | Agent::Codex).then(Vec::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_provider_without_selected_options_shows_no_default_flags() {
        let app = AppState::test_new();
        assert_eq!(
            provider_flags_label(&app, "claude"),
            "claude   (no default flags)"
        );
        assert_eq!(
            provider_flags_label(&app, "codex"),
            "codex   (no default flags)"
        );
    }

    #[test]
    fn a_provider_herdr_cannot_launch_says_so() {
        let app = AppState::test_new();
        assert_eq!(
            provider_flags_label(&app, "gemini"),
            "not launchable from herdr"
        );
        assert_eq!(
            provider_flags_label(&app, "gh"),
            "not launchable from herdr"
        );
    }
}
