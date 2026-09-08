use std::time::Duration;

use super::harness::*;

#[test]
fn theme_cli_sets_and_reports_server_owned_appearance() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let herdr = spawn_herdr_with_config(
        &config_home,
        &runtime_dir,
        &socket_path,
        None,
        "onboarding = false\n[theme]\nauto_switch = true\n",
    );
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let initial = run_cli_json(&socket_path, &["theme", "status", "--json"]);
    assert_eq!(initial["result"]["host_reported"], serde_json::Value::Null);
    assert_eq!(initial["result"]["override"], "auto");
    assert_eq!(initial["result"]["effective_appearance"], "dark");
    assert_eq!(initial["result"]["theme_name"], "catppuccin");

    let set = run_cli(&socket_path, &["theme", "set", "light"]);
    assert!(
        set.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&set.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&set.stdout).trim(),
        "theme appearance override set to light"
    );

    let updated = run_cli_json(&socket_path, &["theme", "status", "--json"]);
    assert_eq!(updated["result"]["host_reported"], serde_json::Value::Null);
    assert_eq!(updated["result"]["override"], "light");
    assert_eq!(updated["result"]["effective_appearance"], "light");
    assert_eq!(updated["result"]["theme_name"], "catppuccin-latte");

    cleanup_spawned_herdr(herdr, base);
}
