# libghostty-vt local patches

This file tracks intentional local changes applied on top of the vendored
`libghostty-vt` source. Remove a patch only when the vendored source commit
contains the upstream behavior and the listed verification still passes.

## 0003 expose parser-classified output

status: active

patch: `vendor/patches/libghostty-vt/0003-expose-parser-classified-output.patch`

herdr issue: MAT-160; fixes R360-36 and R360-37 in
https://github.com/matthias-scale/herdr/pull/360

upstream discussion: not opened

upstream pr: not opened

vendored base: `44f2a44df7e8c4a0c6df3f7d872ef3d7ead88e51`

local files:

- `vendor/libghostty-vt/include/ghostty/vt/terminal.h`
- `vendor/libghostty-vt/src/terminal/Terminal.zig`
- `vendor/libghostty-vt/src/terminal/c/terminal.zig`
- `vendor/libghostty-vt/src/terminal/osc.zig`
- `vendor/libghostty-vt/src/terminal/osc/parsers/hyperlink.zig`
- `vendor/libghostty-vt/src/terminal/stream.zig`
- `vendor/libghostty-vt/src/terminal/stream_terminal.zig`

reason: Herdr extracts links as pane bytes arrive, including links that later
leave scrollback, but must not maintain a second VT visibility state machine.
This patch exposes printable text and parser-confirmed OSC 8 targets from the
same action stream that updates the terminal. OSC 8 capture is bounded at two
8 KiB components plus their delimiter. Classification runs only while an
embedder callback is installed; clearing the callback clears the handler
effect, so a pane without link extraction pays nothing on the parse path.
Printable callbacks use the terminal's own print result so discarded
codepoints are not exposed and charset-mapped glyphs are reported as Ghostty
rendered them. Actions that cannot move rendered text keep two printed runs
joined; every other action separates them, including cursor motion, erasure,
and scrolling. An unclassified new upstream action separates by default.

The refreshed base treats high bytes as UTF-8 payload inside DCS and OSC, and
the parser-classified output callbacks preserve that behavior.

remove when: the vendored source exposes equivalent post-parse text and OSC 8
events, including confirmed BEL and split `ESC \\` termination, with
bounded 8 KiB targets and suppression for non-rendered status-display text and
discarded codepoints, charset-mapped glyph reporting, and continuity across
non-rendering controls, screen-motion separation, and the Herdr visibility
regressions pass without this patch.

verification:

```sh
just test-one matches_ghostty_rendered_text
just test-one c1_introducers_and_st_match_ghostty_rendered_text
just test-one osc_title_with_star_continuation_byte_stays_hidden
just test-one osc_title_with_umlaut_continuation_byte_stays_hidden
just test-one cancelled_osc_capture_matches_ghostty_visible_output
just test-one osc8_target_bound_excludes_split_and_unsplit_st_bytes
just test-one screen_motion_between_runs_does_not_join_one_url
just test-one parsed_output_stops_when_disabled
python3 -m unittest scripts.test_vendor_libghostty_vt
```
