mod tokens;

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use self::tokens::{ResolvedToken, ResolvedTokenKind};
use super::scrollbar::{render_scrollbar, should_show_scrollbar};
use super::status::{
    state_icon_with_stale, state_label, state_label_color_with_stale,
    status_report_age_compact_label, status_report_age_label,
};
use super::text::{display_width, display_width_u16, truncate_end};
use crate::app::state::Palette;
use crate::app::{AppState, Mode};
use crate::config::StatusIndicatorStyle;
use crate::detect::{Agent, AgentState};
use crate::terminal::TerminalRuntimeRegistry;

const WORKSPACE_SECTION_HEADER_ROWS: u16 = 1;
const PRIO_PANEL_HEADER_ROWS: u16 = 1;
const MIN_WORKSPACE_LIST_ROWS: u16 = 3;
const AGENT_ACTIVITY_AGE_FIELD_WIDTH: usize = 5;
const AGENT_ACTIVITY_AGE_MIN_CONTENT_WIDTH: usize = 8;
const TAB_ACTIVITY_AGE_MIN_TITLE_WIDTH: usize = 3;
pub(crate) const TAB_PRIO_FIELD_WIDTH: usize = 2;
const DEFAULT_THREAD_TITLE: &str = "New Thread";

pub(crate) fn sidebar_separator_col(area: Rect) -> Option<u16> {
    (area.width > 0).then(|| area.x + area.width.saturating_sub(1))
}

pub(crate) fn tab_agent_suffix(agent: Option<Agent>) -> Option<&'static str> {
    match agent {
        Some(Agent::Codex) => Some("cx"),
        Some(Agent::Claude) => Some("cc"),
        Some(Agent::Pi) => Some("pi"),
        _ => None,
    }
}

fn title_repeats_agent_identity(entry: &AgentPanelEntry, title: &str) -> bool {
    let title = title.trim().to_ascii_lowercase();
    let provider = match entry.agent {
        Some(Agent::Codex) => Some("codex"),
        Some(Agent::Claude) => Some("claude"),
        Some(Agent::Pi) => Some("pi"),
        _ => None,
    };
    entry
        .agent_label
        .as_deref()
        .into_iter()
        .chain(entry.agent_kind_label.as_deref())
        .chain(provider)
        .any(|identity| identity.trim().eq_ignore_ascii_case(&title))
}

pub(super) fn tab_lifecycle_visible(entry: &AgentPanelEntry) -> bool {
    entry.has_agent && (entry.stale || entry.state != AgentState::Idle || !entry.seen)
}

pub(super) struct TabRowLayout {
    pub state: Option<String>,
    pub show_state_label: bool,
    pub title: String,
    pub agent_suffix: Option<String>,
    pub foreground_process: Option<String>,
    pub background_jobs: Option<String>,
    pub activity_age: Option<String>,
    pub activity_instant: Option<std::time::Instant>,
}

pub(super) fn tab_row_layout(
    entry: &AgentPanelEntry,
    now: std::time::Instant,
    width: usize,
    prefix_width: usize,
    palette: &Palette,
    indicator_style: StatusIndicatorStyle,
) -> TabRowLayout {
    let state = tab_lifecycle_visible(entry).then(|| {
        entry
            .state_labels
            .get(agent_panel_status_key_with_stale(
                entry.state,
                entry.seen,
                entry.stale,
            ))
            .cloned()
            .unwrap_or_else(|| match entry.state {
                _ if entry.stale => "stale".to_string(),
                AgentState::Idle if !entry.seen => "done".to_string(),
                _ => state_label(entry.state, entry.seen).to_string(),
            })
    });
    let dot_width = state
        .as_ref()
        .map(|_| {
            display_width(
                state_icon_with_stale(
                    entry.state,
                    entry.seen,
                    entry.stale,
                    indicator_style,
                    palette,
                )
                .0,
            )
        })
        .unwrap_or_default();
    let full_status_width = state
        .as_deref()
        .map(|label| dot_width + 1 + display_width(label) + display_width(" · "))
        .unwrap_or_default();
    let dot_status_width = state.as_ref().map(|_| dot_width + 1).unwrap_or_default();
    // The prio cell is reserved on every row so its click target never moves. Overview rows carry
    // no work-context marker: links live in the session's work-context panel, and on a narrow
    // sidebar a second gutter cell would push the activity age off the row entirely.
    let tab_gutter_width = TAB_PRIO_FIELD_WIDTH;
    let tab_title = entry
        .primary_tab_label
        .as_deref()
        .unwrap_or(DEFAULT_THREAD_TITLE);
    let agent_suffix = (!entry.tab_label_leads_with_agent
        && !title_repeats_agent_identity(entry, tab_title))
    .then(|| tab_agent_suffix(entry.agent))
    .flatten()
    .map(|suffix| format!(" · {suffix}"));
    let agent_suffix_width = agent_suffix
        .as_deref()
        .map(display_width)
        .unwrap_or_default();
    let mut background_jobs = entry
        .background_job_count
        .filter(|count| *count > 0)
        .map(|count| format!("  {count} >_"));
    let (mut activity_age, mut activity_instant) =
        if let Some(label) = status_report_age_label(entry.reported_at, now) {
            (Some(label), entry.reported_at)
        } else if let Some(activity_at) = entry.activity_at {
            (
                Some(format!(
                    "{} ago",
                    crate::activity_age::compact_label(Some(activity_at), now)
                )),
                Some(activity_at),
            )
        } else {
            (None, None)
        };

    let background_width = background_jobs
        .as_deref()
        .map(display_width)
        .unwrap_or_default();
    let activity_width = activity_age
        .as_deref()
        .map(|label| 1 + display_width(label))
        .unwrap_or_default();
    let mut foreground_process = entry
        .foreground_process_name
        .as_deref()
        .map(|name| format!(" · {name}"));
    if let Some(process) = foreground_process.as_mut() {
        let process_budget = width.saturating_sub(
            prefix_width
                + tab_gutter_width
                + full_status_width
                + agent_suffix_width
                + background_width
                + activity_width
                + TAB_ACTIVITY_AGE_MIN_TITLE_WIDTH,
        );
        if process_budget < display_width(" · ") + 1 {
            foreground_process = None;
        } else {
            *process = truncate_end(process, process_budget.min(display_width(process)));
        }
    }
    let foreground_process_width = foreground_process
        .as_deref()
        .map(display_width)
        .unwrap_or_default();
    if activity_age.as_deref().is_some_and(|label| {
        width
            < prefix_width
                + tab_gutter_width
                + full_status_width
                + agent_suffix_width
                + foreground_process_width
                + background_width
                + TAB_ACTIVITY_AGE_MIN_TITLE_WIDTH
                + 1
                + display_width(label)
    }) {
        activity_age = None;
        activity_instant = None;
    }
    let activity_width = activity_age
        .as_deref()
        .map(|label| 1 + display_width(label))
        .unwrap_or_default();
    if width
        < prefix_width
            + tab_gutter_width
            + full_status_width
            + agent_suffix_width
            + foreground_process_width
            + background_width
            + activity_width
            + 1
    {
        background_jobs = None;
    }
    let background_width = background_jobs
        .as_deref()
        .map(display_width)
        .unwrap_or_default();
    let show_state_label = width
        > prefix_width
            + tab_gutter_width
            + full_status_width
            + agent_suffix_width
            + background_width
            + activity_width;
    let status_width = if show_state_label {
        full_status_width
    } else {
        dot_status_width
    };
    let fixed_width = prefix_width
        + tab_gutter_width
        + status_width
        + agent_suffix_width
        + foreground_process_width
        + background_width
        + activity_width;
    let title = truncate_end(tab_title, width.saturating_sub(fixed_width));

    TabRowLayout {
        state,
        show_state_label,
        title,
        agent_suffix,
        foreground_process,
        background_jobs,
        activity_age,
        activity_instant,
    }
}

/// Selected sidebar titles need stronger foreground contrast without restoring
/// the old filled-row treatment. Darken RGB text tokens by one third and pair
/// them with bold weight; terminal/reset themes keep their authored foreground.
pub(crate) fn active_sidebar_title_color(palette: &Palette) -> Color {
    if palette.panel_bg == Color::Reset {
        return palette.text;
    }
    match palette.text {
        Color::Rgb(r, g, b) => Color::Rgb(
            ((u16::from(r) * 2) / 3) as u8,
            ((u16::from(g) * 2) / 3) as u8,
            ((u16::from(b) * 2) / 3) as u8,
        ),
        color => color,
    }
}

