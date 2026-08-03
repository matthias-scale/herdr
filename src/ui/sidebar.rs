mod tokens;

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use self::tokens::{ResolvedToken, ResolvedTokenKind, SpaceTokenContext};
use super::scrollbar::{render_scrollbar, should_show_scrollbar};
use super::status::{state_dot, state_label, state_label_color};
use super::text::{display_width, display_width_u16, truncate_end};
use crate::app::state::Palette;
use crate::app::{AppState, Mode};
use crate::detect::AgentState;
use crate::terminal::TerminalRuntimeRegistry;

const WORKSPACE_SECTION_HEADER_ROWS: u16 = 1;
const AGENT_ACTIVITY_AGE_FIELD_WIDTH: usize = 5;
const AGENT_ACTIVITY_AGE_MIN_CONTENT_WIDTH: usize = 8;
const DEFAULT_THREAD_TITLE: &str = "New Thread";

#[derive(Clone)]
pub(crate) struct AgentPanelEntry {
    pub ws_idx: usize,
    pub tab_idx: usize,
    pub pane_id: crate::layout::PaneId,
    pub primary_label: String,
    pub primary_tab_label: Option<String>,
    pub pane_label: Option<String>,
    pub terminal_title: Option<String>,
    pub terminal_title_stripped: Option<String>,
    pub agent_label: Option<String>,
    pub agent_kind_label: Option<String>,
    pub agent: Option<crate::detect::Agent>,
    pub state: AgentState,
    pub seen: bool,
    pub last_agent_state_change_seq: Option<u64>,
    pub activity_at: Option<std::time::Instant>,
    pub state_labels: std::collections::HashMap<String, String>,
    pub tokens: std::collections::HashMap<String, String>,
    /// First pane in canonical layout order for its tab. The renderer uses it
    /// to project the tab row exactly once before its pane children.
    pub tab_first_pane: bool,
}

pub(crate) fn expanded_sidebar_sections(area: Rect, _split_ratio: f32) -> (Rect, Rect) {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height == 0 {
        return (Rect::default(), Rect::default());
    }
    // Both legacy callers receive the same unified content rectangle. The
    // sidebar no longer has independent Spaces and Agents sections.
    (content, content)
}

pub(crate) fn agent_panel_entries(app: &AppState) -> Vec<AgentPanelEntry> {
    agent_panel_entries_with_runtimes(app, None)
}

pub(crate) fn all_agent_panel_entries(app: &AppState) -> Vec<AgentPanelEntry> {
    collect_agent_panel_entries_with_runtimes(app, None)
}

pub(crate) fn sidebar_thread_entries(app: &AppState) -> Vec<AgentPanelEntry> {
    collect_sidebar_thread_entries_with_runtimes(app, None)
}

