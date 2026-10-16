//! Attach-local project → agent projection. Host is a row attribute.
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

pub(super) const PROJECTS: &str = "projects";

fn project_section_key(project: &str) -> String {
    format!("fleet-project:{}", project.to_ascii_lowercase())
}

fn working_key(project: &str) -> String {
    format!("fleet-working:{}", project.to_ascii_lowercase())
}

pub(super) fn marker(app: &AppState, entry: &AgentPanelEntry) -> (&'static str, Color) {
    let state = state_index(app, entry);
    let glyph = if state == 2 {
        "?"
    } else if app.nerd_font {
        "●"
    } else {
        ["!", "+", "?", "~"][state]
    };
    (
        glyph,
        [
            app.palette.red,
            app.palette.green,
            app.palette.overlay0,
            app.palette.blue,
        ][state],
    )
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
        1
    } else if entry_is_blocked(entry) {
        0
    } else if entry_needs_human_attention(entry) {
        2
    } else if sidebar_entry_has_working_state(entry) {
        3
    } else if entry.state == AgentState::Unknown {
        2
    } else {
        1
    }
}

pub(super) fn collapsed_placement(
    app: &AppState,
    entry: &AgentPanelEntry,
) -> Option<(String, String)> {
    let shelf = match sidebar_entry_lifecycle(app, entry) {
        SidebarEntryLifecycle::Snoozed => Some(SNOOZED_SECTION_TITLE),
        SidebarEntryLifecycle::Settled => Some(SETTLED_SECTION_TITLE),
        SidebarEntryLifecycle::Active if entry_is_past_done_hide_threshold(app, entry) => {
            Some(SETTLED_SECTION_TITLE)
        }
        SidebarEntryLifecycle::Active => None,
    };
    if let Some(shelf) = shelf {
        return section_is_collapsed(app, shelf).then(|| (shelf.into(), shelf.into()));
    }
    let target = entry.local_target()?;
    let workspace = app.workspaces.get(target.ws_idx)?;
    let (key, name) = workspace_declared_repo(app, target.ws_idx)
        .map(|(repo, _)| project(repo))
        .unwrap_or_else(|| {
            let name = workspace
                .custom_name
                .as_deref()
                .unwrap_or(&workspace.cached_auto_label);
            (name, name)
        });
    if section_is_collapsed(app, &project_section_key(key)) {
        Some((PROJECTS.into(), name.into()))
    } else if state_index(app, entry) == 3 && section_is_collapsed(app, &working_key(key)) {
        Some(("working".into(), name.into()))
    } else {
        None
    }
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
    let mut groups: BTreeMap<ProjectKey<'_>, (&str, Vec<&AgentPanelEntry>)> = BTreeMap::new();
    for entry in entries {
        if sidebar_entry_lifecycle(app, entry) != SidebarEntryLifecycle::Active
            || entry_is_past_done_hide_threshold(app, entry)
        {
            continue;
        }
        if app
            .sidebar_presentation
            .host_filter
            .as_deref()
            .is_some_and(|host| sidebar_machine_host(app, entry) != host)
        {
            continue;
        }
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
        groups
            .entry(ProjectKey(key))
            .or_insert_with(|| (title, Vec::new()))
            .1
            .push(entry);
    }
    let activity_sort =
        app.sidebar_presentation.fleet_sort == crate::app::state::FleetSidebarSort::Activity;
    let mut groups = groups.into_iter().collect::<Vec<_>>();
    // Compare only visible agents for urgency/activity. Working-only projects
    // remain last in either mode; their own newest activity breaks ties.
    groups.sort_by_cached_key(|(key, (_, entries))| {
        let mut urgency = 3;
        let mut newest = None;
        for entry in entries {
            let state = state_index(app, entry);
            if state != 3 {
                urgency = urgency.min(state);
                newest = newest.max(compact_activity_instant(entry));
            }
        }
        if urgency == 3 {
            newest = entries
                .iter()
                .filter_map(|entry| compact_activity_instant(entry))
                .max();
        }
        (
            urgency == 3,
            if activity_sort { 0 } else { urgency },
            std::cmp::Reverse(newest),
            *key,
        )
    });
    let mut rows = vec![SidebarRow::SectionHeader {
        title: PROJECTS,
        count: 0,
        host_counts: Vec::new(),
        collapsed: false,
    }];
    for (project, (name, mut entries)) in groups {
        entries.sort_by_key(|entry| {
            (
                if activity_sort {
                    0
                } else {
                    state_index(app, entry)
                },
                std::cmp::Reverse(compact_activity_instant(entry)),
            )
        });
        let total = entries.len();
        let working = entries
            .iter()
            .filter(|entry| state_index(app, entry) == 3)
            .count();
        let visible = total - working;
        let working_collapsed =
            !expand_collapsed && section_is_collapsed(app, &working_key(project.0));
        let collapsed =
            !expand_collapsed && section_is_collapsed(app, &project_section_key(project.0));
        let only_working = visible == 0;
        rows.push(SidebarRow::NestedHeader {
            key: if only_working {
                working_key(project.0)
            } else {
                project_section_key(project.0)
            },
            action_key: None,
            sort_key: None,
            sort_mode: SidebarSortMode::Default,
            title: name.into(),
            count: visible,
            activity_count: Some((working, total)),
            collapsed: if only_working {
                working_collapsed
            } else {
                collapsed
            },
            dim: only_working,
            status: None,
            spawn: false,
        });
        if !only_working && collapsed {
            continue;
        }
        let (working_entries, visible_entries): (Vec<_>, Vec<_>) = entries
            .into_iter()
            .partition(|entry| state_index(app, entry) == 3);
        let append = |rows: &mut Vec<SidebarRow>, entries: Vec<&AgentPanelEntry>, dim| {
            append_tab_rows(
                rows,
                entries
                    .into_iter()
                    .map(|entry| {
                        let mut entry = entry.clone();
                        entry.space_label_redundant = true;
                        entry.working_shelf = dim;
                        entry
                    })
                    .collect(),
                2,
            );
        };
        append(&mut rows, visible_entries, false);
        if working > 0 {
            if !only_working {
                rows.push(SidebarRow::NestedHeader {
                    key: working_key(project.0),
                    action_key: None,
                    sort_key: None,
                    sort_mode: SidebarSortMode::Default,
                    title: "working".into(),
                    count: working,
                    activity_count: None,
                    collapsed: working_collapsed,
                    dim: true,
                    status: None,
                    spawn: false,
                });
            }
            if !working_collapsed {
                append(&mut rows, working_entries, true);
            }
        }
    }
    if rows.len() > 1 {
        rows.push(SidebarRow::ShelfDivider);
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
        let hidden = rows(&app, &entries, false);
        assert_eq!(
            hidden
                .iter()
                .filter(
                    |row| matches!(row, SidebarRow::NestedHeader { title, .. } if title == "herdr")
                )
                .count(),
            1
        );
        assert!(!hidden
            .iter()
            .any(|row| matches!(row, SidebarRow::Tab { .. } | SidebarRow::RemoteAgent { .. })));
        let expanded = rows(&app, &entries, true);
        let hosts = expanded
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
        assert!(!expanded.iter().any(
            |row| matches!(row, SidebarRow::NestedHeader { key, .. } if key.starts_with("device:"))
        ));
        assert!(expanded.iter().all(
            |row| !matches!(row, SidebarRow::SectionHeader { title, .. } if *title != PROJECTS)
        ));
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
        assert_eq!(projects, [("fleet-working:org/herdr", 0)]);
        app.collapsed_sidebar_groups
            .insert("sections:fleet-project:org/herdr".into());
        assert_eq!(
            collapsed_placement(&app, &entries[0]),
            Some((PROJECTS.into(), "Herdr".into()))
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
        let rows = rows(&app, &entries, true);
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
    fn four_states_order_and_working_default_collapse() {
        let (app, entries) = fixture();
        let mut blocked = entries[0].clone();
        blocked.attention_tier = Some(AttentionTier::Blocked);
        let mut waiting = entries[0].clone();
        waiting.attention_tier = Some(AttentionTier::Attention);
        let mut idle = entries[0].clone();
        idle.state = AgentState::Idle;
        idle.attention_tier = Some(AttentionTier::None);
        let entries = [waiting, entries[0].clone(), idle, blocked];
        let projected = rows(&app, &entries, false);
        let order = projected
            .iter()
            .filter_map(|row| match row {
                SidebarRow::Tab { entry, .. } => Some(state_index(&app, entry)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(order, [0, 1, 2]);
        assert!(projected.iter().any(|row| matches!(
            row,
            SidebarRow::NestedHeader {
                activity_count: Some((1, 4)),
                count: 3,
                ..
            }
        )));
        assert!(projected.iter().any(|row| matches!(row, SidebarRow::NestedHeader { title, collapsed: true, dim: true, .. } if title == "working")));
    }

    #[test]
    fn deferred_entries_stay_on_shelves_and_do_not_affect_projects() {
        let (mut app, mut entries) = fixture();
        let mut snoozed = entries[1].clone();
        std::sync::Arc::make_mut(snoozed.remote_entry.as_mut().unwrap()).snoozed_until =
            Some(u64::MAX);
        let mut settled = entries[2].clone();
        std::sync::Arc::make_mut(settled.remote_entry.as_mut().unwrap()).settled = true;
        std::sync::Arc::make_mut(snoozed.remote_entry.as_mut().unwrap())
            .agent_ref
            .agent = "snoozed-agent".into();
        snoozed = remote_agent_as_panel_entry(snoozed.remote_entry.as_ref().unwrap());
        let remote = std::sync::Arc::make_mut(settled.remote_entry.as_mut().unwrap());
        remote.agent_ref.agent = "settled-agent".into();
        remote.work_context.repo = Some("org/deferred-only".into());
        settled = remote_agent_as_panel_entry(settled.remote_entry.as_ref().unwrap());
        entries[0].state = AgentState::Idle;
        let mut expired = entries[0].clone();
        expired.state = AgentState::Idle;
        expired.seen = false;
        expired.stale = false;
        expired.done_since =
            Some(app.view_observed_at - app.hide_done_after - std::time::Duration::from_secs(1));
        assert!(entry_is_past_done_hide_threshold(&app, &expired));
        let deferred = [snoozed, settled, expired];
        for entry in &deferred {
            assert!(collapsed_placement(&app, entry).is_some());
        }
        for sort in [
            crate::app::state::FleetSidebarSort::Status,
            crate::app::state::FleetSidebarSort::Activity,
        ] {
            app.sidebar_presentation.fleet_sort = sort;
            let active_rows = rows(&app, &entries, false);
            let with_deferred = rows(&app, entries.iter().chain(&deferred), false);
            // Includes all project headers/counts, ordering, and projected rows.
            let headers = |rows: &[SidebarRow]| {
                rows.iter()
                    .filter_map(|row| match row {
                        SidebarRow::NestedHeader {
                            key,
                            count,
                            activity_count,
                            ..
                        } => Some((key.clone(), *count, *activity_count)),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(headers(&active_rows), headers(&with_deferred));
            assert_eq!(active_rows.len(), with_deferred.len());
        }
        // Restore the existing shelves, including remote entries and done expiry.
        entries.extend(deferred[..2].iter().cloned());
        app.remote_agent_panel_entries = entries
            .iter()
            .filter_map(|entry| entry.remote_entry.clone())
            .collect();
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let pane = app.workspaces[0].tabs[0].panes.get_mut(&pane_id).unwrap();
        pane.seen = false;
        pane.done_since = deferred[2].done_since;
        let terminal_id = pane.attached_terminal_id.clone();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_raw_agent_state_for_test(AgentState::Idle);
        let projected = sidebar_rows(&app);
        for (title, expected) in [(SNOOZED_SECTION_TITLE, 1), (SETTLED_SECTION_TITLE, 2)] {
            assert!(
                projected.iter().any(|row| matches!(row,
                SidebarRow::SectionHeader { title: actual, count, collapsed: true, .. }
                if *actual == title && *count == expected)),
                "missing collapsed {title} shelf with {expected} entries"
            );
        }
        for width in [18, 28] {
            let mut terminal = Terminal::new(TestBackend::new(width, 2)).unwrap();
            terminal
                .draw(|frame| {
                    for (y, title, count) in
                        [(0, SNOOZED_SECTION_TITLE, 1), (1, SETTLED_SECTION_TITLE, 2)]
                    {
                        render_section_header(
                            &app,
                            frame,
                            &SectionHeaderArea {
                                title,
                                rect: Rect::new(0, y, width, 1),
                            },
                            count,
                            &[],
                            true,
                        );
                    }
                })
                .unwrap();
            println!("DEFERRED SHELVES width={width}");
            for y in 0..2 {
                let line = (0..width)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>();
                assert!(!line.is_empty());
                println!("{}", line.trim_end());
            }
        }
        let coverage = sidebar_coverage(&app, &TerminalRuntimeRegistry::new());
        assert!(
            coverage.iter().all(|row| row.placement.is_some()),
            "{coverage:?}"
        );
    }

    #[test]
    fn active_unknown_agents_wait_on_local_and_remote_hosts_but_settled_stays_hidden() {
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
                *entry = remote_agent_as_panel_entry(entry.remote_entry.as_ref().unwrap());
            } else {
                entry.state = AgentState::Unknown;
                entry.attention_tier = Some(AttentionTier::None);
            }
        }
        let mut settled = entries.pop().unwrap();
        std::sync::Arc::make_mut(settled.remote_entry.as_mut().unwrap()).settled = true;
        entries.push(settled);
        let projected = rows(&app, &entries, false);
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
            assert_eq!(state_index(&app, entry), 2);
            assert_eq!(collapsed_placement(&app, entry), None);
            assert_eq!(agent_dot_tooltip(entry), "?");
            for width in [18, 28] {
                assert!(rendered_row(&app, entry, width).contains('?'));
            }
        }
        assert_eq!(state_index(&app, &entries[2]), 1);
        assert_eq!(
            collapsed_placement(&app, &entries[2]),
            Some((SETTLED_SECTION_TITLE.into(), SETTLED_SECTION_TITLE.into()))
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
        app.toggle_sidebar_group("fleet-working:pinned");
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
            SidebarRow::NestedHeader {
                activity_count: Some((1, 1)),
                collapsed: true,
                ..
            }
        )));
        assert!(!working
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
            SidebarRow::NestedHeader {
                activity_count: Some((0, 1)),
                ..
            }
        )));
        assert!(idle.iter().any(|row| matches!(row, SidebarRow::Tab { .. })));
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
                app.sidebar_sections_layout = true;
                // Profile populated visible rows as well as pane metadata;
                // default-hidden folds would otherwise hide the render cost.
                app.toggle_sidebar_group("fleet-working:org/herdr");
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
                app.sidebar_width = 28;
                app.reconcile_sidebar_presentation();
                assert_eq!(super::super::sidebar_rows(&app).iter().filter(|row| matches!(row, SidebarRow::Tab { entry, .. } if entry.working_shelf)).count(), if active_panes { 1 } else { count });
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
    fn projects_sort_by_visible_urgency_then_age_and_working_only_stays_last() {
        let (mut app, entries) = fixture();
        let mut agents = Vec::new();
        for (index, (repo, state)) in [
            ("org/z-blocked", 0),
            ("org/a-done", 1),
            ("org/b-waiting", 2),
            ("org/only-working", 3),
        ]
        .into_iter()
        .enumerate()
        {
            let mut entry = entries[1].clone();
            let remote = std::sync::Arc::make_mut(entry.remote_entry.as_mut().unwrap());
            remote.work_context.repo = Some(repo.into());
            remote.entry.state = if state == 1 {
                AgentState::Idle
            } else {
                AgentState::Working
            };
            remote.entry.attention_tier = Some(match state {
                0 => AttentionTier::Blocked,
                2 => AttentionTier::Attention,
                _ => AttentionTier::None,
            });
            remote.entry.activity_at = Some(
                app.view_observed_at - std::time::Duration::from_secs((4 - index) as u64 * 60),
            );
            remote.entry.agent_activity_at = remote.entry.activity_at;
            remote.entry.human_activity_at = None;
            remote.entry.last_turn_at = None;
            entry = remote_agent_as_panel_entry(entry.remote_entry.as_ref().unwrap());
            agents.push(entry);
        }
        let names = |app: &AppState| {
            rows(app, &agents, false)
                .into_iter()
                .filter_map(|row| match row {
                    SidebarRow::NestedHeader { title, .. } => Some(title),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(&app),
            ["z-blocked", "a-done", "b-waiting", "only-working"]
        );
        app.sidebar_presentation.fleet_sort = crate::app::state::FleetSidebarSort::Activity;
        assert_eq!(
            names(&app),
            ["b-waiting", "a-done", "z-blocked", "only-working"]
        );
    }

    #[test]
    fn markers_and_expanded_working_rows_are_distinct_and_dim() {
        let (mut app, mut entries) = fixture();
        for (state, expected, color) in [
            (AgentState::Blocked, "!", app.palette.red),
            (AgentState::Idle, "+", app.palette.green),
            (AgentState::Unknown, "?", app.palette.overlay0),
            (AgentState::Working, "~", app.palette.blue),
        ] {
            entries[0].state = state;
            entries[0].attention_tier = None;
            assert_eq!(marker(&app, &entries[0]), (expected, color));
            assert!(rendered_row(&app, &entries[0], 28).contains(expected));
        }
        app.nerd_font = true;
        assert_eq!(marker(&app, &entries[0]), ("●", app.palette.blue));
        let expanded = rows(&app, &entries, true);
        assert!(expanded
            .iter()
            .any(|row| matches!(row, SidebarRow::Tab { entry, .. } if entry.working_shelf)));
        assert!(expanded.iter().any(|row| matches!(
            row,
            SidebarRow::RemoteAgent {
                working_shelf: true,
                ..
            }
        )));
        let local = expanded
            .iter()
            .find_map(|row| match row {
                SidebarRow::Tab { entry, .. } => Some(entry),
                _ => None,
            })
            .unwrap();
        let mut terminal = Terminal::new(TestBackend::new(28, 1)).unwrap();
        terminal
            .draw(|frame| {
                render_compact_agent_row_with_prefix(
                    &app,
                    frame,
                    local,
                    Rect::new(0, 0, 28, 1),
                    2,
                    true,
                    None,
                    None,
                )
            })
            .unwrap();
        assert!((0..28).any(|x| {
            let cell = &terminal.backend().buffer()[(x, 0)];
            cell.symbol() == "●" && cell.modifier.contains(Modifier::DIM)
        }));
    }

    #[test]
    fn project_headers_render_visible_total_and_working_only_summary() {
        let (mut app, _) = fixture();
        for width in [18, 28, app.default_sidebar_width] {
            for (key, title, count, activity_count, expected) in [
                (
                    "fleet-project:org/herdr",
                    "herdr",
                    3,
                    Some((4, 7)),
                    "herdr 3 of 7",
                ),
                (
                    "fleet-project:org/herdr",
                    "herdr",
                    3,
                    Some((0, 3)),
                    "herdr 3",
                ),
                (
                    "fleet-working:org/mable",
                    "mable",
                    0,
                    Some((3, 3)),
                    "mable ~ 3 working",
                ),
                (
                    "fleet-working:org/herdr",
                    "working",
                    4,
                    None,
                    "~ 4 working ▸",
                ),
            ] {
                let header = NestedHeaderArea {
                    key: key.into(),
                    action_key: None,
                    sort_key: None,
                    sort_mode: SidebarSortMode::Default,
                    title: title.into(),
                    count,
                    activity_count,
                    collapsed: true,
                    dim: key.starts_with("fleet-working:"),
                    status: None,
                    spawn: false,
                    rect: Rect::new(0, 0, width, 1),
                };
                let mut terminal = Terminal::new(TestBackend::new(width, 1)).unwrap();
                terminal
                    .draw(|frame| render_fleet_header(&app, frame, &header))
                    .unwrap();
                let text = (0..width)
                    .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
                    .collect::<String>();
                // At 18 columns the working-only title yields before its count.
                if width > 18 || activity_count != Some((3, 3)) {
                    assert!(text.contains(expected), "{width}: {text:?}");
                }
                if header.dim {
                    assert!(terminal.backend().buffer()[(2, 0)]
                        .modifier
                        .contains(Modifier::DIM));
                }
            }
        }
        app.sidebar_presentation.fleet_sort = crate::app::state::FleetSidebarSort::Activity;
        let mut other = crate::app::state::SidebarPresentationState::default();
        app.swap_sidebar_presentation(&mut other);
        assert_eq!(
            app.sidebar_presentation.fleet_sort,
            crate::app::state::FleetSidebarSort::Status
        );
        assert_eq!(
            other.fleet_sort,
            crate::app::state::FleetSidebarSort::Activity
        );
    }

    #[test]
    fn rendered_project_snapshots_status_expanded_and_activity() {
        let (mut app, mut entries) = fixture();
        for (name, state, tier, age) in [
            (
                "billing review",
                AgentState::Working,
                AttentionTier::Blocked,
                240,
            ),
            ("QA ready", AgentState::Idle, AttentionTier::None, 120),
            (
                "choose layout",
                AgentState::Unknown,
                AttentionTier::None,
                60,
            ),
        ] {
            let mut entry = entries[0].clone();
            entry.primary_tab_label = Some(name.into());
            entry.state = state;
            entry.attention_tier = Some(tier);
            entry.activity_at = Some(app.view_observed_at - std::time::Duration::from_secs(age));
            entry.agent_activity_at = entry.activity_at;
            entry.human_activity_at = None;
            entry.last_turn_at = None;
            entries.push(entry);
        }
        let mut background = entries[1].clone();
        std::sync::Arc::make_mut(background.remote_entry.as_mut().unwrap())
            .work_context
            .repo = Some("org/mable".into());
        entries.push(background);
        for width in [app.default_sidebar_width, 28] {
            for mode in ["status", "expanded", "activity"] {
                app.sidebar_presentation.fleet_sort = if mode == "activity" {
                    crate::app::state::FleetSidebarSort::Activity
                } else {
                    crate::app::state::FleetSidebarSort::Status
                };
                app.collapsed_sidebar_groups.clear();
                if mode == "expanded" {
                    app.toggle_sidebar_group("fleet-working:org/herdr");
                }
                let projected = rows(&app, &entries, false);
                let height = projected.len() as u16;
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal
                    .draw(|frame| {
                        for (y, row) in projected.iter().enumerate() {
                            let rect = Rect::new(0, y as u16, width, 1);
                            match row {
                                SidebarRow::SectionHeader {
                                    title,
                                    count,
                                    host_counts,
                                    collapsed,
                                } => render_section_header(
                                    &app,
                                    frame,
                                    &SectionHeaderArea { title, rect },
                                    *count,
                                    host_counts,
                                    *collapsed,
                                ),
                                SidebarRow::NestedHeader {
                                    key,
                                    title,
                                    count,
                                    activity_count,
                                    collapsed,
                                    dim,
                                    ..
                                } => render_fleet_header(
                                    &app,
                                    frame,
                                    &NestedHeaderArea {
                                        key: key.clone(),
                                        action_key: None,
                                        sort_key: None,
                                        sort_mode: SidebarSortMode::Default,
                                        title: title.clone(),
                                        count: *count,
                                        activity_count: *activity_count,
                                        collapsed: *collapsed,
                                        dim: *dim,
                                        status: None,
                                        spawn: false,
                                        rect,
                                    },
                                ),
                                SidebarRow::Tab { entry, depth, .. } => {
                                    render_compact_agent_row_with_prefix(
                                        &app, frame, entry, rect, *depth, true, None, None,
                                    )
                                }
                                SidebarRow::RemoteAgent {
                                    entry,
                                    depth,
                                    working_shelf,
                                    ..
                                } => render_remote_compact_agent_row_body(
                                    &app,
                                    frame,
                                    entry,
                                    rect,
                                    *depth,
                                    None,
                                    None,
                                    false,
                                    *working_shelf,
                                ),
                                _ => {}
                            }
                        }
                    })
                    .unwrap();
                println!("SNAPSHOT {mode} width={width}");
                let mut snapshot = String::new();
                for y in 0..height {
                    let text = (0..width)
                        .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                        .collect::<String>();
                    println!("{}", text.trim_end());
                    snapshot.push_str(&text);
                }
                assert!(snapshot.contains("3 of 6"), "{snapshot}");
                assert!(snapshot.contains("working"));
                println!("END SNAPSHOT");
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
