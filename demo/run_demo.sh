#!/usr/bin/env bash
# End-to-end demo against an ISOLATED herdr server.
#
# Never touches the live daily-driver server: every HERDR_* var is unset first
# and the socket/config/state roots are redirected, matching the guard used by
# ~/.herdr-test/launch.sh (herdr also autodetects a running server, so unsetting
# the inherited vars alone is not enough).
set -euo pipefail

DEMO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="${DEMO_ROOT:-/tmp/herdr-closing-block-demo}"
BIN="${HERDR_DEMO_BIN:-$HOME/.local/bin/herdr}"

for var in $(env | grep -o '^HERDR_[A-Z_]*' || true); do unset "$var"; done

export XDG_CONFIG_HOME="$ROOT/config"
export XDG_STATE_HOME="$ROOT/state"
export HERDR_SOCKET_PATH="$ROOT/config/herdr/herdr.sock"
export HERDR_CLIENT_SOCKET_PATH="$ROOT/config/herdr/herdr-client.sock"
mkdir -p "$ROOT/config/herdr" "$XDG_STATE_HOME"

echo "binary : $BIN"
echo "socket : $HERDR_SOCKET_PATH"

cleanup() { "$BIN" server stop >/dev/null 2>&1 || true; }
trap cleanup EXIT

"$BIN" server >"$ROOT/server.log" 2>&1 &
for _ in $(seq 1 50); do [ -S "$HERDR_SOCKET_PATH" ] && break; sleep 0.2; done
[ -S "$HERDR_SOCKET_PATH" ] || { echo "server never came up"; cat "$ROOT/server.log"; exit 1; }

python3 "$DEMO_DIR/drive.py"