pub(crate) fn sidebar_thread_entries_from(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Vec<AgentPanelEntry> {
    collect_sidebar_thread_entries_with_runtimes(app, Some(terminal_runtimes))
}

pub(crate) fn relative_agent_navigation_entry(
    app: &AppState,
    forward: bool,
) -> Option<(usize, AgentPanelEntry)> {
    let entries = all_agent_panel_entries(app);
    if entries.is_empty() {
        return None;
    }
    let focused = app.active.and_then(|ws_idx| {
        app.workspaces
            .get(ws_idx)
            .and_then(crate::workspace::Workspace::focused_pane_id)
            .map(|pane_id| (ws_idx, pane_id))
    });
    let current_idx = entries.iter().position(|entry| {
        focused.is_some_and(|(ws_idx, pane_id)| entry.ws_idx == ws_idx && entry.pane_id == pane_id)
    });
    let next_idx = match (current_idx, forward) {
        (Some(idx), true) => (idx + 1) % entries.len(),
        (Some(0), false) => entries.len() - 1,
        (Some(idx), false) => idx - 1,
        (None, true) => 0,
        (None, false) => entries.len() - 1,
    };
    entries
        .into_iter()
        .nth(next_idx)
        .map(|entry| (next_idx, entry))
}

pub(crate) fn agent_panel_entries_from(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Vec<AgentPanelEntry> {
    agent_panel_entries_with_runtimes(app, Some(terminal_runtimes))
}

fn agent_panel_entries_with_runtimes(
    app: &AppState,
    terminal_runtimes: Option<&TerminalRuntimeRegistry>,
) -> Vec<AgentPanelEntry> {
    let mut entries = collect_agent_panel_entries_with_runtimes(app, terminal_runtimes);
    crate::app::agent_view::apply_agent_view(app, &mut entries);
    entries
}

fn collect_agent_panel_entries_with_runtimes(
    app: &AppState,
    terminal_runtimes: Option<&TerminalRuntimeRegistry>,
) -> Vec<AgentPanelEntry> {
    let empty_runtimes;
    let terminal_runtimes = match terminal_runtimes {
        Some(terminal_runtimes) => terminal_runtimes,
        None => {
            empty_runtimes = TerminalRuntimeRegistry::new();
            &empty_runtimes
        }
    };

    app.workspaces
        .iter()
        .enumerate()
        .flat_map(|(ws_idx, ws)| {
            let workspace_label = ws.display_name_from(&app.terminals, terminal_runtimes);
            ws.pane_details(&app.terminals)
                .into_iter()
                .map(move |detail| {
                    let thread_title = ws
                        .tabs
                        .get(detail.tab_idx)
                        .and_then(|tab| tab.custom_name.clone())
                        .unwrap_or_else(|| DEFAULT_THREAD_TITLE.to_string());
                    AgentPanelEntry {
                        ws_idx,
                        tab_idx: detail.tab_idx,
                        pane_id: detail.pane_id,
                        primary_label: workspace_label.clone(),
                        primary_tab_label: Some(thread_title),
                        pane_label: detail.pane_label,
                        terminal_title: detail.terminal_title,
                        terminal_title_stripped: detail.terminal_title_stripped,
                        agent_label: Some(detail.agent_label),
                        agent_kind_label: detail.agent_kind_label,
                        agent: detail.agent,
                        state: detail.state,
                        seen: detail.seen,
                        last_agent_state_change_seq: detail.last_agent_state_change_seq,
                        activity_at: detail.activity_at,
                        state_labels: detail.state_labels,
                        tokens: detail.tokens,
                        tab_first_pane: false,
                    }
                })
        })
        .collect()
}

fn collect_sidebar_thread_entries_with_runtimes(
    app: &AppState,
    terminal_runtimes: Option<&TerminalRuntimeRegistry>,
) -> Vec<AgentPanelEntry> {
    // `Workspace::pane_details` is canonical workspace, tab and layout-pane
    // order and includes agentless terminals. Do not apply attention sorting:
    // lifecycle changes must never move sidebar rows.
    let mut entries = collect_agent_panel_entries_with_runtimes(app, terminal_runtimes);
    let mut previous_tab = None;
    for entry in &mut entries {
        let tab = (entry.ws_idx, entry.tab_idx);
        entry.tab_first_pane = previous_tab != Some(tab);
        previous_tab = Some(tab);
    }
    entries
}

pub(super) fn agent_panel_status_key(state: AgentState, seen: bool) -> &'static str {
    match (state, seen) {
        (AgentState::Idle, false) => "done",
        (AgentState::Idle, true) => "idle",
        (AgentState::Working, _) => "working",
        (AgentState::Blocked, _) => "blocked",
        (AgentState::Unknown, _) => "unknown",
    }
}

fn workspace_row_height(app: &AppState, ws: &crate::workspace::Workspace, indented: bool) -> u16 {
    let (state, seen) = ws.aggregate_state(&app.terminals);
    let label = if indented {
        grouped_child_display_label(
            &ws.display_name_from_terminals(&app.terminals),
            ws.branch().as_deref(),
            ws.custom_name.is_some(),
        )
    } else {
        ws.display_name_from_terminals(&app.terminals)
    };
    let token_values = ws.metadata_tokens.values();
    tokens::space_rows(
        &app.sidebar_spaces,
        SpaceTokenContext {
            workspace: &label,
            branch: ws.branch().as_deref(),
            state_text: state_label(state, seen),
            ahead_behind: ws.git_ahead_behind(),
            tokens: &token_values,
            suppress_git_details: indented,
        },
    )
    .len()
    .max(1)
    .min(u16::MAX as usize) as u16
}

fn workspace_row_height_in_body(
    app: &AppState,
    workspace: &crate::workspace::Workspace,
    indented: bool,
    body_height: u16,
) -> u16 {
    workspace_row_height(app, workspace, indented).min(body_height)
}

fn workspace_attention_priority(state: AgentState, seen: bool) -> u8 {
    match (state, seen) {
        (AgentState::Blocked, _) => 4,
        (AgentState::Idle, false) => 3,
        (AgentState::Working, _) => 2,
        (AgentState::Idle, true) => 1,
        (AgentState::Unknown, _) => 0,
    }
}

/// Lifecycle precedence for a single tab/window. A completed pane must never
/// mask work that is still running in another pane owned by the same tab.
fn tab_lifecycle_priority(state: AgentState, seen: bool) -> u8 {
    match (state, seen) {
        (AgentState::Blocked, _) => 4,
        (AgentState::Working, _) => 3,
        (AgentState::Idle, false) => 2,
        (AgentState::Idle, true) => 1,
        (AgentState::Unknown, _) => 0,
    }
}

fn space_aggregate_state(app: &AppState, key: &str) -> (AgentState, bool) {
    app.workspaces
        .iter()
        .filter(|ws| ws.worktree_space().is_some_and(|space| space.key == key))
        .map(|ws| ws.aggregate_state(&app.terminals))
        .max_by_key(|(state, seen)| workspace_attention_priority(*state, *seen))
        .unwrap_or((AgentState::Unknown, true))
}

pub(crate) fn workspace_parent_group_state(
    app: &AppState,
    ws_idx: usize,
) -> Option<(String, bool)> {
    let space = app.workspaces.get(ws_idx)?.worktree_space()?;
    if space.is_linked_worktree {
        return None;
    }
    let member_count = app
        .workspaces
        .iter()
        .filter(|ws| {
            ws.worktree_space()
                .is_some_and(|member| member.key == space.key)
        })
        .count();
    (member_count >= 2).then(|| {
        (
            space.key.clone(),
            app.collapsed_space_keys.contains(&space.key),
        )
    })
}

pub(crate) fn grouped_child_display_label(
    label: &str,
    branch: Option<&str>,
    has_custom_name: bool,
) -> String {
    if has_custom_name {
        return label.to_string();
    }
    let Some(branch) = branch else {
        return label.to_string();
    };
    branch
        .strip_prefix("worktree/")
        .unwrap_or(branch)
        .to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspaceListEntry {
    Workspace { ws_idx: usize, indented: bool },
}

#[derive(Clone)]
pub(crate) enum SidebarRow {
    Workspace {
        ws_idx: usize,
        indented: bool,
    },
    Tab {
        entry: Box<AgentPanelEntry>,
        depth: u16,
    },
    Agent {
        entry: Box<AgentPanelEntry>,
        depth: u16,
    },
}

pub(crate) fn sidebar_rows(app: &AppState) -> Vec<SidebarRow> {
    sidebar_rows_inner(app, None, false)
}

fn sidebar_rows_from(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Vec<SidebarRow> {
    sidebar_rows_inner(app, Some(terminal_runtimes), false)
}

pub(crate) fn mobile_sidebar_rows(app: &AppState) -> Vec<SidebarRow> {
    sidebar_rows_inner(app, None, true)
}

pub(crate) fn mobile_sidebar_rows_from(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Vec<SidebarRow> {
    sidebar_rows_inner(app, Some(terminal_runtimes), true)
}

fn sidebar_rows_inner(
    app: &AppState,
    terminal_runtimes: Option<&TerminalRuntimeRegistry>,
    expand_worktrees: bool,
) -> Vec<SidebarRow> {
    if !app.sidebar_shows_spaces_tree() {
        let entries = match terminal_runtimes {
            Some(runtimes) => agent_panel_entries_from(app, runtimes),
            None => agent_panel_entries(app),
        };
        return entries
            .into_iter()
            .map(|entry| SidebarRow::Agent {
                entry: Box::new(entry),
                depth: 0,
            })
            .collect();
    }

    let agents = match terminal_runtimes {
        Some(runtimes) => sidebar_thread_entries_from(app, runtimes),
        None => sidebar_thread_entries(app),
    };
    let mut agents_by_workspace = std::collections::HashMap::<usize, Vec<AgentPanelEntry>>::new();
    for entry in agents {
        agents_by_workspace
            .entry(entry.ws_idx)
            .or_default()
            .push(entry);
    }

    let mut rows = Vec::new();
    let workspaces = if expand_worktrees {
        workspace_list_entries_expanded(app)
    } else {
        workspace_list_entries(app)
    };
    for workspace in workspaces {
        let WorkspaceListEntry::Workspace { ws_idx, indented } = workspace;
        rows.push(SidebarRow::Workspace { ws_idx, indented });
        if app.workspace_agents_expanded(ws_idx) {
            let depth = if indented { 2 } else { 1 };
            if let Some(entries) = agents_by_workspace.remove(&ws_idx) {
                let tab_states = entries.iter().fold(
                    std::collections::HashMap::<usize, (AgentState, bool)>::new(),
                    |mut states, entry| {
                        let candidate = (entry.state, entry.seen);
                        states
                            .entry(entry.tab_idx)
                            .and_modify(|state| {
                                if tab_lifecycle_priority(candidate.0, candidate.1)
                                    > tab_lifecycle_priority(state.0, state.1)
                                {
                                    *state = candidate;
                                }
                            })
                            .or_insert(candidate);
                        states
                    },
                );
                let mut current_tab = None;
                for entry in entries {
                    let tab = entry.tab_idx;
                    if current_tab != Some(tab) {
                        let mut tab_entry = entry.clone();
                        if let Some((state, seen)) = tab_states.get(&tab) {
                            tab_entry.state = *state;
                            tab_entry.seen = *seen;
                        }
                        rows.push(SidebarRow::Tab {
                            entry: Box::new(tab_entry),
                            depth,
                        });
                        current_tab = Some(tab);
                    }
                }
            }
        }
    }
    rows
}

pub(crate) fn next_entry_is_indented_workspace(entries: &[WorkspaceListEntry], idx: usize) -> bool {
    matches!(
        entries.get(idx.saturating_add(1)),
        Some(WorkspaceListEntry::Workspace { indented: true, .. })
    )
}

pub(crate) fn normalized_workspace_scroll(app: &AppState, area: Rect, requested: usize) -> usize {
    let ws_area = workspace_list_rect(area, app.sidebar_section_split);
    let body = workspace_list_body_rect(ws_area, false);
    if body.height == 0 {
        return requested;
    }

    if workspace_list_entries(app).is_empty() {
        0
    } else {
        requested.min(workspace_list_bottom_start(app, ws_area))
    }
}

pub(crate) fn workspace_list_entries(app: &AppState) -> Vec<WorkspaceListEntry> {
    workspace_list_entries_inner(app, false)
}

/// Like [`workspace_list_entries`] but always expands worktree groups, ignoring
/// `collapsed_space_keys`. The mobile switcher has no collapse affordance and
/// always shows the full worktree tree.
pub(crate) fn workspace_list_entries_expanded(app: &AppState) -> Vec<WorkspaceListEntry> {
    workspace_list_entries_inner(app, true)
}

fn workspace_list_entries_inner(app: &AppState, force_expanded: bool) -> Vec<WorkspaceListEntry> {
    let mut members_by_key = std::collections::HashMap::<String, Vec<usize>>::new();
    for (ws_idx, ws) in app.workspaces.iter().enumerate() {
        if let Some(space) = ws.worktree_space() {
            members_by_key
                .entry(space.key.clone())
                .or_default()
                .push(ws_idx);
        }
    }
    let grouped_keys = members_by_key
        .iter()
        .filter(|(_, members)| {
            members.len() >= 2
                && members.iter().any(|idx| {
                    app.workspaces
                        .get(*idx)
                        .and_then(|ws| ws.worktree_space())
                        .is_some_and(|space| !space.is_linked_worktree)
                })
        })
        .map(|(key, _)| key.clone())
        .collect::<std::collections::HashSet<_>>();

    let visible_group_idx = if matches!(app.mode, Mode::Navigate) {
        Some(app.selected)
    } else {
        app.active
    };
    let active_group = visible_group_idx.and_then(|idx| {
        app.workspaces
            .get(idx)
            .and_then(|ws| ws.worktree_space())
            .map(|space| space.key.clone())
    });

    let mut emitted_groups = std::collections::HashSet::<String>::new();
    let mut entries = Vec::new();
    for (ws_idx, ws) in app.workspaces.iter().enumerate() {
        let Some(space) = ws
            .worktree_space()
            .filter(|space| grouped_keys.contains(&space.key))
        else {
            entries.push(WorkspaceListEntry::Workspace {
                ws_idx,
                indented: false,
            });
            continue;
        };

        if !emitted_groups.insert(space.key.clone()) {
            continue;
        }

        let Some(members) = members_by_key.get(&space.key) else {
            continue;
        };
        let Some(parent_idx) = members.iter().copied().find(|idx| {
            app.workspaces
                .get(*idx)
                .and_then(|member| member.worktree_space())
                .is_some_and(|member_space| !member_space.is_linked_worktree)
        }) else {
            entries.push(WorkspaceListEntry::Workspace {
                ws_idx,
                indented: false,
            });
            continue;
        };
        let collapsed = !force_expanded && app.collapsed_space_keys.contains(&space.key);
        entries.push(WorkspaceListEntry::Workspace {
            ws_idx: parent_idx,
            indented: false,
        });

        if collapsed {
            if let Some(active_idx) = visible_group_idx
                .filter(|idx| *idx != parent_idx)
                .filter(|_| active_group.as_deref() == Some(space.key.as_str()))
            {
                entries.push(WorkspaceListEntry::Workspace {
                    ws_idx: active_idx,
                    indented: true,
                });
            }
        } else {
            for member_idx in members {
                if *member_idx == parent_idx {
                    continue;
                }
                entries.push(WorkspaceListEntry::Workspace {
                    ws_idx: *member_idx,
                    indented: true,
                });
            }
        }
    }
    entries
}

pub(crate) fn workspace_list_rect(area: Rect, split_ratio: f32) -> Rect {
    let (ws_area, _) = expanded_sidebar_sections(area, split_ratio);
    ws_area
}

pub(crate) fn workspace_list_body_rect(area: Rect, has_scrollbar: bool) -> Rect {
    if area.width == 0 || area.height <= WORKSPACE_SECTION_HEADER_ROWS {
        return Rect::default();
    }

    let body_y = area.y.saturating_add(WORKSPACE_SECTION_HEADER_ROWS);
    let body_height = area.y.saturating_add(area.height).saturating_sub(body_y);
    let body_width = area.width.saturating_sub(u16::from(has_scrollbar));
    Rect::new(area.x, body_y, body_width, body_height)
}

fn sidebar_row_height(app: &AppState, row: &SidebarRow, body_height: u16) -> u16 {
    match row {
        SidebarRow::Workspace { ws_idx, indented } => app
            .workspaces
            .get(*ws_idx)
            .map(|workspace| workspace_row_height_in_body(app, workspace, *indented, body_height))
            .unwrap_or(0),
        SidebarRow::Agent { entry, .. } => agent_entry_height_in_body(app, entry, body_height),
        SidebarRow::Tab { .. } => 1,
    }
}

fn sidebar_row_gap(app: &AppState, rows: &[SidebarRow], row_idx: usize) -> u16 {
    let Some(row) = rows.get(row_idx) else {
        return 0;
    };
    let Some(next) = rows.get(row_idx + 1) else {
        return 0;
    };
    match (row, next) {
        (SidebarRow::Workspace { .. }, SidebarRow::Tab { .. }) => 0,
        (SidebarRow::Tab { .. }, SidebarRow::Agent { .. }) => 0,
        (SidebarRow::Workspace { .. }, SidebarRow::Agent { .. }) => 0,
        (
            SidebarRow::Workspace { indented: true, .. },
            SidebarRow::Workspace { indented: true, .. },
        ) => 0,
        (SidebarRow::Workspace { .. }, SidebarRow::Workspace { .. }) => app.sidebar_spaces.row_gap,
        (SidebarRow::Agent { .. }, SidebarRow::Agent { .. }) => app.sidebar_agents.row_gap,
        (SidebarRow::Agent { .. }, SidebarRow::Workspace { .. }) => app.sidebar_spaces.row_gap,
        (SidebarRow::Tab { .. }, SidebarRow::Workspace { .. }) => app.sidebar_spaces.row_gap,
        (SidebarRow::Agent { .. }, SidebarRow::Tab { .. }) => app.sidebar_agents.row_gap,
        (SidebarRow::Tab { .. }, SidebarRow::Tab { .. }) => app.sidebar_agents.row_gap,
    }
}

fn workspace_list_visible_count(app: &AppState, area: Rect, scroll: usize) -> usize {
    let body = workspace_list_body_rect(area, false);
    if body.width == 0 || body.height == 0 {
        return 0;
    }

    let mut used_rows = 0u16;
    let mut visible = 0usize;
    let entries = sidebar_rows(app);
    for (entry_idx, entry) in entries.iter().enumerate().skip(scroll) {
        let row_height = sidebar_row_height(app, entry, body.height);
        let gap = sidebar_row_gap(app, &entries, entry_idx);
        if used_rows.saturating_add(row_height) > body.height {
            break;
        }
        used_rows = used_rows.saturating_add(row_height);
        visible += 1;
        used_rows = used_rows.saturating_add(gap).min(body.height);
    }
    visible
}

fn workspace_list_bottom_start(app: &AppState, area: Rect) -> usize {
    let body = workspace_list_body_rect(area, false);
    let entries = sidebar_rows(app);
    let mut used_rows = 0u16;
    let mut start = entries.len();
    for (entry_idx, entry) in entries.iter().enumerate().rev() {
        let gap = sidebar_row_gap(app, &entries, entry_idx);
        let needed = sidebar_row_height(app, entry, body.height).saturating_add(gap);
        if used_rows.saturating_add(needed) > body.height {
            break;
        }
        used_rows = used_rows.saturating_add(needed);
        start = entry_idx;
    }
    start.min(entries.len().saturating_sub(1))
}

/// Position of `ws_idx` in the unified sidebar row list, which is the index
/// space `AppState::workspace_scroll` lives in. Flat projections have no
/// workspace rows, so the workspace's first agent row stands in for it.
pub(crate) fn sidebar_row_index_for_workspace(app: &AppState, ws_idx: usize) -> Option<usize> {
    sidebar_rows(app)
        .iter()
        .position(|row| sidebar_row_belongs_to_workspace(row, ws_idx))
}

pub(crate) fn sidebar_row_belongs_to_workspace(row: &SidebarRow, ws_idx: usize) -> bool {
    match row {
        SidebarRow::Workspace { ws_idx: row_ws, .. } => *row_ws == ws_idx,
        SidebarRow::Agent { entry, .. } => entry.ws_idx == ws_idx,
        SidebarRow::Tab { entry, .. } => entry.ws_idx == ws_idx,
    }
}

/// Smallest scroll offset that keeps sidebar row `target` inside the workspace
/// list viewport, starting from `current_scroll`. `area` is the full sidebar
/// rect, matching [`normalized_workspace_scroll`].
pub(crate) fn sidebar_row_scroll_for_target(
    app: &AppState,
    area: Rect,
    current_scroll: usize,
    target: usize,
) -> usize {
    let ws_area = workspace_list_rect(area, app.sidebar_section_split);
    let max_scroll = workspace_list_bottom_start(app, ws_area);
    if target < current_scroll {
        return target.min(max_scroll);
    }

    let mut scroll = current_scroll.min(max_scroll);
    while scroll < target {
        let visible = workspace_list_visible_count(app, ws_area, scroll);
        if visible > 0 && target < scroll.saturating_add(visible) {
            break;
        }
        scroll = scroll.saturating_add(1);
    }
    scroll.min(max_scroll)
}

pub(crate) fn workspace_list_scroll_metrics(
    app: &AppState,
    area: Rect,
) -> crate::pane::ScrollMetrics {
    let max_scroll = workspace_list_bottom_start(app, area);
    let scroll = app.workspace_scroll.min(max_scroll);
    let viewport_rows = workspace_list_visible_count(app, area, scroll);

    crate::pane::ScrollMetrics {
        offset_from_bottom: max_scroll.saturating_sub(scroll),
        max_offset_from_bottom: max_scroll,
        viewport_rows,
    }
}

pub(crate) fn workspace_list_scrollbar_rect(app: &AppState, area: Rect) -> Option<Rect> {
    let metrics = workspace_list_scroll_metrics(app, area);
    let body = workspace_list_body_rect(area, true);
    (should_show_scrollbar(metrics) && body.width > 0 && body.height > 0).then_some(Rect::new(
        area.x + area.width.saturating_sub(1),
        body.y,
        1,
        body.height,
    ))
}

fn resolved_agent_rows(app: &AppState, entry: &AgentPanelEntry) -> Vec<Vec<ResolvedToken>> {
    let label = entry
        .state_labels
        .get(agent_panel_status_key(entry.state, entry.seen))
        .map(String::as_str)
        .unwrap_or_else(|| match entry.state {
            AgentState::Idle => "done",
            _ => state_label(entry.state, entry.seen),
        });
    tokens::agent_rows(&app.sidebar_agents, entry, label)
}

pub(crate) fn agent_entry_height_in_body(
    app: &AppState,
    entry: &AgentPanelEntry,
    body_height: u16,
) -> u16 {
    (resolved_agent_rows(app, entry)
        .len()
        .max(1)
        .saturating_add(usize::from(entry.tab_first_pane))
        .min(u16::MAX as usize) as u16)
        .min(body_height)
}

pub(crate) fn compute_workspace_list_areas(
    app: &AppState,
    area: Rect,
) -> (Vec<crate::app::state::WorkspaceCardArea>, Vec<()>) {
    (compute_sidebar_row_areas(app, area).0, Vec::new())
}

pub(crate) fn compute_sidebar_row_areas(
    app: &AppState,
    area: Rect,
) -> (
    Vec<crate::app::state::WorkspaceCardArea>,
    Vec<crate::app::state::AgentCardArea>,
) {
    let ws_area = workspace_list_rect(area, app.sidebar_section_split);
    if ws_area == Rect::default() {
        return (Vec::new(), Vec::new());
    }

    let metrics = workspace_list_scroll_metrics(app, ws_area);
    let body = workspace_list_body_rect(ws_area, should_show_scrollbar(metrics));
    if body.width == 0 || body.height == 0 {
        return (Vec::new(), Vec::new());
    }

    let scroll = app.workspace_scroll.min(metrics.max_offset_from_bottom);
    let mut row_y = body.y;
    let body_bottom = body.y + body.height;
    let mut cards = Vec::new();
    let mut agent_cards = Vec::new();

    let entries = sidebar_rows(app);
    for (entry_idx, entry) in entries.iter().enumerate().skip(scroll) {
        match entry {
            SidebarRow::Workspace { ws_idx, indented } => {
                let Some(ws) = app.workspaces.get(*ws_idx) else {
                    continue;
                };
                let row_height = workspace_row_height_in_body(app, ws, *indented, body.height);
                if row_y.saturating_add(row_height) > body_bottom {
                    break;
                }
                cards.push(crate::app::state::WorkspaceCardArea {
                    ws_idx: *ws_idx,
                    rect: Rect::new(body.x, row_y, body.width, row_height),
                    indented: *indented,
                });
            }
            SidebarRow::Agent { entry, .. } => {
                let row_height = agent_entry_height_in_body(app, entry, body.height);
                if row_y.saturating_add(row_height) > body_bottom {
                    break;
                }
                agent_cards.push(crate::app::state::AgentCardArea {
                    ws_idx: entry.ws_idx,
                    tab_idx: entry.tab_idx,
                    pane_id: entry.pane_id,
                    rect: Rect::new(body.x, row_y, body.width, row_height),
                });
            }
            SidebarRow::Tab { .. } => {}
        }
        row_y = row_y
            .saturating_add(sidebar_row_height(app, entry, body.height))
            .saturating_add(sidebar_row_gap(app, &entries, entry_idx))
            .min(body_bottom);
    }

    (cards, agent_cards)
}

pub(crate) fn compute_workspace_card_areas(
    app: &AppState,
    area: Rect,
) -> Vec<crate::app::state::WorkspaceCardArea> {
    compute_workspace_list_areas(app, area).0
}

pub(crate) fn compute_agent_card_areas(
    app: &AppState,
    area: Rect,
) -> Vec<crate::app::state::AgentCardArea> {
    compute_sidebar_row_areas(app, area).1
}

pub(crate) fn compute_tab_card_areas(
    app: &AppState,
    area: Rect,
) -> Vec<crate::app::state::TabCardArea> {
    let ws_area = workspace_list_rect(area, app.sidebar_section_split);
    let metrics = workspace_list_scroll_metrics(app, ws_area);
    let body = workspace_list_body_rect(ws_area, should_show_scrollbar(metrics));
    let mut y = body.y;
    let mut out = Vec::new();
    let rows = sidebar_rows(app);
    for (idx, row) in rows
        .iter()
        .enumerate()
        .skip(app.workspace_scroll.min(metrics.max_offset_from_bottom))
    {
        let height = sidebar_row_height(app, row, body.height);
        if y.saturating_add(height) > body.y.saturating_add(body.height) {
            break;
        }
        if let SidebarRow::Tab { entry, .. } = row {
            out.push(crate::app::state::TabCardArea {
                ws_idx: entry.ws_idx,
                tab_idx: entry.tab_idx,
                pane_id: entry.pane_id,
                rect: Rect::new(body.x, y, body.width, height),
            });
        }
        y = y
            .saturating_add(height)
            .saturating_add(sidebar_row_gap(app, &rows, idx));
    }
    out
}

pub(crate) fn agent_counts_by_workspace(
    entries: &[AgentPanelEntry],
) -> std::collections::HashMap<usize, usize> {
    let mut counts = std::collections::HashMap::new();
    let mut counted_tabs = std::collections::HashSet::new();
    for entry in entries {
        if counted_tabs.insert((entry.ws_idx, entry.tab_idx)) {
            *counts.entry(entry.ws_idx).or_default() += 1;
        }
    }
    counts
}

/// `has_agents` is supplied by the caller so a per-frame or per-hit-test agent
/// scan is shared across every card instead of rebuilt for each row.
pub(crate) fn workspace_agent_chevron_rect(
    app: &AppState,
    card: &crate::app::state::WorkspaceCardArea,
    has_agents: bool,
) -> Rect {
    if !has_agents || card.rect.width < 2 || card.rect.height == 0 {
        return Rect::default();
    }
    let worktree_width = u16::from(workspace_parent_group_state(app, card.ws_idx).is_some());
    Rect::new(
        card.rect.x + card.rect.width.saturating_sub(1 + worktree_width),
        card.rect.y,
        1,
        1,
    )
}

pub(crate) fn workspace_group_chevron_rect(card: &crate::app::state::WorkspaceCardArea) -> Rect {
    if card.rect.width == 0 || card.rect.height == 0 {
        return Rect::default();
    }

    Rect::new(
        card.rect.x + card.rect.width.saturating_sub(1),
        card.rect.y,
        1,
        1,
    )
}

/// Auto-scale sidebar width based on workspace identity + agent summary.
pub(crate) fn collapsed_sidebar_sections(area: Rect) -> (Rect, Option<u16>, Rect) {
    // Reserve the top-left cell for the always-visible disclosure control.
    let content = Rect::new(
        area.x,
        area.y.saturating_add(1),
        area.width.saturating_sub(1),
        area.height.saturating_sub(1),
    );
    if content.width == 0 || content.height == 0 {
        return (Rect::default(), None, Rect::default());
    }
    (content, None, Rect::default())
}

/// Collapsed sidebar: workspace glance on top, compact agent list below.
/// Scroll offset of the collapsed rail, in the same unified row index space as
/// [`normalized_workspace_scroll`].
pub(crate) fn collapsed_sidebar_row_scroll(app: &AppState, ws_area: Rect) -> usize {
    let max_scroll = sidebar_rows(app)
        .len()
        .saturating_sub(ws_area.height as usize);
    app.workspace_scroll.min(max_scroll)
}

pub(crate) fn collapsed_sidebar_scroll_for_target(
    app: &AppState,
    ws_area: Rect,
    current_scroll: usize,
    target: usize,
) -> usize {
    let max_scroll = sidebar_rows(app)
        .len()
        .saturating_sub(ws_area.height as usize);
    let current_scroll = current_scroll.min(max_scroll);
    if target < current_scroll {
        return target;
    }
    let height = ws_area.height as usize;
    if height > 0 && target >= current_scroll.saturating_add(height) {
        return target
            .saturating_sub(height.saturating_sub(1))
            .min(max_scroll);
    }
    current_scroll
}

pub(super) fn render_sidebar_collapsed(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let is_navigating = matches!(app.mode, Mode::Navigate);

    let p = &app.palette;
    let sep_style = if is_navigating {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.surface_dim)
    };
    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(sep_x, y)].set_symbol("│");
        buf[(sep_x, y)].set_style(sep_style);
    }

    let (ws_area, _, _) = collapsed_sidebar_sections(area);
    if ws_area == Rect::default() {
        render_sidebar_toggle(app, frame, area, true, p);
        return;
    }

    let scroll = collapsed_sidebar_row_scroll(app, ws_area);
    for (row_idx, row) in sidebar_rows(app).iter().enumerate().skip(scroll) {
        let y = ws_area.y + (row_idx - scroll) as u16;
        if y >= ws_area.y + ws_area.height {
            break;
        }
        match row {
            SidebarRow::Workspace { ws_idx, indented } => {
                let Some(ws) = app.workspaces.get(*ws_idx) else {
                    continue;
                };
                let (agg_state, agg_seen) = ws.aggregate_state(&app.terminals);
                let (icon, icon_style) = state_dot(agg_state, agg_seen, p);
                let is_selected = *ws_idx == app.selected && is_navigating;
                let is_active = Some(*ws_idx) == app.active;
                let row_style = Style::default();
                let num_style = if is_selected || is_active {
                    Style::default().fg(p.text).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(p.overlay0)
                };
                let index = if *indented {
                    "└".to_string()
                } else {
                    format!("{}", ws_idx + 1)
                };
                let gap = if display_width_u16(&index).saturating_add(2) <= ws_area.width {
                    " "
                } else {
                    ""
                };
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(index, num_style),
                        Span::styled(gap, row_style),
                        Span::styled(icon, icon_style),
                    ])),
                    Rect::new(ws_area.x, y, ws_area.width, 1),
                );
            }
            SidebarRow::Agent { entry, depth } => {
                let (icon, icon_style) = state_dot(entry.state, entry.seen, p);
                let row_style = Style::default();
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(
                            if *depth == 0 {
                                format!("{}", row_idx + 1)
                            } else if *depth > 1 {
                                "  ".to_string()
                            } else {
                                " ".to_string()
                            },
                            Style::default().fg(p.overlay0),
                        ),
                        Span::raw(" "),
                        Span::styled(icon, icon_style),
                    ]))
                    .style(row_style),
                    Rect::new(ws_area.x, y, ws_area.width, 1),
                );
            }
            SidebarRow::Tab { entry, .. } => {
                let (icon, icon_style) = state_dot(entry.state, entry.seen, p);
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(icon, icon_style),
                    ])),
                    Rect::new(ws_area.x, y, ws_area.width, 1),
                );
            }
        }
    }

    render_sidebar_toggle(app, frame, area, true, p);
}

