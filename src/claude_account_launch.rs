//! Private child-only subscription authentication. No secret-bearing state or wire fields.
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub(crate) const CHILD_COMMAND: &str = "--internal-claude-account";
const VERIFIED_KEYS: [&str; 5] = [
    "CLAUDE_T3_TOKEN_MABLE",
    "CLAUDE_T3_TOKEN_SCALABLEHQ",
    "CLAUDE_T3_TOKEN_SCALABLE_SO",
    "CLAUDE_T3_TOKEN_SCALABLE_SO_V2",
    "CLAUDE_T3_TOKEN_TEAM_SCALABLE_SO",
];
/// Presence is enough to refuse; neither composer nor adapter inspects values.
pub(crate) const AUTH_OVERRIDES: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_CUSTOM_HEADERS",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
    "CLAUDE_CODE_OAUTH_SCOPES",
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_CODE_API_KEY_HELPER_TTL_MS",
    "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST",
    "ANTHROPIC_FOUNDRY_API_KEY",
    "ANTHROPIC_FOUNDRY_BASE_URL",
    "ANTHROPIC_FOUNDRY_RESOURCE",
];
/// These variables control the canonical helper's lookup source. Presence alone
/// makes an explicit account launch unsafe, even though the child scrubs them.
pub(crate) const HELPER_CONTROL_PREFIXES: &[&str] = &["SECRET_GET_", "DOPPLER_"];
const SHELL_INPUTS: &[&str] = &[
    "BASH_ENV",
    "ENV",
    "SHELLOPTS",
    "BASHOPTS",
    "PS4",
    "BASH_XTRACEFD",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeTokenBinding {
    pub(crate) account: String,
    pub(crate) secret_name: String,
}

pub(crate) fn token_key(account: &str) -> Result<String, String> {
    if account.is_empty()
        || account.len() > 64
        || !account.as_bytes()[0].is_ascii_alphanumeric()
        || !account
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err("Claude unavailable: invalid account identifier".into());
    }
    let normalized = account.to_ascii_uppercase().replace('-', "_");
    let key = format!("CLAUDE_T3_TOKEN_{normalized}");
    if !VERIFIED_KEYS.contains(&key.as_str()) {
        return Err("Claude unavailable: no verified subscription-token mapping".into());
    }
    Ok(key)
}

/// Inspect names and metadata existence only. Archives never become config roots.
/// Scan all owners before selecting any key; an incomplete scan authorizes none.
pub(crate) fn discover_bindings(root: &Path) -> Vec<ClaudeTokenBinding> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut owners = Vec::new();
    for (count, entry) in entries.enumerate() {
        if count >= 4096 {
            return Vec::new();
        }
        let Ok(entry) = entry else { return Vec::new() };
        let Ok(kind) = entry.file_type() else {
            return Vec::new();
        };
        if !kind.is_dir() {
            continue;
        }
        let marker = match fs::symlink_metadata(entry.path().join("meta.env")) {
            Ok(marker) => marker,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return Vec::new(),
        };
        if !marker.file_type().is_file() {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        // Unknown keys can still collide with a known account under normalization.
        owners.push(id);
    }
    bindings_for_owners(&owners)
}

fn bindings_for_owners(owners: &[String]) -> Vec<ClaudeTokenBinding> {
    let mut counts = std::collections::HashMap::<String, usize>::new();
    for owner in owners {
        let key = format!(
            "CLAUDE_T3_TOKEN_{}",
            owner.to_ascii_uppercase().replace('-', "_")
        );
        *counts.entry(key).or_default() += 1;
    }
    let mut bindings = owners
        .iter()
        .filter_map(|account| {
            let key = token_key(account).ok()?;
            (counts.get(&key) == Some(&1)).then(|| ClaudeTokenBinding {
                account: account.clone(),
                secret_name: key,
            })
        })
        .collect::<Vec<_>>();
    bindings.sort_by(|a, b| a.account.cmp(&b.account));
    bindings
}

pub(crate) fn local_paths() -> Option<(PathBuf, PathBuf)> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let home = PathBuf::from(home);
    Some((
        home.join(".claude-profiles"),
        home.join(".local/bin/secret-get"),
    ))
}

pub(crate) fn compose_argv(
    binding: &ClaudeTokenBinding,
    argv: Vec<String>,
) -> Result<Vec<String>, String> {
    validate_request(binding, &argv)?;
    if !crate::platform::claude_account_launch_supported() {
        return Err(
            "Claude unavailable: subscription-token launcher unsupported on this platform".into(),
        );
    }
    let executable = crate::platform::launch_executable()
        .map_err(|_| "Claude unavailable: cannot resolve native Herdr launcher")?;
    let mut wrapped = vec![
        executable.to_string_lossy().into_owned(),
        CHILD_COMMAND.into(),
        binding.account.clone(),
        binding.secret_name.clone(),
        "--".into(),
    ];
    wrapped.extend(argv);
    Ok(wrapped)
}

