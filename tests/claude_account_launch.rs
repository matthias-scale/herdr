//! Synthetic child adapter proof; never reads real credentials or starts a real agent.
#![cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new(helper: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "herdr-token-fixture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join(".local/bin")).unwrap();
        fs::create_dir_all(root.join(".claude-profiles/scalable-so")).unwrap();
        fs::write(root.join(".claude-profiles/scalable-so/meta.env"), "").unwrap();
        // Capture layout is retained and must never be read, copied or modified.
        fs::write(
            root.join(".claude-profiles/scalable-so/credentials.json"),
            "dummy-capture-unchanged",
        )
        .unwrap();
        fs::create_dir_all(root.join(".claude")).unwrap();
        fs::write(root.join(".claude/global-marker"), "unchanged").unwrap();
        let fixture = Self { root };
        fixture.script(".local/bin/secret-get", helper);
        fixture.script(".local/bin/claude", r#"
if [ "$1" = --version ]; then printf '2.1.299 (Claude Code)\n'; exit 0; fi
[ "$CLAUDE_CODE_OAUTH_TOKEN" = dummy-subscription-value ] || exit 71
[ "$CLAUDE_PROFILE_ACCOUNT" = scalable-so ] || exit 72
[ "$CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST" = 1 ] || exit 73
[ -z "${ANTHROPIC_API_KEY+x}${ANTHROPIC_AUTH_TOKEN+x}${ANTHROPIC_BASE_URL+x}${CLAUDE_CONFIG_DIR+x}${BASH_ENV+x}" ] || exit 74
printf '%s\n' "$CLAUDE_PROFILE_ACCOUNT" "$@" > "$HOME/launch-record"
"#);
        fixture
    }
    fn script(&self, path: &str, body: &str) {
        let path = self.root.join(path);
        fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn command(&self) -> Command {
        self.request("scalable-so", "CLAUDE_T3_TOKEN_SCALABLE_SO", "claude")
    }
    fn request(&self, account: &str, key: &str, program: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_herdr"));
        command
            .env_clear()
            .env("HOME", &self.root)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join(".local/bin").display()),
            )
            .args([
                "--internal-claude-account",
                account,
                key,
                "--",
                program,
                "--model",
                "haiku",
                "--effort",
                "medium",
                "a prompt with ; $(injection) and 'quotes'",
            ]);
        command
    }
    fn assert_private(&self, output: &Output) {
        assert!(!String::from_utf8_lossy(&output.stdout).contains("dummy-subscription-value"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("dummy-subscription-value"));
        assert_eq!(
            fs::read_to_string(
                self.root
                    .join(".claude-profiles/scalable-so/credentials.json")
            )
            .unwrap(),
            "dummy-capture-unchanged"
        );
        assert_eq!(
            fs::read_to_string(self.root.join(".claude/global-marker")).unwrap(),
            "unchanged"
        );
        assert!(!self.root.join(".claude-profiles/active").exists());
        assert!(!self
            .root
            .join(".claude-profiles/scalable-so/.t3_token")
            .exists());
        assert!(!self.root.join(".claude/.credentials.json").exists());
        assert!(!self.root.join(".claude.json").exists());
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
const GOOD_HELPER: &str = r#"
[ "$#" = 1 ] && [ "$1" = CLAUDE_T3_TOKEN_SCALABLE_SO ] || exit 61
[ -z "${BASH_ENV+x}${ENV+x}${BASH_XTRACEFD+x}" ] || exit 62
case "$-" in *x*|*v*) exit 63;; esac
printf '%s' "$1" > "$HOME/key-request"
printf 'dummy-subscription-value\n'
"#;

#[test]
fn account_picker_claude_child_propagates_exact_key_account_and_original_argv_without_global_writes(
) {
    let fixture = Fixture::new(GOOD_HELPER);
    // Child removes tracing/startup hooks; no shell handles account or prompt text.
    let output = fixture
        .command()
        .env("SHELLOPTS", "xtrace")
        .env("BASH_ENV", "/nonexistent")
        .env("ENV", "/nonexistent")
        .env("BASHOPTS", "verbose")
        .env("BASH_XTRACEFD", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("key-request")).unwrap(),
        "CLAUDE_T3_TOKEN_SCALABLE_SO"
    );
    assert_eq!(fs::read_to_string(fixture.root.join("launch-record")).unwrap(),
        "scalable-so\n--model\nhaiku\n--effort\nmedium\na prompt with ; $(injection) and 'quotes'\n");
    fixture.assert_private(&output);
}

