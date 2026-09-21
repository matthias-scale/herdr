# libghostty-vt local patches

This file tracks intentional local changes applied on top of the vendored
`libghostty-vt` source. Remove a patch only when the vendored source commit
contains the upstream behavior and the listed verification still passes.

## 0001 default lib-vt panes to grapheme clustering

status: active

patch: `vendor/patches/libghostty-vt/0001-default-grapheme-cluster-mode.patch`

herdr issue: https://github.com/herdrdev/herdr/issues/243

upstream discussion: not opened; libghostty-vt currently exposes current mode mutation but no C API for configuring terminal default modes

upstream pr: not opened

vendored base: `c5a21edfcbc2d5b46540ad91b7980aca31f5f1f3`

local files:

- `vendor/libghostty-vt/src/terminal/c/terminal.zig`

reason: Herdr renders terminal cells directly and requires DEC private mode
2027 to store flags, ZWJ emoji, and other multi-codepoint grapheme clusters in
one cell. This patch makes clustering active for new terminals and keeps it as
the reset default so RIS (`ESC c`) does not disable it.

remove when: libghostty-vt exposes a C API for setting default mode 2027, or
upstream makes grapheme clustering the lib-vt default, and the reset-survival
regression passes without this patch.

verification:

```sh
cargo nextest run --locked grapheme_cluster_mode_is_default_and_survives_full_reset
cargo nextest run --locked grapheme_cluster_mode_renders_flag_emoji_in_single_wide_cell
cargo nextest run --locked grapheme_cluster_mode_renders_zwj_family_in_single_wide_cell
```

## 0002 expose modifyOtherKeys mode through terminal data

status: active

patch: `vendor/patches/libghostty-vt/0002-expose-modify-other-keys-mode.patch`

herdr issue: none; fixes the performance regression exposed by
https://github.com/herdrdev/herdr/pull/2303

upstream discussion: not opened

upstream pr: not opened

vendored base: `c5a21edfcbc2d5b46540ad91b7980aca31f5f1f3`

local files:

- `vendor/libghostty-vt/include/ghostty/vt/terminal.h`
- `vendor/libghostty-vt/src/terminal/c/terminal.zig`

reason: Herdr must know whether xterm modifyOtherKeys mode 2 is active to
request printable key releases from the outer terminal. The formatter API can
recover this fact only by formatting the active screen and scrollback. A typed
terminal-data query exposes the authoritative scalar without formatting or
allocation.

remove when: the vendored source exposes an equivalent scalar query for
modifyOtherKeys mode 2 and Herdr can use it without this patch.

verification:

```sh
cargo nextest run --locked modify_other_keys_query_tracks_mode_two
cargo nextest run --locked host_report_all_supplies_printable_releases_for_event_type_only_panes
python3 -m unittest scripts.test_vendor_libghostty_vt scripts.test_ui_hot_path_architecture
```

## 0003 expose parser-classified output

status: active

patch: `vendor/patches/libghostty-vt/0003-expose-parser-classified-output.patch`

herdr issue: MAT-160; fixes R360-36 and R360-37 in
https://github.com/matthias-scale/herdr/pull/360

upstream discussion: not opened

upstream pr: not opened

vendored base: `c5a21edfcbc2d5b46540ad91b7980aca31f5f1f3`

local files:

- `vendor/libghostty-vt/include/ghostty/vt/terminal.h`
- `vendor/libghostty-vt/src/terminal/c/terminal.zig`
- `vendor/libghostty-vt/src/terminal/osc.zig`
- `vendor/libghostty-vt/src/terminal/stream.zig`
- `vendor/libghostty-vt/src/terminal/stream_terminal.zig`

reason: Herdr extracts links as pane bytes arrive, including links that later
leave scrollback, but must not maintain a second VT visibility state machine.
This patch exposes printable text and parser-confirmed OSC 8 targets from the
same action stream that updates the terminal. OSC 8 capture is bounded at two
8 KiB components plus their delimiter, and the callback is disabled outside
live PTY writes.

remove when: the vendored source exposes equivalent post-parse text and OSC 8
events, including confirmed BEL, 8-bit ST, and split `ESC \\` termination, with
bounded 8 KiB targets and suppression for non-rendered status-display text, and
the Herdr visibility regressions pass without this patch.

verification:

```sh
cargo nextest run --locked 'test(c1_introducers_and_st_match_ghostty_rendered_text) | test(escape_followed_by_c0_matches_ghostty_rendered_text) | test(status_display_text_matches_ghostty_rendered_text)'
cargo nextest run --locked 'test(cancelled_osc_capture_matches_ghostty_visible_output) | test(osc8_target_bound_excludes_split_and_unsplit_st_bytes)'
python3 -m unittest scripts.test_vendor_libghostty_vt
```
