/// Update one config file without replacing a managed symlink or exposing a
/// partially written file.
pub(crate) fn update_file_at(
    path: &std::path::Path,
    description: &str,
    update: impl FnOnce(&str) -> String,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create config directory: {error}"))?;
    }
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(format!(
                "failed to read config before saving {description}: {error}"
            ));
        }
    };
    let new_content = update(&content);
    if new_content == content {
        return Ok(());
    }
    write_atomically(path, &new_content)
        .map_err(|error| format!("failed to save {description}: {error}"))
}

/// Write `content` to `path` without ever leaving a half-written config behind.
fn write_atomically(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;

    // Renaming over a symlink would detach the config from the dotfiles
    // checkout that owns it, so install the new inode at the resolved target.
    let resolved = crate::platform::resolve_write_target(path)?;
    let path = resolved.as_path();
    let directory = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(directory)?;
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config.toml".to_string());
    let temp_path = directory.join(format!(".{file_name}.{}.tmp", std::process::id()));

    let write = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temp_path)?;
        // Rename installs a new inode, so retain a restrictive target mode.
        if let Ok(metadata) = std::fs::metadata(path) {
            let _ = file.set_permissions(metadata.permissions());
        }
        file.write_all(content.as_bytes())?;
        file.sync_all()
    })();
    if let Err(error) = write {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error);
    }

    if let Err(error) = std::fs::rename(&temp_path, path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::update_file_at;

    fn scratch_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-config-write-{}",
            crate::config::test_unique_suffix()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn atomic_config_write_replaces_the_file_and_leaves_no_temp_behind() {
        let dir = scratch_dir();
        let path = dir.join("config.toml");
        std::fs::write(&path, "[ui]\nconfirm_close = true\n").expect("seed");

        update_file_at(&path, "test setting", |_| {
            "[ui]\nconfirm_close = false\n".into()
        })
        .expect("write");

        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "[ui]\nconfirm_close = false\n"
        );
        let leftovers = std::fs::read_dir(&dir)
            .expect("list")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A dotfiles-managed config must remain a symlink after an edit.
    #[cfg(unix)]
    #[test]
    fn atomic_config_write_follows_a_symlink_instead_of_replacing_it() {
        let dir = scratch_dir();
        let target = dir.join("generated.toml");
        let link = dir.join("config.toml");
        std::fs::write(&target, "[ui]\nconfirm_close = true\n").expect("seed");
        std::os::unix::fs::symlink("generated.toml", &link).expect("symlink");

        update_file_at(&link, "test setting", |_| {
            "[ui]\nconfirm_close = false\n".into()
        })
        .expect("write");

        assert!(
            std::fs::symlink_metadata(&link)
                .expect("link metadata")
                .file_type()
                .is_symlink(),
            "config write replaced the managed symlink with a regular file"
        );
        assert_eq!(
            std::fs::read_to_string(&target).expect("read target"),
            "[ui]\nconfirm_close = false\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn atomic_config_write_preserves_the_target_file_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch_dir();
        let target = dir.join("generated.toml");
        let link = dir.join("config.toml");
        std::fs::write(&target, "[ui]\nconfirm_close = true\n").expect("seed");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        std::os::unix::fs::symlink("generated.toml", &link).expect("symlink");

        update_file_at(&link, "test setting", |_| {
            "[ui]\nconfirm_close = false\n".into()
        })
        .expect("write");

        let mode = std::fs::metadata(&target)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "config write widened the target file mode");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn atomic_config_write_creates_a_missing_symlink_target_directory() {
        let dir = scratch_dir();
        let link = dir.join("config.toml");
        std::os::unix::fs::symlink("generated/config.toml", &link).expect("symlink");

        update_file_at(&link, "test setting", |_| {
            "[ui]\nconfirm_close = false\n".into()
        })
        .expect("write");

        assert_eq!(
            std::fs::read_to_string(dir.join("generated/config.toml")).expect("read target"),
            "[ui]\nconfirm_close = false\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_error_does_not_invoke_update_or_replace_the_path() {
        let dir = scratch_dir();
        let invoked = std::cell::Cell::new(false);

        let error = update_file_at(&dir, "test setting", |_| {
            invoked.set(true);
            "replacement".into()
        })
        .expect_err("reading a directory must fail");

        assert!(!invoked.get());
        assert!(error.starts_with("failed to read config before saving test setting:"));
        assert!(dir.is_dir());
        std::fs::remove_dir_all(&dir).ok();
    }
}
