# MAT-349 / #620 design-session handoff (from ubuntu-4c, 2026-10-16)
Owner: scalable-66 (to spawn design session after MAT-340 flicker fix merges; Matthias 1a).
- PR https://github.com/matthias-scale/herdr/pull/620 branch feat/mat-349-project-first-sidebar head 4e06ec137219fda1428f525599c761564a7dfc57. Fix r1 pushed; 1 CI check pending. Worker ra-261016-mat-349-fix-r1-ef38dac on ub2 (/home/ubuntu2/wt/ra-261016-mat-349-fix-r1-ef38dac) finishing.
- Mock: https://ubuntu-direct.tailc1d69d.ts.net/herdr-sidebar-grouping-proto/ "your direction: project first, working hidden". Briefs: /tmp/mat-herdr/brief-mat-349.md, brief-mat-349-r2.md.
- Review at 3ef6a7e in /tmp/mat-herdr/r620/. P1/P2 addressed, not re-reviewed. Open P3: dead "herdr" literal collapsed_sidebar_placement (sidebar.rs ~4124); "projects" literal mobile.rs:1139; bare numeric status indices fleet_grouping.rs; duplicate sections-only Tab focus route mouse.rs ~1682 not setting `selected`; dead blocked-section header code; fold keys as scattered string prefixes.
- Needed: full /review at new head, then Matthias hands-on UI QA (tested-by-human).
- QA: tab w98:t1P "QA #620", pane w98:p2H ub1, /var/tmp/qa620/bin/herdr at old 3ef6a7e, session repro-620-qa; needs rebuild. Cleanup owned by us.
- Overlap: PR #594 (MAT-333, ub2 session) conflicts in src/ui/sidebar.rs.
- Matthias wants a T3-style improved visual design/layout.

## Takeover 2026-10-16 23:45 (previous design session shag-…-92 hit Claude weekly limit at ~22:25, resets Oct 21)
- #620 head 9fd318a: restyle (approved prototype https://ubuntu-direct.tailc1d69d.ts.net/herdr-sidebar-349-restyle/) + main merged incl. MAT-351 flicker fix. CI 6/6 green. Rule: order rows by stable identity, never activity time.
- Previous session's next steps (not done): QA release build of 9fd318a at /var/tmp/qa620/bin/herdr (check it finished); throwaway before/after sessions via /var/tmp/qa620/start.sh, h.sh, sessions file (baseline = installed 094b201 ~/.local/bin/herdr; after = PR build), clearing HERDR_* socket env as in those scripts; capture both for the QA card; full /review at 9fd318a; then ask Matthias for hands-on UI QA (tested-by-human). Do not merge before his sign-off.
- Old repro-620-qa session already deleted. Clean up the qa620 throwaway sessions when done.
- Restyle worker run ra-261016-mat-349-impl-restyle-db6307d finished exit 0 (result via ra-result).

## Reconciliation checkpoint 2026-10-17 — exact head 88647dfa6503
- User explicitly requested finish captures + full review + hands-on QA card; do not merge.
- PR #620 now at 88647dfa65037972cd221927ea0e2e517bc4f2ba, CI6/6 green verified in-turn. Commits e674bdd remove host/sort toolbar; 88647dfa fixes fleet headers, working dimming, cog geometry and dark Herdr glyph. Stable identity comparator unchanged.
- Binary /var/tmp/qa620/round7/bin/herdr version0.9.1+fork.88647dfa6503, SHA256 f99aea5ee0e3f853dcd0573e40deb0d5335537b74217f03d71f60c80768cb2bd. Built after commit.
- 546/546 sidebar/icons pass. Full nextest8065/8067 initially; both transient failures pass individually. Standard retry Rust/lint/maintenance/integrations pass; local Windows cross-lint fails because Linux cache has no ghostty-vt-static.lib. Native Windows CI green.
- Nine actual120x40 capture artifacts,34columnsidebar crops under /var/tmp/qa620/round7. render_pair.py + render_capture.py embed actual captured cells with currentfonts. Dashboard https://ubuntu-direct.tailc1d69d.ts.net/herdr-sidebar-620-qa/ verified9embeddedPNGs. build-dashboard.py owns page content.
- Fresh hands-on session qa620-round7-hands-on: outer wA4:t6, pane wA4:p9; exactbinary, realHaiku+Luna, pinned/blocked/snoozed/settled fixtures. candidate-fixture.json current IDs. Originalcapturefixture archived captured-fixture.json. Do not reload config in freshsession.
- Confirmed pre-existing configreload issue: config/io.rs load_live_config_from_str omitsprojects/launch_profiles; app/mod.rs apply_live_config replacesprojects fromdefaultempty so pickerdisappears. after-plain capture shows bug; freshhands-on session startswith2projects and pickerintact. No sourcefix/changehead. Reviewowner says unchangedpaths thus notintroduceddiff; documentknownlimitation.
- Fullreview orchestration delegated to /root/design_history.3exactheadLunamediumlanescompleted; validatinginspections beforeaggregate. Wrongmainbranchattempts rejected. QAcarddraft /var/tmp/qa620/round7/qa-card.md ready but notyetposted. Finalize review thenpublishcard/updatepage.
- Local checkout branchfix/mat-349-round7-reconcile clean88647dfa. CurrentopenPRbranch feat/mat-349-project-first-sidebar samehead. No merge.
- Finalreview PASS posted https://github.com/matthias-scale/herdr/pull/620#issuecomment-6231480649;3lanes validated, state ~/.claude/state/review/matthias-scale-herdr-620-88647dfa65037972cd221927ea0e2e517bc4f2ba.json. Advisory P2 stale status/activitysort wording in configuration.mdx55 remainsforlatercorrection. Do not conflatePASSwithmergeapproval.
- QAcard posted https://github.com/matthias-scale/herdr/pull/620#issuecomment-6231483296. Ninecapturepagepublished finalwithCI+review+knownlimitations. Hands-on link wA4:t6. User asked don'tmerge. Await humanstylingQA only; no worker remainsrunning.
