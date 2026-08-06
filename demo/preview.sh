#!/usr/bin/env bash
# Isolated preview of PR #31, with a live pane you can watch change state.
#
# Runs against its own config/state/socket so it can never attach to the live
# herdr server. Both guards are required: unsetting inherited HERDR_* vars alone
# is not enough, because herdr also autodetects a running server.
set -euo pipefail

DEMO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$HOME/.herdr-cb-preview"
BIN="$DEMO_DIR/../target/release/herdr"

for var in $(env | grep -o '^HERDR_[A-Z_]*' || true); do unset "$var"; done

export XDG_CONFIG_HOME="$ROOT/config"
export XDG_STATE_HOME="$ROOT/state"
export HERDR_SOCKET_PATH="$ROOT/config/herdr/herdr.sock"
export HERDR_CLIENT_SOCKET_PATH="$ROOT/config/herdr/herdr-client.sock"
mkdir -p "$ROOT/config/herdr" "$XDG_STATE_HOME"

# Mirror the real config so keybindings (prefix = ctrl+a) match the live session.
[ -f "$HOME/.config/herdr/config.toml" ] &&
  cp -L "$HOME/.config/herdr/config.toml" "$ROOT/config/herdr/config.toml"

cat <<BANNER
herdr preview — PR #31, turn-end status contract

  binary : $BIN
  socket : $HERDR_SOCKET_PATH

Watch the pane's status dot and label in the sidebar, then from any other
terminal drive it through the three channels:

  export HERDR_SOCKET_PATH=$HERDR_SOCKET_PATH
  export HERDR_ENV=1 HERDR_PANE_ID=w1:p1

  # nothing for you, but 3 agents still running  -> working / "3 agents"
  $DEMO_DIR/herdr-status --agent claude --blocking 0 --agents 3

  # two human gates                              -> blocked / "gate x2"
  $DEMO_DIR/herdr-status --agent claude --blocking 2 --agents 2 \\
      --gate "Merge #30 into fork/pr-base"

  # genuinely done                               -> idle
  $DEMO_DIR/herdr-status --agent claude --blocking 0 --agents 0

  # durable JSON mirror
  cat $ROOT/state/herdr/agent-status/w1_p1.json

BANNER

exec "$BIN" "$@"