fn validate_request(binding: &ClaudeTokenBinding, argv: &[String]) -> Result<(), String> {
    if token_key(&binding.account)? != binding.secret_name {
        return Err("Claude unavailable: subscription-token identity mismatch".into());
    }
    if argv.first().map(String::as_str) != Some("claude") {
        return Err("Claude unavailable: only the native Claude command is supported".into());
    }
    Ok(())
}

/// Called before session parsing, logging or server startup. Never print child output.
pub(crate) fn run_child(args: &[String]) -> i32 {
    let result: Result<i32, String> = (|| {
        if !crate::platform::claude_account_launch_supported() {
            return Err(
                "Claude unavailable: subscription-token launcher unsupported on this platform"
                    .into(),
            );
        }
        let [account, key, separator, rest @ ..] = args else {
            return Err("Claude unavailable: invalid private launch request".into());
        };
        if separator != "--" {
            return Err("Claude unavailable: invalid private launch request".into());
        }
        let binding = ClaudeTokenBinding {
            account: account.clone(),
            secret_name: key.clone(),
        };
        validate_request(&binding, rest)?;
        let (root, helper) =
            local_paths().ok_or("Claude unavailable: local account metadata missing")?;
        if !discover_bindings(&root).contains(&binding) {
            return Err(
                "Claude unavailable: account metadata missing or normalized token key collides"
                    .into(),
            );
        }
        if AUTH_OVERRIDES
            .iter()
            .any(|key| std::env::var_os(key).is_some())
            || std::env::vars_os().any(|(name, _)| {
                name.to_str().is_some_and(|name| {
                    HELPER_CONTROL_PREFIXES
                        .iter()
                        .any(|prefix| name.starts_with(prefix))
                })
            })
        {
            return Err(
                "Claude unavailable: ambient authentication/provider/config override".into(),
            );
        }
        // The bare name lookup is canonical: no global-current helper or alternate account.
        let mut command = prepare_child(&binding, rest, &helper, Duration::from_secs(30))?;
        crate::platform::exec_private_account_command(&mut command)
            .map_err(|_| "Claude unavailable: native command could not start".into())
    })();
    match result {
        Ok(code) => code,
        Err(reason) => {
            eprintln!("{reason}");
            1
        }
    }
}

fn clean_environment(command: &mut Command) {
    for key in AUTH_OVERRIDES.iter().chain(SHELL_INPUTS) {
        command.env_remove(key);
    }
}

/// Private Command contains the token only after it leaves all Herdr state/IPC paths.
/// Never log/debug this value. Caller immediately execs it in the adapter process.
fn prepare_child(
    binding: &ClaudeTokenBinding,
    argv: &[String],
    helper: &Path,
    timeout: Duration,
) -> Result<Command, String> {
    validate_request(binding, argv)?;
    let mut retrieve = Command::new(helper);
    retrieve.arg(&binding.secret_name);
    clean_environment(&mut retrieve);
    let bytes = bounded_output(retrieve, timeout, 16384).map_err(|_| {
        "Claude unavailable: subscription token missing, unreadable or retrieval failed".to_string()
    })?;
    let token = String::from_utf8(bytes)
        .map_err(|_| "Claude unavailable: invalid subscription-token response".to_string())?;
    let token = token.trim_end_matches(['\r', '\n']);
    if token.is_empty() || token.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("Claude unavailable: empty or invalid subscription-token response".into());
    }
    let mut version = Command::new(&argv[0]);
    version.arg("--version");
    clean_environment(&mut version);
    let output = bounded_output(version, timeout, 256)
        .map_err(|_| "Claude unavailable: native version check failed".to_string())?;
    if !supported_cli_version(&output) {
        return Err(
            "Claude unavailable: native CLI 2.1.299 or newer required for verified token routing"
                .into(),
        );
    }
    let mut child = Command::new(&argv[0]);
    child.args(&argv[1..]);
    clean_environment(&mut child);
    child.env("CLAUDE_CODE_OAUTH_TOKEN", token);
    child.env("CLAUDE_PROFILE_ACCOUNT", &binding.account);
    // CLI contract prevents settings-file API keys/endpoints from replacing the subscription.
    child.env("CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST", "1");
    Ok(child)
}

