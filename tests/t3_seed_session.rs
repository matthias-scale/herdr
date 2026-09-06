#![cfg(unix)]

use std::process::Command;

#[test]
fn t3_seed_session_script() {
    let status = Command::new("python3")
        .args(["-m", "unittest", "scripts.test_t3_seed_session"])
        .status()
        .expect("run t3 seed session script tests");
    assert!(status.success(), "t3 seed session script tests failed");
}