pub(crate) fn workspace_drop_slots(
    app: &AppState,
    cards: &[crate::app::state::WorkspaceCardArea],
    area: Rect,
) -> Vec<(crate::app::state::WorkspaceDropTarget, u16)> {
    if area.height == 0 || cards.is_empty() {
        return Vec::new();
    }
    let list_bottom = area.y + area.height.saturating_sub(1);
    let entries = workspace_list_entries(app);
    let entry_position = |ws_idx| {
        entries.iter().position(|entry| {
            matches!(
                entry,
                WorkspaceListEntry::Workspace {
                    ws_idx: entry_ws_idx,
                    ..
                } if *entry_ws_idx == ws_idx
            )
        })
    };
    let block_root_at = |entry_idx: usize| {
        entries[..=entry_idx]
            .iter()
            .rev()
            .find_map(|entry| match entry {
                WorkspaceListEntry::Workspace {
                    ws_idx,
                    indented: false,
                } => Some(*ws_idx),
                WorkspaceListEntry::Workspace { .. } => None,
            })
    };

    let mut slots = Vec::new();
    let mut previous_root = None;
    for card in cards {
        let Some(entry_idx) = entry_position(card.ws_idx) else {
            continue;
        };
        let Some(root_idx) = block_root_at(entry_idx) else {
            continue;
        };
        if previous_root == Some(root_idx) {
            continue;
        }
        previous_root = Some(root_idx);
        if let Some(row) = card.rect.y.checked_sub(1).filter(|row| *row < list_bottom) {
            slots.push((
                crate::app::state::WorkspaceDropTarget::Before(root_idx),
                row,
            ));
        }
    }

    let Some(last) = cards.last() else {
        return slots;
    };
    let Some(last_entry_idx) = entry_position(last.ws_idx) else {
        return slots;
    };
    let next_entry = entries.get(last_entry_idx.saturating_add(1));
    if matches!(
        next_entry,
        Some(WorkspaceListEntry::Workspace { indented: true, .. })
    ) {
        return slots;
    }
    let target = match next_entry {
        Some(WorkspaceListEntry::Workspace { ws_idx, .. }) => {
            crate::app::state::WorkspaceDropTarget::Before(*ws_idx)
        }
        None => crate::app::state::WorkspaceDropTarget::End,
    };
    let row = last.rect.y.saturating_add(last.rect.height);
    if row < list_bottom
        && slots
            .last()
            .is_none_or(|(last_target, _)| *last_target != target)
    {
        slots.push((target, row));
    }
    slots
}

pub(crate) fn workspace_drop_indicator_row(
    app: &AppState,
    cards: &[crate::app::state::WorkspaceCardArea],
    area: Rect,
    target: crate::app::state::WorkspaceDropTarget,
) -> Option<u16> {
    workspace_drop_slots(app, cards, area)
        .into_iter()
        .find_map(|(candidate, row)| (candidate == target).then_some(row))
}

pub(super) fn render_sidebar(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    let p = &app.palette;
    let is_navigating = matches!(app.mode, Mode::Navigate);
    let sep_style = if is_navigating {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.surface_dim)
    };

    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(sep_x, y)].set_symbol("│");
        buf[(sep_x, y)].set_style(sep_style);
    }

    let (ws_area, _) = expanded_sidebar_sections(area, app.sidebar_section_split);
    render_workspace_list(app, terminal_runtimes, frame, ws_area, is_navigating);
    render_sidebar_header(app, frame, area, p);
}

fn render_sidebar_header(app: &AppState, frame: &mut Frame, area: Rect, p: &Palette) {
    if area.width <= 1 || area.height == 0 {
        return;
    }
    let toggle = expanded_sidebar_toggle_rect(area);
    let new_space = sidebar_header_new_space_rect(area);
    let overflow = sidebar_header_overflow_rect(area);
    frame.render_widget(
        Paragraph::new(Span::styled("«", Style::default().fg(p.overlay0))),
        toggle,
    );
    let title_x = toggle.x.saturating_add(toggle.width).saturating_add(1);
    let title_right = new_space.x.saturating_sub(1);
    if title_right > title_x {
        let title = if app.sidebar_shows_spaces_tree() {
            "Spaces"
        } else {
            "Agents"
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                title,
                Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
            )),
            Rect::new(title_x, area.y, title_right - title_x, 1),
        );
    }
    frame.render_widget(
        Paragraph::new(Span::styled("＋", Style::default().fg(p.accent))),
        new_space,
    );
    let overflow_style = if app.global_menu_attention_badge_visible() {
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.overlay0)
    };
    frame.render_widget(Paragraph::new(Span::styled("…", overflow_style)), overflow);
}

fn resolved_token_spans(
    resolved: &[ResolvedToken],
    state_icon: (&str, Style),
    state_text_style: Style,
    workspace_style: Style,
    secondary_style: Style,
    custom_style: Style,
    p: &Palette,
    max_width: usize,
) -> Vec<Span<'static>> {
    let fixed_widths = resolved
        .iter()
        .map(|token| match &token.kind {
            ResolvedTokenKind::StateIcon => display_width(state_icon.0),
            ResolvedTokenKind::GitStatus { ahead, behind } => {
                usize::from(*ahead > 0) * display_width(&format!("↑{ahead}"))
                    + usize::from(*behind > 0) * display_width(&format!("↓{behind}"))
                    + usize::from(*ahead > 0 && *behind > 0)
            }
            _ => 0,
        })
        .collect::<Vec<_>>();
    let flexible_widths = resolved
        .iter()
        .map(|token| match &token.kind {
            ResolvedTokenKind::StateText(text)
            | ResolvedTokenKind::Workspace(text)
            | ResolvedTokenKind::Tab(text)
            | ResolvedTokenKind::Pane(text)
            | ResolvedTokenKind::Agent(text)
            | ResolvedTokenKind::TerminalTitle(text)
            | ResolvedTokenKind::Branch(text)
            | ResolvedTokenKind::Custom(text) => display_width(text),
            _ => 0,
        })
        .collect::<Vec<_>>();
    let minimum_width = |active: &[bool]| {
        let indices = active
            .iter()
            .enumerate()
            .filter_map(|(index, active)| active.then_some(index))
            .collect::<Vec<_>>();
        let content = indices
            .iter()
            .map(|index| fixed_widths[*index] + usize::from(flexible_widths[*index] > 0))
            .sum::<usize>();
        let separators = indices
            .windows(2)
            .map(|pair| display_width(tokens::separator(&resolved[pair[0]], &resolved[pair[1]])))
            .sum::<usize>();
        content + separators
    };
    let mut active = resolved.iter().map(|_| true).collect::<Vec<_>>();
    if minimum_width(&active) > max_width {
        for (index, width) in flexible_widths.iter().enumerate() {
            if *width > 0 {
                active[index] = false;
            }
        }
        for index in (0..resolved.len()).rev() {
            if flexible_widths[index] == 0 {
                continue;
            }
            active[index] = true;
            if minimum_width(&active) > max_width {
                active[index] = false;
            }
        }
    }
    let visible_indices = active
        .iter()
        .enumerate()
        .filter_map(|(index, active)| active.then_some(index))
        .collect::<Vec<_>>();
    let separator_width = visible_indices
        .windows(2)
        .map(|pair| display_width(tokens::separator(&resolved[pair[0]], &resolved[pair[1]])))
        .sum::<usize>();
    let fixed_width = visible_indices
        .iter()
        .map(|index| fixed_widths[*index])
        .sum::<usize>();
    let mut budgets = flexible_widths
        .iter()
        .enumerate()
        .map(|(index, width)| usize::from(active[index] && *width > 0))
        .collect::<Vec<_>>();
    let minimum = budgets.iter().sum::<usize>();
    let mut remaining = max_width
        .saturating_sub(separator_width + fixed_width)
        .saturating_sub(minimum);
    while remaining > 0 {
        let mut grew = false;
        for (budget, width) in budgets.iter_mut().zip(&flexible_widths) {
            if *budget > 0 && *budget < *width {
                *budget += 1;
                remaining -= 1;
                grew = true;
                if remaining == 0 {
                    break;
                }
            }
        }
        if !grew {
            break;
        }
    }
    let mut spans = Vec::new();
    for (position, index) in visible_indices.iter().copied().enumerate() {
        let token = &resolved[index];
        if position > 0 {
            let previous = &resolved[visible_indices[position - 1]];
            spans.push(Span::styled(
                tokens::separator(previous, token),
                Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
            ));
        }
        match &token.kind {
            ResolvedTokenKind::StateIcon => {
                spans.push(Span::styled(
                    state_icon.0.to_string(),
                    apply_token_style(state_icon.1, token.style),
                ));
            }
            ResolvedTokenKind::StateText(text) => {
                spans.push(Span::styled(
                    truncate_end(text, budgets[index]),
                    apply_token_style(state_text_style, token.style),
                ));
            }
            ResolvedTokenKind::Workspace(text) => {
                spans.push(Span::styled(
                    truncate_end(text, budgets[index]),
                    apply_token_style(workspace_style, token.style),
                ));
            }
            ResolvedTokenKind::Tab(text) => {
                spans.push(Span::styled(
                    truncate_end(text, budgets[index]),
                    apply_token_style(workspace_style, token.style),
                ));
            }
            ResolvedTokenKind::Pane(text)
            | ResolvedTokenKind::Agent(text)
            | ResolvedTokenKind::Branch(text) => {
                spans.push(Span::styled(
                    truncate_end(text, budgets[index]),
                    apply_token_style(secondary_style, token.style),
                ));
            }
            ResolvedTokenKind::GitStatus { ahead, behind } => {
                if *ahead > 0 {
                    spans.push(Span::styled(
                        format!("↑{ahead}"),
                        apply_token_style(Style::default().fg(p.green), token.style),
                    ));
                }
                if *ahead > 0 && *behind > 0 {
                    spans.push(Span::styled(
                        " ",
                        apply_token_style(Style::default(), token.style),
                    ));
                }
                if *behind > 0 {
                    spans.push(Span::styled(
                        format!("↓{behind}"),
                        apply_token_style(Style::default().fg(p.red), token.style),
                    ));
                }
            }
            ResolvedTokenKind::TerminalTitle(text) | ResolvedTokenKind::Custom(text) => {
                spans.push(Span::styled(
                    truncate_end(text, budgets[index]),
                    apply_token_style(custom_style, token.style),
                ));
            }
        }
    }
    spans
}

fn apply_token_style(mut style: Style, patch: crate::config::SidebarTokenStyle) -> Style {
    if let Some(fg) = patch.fg {
        style = style.fg(fg.ratatui());
    }
    if let Some(bold) = patch.bold {
        style = if bold {
            style.add_modifier(Modifier::BOLD)
        } else {
            style.remove_modifier(Modifier::BOLD)
        };
    }
    if let Some(dim) = patch.dim {
        style = if dim {
            style.add_modifier(Modifier::DIM)
        } else {
            style.remove_modifier(Modifier::DIM)
        };
    }
    style
}

