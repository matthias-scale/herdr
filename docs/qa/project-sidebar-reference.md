# Native project sidebar QA

Status: live QA pending. Static tests cover the projection, input and display
contracts; no live-agent or screenshot evidence has been captured yet.

## Setup

- Candidate binary: `$CARGO_TARGET_DIR/release/herdr`, built with
  `cargo build --locked --release` from the candidate SHA. Do not install it.
- Run it only in an isolated, named throwaway session with its own config and
  state directories. Keep installed Herdr and existing sessions untouched.
- Open two projects (for example `owner/alpha` and `owner/beta`) with at least
  two tabs each, including one Claude Haiku pane and one Codex Luna pane. Use the
  Current layout with the Repo view.

## Actions and expected results

1. Expand and collapse each project. Expected: tabs list directly under the
   project header with no branch subheaders. A collapsed project keeps exactly
   its activated tab. A collapsed project with no activated tab shows no rows.
2. Hover, then keyboard-focus, a collapsed project header. Expected: the
   status glyph in the header slot swaps to the chevron, and the title does not
   move.
3. Focus sidebar Search and type one character. Expected: the agent finder
   opens with that character in the query. Repeat with a paste. Clear the
   query, type again, and confirm results update after the debounce.
4. Activate a finder result from another collapsed project. Expected: that
   project now shows only the newly activated tab. If the previous project is
   collapsed, it no longer shows its old tab.
5. Open the view dropdown and toggle Provider icon and Machine name
   independently through all four states, by both keyboard and mouse. Expected:
   rows and finder results show or hide the provider token and machine
   character. `[ui.sidebar] show_provider_icons` / `show_machine_names` in the
   isolated config match the toggles, and survive a restart.
6. Capture desktop and mobile at 18 columns and at a normal width, in dark and
   light themes, with Nerd Font on and off.

## Evidence

Record the candidate SHA, binary path, agent CLI versions, exact actions, and
captured screens here after live QA. Static tests do not establish agent
lifecycle behavior.
