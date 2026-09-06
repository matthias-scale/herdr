#!/usr/bin/env python3
"""Generate deterministic-shape Claude and Codex usage logs under an isolated HOME."""

import datetime
import json
import pathlib
import sys
import time


def stamp(epoch: int) -> str:
    return datetime.datetime.fromtimestamp(epoch, datetime.UTC).isoformat().replace("+00:00", "Z")


def write_jsonl(path: pathlib.Path, records: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("".join(json.dumps(record) + "\n" for record in records), encoding="utf-8")


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit("usage: generate_usage.py HOME")
    home = pathlib.Path(sys.argv[1])
    now = int(time.time())
    claude = []
    codex = []
    for day in range(30):
        timestamp = now - day * 86400 - 3600
        claude.append(
            {
                "type": "assistant",
                "timestamp": stamp(timestamp),
                "message": {
                    "model": "claude-sonnet-5" if day % 3 else "claude-opus-5",
                    "usage": {
                        "input_tokens": 120_000 + day * 1_000,
                        "output_tokens": 18_000 + day * 100,
                        "cache_creation_input_tokens": 30_000,
                        "cache_read_input_tokens": 480_000 - day * 2_000,
                    },
                },
            }
        )
        turn_id = f"turn-{day}"
        codex.extend(
            [
                {
                    "type": "turn_context",
                    "timestamp": stamp(timestamp + 900),
                    "payload": {
                        "turn_id": turn_id,
                        "model": "gpt-5.6-sol" if day % 4 else "gpt-5.6-luna",
                    },
                },
                {
                    "type": "token_usage_record",
                    "timestamp": stamp(timestamp + 960),
                    "payload": {
                        "turn_id": turn_id,
                        "session_id": "codex-fixture",
                        "usage": {
                            "input_tokens": 360_000 + day * 2_000,
                            "cached_input_tokens": 280_000,
                            "cache_write_input_tokens": 10_000,
                            "output_tokens": 24_000,
                            "reasoning_output_tokens": 5_000,
                            "total_tokens": 384_000 + day * 2_000,
                        },
                    },
                },
            ]
        )
    claude.append(
        {
            "type": "assistant",
            "timestamp": stamp(now - 600),
            "message": {
                "model": "local-unpriced-model",
                "usage": {"input_tokens": 20_000, "output_tokens": 2_000},
            },
        }
    )
    write_jsonl(home / ".claude/projects/fixture/claude-fixture.jsonl", claude)
    write_jsonl(home / ".codex/sessions/fixture/codex-fixture.jsonl", codex)
    config = home / "config/herdr-dev/config.toml"
    config.parent.mkdir(parents=True, exist_ok=True)
    config.write_text("onboarding = false\n", encoding="utf-8")


if __name__ == "__main__":
    main()