#[derive(Clone)]
pub(crate) struct AgentPanelEntry {
    pub ws_idx: usize,
    pub tab_idx: usize,
    pub pane_id: crate::layout::PaneId,
    pub primary_label: String,
    pub primary_tab_label: Option<String>,
    pub tab_label_leads_with_agent: bool,
    pub pane_label: Option<String>,
    pub terminal_title: Option<String>,
    pub terminal_title_stripped: Option<String>,
    pub agent_label: Option<String>,
    pub agent_kind_label: Option<String>,
    pub agent: Option<crate::detect::Agent>,
    pub foreground_process_name: Option<String>,
    /// Current or most recently exited provider, used only to detect
    /// ambiguous multi-pane rollups. Rendering still uses `agent` so an exited
    /// provider never leaves a stale suffix on a single-pane row.
    pub agent_context: Option<crate::detect::Agent>,
    /// At least one pane in this row is agent-backed. This stays true for a
    /// rolled-up tab whose panes have conflicting providers, while `agent`
    /// becomes `None` so the provider suffix is not misleading.
    pub has_agent: bool,
    pub prio: bool,
    pub state: AgentState,
    pub background_job_count: Option<u16>,
    pub seen: bool,
    pub stale: bool,
    pub reported_at: Option<std::time::Instant>,
    pub last_agent_state_change_seq: Option<u64>,
    pub activity_at: Option<std::time::Instant>,
    pub state_labels: std::collections::HashMap<String, String>,
    pub tokens: std::collections::HashMap<String, String>,
    /// First pane in canonical layout order for its tab. The renderer uses it
    /// to project the tab row exactly once before its pane children.
    pub tab_first_pane: bool,
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn expanded_sidebar_sections(area: Rect, split_ratio: f32) -> (Rect, Rect) {
    // This is the single vertical partition for the expanded sidebar. All
    // list geometry, scrolling, rendering, and hit-testing derive from it.
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height == 0 {
        return (Rect::default(), Rect::default());
    }
    let max_panel_height = content.height.saturating_sub(MIN_WORKSPACE_LIST_ROWS);
    let ratio = split_ratio.clamp(0.0, 1.0);
    let panel_height = (f32::from(content.height) * ratio)
        .round()
        .clamp(1.0, f32::from(max_panel_height.max(1))) as u16;
    let panel_height = panel_height
        .clamp(1, content.height)
        .min(max_panel_height.max(1));
    let list_height = content.height.saturating_sub(panel_height);
    (
        Rect::new(content.x, content.y, content.width, list_height),
        Rect::new(
            content.x,
            content.y + list_height,
            content.width,
            panel_height,
        ),
    )
}

fn expanded_sidebar_sections_for_app(app: &AppState, area: Rect) -> (Rect, Rect) {
    let entries = prio_panel_entries(app, None);
    let panel_collapsed = app.prio_panel_collapsed || entries.is_empty();
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height == 0 {
        return (Rect::default(), Rect::default());
    }

    let entry_rows = entries.len().min(usize::from(u16::MAX)) as u16;
    let minimum_panel_height = if panel_collapsed {
        PRIO_PANEL_HEADER_ROWS
    } else {
        PRIO_PANEL_HEADER_ROWS.saturating_add(1)
    };
    let content_panel_height = if panel_collapsed {
        PRIO_PANEL_HEADER_ROWS
    } else {
        PRIO_PANEL_HEADER_ROWS.saturating_add(entry_rows)
    };
    let split_ratio = if panel_collapsed {
        f32::from(PRIO_PANEL_HEADER_ROWS) / f32::from(content.height)
    } else {
        app.sidebar_section_split.clamp(0.0, 1.0)
    };
    let split_panel_height = (f32::from(content.height) * split_ratio).floor() as u16;
    let workspace_minimum = content
        .height
        .saturating_sub(MIN_WORKSPACE_LIST_ROWS)
        .max(minimum_panel_height)
        .min(content.height);
    let panel_height = content_panel_height
        .min(split_panel_height.max(minimum_panel_height))
        .max(minimum_panel_height)
        .min(workspace_minimum);
    let list_height = content.height.saturating_sub(panel_height);

    (
        Rect::new(content.x, content.y, content.width, list_height),
        Rect::new(
            content.x,
            content.y + list_height,
            content.width,
            panel_height,
        ),
    )
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

pub(crate) fn prio_panel_entries(
    app: &AppState,
    terminal_runtimes: Option<&TerminalRuntimeRegistry>,
) -> Vec<AgentPanelEntry> {
    let entries = match terminal_runtimes {
        Some(terminal_runtimes) => sidebar_thread_entries_from(app, terminal_runtimes),
        None => sidebar_thread_entries(app),
    };
    prio_panel_entries_from_entries(entries)
}

fn prio_panel_entries_from_entries(entries: Vec<AgentPanelEntry>) -> Vec<AgentPanelEntry> {
    let tab_entries = aggregate_tab_entries(&entries);
    entries
        .into_iter()
        .filter(|entry| entry.prio && entry.tab_first_pane)
        .filter_map(|entry| tab_entries.get(&(entry.ws_idx, entry.tab_idx)).cloned())
        .collect()
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
                    let prio = ws.tabs.get(detail.tab_idx).is_some_and(|tab| tab.prio);
                    let projection = ws.tab_display_projection(&app.terminals, detail.tab_idx);
                    let tab_label_leads_with_agent = projection
                        .as_ref()
                        .is_some_and(|projection| projection.leads_with_agent_component());
                    let thread_title = ws
                        .tab_display_name_from(&app.terminals, detail.tab_idx)
                        .or_else(|| Some(DEFAULT_THREAD_TITLE.to_string()));
                    AgentPanelEntry {
                        ws_idx,
                        tab_idx: detail.tab_idx,
                        pane_id: detail.pane_id,
                        primary_label: workspace_label.clone(),
                        primary_tab_label: thread_title,
                        tab_label_leads_with_agent,
                        pane_label: detail.pane_label,
                        terminal_title: detail.terminal_title,
                        terminal_title_stripped: detail.terminal_title_stripped,
                        agent_label: Some(detail.agent_label),
                        agent_kind_label: detail.agent_kind_label,
                        agent: detail.agent,
                        foreground_process_name: detail.foreground_process_name,
                        agent_context: detail.agent_context,
                        has_agent: detail.has_agent,
                        prio,
                        state: detail.state,
                        background_job_count: detail.background_job_count,
                        seen: detail.seen,
                        stale: detail.stale,
                        reported_at: detail.reported_at,
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

pub(super) fn agent_panel_status_key_with_stale(
    state: AgentState,
    seen: bool,
    stale: bool,
) -> &'static str {
    if stale {
        "stale"
    } else {
        agent_panel_status_key(state, seen)
    }
}

fn workspace_row_height(
    _app: &AppState,
    _ws: &crate::workspace::Workspace,
    _indented: bool,
) -> u16 {
    // The final Spaces projection is deliberately one line per Space. Branch
    // and worktree identity remain available elsewhere, never as a subtitle.
    1
}

fn workspace_row_height_in_body(
    app: &AppState,
    workspace: &crate::workspace::Workspace,
    indented: bool,
    body_height: u16,
) -> u16 {
    workspace_row_height(app, workspace, indented).min(body_height)
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

fn aggregate_tab_entries(
    entries: &[AgentPanelEntry],
) -> std::collections::HashMap<(usize, usize), AgentPanelEntry> {
    let mut aggregated = std::collections::HashMap::<
        (usize, usize),
        (AgentPanelEntry, bool, bool, bool, Option<String>),
    >::new();

    for entry in entries {
        let key = (entry.ws_idx, entry.tab_idx);
        let candidate = (entry.state, entry.seen);
        aggregated
            .entry(key)
            .and_modify(
                |(
                    tab_entry,
                    mixed_agents,
                    has_agent,
                    has_current_agent,
                    first_foreground_process_name,
                )| {
                    if first_foreground_process_name.is_none() {
                        *first_foreground_process_name = entry.foreground_process_name.clone();
                    }
                    if tab_entry.foreground_process_name.is_none() {
                        tab_entry.foreground_process_name = first_foreground_process_name.clone();
                    }
                    if !tab_entry.stale
                        && (entry.stale
                            || tab_lifecycle_priority(candidate.0, candidate.1)
                                > tab_lifecycle_priority(tab_entry.state, tab_entry.seen))
                    {
                        tab_entry.state = candidate.0;
                        tab_entry.seen = candidate.1;
                        tab_entry.stale = entry.stale;
                        tab_entry.state_labels = entry.state_labels.clone();
                        tab_entry.foreground_process_name = entry
                            .foreground_process_name
                            .clone()
                            .or_else(|| first_foreground_process_name.clone());
                    }
                    tab_entry.activity_at = match (tab_entry.activity_at, entry.activity_at) {
                        (Some(current), Some(candidate)) => Some(current.max(candidate)),
                        (current, candidate) => current.or(candidate),
                    };
                    tab_entry.background_job_count =
                        match (tab_entry.background_job_count, entry.background_job_count) {
                            (Some(current), Some(candidate)) => {
                                Some(current.saturating_add(candidate))
                            }
                            (current, candidate) => current.or(candidate),
                        };
                    if !*mixed_agents {
                        match (tab_entry.agent_context, entry.agent_context) {
                            (Some(current), Some(candidate)) if current != candidate => {
                                tab_entry.agent_context = None;
                                *mixed_agents = true;
                            }
                            (None, Some(candidate)) => tab_entry.agent_context = Some(candidate),
                            _ => {}
                        }
                    }
                    *has_agent |= entry.has_agent;
                    *has_current_agent |= entry.agent.is_some();
                },
            )
            .or_insert_with(|| {
                (
                    entry.clone(),
                    false,
                    entry.has_agent,
                    entry.agent.is_some(),
                    entry.foreground_process_name.clone(),
                )
            });
    }

    aggregated
        .into_iter()
        .map(
            |(key, (mut entry, mixed_agents, has_agent, has_current_agent, _))| {
                let agent_context = (!mixed_agents).then_some(entry.agent_context).flatten();
                entry.agent = (has_current_agent && !mixed_agents)
                    .then_some(agent_context)
                    .flatten();
                entry.agent_context = agent_context;
                entry.has_agent = has_agent;
                (key, entry)
            },
        )
        .collect()
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
    /// A group label. Carries no pane, so it is deliberately absent from every
    /// card-area list: it cannot be focused or navigated onto. Clicking it
    /// collapses the group, which is why it carries the count -- a collapsed
    /// group has no rows left to count from the outside.
    SectionHeader {
        title: &'static str,
        count: usize,
        collapsed: bool,
    },
}

/// Agents waiting on a human are the only ones whose wait you can end, so they
/// are grouped above everything else rather than sorted among it. The group is
/// omitted entirely when empty, which is the common case.
pub(crate) const BLOCKED_SECTION_TITLE: &str = "Blocked";
pub(crate) const AGENTS_SECTION_TITLE: &str = "Agents";
pub(crate) const PINNED_SECTION_TITLE: &str = "Pinned";
pub(crate) const SPACES_SECTION_TITLE: &str = "Spaces";

/// Only the group that demands action is coloured. Pinned and Spaces are
/// organisation, not urgency, so they stay in the muted chrome tone.
fn section_header_color(title: &str, p: &Palette) -> ratatui::style::Color {
    if title == BLOCKED_SECTION_TITLE {
        p.red
    } else {
        p.overlay0
    }
}

/// A pin is per tab, while a sidebar row is per agent pane, so every pane of a
/// pinned tab is pinned. Out-of-range indices simply are not pinned -- a stale
/// entry must never panic the whole sidebar.
/// Groups collapse by title rather than by index: the set of groups changes
/// every time an agent blocks or unblocks, and a collapse the user asked for
/// must survive that churn.
pub(crate) fn section_is_collapsed(app: &AppState, title: &str) -> bool {
    app.collapsed_sidebar_groups.contains(title)
}

fn tab_is_pinned(app: &AppState, ws_idx: usize, tab_idx: usize) -> bool {
    app.workspaces
        .get(ws_idx)
        .and_then(|ws| ws.tabs.get(tab_idx))
        .is_some_and(|tab| tab.pinned)
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
        // Partition rather than sort: an entry belongs to exactly one group, so
        // the row count still equals the agent count. Headers appear only when
        // something is blocked -- with nothing blocked the list is byte-for-byte
        // what it was before.
        let (blocked, rest): (Vec<_>, Vec<_>) = entries
            .into_iter()
            .partition(|entry| entry.state == AgentState::Blocked);
        let agent_row = |entry: AgentPanelEntry| SidebarRow::Agent {
            entry: Box::new(entry),
            depth: 0,
        };
        if blocked.is_empty() {
            return rest.into_iter().map(agent_row).collect();
        }
        let mut rows = Vec::with_capacity(blocked.len() + rest.len() + 2);
        rows.push(SidebarRow::SectionHeader {
            title: BLOCKED_SECTION_TITLE,
            count: blocked.len(),
            collapsed: section_is_collapsed(app, BLOCKED_SECTION_TITLE),
        });
        if !section_is_collapsed(app, BLOCKED_SECTION_TITLE) {
            rows.extend(blocked.into_iter().map(agent_row));
        }
        if !rest.is_empty() {
            rows.push(SidebarRow::SectionHeader {
                title: AGENTS_SECTION_TITLE,
                count: rest.len(),
                collapsed: section_is_collapsed(app, AGENTS_SECTION_TITLE),
            });
            if !section_is_collapsed(app, AGENTS_SECTION_TITLE) {
                rows.extend(rest.into_iter().map(agent_row));
            }
        }
        return rows;
    }

    let agents = match terminal_runtimes {
        Some(runtimes) => sidebar_thread_entries_from(app, runtimes),
        None => sidebar_thread_entries(app),
    };
    // The blocked group is a worklist, not an ownership claim, so unlike the
    // priority projection it does *not* remove the agent from its workspace
    // subtree below -- the tree stays a complete picture of what runs where,
    // and the group above is the shortcut to what is waiting on you.
    let mut rows = Vec::new();
    let blocked: Vec<AgentPanelEntry> = agents
        .iter()
        .filter(|entry| entry.state == AgentState::Blocked)
        .cloned()
        .collect();
    // A pinned tab that is also blocked appears once, under Blocked: the gate
    // is the more urgent fact, and listing a row twice above the tree would
    // make the groups read as a queue you have to de-duplicate by eye.
    let pinned: Vec<AgentPanelEntry> = agents
        .iter()
        .filter(|entry| {
            entry.state != AgentState::Blocked && tab_is_pinned(app, entry.ws_idx, entry.tab_idx)
        })
        .cloned()
        .collect();
    let push_group =
        |rows: &mut Vec<SidebarRow>, title: &'static str, group: Vec<AgentPanelEntry>| {
            if group.is_empty() {
                return;
            }
            let collapsed = section_is_collapsed(app, title);
            rows.push(SidebarRow::SectionHeader {
                title,
                count: group.len(),
                collapsed,
            });
            if collapsed {
                return;
            }
            rows.extend(group.into_iter().map(|entry| SidebarRow::Agent {
                entry: Box::new(entry),
                depth: 0,
            }));
        };
    push_group(&mut rows, BLOCKED_SECTION_TITLE, blocked);
    push_group(&mut rows, PINNED_SECTION_TITLE, pinned);

    let mut agents_by_workspace = std::collections::HashMap::<usize, Vec<AgentPanelEntry>>::new();
    for entry in agents {
        agents_by_workspace
            .entry(entry.ws_idx)
            .or_default()
            .push(entry);
    }

    let workspaces = if expand_worktrees {
        workspace_list_entries_expanded(app)
    } else {
        workspace_list_entries(app)
    };
    // The tree gets a header -- and with it the handle that folds it away --
    // only once some group sits above it. Alone in the sidebar it needs no
    // label, and folding it would leave nothing behind to fold back.
    let spaces_collapsed = !rows.is_empty() && section_is_collapsed(app, SPACES_SECTION_TITLE);
    if !rows.is_empty() {
        rows.push(SidebarRow::SectionHeader {
            title: SPACES_SECTION_TITLE,
            count: workspaces
                .iter()
                .filter(|WorkspaceListEntry::Workspace { indented, .. }| !indented)
                .count(),
            collapsed: spaces_collapsed,
        });
    }
    if spaces_collapsed {
        return rows;
    }
    for workspace in workspaces {
        let WorkspaceListEntry::Workspace { ws_idx, indented } = workspace;
        if indented {
            continue;
        }
        rows.push(SidebarRow::Workspace { ws_idx, indented });
        if app.workspace_agents_expanded(ws_idx) {
            let depth = 1;
            for member_idx in sidebar_space_member_indices(app, ws_idx) {
                let Some(entries) = agents_by_workspace.remove(&member_idx) else {
                    continue;
                };
                let tab_entries = aggregate_tab_entries(&entries);
                let mut current_tab = None;
                for entry in entries {
                    let tab = (entry.ws_idx, entry.tab_idx);
                    if current_tab != Some(tab) {
                        if let Some(tab_entry) = tab_entries.get(&tab).cloned() {
                            rows.push(SidebarRow::Tab {
                                entry: Box::new(tab_entry),
                                depth,
                            });
                        }
                        current_tab = Some(tab);
                    }
                }
            }
        }
    }
    rows
}

pub(super) fn sidebar_space_member_indices(app: &AppState, root_idx: usize) -> Vec<usize> {
    let Some((key, _)) = workspace_parent_group_state(app, root_idx) else {
        return vec![root_idx];
    };
    app.workspaces
        .iter()
        .enumerate()
        .filter_map(|(idx, workspace)| {
            workspace
                .worktree_space()
                .is_some_and(|space| space.key == key)
                .then_some(idx)
        })
        .collect()
}

pub(crate) fn normalized_workspace_scroll(app: &AppState, area: Rect, requested: usize) -> usize {
    let ws_area = workspace_list_rect_for_app(app, area);
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

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn workspace_list_rect(area: Rect, split_ratio: f32) -> Rect {
    let (ws_area, _) = expanded_sidebar_sections(area, split_ratio);
    ws_area
}

pub(crate) fn workspace_list_rect_for_app(app: &AppState, area: Rect) -> Rect {
    expanded_sidebar_sections_for_app(app, area).0
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
        SidebarRow::Agent { entry, depth } => {
            agent_entry_height_in_body_at(app, entry, body_height, *depth)
        }
        SidebarRow::Tab { .. } | SidebarRow::SectionHeader { .. } => 1,
    }
}

/// For each row index, the 1-based number to show if it is a top-level agent.
/// Non-agent rows get 0; they are never rendered with a number.
fn agent_row_ordinals(rows: &[SidebarRow]) -> Vec<usize> {
    let mut next = 0usize;
    rows.iter()
        .map(|row| match row {
            SidebarRow::Agent { .. } => {
                next += 1;
                next
            }
            _ => 0,
        })
        .collect()
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
        (_, SidebarRow::Workspace { indented: true, .. }) => 0,
        (SidebarRow::Workspace { .. }, SidebarRow::Workspace { .. }) => app.sidebar_spaces.row_gap,
        (SidebarRow::Agent { .. }, SidebarRow::Agent { .. }) => app.sidebar_agents.row_gap,
        (SidebarRow::Agent { .. }, SidebarRow::Workspace { .. }) => app.sidebar_spaces.row_gap,
        (SidebarRow::Tab { .. }, SidebarRow::Workspace { .. }) => app.sidebar_spaces.row_gap,
        (SidebarRow::Agent { .. }, SidebarRow::Tab { .. }) => 0,
        (SidebarRow::Tab { .. }, SidebarRow::Tab { .. }) => 0,
        // A header hugs the group it names, and earns the agent gap above it so
        // the two groups read as separate lists rather than one long one.
        (SidebarRow::SectionHeader { .. }, _) => 0,
        (_, SidebarRow::SectionHeader { .. }) => app.sidebar_agents.row_gap,
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
        // Headers belong to a state, not a workspace, so scrolling to a
        // workspace must never land on one.
        SidebarRow::SectionHeader { .. } => false,
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
    let ws_area = workspace_list_rect_for_app(app, area);
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

/// Depth 0 means the row sits in a worklist group rather than in the tree, and
/// those two want different shapes: the tree keeps the configured template, the
/// worklist collapses to one line.
fn resolved_agent_rows_at(
    app: &AppState,
    entry: &AgentPanelEntry,
    depth: u16,
) -> Vec<Vec<ResolvedToken>> {
    if depth == 0 {
        return tokens::worklist_row(entry);
    }
    resolved_agent_rows(app, entry)
}

fn resolved_agent_rows(app: &AppState, entry: &AgentPanelEntry) -> Vec<Vec<ResolvedToken>> {
    let label = entry
        .state_labels
        .get(agent_panel_status_key_with_stale(
            entry.state,
            entry.seen,
            entry.stale,
        ))
        .map(String::as_str)
        .unwrap_or_else(|| match entry.state {
            _ if entry.stale => "stale",
            AgentState::Idle => "done",
            _ => state_label(entry.state, entry.seen),
        });
    tokens::agent_rows(&app.sidebar_agents, entry, label)
}

#[cfg(test)]
pub(crate) fn agent_entry_height_in_body(
    app: &AppState,
    entry: &AgentPanelEntry,
    body_height: u16,
) -> u16 {
    agent_entry_height_in_body_at(app, entry, body_height, 1)
}

/// A worklist row is one line and owns no tab header -- it is a pointer into
/// the tree, not a copy of the tree's structure.
fn agent_entry_height_in_body_at(
    app: &AppState,
    entry: &AgentPanelEntry,
    body_height: u16,
    depth: u16,
) -> u16 {
    (resolved_agent_rows_at(app, entry, depth)
        .len()
        .max(1)
        .saturating_add(usize::from(entry.tab_first_pane && depth > 0))
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
    let ws_area = workspace_list_rect_for_app(app, area);
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
            SidebarRow::Agent { entry, depth } => {
                let row_height = agent_entry_height_in_body_at(app, entry, body.height, *depth);
                if row_y.saturating_add(row_height) > body_bottom {
                    break;
                }
                agent_cards.push(crate::app::state::AgentCardArea {
                    ws_idx: entry.ws_idx,
                    tab_idx: entry.tab_idx,
                    pane_id: entry.pane_id,
                    rect: Rect::new(body.x, row_y, body.width, row_height),
                    row_idx: entry_idx,
                });
            }
            SidebarRow::Tab { .. } | SidebarRow::SectionHeader { .. } => {}
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
    let ws_area = workspace_list_rect_for_app(app, area);
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
        if let SidebarRow::Tab { entry, depth } = row {
            out.push(crate::app::state::TabCardArea {
                ws_idx: entry.ws_idx,
                tab_idx: entry.tab_idx,
                pane_id: entry.pane_id,
                depth: *depth,
                rect: Rect::new(body.x, y, body.width, height),
            });
        }
        y = y
            .saturating_add(height)
            .saturating_add(sidebar_row_gap(app, &rows, idx));
    }
    out
}

/// Where a group header landed this frame. Headers are not cards -- they own no
/// pane -- so they get their own list rather than a seat in the card areas that
/// focus and navigation walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SectionHeaderArea {
    pub title: &'static str,
    pub rect: Rect,
}

pub(crate) fn compute_sidebar_section_header_areas(
    app: &AppState,
    area: Rect,
) -> Vec<SectionHeaderArea> {
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
        if let SidebarRow::SectionHeader { title, .. } = row {
            out.push(SectionHeaderArea {
                title,
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
    _app: &AppState,
    card: &crate::app::state::WorkspaceCardArea,
    _has_agents: bool,
) -> Rect {
    if card.rect.width < 2 || card.rect.height == 0 {
        return Rect::default();
    }
    Rect::new(card.rect.x.saturating_add(1), card.rect.y, 1, 1)
}

/// A gutter cell is `TAB_*_FIELD_WIDTH` columns wide — the marker plus its trailing spacer — and the
/// whole cell is the hitbox. Returning only the marker column would leave the spacer falling through
/// to the row's focus target, so half of every visible cell would do the wrong thing.
fn tab_gutter_rect(card: &crate::app::state::TabCardArea, offset: u16, width: u16) -> Rect {
    let prefix_width = card.depth.saturating_mul(3).saturating_add(1);
    let x = card
        .rect
        .x
        .saturating_add(prefix_width)
        .saturating_add(offset);
    let card_end = card.rect.x.saturating_add(card.rect.width);
    if card.rect.height == 0 || x < card.rect.x || x >= card_end {
        return Rect::default();
    }
    Rect::new(x, card.rect.y, width.min(card_end.saturating_sub(x)), 1)
}

pub(crate) fn tab_prio_rect(card: &crate::app::state::TabCardArea) -> Rect {
    tab_gutter_rect(card, 0, TAB_PRIO_FIELD_WIDTH as u16)
}

pub(crate) fn prio_panel_chevron_rect(panel: Rect) -> Rect {
    if panel.width < 2 || panel.height == 0 {
        return Rect::default();
    }
    Rect::new(panel.x.saturating_add(1), panel.y, 1, 1)
}

fn prio_panel_body(panel: Rect) -> Rect {
    Rect::new(
        panel.x,
        panel.y.saturating_add(PRIO_PANEL_HEADER_ROWS),
        panel.width,
        panel.height.saturating_sub(PRIO_PANEL_HEADER_ROWS),
    )
}

fn prio_panel_entry_row_count(entry_count: usize, body_height: u16) -> usize {
    let available_rows = usize::from(body_height);
    if entry_count > available_rows {
        available_rows.saturating_sub(1)
    } else {
        entry_count
    }
}

pub(crate) fn compute_prio_panel_row_areas(
    app: &AppState,
    area: Rect,
) -> Vec<crate::app::state::PrioPanelRowArea> {
    let (_, panel) = expanded_sidebar_sections_for_app(app, area);
    let body = prio_panel_body(panel);
    if body.width == 0 || body.height == 0 {
        return Vec::new();
    }

    let entries = prio_panel_entries(app, None);
    let row_count = prio_panel_entry_row_count(entries.len(), body.height);
    entries
        .into_iter()
        .take(row_count)
        .enumerate()
        .map(|(row, entry)| crate::app::state::PrioPanelRowArea {
            ws_idx: entry.ws_idx,
            tab_idx: entry.tab_idx,
            rect: Rect::new(body.x, body.y + row as u16, body.width, 1),
        })
        .collect()
}

pub(crate) fn prio_panel_rect(app: &AppState, area: Rect) -> Rect {
    expanded_sidebar_sections_for_app(app, area).1
}

#[cfg(test)]
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
    let Some(sep_x) = sidebar_separator_col(area) else {
        return;
    };
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
    let rows = sidebar_rows(app);
    // Agents are numbered by their position among agents, not among rows: a
    // section header must not consume a number the user could type.
    let agent_ordinals = agent_row_ordinals(&rows);
    for (row_idx, row) in rows.iter().enumerate().skip(scroll) {
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
                let stale = ws.tabs.iter().any(|tab| {
                    tab.panes.values().any(|pane| {
                        app.terminals
                            .get(&pane.attached_terminal_id)
                            .is_some_and(|terminal| terminal.supervisor_stale)
                    })
                });
                let (icon, icon_style) =
                    state_icon_with_stale(agg_state, agg_seen, stale, app.status_indicators, p);
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
                let (icon, icon_style) = state_icon_with_stale(
                    entry.state,
                    entry.seen,
                    entry.stale,
                    app.status_indicators,
                    p,
                );
                let row_style = Style::default();
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(
                            if *depth == 0 {
                                format!("{}", agent_ordinals[row_idx])
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
                let (icon, icon_style) = state_icon_with_stale(
                    entry.state,
                    entry.seen,
                    entry.stale,
                    app.status_indicators,
                    p,
                );
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(icon, icon_style),
                    ])),
                    Rect::new(ws_area.x, y, ws_area.width, 1),
                );
            }
            SidebarRow::SectionHeader { title, .. } => {
                // Collapsed leaves no room for a word, so the header degrades to
                // a rule: the grouping is still legible, the labels are not.
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        "─".repeat(usize::from(ws_area.width)),
                        Style::default().fg(section_header_color(title, p)),
                    ))),
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