fn render_workspace_list(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
    is_navigating: bool,
) {
    let p = &app.palette;
    let dragged_ws_idx = match app.drag.as_ref().map(|drag| &drag.target) {
        Some(crate::app::state::DragTarget::WorkspaceReorder { source_ws_idx, .. }) => {
            Some(*source_ws_idx)
        }
        _ => None,
    };
    let insertion_row = match app.drag.as_ref().map(|drag| &drag.target) {
        Some(crate::app::state::DragTarget::WorkspaceReorder {
            drop_target: Some(drop_target),
            ..
        }) => workspace_drop_indicator_row(app, &app.view.workspace_card_areas, area, *drop_target),
        _ => None,
    };

    let list_bottom = area.y + area.height;

    let metrics = workspace_list_scroll_metrics(app, area);
    let scrollbar_rect = workspace_list_scrollbar_rect(app, area);
    let sidebar_area = Rect::new(area.x, area.y, area.width.saturating_add(1), area.height);
    let computed_cards;
    let cards = if app.view.workspace_card_areas.is_empty() {
        computed_cards = compute_workspace_card_areas(app, sidebar_area);
        &computed_cards
    } else {
        &app.view.workspace_card_areas
    };
    let entries = workspace_list_entries(app);
    let all_agents = sidebar_thread_entries_from(app, terminal_runtimes);
    let agent_counts = agent_counts_by_workspace(&all_agents);

    for card in cards {
        let i = card.ws_idx;
        let ws = &app.workspaces[i];
        let row_y = card.rect.y;
        let row_height = card.rect.height;
        let selected = i == app.selected && is_navigating;
        let is_active = Some(i) == app.active;
        let is_dragged = dragged_ws_idx == Some(i);
        let (agg_state, agg_seen) = ws.aggregate_state(&app.terminals);

        if is_dragged {
            let buf = frame.buffer_mut();
            for y in row_y..row_y + row_height {
                if y >= list_bottom {
                    break;
                }
                for x in card.rect.x..card.rect.x + card.rect.width {
                    buf[(x, y)].set_style(Style::default().bg(p.surface1));
                }
            }
        }

        let name_style = if selected || is_active || is_dragged {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };

        let label = ws.display_name_from(&app.terminals, terminal_runtimes);
        let display_label = if card.indented {
            grouped_child_display_label(&label, ws.branch().as_deref(), ws.custom_name.is_some())
        } else {
            label
        };
        let parent_group = (!card.indented)
            .then(|| workspace_parent_group_state(app, i))
            .flatten();
        let is_last_child = card.indented
            && entries
                .iter()
                .position(|entry| {
                    matches!(
                        entry,
                        WorkspaceListEntry::Workspace { ws_idx, .. } if *ws_idx == i
                    )
                })
                .is_none_or(|entry_idx| !next_entry_is_indented_workspace(&entries, entry_idx));
        let (display_state, display_seen) = parent_group
            .as_ref()
            .filter(|(_, collapsed)| *collapsed)
            .map(|(key, _)| space_aggregate_state(app, key))
            .unwrap_or((agg_state, agg_seen));
        let state_icon = state_dot(display_state, display_seen, p);
        let state_text_style = Style::default()
            .fg(state_label_color(display_state, display_seen, p))
            .add_modifier(Modifier::DIM);
        let branch_style = Style::default().fg(if selected || is_active {
            p.mauve
        } else {
            p.overlay0
        });
        let token_values = ws.metadata_tokens.values();
        let rows = tokens::space_rows(
            &app.sidebar_spaces,
            SpaceTokenContext {
                workspace: &display_label,
                branch: ws.branch().as_deref(),
                state_text: state_label(display_state, display_seen),
                ahead_behind: ws.git_ahead_behind(),
                tokens: &token_values,
                suppress_git_details: card.indented,
            },
        );

        for (row_index, resolved) in rows.iter().enumerate() {
            if row_index as u16 >= row_height || row_y + row_index as u16 >= list_bottom {
                break;
            }
            let mut spans = Vec::new();
            let prefix_width = if card.indented {
                spans.push(Span::raw("   "));
                if row_index == 0 {
                    spans.push(Span::styled(
                        if is_last_child { "└─ " } else { "├─ " },
                        Style::default().fg(p.overlay0),
                    ));
                    6
                } else if is_last_child {
                    spans.push(Span::raw("     "));
                    8
                } else {
                    spans.push(Span::styled("│", Style::default().fg(p.overlay0)));
                    spans.push(Span::raw("    "));
                    8
                }
            } else if row_index == 0 {
                spans.push(Span::raw(" "));
                1
            } else {
                spans.push(Span::raw("   "));
                3
            };
            let agent_suffix_width = if row_index == 0 {
                agent_counts
                    .get(&i)
                    .map(|count| 1u16.saturating_add(count.to_string().len() as u16))
                    .unwrap_or(0)
            } else {
                0
            };
            let trailing_width = agent_suffix_width
                + if row_index == 0 && parent_group.is_some() {
                    2
                } else {
                    0
                };
            spans.extend(resolved_token_spans(
                resolved,
                state_icon,
                state_text_style,
                name_style,
                branch_style,
                branch_style,
                p,
                card.rect
                    .width
                    .saturating_sub(prefix_width + trailing_width) as usize,
            ));
            frame.render_widget(
                Paragraph::new(Line::from(spans)),
                Rect::new(card.rect.x, row_y + row_index as u16, card.rect.width, 1),
            );
        }

        if let Some((_, collapsed)) = parent_group {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    if collapsed { "▸" } else { "▾" },
                    Style::default().fg(p.accent),
                )),
                workspace_group_chevron_rect(card),
            );
        }
        if let Some(count) = agent_counts.get(&i) {
            let chevron = workspace_agent_chevron_rect(app, card, *count > 0);
            if chevron != Rect::default() {
                let count_label = count.to_string();
                let count_width = display_width_u16(&count_label);
                if chevron.x >= card.rect.x.saturating_add(count_width) {
                    frame.render_widget(
                        Paragraph::new(Span::styled(
                            count_label,
                            Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
                        )),
                        Rect::new(
                            chevron.x.saturating_sub(count_width),
                            chevron.y,
                            count_width,
                            1,
                        ),
                    );
                }
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        if app.workspace_agents_expanded(i) {
                            "▾"
                        } else {
                            "▸"
                        },
                        Style::default().fg(p.accent),
                    )),
                    chevron,
                );
            }
        }
    }

    let row_entries = sidebar_rows_from(app, terminal_runtimes);
    if row_entries.is_empty() && !app.sidebar_shows_spaces_tree() {
        let body = workspace_list_body_rect(area, should_show_scrollbar(metrics));
        if body.width > 0 && body.height > 0 {
            frame.render_widget(
                Paragraph::new(" no matching agents")
                    .style(Style::default().fg(p.overlay0).add_modifier(Modifier::DIM)),
                Rect::new(body.x, body.y, body.width, 1),
            );
        }
    }
    let agent_cards = if app.view.agent_card_areas.is_empty() {
        compute_agent_card_areas(app, sidebar_area)
    } else {
        app.view.agent_card_areas.clone()
    };
    for card in compute_tab_card_areas(app, sidebar_area) {
        render_tab_card(app, frame, &card);
    }
    for card in agent_cards {
        let Some((entry, depth)) = row_entries.iter().find_map(|row| match row {
            SidebarRow::Agent { entry, depth }
                if entry.ws_idx == card.ws_idx
                    && entry.tab_idx == card.tab_idx
                    && entry.pane_id == card.pane_id =>
            {
                Some((entry, *depth))
            }
            _ => None,
        }) else {
            continue;
        };
        render_agent_card(app, frame, entry, card.rect, depth);
    }

    if let Some(y) = insertion_row.filter(|y| *y < list_bottom) {
        let indicator_right = scrollbar_rect
            .map(|rect| rect.x)
            .unwrap_or(area.x + area.width);
        let buf = frame.buffer_mut();
        for x in area.x..indicator_right {
            buf[(x, y)].set_symbol("─");
            buf[(x, y)].set_style(Style::default().fg(p.accent));
        }
    }

    if let Some(track) = scrollbar_rect {
        render_scrollbar(frame, metrics, track, p.surface_dim, p.overlay0, "▕");
    }
}

fn render_tab_card(app: &AppState, frame: &mut Frame, card: &crate::app::state::TabCardArea) {
    let p = &app.palette;
    let active = app.active == Some(card.ws_idx)
        && app
            .workspaces
            .get(card.ws_idx)
            .is_some_and(|ws| ws.active_tab_index() == card.tab_idx);
    let entry = sidebar_rows(app).into_iter().find_map(|row| match row {
        SidebarRow::Tab { entry, depth }
            if entry.ws_idx == card.ws_idx && entry.tab_idx == card.tab_idx =>
        {
            Some((entry, depth))
        }
        _ => None,
    });
    let Some((entry, depth)) = entry else { return };
    let state = entry
        .state_labels
        .get(agent_panel_status_key(entry.state, entry.seen))
        .map(String::as_str)
        .unwrap_or_else(|| match entry.state {
            AgentState::Idle if !entry.seen => "done",
            _ => state_label(entry.state, entry.seen),
        });
    let state_icon = state_dot(entry.state, entry.seen, p);
    let style = if active {
        Style::default().fg(p.text).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.subtext0)
    };
    let prefix = " ".repeat(usize::from(depth) * 3 + 1);
    let fixed_width = display_width(&prefix)
        + display_width(state_icon.0)
        + 1
        + display_width(state)
        + display_width(" · ");
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(prefix),
            Span::styled(state_icon.0.to_string(), state_icon.1),
            Span::styled(
                format!(" {state}"),
                Style::default().fg(state_label_color(entry.state, entry.seen, p)),
            ),
            Span::styled(" · ", Style::default().fg(p.overlay0)),
            Span::styled(
                truncate_end(
                    entry
                        .primary_tab_label
                        .as_deref()
                        .unwrap_or(DEFAULT_THREAD_TITLE),
                    usize::from(card.rect.width).saturating_sub(fixed_width),
                ),
                style,
            ),
        ])),
        card.rect,
    );
}

fn render_agent_card(
    app: &AppState,
    frame: &mut Frame,
    detail: &AgentPanelEntry,
    rect: Rect,
    depth: u16,
) {
    let p = &app.palette;
    let label_color = state_label_color(detail.state, detail.seen, p);
    let rows = resolved_agent_rows(app, detail);
    let header_height = 0;
    let height = (rows.len().max(1) as u16)
        .saturating_add(header_height)
        .min(rect.height);
    let is_active = app.is_active_pane(detail.ws_idx, detail.tab_idx, detail.pane_id);
    let row_style = Style::default();
    let name_style = if is_active {
        Style::default().fg(p.text).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.subtext0).add_modifier(Modifier::BOLD)
    };
    let status_style = Style::default().fg(label_color).add_modifier(if is_active {
        Modifier::empty()
    } else {
        Modifier::DIM
    });
    let agent_style = Style::default().fg(p.overlay0).add_modifier(Modifier::DIM);
    let state_icon = state_dot(detail.state, detail.seen, p);
    let indent = usize::from(depth) * 3;

    for (row_index, resolved) in rows
        .iter()
        .take(height.saturating_sub(header_height) as usize)
        .enumerate()
    {
        let prefix = if row_index == 0 {
            " ".repeat(indent + usize::from(depth > 0))
        } else {
            " ".repeat(indent + 3)
        };
        let prefix_width = display_width_u16(&prefix);
        let mut spans = vec![Span::raw(prefix)];
        let content_width = rect.width.saturating_sub(prefix_width) as usize;
        let resolve_tokens = |max_width| {
            resolved_token_spans(
                resolved,
                state_icon,
                status_style,
                name_style,
                agent_style,
                agent_style,
                p,
                max_width,
            )
        };
        let baseline_token_spans = resolve_tokens(content_width);
        let mut activity_field = None;
        let token_spans = if row_index == 0 && agent_activity_age_fits(app, detail, rect, depth) {
            let label =
                crate::activity_age::compact_label(detail.activity_at, app.view_observed_at);
            activity_field = Some(format!(" {label:>4}"));
            resolve_tokens(content_width.saturating_sub(AGENT_ACTIVITY_AGE_FIELD_WIDTH))
        } else {
            baseline_token_spans
        };
        let activity_width = activity_field
            .as_deref()
            .map(display_width)
            .unwrap_or_default();
        spans.extend(token_spans);
        if let Some(activity_field) = activity_field {
            let used_width = spans
                .iter()
                .map(|span| display_width(span.content.as_ref()))
                .sum::<usize>();
            let padding = rect
                .width
                .saturating_sub(used_width.min(usize::from(u16::MAX)) as u16)
                .saturating_sub(activity_width.min(usize::from(u16::MAX)) as u16);
            if padding > 0 {
                spans.push(Span::raw(" ".repeat(usize::from(padding))));
            }
            let activity_style = if detail.state == AgentState::Working {
                Style::default().fg(p.yellow)
            } else {
                Style::default().fg(p.overlay0).add_modifier(Modifier::DIM)
            };
            spans.push(Span::styled(activity_field, activity_style));
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(row_style),
            Rect::new(
                rect.x,
                rect.y + header_height + row_index as u16,
                rect.width,
                1,
            ),
        );
    }
}

fn agent_activity_age_fits(
    app: &AppState,
    detail: &AgentPanelEntry,
    rect: Rect,
    depth: u16,
) -> bool {
    let Some(resolved) = resolved_agent_rows(app, detail).into_iter().next() else {
        return false;
    };
    let prefix_width = usize::from(depth) * 3 + usize::from(depth > 0);
    let content_width = usize::from(rect.width).saturating_sub(prefix_width);
    if content_width < AGENT_ACTIVITY_AGE_MIN_CONTENT_WIDTH + AGENT_ACTIVITY_AGE_FIELD_WIDTH {
        return false;
    }
    let p = &app.palette;
    let label_color = state_label_color(detail.state, detail.seen, p);
    let is_active = app.is_active_pane(detail.ws_idx, detail.tab_idx, detail.pane_id);
    let name_style = if is_active {
        Style::default().fg(p.text).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.subtext0).add_modifier(Modifier::BOLD)
    };
    let status_style = Style::default().fg(label_color).add_modifier(if is_active {
        Modifier::empty()
    } else {
        Modifier::DIM
    });
    let agent_style = Style::default().fg(p.overlay0).add_modifier(Modifier::DIM);
    let state_icon = state_dot(detail.state, detail.seen, p);
    let resolve_tokens = |max_width| {
        resolved_token_spans(
            &resolved,
            state_icon,
            status_style,
            name_style,
            agent_style,
            agent_style,
            p,
            max_width,
        )
    };
    let baseline = resolve_tokens(content_width);
    let candidate = resolve_tokens(content_width.saturating_sub(AGENT_ACTIVITY_AGE_FIELD_WIDTH));
    let candidate_width = candidate
        .iter()
        .map(|span| display_width(span.content.as_ref()))
        .sum::<usize>();
    candidate.len() == baseline.len()
        && candidate
            .iter()
            .zip(&baseline)
            .all(|(candidate, baseline)| candidate.content == baseline.content)
        && candidate_width <= content_width.saturating_sub(AGENT_ACTIVITY_AGE_FIELD_WIDTH)
}

pub(crate) fn visible_agent_activity_instants_from(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    cards: &[crate::app::state::AgentCardArea],
) -> Vec<std::time::Instant> {
    let rows = sidebar_rows_from(app, terminal_runtimes);
    cards
        .iter()
        .filter_map(|card| {
            let (entry, depth) = rows.iter().find_map(|row| match row {
                SidebarRow::Agent { entry, depth }
                    if entry.ws_idx == card.ws_idx
                        && entry.tab_idx == card.tab_idx
                        && entry.pane_id == card.pane_id =>
                {
                    Some((entry.as_ref(), *depth))
                }
                _ => None,
            })?;
            agent_activity_age_fits(app, entry, card.rect, depth)
                .then_some(entry.activity_at)
                .flatten()
        })
        .collect()
}

pub(crate) fn collapsed_sidebar_toggle_rect(area: Rect) -> Rect {
    let content_w = area.width.saturating_sub(1);
    if content_w == 0 || area.height == 0 {
        return Rect::default();
    }
    Rect::new(area.x, area.y, 1, 1)
}

pub(crate) fn expanded_sidebar_toggle_rect(area: Rect) -> Rect {
    if area.width <= 1 || area.height == 0 {
        return Rect::default();
    }
    Rect::new(area.x, area.y, 1, 1)
}

pub(crate) fn sidebar_header_new_space_rect(area: Rect) -> Rect {
    if area.width < 6 || area.height == 0 {
        return Rect::default();
    }
    Rect::new(area.x + area.width.saturating_sub(5), area.y, 2, 1)
}

pub(crate) fn sidebar_header_overflow_rect(area: Rect) -> Rect {
    if area.width < 3 || area.height == 0 {
        return Rect::default();
    }
    Rect::new(area.x + area.width.saturating_sub(2), area.y, 1, 1)
}