fn supported_cli_version(output: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(output) else {
        return false;
    };
    let Some(version) = text.split_whitespace().next() else {
        return false;
    };
    let numbers = version
        .split('.')
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>();
    matches!(numbers.as_deref(), Ok([major, minor, patch]) if (*major, *minor, *patch) >= (2, 1, 299))
}

/// Bounded reader and deadline, with sanitized errors/stderr. No output reaches logs.
fn bounded_output(mut command: Command, timeout: Duration, limit: usize) -> io::Result<Vec<u8>> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    crate::platform::configure_private_account_helper(&mut command);
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("private helper pipe missing"))?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut stdout = stdout;
        let mut buffer = [0; 4096];
        let result = loop {
            match stdout.read(&mut buffer) {
                Ok(0) => break Ok(bytes),
                Ok(count) => {
                    if bytes.len().saturating_add(count) > limit {
                        break Err(io::Error::other("private helper output limit"));
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                }
                Err(_) => break Err(io::Error::other("private helper output unreadable")),
            }
        };
        let _ = sender.send(result);
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => return Err(io::Error::other("private helper failed")),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                crate::platform::terminate_private_account_helper(&mut child);
                return Err(io::Error::other("private helper timed out"));
            }
        }
    }
    receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| io::Error::other("private helper pipe timed out"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_picker_claude_metadata_is_identity_only_and_all_collision_owners_are_checked() {
        let root = std::env::temp_dir().join(format!(
            "herdr-token-metadata-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for id in [
            "scalable-so",
            "scalable_so",
            "mable",
            "unsupported",
            "scalablehq",
        ] {
            fs::create_dir_all(root.join(id)).unwrap();
            if id != "scalablehq" {
                fs::write(root.join(id).join("meta.env"), "").unwrap();
            }
            fs::write(root.join(id).join("credentials.json"), "").unwrap();
        }
        assert_eq!(
            discover_bindings(&root),
            vec![ClaudeTokenBinding {
                account: "mable".into(),
                secret_name: "CLAUDE_T3_TOKEN_MABLE".into(),
            }],
            "credentials/cache/native-looking files are never token authority"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn account_picker_claude_token_identity_rejects_injection_unknown_keys_and_collisions() {
        for id in [
            "",
            "../mable",
            "mable;echo bad",
            "$(mable)",
            "mable\n",
            "mable/email",
            "unverified",
        ] {
            assert!(token_key(id).is_err());
        }
        assert_eq!(
            token_key("scalable-so").unwrap(),
            "CLAUDE_T3_TOKEN_SCALABLE_SO"
        );
        let bindings =
            bindings_for_owners(&["scalable-so".into(), "scalable_so".into(), "mable".into()]);
        assert_eq!(
            bindings,
            vec![ClaudeTokenBinding {
                account: "mable".into(),
                secret_name: "CLAUDE_T3_TOKEN_MABLE".into()
            }]
        );
        let binding = ClaudeTokenBinding {
            account: "mable".into(),
            secret_name: "CLAUDE_T3_TOKEN_SCALABLE_SO".into(),
        };
        assert!(validate_request(&binding, &["claude".into()]).is_err());
        let binding = ClaudeTokenBinding {
            account: "mable".into(),
            secret_name: token_key("mable").unwrap(),
        };
        assert!(
            validate_request(&binding, &["bash".into(), "-c".into(), "claude".into()]).is_err()
        );
    }

    #[test]
    fn account_picker_claude_version_and_platform_contracts_fail_closed() {
        for version in [
            "2.1.298 (Claude Code)",
            "1.99.999",
            "unknown",
            "2.1.x",
            "2.1.299-extra",
        ] {
            assert!(!supported_cli_version(version.as_bytes()));
        }
        for version in ["2.1.299 (Claude Code)", "2.2.0", "3.0.0"] {
            assert!(supported_cli_version(version.as_bytes()));
        }
        assert_eq!(
            crate::platform::claude_account_launch_supported(),
            cfg!(unix)
        );
    }

    #[cfg(unix)]
    #[test]
    fn account_picker_claude_private_helper_bounds_output_and_time_without_printing() {
        let mut large = Command::new("/bin/sh");
        large.args(["-c", "printf 'dummy-only'; printf 'dummy-stderr' >&2"]);
        assert!(bounded_output(large, Duration::from_secs(2), 3).is_err());
        let mut slow = Command::new("/bin/sh");
        slow.args(["-c", "sleep 5"]);
        let start = Instant::now();
        assert!(bounded_output(slow, Duration::from_millis(100), 64).is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