    let Some(sep_x) = sidebar_separator_col(area) else {
        return;
    };
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(sep_x, y)].set_symbol("│");
        buf[(sep_x, y)].set_style(sep_style);
    }

    let (ws_area, panel_area) = expanded_sidebar_sections_for_app(app, area);
    render_workspace_list(app, terminal_runtimes, frame, ws_area, is_navigating);
    render_prio_panel(app, terminal_runtimes, frame, panel_area, area);
    render_sidebar_header(app, frame, area, p);
}

fn render_prio_panel(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    panel: Rect,
    sidebar_area: Rect,
) {
    if panel.width == 0 || panel.height == 0 {
        return;
    }

    let p = &app.palette;
    let entries = prio_panel_entries(app, Some(terminal_runtimes));
    let expanded = !app.prio_panel_collapsed && !entries.is_empty();
    let chevron = if expanded { "▾" } else { "▸" };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(chevron, Style::default().fg(p.accent)),
            Span::raw(" "),
            Span::styled(
                "PRIO",
                Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
            ),
        ])),
        Rect::new(panel.x, panel.y, panel.width, PRIO_PANEL_HEADER_ROWS),
    );

    if !expanded {
        return;
    }

    let body = prio_panel_body(panel);
    let rows = compute_prio_panel_row_areas(app, sidebar_area);
    for row in &rows {
        let Some(entry) = entries
            .iter()
            .find(|entry| entry.ws_idx == row.ws_idx && entry.tab_idx == row.tab_idx)
        else {
            continue;
        };
        render_prio_panel_row(app, frame, entry, row.rect);
    }
    if rows.len() < entries.len() && body.width > 0 && body.height > 0 {
        let indicator = Rect::new(
            body.x,
            body.y.saturating_add(rows.len() as u16),
            body.width,
            1,
        );
        frame.render_widget(
            Paragraph::new(format!("  +{} more", entries.len() - rows.len()))
                .style(Style::default().fg(p.overlay0)),
            indicator,
        );
    }
}

