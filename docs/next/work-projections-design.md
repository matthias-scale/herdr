# Work projections — agent ↔ PR ↔ ticket views

Status: **design approved, unbuilt.** This is item 4 of the 2026-08-29 Whisper planning
session. Items 1–3 shipped (#101 spawn binding, #102 pane lifecycle, ghx #26 agent column),
which unblocks this one. Reconstructed 2026-08-31 from session transcripts; the original
discussion was never written to disk.

Source memos (Obsidian, currently EDEADLK-locked):
`2026-08-29-voice-memo-1633-task-views-vector-spaces.md`,
`2026-08-29-voice-memo-1635-agent-pr-ticket-tui.md`,
`2026-08-29-voice-memo-1641-ascii-ui-input-for-agents.md`.

## Premise

One set of work, several projections. Today Herdr splits work only by space, which the
memo calls "at fault". The ask: rotate the same work item between a PR view, a ticket
view, and an agent view, keeping the selection.

## Measured basis (scalablev2, 2026-08-29)

| Measure | Value | Consequence |
| --- | --- | --- |
| Open PRs | 76 (8 draft) | |
| PRs with **no** ticket in text | 23 (30%) | a `no ticket` bucket is mandatory, not decoration |
| Started tickets (In Progress + In Review) | 88 (72 + 16) | |
| Started tickets with a PR **attachment in Linear** | 80 of 88 | Linear attachments are the real edge |
| Open PRs with a ticket ID in branch/title/body | 53 of 76 | branch-name regex drops ~30% of the graph |
| Started tickets **with children** | 4 of 88 (4.5%) | hierarchy-first tree is the wrong default |
| Mean PRs/ticket, open → merged | 1.23 → 1.68 | 1:1 is a minority shape |
| GitHub `REVIEW_REQUIRED` vs Linear "In Review" | 58 vs 16 | Linear state cannot drive a review queue |

Caveat carried forward: GitHub marks a PR `REVIEW_REQUIRED` at open time when reviewers
are required, so part of the 44-PR gap is noise, not drift.

## Rotation mechanic

```
        ←                    ←                    ←
  ┌──────────┐        ┌──────────┐         ┌──────────┐
  │   PRs    │ ─────▶ │ tickets  │  ─────▶ │  agents  │ ─┐
  └──────────┘   →    └──────────┘    →    └──────────┘  │
       ▲                                                  │
       └──────────────────────────────────────────────────┘

  selection carries across:  #3223 ──▶ SCA-3104 ──▶ cc·opus·high
  ↑/↓  move within the current projection
  f    set the filter for this projection (repo for PRs, team for tickets)
```

`←/→` rotate the projection and keep the selected work item. `↑/↓` move within it.
Filters get their own key rather than an arrow — in the memo `←/→` meant a different
thing per view, the one part that would not survive contact with muscle memory.

## Option A — PR projection (build first)

```
 ub2 · cpu 12% · mem 41%                          working 6 · blocked 2 · idle 3 · done 4
┌─ spaces ───────┬─ work · PRs · scalablev2 · 76 open ────────────────────────────────────┐
│ ▾ scalablev2   │ ▸ 3226  ci(preview): allowlist SCA-2462     cx·sol·hi   SCA-2462    RR │
│    ● 3226 ci   │   3223  A+ editor: Studio parity (lane D)   cc·opus·hi  SCA-3104     — │
│    ○ 3223 aplus│   3214  feat(image): restore prompt access  —           3 tickets    RR│
│    ◐ 3212 route│   3212  fix: route baseline reconciliation  cx·luna·xh  4 tickets    RR│
│ ▸ herdr        │   3211  fix(onboarding): retry dispatch     —           no ticket     D│
│ ▸ fleet        │ ───────────────────────────────────────────────────────────────────────│
│                │   no ticket (23)                                                       │
│                │   3244  chore: update coverage badges       —                        RR│
│                │   3222  ci(preview): diagnostics            —                        RR│
└────────────────┴────────────────────────────────────────────────────────────────────────┘
 ←/→ view [PRs] tickets agents   ↑/↓ move   ⏎ attach agent   f filter repo   RR=review req
```

Row: PR number · title · owning agent · ticket / ticket-count / `no ticket` · review state.
Flat, grouped by repo. Chosen first because PRs are the population with the most complete
data, and the `no ticket (23)` bucket earns its keep on day one.

## Option D — Review queue (build second)

```
┌─ work · review queue · scalablev2 ──────────────────────────────────────────────────────┐
│  awaiting review · 58                        ticket says "In Review" · 16                │
│   3226  ci(preview): allowlist              SCA-2462   In Progress  ⚠ state drift        │
│   3214  feat(image): restore prompt access  3 tickets  In Progress  ⚠ state drift        │
│   3211  fix(onboarding): retry dispatch     no ticket  —            ⚠ untracked          │
│   2531  fix(SCA-2462): renewal reconcile    SCA-2462   In Review    ✓                    │
│ ────────────────────────────────────────────────────────────────────────────────────────│
│  drift 44 PRs awaiting review whose ticket is not In Review                             │
└─────────────────────────────────────────────────────────────────────────────────────────┘
```

Treat GitHub as the source of review truth and Linear state as a claim that can drift,
then surface the drift instead of picking a side. Narrow, and it attacks a gap nothing
currently shows.

## Option B — Ticket projection, PRs as children

```
┌─ work · tickets · SCA · 88 started ─────────────────────────────────────────────────────┐
│  In Progress · 72                                                                       │
│ ▾ SCA-3104  A+ editor port                     cc·opus·hi   3 PRs                       │
│      #3223 Studio parity (lane D)              cc·opus·hi   RR                          │
│      #3219 A+ sidebar (lane A)                 —             —                          │
│      #3078 port A+ editor                      —            RR                          │
│   SCA-2920  feature-flag kill-switch SLA       cx·sol·hi    4 PRs                        │
│   SCA-3121  non-square region windows          cc·opus·hi   1 PR   #3207                │
│   SCA-2412  Conversion IQ audit                —            no PR                        │
│  In Review · 16                                                                         │
│   SCA-2462  renewal reconciliation             cx·luna·xh   2 PRs  #3226 #2531           │
└─────────────────────────────────────────────────────────────────────────────────────────┘
 ←/→ view PRs [tickets] agents   ↑/↓ move   space expand   f filter team   ⏎ open agent
```

The expandable node is **ticket → PRs**, not parent → child tickets. Only 4 of 88 tickets
have children, so a hierarchy-first tree renders 84 flat rows and 4 interesting ones; the
multi-PR case is far more common and deserves the affordance.

## Option C — Agent projection

```
┌─ work · agents · all repos · 11 live ───────────────────────────────────────────────────┐
│ st   agent            repo          pane        PR      ticket      last                │
│ ●    cx·luna·xhigh    scalablev2    3:aplus     #3212   SCA-3129    12s   4 sub          │
│ ◐    cc·opus·high     scalablev2    1:editor    #3223   SCA-3104     3m                  │
│ ⏸    cx·sol·high      scalablev2    2:flags     #2946   SCA-2912     1m   blocked 4m     │
│ ●    cc·opus·high     herdr         1:workview  —       —           45s                  │
│ ○    cx·spark         fleet         2:sync      #218    AGF-77       8m   done           │
│ ────────────────────────────────────────────────────────────────────────────────────────│
│ unbound PRs 65 · unbound started tickets 8                                              │
└─────────────────────────────────────────────────────────────────────────────────────────┘
 ←/→ view PRs tickets [agents]   ↑/↓ move   ⏎ focus pane   b next blocked   ● working ⏸ blocked
```

The footer is the honest line. 76 open PRs, ~11 live agents — "one agent per PR" describes
the bound subset, and the UI must show the unbound remainder or it lies about coverage.
**Non-optional.**

## Settled constraints

1. **Selection preservation is the whole metaphor.** Without it these are three unrelated lists.
2. **Filters are configuration, not navigation.** Own key (`f`); binding to arrows collides with rotation.
3. **Ownership vs participation.** Ownership is exclusive and durable; participation (review, QA, rebase help) is many-to-many and transient.
4. **Active ownership, not exclusivity.** `pr_id` + `role` + `active` live on the *agent*, not the PR — shipped in #101. Falsifiers for exclusivity: `rev27-A`/`rev27-B` on one SHA, `pr33-repair` after `pr33-baseline`.
5. **The assignment gap.** An agent exists on a branch before a PR does. The work item is owned from spawn; the PR attaches later.
6. **Orphaning must render.** A dead owning agent leaves the PR visibly unowned, never a silent dead pointer.
7. **PRs attach only to leaves.** Parent tickets and orchestrators own no PRs, so ticket and PR views coincide at the leaves and diverge above. Collapsible expand belongs to ticket and agent views, not the PR view.
8. **Orchestrators are a relation, not a type.** `parent-of` is an edge on the agent: children → expandable, none → leaf. A distinct type costs a branch in every view forever for a 4.5% case.
9. **Named risk:** forcing Linear hierarchy to carry agent topology. A parent ticket later split across two orchestrators has no representation.
10. **Don't build a new unified TUI.** Join Herdr and ghx by data on `repo + PR number`.

## Where it plugs in

New main-content view beside Symphony and Loop-runs at `src/ui.rs:665-716`, bound to a
`work` keybind in the `symphony = prefix+shift+s` family.

**The blocker is data scope, not rendering.** `PaneWorkContext` (`src/work_context.rs`)
is derived per pane, so it only knows about work you have a pane open for. These views
need a repo-wide index built from **Linear attachments + the GitHub API** — branch-name
parsing misses 23 of 76 open PRs.

## Build order

| # | Unit | Note |
| --- | --- | --- |
| 1 | Repo-wide work index (Linear attachments + GitHub API) | the actual blocker; ships headless, testable without UI |
| 2 | Option A — PR projection | standing (a-rec) |
| 3 | Option D — review queue | discount `REVIEW_REQUIRED`-at-open noise |
| 4 | Options B and C | share the rotation mechanic; C's unbound footer is required |

## Prior art in ghx

ghx (`~/Repos/active/ghx`, Go + Bubble Tea) already owns the PR/blocker/CI half: SQLite
cache, stale-while-revalidate, `gh` GraphQL queue, `linearis` with a 10-min TTL, approve
and land actions, and since #26 an agent column plus `H` to attach to the owning pane.
Herdr owns the agent/pane/session half. Reverse direction — Herdr showing ghx's blocker
verdict — is deferred pending a `ghx --json` mode.

Architectural risk before more ghx surface lands: `internal/ui/ui.go` is 3,065 LOC, 31%
of that codebase.

## Method note

Recorded twice in this thread: **live probes catch what green tests cannot.** The
`linearis --fields` bug returned `{}` silently, and the first "needs me" spec returned
74 of 76 rows because Matthias authors nearly everything. Neither was reachable by unit test.
