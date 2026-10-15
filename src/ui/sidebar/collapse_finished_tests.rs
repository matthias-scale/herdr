//! `[ui.sidebar] collapse_finished`: worker runs, settled and parked sessions
//! fold into count rows; live and blocked sessions render as before.

use super::tests::app_with_agents;
use super::*;

fn run(run_id: &str, state: crate::agent_runs::DisplayState) -> crate::fleet::FleetRow {
    crate::fleet::FleetRow::test_run_summary_row(crate::agent_runs::Summary {
        host: "ub2".into(),
        run_id: run_id.into(),
        label: run_id.into(),
        task: "task".into(),
        phase: "verify".into(),
        started_at: "2026-10-15T08:00:00Z".into(),
        started_at_unix_s: 1_790_000_000,
        heartbeat_age_s: Some(1),
        state,
    })
}

/// Two live sessions, two settled, one parked, three worker runs on ub2.
fn fixture(collapse_finished: bool) -> AppState {
    let mut app = app_with_agents(&["live-a", "live-b", "settled-c", "settled-d", "parked-e"]);
    app.agent_host_name = "ub1".into();
    for ws_idx in [2, 3] {
        for pane in app.workspaces[ws_idx].tabs[0].panes.values_mut() {
            pane.settled_at = Some(1_725_000_000);
        }
    }
    app.workspaces[4].tabs[0].parked = true;
    app.fleet_snapshot = crate::fleet::Snapshot {
        polled: true,
        hosts: vec![crate::fleet::HostSnapshot {
            name: "ub2".into(),
            target: "ub2".into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::HostState::Reachable,
            version: None,
            protocol: None,
            error: None,
            remote_identity: None,
            sessions: None,
            reachable: true,
            last_seen_unix_ms: None,
            entries: ["ra-1", "ra-2", "ra-3"]
                .into_iter()
                .map(|id| run(id, crate::agent_runs::DisplayState::Active))
                .collect(),
        }],
        ..crate::fleet::Snapshot::default()
    };
    app.sidebar_collapse_finished = collapse_finished;
    app.reconcile_sidebar_presentation();
    app
}

/// One line per row, used as the before/after rendering in the PR.
fn render(app: &AppState) -> String {
    sidebar_rows(app)
        .iter()
        .filter_map(|row| match row {
            SidebarRow::SectionHeader {
                title,
                count,
                collapsed,
                ..
            } => Some(count_row_label(app, title, *count).unwrap_or_else(|| {
                format!("{} {title} ({count})", if *collapsed { "▸" } else { "▾" })
            })),
            SidebarRow::Workspace { title, .. } => Some(format!("space {title}")),
            SidebarRow::Tab { entry, .. } | SidebarRow::Agent { entry, .. } => {
                Some(format!("  session {}", entry.space_label))
            }
            SidebarRow::AgentRun { summary, .. } => Some(format!(
                "  run {}",
                summary
                    .as_ref()
                    .map_or("-", |summary| summary.run_id.as_str())
            )),
            SidebarRow::NestedHeader { title, .. } => Some(format!(" host {title}")),
            SidebarRow::NeedsYou { title, .. } => Some(format!("needs-you {title}")),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn session_count(rows: &str, label: &str) -> usize {
    rows.lines()
        .filter(|line| line.starts_with("  session ") && line.ends_with(label))
        .count()
}

#[test]
fn finished_work_folds_into_count_rows_and_live_sessions_stay() {
    let app = fixture(true);
    let rows = render(&app);
    eprintln!("after:\n{rows}");
    assert!(rows.lines().any(|line| line == "3 workers"), "{rows}");
    assert!(rows.lines().any(|line| line == "2 settled"), "{rows}");
    assert!(rows.lines().any(|line| line == "1 parked"), "{rows}");
    assert!(!rows.contains("  run ra-"), "{rows}");
    for label in ["settled-c", "settled-d", "parked-e"] {
        assert_eq!(session_count(&rows, label), 0, "{label} folded: {rows}");
    }
    for label in ["live-a", "live-b"] {
        assert_eq!(session_count(&rows, label), 1, "{label} live: {rows}");
    }
}

#[test]
fn activating_a_count_row_expands_and_collapses_it_in_place() {
    let mut app = fixture(true);
    app.toggle_sidebar_group(SETTLED_SECTION_TITLE);
    app.toggle_sidebar_group(RUNS_SECTION_TITLE);
    app.toggle_sidebar_group(INBOX_NEW_SECTION_TITLE);
    // Remote hosts keep their own device fold inside the expanded row.
    app.toggle_sidebar_group("device:runs/ub2");
    let rows = render(&app);
    for label in ["settled-c", "settled-d", "parked-e"] {
        assert_eq!(session_count(&rows, label), 1, "{label} expanded: {rows}");
    }
    for id in ["ra-1", "ra-2", "ra-3"] {
        assert!(rows.contains(&format!("  run {id}")), "{id}: {rows}");
    }
    assert!(!rows.lines().any(|line| line == "2 settled"), "{rows}");

    app.toggle_sidebar_group(SETTLED_SECTION_TITLE);
    let rows = render(&app);
    assert!(rows.lines().any(|line| line == "2 settled"), "{rows}");
    assert_eq!(session_count(&rows, "settled-c"), 0, "{rows}");
}

#[test]
fn collapse_finished_off_keeps_the_previous_rows() {
    let app = fixture(false);
    let rows = render(&app);
    eprintln!("before:\n{rows}");
    assert_eq!(rows, BEFORE);
}

#[test]
fn blocked_sessions_and_runs_never_fold() {
    let mut app = fixture(true);
    // A settled pane unsettles when its agent changes state
    // (`app::settled`), so blocked sessions reach the builder live or parked.
    let pane = app.workspaces[4].tabs[0].root_pane;
    let terminal_id = app.workspaces[4].tabs[0].panes[&pane]
        .attached_terminal_id
        .clone();
    app.terminals
        .get_mut(&terminal_id)
        .unwrap()
        .set_raw_agent_state_for_test(AgentState::Blocked);
    app.fleet_snapshot.hosts[0]
        .entries
        .push(run("ra-blocked", crate::agent_runs::DisplayState::Blocked));
    app.reconcile_sidebar_presentation();
    let rows = render(&app);
    assert!(rows.lines().any(|line| line == "needs-you pi"), "{rows}");
    assert!(
        !rows.lines().any(|line| line.ends_with(" parked")),
        "{rows}"
    );
    assert_eq!(session_count(&rows, "parked-e"), 1, "{rows}");
    assert!(rows.contains("  run ra-blocked"), "{rows}");
    assert!(!rows.contains("  run ra-1"), "{rows}");
}

/// `sidebar_rows` for this fixture with the switch off. Every new branch is
/// gated on `sidebar_collapse_finished`, so this is the pre-change output.
const BEFORE: &str = "\
▾ Spaces (5)
space 
  session live-a
space 
  session live-b
space 
space 
space 
▸ No repo yet (0)
▸ Snoozed (0)
▸ Settled (2)
▸ Runs (3)
▸ Inbox · new (1)
▸ Symphony (0)";