fn render_prio_panel_row(app: &AppState, frame: &mut Frame, entry: &AgentPanelEntry, rect: Rect) {
    let p = &app.palette;
    let width = usize::from(rect.width);
    let prefix = "  ";
    let state_icon = state_icon_with_stale(
        entry.state,
        entry.seen,
        entry.stale,
        app.status_indicators,
        p,
    );
    let workspace = entry.primary_label.as_str();
    // A derived or manually named tab can already carry the agent identity, so
    // appending the provider here would repeat it in the same row.
    let tab_title = entry
        .primary_tab_label
        .as_deref()
        .unwrap_or(DEFAULT_THREAD_TITLE);
    let agent = (!entry.tab_label_leads_with_agent
        && !title_repeats_agent_identity(entry, tab_title))
    .then(|| {
        entry
            .agent_kind_label
            .as_deref()
            .or(entry.agent_label.as_deref())
            .or_else(|| tab_agent_suffix(entry.agent))
    })
    .flatten()
    .map(|label| format!(" · {label}"));

    let mut spans = Vec::new();
    let mut used = 0usize;
    push_prio_panel_span(&mut spans, &mut used, width, prefix, Style::default());
    push_prio_panel_span(
        &mut spans,
        &mut used,
        width,
        "●",
        Style::default().fg(p.peach),
    );
    push_prio_panel_span(&mut spans, &mut used, width, " ", Style::default());
    push_prio_panel_span(&mut spans, &mut used, width, state_icon.0, state_icon.1);
    push_prio_panel_span(&mut spans, &mut used, width, " ", Style::default());

    let remaining = width.saturating_sub(used);
    let agent_width = agent.as_deref().map(display_width).unwrap_or_default();
    let include_agent = agent.as_ref().is_some_and(|_| {
        remaining >= 1 + display_width(" · ") + display_width(workspace) + agent_width
    });
    let agent_width = if include_agent { agent_width } else { 0 };
    if remaining > display_width(" · ") {
        let workspace_width = display_width(workspace)
            .min(remaining.saturating_sub(display_width(" · ") + agent_width + 1));
        let title_width =
            remaining.saturating_sub(display_width(" · ") + workspace_width + agent_width);
        let title = truncate_end(
            entry
                .primary_tab_label
                .as_deref()
                .unwrap_or(DEFAULT_THREAD_TITLE),
            title_width,
        );
        push_prio_panel_span(
            &mut spans,
            &mut used,
            width,
            &title,
            Style::default().fg(p.subtext0),
        );
        push_prio_panel_span(
            &mut spans,
            &mut used,
            width,
            " · ",
            Style::default().fg(p.overlay0),
        );
        push_prio_panel_span(
            &mut spans,
            &mut used,
            width,
            &truncate_end(workspace, workspace_width),
            Style::default().fg(p.overlay1),
        );
        if include_agent {
            if let Some(agent) = agent {
                push_prio_panel_span(
                    &mut spans,
                    &mut used,
                    width,
                    &agent,
                    Style::default().fg(p.overlay0),
                );
            }
        }
    } else {
        let title = truncate_end(
            entry
                .primary_tab_label
                .as_deref()
                .unwrap_or(DEFAULT_THREAD_TITLE),
            remaining,
        );
        push_prio_panel_span(
            &mut spans,
            &mut used,
            width,
            &title,
            Style::default().fg(p.subtext0),
        );
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
}

fn push_prio_panel_span(
    spans: &mut Vec<Span<'static>>,
    used: &mut usize,
    width: usize,
    text: &str,
    style: Style,
) {
    let clipped = truncate_end(text, width.saturating_sub(*used));
    let clipped_width = display_width(&clipped);
    if clipped_width == 0 {
        return;
    }
    spans.push(Span::styled(clipped, style));
    *used += clipped_width;
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

/// Painted like a space row on purpose -- same chevron in the same column --
/// because it collapses the same way, and the eye should not have to learn two
/// disclosure affordances in one list.
fn render_section_header(
    app: &AppState,
    frame: &mut Frame,
    header: &SectionHeaderArea,
    count: usize,
    collapsed: bool,
) {
    if header.rect.width == 0 || header.rect.height == 0 {
        return;
    }
    let p = &app.palette;
    let color = section_header_color(header.title, p);
    let count_label = format!(" ({count})");
    let title = truncate_end(
        header.title,
        usize::from(header.rect.width)
            .saturating_sub(display_width(" ▾ ") + display_width(&count_label)),
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                if collapsed { "▸" } else { "▾" },
                Style::default().fg(color),
            ),
            Span::raw(" "),
            Span::styled(
                title,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                count_label,
                Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
            ),
        ])),
        Rect::new(header.rect.x, header.rect.y, header.rect.width, 1),
    );
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
    for card in cards {
        let i = card.ws_idx;
        let ws = &app.workspaces[i];
        let row_y = card.rect.y;
        let row_height = card.rect.height;
        let member_indices = sidebar_space_member_indices(app, i);
        let selected = is_navigating && member_indices.contains(&app.selected);
        let is_active = app
            .active
            .is_some_and(|active| member_indices.contains(&active));
        let is_dragged = dragged_ws_idx == Some(i);

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
            Style::default()
                .fg(active_sidebar_title_color(p))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };

        let display_label = workspace_parent_group_state(app, i)
            .and_then(|_| ws.worktree_space().map(|space| space.label.clone()))
            .unwrap_or_else(|| ws.display_name_from(&app.terminals, terminal_runtimes));
        let window_count = member_indices
            .iter()
            .filter_map(|member| app.workspaces.get(*member))
            .map(|workspace| workspace.tabs.len())
            .sum::<usize>();
        let count_label = format!(" ({window_count})");
        let fixed_width = display_width(" ▾ ") + display_width(&count_label);
        let title = truncate_end(
            &display_label,
            usize::from(card.rect.width).saturating_sub(fixed_width),
        );
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    if app.workspace_agents_expanded(i) {
                        "▾"
                    } else {
                        "▸"
                    },
                    Style::default().fg(p.accent),
                ),
                Span::raw(" "),
                Span::styled(title, name_style),
                Span::styled(
                    count_label,
                    Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
                ),
            ])),
            Rect::new(card.rect.x, row_y, card.rect.width, 1),
        );
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
    for header in compute_sidebar_section_header_areas(app, sidebar_area) {
        let Some((count, collapsed)) = row_entries.iter().find_map(|row| match row {
            SidebarRow::SectionHeader {
                title,
                count,
                collapsed,
            } if *title == header.title => Some((*count, *collapsed)),
            _ => None,
        }) else {
            continue;
        };
        render_section_header(app, frame, &header, count, collapsed);
    }
    for card in compute_tab_card_areas(app, sidebar_area) {
        render_tab_card(app, frame, &card);
    }
    for card in agent_cards {
        let Some((entry, depth)) = row_entries.get(card.row_idx).and_then(|row| match row {
            SidebarRow::Agent { entry, depth } => Some((entry, *depth)),
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
    let style = if active {
        Style::default()
            .fg(active_sidebar_title_color(p))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.subtext0)
    };
    let prefix = " ".repeat(usize::from(depth) * 3 + 1);
    let layout = tab_row_layout(
        &entry,
        app.view_observed_at,
        usize::from(card.rect.width),
        display_width(&prefix),
        p,
        app.status_indicators,
    );
    let mut spans = vec![Span::raw(prefix)];
    let prio_marker = if entry.prio { "●" } else { "·" };
    let prio_style = if entry.prio {
        Style::default().fg(p.peach)
    } else {
        Style::default().fg(p.overlay0).add_modifier(Modifier::DIM)
    };
    spans.extend([Span::styled(prio_marker, prio_style), Span::raw(" ")]);
    if let Some(state) = layout.state.as_deref() {
        let state_icon = state_icon_with_stale(
            entry.state,
            entry.seen,
            entry.stale,
            app.status_indicators,
            p,
        );
        spans.push(Span::styled(state_icon.0.to_string(), state_icon.1));
        if layout.show_state_label {
            spans.extend([
                Span::styled(
                    format!(" {state}"),
                    Style::default().fg(state_label_color_with_stale(
                        entry.state,
                        entry.seen,
                        entry.stale,
                        p,
                    )),
                ),
                Span::styled(" · ", Style::default().fg(p.overlay0)),
            ]);
        } else {
            spans.push(Span::raw(" "));
        }
    }
    spans.push(Span::styled(layout.title, style));
    if let Some(agent_suffix) = layout.agent_suffix {
        spans.push(Span::styled(agent_suffix, Style::default().fg(p.overlay1)));
    }
    if let Some(foreground_process) = layout.foreground_process {
        spans.push(Span::styled(
            foreground_process,
            Style::default().fg(p.overlay0),
        ));
    }
    if let Some(background_jobs) = layout.background_jobs {
        spans.push(Span::styled(
            background_jobs,
            Style::default().fg(p.overlay0),
        ));
    }
    if let Some(activity_age) = layout.activity_age {
        let used_width = spans
            .iter()
            .map(|span| display_width(span.content.as_ref()))
            .sum::<usize>();
        let padding = usize::from(card.rect.width)
            .saturating_sub(used_width)
            .saturating_sub(display_width(&activity_age));
        spans.push(Span::raw(" ".repeat(padding)));
        let activity_style = if entry.state == AgentState::Working && !entry.stale {
            Style::default().fg(p.blue)
        } else {
            Style::default().fg(p.green).add_modifier(Modifier::DIM)
        };
        spans.push(Span::styled(activity_age, activity_style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), card.rect);
}

fn render_agent_card(
    app: &AppState,
    frame: &mut Frame,
    detail: &AgentPanelEntry,
    rect: Rect,
    depth: u16,
) {
    let p = &app.palette;
    let label_color = state_label_color_with_stale(detail.state, detail.seen, detail.stale, p);
    let rows = resolved_agent_rows_at(app, detail, depth);
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
    let state_icon = state_icon_with_stale(
        detail.state,
        detail.seen,
        detail.stale,
        app.status_indicators,
        p,
    );
    // Grouped rows are pointers under a section header, so they share the
    // child indentation of the tree even though their semantic depth is zero.
    let indent_depth = depth.max(1);
    let indent = usize::from(indent_depth) * 3;

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
            let label = status_report_age_compact_label(detail.reported_at, app.view_observed_at)
                .unwrap_or_else(|| {
                    crate::activity_age::compact_label(detail.activity_at, app.view_observed_at)
                });
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
            let activity_style = if detail.state == AgentState::Working && !detail.stale {
                Style::default().fg(p.blue)
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
    // Measured against the row this depth actually paints: a worklist row is the
    // one-line shape, and measuring the tree template instead would size the age
    // field against text that never appears.
    let Some(resolved) = resolved_agent_rows_at(app, detail, depth)
        .into_iter()
        .next()
    else {
        return false;
    };
    let prefix_width = usize::from(depth.max(1)) * 3 + usize::from(depth > 0);
    let content_width = usize::from(rect.width).saturating_sub(prefix_width);
    if content_width < AGENT_ACTIVITY_AGE_MIN_CONTENT_WIDTH + AGENT_ACTIVITY_AGE_FIELD_WIDTH {
        return false;
    }
    let p = &app.palette;
    let label_color = state_label_color_with_stale(detail.state, detail.seen, detail.stale, p);
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
    let state_icon = state_icon_with_stale(
        detail.state,
        detail.seen,
        detail.stale,
        app.status_indicators,
        p,
    );
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

pub(crate) fn visible_tab_activity_instants_from(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    cards: &[crate::app::state::TabCardArea],
) -> Vec<std::time::Instant> {
    let rows = sidebar_rows_from(app, terminal_runtimes);
    cards
        .iter()
        .filter_map(|card| {
            let (entry, depth) = rows.iter().find_map(|row| match row {
                SidebarRow::Tab { entry, depth }
                    if entry.ws_idx == card.ws_idx && entry.tab_idx == card.tab_idx =>
                {
                    Some((entry.as_ref(), *depth))
                }
                _ => None,
            })?;
            let layout = tab_row_layout(
                entry,
                app.view_observed_at,
                usize::from(card.rect.width),
                usize::from(depth) * 3 + 1,
                &app.palette,
                app.status_indicators,
            );
            layout.activity_age.and(layout.activity_instant)
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
    use ratatui::{backend::TestBackend, layout::Direction, style::Color, Terminal};

    #[test]
    fn collapsed_and_expanded_sidebars_render_separator_on_shared_column() {
        let app = AppState::test_new();
        let area = Rect::new(3, 1, 26, 8);
        let separator_col = sidebar_separator_col(area).expect("non-empty sidebar");

        for collapsed in [true, false] {
            let mut terminal = Terminal::new(TestBackend::new(32, 10)).unwrap();
            terminal
                .draw(|frame| {
                    if collapsed {
                        render_sidebar_collapsed(&app, frame, area);
                    } else {
                        render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area);
                    }
                })
                .unwrap();

            let buffer = terminal.backend().buffer();
            for row in area.y..area.y + area.height {
                assert_eq!(buffer[(separator_col, row)].symbol(), "│");
            }
        }
    }

    #[test]
    fn active_title_color_darkens_rgb_themes_and_preserves_terminal_fallbacks() {
        // ac3: selected titles are one-third darker than the authored text
        // token in both shipped One themes. Bold supplies the emphasis while
        // the foreground direction remains consistent across machines.
        let one_light = crate::app::state::Palette::one_light();
        assert_eq!(
            active_sidebar_title_color(&one_light),
            Color::Rgb(37, 38, 44)
        );

        let one_dark = crate::app::state::Palette::one_dark();
        assert_eq!(
            active_sidebar_title_color(&one_dark),
            Color::Rgb(114, 118, 127)
        );

        let terminal = crate::app::state::Palette::terminal();
        assert_eq!(active_sidebar_title_color(&terminal), terminal.text);

        let mut custom_reset = crate::app::state::Palette::one_light();
        custom_reset.panel_bg = Color::Reset;
        custom_reset.text = Color::Rgb(12, 34, 56);
        assert_eq!(active_sidebar_title_color(&custom_reset), custom_reset.text);
    }

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

    fn add_work_link(app: &mut AppState, ws_idx: usize) {
        let pane_id = app.workspaces[ws_idx].tabs[0].root_pane;
        let terminal_id = app.workspaces[ws_idx].tabs[0]
            .terminal_id(pane_id)
            .expect("test pane terminal")
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .expect("test terminal")
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                ticket_ids: Some(vec!["MAT-1".into()]),
                ..Default::default()
            })
            .expect("valid test work context");
    }

    fn aggregation_entry(
        state: AgentState,
        seen: bool,
        foreground_process_name: Option<&str>,
        state_label: &str,
    ) -> AgentPanelEntry {
        let mut state_labels = std::collections::HashMap::new();
        state_labels.insert(
            agent_panel_status_key(state, seen).to_string(),
            state_label.to_string(),
        );
        AgentPanelEntry {
            ws_idx: 0,
            tab_idx: 0,
            pane_id: crate::layout::PaneId::alloc(),
            primary_label: "workspace".into(),
            primary_tab_label: Some("tab".into()),
            tab_label_leads_with_agent: false,
            pane_label: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_label: None,
            agent_kind_label: None,
            agent: None,
            foreground_process_name: foreground_process_name.map(str::to_string),
            agent_context: None,
            has_agent: true,
            prio: true,
            state,
            background_job_count: None,
            seen,
            stale: false,
            reported_at: None,
            last_agent_state_change_seq: None,
            activity_at: None,
            state_labels,
            tokens: std::collections::HashMap::new(),
            tab_first_pane: false,
        }
    }

    fn row_kinds(app: &AppState) -> Vec<(char, usize)> {
        sidebar_rows(app)
            .into_iter()
            .map(|row| match row {
                SidebarRow::Workspace { ws_idx, .. } => ('w', ws_idx),
                SidebarRow::Tab { entry, .. } => ('t', entry.ws_idx),
                SidebarRow::Agent { entry, .. } => ('a', entry.ws_idx),
                SidebarRow::SectionHeader { .. } => ('h', 0),
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
        assert!(text.iter().any(|line| line.contains("· pi")), "{text:?}");
    }

    #[test]
    fn flagged_tab_renders_peach_prio_dot_before_state() {
        let mut app = app_with_agents(&["one", "two"]);
        app.workspaces[0].tabs[0].set_prio(true);
        let area = Rect::new(0, 0, 40, 20);
        let card = compute_tab_card_areas(&app, area)[0].clone();
        let mut terminal = Terminal::new(TestBackend::new(40, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let prio_cell = buffer
            .cell((card.rect.x + 4, card.rect.y))
            .expect("flagged tab should have a PRIO cell");
        assert_eq!(prio_cell.symbol(), "●");
        assert_eq!(prio_cell.fg, app.palette.peach);
        assert!(row_text(buffer, card.rect.y, 39).contains("●"));
    }

    #[test]
    fn prio_panel_renders_flagged_tab_in_bottom_rows_with_workspace_context() {
        let mut app = app_with_agents(&["one", "two"]);
        app.workspaces[1].tabs[0].custom_name = Some("review auth".into());
        app.workspaces[1].tabs[0].set_prio(true);
        let area = Rect::new(0, 0, 40, 16);
        let panel = prio_panel_rect(&app, area);
        let rows = compute_prio_panel_row_areas(&app, area);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].rect.y > panel.y);
        assert!(rows[0].rect.y < panel.y + panel.height);

        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let rendered = row_text(terminal.backend().buffer(), rows[0].rect.y, area.width - 1);
        assert!(rendered.contains("review auth"), "{rendered:?}");
        assert!(rendered.contains("two"), "{rendered:?}");
        assert!(rendered.contains("●"), "{rendered:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn prio_panel_uses_runtime_workspace_label_like_sidebar() {
        let mut app = app_with_agents(&["stale"]);
        let stale_cwd = std::env::temp_dir().join(format!(
            "herdr-prio-stale-{}-{}",
            std::process::id(),
            std::time::Instant::now().elapsed().as_nanos()
        ));
        let live_cwd = std::env::temp_dir().join(format!(
            "herdr-prio-live-{}-{}",
            std::process::id(),
            std::time::Instant::now().elapsed().as_nanos()
        ));
        std::fs::create_dir_all(&stale_cwd).expect("create stale cwd");
        std::fs::create_dir_all(&live_cwd).expect("create live cwd");

        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0]
            .terminal_id(pane_id)
            .expect("root terminal")
            .clone();
        app.workspaces[0].custom_name = None;
        app.workspaces[0].identity_cwd = stale_cwd.clone();
        app.workspaces[0].cached_identity_cwd = stale_cwd.clone();
        app.workspaces[0].cached_auto_label = "stale".into();
        app.terminals.get_mut(&terminal_id).unwrap().cwd = stale_cwd.clone();
        app.workspaces[0].tabs[0].set_prio(true);

        let (events, _) = tokio::sync::mpsc::channel(4);
        let runtime = crate::terminal::TerminalRuntime::spawn(
            pane_id,
            24,
            80,
            live_cwd.clone(),
            0,
            crate::terminal_theme::TerminalTheme::default(),
            None,
            crate::pane::PaneShellConfig::new("/bin/sh", crate::config::ShellModeConfig::NonLogin),
            &crate::pane::PaneLaunchEnv::default(),
            events,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(crate::render_signal::RenderSignal::new()),
        )
        .expect("spawn test runtime");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while runtime.cwd() != Some(live_cwd.clone()) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let runtime_cwd = runtime.cwd().expect("runtime cwd");
        assert_eq!(
            runtime_cwd.file_name(),
            live_cwd.file_name(),
            "runtime should retain the live cwd fixture"
        );

        let mut runtimes = TerminalRuntimeRegistry::new();
        runtimes.insert(terminal_id, runtime);
        let sidebar_entry = sidebar_thread_entries_from(&app, &runtimes)
            .into_iter()
            .next()
            .expect("sidebar entry");
        let prio_entry = prio_panel_entries(&app, Some(&runtimes))
            .into_iter()
            .next()
            .expect("PRIO entry");

        assert_eq!(prio_entry.primary_label, sidebar_entry.primary_label);
        assert_eq!(
            prio_entry.primary_label,
            runtime_cwd
                .file_name()
                .expect("live cwd filename")
                .to_string_lossy()
        );
        assert_ne!(
            prio_entry.primary_label,
            sidebar_thread_entries(&app)[0].primary_label
        );

        for (_, runtime) in runtimes.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_dir_all(stale_cwd);
        let _ = std::fs::remove_dir_all(live_cwd);
    }

    #[test]
    fn tab_aggregation_uses_winner_labels_and_process_fallback() {
        let winner = aggregation_entry(AgentState::Working, false, Some("cargo"), "winner working");
        let first = aggregation_entry(AgentState::Idle, true, Some("zsh"), "first idle");
        let aggregated = aggregate_tab_entries(&[first, winner])
            .remove(&(0, 0))
            .expect("aggregated tab entry");

        assert_eq!(aggregated.state, AgentState::Working);
        assert_eq!(
            aggregated.state_labels.get("working").map(String::as_str),
            Some("winner working")
        );
        assert_eq!(aggregated.foreground_process_name.as_deref(), Some("cargo"));

        let winner_without_process =
            aggregation_entry(AgentState::Working, false, None, "winner working");
        let first_with_process =
            aggregation_entry(AgentState::Idle, true, Some("zsh"), "first idle");
        let fallback = aggregate_tab_entries(&[first_with_process, winner_without_process])
            .remove(&(0, 0))
            .expect("aggregated tab entry");
        assert_eq!(fallback.foreground_process_name.as_deref(), Some("zsh"));
    }

    #[test]
    fn prio_panel_height_matches_content_until_split_cap() {
        let area = Rect::new(0, 0, 40, 16);

        let mut one_entry = app_with_agents(&["one"]);
        one_entry.workspaces[0].tabs[0].set_prio(true);
        assert_eq!(prio_panel_rect(&one_entry, area).height, 2);

        let mut two_entries = app_with_agents(&["one", "two"]);
        for workspace in &mut two_entries.workspaces {
            workspace.tabs[0].set_prio(true);
        }
        assert_eq!(prio_panel_rect(&two_entries, area).height, 3);

        let mut many_entries = app_with_agents(&[
            "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
        ]);
        for workspace in &mut many_entries.workspaces {
            workspace.tabs[0].set_prio(true);
        }
        assert_eq!(prio_panel_rect(&many_entries, area).height, 8);
    }

    #[test]
    fn overflowing_prio_panel_shows_indicator_and_matches_hit_test_rows() {
        let mut app = app_with_agents(&[
            "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
        ]);
        for workspace in &mut app.workspaces {
            workspace.tabs[0].set_prio(true);
        }
        let area = Rect::new(0, 0, 40, 16);
        let entries = prio_panel_entries(&app, None);
        let rows = compute_prio_panel_row_areas(&app, area);
        let panel = prio_panel_rect(&app, area);
        let body = Rect::new(
            panel.x,
            panel.y + PRIO_PANEL_HEADER_ROWS,
            panel.width,
            panel.height - PRIO_PANEL_HEADER_ROWS,
        );
        assert_eq!(entries.len(), 10);
        assert_eq!(rows.len(), usize::from(body.height) - 1);

        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let indicator_row = body.y + body.height - 1;
        assert!(
            row_text(buffer, indicator_row, body.width).contains("+4 more"),
            "{buffer:?}"
        );
        let rendered_entry_rows = (body.y..indicator_row)
            .filter(|row| !row_text(buffer, *row, body.width).is_empty())
            .count();
        assert_eq!(rendered_entry_rows, rows.len());
    }

    #[test]
    fn empty_prio_panel_has_header_only_without_empty_body() {
        let app = app_with_agents(&["one"]);
        let area = Rect::new(0, 0, 40, 16);
        let panel = prio_panel_rect(&app, area);
        assert_eq!(panel.height, PRIO_PANEL_HEADER_ROWS);
        assert!(compute_prio_panel_row_areas(&app, area).is_empty());

        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        assert!(row_text(terminal.backend().buffer(), panel.y, area.width - 1).contains("PRIO"));
    }

    #[test]
    fn collapsing_prio_panel_expands_workspace_list_and_normalizes_scroll() {
        let mut app = app_with_agents(&["one", "two", "three", "four", "five", "six"]);
        for workspace in &mut app.workspaces {
            workspace.tabs[0].set_prio(true);
        }
        let area = Rect::new(0, 0, 40, 12);
        app.view.sidebar_rect = area;
        app.workspace_scroll = usize::MAX;
        let expanded_list_height = workspace_list_rect_for_app(&app, area).height;

        let requested = app.workspace_scroll;
        app.toggle_prio_panel();

        assert!(app.prio_panel_collapsed);
        assert!(workspace_list_rect_for_app(&app, area).height > expanded_list_height);
        assert!(compute_prio_panel_row_areas(&app, area).is_empty());
        assert_eq!(
            app.workspace_scroll,
            normalized_workspace_scroll(&app, area, requested)
        );
    }

    #[test]
    fn expanded_prio_panel_keeps_minimum_workspace_height_in_short_sidebar() {
        let mut app = app_with_agents(&["one"]);
        app.workspaces[0].tabs[0].set_prio(true);
        let area = Rect::new(0, 0, 30, 5);
        let list = workspace_list_rect_for_app(&app, area);
        assert!(list.height >= MIN_WORKSPACE_LIST_ROWS);
    }

    #[test]
    fn tab_gutter_keeps_title_column_stable_for_prio_state() {
        let app = app_with_agents(&["one", "two"]);
        let area = Rect::new(0, 0, 40, 20);
        let card = compute_tab_card_areas(&app, area)[0].clone();
        let entry = sidebar_rows(&app)
            .into_iter()
            .find_map(|row| match row {
                SidebarRow::Tab { entry, .. }
                    if entry.ws_idx == card.ws_idx && entry.tab_idx == card.tab_idx =>
                {
                    Some(entry)
                }
                _ => None,
            })
            .expect("tab row should exist");
        let title = entry
            .primary_tab_label
            .as_deref()
            .expect("test tab should have a title");
        let layout = tab_row_layout(
            &entry,
            app.view_observed_at,
            39,
            4,
            &app.palette,
            app.status_indicators,
        );

        let mut terminal = Terminal::new(TestBackend::new(40, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let title_start = (card.rect.x..card.rect.x + card.rect.width)
            .find(|x| {
                buffer.cell((*x, card.rect.y)).is_some_and(|cell| {
                    cell.symbol() == layout.title.chars().next().unwrap_or_default().to_string()
                })
            })
            .expect("unflagged title should be visible");
        let mut flagged = app_with_agents(&["one", "two"]);
        flagged.workspaces[0].tabs[0].set_prio(true);
        let mut flagged_terminal = Terminal::new(TestBackend::new(40, 20)).unwrap();
        flagged_terminal
            .draw(|frame| render_sidebar(&flagged, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let flagged_card = compute_tab_card_areas(&flagged, area)[0].clone();
        let flagged_buffer = flagged_terminal.backend().buffer();
        let flagged_title_start = (flagged_card.rect.x
            ..flagged_card.rect.x + flagged_card.rect.width)
            .find(|x| {
                flagged_buffer
                    .cell((*x, flagged_card.rect.y))
                    .is_some_and(|cell| {
                        cell.symbol() == layout.title.chars().next().unwrap_or_default().to_string()
                    })
            })
            .expect("flagged title should be visible");
        assert_eq!(flagged_title_start, title_start,);
        assert_ne!(
            buffer.cell((card.rect.x + 4, card.rect.y)).unwrap().fg,
            app.palette.peach
        );
        assert_eq!(
            buffer
                .cell((card.rect.x + 4, card.rect.y))
                .expect("unflagged PRIO cell")
                .symbol(),
            "·"
        );
        assert!(row_text(buffer, card.rect.y, 39).contains(title));
    }

    #[test]
    fn desktop_tab_gutters_render_prio_and_never_a_work_link_marker() {
        let mut app = app_with_agents(&["one", "two"]);
        add_work_link(&mut app, 0);
        app.view.terminal_area = Rect::new(40, 0, 80, 20);
        let area = Rect::new(0, 0, 40, 20);
        let cards = compute_tab_card_areas(&app, area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let first_prio = crate::ui::tab_prio_rect(&cards[0]);
        let second_prio = crate::ui::tab_prio_rect(&cards[1]);
        assert_eq!(
            buffer.cell((first_prio.x, first_prio.y)).unwrap().symbol(),
            "·"
        );
        assert_eq!(
            buffer
                .cell((second_prio.x, second_prio.y))
                .unwrap()
                .symbol(),
            "·"
        );
        // The overview never advertises work-context links; the session's work-context panel owns
        // them, so no row may carry the marker even when the pane is linked.
        for row in 0..area.height {
            let rendered = row_text(buffer, row, area.width);
            assert!(
                !rendered.contains('↗'),
                "row {row} must not carry a work-link marker: {rendered:?}"
            );
        }
    }

    #[test]
    fn tab_row_title_budget_ignores_work_links() {
        let mut app = app_with_agents(&["one", "two"]);
        app.workspaces[0].tabs[0].custom_name = Some("abcdefghij".into());
        app.workspaces[1].tabs[0].custom_name = Some("abcdefghij".into());
        add_work_link(&mut app, 0);
        let entries = sidebar_thread_entries(&app);
        let linked = entries
            .iter()
            .find(|entry| entry.ws_idx == 0)
            .expect("linked entry");
        let unlinked = entries
            .iter()
            .find(|entry| entry.ws_idx == 1)
            .expect("unlinked entry");

        let linked_layout = tab_row_layout(
            linked,
            app.view_observed_at,
            18,
            4,
            &app.palette,
            app.status_indicators,
        );
        let unlinked_layout = tab_row_layout(
            unlinked,
            app.view_observed_at,
            18,
            4,
            &app.palette,
            app.status_indicators,
        );

        assert!(display_width(&linked_layout.title) <= 18 - 4 - TAB_PRIO_FIELD_WIDTH);
        assert_eq!(
            display_width(&unlinked_layout.title),
            display_width(&linked_layout.title),
            "a linked row must spend no width on a work-link cell"
        );
    }

    #[test]
    fn narrow_flagged_tab_budget_truncates_title() {
        let mut app = app_with_agents(&["one"]);
        app.workspaces[0].tabs[0].custom_name = Some("a deliberately long tab title".into());
        app.workspaces[0].tabs[0].set_prio(true);
        let area = Rect::new(0, 0, 18, 12);
        let mut terminal = Terminal::new(TestBackend::new(18, 12)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();

        let card = compute_tab_card_areas(&app, area)[0].clone();
        let rendered = row_text(terminal.backend().buffer(), card.rect.y, 17);
        assert!(rendered.contains('…'), "{rendered:?}");
        assert!(
            !rendered.contains("deliberately long tab title"),
            "{rendered:?}"
        );
        assert!(rendered.chars().count() <= 17, "{rendered:?}");
    }

    #[test]
    fn flattened_spaces_sidebar_uses_configured_status_symbols() {
        let mut app = app_with_agents(&["blocked", "done"]);
        app.status_indicators = StatusIndicatorStyle::Symbols;

        let first_pane = app.workspaces[0].tabs[0].root_pane;
        let first_terminal = app.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        let second_pane = app.workspaces[1].tabs[0].root_pane;
        let second_terminal = app.workspaces[1].tabs[0].panes[&second_pane]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&first_terminal).unwrap().state = AgentState::Blocked;
        let second_terminal_state = app.terminals.get_mut(&second_terminal).unwrap();
        second_terminal_state.state = AgentState::Idle;
        app.workspaces[1].tabs[0]
            .panes
            .get_mut(&second_pane)
            .unwrap()
            .seen = false;
        app.reconcile_sidebar_presentation();

        let area = Rect::new(0, 0, 32, 12);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let text = (0..area.height)
            .map(|row| row_text(terminal.backend().buffer(), row, area.width - 1))
            .collect::<Vec<_>>();

        assert!(text.iter().any(|line| line.contains('×')), "{text:?}");
        assert!(text.iter().any(|line| line.contains('✓')), "{text:?}");
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
        assert_eq!(empty_threads[0].primary_tab_label.as_deref(), Some("1"));
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
                    SidebarRow::SectionHeader { .. } => ('h', 0),
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
                SidebarRow::Workspace { .. }
                | SidebarRow::Agent { .. }
                | SidebarRow::SectionHeader { .. } => None,
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
                    SidebarRow::SectionHeader { .. } => ("section", 0, None, None),
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
            generation: None,
            version: 3,
            workspaces: Vec::new(),
            active: None,
            selected: 0,
            sidebar_width: Some(24),
            sidebar_section_split: Some(0.4),
            collapsed_space_keys: std::collections::HashSet::new(),
            prio_panel_collapsed: false,
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
            (Rect::new(0, 0, 30, 18), Rect::new(0, 18, 30, 2))
        );
        assert_eq!(
            expanded_sidebar_sections(area, 0.9),
            (Rect::new(0, 0, 30, 3), Rect::new(0, 3, 30, 17))
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

    fn evidence_color_css(color: Color, fallback: &str) -> String {
        match color {
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            Color::Black => "#000000".into(),
            Color::White => "#ffffff".into(),
            Color::Red => "#ff0000".into(),
            Color::Green => "#00ff00".into(),
            Color::Yellow => "#ffff00".into(),
            Color::Blue => "#0000ff".into(),
            Color::Magenta => "#ff00ff".into(),
            Color::Cyan => "#00ffff".into(),
            Color::Gray => "#808080".into(),
            Color::DarkGray => "#404040".into(),
            _ => fallback.into(),
        }
    }

    #[test]
    fn sidebar_visual_evidence_renders_release_layout() {
        let mut app = AppState::test_new();
        app.palette = crate::app::state::Palette::one_light();

        let mut active = Workspace::test_new("Herdr");
        active.tabs[0].custom_name = Some("Polish sidebar selection".into());
        let active_root = active.tabs[0].root_pane;
        active.test_split(Direction::Horizontal);
        active.test_add_tab(Some("Review lifecycle assertions"));

        let mut queued = Workspace::test_new("Fleet docs");
        queued.tabs[0].custom_name = Some("Update release notes".into());
        app.workspaces = vec![active, queued];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.mode = Mode::Terminal;
        app.sidebar_spaces.row_gap = 1;

        let terminal_id = app.workspaces[0].tabs[0].panes[&active_root]
            .attached_terminal_id
            .clone();
        let terminal_state = app.terminals.get_mut(&terminal_id).unwrap();
        terminal_state.detected_agent = Some(Agent::Codex);
        terminal_state.state = AgentState::Working;
        terminal_state.background_job_count = Some(2);
        app.reconcile_sidebar_presentation();

        let width = 60;
        let height = 12;
        let area = Rect::new(0, 0, width, height);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();

        let rows = (0..height)
            .map(|row| row_text(terminal.backend().buffer(), row, width))
            .collect::<Vec<_>>();
        assert!(rows
            .iter()
            .any(|row| row.contains("working") && row.contains("Polish sidebar selection")));
        assert!(rows
            .iter()
            .any(|row| row.contains("Review lifecycle assertions")));
        assert!(rows.iter().any(|row| row.contains("Fleet docs")));
        assert!(
            rows.iter()
                .any(|row| row.replace('│', "").trim().is_empty()),
            "Space groups should have a visual gap: {rows:?}"
        );

        let Ok(path) = std::env::var("HERDR_SIDEBAR_EVIDENCE_HTML") else {
            return;
        };
        let mut html = String::from(
            "<!doctype html><meta charset=\"utf-8\"><title>Herdr sidebar release evidence</title>\
             <style>body{margin:0;padding:24px;background:#eff1f5;color:#4c4f69;\
             font-family:\"JetBrains Mono\",ui-monospace,monospace}h1{font-size:18px}\
             p{max-width:760px}.terminal{display:grid;width:max-content;font-size:14px;\
             line-height:20px;box-shadow:0 0 0 1px #bcc0cc;background:#eff1f5}.cell{width:1ch;\
             height:20px;white-space:pre;overflow:visible}</style><h1>Herdr sidebar release layout</h1>\
             <p>Actual Ratatui test buffer: selected titles, blue Working status, provider suffix,\
             background-terminal count, one row per tab, multi-pane roll-up, and compact spacing\
             between complete Space groups.</p>\
             <div class=\"terminal\" style=\"grid-template-columns:repeat(60,1ch)\">",
        );
        for cell in terminal.backend().buffer().content() {
            let symbol = cell
                .symbol()
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            let weight = if cell.modifier.contains(Modifier::BOLD) {
                "font-weight:700;"
            } else {
                ""
            };
            html.push_str(&format!(
                "<span class=\"cell\" style=\"color:{};background:{};{weight}\">{symbol}</span>",
                evidence_color_css(cell.fg, "#4c4f69"),
                evidence_color_css(cell.bg, "#eff1f5"),
            ));
        }
        html.push_str("</div>");
        std::fs::write(path, html).expect("write sidebar visual evidence");
    }

    #[test]
    fn ac1_ac2_ac3_ac4_cumulative_space_first_single_line_fixture() {
        let mut app = AppState::test_new();
        app.palette = crate::app::state::Palette::one_light();
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
        app.palette = crate::app::state::Palette::one_light();
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
        assert!(
            tab_window.contains("Fix Billing Retry · pi"),
            "{tab_window:?}"
        );
        assert!(!first.contains("working"));
        assert!(tab_window.contains("working"));
        assert!(agent_cards.is_empty());

        let workspace_x = find_symbol_x(buffer, workspace_row, 59, "o");
        let workspace_style = buffer[(workspace_x, workspace_row)].style();
        // ac2: active titles are visibly darker than the prior One Light text.
        assert_eq!(workspace_style.fg, Some(Color::Rgb(37, 38, 44)));
        assert!(workspace_style.add_modifier.contains(Modifier::BOLD));
        assert!(!workspace_style.add_modifier.contains(Modifier::DIM));
        assert_eq!(workspace_style.bg, Some(ratatui::style::Color::Reset));

        let title_x = find_symbol_x(buffer, tab_row, 59, "F");
        let title_style = buffer[(title_x, tab_row)].style();
        assert_eq!(title_style.fg, Some(Color::Rgb(37, 38, 44)));
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

        let area = Rect::new(0, 0, 50, 12);
        app.view_observed_at = started + std::time::Duration::from_secs(42);
        let mut busy = Terminal::new(TestBackend::new(50, 12)).unwrap();
        busy.draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let tab_row = compute_tab_card_areas(&app, area)[0].rect.y;
        let busy_text = row_text(busy.backend().buffer(), tab_row, 49);
        assert!(busy_text.contains("working"), "{busy_text:?}");
        assert!(busy_text.ends_with("42s ago"), "{busy_text:?}");
        // ac7: both the Working label and its live age use activity blue.
        for token in ["working", "42s ago"] {
            let token_x = busy_text.find(token).expect("rendered token") as u16;
            assert_eq!(
                busy.backend().buffer()[(token_x, tab_row)].style().fg,
                Some(app.palette.blue)
            );
        }

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
        let mut idle = Terminal::new(TestBackend::new(50, 12)).unwrap();
        idle.draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let idle_text = row_text(idle.backend().buffer(), tab_row, 49);
        assert!(idle_text.contains("done"), "{idle_text:?}");
        assert!(idle_text.ends_with("5m ago"), "{idle_text:?}");
    }

    #[test]
    fn seen_idle_tab_omits_status_while_retaining_title_and_clock() {
        let started = std::time::Instant::now();
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("one");
        workspace.tabs[0].custom_name = Some("Review release".into());
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.reconcile_sidebar_presentation();
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
        let finished = started + std::time::Duration::from_secs(5);
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
            .seen = true;
        app.view_observed_at = finished + std::time::Duration::from_secs(65);

        let area = Rect::new(0, 0, 50, 12);
        let mut terminal = Terminal::new(TestBackend::new(50, 12)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let tab_row = compute_tab_card_areas(&app, area)[0].rect.y;
        let rendered = row_text(terminal.backend().buffer(), tab_row, 49);

        assert!(rendered.contains("Review release"), "{rendered:?}");
        assert!(rendered.ends_with("1m ago"), "{rendered:?}");
        assert!(!rendered.contains("idle"), "{rendered:?}");
        assert!(!rendered.contains("done"), "{rendered:?}");
        // The gutter's own prio marker is the only thing allowed before the title; nothing may be
        // interposed between it and the title.
        assert!(
            rendered.trim_start().starts_with("· Review release"),
            "{rendered:?}"
        );
    }

    #[test]
    fn multi_pane_tab_age_uses_latest_thread_communication() {
        let started = std::time::Instant::now();
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("one");
        workspace.tabs[0].custom_name = Some("Grouped work".into());
        let first_pane = workspace.tabs[0].root_pane;
        let second_pane = workspace.test_split(Direction::Horizontal);
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.reconcile_sidebar_presentation();

        for (pane_id, active_at) in [
            (first_pane, started),
            (
                second_pane,
                started + std::time::Duration::from_secs(5 * 60),
            ),
        ] {
            let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.terminals
                .get_mut(&terminal_id)
                .unwrap()
                .set_detected_state_with_screen_signals_at(
                    Some(Agent::Codex),
                    AgentState::Working,
                    false,
                    false,
                    true,
                    false,
                    active_at,
                );
        }

        app.view_observed_at = started + std::time::Duration::from_secs(10 * 60);
        let area = Rect::new(0, 0, 50, 12);
        let mut terminal = Terminal::new(TestBackend::new(50, 12)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let card = &compute_tab_card_areas(&app, area)[0];
        let rendered = row_text(terminal.backend().buffer(), card.rect.y, 49);

        assert!(rendered.ends_with("5m ago"), "{rendered:?}");
        assert_eq!(compute_tab_card_areas(&app, area).len(), 1);
    }

    #[test]
    fn narrow_tab_rows_keep_status_before_truncated_title() {
        let mut app = app_with_agents(&["one"]);
        app.workspaces[0].tabs[0].custom_name = Some("one".into());
        let area = Rect::new(0, 0, 23, 12);
        let mut terminal = Terminal::new(TestBackend::new(23, 12)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let card = &compute_tab_card_areas(&app, area)[0];
        let rendered = row_text(terminal.backend().buffer(), card.rect.y, 23);

        assert!(
            rendered.contains('●') || rendered.contains('w'),
            "{rendered:?}"
        );
        assert!(rendered.contains("· pi"), "{rendered:?}");
        assert!(!rendered.contains("--"), "{rendered:?}");
    }

    #[test]
    fn expanded_tab_row_renders_cached_foreground_process_name() {
        let mut app = app_with_agents(&["one"]);
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.workspaces[0].tabs[0].custom_name = Some("release ticket".into());
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .foreground_process_name = Some("cargo".into());

        let area = Rect::new(0, 0, 60, 12);
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let card = &compute_tab_card_areas(&app, area)[0];
        let rendered = row_text(terminal.backend().buffer(), card.rect.y, area.width - 1);

        assert!(rendered.contains("cargo"), "{rendered:?}");
        assert!(rendered.contains("release ticket"), "{rendered:?}");
    }

    #[test]
    fn shell_only_foreground_cache_renders_no_extra_process() {
        let app = app_with_agents(&["one"]);
        let area = Rect::new(0, 0, 60, 12);
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let card = &compute_tab_card_areas(&app, area)[0];
        let rendered = row_text(terminal.backend().buffer(), card.rect.y, area.width - 1);

        assert!(!rendered.contains("zsh"), "{rendered:?}");
        assert!(!rendered.contains("cargo"), "{rendered:?}");
    }

    #[test]
    fn foreground_process_is_dropped_before_existing_tab_details() {
        let mut app = app_with_agents(&["one"]);
        app.workspaces[0].tabs[0].custom_name = Some("ticket".into());
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .foreground_process_name = Some("cargo".into());
        let entry = sidebar_thread_entries(&app).remove(0);

        let layout = tab_row_layout(
            &entry,
            app.view_observed_at,
            24,
            1,
            &app.palette,
            app.status_indicators,
        );

        assert!(layout.foreground_process.is_none());
        assert!(layout.agent_suffix.is_some());
        assert!(display_width(&layout.title) >= TAB_ACTIVITY_AGE_MIN_TITLE_WIDTH);
    }

    #[test]
    fn foreground_process_drops_before_activity_age_in_width_ladder() {
        let mut app = app_with_agents(&["one"]);
        app.workspaces[0].tabs[0].custom_name = Some("ticket".into());
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .foreground_process_name = Some("node".into());
        let mut entry = sidebar_thread_entries(&app).remove(0);
        entry.background_job_count = Some(1);
        entry.activity_at = Some(
            app.view_observed_at
                .checked_sub(std::time::Duration::from_secs(12 * 60))
                .expect("activity timestamp before observation time"),
        );

        // At this width the old ladder kept "node" while dropping "12m ago".
        let layout = tab_row_layout(
            &entry,
            app.view_observed_at,
            32,
            1,
            &app.palette,
            app.status_indicators,
        );

        assert!(layout.foreground_process.is_none());
        assert!(layout.activity_age.is_none());
        assert!(layout.background_jobs.is_some());
    }

    #[test]
    fn reported_at_age_refresh_uses_the_reported_instant_on_desktop_and_mobile() {
        let mut app = app_with_agents(&["one"]);
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let reported_at = std::time::Instant::now();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_hook_authority_report_at(
                "herdr:pi-closing-block".into(),
                "pi".into(),
                AgentState::Blocked,
                None,
                None,
                None,
                None,
                None,
                Some(1),
                reported_at,
            )
            .expect("blocked report accepted");
        app.view_observed_at = reported_at
            .checked_add(std::time::Duration::from_secs(60))
            .expect("observation timestamp after report");

        let runtimes = TerminalRuntimeRegistry::new();
        let area = Rect::new(0, 0, 80, 20);
        let cards = compute_tab_card_areas(&app, area);
        assert_eq!(
            visible_tab_activity_instants_from(&app, &runtimes, &cards),
            vec![reported_at]
        );
        assert_eq!(
            crate::ui::mobile::visible_tab_activity_instants_from(&app, &runtimes, area),
            vec![reported_at]
        );
    }

    #[test]
    fn long_foreground_process_name_is_truncated_to_its_budget() {
        let mut app = app_with_agents(&["one"]);
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .foreground_process_name = Some("foreground-process-name".into());
        let entry = sidebar_thread_entries(&app).remove(0);

        let layout = tab_row_layout(
            &entry,
            app.view_observed_at,
            32,
            1,
            &app.palette,
            app.status_indicators,
        );
        let process = layout
            .foreground_process
            .expect("process should fit partially");

        assert_ne!(process, " · foreground-process-name");
        let used_width = 1
            + layout
                .state
                .as_deref()
                .map(|state| display_width(state) + 2 + display_width("●") + 3)
                .unwrap_or_default()
            + display_width(&layout.title)
            + layout
                .agent_suffix
                .as_deref()
                .map(display_width)
                .unwrap_or_default()
            + display_width(&process);
        assert!(used_width <= 32, "used width: {used_width}");
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
        app.sidebar_width = 36;
        app.sidebar_min_width = 18;
        app.sidebar_max_width = 36;
        let runtimes = TerminalRuntimeRegistry::new();

        crate::ui::compute_view_with_runtime_registry(&mut app, &runtimes, Rect::new(0, 0, 80, 20));
        assert_eq!(app.view.visible_agent_activity_instants, vec![started]);

        app.sidebar_collapsed = true;
        crate::ui::compute_view_with_runtime_registry(&mut app, &runtimes, Rect::new(0, 0, 80, 20));
        assert!(app.view.visible_agent_activity_instants.is_empty());

        app.sidebar_collapsed = false;
        app.mobile_width_threshold = 80;
        crate::ui::compute_view_with_runtime_registry(&mut app, &runtimes, Rect::new(0, 0, 80, 20));
        assert_eq!(app.view.visible_agent_activity_instants, vec![started]);

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
        assert!(!rendered.contains("New Th"), "{rendered:?}");
        assert!(rendered.contains("pi"), "{rendered:?}");
        assert!(compute_agent_card_areas(&app, area).is_empty());
    }

    #[test]
    fn sidebar_tab_row_uses_live_agent_title() {
        let mut app = app_with_agents(&["one"]);
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_terminal_title(Some("⠋ Add Subabe management token to Doppler".into()));

        let entries = sidebar_thread_entries(&app);
        assert_eq!(
            entries[0].primary_tab_label.as_deref(),
            Some("Add Subabe management token to Doppler")
        );

        let area = Rect::new(0, 0, 40, 8);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let tab_row = compute_tab_card_areas(&app, area)[0].rect.y;
        let rendered = row_text(terminal.backend().buffer(), tab_row, area.width - 1);
        assert!(rendered.contains("Add Subabe"), "{rendered:?}");
    }

    #[test]
    fn sidebar_tab_row_title_does_not_animate_with_a_circle_spinner() {
        let area = Rect::new(0, 0, 40, 8);
        let render_frame = |glyph: &str| {
            let mut app = app_with_agents(&["one"]);
            let pane_id = app.workspaces[0].tabs[0].root_pane;
            let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.terminals
                .get_mut(&terminal_id)
                .unwrap()
                .set_terminal_title(Some(format!("{glyph} Refactor the parser")));
            let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
            terminal
                .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
                .unwrap();
            let tab_row = compute_tab_card_areas(&app, area)[0].rect.y;
            row_text(terminal.backend().buffer(), tab_row, area.width - 1)
        };

        let first = render_frame("◐");
        let second = render_frame("◓");
        assert!(first.contains("Refactor the"), "{first:?}");
        assert!(
            !first.contains('◐') && !second.contains('◓'),
            "{first:?} / {second:?}"
        );
        assert_eq!(
            first, second,
            "an agent's spinner frame must not change the sidebar row"
        );
    }

    #[test]
    fn sidebar_and_tab_bar_render_the_same_agent_title() {
        let mut app = app_with_agents(&["one"]);
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_terminal_title(Some("Fix billing".into()));

        let expected = app.workspaces[0]
            .tab_display_name_from(&app.terminals, 0)
            .expect("canonical tab name");
        // Anchor the canonical name to a literal as well as to the other
        // consumers: comparing two derivations to each other alone would still
        // pass if both regressed the same way.
        assert_eq!(expected, "Fix billing");
        let entry = sidebar_thread_entries(&app)
            .into_iter()
            .next()
            .expect("sidebar tab entry");
        assert_eq!(entry.primary_tab_label.as_deref(), Some(expected.as_str()));

        let area = Rect::new(0, 0, 60, 8);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let tab_row = compute_tab_card_areas(&app, area)[0].rect.y;
        let rendered = row_text(terminal.backend().buffer(), tab_row, area.width - 1);

        let tab_bar_label =
            crate::ui::tabs::tab_chrome_label(&app.workspaces[0], &app.terminals, 0, usize::MAX);
        assert_eq!(tab_bar_label, expected);
        assert!(rendered.contains(&tab_bar_label), "{rendered:?}");
        // The label no longer names the agent, so the sidebar appends the
        // provider chip exactly once and never leads with it.
        assert!(rendered.contains("Fix billing · pi"), "{rendered:?}");
        assert!(
            !rendered.contains("pi · Fix billing"),
            "sidebar led with the agent identity: {rendered:?}"
        );

        app.workspaces[0].tabs[0].set_user_custom_name("Human title".into());
        let expected = app.workspaces[0]
            .tab_display_name_from(&app.terminals, 0)
            .expect("human canonical tab name");
        // A human rename must win over every automatic source.
        assert_eq!(expected, "Human title");
        let entry = sidebar_thread_entries(&app)
            .into_iter()
            .next()
            .expect("sidebar tab entry after rename");
        assert_eq!(entry.primary_tab_label.as_deref(), Some(expected.as_str()));
        let custom_tab_bar_label =
            crate::ui::tabs::tab_chrome_label(&app.workspaces[0], &app.terminals, 0, usize::MAX);
        assert_eq!(custom_tab_bar_label, expected);
    }

    #[test]
    fn split_tab_sidebar_and_tab_bar_share_agent_title_projection() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("one");
        let root_pane = workspace.tabs[0].root_pane;
        let focused_pane = workspace.test_split(Direction::Horizontal);
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.active = Some(0);

        let root_terminal = app.workspaces[0].tabs[0].panes[&root_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&root_terminal)
            .expect("root terminal")
            .detected_agent = Some(Agent::Pi);

        let focused_terminal = app.workspaces[0].tabs[0].panes[&focused_pane]
            .attached_terminal_id
            .clone();
        let focused = app
            .terminals
            .get_mut(&focused_terminal)
            .expect("focused terminal");
        focused.detected_agent = Some(Agent::Pi);
        focused.agent_name = Some("Reviewer".into());
        focused.set_terminal_title(Some("Fix billing".into()));
        app.reconcile_sidebar_presentation();

        let sidebar_entry = sidebar_rows(&app)
            .into_iter()
            .find_map(|row| match row {
                SidebarRow::Tab { entry, .. } => Some(entry),
                _ => None,
            })
            .expect("split tab sidebar row");
        assert!(!sidebar_entry.tab_label_leads_with_agent);

        let area = Rect::new(0, 0, 60, 10);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let tab_row = compute_tab_card_areas(&app, area)[0].rect.y;
        let rendered = row_text(terminal.backend().buffer(), tab_row, area.width - 1);
        let tab_bar_label = crate::ui::tabs::tab_chrome_label(
            &app.workspaces[0],
            &app.terminals,
            0,
            usize::from(area.width),
        );

        assert_eq!(tab_bar_label, "Fix billing");
        assert!(rendered.contains(&tab_bar_label), "{rendered:?}");
        assert!(!rendered.contains("Reviewer · Fix billing"), "{rendered:?}");
    }

    #[test]
    fn prio_row_appends_the_provider_once_after_the_workspace() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("one");
        let focused_pane = workspace.tabs[0].root_pane;
        workspace.tabs[0].set_prio(true);
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.active = Some(0);

        let focused_terminal = app.workspaces[0].tabs[0].panes[&focused_pane]
            .attached_terminal_id
            .clone();
        let focused = app
            .terminals
            .get_mut(&focused_terminal)
            .expect("focused terminal");
        focused.detected_agent = Some(Agent::Pi);
        focused.agent_name = Some("Reviewer".into());
        focused.set_terminal_title(Some("Fix billing".into()));
        app.reconcile_sidebar_presentation();

        let entry = prio_panel_entries(&app, None)
            .into_iter()
            .next()
            .expect("prio entry");
        assert!(!entry.tab_label_leads_with_agent);

        let area = Rect::new(0, 0, 60, 16);
        let rows = compute_prio_panel_row_areas(&app, area);
        assert_eq!(rows.len(), 1);

        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let rendered = row_text(terminal.backend().buffer(), rows[0].rect.y, area.width);

        assert!(rendered.contains("Fix billing · one · pi"), "{rendered:?}");
        assert!(!rendered.contains("Reviewer"), "{rendered:?}");
    }

    #[test]
    fn prio_row_does_not_repeat_agent_identity_from_tab_title() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("one");
        let focused_pane = workspace.tabs[0].root_pane;
        workspace.tabs[0].set_prio(true);
        workspace.tabs[0].set_custom_name("Codex".into());
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.active = Some(0);

        let focused_terminal = app.workspaces[0].tabs[0].panes[&focused_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&focused_terminal)
            .expect("focused terminal")
            .detected_agent = Some(Agent::Codex);
        app.reconcile_sidebar_presentation();

        let area = Rect::new(0, 0, 60, 16);
        let rows = compute_prio_panel_row_areas(&app, area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let rendered = row_text(terminal.backend().buffer(), rows[0].rect.y, area.width);

        assert_eq!(
            rendered.to_ascii_lowercase().matches("codex").count(),
            1,
            "{rendered:?}"
        );
    }

    #[test]
    fn default_space_workspace_style_tracks_active_state() {
        let mut app = crate::app::state::AppState::test_new();
        app.palette = crate::app::state::Palette::one_light();
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
        // ac2: One Light selected titles darken from #383a42 to #25262c.
        assert_eq!(active.fg, Some(Color::Rgb(37, 38, 44)));
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
    fn final_space_row_ignores_legacy_custom_token_rows() {
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
        let rendered = row_text(terminal.backend().buffer(), row, 25);
        assert!(rendered.contains("one (1)"), "{rendered:?}");
        assert!(!rendered.contains("HI"), "{rendered:?}");
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
        assert!(first.contains("· pi"), "{first:?}");
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
    fn legacy_space_row_config_cannot_reintroduce_subtitles() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]; 10];
        let area = Rect::new(0, 0, 20, 10);
        let workspace_area = workspace_list_rect(area, app.sidebar_section_split);
        let metrics = workspace_list_scroll_metrics(&app, workspace_area);
        let (cards, _) = compute_workspace_list_areas(&app, area);

        assert_eq!(metrics.viewport_rows, 2);
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].ws_idx, 0);
        assert_eq!(cards[0].rect.height, 1);
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
    fn a_depth_zero_worklist_entry_occupies_one_line() {
        let mut app = priority_app_with_states(&[AgentState::Blocked]);
        app.sidebar_agents.rows = vec![
            vec![crate::config::AgentSidebarToken::Agent],
            vec![crate::config::AgentSidebarToken::Workspace],
        ];
        let area = Rect::new(0, 0, 40, 12);
        let rows = sidebar_rows(&app);
        let blocked = rows
            .iter()
            .find_map(|row| match row {
                SidebarRow::Agent { entry, depth } if *depth == 0 => Some(entry),
                _ => None,
            })
            .expect("blocked worklist should contain an agent row");

        assert_eq!(
            resolved_agent_rows_at(&app, blocked, 0).len(),
            1,
            "worklist rows ignore the configured multi-row tree template"
        );
        assert_eq!(
            agent_entry_height_in_body_at(&app, blocked, 8, 0),
            1,
            "layout height must match the single painted worklist row"
        );
        let cards = compute_agent_card_areas(&app, area);
        let card = cards
            .iter()
            .find(|card| card.ws_idx == blocked.ws_idx && card.pane_id == blocked.pane_id)
            .expect("blocked worklist card should have geometry");
        assert_eq!(card.rect.height, 1);
    }

    #[test]
    fn a_blocked_worklist_row_renders_its_title_without_its_space_name() {
        let mut app = priority_app_with_states(&[AgentState::Blocked, AgentState::Working]);
        let blocked_pane = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].terminal_id(blocked_pane).unwrap().clone();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_terminal_title(Some("Review blocked thread".into()));
        let area = Rect::new(0, 0, 50, 12);
        let card = compute_agent_card_areas(&app, area)
            .into_iter()
            .find(|card| card.pane_id == blocked_pane)
            .expect("blocked worklist card should render");
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("test terminal should initialize");
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .expect("sidebar should render");

        let rendered = row_text(terminal.backend().buffer(), card.rect.y, area.width - 1);
        assert!(rendered.contains("Review blocked thread"), "{rendered:?}");
        assert!(!rendered.contains("ws0"), "{rendered:?}");
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
                ("auto", Some("pi")),
                ("custom", Some("focus")),
                ("multi", Some("codex")),
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

    /// Describe the priority projection as `("section", title)` / `("agent",
    /// workspace)` pairs, so a test can assert grouping and order together.
    fn priority_row_shape(app: &AppState) -> Vec<(&'static str, String)> {
        sidebar_rows(app)
            .into_iter()
            .map(|row| match row {
                SidebarRow::SectionHeader { title, .. } => ("section", title.to_string()),
                SidebarRow::Agent { entry, .. } => ("agent", entry.primary_label.clone()),
                SidebarRow::Workspace { .. } => ("workspace", String::new()),
                SidebarRow::Tab { .. } => ("tab", String::new()),
            })
            .collect()
    }

    fn priority_app_with_states(states: &[AgentState]) -> AppState {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = states
            .iter()
            .enumerate()
            .map(|(idx, _)| Workspace::test_new(&format!("ws{idx}")))
            .collect();
        app.ensure_test_terminals();
        app.active = Some(0);
        app.selected = 0;
        app.agent_panel_sort = crate::app::state::AgentPanelSort::Priority;
        for (ws_idx, state) in states.iter().enumerate() {
            let pane = app.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            let terminal = app.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(Agent::Claude);
            terminal.state = *state;
        }
        app
    }

    #[test]
    fn blocked_agents_group_above_the_rest_under_their_own_header() {
        let app = priority_app_with_states(&[
            AgentState::Working,
            AgentState::Blocked,
            AgentState::Idle,
            AgentState::Blocked,
        ]);

        let shape = priority_row_shape(&app);
        assert_eq!(
            shape[..4],
            [
                ("section", BLOCKED_SECTION_TITLE.to_string()),
                ("agent", "ws1".to_string()),
                ("agent", "ws3".to_string()),
                ("section", SPACES_SECTION_TITLE.to_string()),
            ]
        );

        // Below the second header the ownership tree resumes untouched: the
        // blocked group is a worklist layered on top, not a reordering of it.
        assert!(
            shape[4..].iter().all(|(kind, _)| *kind != "section"),
            "the tree below must not grow further headers: {shape:?}"
        );
        assert!(shape[4..].iter().any(|(kind, _)| *kind == "workspace"));
    }

    #[test]
    fn pinned_tabs_group_below_blocked_and_above_the_tree() {
        let mut app =
            priority_app_with_states(&[AgentState::Working, AgentState::Blocked, AgentState::Idle]);
        // ws0 is merely pinned; ws1 is pinned *and* blocked.
        app.workspaces[0].tabs[0].pinned = true;
        app.workspaces[1].tabs[0].pinned = true;

        let shape = priority_row_shape(&app);
        assert_eq!(
            shape[..5],
            [
                ("section", BLOCKED_SECTION_TITLE.to_string()),
                ("agent", "ws1".to_string()),
                ("section", PINNED_SECTION_TITLE.to_string()),
                ("agent", "ws0".to_string()),
                ("section", SPACES_SECTION_TITLE.to_string()),
            ],
            "a pinned-and-blocked tab is listed once, under Blocked: {shape:?}"
        );
    }

    #[test]
    fn nothing_pinned_or_blocked_costs_no_header_rows() {
        let mut app = priority_app_with_states(&[AgentState::Working, AgentState::Idle]);
        app.workspaces[0].tabs[0].pinned = true;
        assert!(
            sidebar_rows(&app)
                .iter()
                .any(|row| matches!(row, SidebarRow::SectionHeader { .. })),
            "a pin alone must be enough to open the group"
        );
        app.workspaces[0].tabs[0].pinned = false;
        assert!(
            !sidebar_rows(&app)
                .iter()
                .any(|row| matches!(row, SidebarRow::SectionHeader { .. })),
            "with nothing pinned or blocked the sidebar is byte-for-byte what it was"
        );
    }

    #[test]
    fn collapsing_a_group_keeps_its_header_and_drops_only_its_rows() {
        let mut app = priority_app_with_states(&[
            AgentState::Blocked,
            AgentState::Working,
            AgentState::Blocked,
        ]);
        let open = priority_row_shape(&app);
        assert_eq!(open[0], ("section", BLOCKED_SECTION_TITLE.to_string()));
        assert_eq!(open[1].0, "agent");

        app.collapsed_sidebar_groups
            .insert(BLOCKED_SECTION_TITLE.to_string());
        let folded = priority_row_shape(&app);
        assert_eq!(folded[0], ("section", BLOCKED_SECTION_TITLE.to_string()));
        assert_eq!(
            folded[1],
            ("section", SPACES_SECTION_TITLE.to_string()),
            "a folded group leaves its header and nothing else"
        );
        // The count survives the fold: it is the only thing left saying how much
        // is hidden behind the header.
        assert!(sidebar_rows(&app).iter().any(|row| matches!(
            row,
            SidebarRow::SectionHeader { title, count: 2, collapsed: true } if *title == BLOCKED_SECTION_TITLE
        )));
        assert!(
            folded.iter().any(|(kind, _)| *kind == "workspace"),
            "folding one group must not fold the tree"
        );
    }

    #[test]
    fn folding_spaces_hides_the_whole_tree_but_keeps_the_worklist() {
        let mut app = priority_app_with_states(&[AgentState::Blocked, AgentState::Working]);
        app.collapsed_sidebar_groups
            .insert(SPACES_SECTION_TITLE.to_string());
        let shape = priority_row_shape(&app);
        assert_eq!(
            shape,
            vec![
                ("section", BLOCKED_SECTION_TITLE.to_string()),
                ("agent", shape[1].1.clone()),
                ("section", SPACES_SECTION_TITLE.to_string()),
            ]
        );
    }

    #[test]
    fn a_spaces_fold_is_ignored_while_the_tree_stands_alone() {
        // Without a group above it the tree has no header to unfold from, so a
        // stale fold from a moment when something was blocked must not strand
        // the sidebar empty.
        let mut app = priority_app_with_states(&[AgentState::Working, AgentState::Idle]);
        app.collapsed_sidebar_groups
            .insert(SPACES_SECTION_TITLE.to_string());
        assert!(priority_row_shape(&app)
            .iter()
            .any(|(kind, _)| *kind == "workspace"));
    }

    #[test]
    fn nothing_blocked_leaves_the_list_exactly_as_it_was() {
        let app = priority_app_with_states(&[AgentState::Working, AgentState::Idle]);
        assert!(
            !sidebar_rows(&app)
                .iter()
                .any(|row| matches!(row, SidebarRow::SectionHeader { .. })),
            "an empty blocked group must not cost a header row"
        );
    }

    #[test]
    fn section_headers_are_not_selectable_and_do_not_consume_agent_numbers() {
        let app = priority_app_with_states(&[AgentState::Blocked, AgentState::Blocked]);
        let rows = sidebar_rows(&app);

        // Numbers the user can type must run 1..=n over agents only, skipping
        // the header rows that sit between them.
        let ordinals: Vec<usize> = agent_row_ordinals(&rows)
            .into_iter()
            .zip(rows.iter())
            .filter(|(_, row)| matches!(row, SidebarRow::Agent { .. }))
            .map(|(ordinal, _)| ordinal)
            .collect();
        assert_eq!(ordinals, [1, 2]);

        // And a header belongs to no workspace, so workspace scrolling can
        // never target one.
        for row in &rows {
            if matches!(row, SidebarRow::SectionHeader { .. }) {
                assert!(!sidebar_row_belongs_to_workspace(row, 0));
                assert!(!sidebar_row_belongs_to_workspace(row, 1));
            }
        }
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
            None,
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

        assert_eq!(ws_area, Rect::new(0, 0, 19, 3));
        assert_eq!(detail_area, Rect::new(0, 3, 19, 2));
    }

    #[test]
    fn workspace_list_omits_cjk_branch_subtitle_without_panic() {
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
        let rendered = row_text(terminal.backend().buffer(), 1, 15);
        assert!(!rendered.contains("中文"), "{rendered:?}");
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
    fn desktop_worktree_group_renders_one_space_row() {
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
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].ws_idx, 0);
        assert_eq!(cards[1].ws_idx, 3);
        let grouped = row_text(buffer, cards[0].rect.y, cards[0].rect.width);
        assert!(grouped.contains("herdr (3)"), "{grouped:?}");
        assert!(!grouped.contains("issue"), "{grouped:?}");
        assert!(!grouped.contains("review"), "{grouped:?}");
    }

    #[test]
    fn active_linked_window_darkens_its_root_space_title() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];
        app.ensure_test_terminals();
        app.reconcile_sidebar_presentation();
        app.active = Some(1);
        app.mode = Mode::Terminal;
        let area = Rect::new(0, 0, 30, 10);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let row = app.view.workspace_card_areas[0].rect.y;
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let title = buffer[(find_symbol_x(buffer, row, area.width, "h"), row)].style();

        assert_eq!(title.fg, Some(active_sidebar_title_color(&app.palette)));
        assert!(title.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn linked_worktrees_render_as_one_space_with_direct_window_rows() {
        let mut app = AppState::test_new();
        let mut main = workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr");
        main.test_add_tab(Some("Main review"));
        let mut issue =
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue");
        issue.tabs[0].custom_name = Some("Issue fix".into());
        app.workspaces = vec![main, issue];
        app.ensure_test_terminals();
        app.reconcile_sidebar_presentation();

        let rows = sidebar_rows(&app);
        assert_eq!(
            rows.iter()
                .map(|row| match row {
                    SidebarRow::Workspace { ws_idx, .. } => format!("space:{ws_idx}"),
                    SidebarRow::Tab { entry, .. } => {
                        format!("window:{}:{}", entry.ws_idx, entry.tab_idx)
                    }
                    SidebarRow::Agent { .. } => "agent".to_string(),
                    SidebarRow::SectionHeader { title, .. } => format!("section:{title}"),
                })
                .collect::<Vec<_>>(),
            vec!["space:0", "window:0:0", "window:0:1", "window:1:0"]
        );

        assert!(app.toggle_workspace_agent_disclosure(0));
        assert!(matches!(
            sidebar_rows(&app).as_slice(),
            [SidebarRow::Workspace { ws_idx: 0, .. }]
        ));
        assert!(app.toggle_workspace_agent_disclosure(0));
        assert_eq!(
            sidebar_rows(&app)
                .iter()
                .filter(|row| matches!(row, SidebarRow::Tab { .. }))
                .count(),
            3
        );
    }

    #[test]
    fn tab_background_jobs_sum_across_panes_without_adding_rows() {
        let mut app = app_with_agents(&["one"]);
        let second = app.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.ensure_test_terminals();
        let first = app.workspaces[0].tabs[0].root_pane;
        for (pane, count) in [(first, 1), (second, 2)] {
            let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            app.terminals
                .get_mut(&terminal_id)
                .unwrap()
                .background_job_count = Some(count);
        }

        let tabs = sidebar_rows(&app)
            .into_iter()
            .filter_map(|row| match row {
                SidebarRow::Tab { entry, .. } => Some(entry),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].background_job_count, Some(3));
    }

    #[test]
    fn prio_panel_aggregates_state_activity_and_jobs_across_tab_panes() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("one");
        let first_pane = workspace.tabs[0].root_pane;
        let second_pane = workspace.test_split(Direction::Horizontal);
        workspace.tabs[0].prio = true;
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.active = Some(0);

        let started = std::time::Instant::now();
        let blocked_at = started + std::time::Duration::from_secs(5);
        let first_terminal = app.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&first_terminal)
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
        app.terminals
            .get_mut(&first_terminal)
            .unwrap()
            .set_detected_state_with_screen_signals_at(
                Some(Agent::Pi),
                AgentState::Idle,
                false,
                true,
                false,
                false,
                started + std::time::Duration::from_secs(1),
            );
        app.terminals
            .get_mut(&first_terminal)
            .unwrap()
            .background_job_count = Some(1);
        app.workspaces[0].tabs[0]
            .panes
            .get_mut(&first_pane)
            .unwrap()
            .seen = true;

        let second_terminal = app.workspaces[0].tabs[0].panes[&second_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&second_terminal)
            .unwrap()
            .set_detected_state_with_screen_signals_at(
                Some(Agent::Pi),
                AgentState::Blocked,
                true,
                false,
                false,
                false,
                blocked_at,
            );
        app.terminals
            .get_mut(&second_terminal)
            .unwrap()
            .background_job_count = Some(2);
        app.view_observed_at = blocked_at + std::time::Duration::from_secs(60);
        app.reconcile_sidebar_presentation();

        let sidebar_tab = sidebar_rows(&app)
            .into_iter()
            .find_map(|row| match row {
                SidebarRow::Tab { entry, .. } => Some(entry),
                _ => None,
            })
            .expect("sidebar tab row");
        let prio_tab = prio_panel_entries(&app, None)
            .into_iter()
            .find(|entry| entry.ws_idx == 0 && entry.tab_idx == 0)
            .expect("PRIO tab row");

        assert_eq!(prio_tab.state, AgentState::Blocked);
        assert_eq!(prio_tab.activity_at, sidebar_tab.activity_at);
        assert_eq!(
            prio_tab.background_job_count,
            sidebar_tab.background_job_count
        );
        assert_eq!(prio_tab.background_job_count, Some(3));
        assert_eq!(
            tab_row_layout(
                &prio_tab,
                app.view_observed_at,
                80,
                1,
                &app.palette,
                app.status_indicators
            )
            .activity_age,
            tab_row_layout(
                &sidebar_tab,
                app.view_observed_at,
                80,
                1,
                &app.palette,
                app.status_indicators
            )
            .activity_age
        );
    }

    #[test]
    fn tab_background_job_badge_renders_immediately_after_title() {
        let mut app = app_with_agents(&["one"]);
        app.workspaces[0].tabs[0].custom_name = Some("Use Repository Instructions".into());
        let pane = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .background_job_count = Some(2);
        app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Codex);
        app.reconcile_sidebar_presentation();

        let area = Rect::new(0, 0, 60, 10);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let row = compute_tab_card_areas(&app, area)[0].rect.y;
        let rendered = row_text(terminal.backend().buffer(), row, area.width - 1);

        assert!(
            rendered.contains("Use Repository Instructions · cx  2 >_"),
            "{rendered:?}"
        );
    }

    #[test]
    fn tab_provider_suffixes_distinguish_codex_and_claude_after_title() {
        let mut app = app_with_agents(&["one"]);
        app.workspaces[0].tabs[0].custom_name = Some("Codex task".into());
        let codex_pane = app.workspaces[0].tabs[0].root_pane;
        let codex_terminal = app.workspaces[0].tabs[0].panes[&codex_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&codex_terminal)
            .unwrap()
            .detected_agent = Some(Agent::Codex);
        let claude_tab = app.workspaces[0].test_add_tab(Some("Claude task"));
        app.ensure_test_terminals();
        let claude_pane = app.workspaces[0].tabs[claude_tab].root_pane;
        let claude_terminal = app.workspaces[0].tabs[claude_tab].panes[&claude_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&claude_terminal)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        app.reconcile_sidebar_presentation();

        let area = Rect::new(0, 0, 34, 10);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let rendered = (0..area.height)
            .map(|row| row_text(terminal.backend().buffer(), row, area.width - 1))
            .collect::<Vec<_>>();

        assert!(
            rendered.iter().any(|row| row.contains("Codex task · cx")),
            "{rendered:?}"
        );
        assert!(
            rendered.iter().any(|row| row.contains("Claude task · cc")),
            "{rendered:?}"
        );
    }

    #[test]
    fn mixed_provider_tab_omits_provider_suffix() {
        let mut app = app_with_agents(&["one"]);
        app.workspaces[0].tabs[0].custom_name = Some("Mixed task".into());
        let second = app.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.ensure_test_terminals();
        let first = app.workspaces[0].tabs[0].root_pane;
        for (pane, agent) in [(first, Agent::Codex), (second, Agent::Claude)] {
            let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(agent);
        }
        app.reconcile_sidebar_presentation();

        let entry = sidebar_rows(&app)
            .into_iter()
            .find_map(|row| match row {
                SidebarRow::Tab { entry, .. } => Some(entry),
                _ => None,
            })
            .unwrap();
        assert_eq!(entry.agent, None);
        assert!(
            tab_lifecycle_visible(&entry),
            "mixed provider ambiguity must not hide agent lifecycle"
        );
    }

    #[test]
    fn tab_rows_follow_field_priority_at_minimum_and_normal_widths() {
        let started = std::time::Instant::now();
        let mut app = app_with_agents(&["one"]);
        app.workspaces[0].tabs[0].custom_name = Some("Investigate release regression".into());
        let pane = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        let terminal_state = app.terminals.get_mut(&terminal_id).unwrap();
        terminal_state.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            true,
            false,
            started,
        );
        terminal_state.background_job_count = Some(2);
        app.view_observed_at = started + std::time::Duration::from_secs(65);
        app.reconcile_sidebar_presentation();

        for width in [18, 38] {
            let area = Rect::new(0, 0, width, 10);
            let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
            terminal
                .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
                .unwrap();
            let row = compute_tab_card_areas(&app, area)[0].rect.y;
            let rendered = row_text(terminal.backend().buffer(), row, area.width - 1);

            assert!(rendered.contains("●"), "{width}: {rendered:?}");
            assert!(rendered.contains(" · cx"), "{width}: {rendered:?}");
            let dot = rendered.find('●').unwrap();
            let suffix = rendered.find(" · cx").unwrap();
            assert!(suffix > dot + '●'.len_utf8() + 1, "{width}: {rendered:?}");
            if width == 18 {
                assert!(!rendered.contains("working"), "{rendered:?}");
                assert!(!rendered.contains(">_"), "{rendered:?}");
                assert!(!rendered.contains("ago"), "{rendered:?}");
            } else {
                assert!(rendered.contains("working"), "{rendered:?}");
                assert!(rendered.contains("· cx  2 >_"), "{rendered:?}");
                // The always-reserved prio cell costs two columns, so at 38 the activity age is
                // the field that yields; the status label, title, provider and job count stay.
                assert!(!rendered.contains("ago"), "{rendered:?}");
            }
        }
    }

    #[test]
    fn pi_uses_pi_suffix_while_unsupported_and_agentless_tabs_omit_it() {
        assert_eq!(tab_agent_suffix(Some(Agent::Pi)), Some("pi"));
        assert_eq!(tab_agent_suffix(Some(Agent::Gemini)), None);
        assert_eq!(tab_agent_suffix(None), None);
    }

    #[test]
    fn unseen_agentless_tab_omits_lifecycle_status() {
        let mut app = app_with_agents(&["one"]);
        app.workspaces[0].test_add_tab(Some("Agentless window"));
        app.ensure_test_terminals();
        app.reconcile_sidebar_presentation();

        let area = Rect::new(0, 0, 50, 10);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let rendered = (0..area.height)
            .map(|row| row_text(terminal.backend().buffer(), row, area.width - 1))
            .find(|row| row.contains("Agentless window"))
            .expect("agentless tab row");

        for lifecycle in ["idle", "done", "working", "blocked", "unknown"] {
            assert!(!rendered.contains(lifecycle), "{rendered:?}");
        }
    }

    #[test]
    fn completed_agent_process_exit_retains_done_without_provider_suffix() {
        let started = std::time::Instant::now();
        let mut app = AppState::test_new();
        let workspace = Workspace::test_new("one");
        let pane = workspace.tabs[0].root_pane;
        let terminal_id = workspace.tabs[0].panes[&pane].attached_terminal_id.clone();
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.active = Some(0);
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            true,
            false,
            started,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Idle,
            false,
            true,
            false,
            true,
            started + std::time::Duration::from_secs(5),
        );
        app.workspaces[0].tabs[0].panes.get_mut(&pane).unwrap().seen = false;
        app.reconcile_sidebar_presentation();

        let entry = sidebar_rows(&app)
            .into_iter()
            .find_map(|row| match row {
                SidebarRow::Tab { entry, .. } => Some(entry),
                _ => None,
            })
            .unwrap();
        assert_eq!(entry.agent, None);
        assert!(entry.has_agent);
        assert!(tab_lifecycle_visible(&entry));
        assert_eq!(agent_panel_status_key(entry.state, entry.seen), "done");
    }

    #[test]
    fn exited_claude_plus_live_codex_omits_provider_suffix() {
        let started = std::time::Instant::now();
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("one");
        let exited_pane = workspace.tabs[0].root_pane;
        let live_pane = workspace.test_split(Direction::Horizontal);
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.active = Some(0);

        let exited_terminal = app.workspaces[0].tabs[0].panes[&exited_pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&exited_terminal).unwrap();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            started,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            true,
            started + std::time::Duration::from_secs(5),
        );
        let live_terminal = app.workspaces[0].tabs[0].panes[&live_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&live_terminal)
            .unwrap()
            .set_detected_state_with_screen_signals_at(
                Some(Agent::Codex),
                AgentState::Working,
                false,
                false,
                true,
                false,
                started + std::time::Duration::from_secs(6),
            );
        app.reconcile_sidebar_presentation();

        let entry = sidebar_rows(&app)
            .into_iter()
            .find_map(|row| match row {
                SidebarRow::Tab { entry, .. } => Some(entry),
                _ => None,
            })
            .unwrap();
        assert_eq!(entry.agent, None);
        assert!(tab_lifecycle_visible(&entry));
    }

    #[test]
    fn desktop_worktree_group_has_no_intermediate_connector_rows() {
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
        assert_eq!(app.view.workspace_card_areas.len(), 1);
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

        let rendered = row_text(terminal.backend().buffer(), 1, area.width);
        assert!(!rendered.contains('├'), "{rendered:?}");
        assert!(!rendered.contains('└'), "{rendered:?}");
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
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].ws_idx, 0);
        assert!(!cards[0].indented);
    }

    #[test]
    fn space_row_gap_separates_flattened_groups() {
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
        assert_eq!(spacious.len(), 2);
        assert_eq!(
            spacious[1].rect.y,
            spacious[0].rect.y + spacious[0].rect.height + 2
        );
        let spacious_metrics = workspace_list_scroll_metrics(&app, Rect::new(0, 0, 30, 5));
        assert_eq!(spacious_metrics.viewport_rows, 2);
        assert_eq!(spacious_metrics.max_offset_from_bottom, 0);

        app.sidebar_spaces.row_gap = 0;
        let (packed, _) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 30));
        assert!(packed
            .windows(2)
            .all(|pair| pair[1].rect.y == pair[0].rect.y + pair[0].rect.height));
        let packed_metrics = workspace_list_scroll_metrics(&app, Rect::new(0, 0, 30, 5));
        assert_eq!(packed_metrics.viewport_rows, 2);
        assert_eq!(packed_metrics.max_offset_from_bottom, 0);
    }

    #[test]
    fn space_row_gap_separates_groups_but_never_tabs_inside_them() {
        let mut app = AppState::test_new();
        let mut first = Workspace::test_new("first");
        first.test_add_tab(Some("first-two"));
        let mut second = Workspace::test_new("second");
        second.test_add_tab(Some("second-two"));
        app.workspaces = vec![first, second];
        app.ensure_test_terminals();
        app.reconcile_sidebar_presentation();
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_spaces.row_gap = 1;
        app.sidebar_agents.row_gap = 3;

        let area = Rect::new(0, 0, 30, 30);
        let (spaces, _) = compute_workspace_list_areas(&app, area);
        let tabs = compute_tab_card_areas(&app, area);

        // ac3: legacy Agent row spacing cannot split tabs inside one Space.
        assert_eq!(tabs[1].rect.y, tabs[0].rect.y + tabs[0].rect.height);
        assert_eq!(spaces[1].rect.y, tabs[1].rect.y + tabs[1].rect.height + 1);
        assert_eq!(tabs[3].rect.y, tabs[2].rect.y + tabs[2].rect.height);
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
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].ws_idx, 0);
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

        let (cards, headers) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 4));

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

    #[test]
    fn ac2_work_title_and_manual_label_drive_tab_row_without_losing_lifecycle() {
        let mut app = app_with_agents(&["one"]);
        let pane = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].terminal_id(pane).cloned().unwrap();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(Agent::Codex);
        terminal.state = AgentState::Working;
        terminal
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                work_title: Some("Codex".into()),
                ..Default::default()
            })
            .unwrap();

        let entry = sidebar_thread_entries(&app).remove(0);
        let layout = tab_row_layout(
            &entry,
            std::time::Instant::now(),
            40,
            2,
            &app.palette,
            app.status_indicators,
        );
        assert_eq!(layout.title, "Codex");
        assert_eq!(layout.state.as_deref(), Some("working"));
        // The derived projection already leads with the agent, so no provider chip.
        assert_eq!(layout.agent_suffix.as_deref(), None);

        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                work_title: Some("repair login".into()),
                ..Default::default()
            })
            .unwrap();
        terminal.set_manual_label("manual pane".into());
        let entry = sidebar_thread_entries(&app).remove(0);
        let layout = tab_row_layout(
            &entry,
            std::time::Instant::now(),
            40,
            2,
            &app.palette,
            app.status_indicators,
        );
        assert_eq!(layout.title, "manual pane");
        assert_eq!(layout.state.as_deref(), Some("working"));
        // The label no longer names the agent, so the provider chip returns.
        assert_eq!(layout.agent_suffix.as_deref(), Some(" · cx"));
    }
}
