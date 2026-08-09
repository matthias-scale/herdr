#!/usr/bin/env bash
# HERDR_INTEGRATION_VERSION=2
# Codex allows exactly one `notify` program, and this machine already uses it
# for Computer Use. Chain instead of replacing: the original runs first and
# unchanged, then the herdr closing-block handler gets the same payload.
# Neither may wedge a turn, so both are best-effort.
SKY="/Users/matthiasschedel/.codex-profiles/scalable-so-2/computer-use/Codex Computer Use.app/Contents/SharedSupport/SkyComputerUseClient.app/Contents/MacOS/SkyComputerUseClient"
[ -x "$SKY" ] && "$SKY" turn-ended "$@" >/dev/null 2>&1 &
python3 "$HOME/.local/share/herdr-closing-block/herdr-codex-notify.py" "$@" >/dev/null 2>&1 || true
exit 0
