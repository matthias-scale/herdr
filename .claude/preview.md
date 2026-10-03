# Preview a Herdr PR on ub2

1. Build in the PR worktree: `cargo build --release`. Confirm `target/release/herdr --version` includes `+fork.<head sha12>`.
2. **Approve:** swapping the running server needs Matthias's per-change approval under `AGENTS.md` → “Fork-local operations.” Ask with an **Approve** item; stop here until approved.
3. **Swap:** back up `~/.local/bin/herdr` to `~/.local/bin/herdr.bak-<old sha>`, run `sudo -n chattr -i ~/.local/bin/herdr`, copy the build to `.new` and move it into place, then run `sudo -n chattr +i ~/.local/bin/herdr`.
4. **Hand off:** send `{"id":"preview:handoff","method":"server.live_handoff","params":{}}` plus a newline to `~/.config/herdr/herdr.sock`. Use the Unix-socket `json_request` pattern in `scripts/smoke_live_handoff_sessions.sh`.
5. **Verify:** `herdr status server` reports the new `+fork.<sha12>` version.
6. **Open:** run `open-url "$(herdr-link "$HERDR_PANE_ID")"` to focus the WezTerm tab. Do not use `wezterm cli`.

## What to test

- List the PR's acceptance items and how to check each one.
- Rollback: restore the saved `.bak-<old sha>` binary, run `sudo -n chattr +i ~/.local/bin/herdr`, then hand off again using step 4. Verify the old server version.
