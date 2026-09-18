#!/usr/bin/env python3
"""Install the closing-block adapters as one verified, recoverable bundle."""

from __future__ import annotations

import argparse
import errno
import hashlib
import json
import os
import re
import shutil
import sys
import tempfile
import time
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path


RUNTIME_FILES = (
    "closing_block.py",
    "herdr_status.py",
    "herdr-closing-block.py",
    "herdr-codex-notify.py",
    "herdr-question-gate.py",
)
VERSION_RE = re.compile(r"^# HERDR_INTEGRATION_VERSION=(?P<version>\d+)$", re.MULTILINE)


class BundleValidationError(ValueError):
    pass


def bundle_manifest(source_dir: Path) -> dict[str, dict[str, str | int]]:
    manifest: dict[str, dict[str, str | int]] = {}
    versions: set[int] = set()
    for name in RUNTIME_FILES:
        path = source_dir / name
        if not path.is_file() or path.is_symlink():
            raise BundleValidationError(f"missing regular source file: {name}")
        try:
            data = path.read_bytes()
            text = data.decode("utf-8")
            compile(text, str(path), "exec")
        except (OSError, UnicodeDecodeError, SyntaxError) as error:
            raise BundleValidationError(f"invalid source file {name}: {error}") from error
        matches = VERSION_RE.findall(text)
        if len(matches) != 1:
            raise BundleValidationError(f"expected one integration version in {name}")
        version = int(matches[0])
        versions.add(version)
        manifest[name] = {
            "version": version,
            "sha256": hashlib.sha256(data).hexdigest(),
        }
    if len(versions) != 1:
        raise BundleValidationError("runtime module integration versions do not match")
    return manifest


def _verify_bundle(
    bundle_dir: Path, expected: dict[str, dict[str, str | int]]
) -> None:
    actual = bundle_manifest(bundle_dir)
    if actual != expected:
        raise BundleValidationError("installed bundle hash or version mismatch")


def _backup_path(target: Path) -> Path:
    timestamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    candidate = target.parent / f"{target.name}.backup-{timestamp}"
    suffix = 1
    while candidate.exists():
        candidate = target.parent / f"{target.name}.backup-{timestamp}-{suffix}"
        suffix += 1
    return candidate


def _validate_target(target: Path) -> None:
    if target.is_symlink():
        raise BundleValidationError(f"install target must not be a symlink: {target}")
    if target.exists() and not target.is_dir():
        raise BundleValidationError(f"install target is not a directory: {target}")


@contextmanager
def _exclusive_install_lock(target: Path):
    # Keep this file in place so every installer locks the same inode.
    lock_path = target.parent / f".{target.name}.install.lock"
    with lock_path.open("a+b") as lock:
        if os.name == "nt":
            import msvcrt

            lock.seek(0, os.SEEK_END)
            if lock.tell() == 0:
                lock.write(b"\0")
                lock.flush()
            lock.seek(0)
            while True:
                try:
                    msvcrt.locking(lock.fileno(), msvcrt.LK_NBLCK, 1)
                    break
                except OSError as error:
                    if error.errno not in (errno.EACCES, errno.EAGAIN, errno.EDEADLK):
                        raise
                    time.sleep(0.05)
            try:
                yield
            finally:
                lock.seek(0)
                msvcrt.locking(lock.fileno(), msvcrt.LK_UNLCK, 1)
        else:
            import fcntl

            fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
            try:
                yield
            finally:
                fcntl.flock(lock.fileno(), fcntl.LOCK_UN)


def _written_file_identity(path: Path) -> tuple[int, int, bytes]:
    stat = path.stat()
    return stat.st_dev, stat.st_ino, path.read_bytes()


def _replace_runtime_files(
    source: Path, target: Path, written: list[tuple[Path, tuple[int, int, bytes]]]
) -> None:
    target.mkdir(parents=True, exist_ok=True)
    for name in RUNTIME_FILES:
        destination = target / name
        os.replace(source / name, destination)
        written.append((destination, _written_file_identity(destination)))


def _restore_runtime_files(backup: Path, target: Path) -> None:
    restore = Path(tempfile.mkdtemp(prefix=f".{target.name}.restore-", dir=target.parent))
    try:
        for name in RUNTIME_FILES:
            source = backup / name
            destination = target / name
            if not source.exists() and not source.is_symlink():
                if destination.exists() or destination.is_symlink():
                    destination.unlink()
                continue
            if source.is_symlink():
                os.symlink(os.readlink(source), restore / name)
            else:
                shutil.copy2(source, restore / name)
            os.replace(restore / name, destination)
    finally:
        shutil.rmtree(restore, ignore_errors=True)


def _rollback_runtime_files(
    backup: Path | None,
    target: Path,
    written: list[tuple[Path, tuple[int, int, bytes]]],
) -> None:
    if backup and backup.exists():
        _restore_runtime_files(backup, target)
        return
    for path, identity in written:
        try:
            if _written_file_identity(path) == identity:
                path.unlink()
        except FileNotFoundError:
            pass
    try:
        target.rmdir()
    except FileNotFoundError:
        pass
    except OSError as error:
        if error.errno not in (errno.ENOTEMPTY, errno.EEXIST):
            raise


def install_bundle(source_dir: Path, target: Path, *, dry_run: bool) -> dict:
    source_dir = Path(source_dir).resolve()
    target = Path(target).expanduser().absolute()
    expected = bundle_manifest(source_dir)
    if dry_run:
        _validate_target(target)
        backup = _backup_path(target) if target.exists() else None
        result = {
            "mode": "dry-run",
            "target": str(target),
            "backup": backup.name if backup else None,
            "files": expected,
        }
        return result

    target.parent.mkdir(parents=True, exist_ok=True)
    with _exclusive_install_lock(target):
        _validate_target(target)
        backup = _backup_path(target) if target.exists() else None
        result = {
            "mode": "installed",
            "target": str(target),
            "backup": backup.name if backup else None,
            "files": expected,
        }
        stage = Path(
            tempfile.mkdtemp(prefix=f".{target.name}.stage-", dir=target.parent)
        )
        try:
            if target.exists():
                shutil.copytree(target, stage, dirs_exist_ok=True, symlinks=True)
            for name in RUNTIME_FILES:
                staged_file = stage / name
                if staged_file.exists() or staged_file.is_symlink():
                    staged_file.unlink()
                shutil.copy2(source_dir / name, staged_file)
            _verify_bundle(stage, expected)

            if backup:
                shutil.copytree(target, backup, symlinks=True)
            written: list[tuple[Path, tuple[int, int, bytes]]] = []
            try:
                _replace_runtime_files(stage, target, written)
            except OSError:
                _rollback_runtime_files(backup, target, written)
                raise

            try:
                _verify_bundle(target, expected)
            except (BundleValidationError, OSError):
                _rollback_runtime_files(backup, target, written)
                raise
            return result
        finally:
            if stage.exists():
                shutil.rmtree(stage)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--target",
        type=Path,
        default=Path.home() / ".local" / "share" / "herdr-closing-block",
    )
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args(argv)
    try:
        result = install_bundle(Path(__file__).parent, args.target, dry_run=args.dry_run)
    except (BundleValidationError, OSError) as error:
        print(f"closing-block install failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
