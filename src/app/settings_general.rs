//! The General section of the settings screen.
//!
//! Each row is exactly one config key. The row list is pure data so the
//! renderer, the hit test and the tests all read the same thing, and so a row
//! can never drift away from the key it claims to edit.

use crate::app::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GeneralRow {
    ProjectGrouping,
    AutoSettleFinished,
    AutoSettleInactive,
    SettleAfterDays,
    HideWhitespace,
    NewThreadWorkspace,
    AddProjectStartDir,
    DeleteConfirmation,
}

/// The inactivity thresholds `enter` cycles through on the days row. A settings
/// screen has no number field, and the useful values are few.
const SETTLE_DAY_LADDER: [u64; 5] = [1, 3, 7, 14, 30];

impl GeneralRow {
    pub(crate) const ALL: &'static [Self] = &[
        Self::ProjectGrouping,
        Self::AutoSettleFinished,
        Self::AutoSettleInactive,
        Self::SettleAfterDays,
        Self::HideWhitespace,
        Self::NewThreadWorkspace,
        Self::AddProjectStartDir,
        Self::DeleteConfirmation,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ProjectGrouping => "Project grouping",
            Self::AutoSettleFinished => "Auto-settle finished threads",
            Self::AutoSettleInactive => "Auto-settle inactive threads",
            Self::SettleAfterDays => "Days of inactivity before auto-settle",
            Self::HideWhitespace => "Hide whitespace changes in diff",
            Self::NewThreadWorkspace => "New threads default workspace",
            Self::AddProjectStartDir => "Add project starts in",
            Self::DeleteConfirmation => "Delete confirmation",
        }
    }

    /// The dim second line, when the label alone does not say what the key does.
    pub(crate) fn hint(self) -> Option<&'static str> {
        match self {
            Self::ProjectGrouping => Some("combine matching repos across hosts"),
            _ => None,
        }
    }

    /// `(section, key)` in the config file.
    pub(crate) fn config_key(self) -> (&'static str, &'static str) {
        match self {
            Self::ProjectGrouping => ("ui", "combine_repos_across_hosts"),
            Self::AutoSettleFinished => ("session", "auto_settle_finished"),
            Self::AutoSettleInactive => ("session", "auto_settle_inactive"),
            Self::SettleAfterDays => ("session", "settle_after_days"),
            Self::HideWhitespace => ("ui", "hide_whitespace_in_diff"),
            Self::NewThreadWorkspace => ("ui", "new_thread_workspace"),
            Self::AddProjectStartDir => ("ui", "add_project_start_dir"),
            Self::DeleteConfirmation => ("ui", "confirm_close"),
        }
    }

    /// The bracketed value on the right of the row.
    pub(crate) fn value(self, state: &AppState) -> String {
        let on_off = |value: bool| if value { "on" } else { "off" }.to_string();
        match self {
            Self::ProjectGrouping => on_off(state.combine_repos_across_hosts),
            Self::AutoSettleFinished => on_off(state.auto_settle_finished),
            Self::AutoSettleInactive => on_off(state.auto_settle_inactive),
            Self::SettleAfterDays => state.settle_after.as_secs().div_ceil(24 * 60 * 60).to_string(),
            Self::HideWhitespace => on_off(state.hide_whitespace_in_diff),
            Self::NewThreadWorkspace => state.new_thread_workspace.label().to_string(),
            Self::AddProjectStartDir => {
                if state.add_project_start_dir.trim().is_empty() {
                    "last used".to_string()
                } else {
                    state.add_project_start_dir.clone()
                }
            }
            Self::DeleteConfirmation => on_off(state.confirm_close),
        }
    }

    /// A path is not something a two-column list can edit; the row still names
    /// the key so the operator knows what to change.
    pub(crate) fn is_editable(self) -> bool {
        self != Self::AddProjectStartDir
    }
}

/// What pressing `enter` on `row` should write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConfigEdit {
    Bool {
        section: &'static str,
        key: &'static str,
        value: bool,
    },
    Integer {
        section: &'static str,
        key: &'static str,
        value: u64,
    },
    Text {
        section: &'static str,
        key: &'static str,
        value: String,
    },
}

