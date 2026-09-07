#!/usr/bin/env python3
"""Run independent repository checks concurrently with compact output."""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


@dataclass(frozen=True)
class Check:
    name: str
    command: tuple[str, ...]
    environment: tuple[tuple[str, str], ...] = ()


CHECKS = (
    Check(
        "rust",
        (
            "cargo",
            "nextest",
            "run",
            "--locked",
            "--status-level",
            "fail",
            "--final-status-level",
            "fail",
            "--failure-output",
            "final",
            "--success-output",
            "never",
        ),
    ),
    Check(
        "windows",
        ("just", "windows-lint"),
        (("LIBGHOSTTY_VT_SIMD", "false"),),
    ),
    Check(
        "maintenance",
        (
            "python3",
            "-m",
            "unittest",
            "scripts.test_agent_detection_manifest_check",
            "scripts.test_changelog",
            "scripts.test_config_reference_check",
            "scripts.test_docs_translation_parity",
            "scripts.test_hermes_integration_asset",
            "scripts.test_package_windows_conpty",
            "scripts.test_parallel_check",
            "scripts.test_pr_gate_workflow",
            "scripts.test_preview",
            "scripts.test_qa_preview_adapter",
            "scripts.test_vendor_libghostty_vt",
            "scripts.test_vendor_portable_pty",
            "scripts.test_watch_pr_checks",
        ),
    ),
    Check("integrations", ("just", "integration-assets-test")),
    Check("marketplace", ("just", "plugin-marketplace-test")),
)

WINDOWS_TEST_CHECK = (
    "cargo",
    "check",
    "--bin",
    "herdr",
    "--tests",
    "--target",
    "x86_64-pc-windows-msvc",
    "--message-format",
    "json",
)


def command_output(command: tuple[str, ...]) -> str | None:
    result = subprocess.run(
        command,
        text=True,
        capture_output=True,
        check=False,
    )
    if result.returncode:
        return None
    return result.stdout.strip()


def tool_env() -> dict[str, str]:
    env = os.environ.copy()

    rustup_cargo = command_output(("rustup", "which", "cargo"))
    if rustup_cargo:
        env["PATH"] = (
            f"{Path(rustup_cargo).parent}{os.pathsep}{env.get('PATH', '')}"
        )

    zig = env.get("ZIG")
    if not zig or not Path(zig).is_file():
        mise = shutil.which("mise")
        mise_root = (
            command_output((mise, "where", "zig@0.15.2")) if mise else None
        )
        if mise_root:
            candidate = Path(mise_root) / "zig"
            if candidate.is_file():
                env["ZIG"] = str(candidate)
    return env


def run_check(
    check: Check, log_dir: Path, env: dict[str, str]
) -> tuple[Check, int, float, Path]:
    log_path = log_dir / f"{check.name}.log"
    started = time.monotonic()
    check_env = env.copy()
    check_env.update(check.environment)
    with log_path.open("wb") as log:
        result = subprocess.run(
            check.command,
            cwd=ROOT,
            stdout=log,
            stderr=subprocess.STDOUT,
            env=check_env,
            check=False,
        )
    return check, result.returncode, time.monotonic() - started, log_path


def compiler_error_locations_under_src(output: str) -> list[str]:
    """Return primary source locations from Cargo compiler error records."""
    src_root = (ROOT / "src").resolve()
    locations: set[str] = set()
    for line in output.splitlines():
        try:
            record = json.loads(line)
        except (json.JSONDecodeError, TypeError):
            continue
        if record.get("reason") != "compiler-message":
            continue
        message = record.get("message", {})
        if message.get("level") != "error":
            continue
        for span in message.get("spans", []):
            if not span.get("is_primary"):
                continue
            file_name = span.get("file_name")
            if not isinstance(file_name, str):
                continue
            path = Path(file_name)
            resolved = (path if path.is_absolute() else ROOT / path).resolve()
            if not resolved.is_relative_to(src_root):
                continue
            relative = resolved.relative_to(ROOT)
            locations.add(
                f"{relative}:{span.get('line_start', '?')}:{span.get('column_start', '?')}"
            )
    return sorted(locations)


def run_windows_check(
    check: Check, log_dir: Path, env: dict[str, str]
) -> tuple[Check, int, float, Path]:
    """Keep clippy strict, then ignore Unix-only integration-test errors."""
    log_path = log_dir / f"{check.name}.log"
    started = time.monotonic()
    check_env = env.copy()
    check_env.update(check.environment)
    with log_path.open("wb") as log:
        lint = subprocess.run(
            check.command,
            cwd=ROOT,
            stdout=log,
            stderr=subprocess.STDOUT,
            env=check_env,
            check=False,
        )
        if lint.returncode:
            return check, lint.returncode, time.monotonic() - started, log_path
        build = subprocess.run(
            WINDOWS_TEST_CHECK,
            cwd=ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            env=check_env,
            check=False,
        )
        output = build.stdout.decode(errors="replace")
        log.write(build.stdout)
        locations = compiler_error_locations_under_src(output)
        if locations:
            log.write(b"\nWindows test build found compiler errors under src/:\n")
            for location in locations:
                log.write(f"{location}\n".encode())
    return check, int(bool(locations)), time.monotonic() - started, log_path


def run_independent_check(
    check: Check, log_dir: Path, env: dict[str, str]
) -> tuple[Check, int, float, Path]:
    if check.name == "windows":
        return run_windows_check(check, log_dir, env)
    return run_check(check, log_dir, env)


def platform_filter(value: str) -> str:
    if value != "platform":
        return value
    if sys.platform == "darwin":
        return "not binary(live_handoff)"
    return "all()"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--filter",
        default="platform",
        help="nextest filter expression, or 'platform' for the CI default",
    )
    args = parser.parse_args()

    env = tool_env()
    with tempfile.TemporaryDirectory(prefix="herdr-check-") as directory:
        log_dir = Path(directory)
        lint_result = run_check(
            Check("lint", ("just", "lint")), log_dir, env
        )
        lint_check, lint_code, lint_elapsed, lint_log = lint_result
        lint_state = "pass" if lint_code == 0 else "FAIL"
        print(f"{lint_state:4}  {lint_check.name:12} {lint_elapsed:7.2f}s")
        if lint_code:
            print(lint_log.read_text(errors="replace").rstrip())
            return lint_code

        rust_filter = platform_filter(args.filter)
        checks = (
            Check(
                CHECKS[0].name,
                CHECKS[0].command + ("-E", rust_filter),
            ),
            *CHECKS[1:],
        )

        rust_result = run_check(checks[0], log_dir, env)
        rust_check, rust_code, rust_elapsed, rust_log = rust_result
        rust_state = "pass" if rust_code == 0 else "FAIL"
        print(f"{rust_state:4}  {rust_check.name:12} {rust_elapsed:7.2f}s")
        if rust_code:
            print(rust_log.read_text(errors="replace").rstrip())
            return rust_code

        independent_checks = checks[1:]
        with concurrent.futures.ThreadPoolExecutor(
            max_workers=len(independent_checks)
        ) as executor:
            results = list(
                executor.map(
                    lambda check: run_independent_check(check, log_dir, env),
                    independent_checks,
                )
            )

        failed = False
        for check, returncode, elapsed, log_path in results:
            state = "pass" if returncode == 0 else "FAIL"
            print(f"{state:4}  {check.name:12} {elapsed:7.2f}s")
            if returncode:
                failed = True
                print(log_path.read_text(errors="replace").rstrip())

    if not failed:
        print(
            "docs reminder: update release docs when behavior is user-facing."
        )
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
