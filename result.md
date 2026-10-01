# PR #505 real agent acceptance result

Host: `ubuntu-direct`  
Source: `59cf846abc062aa9fbf058fabaf34f62a4a7d47d`  
Binary SHA256: `0e1e39add5d4aa3db1609d066875bc6c93c1dffab3c3698698f291b95fda1022`

The requested eight case matrix was attempted on this host. No live agent case
could run to classification because the execution sandbox makes each agent's
required home state read-only. Claude cannot write its transcript under
`$HOME/.claude` (`EROFS`); Codex cannot open its subscription profile state
database. The exact `--only` run ended `matched=0/8`; these are recorded as
skipped, not passes.

| Case | Expected | Actual | Result |
|---|---|---|---|
| `real_claude_promised_idle` | promised work remains idle/stalled | skipped | Claude transcript path is read-only (`EROFS`) |
| `real_claude_waiting_on_you` | `waiting_human` | skipped | Claude transcript path is read-only (`EROFS`) |
| `real_claude_done_here` | done/idle | skipped | Claude transcript path is read-only (`EROFS`) |
| `real_claude_vendor_callback` | record observed classification | skipped | Claude transcript path is read-only (`EROFS`) |
| `real_codex_promised_idle` | promised work remains idle/stalled | skipped | Codex subscription profile state database is read-only |
| `real_codex_waiting_on_you` | `waiting_human` | skipped | Codex subscription profile state database is read-only |
| `real_codex_done_here` | done/idle | skipped | Codex subscription profile state database is read-only |
| `real_codex_vendor_callback` | record observed classification | skipped | Codex subscription profile state database is read-only |

The broader matrix also reported 42/53 matches. The three failing non-agent
fixtures were `promised_background_shell_past_deadline`,
`promised_background_agent_past_deadline`, and `p-cross-host`; the eight live
agent cases above were skipped. `incident_rearm` passed.

## Checks

- `python3 src/integration/assets/closing-block/test_closing_block.py` with
  `TMPDIR=/tmp`: 151 tests passed.
- `just test-one closing_block_authority_outranks_visible_idle_prompt_box`:
  passed.
- `just lint`: passed.
- `just check-parallel`: could not be completed in this execution session.
  Its first run failed because Cargo's default cache was read-only; a rerun
  using the writable Cargo cache stalled without returning a check summary.
- `just test`: started 7,564 Rust tests but did not return a summary in this
  execution session; the run was stopped. Focused Rust coverage and the
  touched integration asset suite passed separately.