fn render_sidebar_toggle(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    collapsed: bool,
    p: &Palette,
) {
    let toggle_area = if collapsed {
        collapsed_sidebar_toggle_rect(area)
    } else {
        expanded_sidebar_toggle_rect(area)
    };
    if toggle_area == Rect::default() {
        return;
    }
    let icon = if collapsed { "»" } else { "«" };
    let icon_style = if collapsed && app.global_menu_attention_badge_visible() {
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.overlay0)
    };
    frame.render_widget(Paragraph::new(Span::styled(icon, icon_style)), toggle_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::schema::{
            AgentViewBuiltinField, AgentViewField, AgentViewFilter, AgentViewSetParams,
            AgentViewValue,
        },
        app::state::{AgentPanelSort, SidebarPresentationState, ViewLayout},
        config::BindingConfig,
        detect::Agent,
        workspace::Workspace,
    };
    use ratatui::{backend::TestBackend, layout::Direction, Terminal};

    fn app_with_agents(names: &[&str]) -> AppState {
        let mut app = AppState::test_new();
        app.workspaces = names.iter().map(|name| Workspace::test_new(name)).collect();
        app.ensure_test_terminals();
        for workspace in &app.workspaces {
            for tab in &workspace.tabs {
                for pane in tab.panes.values() {
                    let terminal = app.terminals.get_mut(&pane.attached_terminal_id).unwrap();
                    terminal.detected_agent = Some(Agent::Pi);
                    terminal.state = AgentState::Working;
                }
            }
        }
        app.active = (!app.workspaces.is_empty()).then_some(0);
        app.selected = 0;
        app.reconcile_sidebar_presentation();
        app
    }

    fn row_kinds(app: &AppState) -> Vec<(char, usize)> {
        sidebar_rows(app)
            .into_iter()
            .map(|row| match row {
                SidebarRow::Workspace { ws_idx, .. } => ('w', ws_idx),
                SidebarRow::Tab { entry, .. } => ('t', entry.ws_idx),
                SidebarRow::Agent { entry, .. } => ('a', entry.ws_idx),
            })
            .collect()
    }

    fn filtered_to_missing() -> AgentViewSetParams {
        AgentViewSetParams {
            source: "test".into(),
            label: Some("missing".into()),
            filter: Some(AgentViewFilter::Eq {
                field: AgentViewField::Builtin(AgentViewBuiltinField::WorkspaceId),
                value: AgentViewValue::String("missing".into()),
            }),
            sort: Vec::new(),
        }
    }

    #[test]
    fn expanded_sidebar_nests_single_line_tab_rows_under_owning_space() {
        let app = app_with_agents(&["one", "two"]);
        assert_eq!(
            row_kinds(&app),
            vec![('w', 0), ('t', 0), ('w', 1), ('t', 1)]
        );

        let area = Rect::new(0, 0, 28, 20);
        let mut terminal = Terminal::new(TestBackend::new(28, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let text = (0..20)
            .map(|row| row_text(terminal.backend().buffer(), row, 27))
            .collect::<Vec<_>>();
        assert_eq!(
            text.iter().filter(|line| line.contains("Spaces")).count(),
            1,
            "{text:?}"
        );
        assert!(!text
            .iter()
            .any(|line| line.trim_start().starts_with("agents")));
        assert!(text.iter().any(|line| line.contains("one")));
        assert!(text.iter().any(|line| line.contains(DEFAULT_THREAD_TITLE)));
        assert!(!text.iter().any(|line| line.contains("pi")));
    }

    #[test]
    fn workspace_agent_disclosure_toggles_only_owned_children() {
        let mut app = app_with_agents(&["one", "two"]);
        assert!(app.toggle_workspace_agent_disclosure(1));
        assert_eq!(row_kinds(&app), vec![('w', 0), ('t', 0), ('w', 1)]);
        let collapsed_worktrees = app.collapsed_space_keys.clone();

        assert!(app.toggle_workspace_agent_disclosure(0));
        assert_eq!(row_kinds(&app), vec![('w', 0), ('w', 1)]);
        assert!(!app.workspace_agents_expanded(1));
        assert_eq!(app.collapsed_space_keys, collapsed_worktrees);

        assert!(app.toggle_workspace_agent_disclosure(0));
        assert_eq!(row_kinds(&app), vec![('w', 0), ('t', 0), ('w', 1)]);
    }

    #[test]
    fn agent_tree_mouse_targets_preserve_workspace_and_worktree_actions() {
        let mut app = app_with_agents(&["main", "issue"]);
        app.workspaces[0].worktree_space =
            workspace_with_worktree_space("unused", Some("repo-key"), "/repo/main").worktree_space;
        app.workspaces[1].worktree_space =
            workspace_with_worktree_space("unused", Some("repo-key"), "/repo/issue").worktree_space;
        app.workspaces[0]
            .worktree_space
            .as_mut()
            .unwrap()
            .is_linked_worktree = false;
        let area = Rect::new(0, 0, 30, 20);
        let workspace_cards = compute_workspace_card_areas(&app, area);
        let agent_cards = compute_agent_card_areas(&app, area);
        let main = workspace_cards
            .iter()
            .find(|card| card.ws_idx == 0)
            .unwrap();
        let agent_chevron = workspace_agent_chevron_rect(&app, main, true);
        let group_chevron = workspace_group_chevron_rect(main);

        assert_ne!(agent_chevron, Rect::default());
        assert_ne!(agent_chevron, group_chevron);
        assert!(main.rect.intersects(agent_chevron));
        assert!(main.rect.intersects(group_chevron));
        assert!(agent_cards.iter().all(|agent| {
            workspace_cards
                .iter()
                .all(|workspace| !agent.rect.intersects(workspace.rect))
        }));
    }

    #[test]
    fn agent_tree_rows_preserve_configured_identity_status_and_tab_context() {
        let mut app = app_with_agents(&["one"]);
        app.workspaces[0].tabs[0].custom_name = Some("main".into());
        app.workspaces[0].test_add_tab(Some("review"));
        app.ensure_test_terminals();
        let review_pane = app.workspaces[0].tabs[1].root_pane;
        let review_terminal = app.workspaces[0].tabs[1].panes[&review_pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&review_terminal).unwrap();
        terminal.detected_agent = Some(Agent::Claude);
        terminal.agent_name = Some("reviewer".into());
        terminal.manual_label = Some("right pane".into());
        terminal.state = AgentState::Blocked;

        let entries = all_agent_panel_entries(&app);
        let review = entries.iter().find(|entry| entry.tab_idx == 1).unwrap();
        assert_eq!(review.primary_label, "one");
        assert_eq!(review.primary_tab_label.as_deref(), Some("review"));
        assert_eq!(review.pane_label.as_deref(), Some("right pane"));
        assert_eq!(review.agent_label.as_deref(), Some("reviewer"));
        assert_eq!(review.agent, Some(Agent::Claude));
        assert_eq!(review.state, AgentState::Blocked);
    }

    #[test]
    fn agent_tree_handles_empty_multitab_and_multipane_workspaces() {
        let mut app = AppState::test_new();
        let empty = Workspace::test_new("empty");
        let mut busy = Workspace::test_new("busy");
        busy.tabs[0].custom_name = Some("main".into());
        let split = busy.test_split(Direction::Horizontal);
        let second_tab = busy.test_add_tab(Some("review"));
        app.workspaces = vec![empty, busy];
        app.ensure_test_terminals();
        for (tab_idx, pane_id) in [
            (0, split),
            (1, app.workspaces[1].tabs[second_tab].root_pane),
        ] {
            let terminal_id = app.workspaces[1].tabs[tab_idx].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Pi);
        }
        app.active = Some(1);
        app.reconcile_sidebar_presentation();

        assert_eq!(
            all_agent_panel_entries(&app)
                .iter()
                .map(|entry| (entry.ws_idx, entry.tab_idx, entry.pane_id))
                .collect::<Vec<_>>(),
            vec![
                (0, 0, app.workspaces[0].tabs[0].root_pane),
                (1, 0, app.workspaces[1].tabs[0].root_pane),
                (1, 0, split),
                (1, 1, app.workspaces[1].tabs[1].root_pane)
            ]
        );
        let empty_card = compute_workspace_card_areas(&app, Rect::new(0, 0, 30, 20))
            .into_iter()
            .find(|card| card.ws_idx == 0)
            .unwrap();
        let empty_has_agents = agent_counts_by_workspace(&sidebar_thread_entries(&app))
            .contains_key(&empty_card.ws_idx);
        assert!(empty_has_agents);
        assert_ne!(
            workspace_agent_chevron_rect(&app, &empty_card, empty_has_agents),
            Rect::default()
        );
        let empty_threads = sidebar_thread_entries(&app)
            .into_iter()
            .filter(|entry| entry.ws_idx == 0)
            .collect::<Vec<_>>();
        assert_eq!(empty_threads.len(), 1);
        assert_eq!(
            empty_threads[0].primary_tab_label.as_deref(),
            Some(DEFAULT_THREAD_TITLE)
        );
        assert_eq!(empty_threads[0].agent, None);
        assert!(all_agent_panel_entries(&app)
            .iter()
            .any(|entry| entry.ws_idx == 0));
    }

    #[test]
    fn ac4_tab_rollup_does_not_let_done_mask_working() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("rollup");
        let working_pane = workspace.test_split(Direction::Horizontal);
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let done_pane = app.workspaces[0].tabs[0].root_pane;
        let done_terminal = app.workspaces[0].tabs[0].panes[&done_pane]
            .attached_terminal_id
            .clone();
        let working_terminal = app.workspaces[0].tabs[0].panes[&working_pane]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&done_terminal).unwrap().state = AgentState::Idle;
        app.workspaces[0].tabs[0]
            .panes
            .get_mut(&done_pane)
            .unwrap()
            .seen = false;
        app.terminals.get_mut(&working_terminal).unwrap().state = AgentState::Working;
        app.active = Some(0);
        app.reconcile_sidebar_presentation();

        let tab = sidebar_rows(&app)
            .into_iter()
            .find_map(|row| match row {
                SidebarRow::Tab { entry, .. } => Some(entry),
                _ => None,
            })
            .unwrap();

        assert_eq!(tab.state, AgentState::Working);
    }

    #[test]
    fn shell_only_custom_tab_uses_its_title_without_becoming_an_agent() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("repo-folder");
        workspace.tabs[0].custom_name = Some("Review Auth Migration".into());
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.active = Some(0);

        let entries = sidebar_thread_entries(&app);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].primary_tab_label.as_deref(),
            Some("Review Auth Migration")
        );
        assert_eq!(entries[0].agent_label.as_deref(), Some("Terminal"));
        assert_eq!(row_kinds(&app), vec![('w', 0), ('t', 0)]);
    }

    #[test]
    fn next_agent_wraps_globally_ignoring_sidebar_projection() {
        let mut app = app_with_agents(&["one", "two", "three"]);
        app.agent_panel_sort = AgentPanelSort::Priority;
        app.agent_view_override = Some(filtered_to_missing());
        app.sidebar_collapsed = true;
        app.sidebar_presentation.expanded_workspace_ids.clear();
        app.workspace_scroll = 99;

        app.focus_pane_in_workspace(2, app.workspaces[2].tabs[0].root_pane);
        app.next_agent();
        assert_eq!(app.active, Some(0));
    }

    #[test]
    fn previous_agent_wraps_globally_ignoring_sidebar_projection() {
        let mut app = app_with_agents(&["one", "two", "three"]);
        app.agent_panel_sort = AgentPanelSort::Priority;
        app.agent_view_override = Some(filtered_to_missing());
        app.sidebar_collapsed = true;
        app.sidebar_presentation.expanded_workspace_ids.clear();
        app.workspace_scroll = 99;

        app.previous_agent();
        assert_eq!(app.active, Some(2));
    }

    #[test]
    fn sidebar_tree_preserves_tab_wrap_and_default_navigation_bindings() {
        let keys = crate::config::Config::default().keys;
        assert_eq!(keys.previous_tab, BindingConfig::one("prefix+p"));
        assert_eq!(keys.next_tab, BindingConfig::one("prefix+n"));
        assert_eq!(keys.previous_agent, BindingConfig::empty());
        assert_eq!(keys.next_agent, BindingConfig::empty());

        let mut app = app_with_agents(&["one"]);
        app.workspaces[0].test_add_tab(Some("two"));
        app.next_tab();
        assert_eq!(app.workspaces[0].active_tab, 1);
        app.next_tab();
        assert_eq!(app.workspaces[0].active_tab, 0);
        app.previous_tab();
        assert_eq!(app.workspaces[0].active_tab, 1);
    }

    #[test]
    fn workspace_picker_temporarily_shows_tree_from_priority_projection() {
        let mut app = app_with_agents(&["one", "two"]);
        app.agent_panel_sort = AgentPanelSort::Priority;
        assert!(sidebar_rows(&app)
            .iter()
            .any(|row| matches!(row, SidebarRow::Workspace { .. })));

        app.begin_workspace_picker_presentation();
        assert!(sidebar_rows(&app)
            .iter()
            .any(|row| matches!(row, SidebarRow::Workspace { .. })));
        app.end_workspace_picker_presentation();
        assert!(sidebar_rows(&app)
            .iter()
            .any(|row| matches!(row, SidebarRow::Tab { .. })));
    }

    #[test]
    fn review_findings_workspace_picker_override_is_shared_across_clients() {
        let mut app = app_with_agents(&["one", "two"]);
        app.agent_panel_sort = AgentPanelSort::Priority;
        let mut client_a = SidebarPresentationState::default();
        let mut client_b = SidebarPresentationState::default();

        app.swap_sidebar_presentation(&mut client_a);
        app.begin_workspace_picker_presentation();
        app.swap_sidebar_presentation(&mut client_a);
        assert!(app.sidebar_shows_spaces_tree());

        app.swap_sidebar_presentation(&mut client_b);
        app.end_workspace_picker_presentation();
        app.swap_sidebar_presentation(&mut client_b);

        app.swap_sidebar_presentation(&mut client_a);
        assert!(app.sidebar_shows_spaces_tree());
        app.swap_sidebar_presentation(&mut client_a);
    }

    #[test]
    fn workspace_picker_temporarily_shows_tree_from_agent_view_override() {
        let mut app = app_with_agents(&["one", "two"]);
        app.agent_view_override = Some(filtered_to_missing());
        assert!(sidebar_rows(&app)
            .iter()
            .any(|row| matches!(row, SidebarRow::Workspace { .. })));

        app.begin_workspace_picker_presentation();
        assert_eq!(
            row_kinds(&app),
            vec![('w', 0), ('t', 0), ('w', 1), ('t', 1)]
        );
        app.end_workspace_picker_presentation();
        assert!(sidebar_rows(&app)
            .iter()
            .any(|row| matches!(row, SidebarRow::Workspace { .. })));
    }

    #[test]
    fn agent_projection_switch_preserves_tree_state_and_global_cycle_order() {
        let mut app = app_with_agents(&["one", "two"]);
        app.toggle_workspace_agent_disclosure(0);
        let disclosure = app.sidebar_presentation.expanded_workspace_ids.clone();
        let canonical = all_agent_panel_entries(&app)
            .iter()
            .map(|entry| (entry.ws_idx, entry.pane_id))
            .collect::<Vec<_>>();

        app.agent_panel_sort = AgentPanelSort::Priority;
        app.agent_view_override = Some(filtered_to_missing());
        app.begin_workspace_picker_presentation();
        app.end_workspace_picker_presentation();

        assert_eq!(app.sidebar_presentation.expanded_workspace_ids, disclosure);
        assert_eq!(
            all_agent_panel_entries(&app)
                .iter()
                .map(|entry| (entry.ws_idx, entry.pane_id))
                .collect::<Vec<_>>(),
            canonical
        );
    }

    #[test]
    fn compact_agent_tree_render_and_hit_test_share_order() {
        let mut app = app_with_agents(&["one", "two"]);
        app.sidebar_collapsed = true;
        let area = Rect::new(0, 0, 18, 20);
        let rows = sidebar_rows(&app);
        let workspace_cards = compute_workspace_card_areas(&app, area);
        let agent_cards = compute_agent_card_areas(&app, area);
        let geometry_order = compute_sidebar_row_areas(&app, area);

        assert_eq!(
            rows.len(),
            workspace_cards.len() + compute_tab_card_areas(&app, area).len() + agent_cards.len()
        );
        assert_eq!(workspace_cards, geometry_order.0);
        assert_eq!(agent_cards, geometry_order.1);
    }

    #[test]
    fn review_findings_compact_agent_navigation_reveals_exact_row() {
        let mut app = app_with_agents(&["one", "two", "three"]);
        app.sidebar_presentation.expanded_workspace_ids = app
            .workspaces
            .iter()
            .map(|workspace| workspace.id.clone())
            .collect();
        app.sidebar_collapsed = true;
        app.view.sidebar_rect = Rect::new(0, 0, 4, 2);
        app.workspace_scroll = 0;

        app.next_agent();

        let focused = app.active.unwrap();
        let pane_id = app.workspaces[focused].focused_pane_id().unwrap();
        let target_tab = app.workspaces[focused]
            .find_tab_index_for_pane(pane_id)
            .unwrap();
        let target = sidebar_rows(&app)
            .iter()
            .position(|row| {
                matches!(
                    row,
                    SidebarRow::Tab { entry, .. }
                        if entry.ws_idx == focused && entry.tab_idx == target_tab
                )
            })
            .unwrap();
        assert!(target >= app.workspace_scroll);
        assert!(target < app.workspace_scroll + 2);
    }

    #[test]
    fn mobile_agent_tree_preserves_workspace_ownership() {
        let mut app = app_with_agents(&["one", "two"]);
        app.view.layout = ViewLayout::Mobile;
        app.view.mobile_header_rect = Rect::new(0, 0, 30, 2);
        app.view.terminal_area = Rect::new(0, 2, 30, 20);
        assert_eq!(
            mobile_sidebar_rows(&app)
                .iter()
                .map(|row| match row {
                    SidebarRow::Workspace { ws_idx, .. } => ('w', *ws_idx),
                    SidebarRow::Tab { entry, .. } => ('t', entry.ws_idx),
                    SidebarRow::Agent { entry, .. } => ('a', entry.ws_idx),
                })
                .collect::<Vec<_>>(),
            vec![('w', 0), ('t', 0), ('w', 1), ('t', 1)]
        );
    }

    #[test]
    fn initial_sidebar_projection_keeps_every_workspace_tab_in_expanded_and_collapsed_views() {
        let mut app = app_with_agents(&["active", "inactive"]);
        app.workspaces[0].test_add_tab(Some("active second"));
        let inactive_split = app.workspaces[1].test_split(Direction::Horizontal);
        app.workspaces[1].test_add_tab(Some("agentless second"));
        app.workspaces.push(Workspace::test_new("completed"));
        app.ensure_test_terminals();
        let completed_pane = app.workspaces[2].tabs[0].root_pane;
        let completed_terminal = app.workspaces[2].tabs[0].panes[&completed_pane]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&completed_terminal).unwrap().state = AgentState::Idle;
        app.workspaces[2].tabs[0]
            .panes
            .get_mut(&completed_pane)
            .unwrap()
            .seen = false;
        app.reconcile_sidebar_presentation();

        let rows = sidebar_rows(&app);
        let tabs = rows
            .iter()
            .filter_map(|row| match row {
                SidebarRow::Tab { entry, .. } => Some((entry.ws_idx, entry.tab_idx)),
                SidebarRow::Workspace { .. } | SidebarRow::Agent { .. } => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(tabs, vec![(0, 0), (0, 1), (1, 0), (1, 1), (2, 0)]);
        assert!(app.workspaces[1].tabs[0]
            .panes
            .contains_key(&inactive_split));
        assert_eq!(
            tabs.iter()
                .filter(|(ws_idx, tab_idx)| (*ws_idx, *tab_idx) == (1, 0))
                .count(),
            1,
            "a multi-pane window is represented by exactly one tab row"
        );
        assert!(rows.iter().any(|row| {
            matches!(
                row,
                SidebarRow::Tab { entry, .. } if entry.ws_idx == 1 && entry.tab_idx == 1 && entry.agent.is_none()
            )
        }));
        assert!(rows.iter().any(|row| {
            matches!(
                row,
                SidebarRow::Tab { entry, .. } if entry.ws_idx == 2 && entry.state == AgentState::Idle && !entry.seen
            )
        }));
        let row_identities = |rows: Vec<SidebarRow>| {
            rows.into_iter()
                .map(|row| match row {
                    SidebarRow::Workspace { ws_idx, .. } => ("workspace", ws_idx, None, None),
                    SidebarRow::Tab { entry, .. } => {
                        ("tab", entry.ws_idx, Some(entry.tab_idx), None)
                    }
                    SidebarRow::Agent { entry, .. } => (
                        "pane",
                        entry.ws_idx,
                        Some(entry.tab_idx),
                        Some(entry.pane_id),
                    ),
                })
                .collect::<Vec<_>>()
        };
        let canonical_order = row_identities(rows.clone());

        assert!(app.focus_pane_in_workspace(2, completed_pane));
        assert!(app.workspaces[2].tabs[0].panes[&completed_pane].seen);
        assert_eq!(row_identities(sidebar_rows(&app)), canonical_order);

        app.terminals.get_mut(&completed_terminal).unwrap().state = AgentState::Working;
        assert_eq!(row_identities(sidebar_rows(&app)), canonical_order);
        app.terminals.get_mut(&completed_terminal).unwrap().state = AgentState::Idle;
        app.workspaces[2].tabs[0]
            .panes
            .get_mut(&completed_pane)
            .unwrap()
            .seen = false;
        assert_eq!(row_identities(sidebar_rows(&app)), canonical_order);

        app.sidebar_collapsed = true;
        assert_eq!(
            sidebar_rows(&app)
                .iter()
                .filter(|row| matches!(row, SidebarRow::Tab { .. }))
                .count(),
            5,
            "global collapse changes only presentation, never the tab projection"
        );
    }

    #[test]
    fn sidebar_disclosure_is_isolated_between_app_clients() {
        let mut app = app_with_agents(&["one", "two"]);
        let mut client_a = SidebarPresentationState::default();
        let mut client_b = SidebarPresentationState::default();

        app.swap_sidebar_presentation(&mut client_a);
        app.reconcile_sidebar_presentation();
        app.toggle_workspace_agent_disclosure(0);
        app.swap_sidebar_presentation(&mut client_a);

        app.swap_sidebar_presentation(&mut client_b);
        app.reconcile_sidebar_presentation();
        assert!(app.workspace_agents_expanded(0));
        app.swap_sidebar_presentation(&mut client_b);

        app.swap_sidebar_presentation(&mut client_a);
        assert!(!app.workspace_agents_expanded(0));
        app.swap_sidebar_presentation(&mut client_a);
    }

    #[test]
    fn projection_change_resets_scroll_for_each_attached_client() {
        let mut app = app_with_agents(&["one", "two"]);
        let mut client_a = SidebarPresentationState {
            workspace_scroll: 4,
            mobile_switcher_scroll: 5,
            ..SidebarPresentationState::default()
        };
        let mut client_b = SidebarPresentationState {
            workspace_scroll: 6,
            mobile_switcher_scroll: 7,
            ..SidebarPresentationState::default()
        };

        app.mark_sidebar_projection_changed();
        let revision = app.sidebar_projection_revision;

        app.swap_sidebar_presentation(&mut client_a);
        app.reconcile_sidebar_presentation();
        app.swap_sidebar_presentation(&mut client_a);
        app.swap_sidebar_presentation(&mut client_b);
        app.reconcile_sidebar_presentation();
        app.swap_sidebar_presentation(&mut client_b);

        for client in [&client_a, &client_b] {
            assert_eq!(client.workspace_scroll, 0);
            assert_eq!(client.mobile_switcher_scroll, 0);
            assert_eq!(client.projection_revision, revision);
        }
    }

    #[test]
    fn sidebar_disclosure_resets_on_reconnect() {
        let mut app = app_with_agents(&["one", "two"]);
        app.toggle_workspace_agent_disclosure(1);
        let disconnected = std::mem::take(&mut app.sidebar_presentation);
        assert!(!disconnected.expanded_workspace_ids.is_empty());

        app.active = Some(1);
        app.reconcile_sidebar_presentation();
        assert!(app.workspace_agents_expanded(0));
        assert!(app.workspace_agents_expanded(1));

        app.workspaces.remove(1);
        app.active = Some(0);
        app.reconcile_sidebar_presentation();
        assert!(app
            .sidebar_presentation
            .expanded_workspace_ids
            .iter()
            .all(|id| app.workspaces.iter().any(|workspace| &workspace.id == id)));
    }

    #[test]
    fn agent_tree_does_not_change_runtime_snapshot_or_handoff_schema() {
        let snapshot = crate::persist::SessionSnapshot {
            version: 3,
            workspaces: Vec::new(),
            active: None,
            selected: 0,
            sidebar_width: Some(24),
            sidebar_section_split: Some(0.4),
            collapsed_space_keys: std::collections::HashSet::new(),
        };
        let value = serde_json::to_value(snapshot).unwrap();
        let object = value.as_object().unwrap();
        assert!(object.contains_key("sidebar_section_split"));
        assert!(!object.keys().any(|key| key.contains("disclosure")));
        assert!(!object.keys().any(|key| key.contains("expanded_workspace")));
    }

    #[test]
    fn legacy_sidebar_config_and_snapshot_load_into_agent_tree() {
        let config: crate::config::Config = toml::from_str(
            r#"
[ui]
agent_panel_sort = "workspaces"
sidebar_width = 31

[ui.sidebar.spaces]
row_gap = 2

[ui.sidebar.agents]
row_gap = 1
"#,
        )
        .unwrap();
        assert_eq!(
            config.ui.agent_panel_sort,
            crate::config::AgentPanelSortConfig::Spaces
        );
        assert_eq!(config.ui.sidebar_width, 31);
        assert_eq!(config.ui.sidebar.spaces.row_gap, 2);
        assert_eq!(config.ui.sidebar.agents.row_gap, 1);

        let snapshot: crate::persist::SessionSnapshot = serde_json::from_str(
            r#"{"version":3,"workspaces":[],"active":null,"selected":0,"sidebar_width":31,"sidebar_section_split":0.3,"collapsed_space_keys":["repo"]}"#,
        )
        .unwrap();
        assert_eq!(snapshot.sidebar_section_split, Some(0.3));
        assert!(snapshot.collapsed_space_keys.contains("repo"));

        let area = Rect::new(0, 0, 31, 20);
        assert_eq!(
            expanded_sidebar_sections(area, 0.1),
            expanded_sidebar_sections(area, 0.9)
        );
    }

    fn row_text(buffer: &ratatui::buffer::Buffer, row: u16, width: u16) -> String {
        (0..width)
            .map(|x| buffer[(x, row)].symbol())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    fn find_symbol_x(buffer: &ratatui::buffer::Buffer, row: u16, width: u16, symbol: &str) -> u16 {
        (0..width)
            .find(|x| buffer[(*x, row)].symbol() == symbol)
            .unwrap_or_else(|| {
                panic!(
                    "missing symbol {symbol:?} in row {}",
                    row_text(buffer, row, width)
                )
            })
    }

    #[test]
    fn ac1_ac2_ac3_ac4_cumulative_space_first_single_line_fixture() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("Test");
        workspace.tabs[0].custom_name = Some("Summarize recent commits".into());
        let root_pane = workspace.tabs[0].root_pane;
        let split_pane = workspace.test_split(Direction::Horizontal);
        workspace.test_add_tab(Some("Review sidebar fixtures"));
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.mode = Mode::Terminal;
        for pane_id in [root_pane, split_pane] {
            let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let terminal = app.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(Agent::Codex);
            terminal.state = AgentState::Working;
        }
        app.reconcile_sidebar_presentation();

        // ac1 + ac2: one Spaces projection and exactly one row per tab/window.
        assert_eq!(row_kinds(&app), vec![('w', 0), ('t', 0), ('t', 0)]);
        assert!(sidebar_rows(&app)
            .iter()
            .all(|row| !matches!(row, SidebarRow::Agent { .. })));

        let area = Rect::new(0, 0, 60, 20);
        let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let tab_cards = compute_tab_card_areas(&app, area);
        assert_eq!(tab_cards.len(), 2);
        let working = row_text(buffer, tab_cards[0].rect.y, 59);
        let agentless = row_text(buffer, tab_cards[1].rect.y, 59);

        // ac3: lifecycle status is left of the single title, with no model subtitle.
        let status_at = working.find("working").unwrap();
        let title_at = working.find("Summarize recent commits").unwrap();
        assert!(status_at < title_at, "{working:?}");
        assert!(!working.contains("codex"), "{working:?}");
        assert!(
            agentless.contains("Review sidebar fixtures"),
            "{agentless:?}"
        );

        // ac4: two panes roll up to the one owning tab row.
        assert_eq!(tab_cards.iter().filter(|card| card.tab_idx == 0).count(), 1);
    }

    #[test]
    fn default_tab_row_shows_status_and_title_once_without_pane_identity_row() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("repo-folder");
        workspace.tabs[0].custom_name = Some("Fix Billing Retry".into());
        let pane_id = workspace.tabs[0].root_pane;
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.mode = Mode::Terminal;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal_state = app.terminals.get_mut(&terminal_id).unwrap();
        terminal_state.detected_agent = Some(Agent::Pi);
        terminal_state.state = AgentState::Working;

        let area = Rect::new(0, 0, 60, 20);
        let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let (workspace_cards, agent_cards) = compute_sidebar_row_areas(&app, area);
        let tab_cards = compute_tab_card_areas(&app, area);
        let workspace_row = workspace_cards[0].rect.y;
        let tab_row = tab_cards[0].rect.y;

        let first = row_text(buffer, workspace_row, 59);
        let tab_window = row_text(buffer, tab_row, 59);
        assert!(first.contains("repo-folder"));
        assert!(tab_window.contains("Fix Billing Retry"), "{tab_window:?}");
        assert_eq!(tab_window.matches("Fix Billing Retry").count(), 1);
        assert!(!tab_window.contains("pi"), "{tab_window:?}");
        assert!(!first.contains("working"));
        assert!(tab_window.contains("working"));
        assert!(agent_cards.is_empty());

        let workspace_x = find_symbol_x(buffer, workspace_row, 59, "o");
        let workspace_style = buffer[(workspace_x, workspace_row)].style();
        assert_eq!(workspace_style.fg, Some(app.palette.text));
        assert!(workspace_style.add_modifier.contains(Modifier::BOLD));
        assert!(!workspace_style.add_modifier.contains(Modifier::DIM));
        assert_eq!(workspace_style.bg, Some(ratatui::style::Color::Reset));

        let title_x = find_symbol_x(buffer, tab_row, 59, "F");
        let title_style = buffer[(title_x, tab_row)].style();
        assert!(title_style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(title_style.bg, Some(ratatui::style::Color::Reset));
    }

    #[test]
    fn tab_rows_show_working_then_done_lifecycle_text() {
        let started = std::time::Instant::now();
        let mut app = AppState::test_new();
        let workspace = Workspace::test_new("one");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.reconcile_sidebar_presentation();
        let terminal_state = app.terminals.get_mut(&terminal_id).unwrap();
        terminal_state.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Working,
            false,
            false,
            true,
            false,
            started,
        );

        let area = Rect::new(0, 0, 26, 12);
        app.view_observed_at = started + std::time::Duration::from_secs(42);
        let mut busy = Terminal::new(TestBackend::new(26, 12)).unwrap();
        busy.draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let tab_row = compute_tab_card_areas(&app, area)[0].rect.y;
        let busy_text = row_text(busy.backend().buffer(), tab_row, 25);
        assert!(busy_text.contains("working"), "{busy_text:?}");

        let finished = started + std::time::Duration::from_secs(50);
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state_with_screen_signals_at(
                Some(Agent::Pi),
                AgentState::Idle,
                false,
                true,
                false,
                false,
                finished,
            );
        app.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .unwrap()
            .seen = false;
        app.view_observed_at = finished + std::time::Duration::from_secs(5 * 60);
        let mut idle = Terminal::new(TestBackend::new(26, 12)).unwrap();
        idle.draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let idle_text = row_text(idle.backend().buffer(), tab_row, 25);
        assert!(idle_text.contains("done"), "{idle_text:?}");
    }

    #[test]
    fn narrow_tab_rows_keep_status_before_truncated_title() {
        let mut app = app_with_agents(&["one"]);
        app.workspaces[0].tabs[0].custom_name = Some("one".into());
        let area = Rect::new(0, 0, 12, 12);
        let mut terminal = Terminal::new(TestBackend::new(12, 12)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let card = &compute_tab_card_areas(&app, area)[0];
        let rendered = row_text(terminal.backend().buffer(), card.rect.y, 11);

        assert!(
            rendered.contains('●') || rendered.contains('w'),
            "{rendered:?}"
        );
        assert!(!rendered.contains("--"), "{rendered:?}");
    }

    #[test]
    fn review_findings_activity_deadlines_follow_visible_age_fields() {
        let mut app = app_with_agents(&["one"]);
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let started = std::time::Instant::now();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state_with_screen_signals_at(
                Some(Agent::Pi),
                AgentState::Working,
                false,
                false,
                true,
                false,
                started,
            );
        app.status_bar_enabled = false;
        app.mobile_width_threshold = 0;
        let runtimes = TerminalRuntimeRegistry::new();

        crate::ui::compute_view_with_runtime_registry(&mut app, &runtimes, Rect::new(0, 0, 80, 20));
        assert!(app.view.visible_agent_activity_instants.is_empty());

        app.sidebar_collapsed = true;
        crate::ui::compute_view_with_runtime_registry(&mut app, &runtimes, Rect::new(0, 0, 80, 20));
        assert!(app.view.visible_agent_activity_instants.is_empty());

        app.sidebar_collapsed = false;
        app.mobile_width_threshold = 80;
        crate::ui::compute_view_with_runtime_registry(&mut app, &runtimes, Rect::new(0, 0, 80, 20));
        assert!(app.view.visible_agent_activity_instants.is_empty());

        app.mobile_width_threshold = 0;
        app.sidebar_width = 12;
        app.sidebar_min_width = 12;
        app.sidebar_max_width = 12;
        crate::ui::compute_view_with_runtime_registry(&mut app, &runtimes, Rect::new(0, 0, 80, 20));
        assert!(app.view.visible_agent_activity_instants.is_empty());

        app.sidebar_width = 30;
        app.sidebar_min_width = 18;
        app.sidebar_max_width = 36;
        app.toggle_workspace_agent_disclosure(0);
        crate::ui::compute_view_with_runtime_registry(&mut app, &runtimes, Rect::new(0, 0, 80, 20));
        assert!(app.view.visible_agent_activity_instants.is_empty());
    }

    #[test]
    fn legacy_agent_styles_do_not_add_visible_agent_child_rows() {
        let config: crate::config::Config = toml::from_str(
            r##"
[ui.sidebar.agents]
rows = [[{ token = "workspace", bold = false }, { token = "agent", dim = false }]]
"##,
        )
        .unwrap();
        let mut app = crate::app::state::AppState::test_new();
        app.sidebar_agents = config.ui.sidebar.agents;
        let workspace = Workspace::test_new("one");
        let pane_id = workspace.tabs[0].root_pane;
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.active = Some(0);
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Pi);

        let area = Rect::new(0, 0, 26, 20);
        let mut terminal = Terminal::new(TestBackend::new(26, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let tab_row = compute_tab_card_areas(&app, area)[0].rect.y;
        let rendered = row_text(buffer, tab_row, 25);
        assert!(rendered.contains("New Thread"), "{rendered:?}");
        assert!(!rendered.contains("pi"), "{rendered:?}");
        assert!(compute_agent_card_areas(&app, area).is_empty());
    }

    #[test]
    fn default_space_workspace_style_tracks_active_state() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.active = Some(0);
        app.mode = Mode::Terminal;
        let area = Rect::new(0, 0, 26, 20);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let first_row = app.view.workspace_card_areas[0].rect.y;
        let second_row = app.view.workspace_card_areas[1].rect.y;
        let mut terminal = Terminal::new(TestBackend::new(26, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();

        let active = buffer[(find_symbol_x(buffer, first_row, 25, "o"), first_row)].style();
        assert_eq!(active.fg, Some(app.palette.text));
        assert!(active.add_modifier.contains(Modifier::BOLD));
        assert!(!active.add_modifier.contains(Modifier::DIM));
        assert_eq!(active.bg, Some(ratatui::style::Color::Reset));

        let inactive = buffer[(find_symbol_x(buffer, second_row, 25, "t"), second_row)].style();
        assert_eq!(inactive.fg, Some(app.palette.subtext0));
        assert!(!inactive
            .add_modifier
            .intersects(Modifier::BOLD | Modifier::DIM));
        assert_eq!(inactive.bg, Some(ratatui::style::Color::Reset));
    }

    #[test]
    fn space_occurrence_style_applies_without_styling_separator() {
        let config: crate::config::Config = toml::from_str(
            r##"
[ui.sidebar.spaces]
rows = [[{ token = "$hype", fg = "#abcdef", bold = true, dim = false }, "workspace"]]
"##,
        )
        .unwrap();
        let mut app = crate::app::state::AppState::test_new();
        app.sidebar_spaces = config.ui.sidebar.spaces;
        app.workspaces = vec![Workspace::test_new("one")];
        app.active = Some(0);
        app.mode = Mode::Terminal;
        app.workspaces[0].metadata_tokens.patch(
            std::collections::HashMap::from([("hype".into(), Some("HI".into()))]),
            None,
            std::time::Instant::now(),
        );

        let area = Rect::new(0, 0, 26, 20);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let row = app.view.workspace_card_areas[0].rect.y;
        let mut terminal = Terminal::new(TestBackend::new(26, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let h = buffer[(find_symbol_x(buffer, row, 25, "H"), row)].style();
        let i = buffer[(find_symbol_x(buffer, row, 25, "I"), row)].style();
        let separator = buffer[(find_symbol_x(buffer, row, 25, "·"), row)].style();

        for style in [h, i] {
            assert_eq!(style.fg, Some(ratatui::style::Color::Rgb(0xab, 0xcd, 0xef)));
            assert!(style.add_modifier.contains(Modifier::BOLD));
            assert!(!style.add_modifier.contains(Modifier::DIM));
            assert_eq!(style.bg, Some(ratatui::style::Color::Reset));
        }
        assert_eq!(separator.fg, Some(app.palette.overlay0));
        assert!(separator.add_modifier.contains(Modifier::DIM));
        assert!(!separator.add_modifier.contains(Modifier::BOLD));
        assert_eq!(separator.bg, Some(ratatui::style::Color::Reset));
    }

    #[test]
    fn occurrence_foreground_flattens_composite_git_status_colors() {
        let config: crate::config::Config = toml::from_str(
            r##"[ui.sidebar.spaces]
rows = [[{ token = "git_status", fg = "#123456" }]]
"##,
        )
        .unwrap();
        let spans = resolved_token_spans(
            &[ResolvedToken {
                kind: ResolvedTokenKind::GitStatus {
                    ahead: 2,
                    behind: 1,
                },
                style: config.ui.sidebar.spaces.rows[0][0].parts().1,
            }],
            ("", Style::default()),
            Style::default(),
            Style::default(),
            Style::default(),
            Style::default(),
            &crate::app::state::AppState::test_new().palette,
            20,
        );

        assert_eq!(
            spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            "↑2 ↓1"
        );
        assert!(spans
            .iter()
            .all(|span| { span.style.fg == Some(ratatui::style::Color::Rgb(0x12, 0x34, 0x56)) }));
    }

    #[test]
    fn default_agent_row_gap_packs_rendering_and_scroll_geometry() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.ensure_test_terminals();
        for (workspace, agent) in app.workspaces.iter().zip([Agent::Pi, Agent::Claude]) {
            let pane_id = workspace.tabs[0].root_pane;
            let terminal_id = workspace.tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(agent);
        }
        app.sidebar_agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];
        assert_eq!(app.sidebar_agents.row_gap, 0);

        app.agent_panel_sort = AgentPanelSort::Priority;
        app.sidebar_presentation.expanded_workspace_ids = app
            .workspaces
            .iter()
            .map(|workspace| workspace.id.clone())
            .collect();

        let area = Rect::new(0, 0, 20, 5);
        let ws_area = workspace_list_rect(area, app.sidebar_section_split);
        let metrics = workspace_list_scroll_metrics(&app, ws_area);
        let body = workspace_list_body_rect(ws_area, should_show_scrollbar(metrics));
        let mut terminal = Terminal::new(TestBackend::new(20, 5)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();

        assert!(metrics.viewport_rows >= 2);
        let first = row_text(buffer, body.y, body.width);
        let second = row_text(buffer, body.y + 1, body.width);
        assert!(!first.is_empty(), "{first:?}");
        assert!(!second.is_empty(), "{second:?}");
    }

    #[test]
    fn narrow_agent_rows_preserve_later_tab_tokens() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("very-long-workspace-name");
        let tab_idx = workspace.test_add_tab(Some("logs"));
        let pane_id = workspace.tabs[tab_idx].root_pane;
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[tab_idx].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Pi);
        app.active = Some(0);
        app.reconcile_sidebar_presentation();

        let area = Rect::new(0, 0, 18, 20);
        let mut terminal = Terminal::new(TestBackend::new(18, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let card = compute_tab_card_areas(&app, area)
            .into_iter()
            .find(|card| card.tab_idx == tab_idx)
            .unwrap();
        let first = (card.rect.y..card.rect.y + card.rect.height)
            .map(|row| row_text(buffer, row, 17))
            .find(|line| line.contains("logs"))
            .unwrap_or_else(|| {
                panic!(
                    "missing tab context in {:?}",
                    (card.rect.y..card.rect.y + card.rect.height)
                        .map(|row| row_text(buffer, row, 17))
                        .collect::<Vec<_>>()
                )
            });

        assert!(first.contains("logs"), "rendered row: {first:?}");
        assert!(!first.contains("very-long-workspace-name"), "{first:?}");
    }

    #[test]
    fn stripped_terminal_title_renders_with_unicode_width_truncation() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("one");
        let pane_id = workspace.tabs[0].root_pane;
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(Agent::Claude);
        terminal.set_terminal_title(Some("⠋ 修复🙂标题很长".into()));
        app.active = Some(0);
        app.reconcile_sidebar_presentation();
        app.sidebar_agents.rows = vec![vec![
            crate::config::AgentSidebarToken::TerminalTitleStripped,
        ]];

        assert!(compute_agent_card_areas(&app, Rect::new(0, 0, 10, 12)).is_empty());

        let spans = resolved_token_spans(
            &[ResolvedToken::unstyled(ResolvedTokenKind::TerminalTitle(
                "修复🙂标题很长".into(),
            ))],
            ("", Style::default()),
            Style::default(),
            Style::default(),
            Style::default(),
            Style::default(),
            &app.palette,
            8,
        );
        let text = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(display_width(&text) <= 8, "resolved title: {text:?}");
    }

    #[test]
    fn legacy_agent_heights_do_not_change_single_line_tab_scroll_geometry() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![
            Workspace::test_new("one"),
            Workspace::test_new("two"),
            Workspace::test_new("three"),
        ];
        app.ensure_test_terminals();
        for workspace in &app.workspaces {
            let pane_id = workspace.tabs[0].root_pane;
            let terminal_id = workspace.tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Pi);
        }
        let first_pane = app.workspaces[0].tabs[0].root_pane;
        let first_terminal = app.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&first_terminal)
            .unwrap()
            .metadata_tokens
            .patch(
                std::collections::HashMap::from([
                    ("a".into(), Some("a".into())),
                    ("b".into(), Some("b".into())),
                ]),
                None,
                std::time::Instant::now(),
            );
        app.sidebar_agents.rows = vec![
            vec![crate::config::AgentSidebarToken::Agent],
            vec![crate::config::AgentSidebarToken::Custom("a".into())],
            vec![crate::config::AgentSidebarToken::Custom("b".into())],
        ];
        app.agent_panel_sort = AgentPanelSort::Priority;
        app.sidebar_presentation.expanded_workspace_ids = app
            .workspaces
            .iter()
            .map(|workspace| workspace.id.clone())
            .collect();
        let area = Rect::new(0, 0, 20, 4);
        let ws_area = workspace_list_rect(area, app.sidebar_section_split);

        let metrics = workspace_list_scroll_metrics(&app, ws_area);
        assert!(metrics.max_offset_from_bottom >= 1);
        let rows = sidebar_rows(&app);
        let target = rows.len() - 1;
        assert!(sidebar_row_scroll_for_target(&app, area, 0, target) >= 1);
        assert!(compute_tab_card_areas(&app, area)
            .iter()
            .all(|card| card.rect.height == 1));
    }

    #[test]
    fn oversized_space_layout_is_clipped_to_the_section_body() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]; 10];
        let area = Rect::new(0, 0, 20, 10);
        let workspace_area = workspace_list_rect(area, app.sidebar_section_split);
        let body = workspace_list_body_rect(workspace_area, false);

        let metrics = workspace_list_scroll_metrics(&app, workspace_area);
        let (cards, _) = compute_workspace_list_areas(&app, area);

        assert_eq!(metrics.viewport_rows, 1);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].ws_idx, 0);
        assert_eq!(cards[0].rect.height, body.height);
    }

    #[test]
    fn oversized_agent_override_is_clipped_to_the_panel_body() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("one");
        let pane_id = workspace.tabs[0].root_pane;
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Claude);
        app.sidebar_agents.rows_by_agent.insert(
            "claude".into(),
            vec![vec![crate::config::AgentSidebarToken::Agent]; 6],
        );
        app.agent_panel_sort = AgentPanelSort::Priority;
        let panel = Rect::new(0, 0, 20, 5);
        let ws_area = workspace_list_rect(panel, app.sidebar_section_split);
        let body = workspace_list_body_rect(ws_area, false);

        let metrics = workspace_list_scroll_metrics(&app, ws_area);

        assert_eq!(metrics.viewport_rows, 1);
        assert_eq!(metrics.max_offset_from_bottom, 0);
        let entry = agent_panel_entries(&app).pop().unwrap();
        assert_eq!(
            agent_entry_height_in_body(&app, &entry, body.height),
            body.height
        );
    }

    #[test]
    fn render_sidebar_toggle_draws_expanded_collapse_icon() {
        let app = crate::app::state::AppState::test_new();
        let area = Rect::new(0, 0, 26, 20);
        let mut terminal =
            Terminal::new(TestBackend::new(26, 20)).expect("test terminal should initialize");

        terminal
            .draw(|frame| render_sidebar_toggle(&app, frame, area, false, &app.palette))
            .expect("sidebar toggle should render");

        let toggle = expanded_sidebar_toggle_rect(area);
        assert_eq!(
            terminal.backend().buffer()[(toggle.x, toggle.y)].symbol(),
            "«"
        );
    }

    #[test]
    fn expanded_sidebar_toggle_sits_inside_sidebar_content() {
        let area = Rect::new(0, 0, 26, 20);
        let toggle = expanded_sidebar_toggle_rect(area);

        assert_eq!(toggle.x, area.x);
        assert_eq!(toggle.y, area.y);
    }

    #[test]
    fn expanded_sidebar_header_matches_deployed_controls() {
        for width in [26, 18] {
            let app = crate::app::state::AppState::test_new();
            let area = Rect::new(0, 0, width, 8);
            let mut terminal =
                Terminal::new(TestBackend::new(width, 8)).expect("test terminal should initialize");

            terminal
                .draw(|frame| {
                    render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area);
                })
                .expect("sidebar should render");

            let header = row_text(terminal.backend().buffer(), 0, width - 1);
            assert!(header.starts_with('«'));
            assert!(header.contains("Spaces"));
            assert!(header.contains('＋'));
            assert!(header.contains('…'));
            assert!(!row_text(terminal.backend().buffer(), 7, width - 1).contains("menu"));
        }
    }

    #[test]
    fn agent_panel_tab_labels_use_titles_and_safe_defaults() {
        let mut app = crate::app::state::AppState::test_new();
        let single_auto = Workspace::test_new("auto");
        let mut single_custom = Workspace::test_new("custom");
        single_custom.tabs[0].set_custom_name("focus".into());
        let mut multi = Workspace::test_new("multi");
        multi.test_add_tab(Some("logs"));

        app.workspaces = vec![single_auto, single_custom, multi];
        app.ensure_test_terminals();
        for (ws_idx, tab_idx, agent) in [
            (0, 0, Agent::Pi),
            (1, 0, Agent::Claude),
            (2, 0, Agent::Codex),
            (2, 1, Agent::Pi),
        ] {
            let pane_id = app.workspaces[ws_idx].tabs[tab_idx].root_pane;
            let terminal_id = app.workspaces[ws_idx].tabs[tab_idx].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(agent);
        }

        let entries = agent_panel_entries(&app);
        let labels: Vec<_> = entries
            .iter()
            .map(|entry| {
                (
                    entry.primary_label.as_str(),
                    entry.primary_tab_label.as_deref(),
                )
            })
            .collect();

        assert_eq!(
            labels,
            [
                ("auto", Some("New Thread")),
                ("custom", Some("focus")),
                ("multi", Some("New Thread")),
                ("multi", Some("logs")),
            ]
        );
    }

    #[test]
    fn priority_agent_panel_sort_uses_attention_then_space_order() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![
            Workspace::test_new("one"),
            Workspace::test_new("two"),
            Workspace::test_new("three"),
            Workspace::test_new("four"),
        ];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.selected = 0;
        app.agent_panel_sort = crate::app::state::AgentPanelSort::Priority;

        let set_state = |app: &mut crate::app::state::AppState, ws_idx: usize, state| {
            let pane = app.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            let terminal = app.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(Agent::Claude);
            terminal.state = state;
        };
        set_state(&mut app, 0, AgentState::Working);
        set_state(&mut app, 1, AgentState::Idle);
        set_state(&mut app, 2, AgentState::Working);
        set_state(&mut app, 3, AgentState::Blocked);

        let done_pane = app.workspaces[1].tabs[0].root_pane;
        app.workspaces[1].tabs[0]
            .panes
            .get_mut(&done_pane)
            .unwrap()
            .seen = false;

        let labels: Vec<String> = agent_panel_entries(&app)
            .into_iter()
            .map(|entry| entry.primary_label)
            .collect();

        assert_eq!(labels, ["four", "two", "one", "three"]);
    }

    #[test]
    fn collapsed_sidebar_numbers_grouped_agents_by_list_position() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.ensure_test_terminals();

        for ws_idx in 0..app.workspaces.len() {
            let pane = app.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            let terminal = app.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(Agent::Claude);
            terminal.state = AgentState::Idle;
        }

        let area = Rect::new(0, 0, 4, 12);
        let (_, _, detail_area) = collapsed_sidebar_sections(area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("test terminal should initialize");

        terminal
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .expect("collapsed sidebar should render");

        let buffer = terminal.backend().buffer();
        assert_ne!(buffer[(detail_area.x, detail_area.y)].symbol(), "");
        assert_ne!(buffer[(detail_area.x, detail_area.y + 1)].symbol(), "");
    }

    #[test]
    fn collapsed_sidebar_keeps_status_visible_for_two_digit_positions() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = (1..=10)
            .map(|idx| Workspace::test_new(&format!("workspace-{idx}")))
            .collect();
        app.ensure_test_terminals();

        for ws_idx in 0..app.workspaces.len() {
            let pane = app.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Claude);
        }

        let area = Rect::new(0, 0, 4, 25);
        let (_, _, detail_area) = collapsed_sidebar_sections(area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("test terminal should initialize");

        terminal
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .expect("collapsed sidebar should render");

        let tenth_row = detail_area.y + 9;
        let buffer = terminal.backend().buffer();
        assert_ne!(buffer[(detail_area.x, tenth_row)].symbol(), "");
    }

    #[test]
    fn collapsed_sidebar_numbers_priority_agents_by_list_position() {
        let first = Workspace::test_new("one");
        let first_pane = first.tabs[0].root_pane;
        let mut second = Workspace::test_new("two");
        let second_pane = second.tabs[0].root_pane;
        let urgent_pane = second.test_split(ratatui::layout::Direction::Horizontal);

        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![first, second];
        app.ensure_test_terminals();
        app.agent_panel_sort = crate::app::state::AgentPanelSort::Priority;

        let set_state = |app: &mut crate::app::state::AppState, ws_idx: usize, pane_id, state| {
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let terminal = app.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(Agent::Claude);
            terminal.state = state;
        };
        set_state(&mut app, 0, first_pane, AgentState::Working);
        set_state(&mut app, 1, second_pane, AgentState::Working);
        set_state(&mut app, 1, urgent_pane, AgentState::Blocked);

        assert_eq!(app.workspaces[1].public_pane_number(urgent_pane), Some(2));
        assert_eq!(all_agent_panel_entries(&app)[0].pane_id, first_pane);

        let area = Rect::new(0, 0, 4, 16);
        let (_, _, detail_area) = collapsed_sidebar_sections(area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("test terminal should initialize");

        terminal
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .expect("collapsed sidebar should render");

        let buffer = terminal.backend().buffer();
        assert_ne!(buffer[(detail_area.x, detail_area.y)].symbol(), "");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn all_workspaces_agent_panel_entries_use_live_root_runtime_cwd_for_workspace_label() {
        let unique = format!(
            "herdr-agent-panel-runtime-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let stale_cwd = root.join("issue-264-nix-support");
        let live_cwd = root.join("herdr");
        std::fs::create_dir_all(stale_cwd.join(".git")).unwrap();
        std::fs::create_dir_all(live_cwd.join(".git")).unwrap();

        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("stale-name");
        workspace.custom_name = None;
        workspace.identity_cwd = stale_cwd.clone();
        let pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.cwd = stale_cwd;
        terminal.detected_agent = Some(Agent::Pi);
        app.active = Some(0);
        app.selected = 0;

        let (events, _) = tokio::sync::mpsc::channel(4);
        let runtime = crate::terminal::TerminalRuntime::spawn(
            pane,
            24,
            80,
            live_cwd.clone(),
            0,
            crate::terminal_theme::TerminalTheme::default(),
            crate::pane::PaneShellConfig::new("/bin/sh", crate::config::ShellModeConfig::NonLogin),
            &crate::pane::PaneLaunchEnv::default(),
            events,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(crate::render_signal::RenderSignal::new()),
        )
        .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while runtime.cwd() != Some(live_cwd.clone()) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let mut runtime_registry = TerminalRuntimeRegistry::new();
        runtime_registry.insert(terminal_id, runtime);
        let entries = agent_panel_entries_from(&app, &runtime_registry);
        let primary_label = entries[0].primary_label.clone();

        for (_, runtime) in runtime_registry.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_dir_all(root);

        assert_eq!(primary_label, "herdr");
    }

    #[test]
    fn all_workspaces_agent_panel_entries_prefer_agent_names_for_agent_identity() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("bridge");
        let first_pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let first_terminal_id = app.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .set_agent_name("planner".into());
        app.active = Some(0);
        app.selected = 0;

        let entries = agent_panel_entries(&app);
        assert_eq!(entries[0].primary_label, "bridge");
        assert_eq!(entries[0].agent_label.as_deref(), Some("planner"));
    }

    #[test]
    fn expanded_sidebar_sections_handle_tiny_heights() {
        let (ws_area, detail_area) = expanded_sidebar_sections(Rect::new(0, 0, 20, 5), 0.9);

        assert_eq!(ws_area, Rect::new(0, 0, 19, 5));
        assert_eq!(detail_area, ws_area);
    }

    #[test]
    fn grouped_child_label_keeps_custom_workspace_name() {
        assert_eq!(
            grouped_child_display_label("renamed issue", Some("worktree/issue-137"), true),
            "renamed issue"
        );
    }

    #[test]
    fn grouped_child_label_uses_short_branch_for_auto_named_workspace() {
        assert_eq!(
            grouped_child_display_label("herdr-issue", Some("worktree/issue-137"), false),
            "issue-137"
        );
    }

    #[test]
    fn workspace_list_truncates_cjk_branch_without_panic() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("repo");
        ws.cached_git_branch = Some("feature/中文-分支-644".into());
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.view.workspace_card_areas = vec![crate::app::state::WorkspaceCardArea {
            ws_idx: 0,
            rect: Rect::new(0, 1, 15, 2),
            indented: false,
        }];

        let mut terminal = Terminal::new(TestBackend::new(15, 6)).expect("test terminal");
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();

        terminal
            .draw(|frame| {
                render_workspace_list(&app, &runtimes, frame, Rect::new(0, 0, 15, 6), false)
            })
            .expect("workspace list should render");
    }

    fn workspace_with_worktree_space(
        name: &str,
        key: Option<&str>,
        checkout_key: &str,
    ) -> crate::workspace::Workspace {
        let mut ws = crate::workspace::Workspace::test_new(name);
        if let Some(key) = key {
            ws.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
                key: key.into(),
                label: "herdr".into(),
                repo_root: std::path::PathBuf::from("/repo/herdr"),
                checkout_path: std::path::PathBuf::from(checkout_key),
                is_linked_worktree: name != "main",
            });
        }
        ws
    }

    fn workspace_with_git_space(name: &str, key: &str) -> crate::workspace::Workspace {
        let mut ws = crate::workspace::Workspace::test_new(name);
        ws.cached_git_space = Some(crate::workspace::GitSpaceMetadata {
            key: key.into(),
            checkout_key: format!("/repo/{name}"),
            repo_name: "herdr".into(),
            repo_root: std::path::PathBuf::from(format!("/repo/{name}")),
            is_linked_worktree: false,
        });
        ws
    }

    #[test]
    fn desktop_worktree_tree_aligns_parents_and_marks_children() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            workspace_with_worktree_space("review", Some("repo-key"), "/repo/herdr-review"),
            Workspace::test_new("notes"),
        ];
        app.sidebar_spaces.rows = vec![vec![
            crate::config::SpaceSidebarToken::StateIcon,
            crate::config::SpaceSidebarToken::Workspace,
        ]];
        app.sidebar_spaces.row_gap = 0;
        let area = Rect::new(0, 0, 30, 20);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let list_area = workspace_list_rect(area, app.sidebar_section_split);

        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| {
                render_workspace_list(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    list_area,
                    false,
                )
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let cards = &app.view.workspace_card_areas;
        let parent_name_x = find_symbol_x(buffer, cards[0].rect.y, cards[0].rect.width, "m");
        let plain_name_x = find_symbol_x(buffer, cards[3].rect.y, cards[3].rect.width, "n");
        assert_eq!(parent_name_x, plain_name_x);
        assert_eq!(buffer[(cards[1].rect.x + 3, cards[1].rect.y)].symbol(), "├");
        assert_eq!(buffer[(cards[2].rect.x + 3, cards[2].rect.y)].symbol(), "└");
        assert_eq!(
            buffer[(cards[0].rect.x + cards[0].rect.width - 1, cards[0].rect.y)].symbol(),
            "▾"
        );
    }

    #[test]
    fn desktop_worktree_connector_uses_full_list_at_viewport_boundary() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            workspace_with_worktree_space("review", Some("repo-key"), "/repo/herdr-review"),
        ];
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_spaces.row_gap = 0;
        let area = Rect::new(0, 0, 30, 3);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        assert_eq!(app.view.workspace_card_areas.len(), 2);
        let list_area = workspace_list_rect(area, app.sidebar_section_split);

        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| {
                render_workspace_list(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    list_area,
                    false,
                )
            })
            .unwrap();

        let child = app.view.workspace_card_areas[1];
        assert_eq!(
            terminal.backend().buffer()[(child.rect.x + 3, child.rect.y)].symbol(),
            "├"
        );
    }

    #[test]
    fn parent_workspace_row_stays_clickable_when_grouped() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];
        app.sidebar_spaces.row_gap = 1;

        let (cards, headers) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 20));

        assert!(headers.is_empty());
        assert_eq!(cards[0].ws_idx, 0);
        assert!(!cards[0].indented);
        assert_eq!(cards[1].ws_idx, 1);
        assert!(cards[1].indented);
        assert_eq!(cards[1].rect.y, cards[0].rect.y + cards[0].rect.height + 1);
    }

    #[test]
    fn space_row_gap_preserves_compact_worktree_children() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            workspace_with_worktree_space("review", Some("repo-key"), "/repo/herdr-review"),
            Workspace::test_new("notes"),
        ];
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_spaces.row_gap = 2;

        let (spacious, _) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 30));
        assert_eq!(
            spacious[1].rect.y,
            spacious[0].rect.y + spacious[0].rect.height + 2
        );
        assert_eq!(
            spacious[2].rect.y,
            spacious[1].rect.y + spacious[1].rect.height
        );
        assert_eq!(
            spacious[3].rect.y,
            spacious[2].rect.y + spacious[2].rect.height + 2
        );
        let spacious_metrics = workspace_list_scroll_metrics(&app, Rect::new(0, 0, 30, 5));
        assert_eq!(spacious_metrics.viewport_rows, 2);
        assert_eq!(spacious_metrics.max_offset_from_bottom, 2);

        app.sidebar_spaces.row_gap = 0;
        let (packed, _) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 30));
        assert!(packed
            .windows(2)
            .all(|pair| pair[1].rect.y == pair[0].rect.y + pair[0].rect.height));
        let packed_metrics = workspace_list_scroll_metrics(&app, Rect::new(0, 0, 30, 5));
        assert_eq!(packed_metrics.viewport_rows, 4);
        assert_eq!(packed_metrics.max_offset_from_bottom, 0);
    }

    #[test]
    fn packed_workspace_drag_indicator_overlays_an_internal_boundary() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            Workspace::test_new("a"),
            Workspace::test_new("b"),
            Workspace::test_new("c"),
        ];
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_spaces.row_gap = 0;
        let area = Rect::new(0, 0, 30, 20);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let list_area = workspace_list_rect(area, app.sidebar_section_split);
        let indicator_row = workspace_drop_indicator_row(
            &app,
            &app.view.workspace_card_areas,
            list_area,
            crate::app::state::WorkspaceDropTarget::Before(2),
        )
        .unwrap();
        assert_eq!(indicator_row, app.view.workspace_card_areas[1].rect.y);
        app.drag = Some(crate::app::state::DragState {
            target: crate::app::state::DragTarget::WorkspaceReorder {
                source_ws_idx: 0,
                drop_target: Some(crate::app::state::WorkspaceDropTarget::Before(2)),
            },
        });

        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| {
                render_workspace_list(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    list_area,
                    false,
                )
            })
            .unwrap();

        assert_eq!(
            terminal.backend().buffer()[(list_area.x, indicator_row)].symbol(),
            "─"
        );
    }

    #[test]
    fn linked_only_worktree_members_do_not_form_parentless_group() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            workspace_with_worktree_space("review", Some("repo-key"), "/repo/herdr-review"),
        ];

        let entries = workspace_list_entries(&app);

        assert_eq!(
            entries,
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false
                },
            ]
        );
    }

    #[test]
    fn compact_space_group_scroll_clamps_when_all_entries_fit() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("one", Some("repo-key"), "/repo/herdr-one"),
            workspace_with_worktree_space("two", Some("repo-key"), "/repo/herdr-two"),
        ];
        let area = Rect::new(0, 0, 30, 20);
        app.workspace_scroll = normalized_workspace_scroll(&app, area, 2);

        let (cards, headers) = compute_workspace_list_areas(&app, area);

        assert!(headers.is_empty());
        assert_eq!(app.workspace_scroll, 0);
        assert_eq!(cards.len(), 3);
        assert_eq!(cards[2].ws_idx, 2);
    }

    #[test]
    fn workspace_scroll_metrics_count_display_entries_not_raw_workspaces() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            Workspace::test_new("notes"),
        ];
        for workspace in &mut app.workspaces {
            workspace.cached_git_branch = Some("main".into());
        }
        app.collapsed_space_keys.insert("repo-key".into());
        app.active = None;
        app.mode = Mode::Terminal;

        let ws_area = Rect::new(0, 0, 30, 5);
        let metrics = workspace_list_scroll_metrics(&app, ws_area);

        assert_eq!(metrics.viewport_rows, 2);
        assert_eq!(metrics.max_offset_from_bottom, 0);
        assert_eq!(metrics.offset_from_bottom, 0);
    }

    #[test]
    fn workspace_scroll_offset_applies_to_group_children() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            Workspace::test_new("notes"),
        ];
        app.collapsed_space_keys.insert("repo-key".into());
        app.active = None;
        app.mode = Mode::Terminal;
        app.workspace_scroll = 1;

        let (cards, headers) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 2));

        assert!(headers.is_empty());
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].ws_idx, 2);
    }

    #[test]
    fn workspace_list_entries_group_multiple_workspaces_in_same_git_space() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_group_non_contiguous_explicit_members() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_git_space("normal", "other-key"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 2,
                    indented: true,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_do_not_group_normal_git_workspaces() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_git_space("one", "repo-key"),
            workspace_with_git_space("two", "repo-key"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_do_not_auto_attach_normal_git_workspace_to_group() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_git_space("scratch", "repo-key"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 2,
                    indented: true,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_leave_single_git_and_non_git_workspaces_flat() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_git_space("one", "repo-key"),
            workspace_with_worktree_space("notes", None, "/notes"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn collapsed_group_hides_inactive_children_but_keeps_active_visible() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];
        app.active = Some(1);
        app.mode = Mode::Terminal;
        app.collapsed_space_keys.insert("repo-key".into());

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );

        app.active = None;
        app.mode = Mode::Terminal;
        assert_eq!(
            workspace_list_entries(&app),
            vec![WorkspaceListEntry::Workspace {
                ws_idx: 0,
                indented: false,
            }]
        );
    }

    #[test]
    fn collapsed_group_keeps_selected_child_visible_in_navigate_mode() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];
        app.mode = Mode::Navigate;
        app.selected = 1;
        app.active = Some(1);
        app.collapsed_space_keys.insert("repo-key".into());

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );
    }
}
