//! Attach-local state → project → agent projection. Host is a row attribute.
use super::*;
use std::collections::BTreeMap;

/// GitHub repository identity is case-insensitive, including across hosts.
#[derive(Clone, Copy, Debug)]
struct ProjectKey<'a>(&'a str);

impl PartialEq for ProjectKey<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(other.0)
    }
}

impl Eq for ProjectKey<'_> {}

impl PartialOrd for ProjectKey<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ProjectKey<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0
            .bytes()
            .map(|byte| byte.to_ascii_lowercase())
            .cmp(other.0.bytes().map(|byte| byte.to_ascii_lowercase()))
    }
}

fn project_section_key(state: &str, project: &str) -> String {
    format!("fleet:{state}:{}", project.to_ascii_lowercase())
}

pub(super) fn host_label(app: &AppState) -> &str {
    app.sidebar_presentation
        .host_filter
        .as_deref()
        .unwrap_or("all hosts")
}

pub(crate) fn host_options(app: &AppState) -> Vec<Option<String>> {
    let hosts = app
        .fleet_snapshot
        .hosts
        .iter()
        .map(|host| host.name.as_str())
        .chain(std::iter::once(app.agent_host_name.as_str()))
        .chain(
            app.remote_agent_panel_entries
                .iter()
                .map(|entry| entry.agent_ref.host.as_str()),
        )
        .chain(app.sidebar_presentation.host_filter.as_deref())
        .collect::<std::collections::BTreeSet<_>>();
    std::iter::once(None)
        .chain(hosts.into_iter().map(|host| Some(host.to_string())))
        .collect()
}

pub(super) fn host_tooltip(app: &AppState, host: &str) -> String {
    match app
        .fleet_snapshot
        .hosts
        .iter()
        .find(|snapshot| snapshot.name == host)
    {
        Some(snapshot) if snapshot.state != crate::fleet::HostState::Reachable => {
            let age = snapshot.last_seen_unix_ms.map(|last| {
                // Use the poller's observation clock; never read the wall clock in render.
                app.fleet_snapshot
                    .refreshed_at_unix_ms
                    .unwrap_or(last)
                    .saturating_sub(last)
                    / 1000
            });
            match age {
                Some(age) => format!("{host} reconnecting, {age} s"),
                None => format!("{host} reconnecting"),
            }
        }
        _ => host.to_string(),
    }
}

fn project(repo: &str) -> (&str, &str) {
    let repo = repo.trim().trim_end_matches('/').trim_end_matches(".git");
    (repo, repo.rsplit('/').next().unwrap_or(repo))
}

fn state_index(app: &AppState, entry: &AgentPanelEntry) -> usize {
    if sidebar_entry_lifecycle(app, entry) != SidebarEntryLifecycle::Active {
        3
    } else if entry_is_blocked(entry) {
        0
    } else if entry_needs_human_attention(entry) {
        1
    } else if sidebar_entry_has_working_state(entry) {
        2
    } else if entry.state == AgentState::Unknown {
        1
    } else {
        3
    }
}

const STATES: [&str; 4] = [
    BLOCKERS_SECTION_TITLE,
    WAITING_SECTION_TITLE,
    WORKING_SECTION_TITLE,
    DONE_IDLE_SECTION_TITLE,
];

pub(super) fn collapsed_placement(
    app: &AppState,
    entry: &AgentPanelEntry,
) -> Option<(String, String)> {
    let title = STATES[state_index(app, entry)];
    if section_is_collapsed(app, title) {
        return Some((title.into(), title.into()));
    }
    let target = entry.local_target()?;
    let workspace = app.workspaces.get(target.ws_idx)?;
    let (project, name) = workspace_declared_repo(app, target.ws_idx)
        .map(|(repo, _)| {
            let (key, title) = project(repo);
            (key.to_string(), title.to_string())
        })
        .unwrap_or_else(|| {
            let name = workspace
                .custom_name
                .as_deref()
                .unwrap_or(&workspace.cached_auto_label);
            (name.to_string(), name.to_string())
        });
    section_is_collapsed(app, &project_section_key(title, &project)).then(|| (title.into(), name))
}

