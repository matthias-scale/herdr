# Compact sections sidebar visual check

1. Set `[ui.sidebar] layout = "sections"` and restart Herdr. The header is plain by default. Set `header = "sky"` separately to check that the sky band and wordmark still appear.
2. In dark and light themes, check Pinned, Active, Snoozed, and Settled in that order. Every shelf has a bare count such as `Pinned 1`. Each agent tab takes one row: colored status dot, readable title, idle age or `✓ Done` when applicable, and one machine icon. A blocked row has a red dot without a label or triangle; the Needs you strip uses dots too.
3. Check that tabs belonging to configured `[[projects]]` appear below their project name within each shelf. Unmatched tabs stay directly in the shelf. No project letter badges, branch line, PR line, or host name should appear on section rows.
4. At a normal sidebar width, use the row pin icon to pin and unpin a tab. Check the adjacent snooze and settle actions on a selected tab, then repeat on a different tab. At 18 columns, the dot and a useful title fragment take priority.
5. Check ub1, ub2, mbpro, and mbair machine icons in their distinct colors. With `ui.nerd_font = false`, check the `U` and `M` fallbacks. Override one host with `icon` under `[[remote.fleet.hosts]]` and check the custom mark.
6. Open the mobile switcher. Check the same single row, shelf counts, project groups, machine fallback, and pin target. Confirm scrolling and tab selection still align with the visible rows.
7. Open the `≡` checklist beside the bell and toggle its areas. Check Search, goto-key hint, View bar, host strip, and Notes at 80x24 and 18 columns; the tab list retains at least three rows.