#[test]
fn account_picker_claude_child_missing_failed_empty_or_invalid_secret_never_starts_claude() {
    for helper in [
        "exit 1",
        "printf 'dummy-subscription-value' >&2; exit 1",
        "exit 0",
        "printf 'dummy-subscription-value\nsecond-line'",
        "printf 'dummy-subscription-value'; exit 1",
    ] {
        let fixture = Fixture::new(helper);
        fixture.script(
            ".local/bin/claude",
            "printf called > \"$HOME/claude-called\"; exit 70",
        );
        let output = fixture.command().output().unwrap();
        assert!(!output.status.success());
        assert!(
            !fixture.root.join("claude-called").exists(),
            "no fallback or even version probe after auth failure"
        );
        fixture.assert_private(&output);
    }
    let fixture = Fixture::new(GOOD_HELPER);
    fs::remove_file(fixture.root.join(".local/bin/secret-get")).unwrap();
    let output = fixture.command().output().unwrap();
    assert!(!output.status.success());
    assert!(!fixture.root.join("launch-record").exists());
    fixture.assert_private(&output);
}

#[test]
fn account_picker_claude_child_colliding_metadata_and_removed_metadata_fail_before_retrieval() {
    let fixture = Fixture::new(GOOD_HELPER);
    fs::create_dir_all(fixture.root.join(".claude-profiles/scalable_so")).unwrap();
    fs::write(
        fixture.root.join(".claude-profiles/scalable_so/meta.env"),
        "",
    )
    .unwrap();
    let output = fixture.command().output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("collides"));
    assert!(!fixture.root.join("key-request").exists());
    fs::remove_dir_all(fixture.root.join(".claude-profiles/scalable_so")).unwrap();
    fs::remove_file(fixture.root.join(".claude-profiles/scalable-so/meta.env")).unwrap();
    let output = fixture.command().output().unwrap();
    assert!(!output.status.success());
    assert!(!fixture.root.join("key-request").exists());
    fixture.assert_private(&output);
}

#[test]
fn account_picker_claude_child_refuses_ambient_auth_provider_config_and_old_cli_without_fallback() {
    for key in [
        "CLAUDE_CODE_OAUTH_TOKEN",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "CLAUDE_CONFIG_DIR",
        "SECRET_GET_COMMAND",
        "DOPPLER_TOKEN",
    ] {
        let fixture = Fixture::new(GOOD_HELPER);
        let output = fixture
            .command()
            .env(key, "dummy-conflict")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!fixture.root.join("key-request").exists());
        assert!(!fixture.root.join("launch-record").exists());
        fixture.assert_private(&output);
    }
    let fixture = Fixture::new(GOOD_HELPER);
    fixture.script(".local/bin/claude", "printf '2.1.298 (Claude Code)\\n'");
    let output = fixture.command().output().unwrap();
    assert!(!output.status.success());
    assert!(!fixture.root.join("launch-record").exists());
    fixture.assert_private(&output);
}

#[test]
fn account_picker_claude_child_rejects_injection_and_mismatched_names_or_wrappers_before_retrieval()
{
    let fixture = Fixture::new(GOOD_HELPER);
    for (account, key, program) in [
        (
            "scalable-so;touch hacked",
            "CLAUDE_T3_TOKEN_SCALABLE_SO",
            "claude",
        ),
        ("$(scalable-so)", "CLAUDE_T3_TOKEN_SCALABLE_SO", "claude"),
        ("../scalable-so", "CLAUDE_T3_TOKEN_SCALABLE_SO", "claude"),
        ("scalable-so", "CLAUDE_T3_TOKEN_MABLE", "claude"),
        ("scalable-so", "CLAUDE_T3_TOKEN_SCALABLE_SO", "bash"),
        ("unknown", "CLAUDE_T3_TOKEN_UNKNOWN", "claude"),
    ] {
        let output = fixture.request(account, key, program).output().unwrap();
        assert!(!output.status.success());
        assert!(!fixture.root.join("key-request").exists());
        assert!(!fixture.root.join("launch-record").exists());
        fixture.assert_private(&output);
    }
}
