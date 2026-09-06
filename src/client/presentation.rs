use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tracing::warn;

const PRESENTATION_FILE_NAME: &str = "client-presentation.json";

#[derive(Debug, Default, Deserialize, Serialize)]
struct ClientPresentationFile {
    #[serde(default)]
    dock_width: Option<u16>,
    #[serde(default)]
    sidebar_group_mode: Option<crate::app::state::SidebarGroupMode>,
    #[serde(default)]
    sidebar_work_filter: Option<crate::app::state::SidebarWorkFilter>,
}

fn presentation_path() -> PathBuf {
    crate::config::state_dir().join(PRESENTATION_FILE_NAME)
}

fn clamp_dock_width(width: u16) -> u16 {
    width.clamp(crate::ui::DOCK_MIN_WIDTH, crate::ui::DOCK_MAX_WIDTH)
}

pub(crate) fn load_dock_width() -> u16 {
    let path = presentation_path();
    match load_from_path(&path) {
        Ok(state) => state
            .dock_width
            .map(clamp_dock_width)
            .unwrap_or(crate::ui::DOCK_DEFAULT_WIDTH),
        Err(err) => {
            warn!(path = %path.display(), err = %err, "failed to load client presentation state");
            crate::ui::DOCK_DEFAULT_WIDTH
        }
    }
}

pub(crate) fn save_dock_width(width: u16) {
    let path = presentation_path();
    if let Err(err) = update_path(&path, |state| {
        state.dock_width = Some(clamp_dock_width(width));
    }) {
        warn!(path = %path.display(), err = %err, "failed to save client presentation state");
    }
}

pub(crate) fn load_sidebar_group_mode() -> crate::app::state::SidebarGroupMode {
    let path = presentation_path();
    match load_from_path(&path) {
        Ok(state) => state.sidebar_group_mode.unwrap_or_default(),
        Err(err) => {
            warn!(path = %path.display(), err = %err, "failed to load client presentation state");
            crate::app::state::SidebarGroupMode::default()
        }
    }
}

pub(crate) fn save_sidebar_group_mode(mode: crate::app::state::SidebarGroupMode) {
    let path = presentation_path();
    if let Err(err) = update_path(&path, |state| state.sidebar_group_mode = Some(mode)) {
        warn!(path = %path.display(), err = %err, "failed to save client presentation state");
    }
}

pub(crate) fn load_sidebar_work_filter() -> crate::app::state::SidebarWorkFilter {
    let path = presentation_path();
    match load_from_path(&path) {
        Ok(state) => state.sidebar_work_filter.unwrap_or_default(),
        Err(err) => {
            warn!(path = %path.display(), err = %err, "failed to load client presentation state");
            crate::app::state::SidebarWorkFilter::default()
        }
    }
}

pub(crate) fn save_sidebar_work_filter(filter: crate::app::state::SidebarWorkFilter) {
    let path = presentation_path();
    if let Err(err) = update_path(&path, |state| state.sidebar_work_filter = Some(filter)) {
        warn!(path = %path.display(), err = %err, "failed to save client presentation state");
    }
}

fn load_from_path(path: &Path) -> std::io::Result<ClientPresentationFile> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ClientPresentationFile::default());
        }
        Err(err) => return Err(err),
    };
    let state: ClientPresentationFile = serde_json::from_str(&content).map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid state: {err}"),
        )
    })?;
    Ok(state)
}

fn update_path(
    path: &Path,
    update: impl FnOnce(&mut ClientPresentationFile),
) -> std::io::Result<()> {
    let mut state = load_from_path(path)?;
    update(&mut state);
    save_to_path(path, &state)
}

fn save_to_path(path: &Path, state: &ClientPresentationFile) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("client presentation path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let json = serde_json::to_string_pretty(state)?;
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp_path = path.with_extension(format!("json.{}.{}.tmp", std::process::id(), unique));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)?;
    if let Err(err) = file
        .write_all(json.as_bytes())
        .and_then(|_| file.sync_all())
    {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }
    drop(file);
    if let Err(err) = crate::platform::replace_file_durably(&temp_path, path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "herdr-client-presentation-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    #[test]
    fn dock_width_round_trips_through_client_local_state() {
        let path = temp_path();
        update_path(&path, |state| state.dock_width = Some(25))
            .expect("save client presentation state");
        assert_eq!(
            load_from_path(&path)
                .expect("load client presentation state")
                .dock_width
                .map(clamp_dock_width),
            Some(25)
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn invalid_or_missing_dock_width_uses_default() {
        let path = temp_path();
        assert_eq!(
            load_from_path(&path).expect("missing state").dock_width,
            None,
        );
        std::fs::write(&path, "not json").expect("write invalid state");
        assert!(load_from_path(&path).is_err());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sidebar_work_filter_round_trips_with_the_group_mode() {
        let path = temp_path();
        update_path(&path, |state| {
            state.sidebar_group_mode = Some(crate::app::state::SidebarGroupMode::LinearTeam);
            state.sidebar_work_filter = Some(crate::app::state::SidebarWorkFilter {
                team: Some("SCA".into()),
                assignee: Some("matthias".into()),
            });
        })
        .expect("save sidebar work filter");
        let state = load_from_path(&path).expect("load client presentation state");
        assert_eq!(
            state.sidebar_group_mode,
            Some(crate::app::state::SidebarGroupMode::LinearTeam)
        );
        assert_eq!(
            state.sidebar_work_filter,
            Some(crate::app::state::SidebarWorkFilter {
                team: Some("SCA".into()),
                assignee: Some("matthias".into()),
            })
        );
        // A filter that narrows nothing round trips as the default, not as a
        // missing key that would resurrect an older narrowing.
        update_path(&path, |state| {
            state.sidebar_work_filter = Some(crate::app::state::SidebarWorkFilter::default());
        })
        .expect("clear sidebar work filter");
        assert_eq!(
            load_from_path(&path)
                .expect("load client presentation state")
                .sidebar_work_filter,
            Some(crate::app::state::SidebarWorkFilter::default())
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sidebar_group_mode_round_trips_without_erasing_dock_width() {
        let path = temp_path();
        update_path(&path, |state| state.dock_width = Some(25)).expect("save dock width");
        update_path(&path, |state| {
            state.sidebar_group_mode = Some(crate::app::state::SidebarGroupMode::RepoPr);
        })
        .expect("save sidebar group mode");
        let state = load_from_path(&path).expect("load client presentation state");
        assert_eq!(state.dock_width, Some(25));
        assert_eq!(
            state.sidebar_group_mode,
            Some(crate::app::state::SidebarGroupMode::RepoPr)
        );
        let _ = std::fs::remove_file(path);
    }
}
