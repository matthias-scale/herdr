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
}

fn presentation_path() -> PathBuf {
    crate::config::state_dir().join(PRESENTATION_FILE_NAME)
}

fn clamp_dock_width(width: u16) -> u16 {
    width.clamp(crate::ui::DOCK_MIN_WIDTH, crate::ui::DOCK_MAX_WIDTH)
}

pub(crate) fn load_dock_width() -> u16 {
    let path = presentation_path();
    match load_dock_width_from_path(&path) {
        Ok(Some(width)) => clamp_dock_width(width),
        Ok(None) => crate::ui::DOCK_DEFAULT_WIDTH,
        Err(err) => {
            warn!(path = %path.display(), err = %err, "failed to load client presentation state");
            crate::ui::DOCK_DEFAULT_WIDTH
        }
    }
}

pub(crate) fn save_dock_width(width: u16) {
    let path = presentation_path();
    if let Err(err) = save_dock_width_to_path(&path, clamp_dock_width(width)) {
        warn!(path = %path.display(), err = %err, "failed to save client presentation state");
    }
}

fn load_dock_width_from_path(path: &Path) -> std::io::Result<Option<u16>> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let state: ClientPresentationFile = serde_json::from_str(&content).map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid state: {err}"),
        )
    })?;
    Ok(state.dock_width)
}

fn save_dock_width_to_path(path: &Path, width: u16) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("client presentation path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let json = serde_json::to_string_pretty(&ClientPresentationFile {
        dock_width: Some(width),
    })?;
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
        save_dock_width_to_path(&path, 25).expect("save client presentation state");
        assert_eq!(
            load_dock_width_from_path(&path)
                .expect("load client presentation state")
                .map(clamp_dock_width),
            Some(25)
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn invalid_or_missing_dock_width_uses_default() {
        let path = temp_path();
        assert_eq!(
            load_dock_width_from_path(&path).expect("missing state"),
            None
        );
        std::fs::write(&path, "not json").expect("write invalid state");
        assert!(load_dock_width_from_path(&path).is_err());
        let _ = std::fs::remove_file(path);
    }
}
