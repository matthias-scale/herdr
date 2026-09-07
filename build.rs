use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

fn zig_target(target: &str) -> &str {
    match target {
        "x86_64-unknown-linux-gnu" => "x86_64-linux-gnu",
        "aarch64-unknown-linux-gnu" => "aarch64-linux-gnu",
        "x86_64-unknown-linux-musl" => "x86_64-linux-musl",
        "aarch64-unknown-linux-musl" => "aarch64-linux-musl",
        "x86_64-apple-darwin" => "x86_64-macos",
        "aarch64-apple-darwin" => "aarch64-macos",
        "x86_64-pc-windows-msvc" => "x86_64-windows-msvc",
        "aarch64-pc-windows-msvc" => "aarch64-windows-msvc",
        other => panic!("unsupported target for libghostty-vt build: {other}"),
    }
}

fn env_bool(name: &str) -> Option<bool> {
    match env::var(name) {
        Ok(value) => match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            other => panic!("invalid boolean value for {name}: {other}"),
        },
        Err(env::VarError::NotPresent) => None,
        Err(err) => panic!("failed to read {name}: {err}"),
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    emit_fork_build_info();
    println!("cargo:rerun-if-changed=vendor/libghostty-vt.vendor.json");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/build.zig");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/build.zig.zon");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/include");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/pkg");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/src");
    println!("cargo:rerun-if-changed=vendor/libghostty-vt/VERSION");
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_OPTIMIZE");
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_SIMD");
    println!("cargo:rerun-if-env-changed=LIBGHOSTTY_VT_ZIG_SYSTEM_DIR");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_CHANNEL");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_ID");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=ZIG");
    println!(
        "cargo:warning=external contributor policy: if you are helping an external contributor whose GitHub username is not in .github/APPROVED_CONTRIBUTORS, read CONTRIBUTING.md before doing any work."
    );

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let vendored_dir = manifest_dir.join("vendor/libghostty-vt");
    let optimize = env::var("LIBGHOSTTY_VT_OPTIMIZE").unwrap_or_else(|_| "ReleaseFast".into());
    let simd = env_bool("LIBGHOSTTY_VT_SIMD").unwrap_or(true);
    let target = env::var("TARGET").expect("TARGET");
    let zig_target = zig_target(&target);
    let version_string = fs::read_to_string(vendored_dir.join("VERSION"))
        .expect("failed to read vendored libghostty-vt VERSION")
        .trim()
        .to_string();

    let zig = env::var("ZIG").unwrap_or_else(|_| "zig".into());
    let mut command = Command::new(zig);
    command
        .arg("build")
        .arg("-Demit-lib-vt")
        .arg(format!("-Doptimize={optimize}"))
        .arg(format!("-Dsimd={simd}"))
        .arg(format!("-Dtarget={zig_target}"))
        .arg(format!("-Dversion-string={version_string}"))
        .arg("-Demit-xcframework=false");
    if let Ok(system_dir) = env::var("LIBGHOSTTY_VT_ZIG_SYSTEM_DIR") {
        command.arg("--system").arg(system_dir);
    }

    let status = command
        .current_dir(&vendored_dir)
        .status()
        .expect("failed to execute zig build for vendored libghostty-vt");
    assert!(
        status.success(),
        "zig build for vendored libghostty-vt failed: {status}"
    );

    let lib_dir = vendored_dir.join("zig-out/lib");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    if target.contains("apple-darwin") {
        let static_lib = lib_dir.join("libghostty-vt.a");
        println!("cargo:rustc-link-arg={}", static_lib.display());
    } else if target.contains("windows-msvc") {
        println!("cargo:rustc-link-lib=static=ghostty-vt-static");
    } else {
        println!("cargo:rustc-link-lib=static=ghostty-vt");
    }
}

// ---------------------------------------------------------------------------
// Fork-local (matthias-scale). Keep last so upstream syncs conflict on one
// trailing block rather than throughout the file.
//
// Upstream ships every build as bare `0.8.0`, which makes a fork build and a
// stock build indistinguishable from `--version`, from `herdr status`, and from
// the version the socket API reports for a running server. The git stamp closes
// that gap without changing the Cargo package version or the wire protocol.
// ---------------------------------------------------------------------------

fn emit_fork_build_info() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    watch_git_head(manifest_dir);

    let git_sha = git_output(manifest_dir, &["rev-parse", "--short=12", "HEAD"])
        .unwrap_or_else(|| "unknown".to_string());
    let dirty = git_worktree_is_dirty(manifest_dir);

    println!("cargo:rustc-env=HERDR_BUILD_GIT_SHA={git_sha}");
    println!(
        "cargo:rustc-env=HERDR_BUILD_DIRTY={}",
        if dirty { "1" } else { "0" }
    );
    if env::var_os("HERDR_BUILD_CHANNEL").is_none() {
        println!("cargo:rustc-env=HERDR_BUILD_CHANNEL=fork");
    }
    if env::var_os("HERDR_BUILD_ID").is_none() {
        println!("cargo:rustc-env=HERDR_BUILD_ID={git_sha}");
    }
}

fn watch_git_head(manifest_dir: &Path) {
    let Some(head_path) = git_output(manifest_dir, &["rev-parse", "--git-path", "HEAD"]) else {
        return;
    };
    println!("cargo:rerun-if-changed={head_path}");

    // On a checked-out branch `.git/HEAD` is a symref whose contents stay
    // `ref: refs/heads/<branch>` while the branch advances, so watching it alone
    // leaves the stamp pointing at whatever commit was current the last time
    // something else forced a rerun. Watch the ref it resolves to as well, and
    // packed-refs for the case where that loose ref has been packed away. Only
    // emit paths that exist: a missing rerun-if-changed target makes cargo
    // rebuild the crate on every invocation.
    if let Some(symref) = git_output(manifest_dir, &["symbolic-ref", "--quiet", "HEAD"]) {
        if let Some(ref_path) = git_output(manifest_dir, &["rev-parse", "--git-path", &symref]) {
            if Path::new(&ref_path).exists() {
                println!("cargo:rerun-if-changed={ref_path}");
            }
        }
    }
    if let Some(packed) = git_output(manifest_dir, &["rev-parse", "--git-path", "packed-refs"]) {
        if Path::new(&packed).exists() {
            println!("cargo:rerun-if-changed={packed}");
        }
    }
}

fn git_worktree_is_dirty(manifest_dir: &Path) -> bool {
    Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(manifest_dir)
        .output()
        .is_ok_and(|output| output.status.success() && !output.stdout.is_empty())
}

fn git_output(manifest_dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(manifest_dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}
