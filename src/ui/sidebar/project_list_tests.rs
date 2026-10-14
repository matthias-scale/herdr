//! MAT-333 shallow project list: Current layout + Repo view rows, collapsed
//! retention of the activated tab, the disclosure-slot status, and the
//! Provider icon / Machine name display options.

use super::tests::row_text;
use super::*;
use crate::app::state::{ControlId, Mode};
use crate::workspace::Workspace;
use ratatui::{backend::TestBackend, Terminal};

const ALPHA_KEY: &str = "repo:repo:owner/alpha";
const BETA_KEY: &str = "repo:repo:owner/beta";

fn set_repo(app: &mut AppState, ws_idx: usize, tab_idx: usize, repo: &str, branch: &str) {
    let pane_id = app.workspaces[ws_idx].tabs[tab_idx].root_pane;
    let terminal_id = app.workspaces[ws_idx].tabs[tab_idx].panes[&pane_id]
        .attached_terminal_id
        .clone();
    app.terminals
        .get_mut(&terminal_id)
        .expect("fixture terminal")
        .replace_prevalidated_manual_work_context(crate::work_context::PaneWorkContext {
            repo: Some(repo.into()),
            branch: Some(branch.into()),
            work_title: Some("Sidebar polish".into()),
            ..Default::default()
        });
}

/// `alpha` holds `alpha_tabs` tabs of owner/alpha on alternating branches;
/// `beta` holds one owner/beta tab. Every pane is a working Codex agent.
fn project_app(alpha_tabs: usize) -> AppState {
    let mut alpha = Workspace::test_new("alpha");
    for index in 1..alpha_tabs {
        alpha.test_add_tab(Some(&format!("alpha-{index}")));
    }
    let mut app = AppState::test_new();
    app.workspaces = vec![alpha, Workspace::test_new("beta")];
    app.ensure_test_terminals();
    for terminal in app.terminals.values_mut() {
        terminal.detected_agent = Some(Agent::Codex);
        terminal.set_raw_agent_state_for_test(AgentState::Working);
    }
    for tab_idx in 0..alpha_tabs {
        let branch = if tab_idx % 2 == 0 { "main" } else { "feature" };
        set_repo(&mut app, 0, tab_idx, "owner/alpha", branch);
    }
    set_repo(&mut app, 1, 0, "owner/beta", "main");
    app.agent_host_name = "ub1".into();
    app.nerd_font = false;
    app.refresh_local_agent_panel_identities();
    app.active = Some(0);
    app.selected = 0;
    app.reconcile_sidebar_presentation();
    app
}

fn tab_entry(app: &AppState) -> AgentPanelEntry {
    sidebar_rows(app)
        .into_iter()
        .find_map(|row| match row {
            SidebarRow::Tab { entry, .. } => Some(*entry),
            _ => None,
        })
        .expect("tab row")
}

fn local_tab_rows(rows: &[SidebarRow]) -> Vec<(usize, usize, u16)> {
    rows.iter()
        .filter_map(|row| match row {
            SidebarRow::Tab { entry, depth } => entry
                .local_target()
                .map(|target| (target.ws_idx, target.tab_idx, *depth)),
            _ => None,
        })
        .collect()
}

fn remote_rows(rows: &[SidebarRow]) -> Vec<String> {
    rows.iter()
        .filter_map(|row| match row {
            SidebarRow::RemoteAgent { entry, .. } => Some(entry.agent_ref.agent.clone()),
            _ => None,
        })
        .collect()
}

fn agent_info(value: serde_json::Value) -> crate::api::schema::AgentInfo {
    let mut base = serde_json::json!({
        "terminal_id": "terminal",
        "agent": "codex",
        "agent_status": "working",
        "workspace_id": "workspace",
        "tab_id": "tab",
        "pane_id": "pane",
        "focused": false,
        "revision": 1
    });
    for (key, value) in value.as_object().expect("object overrides") {
        base[key] = value.clone();
    }
    serde_json::from_value(base).expect("agent fixture")
}

fn finder_hit(
    title: &str,
    agent: crate::api::schema::AgentInfo,
) -> crate::api::schema::AgentSearchHit {
    crate::api::schema::AgentSearchHit {
        target: "pane".into(),
        title: title.into(),
        source: crate::api::schema::AgentSearchSource::Title,
        path: None,
        line: None,
        context: Vec::new(),
        preview: Vec::new(),
        agent,
    }
}