pub(super) fn rows<'a>(
    app: &AppState,
    entries: impl IntoIterator<Item = &'a AgentPanelEntry>,
    expand_collapsed: bool,
) -> Vec<SidebarRow> {
    // Resolve workspace metadata once, outside the entry loop. No PTY reads or I/O.
    let projects = app
        .workspaces
        .iter()
        .enumerate()
        .map(|(index, workspace)| {
            workspace_declared_repo(app, index)
                .map(|(repo, _)| {
                    let (key, title) = project(repo);
                    (key.to_string(), title.to_string())
                })
                .unwrap_or_else(|| {
                    let name = workspace
                        .custom_name
                        .as_deref()
                        .unwrap_or(&workspace.cached_auto_label);
                    (name.to_string(), name.to_string())
                })
        })
        .collect::<Vec<_>>();
    let mut groups: [BTreeMap<ProjectKey<'_>, (&str, Vec<&AgentPanelEntry>)>; 4] =
        Default::default();
    for entry in entries {
        if app
            .sidebar_presentation
            .host_filter
            .as_deref()
            .is_some_and(|host| sidebar_machine_host(app, entry) != host)
        {
            continue;
        }
        let state = state_index(app, entry);
        let (key, title) = if let Some(remote) = entry.remote_entry.as_ref() {
            remote
                .work_context
                .repo
                .as_deref()
                .map(project)
                .unwrap_or((entry.space_label.as_str(), entry.space_label.as_str()))
        } else if let Some(target) = entry.local_target() {
            let Some(project) = projects.get(target.ws_idx) else {
                continue;
            };
            (project.0.as_str(), project.1.as_str())
        } else {
            continue;
        };
        groups[state]
            .entry(ProjectKey(key))
            .or_insert_with(|| (title, Vec::new()))
            .1
            .push(entry);
    }
    let mut rows = Vec::new();
    for (title, groups) in [
        BLOCKERS_SECTION_TITLE,
        WAITING_SECTION_TITLE,
        WORKING_SECTION_TITLE,
        DONE_IDLE_SECTION_TITLE,
    ]
    .into_iter()
    .zip(groups)
    {
        if !rows.is_empty() {
            rows.push(SidebarRow::ShelfDivider);
        }
        let count = groups.values().map(|(_, entries)| entries.len()).sum();
        let collapsed = !expand_collapsed && section_is_collapsed(app, title);
        rows.push(SidebarRow::SectionHeader {
            title,
            count,
            host_counts: Vec::new(),
            collapsed,
        });
        if collapsed {
            continue;
        }
        for (project, (name, mut entries)) in groups {
            entries.sort_by_key(|entry| std::cmp::Reverse(compact_activity_instant(entry)));
            let key = project_section_key(title, project.0);
            let collapsed = !expand_collapsed && section_is_collapsed(app, &key);
            rows.push(SidebarRow::NestedHeader {
                key,
                action_key: None,
                sort_key: None,
                sort_mode: SidebarSortMode::Default,
                title: name.to_string(),
                count: entries.len(),
                activity_count: None,
                collapsed,
                dim: false,
                status: None,
                spawn: false,
            });
            if !collapsed {
                let entries = entries
                    .into_iter()
                    .map(|entry| {
                        let mut entry = entry.clone();
                        entry.space_label_redundant = true;
                        // Preserve the lifecycle dot, including on working rows.
                        entry.working_shelf = false;
                        entry
                    })
                    .collect();
                append_tab_rows(&mut rows, entries, 2);
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::super::tests::{
        app_with_agents, compact_test_entry, fleet_host_snapshot, remote_agent_info,
    };
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn fixture() -> (AppState, Vec<AgentPanelEntry>) {
        let mut app = app_with_agents(&["herdr"]);
        app.agent_host_name = "ub2".into();
        app.sidebar_sections_layout = true;
        app.nerd_font = false;
        app.workspaces[0].repo_binding = Some("org/herdr".into());
        let mut entries = Vec::new();
        let mut local = compact_test_entry("local long activity title", Some(Agent::Claude));
        local.state = AgentState::Working;
        local.activity_at = Some(app.view_observed_at - std::time::Duration::from_secs(180));
        entries.push(local);
        let hosts = ["ub1", "mbpro"]
            .into_iter()
            .map(|host| {
                let mut info = remote_agent_info(
                    host,
                    &format!("{host} long activity title"),
                    crate::api::schema::AgentStatus::Working,
                    false,
                    false,
                );
                info.work_context = crate::work_context::PaneWorkContext {
                    repo: Some("org/herdr".into()),
                    ..Default::default()
                };
                fleet_host_snapshot(
                    host,
                    false,
                    vec![crate::fleet::FleetRow::test_agent_info_row(host, info)],
                )
            })
            .collect();
        app.fleet_snapshot = crate::fleet::Snapshot {
            hosts,
            ..Default::default()
        };
        app.remote_agent_panel_entries = remote_agent_panel_entries(&app.fleet_snapshot, false);
        for remote in &mut app.remote_agent_panel_entries {
            let seconds = if remote.agent_ref.host == "ub1" {
                0
            } else {
                360
            };
            std::sync::Arc::make_mut(remote).entry.activity_at =
                Some(app.view_observed_at - std::time::Duration::from_secs(seconds));
            entries.push(remote_agent_as_panel_entry(remote));
        }
        (app, entries)
    }

    #[test]
    fn three_hosts_share_one_project_and_sort_by_activity() {
        let (app, entries) = fixture();
        let rows = rows(&app, &entries, false);
        assert_eq!(
            rows.iter()
                .filter(
                    |row| matches!(row, SidebarRow::NestedHeader { title, .. } if title == "herdr")
                )
                .count(),
            1
        );
        assert!(!rows.iter().any(
            |row| matches!(row, SidebarRow::NestedHeader { key, .. } if key.starts_with("device:"))
        ));
        let hosts = rows
            .iter()
            .filter_map(|row| match row {
                SidebarRow::Tab { entry, .. } => Some(sidebar_machine_host(&app, entry)),
                SidebarRow::RemoteAgent { entry, .. } => Some(entry.agent_ref.host.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(hosts, ["ub1", "ub2", "mbpro"]);
        assert_eq!(
            hosts
                .iter()
                .map(|host| sidebar_device_code(&app, host))
                .collect::<Vec<_>>(),
            [Some('1'), Some('2'), Some('m')]
        );
        assert!(rows.iter().any(|row| matches!(
            row,
            SidebarRow::SectionHeader {
                title: DONE_IDLE_SECTION_TITLE,
                collapsed: true,
                ..
            }
        )));
    }

    #[test]
    fn repository_case_differences_share_a_group_and_collapse_key() {
        let (mut app, entries) = fixture();
        app.workspaces[0].repo_binding = Some("Org/Herdr".into());
        let projected = rows(&app, &entries, false);
        let projects = projected
            .iter()
            .filter_map(|row| match row {
                SidebarRow::NestedHeader { key, count, .. } => Some((key.as_str(), *count)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(projects, [("fleet:working:org/herdr", 3)]);
        app.collapsed_sidebar_groups
            .insert("sections:fleet:working:org/herdr".into());
        assert_eq!(
            collapsed_placement(&app, &entries[0]),
            Some(("working".into(), "Herdr".into()))
        );
        app.workspaces[0].repo_binding = Some("org/herdr".into());
        assert!(rows(&app, &entries, false)
            .iter()
            .all(|row| !matches!(row, SidebarRow::Tab { .. } | SidebarRow::RemoteAgent { .. })));
    }

    #[test]
    fn host_filter_applies_to_all_states_and_no_device_summaries() {
        let (mut app, entries) = fixture();
        app.sidebar_presentation.host_filter = Some("ub1".into());
        let rows = rows(&app, &entries, false);
        assert_eq!(host_label(&app), "ub1");
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, SidebarRow::RemoteAgent { .. }))
                .count(),
            1
        );
        assert!(!rows.iter().any(|row| matches!(row, SidebarRow::Tab { .. })));
    }

    fn rendered_row(app: &AppState, entry: &AgentPanelEntry, width: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, 1)).unwrap();
        terminal
            .draw(|frame| {
                let rect = Rect::new(0, 0, width, 1);
                if let Some(remote) = entry.remote_entry.as_ref() {
                    render_remote_compact_agent_row_with_prefix(
                        app, frame, remote, rect, 2, None, None, false, false,
                    );
                } else {
                    render_compact_agent_row_with_prefix(
                        app, frame, entry, rect, 2, true, None, None,
                    );
                }
            })
            .unwrap();
        (0..width)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect()
    }

    #[test]
    fn narrow_rows_preserve_provider_device_and_age() {
        let (mut app, entries) = fixture();
        for width in [18, app.default_sidebar_width, 28, 40] {
            for entry in &entries {
                let text = rendered_row(&app, entry, width);
                let code = sidebar_device_code(&app, sidebar_machine_host(&app, entry)).unwrap();
                let provider = compact_provider(entry, false);
                assert!(
                    text.contains(&format!("{provider}{code}")),
                    "{width}: {text:?}"
                );
                if entry.remote_entry.is_none() && width >= 26 {
                    assert!(text.contains("3m"), "{width}: {text:?}");
                }
                println!("after width {width}: {text}");
            }
            if width == app.default_sidebar_width || width == 28 {
                // Reproduce the former local-only row contract: its device
                // character was supplied by the surrounding device header.
                app.sidebar_sections_layout = false;
                println!(
                    "before local row width {width}: {}",
                    rendered_row(&app, &entries[0], width)
                );
                app.sidebar_sections_layout = true;
            }
        }
    }

    #[test]
    fn four_states_and_done_idle_default_collapse() {
        let (app, entries) = fixture();
        let mut blocked = entries[0].clone();
        blocked.attention_tier = Some(AttentionTier::Blocked);
        let mut waiting = entries[0].clone();
        waiting.attention_tier = Some(AttentionTier::Attention);
        let mut idle = entries[0].clone();
        idle.state = AgentState::Idle;
        idle.attention_tier = Some(AttentionTier::None);
        let rows = rows(&app, &[blocked, waiting, entries[0].clone(), idle], false);
        let states = rows
            .iter()
            .filter_map(|row| match row {
                SidebarRow::SectionHeader {
                    title,
                    count,
                    collapsed,
                    ..
                } => Some((*title, *count, *collapsed)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            states,
            [
                ("blocked", 1, false),
                ("waiting", 1, false),
                ("working", 1, false),
                ("done / idle", 1, true)
            ]
        );
    }

    #[test]
    fn active_unknown_agents_wait_on_local_and_remote_hosts_but_settled_stays_done() {
        let (app, mut entries) = fixture();
        for entry in &mut entries {
            if let Some(remote) = entry.remote_entry.as_mut() {
                let mut data = remote.entry.clone();
                data.state = AgentState::Unknown;
                data.attention_tier = Some(AttentionTier::None);
                *remote = std::sync::Arc::new(RemoteAgentPanelEntry::new_with_narrow_host(
                    remote.agent_ref.clone(),
                    data,
                    remote.model.clone(),
                    remote.effort.clone(),
                    remote.work_context.clone(),
                    false,
                    None,
                    true,
                    false,
                ));
                *entry = remote_agent_as_panel_entry(remote);
            } else {
                entry.state = AgentState::Unknown;
                entry.attention_tier = Some(AttentionTier::None);
            }
        }
        let mut settled = entries.pop().unwrap();
        std::sync::Arc::make_mut(settled.remote_entry.as_mut().unwrap()).settled = true;
        entries.push(settled);
        let projected = rows(&app, &entries, false);
        let states = projected
            .iter()
            .filter_map(|row| match row {
                SidebarRow::SectionHeader {
                    title,
                    count,
                    collapsed,
                    ..
                } => Some((*title, *count, *collapsed)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            states,
            [
                ("blocked", 0, false),
                ("waiting", 2, false),
                ("working", 0, false),
                ("done / idle", 1, true),
            ]
        );
        assert_eq!(
            projected
                .iter()
                .filter(|row| matches!(
                    row,
                    SidebarRow::Tab { .. } | SidebarRow::RemoteAgent { .. }
                ))
                .count(),
            2
        );
        for entry in &entries[..2] {
            assert_eq!(state_index(&app, entry), 1);
            assert_eq!(collapsed_placement(&app, entry), None);
            assert_eq!(agent_dot_tooltip(entry), "?");
            for width in [18, 28] {
                assert!(rendered_row(&app, entry, width).contains('?'));
            }
        }
        assert_eq!(state_index(&app, &entries[2]), 3);
        assert_eq!(
            collapsed_placement(&app, &entries[2]),
            Some(("done / idle".into(), "done / idle".into()))
        );
    }

    #[test]
    fn host_filters_are_isolated_between_attached_clients() {
        let (mut app, _) = fixture();
        let mut client = crate::app::state::SidebarPresentationState::default();
        app.sidebar_presentation.host_filter = Some("ub1".into());
        app.swap_sidebar_presentation(&mut client);
        assert_eq!(app.sidebar_presentation.host_filter, None);
        app.sidebar_presentation.host_filter = Some("mbpro".into());
        app.swap_sidebar_presentation(&mut client);
        assert_eq!(app.sidebar_presentation.host_filter.as_deref(), Some("ub1"));
        assert_eq!(client.host_filter.as_deref(), Some("mbpro"));
    }

    #[test]
    fn full_projection_preserves_hidden_lifecycle_coverage_and_host_exclusions() {
        let mut app = app_with_agents(&["working", "idle"]);
        app.agent_host_name = "ub2".into();
        app.sidebar_sections_layout = true;
        let pane = app.workspaces[1].tabs[0].root_pane;
        let terminal_id = app.workspaces[1].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_raw_agent_state_for_test(AgentState::Idle);
        app.reconcile_sidebar_presentation();
        let coverage = sidebar_coverage(&app, &TerminalRuntimeRegistry::new());
        assert_eq!(coverage.len(), 2);
        assert!(
            coverage
                .iter()
                .all(|row| row.placement.is_some() && row.dropped_by.is_none()),
            "{coverage:?}"
        );
        app.sidebar_presentation.host_filter = Some("ub1".into());
        let coverage = sidebar_coverage(&app, &TerminalRuntimeRegistry::new());
        assert!(coverage
            .iter()
            .all(|row| row.dropped_by.as_deref() == Some("host_filter")));
        assert!(!super::super::sidebar_rows(&app).iter().any(|row| matches!(
            row,
            SidebarRow::Tab { .. } | SidebarRow::Agent { .. } | SidebarRow::NestedHeader { .. }
        )));
    }

    #[test]
    fn pinned_windows_remain_one_row_in_their_state() {
        let mut app = app_with_agents(&["pinned"]);
        app.sidebar_sections_layout = true;
        app.workspaces[0].tabs[0].pinned = true;
        let rows = super::super::sidebar_rows(&app);
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, SidebarRow::Tab { .. }))
                .count(),
            1
        );
        assert!(rows
            .iter()
            .any(|row| matches!(row, SidebarRow::Tab { entry, .. } if entry.pinned)));
        assert!(!rows.iter().any(|row| matches!(
            row,
            SidebarRow::SectionHeader {
                title: PINNED_SECTION_TITLE,
                ..
            }
        )));
    }

    #[test]
    fn active_subagents_keep_idle_parents_in_working_until_they_finish() {
        let mut app = app_with_agents(&["parent"]);
        app.sidebar_sections_layout = true;
        let pane = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_raw_agent_state_for_test(AgentState::Idle);
        terminal.set_active_subagents(Some(2));
        app.reconcile_sidebar_presentation();
        let working = super::super::sidebar_rows(&app);
        assert!(working.iter().any(|row| matches!(
            row,
            SidebarRow::SectionHeader {
                title: WORKING_SECTION_TITLE,
                count: 1,
                ..
            }
        )));
        assert!(working
            .iter()
            .any(|row| matches!(row, SidebarRow::Tab { .. })));
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_active_subagents(Some(0));
        app.reconcile_sidebar_presentation();
        let idle = super::super::sidebar_rows(&app);
        assert!(idle.iter().any(|row| matches!(
            row,
            SidebarRow::SectionHeader {
                title: DONE_IDLE_SECTION_TITLE,
                count: 1,
                collapsed: true,
                ..
            }
        )));
        assert!(!idle.iter().any(|row| matches!(row, SidebarRow::Tab { .. })));
        app.toggle_sidebar_group(DONE_IDLE_SECTION_TITLE);
        assert!(super::super::sidebar_rows(&app)
            .iter()
            .any(|row| matches!(row, SidebarRow::Tab { .. })));
    }

    #[test]
    #[ignore = "manual fixed-geometry unified sidebar scaling profile"]
    fn render_scale_profile_unified_sidebar() {
        for active_panes in [false, true] {
            for count in [1, 15] {
                let names = (0..if active_panes { 1 } else { count })
                    .map(|index| format!("project-{index}"))
                    .collect::<Vec<_>>();
                let mut app =
                    app_with_agents(&names.iter().map(String::as_str).collect::<Vec<_>>());
                for workspace in &mut app.workspaces {
                    workspace.repo_binding = Some("org/herdr".into());
                }
                if active_panes {
                    for _ in 1..count {
                        app.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
                    }
                    app.ensure_test_terminals();
                    for terminal in app.terminals.values_mut() {
                        terminal.detected_agent = Some(Agent::Claude);
                        terminal.set_raw_agent_state_for_test(AgentState::Working);
                    }
                    app.refresh_local_agent_panel_identities();
                }
                app.sidebar_sections_layout = true;
                app.sidebar_width = 28;
                app.reconcile_sidebar_presentation();
                let area = Rect::new(0, 0, 120, 40);
                let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
                let mut samples = Vec::new();
                for iteration in 0..45 {
                    // Exercise projection after a metadata event as well as draw.
                    app.mark_sidebar_projection_changed();
                    let started = std::time::Instant::now();
                    crate::ui::compute_view(&mut app, area);
                    terminal
                        .draw(|frame| {
                            render_sidebar(
                                &app,
                                &TerminalRuntimeRegistry::new(),
                                frame,
                                app.view.sidebar_rect,
                            )
                        })
                        .unwrap();
                    if iteration >= 5 {
                        samples.push(started.elapsed().as_micros());
                    }
                }
                samples.sort_unstable();
                println!("unified sidebar: {} panes={count} geometry=120x40 sidebar=28 median_us={} p95_us={}", if active_panes { "active" } else { "background" }, samples[20], samples[38]);
            }
        }
    }

    #[test]
    fn reconnecting_tooltip_uses_poller_timestamps() {
        let (mut app, _) = fixture();
        app.fleet_snapshot.refreshed_at_unix_ms = Some(10_000);
        let host = &mut app.fleet_snapshot.hosts[0];
        host.reachable = false;
        host.state = crate::fleet::HostState::Unreachable;
        host.last_seen_unix_ms = Some(6_000);
        assert_eq!(host_tooltip(&app, "ub1"), "ub1 reconnecting, 4 s");
    }
}
