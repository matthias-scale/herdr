# Sidebar sections visual check

1. Set `[ui.sidebar] layout = "sections"` and `header = "sky"`, then restart Herdr.
2. Compare the expanded sidebar with `shot-d-normal.png` and `shot-d-light.png` from `/home/ubuntu2/.agents/briefs/mat-207/` in dark and light themes.
3. Check the four shelf order, two-line cards, project badges, status colors, and narrow-width field removal. Click both card lines and confirm they select the same tab. Counts read `Snoozed 2` and `Settled 14`, without parentheses.
4. Confirm AC9: open the `≡` checklist beside the bell and toggle its nine areas by mouse and keys. Open and operate it from Home, Inbox, Work, Usage, and the dock; modal owners keep control.
5. Confirm AC10: `header = "sky"` shows the sky band and `herdr` wordmark, with Search and the goto-key hint below and the View bar beneath them. With `header = "plain"`, the sky band and wordmark disappear and the remaining header controls reflow to the plain layout.
6. Check the host strip above the footer: the local host comes first and the help hint sits at the right. At 18 columns, the local host remains visible (truncated if needed) and the help hint yields first. Check the sidebar still keeps three tab-list rows at 80x24 with Notes, Pomodoro, and Hosts enabled.
7. Set `ui.nerd_font = false` and check the card and shelf icons.
8. Hide Notes, invoke the Toggle Notes action, then show Notes again. Confirm the focused pane still receives typing and the Notes editor does not.

## Folders

1. Create folders in Pinned, Active, Snoozed, and Settled with each shelf's `+ Folder` control. From a tab menu, choose **Move to folder** → **New…** and name a folder.
2. Confirm each folder row shows its disclosure arrow, folder icon, name, and member-tab count without parentheses. Its cards are indented two columns and keep the two-column card layout.
3. Collapse and reopen a folder. A collapsed folder occupies one row and its count remains visible. Test an empty folder too.
4. Move a tab into a folder and back out with the tab menu, then repeat with the `move_tab_to_folder` keybinding. The picker offers **No folder** for a filed tab.
5. Rename a folder and confirm its tabs follow the new name. Delete it and confirm those tabs return loose to their shelf.
6. Restart Herdr and confirm folder names, shelf placement, order, membership, and collapsed state persist.
7. Unpin a filed tab and settle a filed tab; each should appear loose in its new shelf. Pin the first again and wake the second; each returns to its folder. Close a filed tab; it leaves its folder for good.
8. Split a tab and snooze one pane, so the tab shows in Active and Snoozed. File each row into a folder on its own shelf; both folders show it, and moving or removing one leaves the other.
9. Set `ui.nerd_font = false`; confirm folder rows use `F` and each shelf still shows `+ Folder` (or `+` when narrow).