/// The next value for `row`, or `None` when the row is not editable here.
pub(crate) fn cycle_general_row(state: &AppState, row: GeneralRow) -> Option<ConfigEdit> {
    let (section, key) = row.config_key();
    let toggle = |value: bool| {
        Some(ConfigEdit::Bool {
            section,
            key,
            value: !value,
        })
    };
    match row {
        GeneralRow::ProjectGrouping => toggle(state.combine_repos_across_hosts),
        GeneralRow::AutoSettleFinished => toggle(state.auto_settle_finished),
        GeneralRow::AutoSettleInactive => toggle(state.auto_settle_inactive),
        GeneralRow::HideWhitespace => toggle(state.hide_whitespace_in_diff),
        GeneralRow::DeleteConfirmation => toggle(state.confirm_close),
        GeneralRow::SettleAfterDays => {
            let current = state.settle_after.as_secs().div_ceil(24 * 60 * 60);
            let next = SETTLE_DAY_LADDER
                .iter()
                .copied()
                .find(|days| *days > current)
                .unwrap_or(SETTLE_DAY_LADDER[0]);
            Some(ConfigEdit::Integer {
                section,
                key,
                value: next,
            })
        }
        GeneralRow::NewThreadWorkspace => {
            let next = match state.new_thread_workspace {
                crate::config::NewThreadWorkspaceConfig::CurrentCheckout => {
                    crate::config::NewThreadWorkspaceConfig::NewWorktree
                }
                crate::config::NewThreadWorkspaceConfig::NewWorktree => {
                    crate::config::NewThreadWorkspaceConfig::CurrentCheckout
                }
            };
            Some(ConfigEdit::Text {
                section,
                key,
                value: next.as_str().to_string(),
            })
        }
        GeneralRow::AddProjectStartDir => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_general_row_names_a_distinct_config_key() {
        let mut keys = GeneralRow::ALL
            .iter()
            .map(|row| row.config_key())
            .collect::<Vec<_>>();
        let total = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), total);
        assert_eq!(total, 8);
    }

    #[test]
    fn general_row_values_read_live_state() {
        let mut state = AppState::test_new();
        assert_eq!(GeneralRow::ProjectGrouping.value(&state), "off");
        assert_eq!(GeneralRow::DeleteConfirmation.value(&state), "on");
        assert_eq!(
            GeneralRow::NewThreadWorkspace.value(&state),
            "current checkout"
        );
        assert_eq!(GeneralRow::AddProjectStartDir.value(&state), "last used");

        state.combine_repos_across_hosts = true;
        state.add_project_start_dir = "~/Repos".into();
        assert_eq!(GeneralRow::ProjectGrouping.value(&state), "on");
        assert_eq!(GeneralRow::AddProjectStartDir.value(&state), "~/Repos");
    }

    #[test]
    fn toggles_flip_and_the_day_ladder_wraps() {
        let mut state = AppState::test_new();
        assert_eq!(
            cycle_general_row(&state, GeneralRow::ProjectGrouping),
            Some(ConfigEdit::Bool {
                section: "ui",
                key: "combine_repos_across_hosts",
                value: true
            })
        );

        state.settle_after = std::time::Duration::from_secs(3 * 24 * 60 * 60);
        assert_eq!(
            cycle_general_row(&state, GeneralRow::SettleAfterDays),
            Some(ConfigEdit::Integer {
                section: "session",
                key: "settle_after_days",
                value: 7
            })
        );
        state.settle_after = std::time::Duration::from_secs(30 * 24 * 60 * 60);
        assert_eq!(
            cycle_general_row(&state, GeneralRow::SettleAfterDays),
            Some(ConfigEdit::Integer {
                section: "session",
                key: "settle_after_days",
                value: 1
            })
        );
    }

    #[test]
    fn the_start_directory_row_is_not_editable_in_place() {
        let state = AppState::test_new();
        assert!(!GeneralRow::AddProjectStartDir.is_editable());
        assert_eq!(cycle_general_row(&state, GeneralRow::AddProjectStartDir), None);
    }
}