fn remote_fixture(app: &mut AppState, repo: &str) {
    let info = agent_info(serde_json::json!({
        "terminal_id": "terminal-remote",
        "name": "remote work",
        "pane_id": "pane/9",
        "work_context": {"ticket_ids": [], "pr_urls": [], "repo": repo, "branch": "main"}
    }));
    let snapshot = crate::fleet::Snapshot {
        polled: true,
        configured_hosts: vec!["ub2".into()],
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
            entries: vec![crate::fleet::FleetRow::test_agent_info_row("ub2", info)],
        }],
        ..crate::fleet::Snapshot::default()
    };
    app.remote_agent_panel_entries = remote_agent_panel_entries(&snapshot, false);
}

fn render(app: &AppState, width: u16, height: u16) -> Terminal<TestBackend> {
    let area = Rect::new(0, 0, width, height);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| render_sidebar(app, &TerminalRuntimeRegistry::new(), frame, area))
        .expect("render sidebar");
    terminal
}

#[test]
fn repo_group_shallow_project_rows_sit_one_cell_in_without_branch_nesting() {
    let app = project_app(3);
    assert!(sidebar_shallow_project_list(&app));
    let rows = sidebar_rows(&app);
    let headers = rows
        .iter()
        .filter_map(|row| match row {
            SidebarRow::Workspace { title, .. } => Some(title.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(headers, ["owner/alpha", "owner/beta"]);
    assert!(
        !rows.iter().any(|row| matches!(
            row,
            SidebarRow::NestedHeader { title, .. } if title.starts_with('⎇')
        )),
        "branch subgroups do not nest inside a project"
    );
    let tabs = local_tab_rows(&rows);
    assert_eq!(tabs.len(), 4, "one row per tab");
    assert!(tabs.iter().all(|(.., depth)| *depth == 1), "{tabs:?}");
}

#[test]
fn repo_group_shallow_list_leaves_sections_and_other_views_alone() {
    let mut app = project_app(3);
    app.sidebar_group_mode = SidebarGroupMode::Spaces;
    assert!(!sidebar_shallow_project_list(&app));
    app.sidebar_group_mode = SidebarGroupMode::Repo;
    app.sidebar_sections_layout = true;
    assert!(!sidebar_shallow_project_list(&app));
}

#[test]
fn collapsed_group_retains_exactly_the_activated_tab_once() {
    let mut app = project_app(4);
    app.workspaces[0].switch_tab(2);
    app.collapsed_sidebar_groups.insert(ALPHA_KEY.into());
    app.collapsed_sidebar_groups.insert(BETA_KEY.into());
    let persisted = app.collapsed_sidebar_groups.clone();

    let rows = sidebar_rows(&app);
    assert_eq!(
        local_tab_rows(&rows),
        [(0, 2, 1)],
        "only the activated tab, once, one cell in"
    );
    assert_eq!(
        rows.iter()
            .filter(|row| matches!(row, SidebarRow::Workspace { .. }))
            .count(),
        2
    );

    // Projection is pure: persisted keys and activation are unchanged.
    assert_eq!(app.collapsed_sidebar_groups, persisted);
    assert_eq!(app.active, Some(0));
    assert_eq!(app.workspaces[0].active_tab_index(), 2);
    assert!(app.sidebar_selected_remote_agent.is_none());
}

#[test]
fn collapsed_group_without_the_activated_tab_has_no_children() {
    let mut app = project_app(2);
    app.active = Some(1);
    app.collapsed_sidebar_groups.insert(ALPHA_KEY.into());
    assert_eq!(local_tab_rows(&sidebar_rows(&app)), [(1, 0, 1)]);
}

#[test]
fn collapsed_group_keeps_an_activated_tab_beyond_the_first_rows() {
    let mut app = project_app(12);
    app.workspaces[0].switch_tab(11);
    app.collapsed_sidebar_groups.insert(ALPHA_KEY.into());
    assert_eq!(local_tab_rows(&sidebar_rows(&app)), [(0, 11, 1), (1, 0, 1)]);
}

#[test]
fn collapsed_group_retention_moves_with_activation_not_finder_highlight() {
    let mut app = project_app(3);
    app.collapsed_sidebar_groups.insert(ALPHA_KEY.into());
    app.collapsed_sidebar_groups.insert(BETA_KEY.into());
    assert_eq!(local_tab_rows(&sidebar_rows(&app)), [(0, 0, 1)]);

    // Highlighting a finder hit for another project is not activation.
    app.sidebar_search_active = true;
    app.open_agent_finder();
    let workspace_id = app.workspaces[1].id.clone();
    app.agent_finder_results.push(finder_hit(
        "beta",
        agent_info(serde_json::json!({"workspace_id": workspace_id})),
    ));
    app.agent_finder_selected = 0;
    assert_eq!(local_tab_rows(&sidebar_rows(&app)), [(0, 0, 1)]);
    app.close_agent_finder();
    assert!(app.collapsed_sidebar_groups.contains(ALPHA_KEY));
    assert!(app.collapsed_sidebar_groups.contains(BETA_KEY));

    // Activating a tab in the other collapsed project moves the exception.
    app.active = Some(1);
    assert_eq!(local_tab_rows(&sidebar_rows(&app)), [(1, 0, 1)]);
    app.active = Some(0);
    app.workspaces[0].switch_tab(1);
    assert_eq!(local_tab_rows(&sidebar_rows(&app)), [(0, 1, 1)]);
}

#[test]
fn collapsed_group_retains_the_selected_remote_tab_by_identity() {
    let mut app = project_app(2);
    remote_fixture(&mut app, "owner/alpha");
    assert_eq!(remote_rows(&sidebar_rows(&app)), ["pane/9"]);

    app.collapsed_sidebar_groups.insert(ALPHA_KEY.into());
    let rows = sidebar_rows(&app);
    assert!(
        remote_rows(&rows).is_empty(),
        "an unselected remote sibling folds"
    );
    assert_eq!(local_tab_rows(&rows), [(0, 0, 1), (1, 0, 1)]);

    let agent_ref = app.remote_agent_panel_entries[0].agent_ref.clone();
    app.sidebar_selected_remote_agent = Some(agent_ref);
    let rows = sidebar_rows(&app);
    assert_eq!(
        remote_rows(&rows),
        ["pane/9"],
        "the activated remote tab stays"
    );
    assert!(
        local_tab_rows(&rows)
            .iter()
            .all(|(ws_idx, ..)| *ws_idx != 0),
        "the local active tab is not activated while a remote one is"
    );
}

#[test]
fn collapsed_group_expand_restores_rows_and_show_more_state() {
    let mut app = project_app(3);
    app.sidebar_unassigned_expanded_views
        .insert(SidebarGroupMode::Repo);
    let expanded = local_tab_rows(&sidebar_rows(&app));
    app.toggle_sidebar_group("repo:owner/alpha");
    assert!(app.collapsed_sidebar_groups.contains(ALPHA_KEY));
    assert_eq!(local_tab_rows(&sidebar_rows(&app)), [(0, 0, 1), (1, 0, 1)]);
    app.toggle_sidebar_group("repo:owner/alpha");
    assert_eq!(local_tab_rows(&sidebar_rows(&app)), expanded);
    assert!(app
        .sidebar_unassigned_expanded_views
        .contains(&SidebarGroupMode::Repo));
    assert_eq!(app.active, Some(0));
    assert_eq!(app.workspaces[0].active_tab_index(), 0);
}

#[test]
fn collapsed_group_records_one_placement_per_pane() {
    let mut app = project_app(3);
    app.workspaces[0].switch_tab(1);
    app.collapsed_sidebar_groups.insert(ALPHA_KEY.into());
    let coverage = sidebar_coverage(&app, &TerminalRuntimeRegistry::new());
    assert_eq!(coverage.len(), 4);
    for pane in coverage {
        assert_ne!(pane.placement.is_some(), pane.dropped_by.is_some());
    }
}

#[test]
fn collapsed_group_header_swaps_status_for_chevron_on_hover_and_focus() {
    let mut app = project_app(2);
    app.set_server_mode(Mode::Terminal);
    app.active = Some(1);
    app.collapsed_sidebar_groups.insert(ALPHA_KEY.into());
    let area = Rect::new(0, 0, 40, 16);
    let card = compute_workspace_card_areas(&app, area)
        .into_iter()
        .find(|card| card.ws_idx == 0)
        .expect("alpha header");
    let status = sidebar_rows(&app)
        .into_iter()
        .find_map(|row| match row {
            SidebarRow::Workspace {
                ws_idx: 0,
                collapsed_status,
                ..
            } => collapsed_status,
            _ => None,
        })
        .expect("collapsed project carries its status");
    assert_eq!(status.count, 2);

    let header = |app: &AppState| {
        let terminal = render(app, area.width, area.height);
        let buffer = terminal.backend().buffer();
        (
            buffer[(card.rect.x, card.rect.y)].symbol().to_string(),
            (0..area.width)
                .find(|x| {
                    buffer[(*x, card.rect.y)].symbol() == "o"
                        && buffer[(*x + 1, card.rect.y)].symbol() == "w"
                })
                .expect("title column"),
        )
    };
    let (rest_slot, rest_title) = header(&app);
    assert_eq!(rest_slot, status.glyph, "status fills the disclosure slot");

    app.hovered_control = Some(ControlId::SidebarRowHover(card.rect.y));
    let (hover_slot, hover_title) = header(&app);
    assert_eq!(hover_slot, "▸");
    assert_eq!(hover_title, rest_title, "no geometry shift on hover");

    app.hovered_control = None;
    app.set_server_mode(Mode::Navigate);
    app.selected = 0;
    let (focus_slot, focus_title) = header(&app);
    assert_eq!(focus_slot, "▸", "keyboard focus shows the chevron");
    assert_eq!(focus_title, rest_title);

    app.set_server_mode(Mode::Terminal);
    let (back_slot, back_title) = header(&app);
    assert_eq!(back_slot, status.glyph, "leaving restores the status");
    assert_eq!(back_title, rest_title);

    app.collapsed_sidebar_groups.remove(ALPHA_KEY);
    let (open_slot, _) = header(&app);
    assert_eq!(open_slot, "▾", "an expanded project keeps its open chevron");
}

fn display_row(app: &mut AppState, width: u16, mobile: bool) -> String {
    let layout = app.view.layout;
    if mobile {
        app.view.layout = crate::app::state::ViewLayout::Mobile;
    }
    let entry = tab_entry(app);
    let rect = Rect::new(0, 0, width, 1);
    let mut terminal = Terminal::new(TestBackend::new(width, 1)).expect("terminal");
    terminal
        .draw(|frame| render_compact_agent_row(app, frame, &entry, rect, 1, true, None))
        .expect("render row");
    app.view.layout = layout;
    row_text(terminal.backend().buffer(), 0, width)
}

#[test]
fn sidebar_display_defaults_hide_provider_and_machine() {
    let mut app = project_app(1);
    assert!(!app.sidebar_show_provider_icons);
    assert!(!app.sidebar_show_machine_names);
    let row = display_row(&mut app, 40, false);
    assert!(!row.contains('x'), "{row:?}");
    assert!(!row.contains('1'), "{row:?}");
}

#[test]
fn sidebar_display_all_four_combinations_are_independent() {
    for (icons, machines) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut app = project_app(1);
        app.sidebar_show_provider_icons = icons;
        app.sidebar_show_machine_names = machines;
        let entry = tab_entry(&app);
        assert_eq!(!sidebar_row_provider(&app, &entry).is_empty(), icons);
        assert_eq!(
            sidebar_device_code(&app, sidebar_machine_host(&app, &entry)),
            machines.then_some('1')
        );
        for mobile in [false, true] {
            let row = display_row(&mut app, 40, mobile);
            let case = format!("icons={icons} machines={machines} mobile={mobile}: {row:?}");
            assert!(row.contains("Sidebar polish"), "{case}");
            assert_eq!(row.contains('x'), icons, "{case}");
            assert_eq!(row.contains('1'), machines, "{case}");
        }
    }
}

#[test]
fn sidebar_display_provider_stays_absent_without_a_known_provider() {
    let mut app = project_app(1);
    app.sidebar_show_provider_icons = true;
    for terminal in app.terminals.values_mut() {
        terminal.detected_agent = None;
    }
    for row in sidebar_rows(&app) {
        if let SidebarRow::Tab { entry, .. } = row {
            assert!(
                sidebar_row_provider(&app, &entry).is_empty(),
                "agentless rows never invent a provider"
            );
        }
    }
    let unknown = agent_info(serde_json::json!({"agent": "mystery-agent"}));
    assert_eq!(finder_provider_token(&app, &unknown), None);
    let known = agent_info(serde_json::json!({}));
    assert_eq!(finder_provider_token(&app, &known), Some("x"));
    app.sidebar_show_provider_icons = false;
    assert_eq!(finder_provider_token(&app, &known), None);
}

#[test]
fn sidebar_display_metadata_yields_before_status_and_title_at_18_columns() {
    let mut app = project_app(1);
    app.sidebar_show_provider_icons = true;
    app.sidebar_show_machine_names = true;
    for mobile in [false, true] {
        let narrow = display_row(&mut app, 18, mobile);
        let dot = narrow.trim_start().chars().next().expect("status dot");
        assert!(!dot.is_alphanumeric(), "status leads the row: {narrow:?}");
        assert!(
            narrow.contains("Sidebar"),
            "readable title kept: {narrow:?}"
        );
        let wide = display_row(&mut app, 40, mobile);
        assert!(
            wide.contains("Sidebar polish") && wide.contains("x1"),
            "{wide:?}"
        );
    }
}

#[test]
fn sidebar_display_toggles_finder_result_host_and_provider() {
    let mut app = project_app(1);
    app.agent_finder_results.push(finder_hit(
        "Result title",
        agent_info(serde_json::json!({
            "agent_ref": "ub2::pane"
        })),
    ));
    let lines = |app: &AppState| {
        let area = Rect::new(0, 0, 40, 4);
        let mut terminal = Terminal::new(TestBackend::new(40, 4)).expect("terminal");
        terminal
            .draw(|frame| render_agent_finder_results_in_area(app, frame, area))
            .expect("render finder");
        let buffer = terminal.backend().buffer();
        (row_text(buffer, 0, 40), row_text(buffer, 1, 40))
    };
    let (title, detail) = lines(&app);
    assert!(
        title.contains("Result title") && !title.contains(" x "),
        "{title:?}"
    );
    assert!(
        !detail.contains("ub2") && detail.contains("title"),
        "{detail:?}"
    );
    app.sidebar_show_provider_icons = true;
    app.sidebar_show_machine_names = true;
    let (title, detail) = lines(&app);
    assert!(title.contains(" x Result title"), "{title:?}");
    assert!(detail.contains("ub2 · title"), "{detail:?}");
}

#[test]
fn sidebar_display_view_dropdown_lists_both_options_after_views() {
    use crate::app::state::SidebarDisplayOption;
    let views = SidebarGroupMode::VIEWS.len();
    assert_eq!(
        SidebarDisplayOption::at_group_menu_index(views),
        Some(SidebarDisplayOption::ProviderIcons)
    );
    assert_eq!(
        SidebarDisplayOption::at_group_menu_index(views + 1),
        Some(SidebarDisplayOption::MachineNames)
    );
    assert_eq!(SidebarDisplayOption::at_group_menu_index(0), None);
    assert_eq!(SidebarDisplayOption::at_group_menu_index(views + 2), None);

    let mut app = project_app(1);
    app.sidebar_show_machine_names = true;
    app.view.sidebar_rect = Rect::new(0, 0, 30, 30);
    app.sidebar_group_menu_open = true;
    let area = Rect::new(0, 0, 60, 30);
    let layout = sidebar_group_menu_layout(&app, area).expect("dropdown fits");
    assert_eq!(layout.visible_rows, sidebar_group_menu_item_count());
    let mut terminal = Terminal::new(TestBackend::new(60, 30)).expect("terminal");
    terminal
        .draw(|frame| render_sidebar_group_menu(&app, frame))
        .expect("render dropdown");
    let buffer = terminal.backend().buffer();
    let text = (layout.list_rect.y..layout.list_rect.bottom())
        .map(|row| row_text(buffer, row, 60))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("[ ] Provider icon"), "{text}");
    assert!(text.contains("[x] Machine name"), "{text}");
}
