//! Build identity helpers.

pub const BASE_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn channel() -> &'static str {
    non_empty(option_env!("HERDR_BUILD_CHANNEL")).unwrap_or("stable")
}

pub fn build_id() -> Option<&'static str> {
    non_empty(option_env!("HERDR_BUILD_ID"))
}

pub fn version() -> String {
    fork_version(
        BASE_VERSION,
        env!("HERDR_BUILD_GIT_SHA"),
        env!("HERDR_BUILD_DIRTY") == "1",
    )
}

pub fn is_preview() -> bool {
    channel() == "preview"
}

fn non_empty(value: Option<&'static str>) -> Option<&'static str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

fn fork_version(base_version: &str, git_sha: &str, dirty: bool) -> String {
    let dirty_suffix = if dirty { "-dirty" } else { "" };
    format!("{base_version}+fork.{git_sha}{dirty_suffix}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn fork_version_formats_git_sha() {
        assert_eq!(
            super::fork_version("0.8.0", "0123456789ab", false),
            "0.8.0+fork.0123456789ab"
        );
    }

    #[test]
    fn fork_version_marks_dirty_worktree() {
        assert_eq!(
            super::fork_version("0.8.0", "0123456789ab", true),
            "0.8.0+fork.0123456789ab-dirty"
        );
    }

    #[test]
    fn fork_version_formats_unknown_git_sha() {
        assert_eq!(
            super::fork_version("0.8.0", "unknown", false),
            "0.8.0+fork.unknown"
        );
    }
}
