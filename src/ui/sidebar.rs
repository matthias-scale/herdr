#[cfg(test)]
mod tokens;

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

#[cfg(test)]
use self::tokens::{ResolvedToken, ResolvedTokenKind};
use super::scrollbar::{render_scrollbar, should_show_scrollbar};
use super::status::state_label_color;
use super::status::status_report_age_compact_label;
use super::text::{display_width, display_width_u16, truncate_end};
use crate::app::state::{Palette, SidebarGroupMode};
use crate::app::{AppState, Mode};
use crate::config::StatusIndicatorStyle;
use crate::detect::{Agent, AgentState};
use crate::terminal::state::derive_completion_tier;
use crate::terminal::state::CompletionTier;
use crate::terminal::TerminalRuntimeRegistry;
use crate::ui::work_list_detail::{PrAction, PrActionKind, PrActionPlacement, PrItem};
use crate::ui::work_status::WorkGroupStatus;

const WORKSPACE_SECTION_HEADER_ROWS: u16 = 2;
const MIN_WORKSPACE_LIST_ROWS: u16 = 3;
#[cfg(test)]
const TAB_ACTIVITY_AGE_MIN_TITLE_WIDTH: usize = 3;
pub(super) const DEFAULT_THREAD_TITLE: &str = "New Thread";
#[cfg(test)]
const ACTIVE_SUBAGENT_GLYPH: &str = "+";
const SIDEBAR_SPACE_SUFFIX_MIN_ROW_WIDTH: usize = 44;
const SIDEBAR_SPACE_SUFFIX_MIN_TITLE_WIDTH: usize = 16;

/// Focus-star suffix drawn immediately after a starred session's title. Kept to
/// two display columns (space + glyph) so it costs the title field almost
/// nothing at narrow sidebar widths.
pub(crate) const SIDEBAR_STAR_SUFFIX: &str = " \u{2605}";
/// Below this the title field is too short to give up columns to the star, so a
/// starred row simply renders without it rather than truncating the name.
const SIDEBAR_STAR_MIN_TITLE_WIDTH: usize = 6;

pub(crate) fn sidebar_separator_col(area: Rect) -> Option<u16> {
    (area.width > 0).then(|| area.x + area.width.saturating_sub(1))
}

pub(crate) fn tab_agent_suffix(agent: Option<Agent>) -> Option<&'static str> {
    match agent {
        Some(Agent::Codex) => Some("cx"),
        Some(Agent::Claude) => Some("cc"),
        Some(Agent::Pi) => Some("pi"),
        Some(Agent::Kimi) => Some("ki"),
        _ => None,
    }
}

#[cfg(test)]
pub(super) fn title_repeats_agent_identity(entry: &AgentPanelEntry, title: &str) -> bool {
    let title = title.trim().to_ascii_lowercase();
    let provider = match entry.agent {
        Some(Agent::Codex) => Some("codex"),
        Some(Agent::Claude) => Some("claude"),
        Some(Agent::Pi) => Some("pi"),
        Some(Agent::Kimi) => Some("kimi"),
        _ => None,
    };
    entry
        .agent_label
        .as_deref()
        .into_iter()
        .chain(entry.agent_kind_label.as_deref())
        .chain(provider)
        .chain(tab_agent_suffix(entry.agent))
        .any(|identity| identity.trim().eq_ignore_ascii_case(&title))
}

#[cfg(test)]
pub(super) fn canonical_sidebar_agent_identity(entry: &AgentPanelEntry) -> Option<&str> {
    tab_agent_suffix(entry.agent)
        .or(entry.agent_kind_label.as_deref())
        .or_else(|| {
            entry
                .agent
                .is_none()
                .then_some(entry.agent_label.as_deref())
                .flatten()
        })
}

#[cfg(test)]
pub(super) fn compact_agent_identity<'a>(
    entry: &'a AgentPanelEntry,
    title: &str,
) -> Option<&'a str> {
    if title_repeats_agent_identity(entry, title) {
        return None;
    }
    canonical_sidebar_agent_identity(entry)
}

#[cfg(test)]
pub(super) fn tab_lifecycle_visible(entry: &AgentPanelEntry) -> bool {
    entry.has_agent
        && (entry.open_blockers
            || entry.usage_limited
            || entry.stale
            || entry.state != AgentState::Idle
            || !entry.seen)
}

/// Membership in the Blocked worklist.
///
/// The terminal predicate is the single rule for this section and the inbox.
/// A human gate blocks only after the pane stops working. Usage limits remain
/// blocked because the pane cannot proceed until the reset window.
#[cfg(test)]
pub(crate) fn entry_is_blocked(entry: &AgentPanelEntry) -> bool {
    crate::terminal::counts_as_blocked(entry.state, entry.open_blockers, entry.usage_limited)
}

pub(crate) fn entry_has_red_dot(entry: &AgentPanelEntry) -> bool {
    entry.state == AgentState::Blocked || entry.usage_limited || entry_has_gate(entry)
}

/// A working pane keeps its blue lifecycle label while a human gate is latched.
/// Once work stops, the gate becomes blocking and supplies the blocked label.
/// Usage limits still override every lifecycle label.
#[cfg(test)]
pub(super) fn gate_overrides_label(entry: &AgentPanelEntry) -> bool {
    entry.usage_limited
        || (entry.open_blockers
            && !entry.stale
            && entry.state != AgentState::Blocked
            && entry.state != AgentState::Working)
}

/// A usage limit outranks every other label: the pane is not working, and no
/// answer from the human releases it — only the reset window does.
#[cfg(test)]
pub(super) fn usage_limit_label() -> &'static str {
    "usage"
}

#[cfg(test)]
pub(super) fn gate_override_label(entry: &AgentPanelEntry) -> String {
    if entry.usage_limited {
        return entry
            .state_labels
            .get("usage")
            .cloned()
            .unwrap_or_else(|| usage_limit_label().to_string());
    }
    entry
        .state_labels
        .get("blocked")
        .cloned()
        .unwrap_or_else(|| "blocked".to_string())
}

#[cfg(test)]
pub(super) fn agent_panel_label_color(
    entry: &AgentPanelEntry,
    p: &Palette,
) -> ratatui::style::Color {
    if gate_overrides_label(entry) {
        return p.red;
    }
    state_label_color(entry.state, entry.seen, p)
}

pub(super) struct TabRowLayout {
    pub dot: String,
    pub title: String,
    pub provider: String,
    pub activity_age: Option<String>,
    pub activity_instant: Option<std::time::Instant>,
}

const SIDEBAR_DOT_FIELD_WIDTH: usize = 3;
const SIDEBAR_PROVIDER_GAP_WIDTH: usize = 1;
const SIDEBAR_AGE_FIELD_WIDTH: usize = 4;
const SIDEBAR_MIN_NESTED_TITLE_WIDTH: usize = 8;
const SIDEBAR_MIN_NESTED_PREFIX_WIDTH: usize = 3;
const SIDEBAR_TITLE_TARGET_WIDTH: usize = 16;

fn entry_has_gate(entry: &AgentPanelEntry) -> bool {
    entry.gate_count > 0 || entry.open_blockers
}

fn compact_row_dot(entry: &AgentPanelEntry) -> &'static str {
    compact_dot_for_state(
        entry.state,
        entry.seen,
        entry.has_agent,
        entry.state == AgentState::Working && entry_has_gate(entry),
        entry.usage_limited,
    )
}

fn compact_row_dot_text(entry: &AgentPanelEntry) -> String {
    // Red already says the row owes you an answer; how many gates are open does
    // not change what you do next, so the count is not rendered.
    compact_row_dot(entry).to_string()
}

pub(crate) fn compact_dot_for_state(
    state: AgentState,
    // Seen no longer selects a shape: done-unread and idle-seen are both `○`,
    // separated by colour via state_label_color.
    _seen: bool,
    has_agent: bool,
    _gate: bool,
    _usage_limited: bool,
) -> &'static str {
    if !has_agent {
        return "·";
    }
    match state {
        AgentState::Working => "●",
        AgentState::Blocked => "○",
        AgentState::Idle if has_agent => "○",
        _ => "·",
    }
}

fn compact_provider(entry: &AgentPanelEntry) -> String {
    if !entry.has_agent {
        return ">_".to_string();
    }
    let Some(agent) = entry.agent.or(entry.agent_context) else {
        return ">_".to_string();
    };
    let Some(suffix) = tab_agent_suffix(Some(agent)) else {
        return if entry.holds_shell {
            ">_".to_string()
        } else {
            String::new()
        };
    };
    let mut provider = suffix.to_string();
    if !entry.stale {
        if let Some(count) = entry.active_subagents.filter(|count| *count > 0) {
            provider.push_str(&format!("+{count}"));
        }
    }
    if entry.holds_shell {
        provider.push_str(" >_");
    }
    provider
}

fn compact_age(
    entry: &AgentPanelEntry,
    now: std::time::Instant,
) -> (String, Option<std::time::Instant>) {
    let instant = entry.reported_at.or(entry.activity_at);
    let age = instant
        .and_then(|instant| status_report_age_compact_label(Some(instant), now))
        .unwrap_or_else(|| "—".to_string());
    (age, instant)
}

fn compact_row_title(entry: &AgentPanelEntry, tab: bool) -> &str {
    let candidate = if tab {
        entry.primary_tab_label.as_deref()
    } else if entry.pane_label_is_agent_identity {
        entry
            .primary_tab_label
            .as_deref()
            .or(entry.terminal_title_stripped.as_deref())
    } else {
        entry
            .pane_label
            .as_deref()
            .or(entry.terminal_title_stripped.as_deref())
    };
    let candidate = if (entry.tab_label_leads_with_agent && !entry.tab_has_custom_name) || !tab {
        compact_title_candidate(candidate)
    } else {
        candidate.map(str::trim).filter(|title| {
            !title.is_empty()
                && !title.eq_ignore_ascii_case(DEFAULT_THREAD_TITLE)
                && !title.eq_ignore_ascii_case("Terminal")
        })
    };
    candidate.unwrap_or(DEFAULT_THREAD_TITLE)
}

fn title_without_object_identifier(title: &str) -> Option<&str> {
    let (identifier, title) = title.split_once(" · ")?;
    let github_identifier = identifier
        .strip_prefix('#')
        .is_some_and(|number| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()));
    let linear_identifier = identifier.split_once('-').is_some_and(|(team, number)| {
        !team.is_empty()
            && team.chars().all(|c| c.is_ascii_alphabetic())
            && !number.is_empty()
            && number.chars().all(|c| c.is_ascii_digit())
    });
    (github_identifier || linear_identifier).then_some(title)
}

fn compact_row_title_for_width<'a>(
    title: &'a str,
    provider: &str,
    width: usize,
    requested_prefix: usize,
) -> &'a str {
    if width >= SIDEBAR_SPACE_SUFFIX_MIN_ROW_WIDTH {
        return title;
    }
    let Some(title_only) = title_without_object_identifier(title) else {
        return title;
    };
    let widths = compact_row_widths(title, provider, width, requested_prefix);
    let fixed_width = requested_prefix + SIDEBAR_DOT_FIELD_WIDTH + widths.provider + widths.age;
    if display_width(title) <= width.saturating_sub(fixed_width) {
        title
    } else {
        title_only
    }
}

fn compact_title_candidate(title: Option<&str>) -> Option<&str> {
    let title = title?.trim();
    if title.is_empty()
        || title.eq_ignore_ascii_case(DEFAULT_THREAD_TITLE)
        || title.eq_ignore_ascii_case("Terminal")
        || (title.contains('.')
            && title
                .chars()
                .all(|character| character.is_ascii_digit() || character == '.'))
    {
        None
    } else {
        Some(title)
    }
}

fn compact_row_layout(
    entry: &AgentPanelEntry,
    now: std::time::Instant,
    width: usize,
    prefix_width: usize,
    tab: bool,
) -> TabRowLayout {
    let (age, activity_instant) = compact_age(entry, now);
    let provider = compact_provider(entry);
    let title = compact_row_title_for_width(
        compact_row_title(entry, tab),
        &provider,
        width,
        prefix_width,
    );
    let widths = compact_row_widths(title, &provider, width, prefix_width);
    let fixed_width = widths.prefix + SIDEBAR_DOT_FIELD_WIDTH + widths.provider + widths.age;
    TabRowLayout {
        dot: compact_row_dot_text(entry),
        title: truncate_end(title, width.saturating_sub(fixed_width)),
        provider,
        activity_age: (widths.age > 0).then_some(age),
        activity_instant: (widths.age > 0).then_some(activity_instant).flatten(),
    }
}

fn compact_provider_field_width(provider: &str) -> usize {
    if provider.is_empty() {
        0
    } else {
        display_width(provider) + SIDEBAR_PROVIDER_GAP_WIDTH
    }
}

struct CompactRowWidths {
    prefix: usize,
    provider: usize,
    age: usize,
}

fn compact_row_widths(
    title: &str,
    provider: &str,
    width: usize,
    requested_prefix: usize,
) -> CompactRowWidths {
    let provider = compact_provider_field_width(provider);
    let title_width = display_width(title);
    let readable_title_width = title_width.min(SIDEBAR_MIN_NESTED_TITLE_WIDTH);
    let minimum_prefix_width = requested_prefix.min(SIDEBAR_MIN_NESTED_PREFIX_WIDTH);
    let age = if width
        >= SIDEBAR_DOT_FIELD_WIDTH
            + provider
            + SIDEBAR_AGE_FIELD_WIDTH
            + readable_title_width
            + minimum_prefix_width
    {
        SIDEBAR_AGE_FIELD_WIDTH
    } else {
        0
    };
    let target_title_width = title_width.min(SIDEBAR_TITLE_TARGET_WIDTH);
    let prefix_budget = width
        .saturating_sub(SIDEBAR_DOT_FIELD_WIDTH + provider + age)
        .saturating_sub(target_title_width);
    let preserve_nested_prefix = requested_prefix > 0
        && width
            >= SIDEBAR_DOT_FIELD_WIDTH
                + provider
                + age
                + readable_title_width
                + minimum_prefix_width;
    let prefix = requested_prefix.min(if preserve_nested_prefix {
        prefix_budget.max(minimum_prefix_width)
    } else {
        prefix_budget
    });
    CompactRowWidths {
        prefix,
        provider,
        age,
    }
}

fn compact_row_color(entry: &AgentPanelEntry, p: &Palette) -> Color {
    if entry_has_gate(entry) || entry.usage_limited {
        return p.red;
    }
    // A session that declared a contract and reported it met is the one kind of
    // done you can act on without reading the pane: close it. That earns its own
    // colour rather than a fourth dot shape.
    if entry.completion_tier == Some(CompletionTier::ContractSatisfied) {
        return p.mauve;
    }
    state_label_color(entry.state, entry.seen, p)
}

fn provider_color(entry: &AgentPanelEntry, p: &Palette) -> Color {
    match entry.agent.or(entry.agent_context) {
        Some(Agent::Claude) => p.peach,
        Some(Agent::Codex) => p.green,
        Some(Agent::Pi) => p.mauve,
        Some(Agent::Kimi) => p.yellow,
        _ => p.overlay0,
    }
}

fn pad_right(text: &str, width: usize) -> String {
    format!(
        "{text}{}",
        " ".repeat(width.saturating_sub(display_width(text)))
    )
}

fn pad_left(text: &str, width: usize) -> String {
    format!(
        "{}{}",
        " ".repeat(width.saturating_sub(display_width(text))),
        text
    )
}

fn compact_row_style(style: Style, bg: Option<Color>) -> Style {
    bg.map_or(style, |bg| style.bg(bg))
}

fn superscript(index: usize) -> String {
    index
        .to_string()
        .chars()
        .map(|digit| match digit {
            '0' => '⁰',
            '1' => '¹',
            '2' => '²',
            '3' => '³',
            '4' => '⁴',
            '5' => '⁵',
            '6' => '⁶',
            '7' => '⁷',
            '8' => '⁸',
            '9' => '⁹',
            _ => digit,
        })
        .collect()
}

pub(super) fn sidebar_workspace_labels(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> std::collections::HashMap<usize, (String, bool)> {
    let mut labels = std::collections::HashMap::new();
    for (ws_idx, ws) in app.workspaces.iter().enumerate() {
        let members = sidebar_space_member_indices(app, ws_idx);
        // Members of a worktree group share one checkout identity, so a name any
        // of them carries names the group. Repository groups collect Spaces that
        // only share a repo, and one member's rename must not retitle the row.
        let manual = if ws.worktree_space().is_some() {
            members
                .iter()
                .filter_map(|member| app.workspaces.get(*member))
                .find_map(|member| member.custom_name.clone())
        } else {
            ws.custom_name.clone()
        };
        let label = manual.map(|label| (label, false)).unwrap_or_else(|| {
            (
                ws.display_name_from(&app.terminals, terminal_runtimes),
                true,
            )
        });
        labels.insert(ws_idx, label);
    }
    let mut duplicate_positions = std::collections::HashMap::<String, usize>::new();
    let mut duplicate_counts = std::collections::HashMap::<String, usize>::new();
    for (label, _) in labels.values() {
        *duplicate_counts.entry(label.clone()).or_default() += 1;
    }
    for ws_idx in 0..app.workspaces.len() {
        let Some((label, _derived)) = labels.get_mut(&ws_idx) else {
            continue;
        };
        if duplicate_counts.get(label).copied().unwrap_or_default() > 1 {
            let position = duplicate_positions.entry(label.clone()).or_default();
            *position += 1;
            label.push_str(&superscript(*position));
        }
    }
    labels
}

pub(super) fn render_compact_agent_row(
    app: &AppState,
    frame: &mut Frame,
    entry: &AgentPanelEntry,
    rect: Rect,
    depth: u16,
    tab: bool,
    bg: Option<Color>,
) {
    render_compact_agent_row_with_prefix(app, frame, entry, rect, depth, tab, bg, None);
}

fn render_compact_agent_row_with_prefix(
    app: &AppState,
    frame: &mut Frame,
    entry: &AgentPanelEntry,
    rect: Rect,
    depth: u16,
    tab: bool,
    bg: Option<Color>,
    prefix_override: Option<usize>,
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let p = &app.palette;
    let requested_prefix_width = prefix_override.unwrap_or_else(|| usize::from(depth) * 3 + 1);
    let provider = compact_provider(entry);
    let row_title = compact_row_title_for_width(
        compact_row_title(entry, tab),
        &provider,
        usize::from(rect.width),
        requested_prefix_width,
    );
    let widths = compact_row_widths(
        row_title,
        &provider,
        usize::from(rect.width),
        requested_prefix_width,
    );
    let prefix = " ".repeat(widths.prefix);
    let layout = compact_row_layout(
        entry,
        app.view_observed_at,
        usize::from(rect.width),
        widths.prefix,
        tab,
    );
    let fixed_width = widths.prefix + SIDEBAR_DOT_FIELD_WIDTH + widths.provider + widths.age;
    let title_width = usize::from(rect.width).saturating_sub(fixed_width);
    let trailing_tag = tab
        .then_some(entry.space_label.as_str())
        .filter(|_| !entry.space_label_redundant)
        .filter(|tag| !tag.is_empty());
    let mut space_suffix = None;
    let mut displayed_title_width = title_width;
    if let Some(tag) = trailing_tag {
        if usize::from(rect.width) >= SIDEBAR_SPACE_SUFFIX_MIN_ROW_WIDTH {
            let suffix = format!(" · {tag}");
            let candidate_title_width = title_width.saturating_sub(display_width(&suffix));
            if candidate_title_width >= SIDEBAR_SPACE_SUFFIX_MIN_TITLE_WIDTH {
                space_suffix = Some(suffix);
                displayed_title_width = candidate_title_width;
            }
        }
    }
    // The star sits inside the title field, right after the name, so it reads as
    // part of the session's label rather than as another right-hand column.
    let star_suffix = (entry.starred
        && displayed_title_width
            >= SIDEBAR_STAR_MIN_TITLE_WIDTH + display_width(SIDEBAR_STAR_SUFFIX))
    .then_some(SIDEBAR_STAR_SUFFIX);
    let title_text_width =
        displayed_title_width.saturating_sub(star_suffix.map_or(0, display_width));
    let title_text = truncate_end(&layout.title, title_text_width);
    let title_pad = " ".repeat(title_text_width.saturating_sub(display_width(&title_text)));
    let dot = pad_right(&layout.dot, SIDEBAR_DOT_FIELD_WIDTH);
    let provider = pad_left(&layout.provider, widths.provider);
    let age = layout
        .activity_age
        .as_deref()
        .map_or_else(String::new, |age| pad_left(age, widths.age));
    let is_active = tab
        && app.active == Some(entry.ws_idx)
        && app
            .workspaces
            .get(entry.ws_idx)
            .is_some_and(|ws| ws.active_tab_index() == entry.tab_idx);
    let title_style = if is_active || app.is_active_pane(entry.ws_idx, entry.tab_idx, entry.pane_id)
    {
        Style::default()
            .fg(active_sidebar_title_color(p))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.subtext0)
    };
    let dot_style = Style::default().fg(compact_row_color(entry, p));
    let provider_style = Style::default()
        .fg(provider_color(entry, p))
        .add_modifier(Modifier::DIM);
    let age_style = Style::default().fg(if entry.state == AgentState::Working {
        p.blue
    } else {
        p.overlay0
    });
    let mut spans = vec![
        Span::styled(prefix, compact_row_style(Style::default(), bg)),
        Span::styled(dot, compact_row_style(dot_style, bg)),
        Span::styled(title_text, compact_row_style(title_style, bg)),
    ];
    if let Some(star) = star_suffix {
        spans.push(Span::styled(
            star,
            compact_row_style(Style::default().fg(p.yellow), bg),
        ));
    }
    spans.extend([
        Span::styled(title_pad, compact_row_style(title_style, bg)),
        Span::styled(provider, compact_row_style(provider_style, bg)),
        Span::styled(age, compact_row_style(age_style, bg)),
    ]);
    if let Some(suffix) = space_suffix.as_deref() {
        spans.push(Span::styled(
            suffix,
            compact_row_style(
                Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
                bg,
            ),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
}

pub(super) fn tab_row_layout(
    entry: &AgentPanelEntry,
    now: std::time::Instant,
    width: usize,
    prefix_width: usize,
    palette: &Palette,
    indicator_style: StatusIndicatorStyle,
) -> TabRowLayout {
    let _ = (palette, indicator_style);
    compact_row_layout(entry, now, width, prefix_width, true)
}

pub(super) fn mobile_tab_row_layout(
    entry: &AgentPanelEntry,
    now: std::time::Instant,
    width: usize,
    prefix_width: usize,
    palette: &Palette,
    indicator_style: StatusIndicatorStyle,
) -> TabRowLayout {
    let _ = (palette, indicator_style);
    compact_row_layout(entry, now, width, prefix_width, true)
}

/// Foreground for the selected Space and the current tab title in the sidebar.
///
/// Emphasis is a step *away* from the panel background, so the same rule reads
/// as emphasis in both appearances: one third darker than the authored text on
/// a light panel, one third of the way to white on a dark one. A single
/// darkening rule (herdr #17, which dropped the luminance guard added by #16)
/// made the selected row the dimmest text in every dark theme -- under
/// `github-dark-high-contrast` it rendered at `Rgb(160, 162, 164)`, below the
/// `Rgb(189, 196, 204)` of the unselected rows around it.
pub(crate) fn active_sidebar_title_color(palette: &Palette) -> Color {
    let Color::Rgb(bg_r, bg_g, bg_b) = palette.panel_bg else {
        return palette.text;
    };
    // Rec. 601 luma, scaled by 1000 to stay in integer arithmetic.
    let panel_luma = u32::from(bg_r) * 299 + u32::from(bg_g) * 587 + u32::from(bg_b) * 114;
    let panel_is_dark = panel_luma < 128_000;
    match palette.text {
        Color::Rgb(r, g, b) => {
            let step = |channel: u8| -> u8 {
                if panel_is_dark {
                    channel + ((255 - channel) / 3)
                } else {
                    ((u16::from(channel) * 2) / 3) as u8
                }
            };
            Color::Rgb(step(r), step(g), step(b))
        }
        color => color,
    }
}

#[derive(Clone)]
#[allow(dead_code)]
pub(crate) struct AgentPanelEntry {
    pub ws_idx: usize,
    pub tab_idx: usize,
    pub pane_id: crate::layout::PaneId,
    pub primary_label: String,
    /// Server-owned Space label. Grouping views render this in the existing
    /// trailing tag cell instead of deriving a label from the pane title.
    pub space_label: String,
    /// The enclosing group repeats this Space label, or the projection has no
    /// second Space label to distinguish. Disambiguated headers preserve the
    /// raw tag when another distinct Space label is present.
    pub space_label_redundant: bool,
    pub primary_tab_label: Option<String>,
    pub tab_has_custom_name: bool,
    pub tab_label_leads_with_agent: bool,
    pub pane_label: Option<String>,
    pub pane_label_is_agent_identity: bool,
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
    /// User-set focus star on the owning tab. Rendering only: it never
    /// reorders or regroups the row.
    pub starred: bool,
    pub state: AgentState,
    /// The last closing-block report still names at least one gate, even if
    /// the lifecycle state has moved on. It becomes a red blocker dot once the
    /// pane stops working.
    pub open_blockers: bool,
    pub completion_tier: Option<CompletionTier>,
    /// The pane's agent stopped on an exhausted plan usage/rate limit. Nobody
    /// can answer it, so it reads as its own red "usage" label rather than a
    /// gate or a lifecycle state.
    pub usage_limited: bool,
    /// Positive count from the current sub-agent source. Rendering depends on
    /// this field only so the source can change without changing row layout.
    pub active_subagents: Option<u32>,
    pub holds_shell: bool,
    pub gate_count: usize,
    pub seen: bool,
    pub done_since: Option<std::time::Instant>,
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

fn expanded_sidebar_content(area: Rect) -> Rect {
    Rect::new(
        area.x,
        area.y,
        area.width.saturating_sub(1),
        area.height.saturating_sub(1),
    )
}

fn sidebar_footer_slot(area: Rect, index: u16) -> Rect {
    let content_width = area.width.saturating_sub(1);
    let x_offset = 1 + index.saturating_mul(2);
    if content_width < x_offset.saturating_add(2) || area.height == 0 {
        return Rect::default();
    }
    Rect::new(
        area.x.saturating_add(x_offset),
        area.bottom().saturating_sub(1),
        2,
        1,
    )
}

pub(crate) fn sidebar_footer_settings_hit_area(area: Rect) -> Rect {
    sidebar_footer_slot(area, 0)
}

pub(crate) fn sidebar_footer_work_hit_area(area: Rect) -> Rect {
    sidebar_footer_slot(area, 1)
}

pub(crate) fn sidebar_footer_ticket_hit_area(area: Rect) -> Rect {
    sidebar_footer_slot(area, 3)
}

pub(crate) fn sidebar_footer_usage_hit_area(area: Rect) -> Rect {
    sidebar_footer_slot(area, 2)
}

pub(crate) fn sidebar_footer_missive_hit_area(area: Rect) -> Rect {
    sidebar_footer_slot(area, 4)
}

pub(crate) fn sidebar_footer_refresh_hit_area(area: Rect) -> Rect {
    sidebar_footer_slot(area, 5)
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

#[cfg(test)]
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
                    let space_label = workspace_label.clone();
                    let prio = ws.tabs.get(detail.tab_idx).is_some_and(|tab| tab.prio);
                    let starred = ws.tabs.get(detail.tab_idx).is_some_and(|tab| tab.starred);
                    let tab_has_custom_name = ws
                        .tabs
                        .get(detail.tab_idx)
                        .is_some_and(|tab| tab.custom_name.is_some());
                    let projection = ws.tab_display_projection(&app.terminals, detail.tab_idx);
                    let tab_label_leads_with_agent = projection
                        .as_ref()
                        .is_some_and(|projection| projection.leads_with_agent_component());
                    let thread_title = ws
                        .tab_display_name_from(&app.terminals, detail.tab_idx)
                        .or_else(|| Some(DEFAULT_THREAD_TITLE.to_string()));
                    // Prefer the live count; fall back to the reported token so
                    // panes without a live source keep a count.
                    let active_subagents = detail
                        .active_subagents
                        .or_else(|| {
                            detail
                                .tokens
                                .get("closing_agents")
                                .and_then(|value| value.parse::<u32>().ok())
                        })
                        .filter(|count| *count > 0);
                    let has_closing_block_tokens =
                        detail.tokens.keys().any(|key| key.starts_with("closing_"));
                    let completion_tier = derive_completion_tier(
                        detail.state,
                        detail.closing_contract.as_deref(),
                        detail.closing_contract_met,
                        detail.closing_idle,
                        detail.open_blockers,
                        active_subagents,
                        detail.holds_shell,
                        has_closing_block_tokens,
                    );
                    AgentPanelEntry {
                        ws_idx,
                        tab_idx: detail.tab_idx,
                        pane_id: detail.pane_id,
                        primary_label: workspace_label.clone(),
                        space_label: space_label.clone(),
                        space_label_redundant: false,
                        primary_tab_label: crate::workspace::session_title(
                            projection.as_ref(),
                            thread_title,
                        ),
                        tab_has_custom_name,
                        tab_label_leads_with_agent,
                        pane_label: detail.pane_label,
                        pane_label_is_agent_identity: detail.pane_label_is_agent_identity,
                        terminal_title: detail.terminal_title,
                        terminal_title_stripped: detail.terminal_title_stripped,
                        agent_label: Some(detail.agent_label),
                        agent_kind_label: detail.agent_kind_label,
                        agent: detail.agent,
                        foreground_process_name: detail.foreground_process_name,
                        agent_context: detail.agent_context,
                        has_agent: detail.has_agent,
                        prio,
                        starred,
                        state: detail.state,
                        open_blockers: detail.open_blockers,
                        completion_tier,
                        usage_limited: detail.usage_limited,
                        active_subagents,
                        seen: detail.seen,
                        done_since: detail.done_since,
                        stale: detail.stale,
                        reported_at: detail.reported_at,
                        last_agent_state_change_seq: detail.last_agent_state_change_seq,
                        activity_at: detail.activity_at,
                        state_labels: detail.state_labels,
                        tokens: detail.tokens,
                        holds_shell: detail.holds_shell,
                        gate_count: detail.gate_count,
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
        (
            AgentPanelEntry,
            bool,
            bool,
            bool,
            Option<String>,
            Option<String>,
        ),
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
                    usage_label,
                )| {
                    if usage_label.is_none() && entry.usage_limited {
                        *usage_label = entry.state_labels.get("usage").cloned();
                    }
                    if first_foreground_process_name.is_none() {
                        *first_foreground_process_name = entry.foreground_process_name.clone();
                    }
                    if tab_entry.foreground_process_name.is_none() {
                        tab_entry.foreground_process_name = first_foreground_process_name.clone();
                    }
                    if tab_lifecycle_priority(candidate.0, candidate.1)
                        > tab_lifecycle_priority(tab_entry.state, tab_entry.seen)
                    {
                        tab_entry.state = candidate.0;
                        tab_entry.seen = candidate.1;
                        tab_entry.stale = entry.stale;
                        tab_entry.completion_tier = entry.completion_tier;
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
                    tab_entry.holds_shell |= entry.holds_shell;
                    tab_entry.gate_count = tab_entry.gate_count.saturating_add(entry.gate_count);
                    tab_entry.active_subagents =
                        match (tab_entry.active_subagents, entry.active_subagents) {
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
                    tab_entry.open_blockers |= entry.open_blockers;
                    tab_entry.usage_limited |= entry.usage_limited;
                },
            )
            .or_insert_with(|| {
                (
                    entry.clone(),
                    false,
                    entry.has_agent,
                    entry.agent.is_some(),
                    entry.foreground_process_name.clone(),
                    entry
                        .usage_limited
                        .then(|| entry.state_labels.get("usage").cloned())
                        .flatten(),
                )
            });
    }

    aggregated
        .into_iter()
        .map(
            |(key, (mut entry, mixed_agents, has_agent, has_current_agent, _, usage_label))| {
                let agent_context = (!mixed_agents).then_some(entry.agent_context).flatten();
                entry.agent = (has_current_agent && !mixed_agents)
                    .then_some(agent_context)
                    .flatten();
                entry.agent_context = agent_context;
                entry.has_agent = has_agent;
                if entry.state != AgentState::Idle
                    || entry.open_blockers
                    || entry.active_subagents.unwrap_or_default() > 0
                    || entry.holds_shell
                {
                    entry.completion_tier = None;
                }
                if let Some(usage_label) = usage_label {
                    entry.state_labels.insert("usage".into(), usage_label);
                }
                (key, entry)
            },
        )
        .collect()
}

/// The key the sidebar groups a worktree space under.
///
/// Normally the space key, which is per checkout root, so the same repository
/// cloned twice (a second host mounted locally, a second clone path) is two
/// project headers. With `ui.combine_repos_across_hosts` the repo name alone is
/// the key, so those checkouts collapse into one project.
pub(crate) fn project_group_key(
    app: &AppState,
    space: &crate::workspace::WorktreeSpaceMembership,
) -> String {
    if !app.combine_repos_across_hosts {
        return space.key.clone();
    }
    space
        .repo_root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| space.key.clone())
}

/// Repository a Space belongs to when it carries no worktree membership.
///
/// The declaration wins: a bound Space names its repository directly. Only an
/// unbound Space falls back to what its panes resolved, and the scan stops at
/// the first pane that knows a repository, so an unbound Space of shells costs
/// one pass over panes that have nothing to say.
/// Returns the repository and whether the Space declared it. Borrowed, because
/// this runs per Space for every Space the Repo view lays out: building a key
/// string here would allocate quadratically for a view that needs one key per
/// group.
fn workspace_declared_repo(app: &AppState, ws_idx: usize) -> Option<(&str, bool)> {
    let workspace = app.workspaces.get(ws_idx)?;
    if let Some(repo) = workspace.repo_binding.as_deref() {
        return Some((repo, true));
    }
    // Every pane that resolved a repository must name the same one. A Space
    // holding two checkouts stays ungrouped rather than nesting under whichever
    // pane the map happened to yield first, which is the rule repo routing
    // already applies when it places panes.
    let mut resolved: Option<&str> = None;
    for pane in workspace.tabs.iter().flat_map(|tab| tab.panes.values()) {
        let Some(repo) = app
            .terminals
            .get(&pane.attached_terminal_id)
            .and_then(|terminal| terminal.effective_work_context().repo.as_deref())
        else {
            continue;
        };
        match resolved {
            Some(seen) if !seen.eq_ignore_ascii_case(repo) => return None,
            Some(_) => {}
            None => resolved = Some(repo),
        }
    }
    resolved.map(|repo| (repo, false))
}

/// What a Space groups by in the Repo view.
///
/// Borrowed and compared rather than formatted: only the row that heads a group
/// turns its identity into a key, and the Repo view compares every Space against
/// every group on each render.
#[derive(Debug, Clone, PartialEq, Eq)]
enum GroupIdent<'a> {
    /// Worktree membership, already keyed by `project_group_key`.
    Worktree(String),
    /// Repository slug, compared the way GitHub treats owner casing.
    Repo(&'a str),
}

impl GroupIdent<'_> {
    fn key(&self) -> String {
        match self {
            Self::Worktree(key) => key.clone(),
            Self::Repo(repo) => format!("{REPO_GROUP_PREFIX}{}", repo.to_ascii_lowercase()),
        }
    }
}

/// Group key for a Space in the Repo view, and whether it can be the group's
/// home row.
///
/// Worktree membership is the established key and keeps its exact meaning. A
/// Space without one is not repo-less: a Space created for a checkout of a repo
/// another Space is bound to, which is how agent tooling makes them, still
/// belongs under that repo. Such a Space groups by repository, and only a bound
/// Space can be the home row, so two loose checkouts never invent a header for
/// a repository no Space claims.
fn workspace_group_ident(app: &AppState, ws_idx: usize) -> Option<(GroupIdent<'_>, bool)> {
    let workspace = app.workspaces.get(ws_idx)?;
    if let Some(space) = workspace.worktree_space() {
        return Some((
            GroupIdent::Worktree(project_group_key(app, space)),
            !space.is_linked_worktree,
        ));
    }
    let (repo, declared) = workspace_declared_repo(app, ws_idx)?;
    Some((GroupIdent::Repo(repo), declared))
}

/// Whether `ws_idx` belongs under `group`. A Space with worktree membership is
/// only ever compared against worktree groups, so the established path stays
/// exactly as cheap as it was: no pane is inspected for it.
fn workspace_joins_group(app: &AppState, ws_idx: usize, group: &GroupIdent<'_>) -> bool {
    let Some(workspace) = app.workspaces.get(ws_idx) else {
        return false;
    };
    match (workspace.worktree_space(), group) {
        (Some(space), GroupIdent::Worktree(key)) => project_group_key(app, space) == *key,
        (Some(_), GroupIdent::Repo(_)) | (None, GroupIdent::Worktree(_)) => false,
        (None, GroupIdent::Repo(repo)) => workspace_declared_repo(app, ws_idx)
            .is_some_and(|(candidate, _)| candidate.eq_ignore_ascii_case(repo)),
    }
}

pub(crate) fn workspace_parent_group_state(
    app: &AppState,
    ws_idx: usize,
) -> Option<(String, bool)> {
    let (ident, home) = workspace_group_ident(app, ws_idx)?;
    if !home {
        return None;
    }
    let member_count = (0..app.workspaces.len())
        .filter(|idx| workspace_joins_group(app, *idx, &ident))
        .count();
    (member_count >= 2).then(|| {
        let key = ident.key();
        let collapsed = app.collapsed_space_keys.contains(&key);
        (key, collapsed)
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspaceListEntry {
    Workspace {
        ws_idx: usize,
        indented: bool,
    },
    NestedHeader {
        parent_ws_idx: usize,
        key: String,
        title: String,
    },
}

#[derive(Clone)]
pub(crate) enum SidebarRow {
    Workspace {
        ws_idx: usize,
        indented: bool,
        title: String,
        count: Option<usize>,
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
    NestedHeader {
        key: String,
        /// Canonical provider object key for the trailing action menu.
        /// Unlike `key`, this never includes workspace or settled prefixes.
        action_key: Option<String>,
        title: String,
        count: usize,
        collapsed: bool,
        /// A work item with no pane: rendered dim, never collapsible.
        dim: bool,
        /// Where the work item stands, rendered as a glyph before the id.
        /// `None` for a group that names no work item (the unlinked bucket,
        /// a worktree branch).
        status: Option<WorkGroupStatus>,
        /// An unassigned object that can spawn a thread from its trailing `+`.
        spawn: bool,
    },
    /// A Symphony workflow running outside this app. It owns no pane, so like
    /// the headers it stays out of every card-area list: it cannot be focused
    /// or navigated onto, and clicking it opens the Symphony window instead.
    SymphonyJob {
        /// Index into `AppState::symphony_snapshot.workflows`, so a click can
        /// open the window on the workflow the row was drawn from.
        index: usize,
        name: String,
        phase: String,
        /// The named wait the workflow is parked on, if any. A waiting workflow
        /// owes a human an answer, so it earns the blocked dot.
        wait: Option<String>,
        started_at: Option<String>,
    },
    /// Placeholder shown when the runner answered and has no open jobs. A
    /// section that disappears when empty cannot be told apart from one that is
    /// broken, so the reachable-and-empty case says so instead of vanishing.
    SymphonyEmpty,
}

/// Agents waiting on a human are the only ones whose wait you can end, so they
/// are grouped above everything else rather than sorted among it. The group is
/// omitted entirely when empty, which is the common case.
pub(crate) const BLOCKED_SECTION_TITLE: &str = "Blocked";
pub(crate) const RECENTLY_DONE_SECTION_TITLE: &str = "Recently done";
pub(crate) const SETTLED_SECTION_TITLE: &str = "Settled";
#[cfg(test)]
pub(crate) const PINNED_SECTION_TITLE: &str = "Pinned";
pub(crate) const SPACES_SECTION_TITLE: &str = "Spaces";
/// Symphony workflows run headless on a Temporal worker, so nothing in the
/// pane list ever shows them. The section is the only ambient surface they get.
pub(crate) const SYMPHONY_SECTION_TITLE: &str = "Symphony";

/// Shown under a reachable runner with nothing to list. It states the fact so
/// an empty section reads as an answer rather than as a missing feature.
pub(crate) const SYMPHONY_EMPTY_LABEL: &str = "no open jobs";

/// Only the group that demands action is coloured. Pinned and Spaces are
/// organisation, not urgency, so they stay in the muted chrome tone.
fn section_header_color(title: &str, p: &Palette) -> ratatui::style::Color {
    if title == BLOCKED_SECTION_TITLE {
        p.red
    } else {
        p.overlay0
    }
}

/// Groups collapse by title rather than by index: the set of groups changes
/// every time an agent blocks or unblocks, and a collapse the user asked for
/// must survive that churn.
pub(crate) fn section_is_collapsed(app: &AppState, title: &str) -> bool {
    app.collapsed_sidebar_groups.contains(&format!(
        "{}:{title}",
        app.sidebar_group_mode.collapse_namespace()
    ))
}

pub(crate) fn sidebar_rows(app: &AppState) -> Vec<SidebarRow> {
    sidebar_rows_inner(app, None, false)
}

fn sidebar_query_parts(query: &str) -> (Vec<&str>, Vec<&str>) {
    query
        .split_whitespace()
        .fold((Vec::new(), Vec::new()), |(mut text, mut labels), token| {
            if let Some(label) = token
                .strip_prefix("label:")
                .filter(|label| !label.is_empty())
            {
                labels.push(label);
            } else {
                text.push(token);
            }
            (text, labels)
        })
}

/// The tree is showing a filtered subset, so a workspace that contributes no
/// entry is noise and gets dropped rather than rendered as an empty header.
fn sidebar_rows_are_filtered(app: &AppState) -> bool {
    !app.sidebar_work_filter.query.is_empty() || app.sidebar_starred_only
}

fn sidebar_entry_matches_query(app: &AppState, entry: &AgentPanelEntry) -> bool {
    let (terms, _) = sidebar_query_parts(&app.sidebar_work_filter.query);
    if terms.is_empty() {
        return true;
    }
    let workspace = app.workspaces.get(entry.ws_idx);
    let context = entry_work_context(app, entry);
    let haystack = format!(
        "{} {} {} {} {} {} {} {} {} {}",
        entry.primary_label,
        entry.primary_tab_label.as_deref().unwrap_or_default(),
        entry.pane_label.as_deref().unwrap_or_default(),
        entry.terminal_title.as_deref().unwrap_or_default(),
        entry.agent_label.as_deref().unwrap_or_default(),
        workspace
            .map(|workspace| workspace.display_name_from_terminals(&app.terminals))
            .unwrap_or_default(),
        workspace
            .map(|workspace| workspace.identity_cwd.display().to_string())
            .unwrap_or_default(),
        context
            .and_then(|context| context.repo.as_deref())
            .unwrap_or_default(),
        context
            .and_then(|context| context.work_title.as_deref())
            .unwrap_or_default(),
        context
            .map(|context| context.ticket_ids.join(" "))
            .unwrap_or_default(),
    )
    .to_ascii_lowercase();
    terms
        .iter()
        .all(|term| haystack.contains(&term.to_ascii_lowercase()))
}

fn labels_match_sidebar_query(labels: &[String], query: &str) -> bool {
    let (_, required) = sidebar_query_parts(query);
    required.iter().all(|required| {
        labels
            .iter()
            .any(|label| label.eq_ignore_ascii_case(required))
    })
}

fn sidebar_query_has_labels(query: &str) -> bool {
    !sidebar_query_parts(query).1.is_empty()
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
    compact_sidebar_rows_inner(app, terminal_runtimes, expand_worktrees)
}

fn compact_sidebar_rows_inner(
    app: &AppState,
    terminal_runtimes: Option<&TerminalRuntimeRegistry>,
    expand_worktrees: bool,
) -> Vec<SidebarRow> {
    let mut entries = match terminal_runtimes {
        Some(runtimes) => sidebar_thread_entries_from(app, runtimes),
        None => sidebar_thread_entries(app),
    }
    .into_iter()
    // Cheap scalar gate first: when the star filter is on it discards most
    // entries before the query matcher builds its haystack string.
    .filter(|entry| !app.sidebar_starred_only || entry.starred)
    .filter(|entry| sidebar_entry_matches_query(app, entry))
    .collect::<Vec<_>>();
    let has_one_space_label = entries.first().is_some_and(|first| {
        entries
            .iter()
            .all(|entry| entry.space_label == first.space_label)
    });
    if has_one_space_label {
        for entry in &mut entries {
            entry.space_label_redundant = true;
        }
    }
    let (settled_entries, active_entries): (Vec<_>, Vec<_>) = entries
        .into_iter()
        .partition(|entry| app.pane_is_settled(entry.ws_idx, entry.pane_id));
    let visible_entries = if app.blocked_filter {
        active_entries
            .iter()
            .filter(|entry| entry_has_red_dot(entry))
            .cloned()
            .collect::<Vec<_>>()
    } else {
        active_entries
    };
    let (recently_done, visible_entries): (Vec<_>, Vec<_>) = visible_entries
        .into_iter()
        .partition(|entry| entry_is_past_done_hide_threshold(app, entry));
    if sidebar_rows_are_filtered(app)
        && visible_entries.is_empty()
        && recently_done.is_empty()
        && settled_entries.is_empty()
    {
        return Vec::new();
    }
    let mut rows = Vec::new();
    append_recently_done_rows(app, &mut rows, recently_done);
    // A Space is a folder, so this separation must hold even when no pane
    // resolved a repository.
    if app.sidebar_group_mode == SidebarGroupMode::Spaces
        || app.sidebar_group_mode == SidebarGroupMode::RepoWorktree
        || (app.sidebar_group_mode == SidebarGroupMode::Repo
            && !visible_entries
                .iter()
                .any(|entry| entry_repo_label(app, entry).is_some())
            && !visible_entries.iter().any(|entry| {
                entry_work_context(app, entry).is_some_and(pane_context_has_sidebar_metadata)
            }))
    {
        append_legacy_space_rows(
            app,
            &mut rows,
            visible_entries,
            expand_worktrees,
            terminal_runtimes,
        );
        append_tail_sections(app, &mut rows, settled_entries, expand_worktrees);
        return rows;
    }
    match app.sidebar_group_mode {
        SidebarGroupMode::Repo | SidebarGroupMode::RepoWorktree => {
            append_repo_group_rows(app, &mut rows, &visible_entries, false);
            append_unassigned_rows(app, &mut rows, &visible_entries);
        }
        SidebarGroupMode::Spaces => {}
        SidebarGroupMode::RepoPr | SidebarGroupMode::LinearTeam | SidebarGroupMode::Missive => {
            append_object_group_rows(app, &mut rows, &visible_entries, false);
        }
    }
    append_tail_sections(app, &mut rows, settled_entries, expand_worktrees);
    rows
}

fn pane_context_has_sidebar_metadata(context: &crate::work_context::PaneWorkContext) -> bool {
    !context.ticket_ids.is_empty()
        || !context.pr_urls.is_empty()
        || !context.missive_urls.is_empty()
        || context.branch.is_some()
        || context.repo.is_some()
        || context.work_title.is_some()
        || context.session_name.is_some()
}

fn append_legacy_space_rows(
    app: &AppState,
    rows: &mut Vec<SidebarRow>,
    entries: Vec<AgentPanelEntry>,
    expand_worktrees: bool,
    terminal_runtimes: Option<&TerminalRuntimeRegistry>,
) {
    let empty_runtimes;
    let terminal_runtimes = match terminal_runtimes {
        Some(terminal_runtimes) => terminal_runtimes,
        None => {
            empty_runtimes = TerminalRuntimeRegistry::new();
            &empty_runtimes
        }
    };
    let workspace_labels = sidebar_workspace_labels(app, terminal_runtimes);
    let workspaces = workspace_list_entries_for_mode(app, expand_worktrees, app.sidebar_group_mode);
    rows.push(SidebarRow::SectionHeader {
        title: SPACES_SECTION_TITLE,
        count: workspaces
            .iter()
            .filter(|entry| {
                matches!(
                    entry,
                    WorkspaceListEntry::Workspace {
                        indented: false,
                        ..
                    }
                )
            })
            .count(),
        collapsed: section_is_collapsed(app, SPACES_SECTION_TITLE),
    });
    if section_is_collapsed(app, SPACES_SECTION_TITLE) {
        return;
    }
    let mut entries_by_workspace = std::collections::HashMap::<usize, Vec<AgentPanelEntry>>::new();
    for entry in entries {
        entries_by_workspace
            .entry(entry.ws_idx)
            .or_default()
            .push(entry);
    }
    for workspace in workspaces {
        let WorkspaceListEntry::Workspace { ws_idx, indented } = workspace else {
            continue;
        };
        if indented {
            continue;
        }
        let mut member_entries = Vec::new();
        let member_indices = if app.sidebar_group_mode == SidebarGroupMode::Spaces {
            vec![ws_idx]
        } else {
            sidebar_space_member_indices(app, ws_idx)
        };
        for member_idx in member_indices {
            if let Some(entries) = entries_by_workspace.remove(&member_idx) {
                member_entries.extend(entries);
            }
        }
        if let Some((header_label, _)) = workspace_labels.get(&ws_idx) {
            mark_redundant_space_labels(&mut member_entries, header_label);
        }
        if sidebar_rows_are_filtered(app) && member_entries.is_empty() {
            continue;
        }
        rows.push(SidebarRow::Workspace {
            ws_idx,
            indented: false,
            title: String::new(),
            count: None,
        });
        if !app.workspace_agents_expanded(ws_idx) {
            continue;
        }
        if app.sidebar_group_mode == SidebarGroupMode::RepoWorktree {
            for mut group in sidebar_tab_groups(app, &member_entries, app.sidebar_group_mode) {
                let key = format!("{ws_idx}:{}", group.key);
                let collapsed = section_is_collapsed(app, &key);
                mark_redundant_space_labels(&mut group.entries, &group.title);
                rows.push(SidebarRow::NestedHeader {
                    key,
                    action_key: None,
                    title: group.title,
                    count: group.entries.len(),
                    collapsed,
                    dim: false,
                    status: None,
                    spawn: false,
                });
                if !collapsed {
                    append_tab_rows(rows, group.entries, 2);
                }
            }
        } else {
            append_tab_rows(rows, ordered_tab_entries(&member_entries), 1);
        }
    }
}

fn entry_repo_group(app: &AppState, entry: &AgentPanelEntry) -> Option<(String, String)> {
    if let Some(repo) = entry_work_context(app, entry)
        .and_then(|context| context.repo.as_deref())
        .map(str::trim)
        .filter(|repo| !repo.is_empty())
    {
        let title = repo.trim_end_matches('/');
        if title.is_empty() {
            return None;
        }
        let title = title.to_string();
        return Some((format!("repo:{repo}"), title));
    }
    if let Some(root) = entry_terminal(app, entry)
        .and_then(|terminal| app.git_root_for_cwd.get(&terminal.cwd))
        .and_then(Option::as_ref)
    {
        let title = root.file_name()?.to_string_lossy().into_owned();
        return Some((format!("repo-path:{}", root.display()), title));
    }
    None
}

fn entry_repo_label(app: &AppState, entry: &AgentPanelEntry) -> Option<String> {
    entry_repo_group(app, entry).map(|(_, title)| title)
}

/// Unlinked buckets stay after every linked group, and the directory-less
/// bucket stays after the ones that do name a directory.
fn unlinked_sort_key(group: &SidebarWorkGroup) -> (bool, bool) {
    (
        group.unlinked,
        group.unlinked && group.key == UNLINKED_GROUP_KEY,
    )
}

fn sidebar_repo_groups(app: &AppState, entries: &[AgentPanelEntry]) -> Vec<SidebarWorkGroup> {
    let mut groups = Vec::new();
    for entry in ordered_tab_entries(entries) {
        let Some((key, title)) = entry_repo_group(app, &entry) else {
            push_unlinked_entry(app, &mut groups, entry);
            continue;
        };
        match work_group_index(&groups, &key) {
            Some(index) => groups[index].entries.push(entry),
            None => groups.push(SidebarWorkGroup {
                key,
                title,
                entries: vec![entry],
                unlinked: false,
                status: None,
                created_at: None,
                activation: None,
            }),
        }
    }
    disambiguate_unlinked_titles(
        &mut groups,
        |group| {
            group
                .unlinked
                .then(|| group.entries.first())
                .flatten()
                .and_then(|entry| unlinked_group_directory(app, entry))
        },
        |group| &mut group.title,
    );
    groups.sort_by_key(unlinked_sort_key);
    groups
}

fn append_tab_rows(rows: &mut Vec<SidebarRow>, entries: Vec<AgentPanelEntry>, depth: u16) {
    rows.extend(entries.into_iter().map(|entry| SidebarRow::Tab {
        entry: Box::new(entry),
        depth,
    }));
}

fn mark_redundant_space_labels(entries: &mut [AgentPanelEntry], group_title: &str) {
    for entry in entries {
        entry.space_label_redundant |= entry.space_label == group_title;
    }
}

fn append_repo_group_rows(
    app: &AppState,
    rows: &mut Vec<SidebarRow>,
    entries: &[AgentPanelEntry],
    settled: bool,
) {
    for mut group in sidebar_repo_groups(app, entries) {
        mark_redundant_space_labels(&mut group.entries, &group.title);
        if !group.unlinked {
            let Some(ws_idx) = group.entries.first().map(|entry| entry.ws_idx) else {
                continue;
            };
            rows.push(SidebarRow::Workspace {
                ws_idx,
                indented: false,
                title: group.title.clone(),
                count: Some(group.entries.len()),
            });
            if !app.workspace_agents_expanded(ws_idx) {
                continue;
            }
        }
        let collapse_key = if settled {
            format!("settled:{}", group.key)
        } else {
            group.key.clone()
        };
        let collapsed = section_is_collapsed(app, &collapse_key);
        if group.unlinked {
            rows.push(SidebarRow::NestedHeader {
                key: collapse_key,
                action_key: None,
                title: group.title,
                count: group.entries.len(),
                collapsed,
                dim: false,
                status: None,
                spawn: false,
            });
            if collapsed {
                continue;
            }
        }
        if group.unlinked {
            append_tab_rows(rows, group.entries, 1);
            continue;
        }
        let mut branches = Vec::<(Option<String>, Vec<AgentPanelEntry>)>::new();
        for entry in group.entries {
            let branch = entry_work_context(app, &entry).and_then(|context| context.branch.clone());
            match branches
                .iter_mut()
                .find(|(candidate, _)| *candidate == branch)
            {
                Some((_, entries)) => entries.push(entry),
                None => branches.push((branch, vec![entry])),
            }
        }
        if branches.len() <= 1 {
            let entries = branches
                .pop()
                .map(|(_, entries)| entries)
                .unwrap_or_default();
            append_tab_rows(rows, entries, 1);
            continue;
        }
        for (branch, entries) in branches {
            let branch_name = branch.as_deref().unwrap_or("unlinked");
            let key = format!("{}:branch:{branch_name}", group.key);
            let collapse_key = if settled {
                format!("settled:{key}")
            } else {
                key
            };
            let collapsed = section_is_collapsed(app, &collapse_key);
            let title = branch.map_or_else(|| "unlinked".into(), |branch| format!("⎇ {branch}"));
            let mut entries = entries;
            mark_redundant_space_labels(&mut entries, &title);
            rows.push(SidebarRow::NestedHeader {
                key: collapse_key,
                action_key: None,
                title,
                count: entries.len(),
                collapsed,
                dim: false,
                status: None,
                spawn: false,
            });
            if !collapsed {
                append_tab_rows(rows, entries, 2);
            }
        }
    }
}

fn append_object_group_rows(
    app: &AppState,
    rows: &mut Vec<SidebarRow>,
    entries: &[AgentPanelEntry],
    settled: bool,
) {
    for mut group in sidebar_work_groups(app, entries, app.sidebar_group_mode)
        .into_iter()
        .filter(|group| !group.entries.is_empty())
    {
        let collapse_key = if settled {
            format!("settled:{}", group.key)
        } else {
            group.key.clone()
        };
        let collapsed = section_is_collapsed(app, &collapse_key);
        // Only provider objects have object actions. A branch group names a
        // branch, so the `…` affordance would open an empty menu.
        let action_key = (!group.unlinked
            && ["github:", "linear:", "missive:"]
                .iter()
                .any(|prefix| group.key.starts_with(prefix)))
        .then(|| group.key.clone());
        mark_redundant_space_labels(&mut group.entries, &group.title);
        rows.push(SidebarRow::NestedHeader {
            key: collapse_key,
            action_key,
            title: group.title,
            count: group.entries.len(),
            collapsed,
            dim: false,
            status: group.status,
            spawn: false,
        });
        if !collapsed {
            append_tab_rows(rows, group.entries, 1);
        }
    }
    if settled {
        return;
    }
    if matches!(
        app.sidebar_group_mode,
        SidebarGroupMode::RepoPr | SidebarGroupMode::LinearTeam | SidebarGroupMode::Missive
    ) {
        append_unassigned_rows(app, rows, entries);
    }
}

/// The two sections that close the list, in order: Symphony first so open
/// workflows sit directly under the spaces they relate to, then Settled, which
/// is history and always sinks to the bottom.
fn append_tail_sections(
    app: &AppState,
    rows: &mut Vec<SidebarRow>,
    settled_entries: Vec<AgentPanelEntry>,
    expand_worktrees: bool,
) {
    append_symphony_rows(app, rows);
    append_settled_rows(app, rows, settled_entries, expand_worktrees);
}

fn append_settled_rows(
    app: &AppState,
    rows: &mut Vec<SidebarRow>,
    entries: Vec<AgentPanelEntry>,
    _expand_worktrees: bool,
) {
    if entries.is_empty() {
        return;
    }
    rows.push(SidebarRow::SectionHeader {
        title: SETTLED_SECTION_TITLE,
        count: entries.len(),
        collapsed: section_is_collapsed(app, SETTLED_SECTION_TITLE),
    });
    if section_is_collapsed(app, SETTLED_SECTION_TITLE) {
        return;
    }

    match app.sidebar_group_mode {
        SidebarGroupMode::Repo | SidebarGroupMode::RepoWorktree | SidebarGroupMode::Spaces => {
            append_repo_group_rows(app, rows, &entries, true);
        }
        SidebarGroupMode::RepoPr | SidebarGroupMode::LinearTeam | SidebarGroupMode::Missive => {
            append_object_group_rows(app, rows, &entries, true);
        }
    }
}

struct SidebarTabGroup {
    key: String,
    title: String,
    entries: Vec<AgentPanelEntry>,
    unlinked: bool,
}

fn ordered_tab_entries(entries: &[AgentPanelEntry]) -> Vec<AgentPanelEntry> {
    let tab_entries = aggregate_tab_entries(entries);
    let mut seen = std::collections::HashSet::new();
    entries
        .iter()
        .filter_map(|entry| {
            let tab = (entry.ws_idx, entry.tab_idx);
            seen.insert(tab)
                .then(|| tab_entries.get(&tab).cloned())
                .flatten()
        })
        .collect()
}

fn entry_work_context<'a>(
    app: &'a AppState,
    entry: &AgentPanelEntry,
) -> Option<&'a crate::work_context::PaneWorkContext> {
    entry_terminal(app, entry).map(crate::terminal::TerminalState::effective_work_context)
}

fn entry_terminal<'a>(
    app: &'a AppState,
    entry: &AgentPanelEntry,
) -> Option<&'a crate::terminal::TerminalState> {
    let workspace = app.workspaces.get(entry.ws_idx)?;
    let tab = workspace.tabs.get(entry.tab_idx)?;
    let pane = tab.panes.get(&entry.pane_id)?;
    app.terminals.get(&pane.attached_terminal_id)
}

fn stable_binding_values<'a>(sources: impl IntoIterator<Item = &'a [String]>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    sources
        .into_iter()
        .flatten()
        .filter(|value| seen.insert((*value).clone()))
        .cloned()
        .collect()
}

fn preferred_pr_urls(app: &AppState, entry: &AgentPanelEntry) -> Vec<String> {
    let Some(terminal) = entry_terminal(app, entry) else {
        return Vec::new();
    };
    let tiers = terminal.work_context.snapshot_tiers();
    let declared = stable_binding_values([
        tiers.manual.pr_urls.as_slice(),
        tiers.hook_turn.pr_urls.as_slice(),
    ]);
    if !declared.is_empty() {
        return declared;
    }
    if !tiers.git_observation.pr_urls.is_empty() {
        return tiers.git_observation.pr_urls;
    }
    tiers.restored_fallback.pr_urls
}

fn preferred_ticket_ids(app: &AppState, entry: &AgentPanelEntry) -> Vec<String> {
    let Some(terminal) = entry_terminal(app, entry) else {
        return Vec::new();
    };
    let tiers = terminal.work_context.snapshot_tiers();
    let declared = stable_binding_values([
        tiers.manual.ticket_ids.as_slice(),
        tiers.hook_turn.ticket_ids.as_slice(),
    ]);
    if !declared.is_empty() {
        return declared;
    }
    if !tiers.git_observation.ticket_ids.is_empty() {
        return tiers.git_observation.ticket_ids;
    }
    tiers.restored_fallback.ticket_ids
}

fn pull_request_number(url: &str) -> Option<&str> {
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|segment| segment.chars().all(|character| character.is_ascii_digit()))
}

/// Same reasoning as `push_unlinked_entry`: a tab with nothing to group by
/// still belongs beside the other tabs in its directory.
fn push_unlinked_tab_group(
    app: &AppState,
    groups: &mut Vec<SidebarTabGroup>,
    entry: AgentPanelEntry,
) {
    let (key, title) = unlinked_group_key_and_title(app, &entry);
    push_sidebar_tab_group(groups, key, title, entry, true);
}

fn push_sidebar_tab_group(
    groups: &mut Vec<SidebarTabGroup>,
    key: String,
    title: String,
    entry: AgentPanelEntry,
    unlinked: bool,
) {
    if let Some(group) = groups.iter_mut().find(|group| group.key == key) {
        group.entries.push(entry);
    } else {
        groups.push(SidebarTabGroup {
            key,
            title,
            entries: vec![entry],
            unlinked,
        });
    }
}

fn sidebar_tab_groups(
    app: &AppState,
    entries: &[AgentPanelEntry],
    mode: SidebarGroupMode,
) -> Vec<SidebarTabGroup> {
    let mut groups = Vec::new();
    for entry in ordered_tab_entries(entries) {
        let context = entry_work_context(app, &entry);
        match mode {
            SidebarGroupMode::RepoPr => {
                // Same source as the work view's groups: a declared pull
                // request is what the window is working on, and grouping it by
                // the effective union filed a linked window under the branch's
                // pull request as well.
                let urls = preferred_pr_urls(app, &entry);
                if urls.is_empty() {
                    match context.and_then(|context| context.branch.as_deref()) {
                        Some(branch) => push_sidebar_tab_group(
                            &mut groups,
                            branch_group_key(
                                context.and_then(|context| context.repo.as_deref()),
                                branch,
                            ),
                            branch_group_title(branch),
                            entry,
                            false,
                        ),
                        None => push_unlinked_tab_group(app, &mut groups, entry),
                    }
                    continue;
                }
                for url in &urls {
                    let title_suffix = app
                        .work_index_snapshot
                        .as_ref()
                        .and_then(|snapshot| {
                            snapshot
                                .items
                                .iter()
                                .find(|item| item.pr_url.as_deref() == Some(url.as_str()))
                        })
                        .and_then(|item| item.pr_title.as_deref())
                        .or_else(|| context.and_then(|context| context.work_title.as_deref()))
                        .or_else(|| context.and_then(|context| context.session_name.as_deref()));
                    let number = pull_request_number(url).unwrap_or(url);
                    let title = work_group_header_title(&format!("#{number}"), title_suffix);
                    push_sidebar_tab_group(&mut groups, url.clone(), title, entry.clone(), false);
                }
            }
            SidebarGroupMode::RepoWorktree => {
                if let Some(branch) = context.and_then(|context| context.branch.as_deref()) {
                    push_sidebar_tab_group(
                        &mut groups,
                        branch.to_string(),
                        format!("⎇ {branch}"),
                        entry,
                        false,
                    );
                } else {
                    push_unlinked_tab_group(app, &mut groups, entry);
                }
            }
            SidebarGroupMode::Repo
            | SidebarGroupMode::Spaces
            | SidebarGroupMode::LinearTeam
            | SidebarGroupMode::Missive => {}
        }
    }
    disambiguate_unlinked_titles(
        &mut groups,
        |group| {
            group
                .unlinked
                .then(|| group.entries.first())
                .flatten()
                .and_then(|entry| unlinked_group_directory(app, entry))
        },
        |group| &mut group.title,
    );
    disambiguate_branch_titles(
        &mut groups,
        |group| group.key.clone(),
        |group| &mut group.title,
    );
    groups.sort_by_key(|group| {
        (
            group.unlinked,
            group.unlinked && group.key == UNLINKED_GROUP_KEY,
        )
    });
    groups
}

/// A ticket or Missive conversation the sidebar groups panes under.
///
/// Unlike the repo-level groups, a work group can exist with no pane at all:
/// the projection knows the ticket, nobody has started a thread on it yet, and
/// that absence is exactly what the operator wants to see.
pub(crate) struct SidebarWorkGroup {
    pub(crate) key: String,
    pub(crate) title: String,
    pub(crate) entries: Vec<AgentPanelEntry>,
    pub(crate) unlinked: bool,
    /// Where the work item stands, rendered as a glyph before the id.
    pub(crate) status: Option<WorkGroupStatus>,
    pub(crate) created_at: Option<std::time::SystemTime>,
    /// Provider object metadata used by the row's `+` and `n` spawn actions.
    /// `None` for the unlinked bucket, which names no work item.
    pub(crate) activation: Option<SidebarWorkGroupActivation>,
}

/// The launch context for an agentless work item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SidebarWorkGroupActivation {
    /// Prompt sent to the new agent. It always contains the object's link.
    pub(crate) spawn_prompt: String,
    pub(crate) object_link: String,
    /// The checkout the work item is linked to, when the projection knows one.
    /// `None` leaves the composer on its last used directory.
    pub(crate) directory: Option<std::path::PathBuf>,
    pub(crate) git_ref: Option<crate::app::home_refs::HomeRef>,
    pub(crate) pr: Option<crate::app::home::HomePrContext>,
    pub(crate) ticket: Option<crate::app::home::HomeTicketContext>,
    pub(crate) missive: Option<crate::app::home::HomeMissiveContext>,
    pub(crate) work_context_patch: crate::work_context::PaneWorkContextPatch,
}

const UNLINKED_GROUP_KEY: &str = "unlinked";
/// Namespaces the repository-derived Repo view keys away from the worktree keys,
/// which are checkout paths or directory names.
const REPO_GROUP_PREFIX: &str = "repo:";
/// Sessions with no work item but a branch of the repo group under the branch.
/// Namespaced so a branch called `unlinked` cannot land in the unlinked bucket.
const BRANCH_GROUP_PREFIX: &str = "branch:";
/// Namespaced away from the raw branch keys the worktree view still uses.
const UNLINKED_DIR_PREFIX: &str = "unlinked-dir:";

/// `SCA-3165` -> `SCA`. The team is the identifier prefix; the projection
/// carries no separate team field.
pub(crate) fn ticket_team(identifier: &str) -> Option<String> {
    identifier
        .split_once('-')
        .map(|(team, _)| team.to_string())
        .filter(|team| !team.is_empty())
}

fn ticket_group_title(ticket: &crate::work_index::WorkTicket) -> String {
    work_group_header_title(&ticket.identifier, ticket.title.as_deref())
}

/// `<id> · <title>` (F12-3). The id alone is the header when the work item
/// carries no title: a separator with nothing after it reads as missing text.
fn work_group_header_title(id: &str, title: Option<&str>) -> String {
    match title.map(str::trim).filter(|title| !title.is_empty()) {
        Some(title) => match crate::workspace::title_without_identifier(id, title) {
            Some("") => id.to_string(),
            Some(title) => format!("{id} · {title}"),
            None => format!("{id} · {title}"),
        },
        _ => id.to_string(),
    }
}

/// The state of a pull request a pane declares, from the work index cache.
/// A URL the index has never seen has no state to show, and
/// `from_pull_request` reads that absence as open.
fn pull_request_status(app: &AppState, url: &str) -> WorkGroupStatus {
    let item = indexed_pull_request(app, url);
    WorkGroupStatus::from_pull_request(
        item.and_then(|item| item.pr_state.as_deref()),
        item.is_some_and(|item| item.draft),
    )
}

fn indexed_pull_request<'a>(
    app: &'a AppState,
    url: &str,
) -> Option<&'a crate::work_index::WorkItem> {
    app.work_index_snapshot
        .as_ref()?
        .items
        .iter()
        .find(|item| item.pr_url.as_deref() == Some(url))
}

fn github_group_title(app: &AppState, url: &str, fallback: Option<&str>) -> String {
    let item = indexed_pull_request(app, url);
    let number = item
        .and_then(|item| item.pr_number)
        .map(|number| number.to_string())
        .or_else(|| pull_request_number(url).map(str::to_string))
        .unwrap_or_else(|| url.to_string());
    work_group_header_title(
        &format!("#{number}"),
        item.and_then(|item| item.pr_title.as_deref()).or(fallback),
    )
}

fn ticket_prompt(ticket: &crate::work_index::WorkTicket) -> String {
    match ticket.title.as_deref() {
        Some(title) => format!("{}: {title}", ticket.identifier),
        None => ticket.identifier.clone(),
    }
}

fn ticket_activation(
    app: &AppState,
    row: &crate::work_projection::DockHomeTicketRow,
) -> SidebarWorkGroupActivation {
    let prompt = ticket_prompt(&row.ticket);
    let object_link = row
        .ticket
        .url
        .clone()
        .or_else(|| crate::work_context::linear_ticket_url(&row.ticket.identifier))
        .unwrap_or_else(|| row.ticket.identifier.clone());
    let repo = row
        .linked_pr_url
        .as_deref()
        .and_then(crate::work_context::repo_slug_from_pr_url);
    SidebarWorkGroupActivation {
        spawn_prompt: format!("{prompt}\n{object_link}"),
        object_link: object_link.clone(),
        directory: ticket_directory(app, row),
        git_ref: row
            .ticket
            .branch
            .as_ref()
            .map(|branch| crate::app::home_refs::HomeRef {
                name: branch.clone(),
                oid: String::new(),
                tag: None,
            }),
        pr: None,
        ticket: Some(crate::app::home::HomeTicketContext {
            identifier: row.ticket.identifier.clone(),
            title: row
                .ticket
                .title
                .clone()
                .unwrap_or_else(|| "(untitled ticket)".into()),
            url: object_link,
        }),
        missive: None,
        work_context_patch: crate::work_context::PaneWorkContextPatch {
            ticket_ids: Some(vec![row.ticket.identifier.clone()]),
            repo,
            branch: row.ticket.branch.clone(),
            work_title: row.ticket.title.clone(),
            ..Default::default()
        },
    }
}

fn sidebar_linear_ticket_rows(app: &AppState) -> Vec<crate::work_projection::DockHomeTicketRow> {
    let mut rows = app.dock_home_projection().ticket_rows;
    for item in app
        .work_index_snapshot
        .as_ref()
        .into_iter()
        .flat_map(|snapshot| snapshot.items.iter())
    {
        for ticket in &item.ticket_details {
            if let Some(row) = rows.iter_mut().find(|row| {
                row.ticket
                    .identifier
                    .eq_ignore_ascii_case(&ticket.identifier)
            }) {
                if row.linked_pr_url.is_none() {
                    row.linked_pr_url.clone_from(&item.pr_url);
                }
                continue;
            }
            rows.push(crate::work_projection::DockHomeTicketRow {
                key: crate::app::state::WorkItemKey {
                    repo: String::new(),
                    pr_number: None,
                    pr_url: None,
                    ticket_id: Some(ticket.identifier.clone()),
                },
                ticket: ticket.clone(),
                linked_pr_url: item.pr_url.clone(),
                jump_target: None,
            });
        }
    }
    rows.sort_by(|left, right| {
        left.ticket
            .identifier
            .to_ascii_lowercase()
            .cmp(&right.ticket.identifier.to_ascii_lowercase())
    });
    rows
}

/// Where a thread for this ticket should start: the checkout of a pane already
/// working the repository the ticket's pull request lives in.
fn ticket_directory(
    app: &AppState,
    row: &crate::work_projection::DockHomeTicketRow,
) -> Option<std::path::PathBuf> {
    let repo = row
        .linked_pr_url
        .as_deref()
        .and_then(crate::work_context::repo_slug_from_pr_url)?;
    app.workspaces
        .iter()
        .flat_map(|workspace| workspace.tabs.iter())
        .flat_map(|tab| tab.panes.values())
        .filter_map(|pane| app.terminals.get(&pane.attached_terminal_id))
        .find(|terminal| terminal.effective_work_context().repo.as_deref() == Some(repo.as_str()))
        .map(|terminal| terminal.cwd.clone())
}

/// The last segment of a Missive conversation URL.
fn missive_url_tail(url: &str) -> String {
    url.rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(url)
        .to_string()
}

/// The link-row label the context dock shows for this conversation, when it
/// carries more than the URL itself. `work_link_candidates` derives the label
/// from the URL today, so this is the seam a cached subject arrives through.
fn cached_missive_link_label(app: &AppState, url: &str) -> Option<String> {
    context_panes(app).find_map(|context| {
        crate::work_context::work_link_candidates(context)
            .into_iter()
            .find(|candidate| {
                candidate.kind == crate::work_context::WorkLinkKind::Missive && candidate.url == url
            })
            .map(|candidate| candidate.label)
    })
}

/// A label that is only the URL restated tells the reader nothing the header
/// would not already show.
fn missive_label_is_url(label: &str, url: &str) -> bool {
    let tail = missive_url_tail(url);
    label == url || label == tail || label == format!("missive/{tail}")
}

/// The declared work title of a pane carrying this conversation. A pane bound
/// to that conversation alone describes it best, so it wins over a pane that
/// spreads across several.
fn missive_work_title(app: &AppState, url: &str) -> Option<String> {
    let carries = |context: &&crate::work_context::PaneWorkContext| {
        context.missive_urls.iter().any(|other| other == url)
    };
    let exclusive = context_panes(app)
        .filter(carries)
        .find(|context| context.missive_urls.len() == 1);
    exclusive
        .or_else(|| context_panes(app).find(carries))
        .and_then(|context| context.work_title.clone())
}

fn indexed_missive_conversation<'a>(
    app: &'a AppState,
    url: &str,
) -> Option<&'a crate::work_index::MissiveConversation> {
    let id = missive_url_tail(url);
    app.work_index_snapshot
        .as_ref()?
        .conversations
        .iter()
        .find(|conversation| {
            conversation.app_url == url || conversation.web_url == url || conversation.id == id
        })
}

/// The URL panes use to join an indexed conversation. Pane work context stores
/// web URLs, so prefer that form while retaining the app URL for sparse cached
/// fixtures.
fn indexed_missive_url(conversation: &crate::work_index::MissiveConversation) -> &str {
    [&conversation.web_url, &conversation.app_url]
        .into_iter()
        .map(String::as_str)
        .find(|url| !url.trim().is_empty())
        .unwrap_or(&conversation.id)
}

/// Prefer the indexed subject, then fall back to pane-local context while the
/// first Missive observation is still pending.
fn missive_subject(app: &AppState, url: &str) -> String {
    missive_subject_from(
        indexed_missive_conversation(app, url).map(|conversation| conversation.subject.as_str()),
        cached_missive_link_label(app, url).as_deref(),
        missive_work_title(app, url).as_deref(),
        url,
    )
}

fn missive_subject_from(
    indexed_subject: Option<&str>,
    link_label: Option<&str>,
    work_title: Option<&str>,
    url: &str,
) -> String {
    indexed_subject
        .map(str::trim)
        .filter(|subject| !subject.is_empty())
        .or_else(|| {
            link_label
                .map(str::trim)
                .filter(|label| !label.is_empty() && !missive_label_is_url(label, url))
        })
        .or_else(|| work_title.map(str::trim).filter(|title| !title.is_empty()))
        .map(str::to_string)
        .unwrap_or_else(|| missive_url_tail(url))
}

/// `<conversation id> · <subject>` (F12-3). The URL tail is the only id a
/// conversation has here, so a subject that resolved to nothing but the tail
/// leaves the header as the id alone.
fn missive_group_title(app: &AppState, url: &str) -> String {
    let tail = missive_url_tail(url);
    work_group_header_title(&tail, Some(missive_subject(app, url)).as_deref())
}

fn missive_prompt(app: &AppState, url: &str) -> String {
    let id = missive_url_tail(url);
    let subject = missive_subject(app, url);
    if subject == id {
        id
    } else {
        format!("{id}: {subject}")
    }
}

/// Use indexed state when present. A pane-only conversation remains open
/// until the read-only observation supplies stronger evidence.
fn missive_group_status(app: &AppState, url: &str) -> WorkGroupStatus {
    indexed_missive_conversation(app, url).map_or_else(
        || WorkGroupStatus::from_conversation(false, true),
        |conversation| {
            WorkGroupStatus::from_conversation(
                conversation.closed,
                !conversation.assignees.is_empty(),
            )
        },
    )
}

/// Every pane's effective work context, in workspace order.
fn context_panes(app: &AppState) -> impl Iterator<Item = &crate::work_context::PaneWorkContext> {
    app.workspaces
        .iter()
        .flat_map(|workspace| workspace.tabs.iter())
        .flat_map(|tab| tab.panes.values())
        .filter_map(|pane| app.terminals.get(&pane.attached_terminal_id))
        .map(|terminal| terminal.effective_work_context())
}

fn work_group_index(groups: &[SidebarWorkGroup], key: &str) -> Option<usize> {
    groups.iter().position(|group| group.key == key)
}

fn pane_bound_ticket(app: &AppState, identifier: &str) -> bool {
    context_panes(app).any(|context| {
        context
            .ticket_ids
            .iter()
            .any(|ticket| ticket.eq_ignore_ascii_case(identifier))
    })
}

fn pane_bound_conversation(app: &AppState, url: &str) -> bool {
    let id = missive_url_tail(url);
    context_panes(app).any(|context| {
        context
            .missive_urls
            .iter()
            .any(|candidate| candidate == url || missive_url_tail(candidate) == id)
    })
}

/// Bucket for a pane no work item claims.
///
/// A single flat "unlinked" list stops being readable as soon as it holds more
/// than a handful of panes, and the panes in it are not actually unrelated:
/// they share a working directory. Key on that directory so they group, and
/// keep the plain bucket for a pane whose directory cannot be resolved.
///
/// The directory is `TerminalState::cwd`, which is OSC 7-only and therefore
/// the launch directory for a pane without shell integration. That is the same
/// source `entry_repo_group` already uses for its git-root lookup, so the two
/// halves of this projection agree. Rendering is pure and cannot reach
/// `TerminalRuntimeRegistry`; resolving a live cwd per pane per render is its
/// own change with its own cost.
/// Normalised, because the key and the title must agree on what one directory
/// is: `components()` folds `/a/./project` into `/a/project`, so keying on the
/// raw path would split one directory into two buckets with the same header.
/// `..` is left alone; resolving it would need filesystem access.
fn unlinked_group_directory(app: &AppState, entry: &AgentPanelEntry) -> Option<std::path::PathBuf> {
    entry_terminal(app, entry)
        .map(|terminal| terminal.cwd.components().collect::<std::path::PathBuf>())
        .filter(|cwd| !cwd.as_os_str().is_empty())
}

/// A directory header, from the last path component.
///
/// `depth` says how many trailing components to keep, so a caller that finds
/// two directories sharing a basename can ask for a longer, distinguishing
/// title.
fn unlinked_directory_title(dir: &std::path::Path, depth: usize) -> String {
    let mut components: Vec<String> = dir
        .components()
        .rev()
        .take(depth.max(1))
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .filter(|component| !component.is_empty())
        .collect();
    if components.is_empty() {
        // A root directory has no named component; show the path itself rather
        // than an empty header.
        return format!("▫ {}", dir.display());
    }
    components.reverse();
    format!("▫ {}", components.join("/"))
}

fn unlinked_group_key_and_title(app: &AppState, entry: &AgentPanelEntry) -> (String, String) {
    let Some(cwd) = unlinked_group_directory(app, entry) else {
        return (UNLINKED_GROUP_KEY.into(), "unlinked".into());
    };
    (unlinked_group_key(&cwd), unlinked_directory_title(&cwd, 1))
}

/// Keyed on the debug form of the raw `OsStr`, not `display()`, which is lossy:
/// two distinct non-UTF-8 paths must not collapse into one bucket.
fn unlinked_group_key(dir: &std::path::Path) -> String {
    format!("{UNLINKED_DIR_PREFIX}{:?}", dir.as_os_str())
}

/// Two unlinked directories can share a basename, and `▫ project` twice tells
/// the operator nothing. Deepen every title until they are all distinct.
///
/// One extra component is not enough on its own: `/a/x/project` and
/// `/b/x/project` are still both `x/project`. The loop is bounded by the
/// longest path in the list, and both it and the group list are small.
fn disambiguate_unlinked_titles<T>(
    groups: &mut [T],
    directory: impl Fn(&T) -> Option<std::path::PathBuf>,
    title: impl Fn(&mut T) -> &mut String,
) {
    let dirs: Vec<std::path::PathBuf> = groups.iter().filter_map(&directory).collect();
    if dirs.len() < 2 {
        return;
    }
    let max_depth = dirs
        .iter()
        .map(|dir| dir.components().count())
        .max()
        .unwrap_or(1);
    let mut depth = 1;
    while depth < max_depth {
        let mut titles: Vec<String> = dirs
            .iter()
            .map(|dir| unlinked_directory_title(dir, depth))
            .collect();
        titles.sort_unstable();
        let before = titles.len();
        titles.dedup();
        if titles.len() == before {
            break;
        }
        depth += 1;
    }
    for group in groups.iter_mut() {
        let Some(dir) = directory(group) else {
            continue;
        };
        *title(group) = unlinked_directory_title(&dir, depth);
    }
}

/// Keyed by repository and branch: `main` is the most common branch name there
/// is, so a bare branch key would fold a session on one repository's `main` into
/// another repository's group.
fn branch_group_key(repo: Option<&str>, branch: &str) -> String {
    match repo {
        Some(repo) => format!(
            "{BRANCH_GROUP_PREFIX}{}\u{1f}{branch}",
            repo.to_ascii_lowercase()
        ),
        None => format!("{BRANCH_GROUP_PREFIX}\u{1f}{branch}"),
    }
}

fn branch_group_title(branch: &str) -> String {
    format!("⎇ {branch}")
}

/// Repository a branch group key was built from, for titles that have to tell
/// two same-named branches apart.
fn branch_group_repo(key: &str) -> Option<&str> {
    let rest = key.strip_prefix(BRANCH_GROUP_PREFIX)?;
    let repo = rest.split('\u{1f}').next()?;
    (!repo.is_empty()).then_some(repo)
}

/// `⎇ main` twice tells the operator nothing, so branch groups that share a
/// title name their repository. Groups are few and this runs once per
/// projection, after every group exists.
fn disambiguate_branch_titles<T>(
    groups: &mut [T],
    key: impl Fn(&T) -> String,
    title: impl Fn(&mut T) -> &mut String,
) {
    let branch_keys = groups
        .iter()
        .map(|group| {
            let key = key(group);
            key.starts_with(BRANCH_GROUP_PREFIX).then_some(key)
        })
        .collect::<Vec<_>>();
    let titles = (0..groups.len())
        .map(|index| title(&mut groups[index]).clone())
        .collect::<Vec<_>>();
    let mut shared = std::collections::HashMap::<&str, usize>::new();
    for (index, group_title) in titles.iter().enumerate() {
        if branch_keys[index].is_some() {
            *shared.entry(group_title.as_str()).or_default() += 1;
        }
    }
    for index in 0..groups.len() {
        let Some(key) = branch_keys[index].as_deref() else {
            continue;
        };
        if shared.get(titles[index].as_str()).copied().unwrap_or(0) < 2 {
            continue;
        }
        if let Some(repo) = branch_group_repo(key) {
            *title(&mut groups[index]) = format!("{} · {repo}", titles[index]);
        }
    }
}

/// A session on a branch of this repo with no pull request yet. It is repo work
/// like any other, so it groups under its branch beside the PR groups instead
/// of sinking into the directory-named unlinked bucket, where a worktree that
/// has not opened a PR reads as work on nothing.
fn push_branch_entry(
    groups: &mut Vec<SidebarWorkGroup>,
    repo: Option<&str>,
    branch: &str,
    entry: AgentPanelEntry,
) {
    let key = branch_group_key(repo, branch);
    match work_group_index(groups, &key) {
        Some(index) => groups[index].entries.push(entry),
        None => groups.push(SidebarWorkGroup {
            key,
            title: branch_group_title(branch),
            entries: vec![entry],
            unlinked: false,
            status: None,
            created_at: None,
            activation: None,
        }),
    }
}

fn push_unlinked_entry(app: &AppState, groups: &mut Vec<SidebarWorkGroup>, entry: AgentPanelEntry) {
    let (key, title) = unlinked_group_key_and_title(app, &entry);
    match work_group_index(groups, &key) {
        Some(index) => groups[index].entries.push(entry),
        None => groups.push(SidebarWorkGroup {
            key,
            title,
            entries: vec![entry],
            unlinked: true,
            status: None,
            created_at: None,
            activation: None,
        }),
    }
}

fn push_linear_unlinked_entry(groups: &mut Vec<SidebarWorkGroup>, entry: AgentPanelEntry) {
    match work_group_index(groups, UNLINKED_GROUP_KEY) {
        Some(index) => groups[index].entries.push(entry),
        None => groups.push(SidebarWorkGroup {
            key: UNLINKED_GROUP_KEY.into(),
            title: "unlinked".into(),
            entries: vec![entry],
            unlinked: true,
            status: None,
            created_at: None,
            activation: None,
        }),
    }
}

/// Group panes by the work item they are bound to, joined with the work items
/// the projection knows about. Work items without a pane stay in the list; the
/// caller renders them dim. Linked groups keep projection or first-seen order,
/// the unlinked group stays last, and rows keep workspace and tab order. Focus
/// and lifecycle state never participate in this ordering.
pub(crate) fn sidebar_work_groups(
    app: &AppState,
    entries: &[AgentPanelEntry],
    mode: SidebarGroupMode,
) -> Vec<SidebarWorkGroup> {
    let mut groups: Vec<SidebarWorkGroup> = Vec::new();
    if mode == SidebarGroupMode::RepoPr {
        for item in app
            .work_index_snapshot
            .as_ref()
            .map(|snapshot| snapshot.items.as_slice())
            .unwrap_or_default()
        {
            let Some(url) = item.pr_url.as_deref() else {
                continue;
            };
            if !app
                .sidebar_work_filter
                .matches_github(item, &app.work_index_session)
            {
                continue;
            }
            let key = format!("github:{url}");
            if work_group_index(&groups, &key).is_some() {
                continue;
            }
            groups.push(SidebarWorkGroup {
                key,
                title: github_group_title(app, url, None),
                entries: Vec::new(),
                unlinked: false,
                status: Some(pull_request_status(app, url)),
                created_at: item.created_at,
                activation: github_activation(app, item, url),
            });
        }
    }
    if mode == SidebarGroupMode::LinearTeam {
        for row in sidebar_linear_ticket_rows(app) {
            if !labels_match_sidebar_query(&row.ticket.labels, &app.sidebar_work_filter.query) {
                continue;
            }
            if !pane_bound_ticket(app, &row.ticket.identifier)
                && !app
                    .sidebar_work_filter
                    .matches_linear(&row.ticket, &app.work_index_session)
            {
                continue;
            }
            let key = format!("linear:{}", row.ticket.identifier);
            if work_group_index(&groups, &key).is_some() {
                continue;
            }
            groups.push(SidebarWorkGroup {
                key,
                title: ticket_group_title(&row.ticket),
                entries: Vec::new(),
                unlinked: false,
                status: Some(WorkGroupStatus::from_ticket_state(
                    row.ticket.state.as_deref(),
                )),
                created_at: row.ticket.created_at,
                activation: Some(ticket_activation(app, &row)),
            });
        }
    }
    if mode == SidebarGroupMode::Missive {
        for conversation in app
            .work_index_snapshot
            .as_ref()
            .map(|snapshot| snapshot.conversations.as_slice())
            .unwrap_or_default()
        {
            if !labels_match_sidebar_query(&conversation.labels, &app.sidebar_work_filter.query) {
                continue;
            }
            if !pane_bound_conversation(app, indexed_missive_url(conversation))
                && !app
                    .sidebar_work_filter
                    .matches_missive_conversation(Some(conversation), &app.work_index_session)
            {
                continue;
            }
            let url = indexed_missive_url(conversation);
            let key = format!("missive:{url}");
            if work_group_index(&groups, &key).is_some() {
                continue;
            }
            groups.push(SidebarWorkGroup {
                key,
                title: missive_group_title(app, url),
                entries: Vec::new(),
                unlinked: false,
                status: Some(WorkGroupStatus::from_conversation(
                    conversation.closed,
                    !conversation.assignees.is_empty(),
                )),
                created_at: conversation.last_activity_at,
                activation: Some(SidebarWorkGroupActivation {
                    spawn_prompt: format!("{}\n{url}", missive_prompt(app, url)),
                    object_link: url.to_string(),
                    directory: None,
                    git_ref: None,
                    pr: None,
                    ticket: None,
                    missive: Some(crate::app::home::HomeMissiveContext {
                        app_url: conversation.app_url.clone(),
                        web_url: conversation.web_url.clone(),
                        subject: conversation.subject.clone(),
                    }),
                    work_context_patch: crate::work_context::PaneWorkContextPatch {
                        missive_urls: Some(vec![url.to_string()]),
                        work_title: Some(conversation.subject.clone()),
                        ..Default::default()
                    },
                }),
            });
        }
    }
    for entry in ordered_tab_entries(entries) {
        let context = entry_work_context(app, &entry);
        match mode {
            SidebarGroupMode::RepoPr => {
                let urls = preferred_pr_urls(app, &entry);
                if urls.is_empty() {
                    match context.and_then(|context| context.branch.as_deref()) {
                        Some(branch) => push_branch_entry(
                            &mut groups,
                            context.and_then(|context| context.repo.as_deref()),
                            branch,
                            entry,
                        ),
                        None => push_unlinked_entry(app, &mut groups, entry),
                    }
                    continue;
                }
                for url in urls {
                    let key = format!("github:{url}");
                    let index = match work_group_index(&groups, &key) {
                        Some(index) => index,
                        None => {
                            let item = indexed_pull_request(app, &url);
                            groups.push(SidebarWorkGroup {
                                key,
                                title: github_group_title(
                                    app,
                                    &url,
                                    context.and_then(|context| context.work_title.as_deref()),
                                ),
                                entries: Vec::new(),
                                unlinked: false,
                                status: Some(pull_request_status(app, &url)),
                                created_at: item.and_then(|item| item.created_at),
                                activation: item
                                    .and_then(|item| github_activation(app, item, &url)),
                            });
                            groups.len() - 1
                        }
                    };
                    if indexed_pull_request(app, &url)
                        .and_then(|item| item.pr_title.as_deref())
                        .is_none()
                    {
                        let fallback = context.and_then(|context| {
                            context
                                .work_title
                                .as_deref()
                                .or(context.session_name.as_deref())
                        });
                        groups[index].title = github_group_title(app, &url, fallback);
                    }
                    groups[index].entries.push(entry.clone());
                }
            }
            SidebarGroupMode::LinearTeam => {
                // A pane on several tickets is work on each of them, so it is
                // listed under every ticket header it belongs to.
                let ticket_ids = preferred_ticket_ids(app, &entry);
                if ticket_ids.is_empty() {
                    if !sidebar_query_has_labels(&app.sidebar_work_filter.query) {
                        push_linear_unlinked_entry(&mut groups, entry);
                    }
                    continue;
                }
                for ticket_id in &ticket_ids {
                    let key = format!("linear:{ticket_id}");
                    let index = match work_group_index(&groups, &key) {
                        Some(index) => index,
                        None if !sidebar_query_has_labels(&app.sidebar_work_filter.query) => {
                            groups.push(SidebarWorkGroup {
                                key,
                                title: work_group_header_title(
                                    ticket_id,
                                    context.and_then(|context| {
                                        context
                                            .work_title
                                            .as_deref()
                                            .or(context.session_name.as_deref())
                                    }),
                                ),
                                entries: Vec::new(),
                                unlinked: false,
                                status: Some(WorkGroupStatus::from_ticket_state(None)),
                                created_at: None,
                                activation: None,
                            });
                            groups.len() - 1
                        }
                        None => continue,
                    };
                    groups[index].entries.push(entry.clone());
                }
            }
            SidebarGroupMode::Missive => {
                let urls = context
                    .map(|context| context.missive_urls.as_slice())
                    .unwrap_or_default();
                if urls.is_empty() {
                    if !sidebar_query_has_labels(&app.sidebar_work_filter.query) {
                        push_unlinked_entry(app, &mut groups, entry);
                    }
                    continue;
                }
                // A pane replying in several conversations belongs under each
                // of them.
                for url in urls {
                    let conversation = indexed_missive_conversation(app, url);
                    if !conversation.is_some_and(|conversation| {
                        labels_match_sidebar_query(
                            &conversation.labels,
                            &app.sidebar_work_filter.query,
                        )
                    }) && sidebar_query_has_labels(&app.sidebar_work_filter.query)
                    {
                        continue;
                    }
                    let group_url = conversation.map(indexed_missive_url).unwrap_or(url);
                    let key = format!("missive:{group_url}");
                    let index = match work_group_index(&groups, &key) {
                        Some(index) => index,
                        None => {
                            groups.push(SidebarWorkGroup {
                                key,
                                title: missive_group_title(app, group_url),
                                entries: Vec::new(),
                                unlinked: false,
                                status: Some(missive_group_status(app, group_url)),
                                created_at: conversation
                                    .and_then(|conversation| conversation.last_activity_at),
                                activation: Some(SidebarWorkGroupActivation {
                                    spawn_prompt: format!(
                                        "{}\n{group_url}",
                                        missive_prompt(app, group_url)
                                    ),
                                    object_link: group_url.to_string(),
                                    directory: None,
                                    git_ref: None,
                                    pr: None,
                                    ticket: None,
                                    missive: conversation.map(|conversation| {
                                        crate::app::home::HomeMissiveContext {
                                            app_url: conversation.app_url.clone(),
                                            web_url: conversation.web_url.clone(),
                                            subject: conversation.subject.clone(),
                                        }
                                    }),
                                    work_context_patch: crate::work_context::PaneWorkContextPatch {
                                        missive_urls: Some(vec![group_url.to_string()]),
                                        work_title: Some(missive_subject(app, group_url)),
                                        ..Default::default()
                                    },
                                }),
                            });
                            groups.len() - 1
                        }
                    };
                    groups[index].entries.push(entry.clone());
                }
            }
            SidebarGroupMode::Repo | SidebarGroupMode::RepoWorktree | SidebarGroupMode::Spaces => {}
        }
    }
    disambiguate_unlinked_titles(
        &mut groups,
        |group| {
            group
                .unlinked
                .then(|| group.entries.first())
                .flatten()
                .and_then(|entry| unlinked_group_directory(app, entry))
        },
        |group| &mut group.title,
    );
    disambiguate_branch_titles(
        &mut groups,
        |group| group.key.clone(),
        |group| &mut group.title,
    );
    groups.sort_by_key(unlinked_sort_key);
    groups
}

pub(crate) const UNASSIGNED_SECTION_TITLE: &str = "Unassigned";
pub(crate) const NO_AGENT_YET_SECTION_TITLE: &str = "No agent yet";
const UNASSIGNED_INITIAL_ROWS: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SidebarUnassignedObject {
    pub(crate) key: String,
    pub(crate) title: String,
    pub(crate) created_at: Option<std::time::SystemTime>,
    pub(crate) status: Option<WorkGroupStatus>,
    pub(crate) activation: SidebarWorkGroupActivation,
}

fn repo_directory(app: &AppState, repo: &str) -> Option<std::path::PathBuf> {
    app.workspaces
        .iter()
        .find(|workspace| {
            workspace
                .repo_binding
                .as_deref()
                .is_some_and(|candidate| crate::work_context::repo_slugs_match(candidate, repo))
        })
        .map(|workspace| workspace.identity_cwd.clone())
        .or_else(|| {
            app.workspaces
                .iter()
                .flat_map(|workspace| workspace.tabs.iter())
                .flat_map(|tab| tab.panes.values())
                .filter_map(|pane| app.terminals.get(&pane.attached_terminal_id))
                .find(|terminal| {
                    terminal
                        .effective_work_context()
                        .repo
                        .as_deref()
                        .is_some_and(|candidate| {
                            crate::work_context::repo_slugs_match(candidate, repo)
                        })
                })
                .map(|terminal| terminal.cwd.clone())
        })
}

fn work_item_prompt(item: &crate::work_index::WorkItem, link: &str) -> String {
    let heading = match (item.pr_number, item.pr_title.as_deref()) {
        (Some(number), Some(title)) => format!("{}#{number}: {title}", item.repo),
        (Some(number), None) => format!("{}#{number}", item.repo),
        _ => item.repo.clone(),
    };
    format!("{heading}\n{link}")
}

fn github_activation(
    app: &AppState,
    item: &crate::work_index::WorkItem,
    url: &str,
) -> Option<SidebarWorkGroupActivation> {
    let number = item.pr_number?;
    let prompt = work_item_prompt(item, url);
    Some(SidebarWorkGroupActivation {
        spawn_prompt: prompt,
        object_link: url.to_string(),
        directory: repo_directory(app, &item.repo),
        git_ref: item
            .branch
            .as_ref()
            .map(|branch| crate::app::home_refs::HomeRef {
                name: branch.clone(),
                oid: String::new(),
                tag: None,
            }),
        pr: Some(crate::app::home::HomePrContext {
            url: url.to_string(),
            number,
            repo: item.repo.clone(),
        }),
        ticket: None,
        missive: None,
        work_context_patch: crate::work_context::PaneWorkContextPatch {
            pr_urls: Some(vec![url.to_string()]),
            repo: Some(item.repo.clone()),
            branch: item.branch.clone(),
            work_title: item.pr_title.clone(),
            ..Default::default()
        },
    })
}

/// Provider objects with no pane binding, newest creation first. The work
/// index owns the objects; this function only projects them into one client's
/// current sidebar view.
pub(crate) fn sidebar_unassigned_objects(
    app: &AppState,
    entries: &[AgentPanelEntry],
    mode: SidebarGroupMode,
) -> Vec<SidebarUnassignedObject> {
    let mut objects: Vec<SidebarUnassignedObject> = match mode {
        SidebarGroupMode::LinearTeam => sidebar_work_groups(app, entries, mode)
            .into_iter()
            .filter(|group| group.entries.is_empty() && !group.unlinked)
            .filter_map(|group| {
                let identifier = group.key.strip_prefix("linear:")?;
                let ticket = app
                    .work_index_snapshot
                    .as_ref()?
                    .items
                    .iter()
                    .flat_map(|item| item.ticket_details.iter())
                    .find(|ticket| ticket.identifier.eq_ignore_ascii_case(identifier))?;
                if !app
                    .sidebar_work_filter
                    .matches_linear(ticket, &app.work_index_session)
                {
                    return None;
                }
                Some(SidebarUnassignedObject {
                    key: group.key,
                    title: group.title,
                    created_at: group.created_at,
                    status: group.status,
                    activation: group.activation?,
                })
            })
            .collect(),
        SidebarGroupMode::RepoPr => app
            .work_index_snapshot
            .as_ref()
            .into_iter()
            .flat_map(|snapshot| snapshot.items.iter())
            .filter(|item| item.source.github)
            .filter(|item| {
                app.sidebar_work_filter
                    .matches_github(item, &app.work_index_session)
            })
            .filter_map(|item| {
                let url = item.pr_url.as_ref()?;
                let linked = context_panes(app)
                    .any(|context| context.pr_urls.iter().any(|candidate| candidate == url));
                if linked {
                    return None;
                }
                let number = item.pr_number?;
                let title = item
                    .pr_title
                    .as_deref()
                    .map(|title| format!("#{number} {title}"))
                    .unwrap_or_else(|| format!("#{number}"));
                Some(SidebarUnassignedObject {
                    key: format!("github:{url}"),
                    title,
                    created_at: item.created_at,
                    status: Some(WorkGroupStatus::from_pull_request(
                        item.pr_state.as_deref(),
                        item.draft,
                    )),
                    activation: github_activation(app, item, url)?,
                })
            })
            .collect(),
        SidebarGroupMode::Repo | SidebarGroupMode::RepoWorktree | SidebarGroupMode::Spaces => {
            let mut by_repo = std::collections::HashMap::<
                String,
                (Option<std::time::SystemTime>, String, std::path::PathBuf),
            >::new();
            for item in app
                .work_index_snapshot
                .as_ref()
                .into_iter()
                .flat_map(|snapshot| snapshot.items.iter())
                .filter(|item| !item.repo.is_empty())
            {
                let linked = context_panes(app).any(|context| {
                    context.repo.as_deref().is_some_and(|candidate| {
                        crate::work_context::repo_slugs_match(candidate, &item.repo)
                    })
                });
                if linked {
                    continue;
                }
                let Some(directory) = repo_directory(app, &item.repo) else {
                    continue;
                };
                let link = directory.display().to_string();
                by_repo
                    .entry(item.repo.clone())
                    .and_modify(|(created_at, _, _)| {
                        if item.created_at > *created_at {
                            *created_at = item.created_at;
                        }
                    })
                    .or_insert((item.created_at, link, directory));
            }
            by_repo
                .into_iter()
                .map(
                    |(repo, (created_at, link, directory))| SidebarUnassignedObject {
                        key: format!("repo:{repo}"),
                        title: repo.clone(),
                        created_at,
                        status: None,
                        activation: SidebarWorkGroupActivation {
                            spawn_prompt: link.clone(),
                            object_link: link,
                            directory: Some(directory),
                            git_ref: None,
                            pr: None,
                            ticket: None,
                            missive: None,
                            work_context_patch: crate::work_context::PaneWorkContextPatch {
                                repo: Some(repo),
                                ..Default::default()
                            },
                        },
                    },
                )
                .collect()
        }
        SidebarGroupMode::Missive => sidebar_work_groups(app, entries, mode)
            .into_iter()
            .filter(|group| group.entries.is_empty() && !group.unlinked)
            .filter_map(|group| {
                let url = group.key.strip_prefix("missive:")?;
                let conversation = indexed_missive_conversation(app, url)?;
                if !app
                    .sidebar_work_filter
                    .matches_missive_conversation(Some(conversation), &app.work_index_session)
                {
                    return None;
                }
                Some(SidebarUnassignedObject {
                    key: group.key,
                    title: group.title,
                    created_at: group.created_at,
                    status: group.status,
                    activation: group.activation?,
                })
            })
            .collect(),
    };
    objects.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.key.cmp(&right.key))
    });
    let (terms, _) = sidebar_query_parts(&app.sidebar_work_filter.query);
    objects.retain(|object| {
        let haystack =
            format!("{} {}", object.title, object.activation.object_link).to_ascii_lowercase();
        terms
            .iter()
            .all(|term| haystack.contains(&term.to_ascii_lowercase()))
    });
    objects
}

pub(crate) fn sidebar_show_more_key(mode: SidebarGroupMode) -> String {
    format!("unassigned-more:{}", mode.collapse_namespace())
}

fn append_unassigned_rows(app: &AppState, rows: &mut Vec<SidebarRow>, entries: &[AgentPanelEntry]) {
    let objects = sidebar_unassigned_objects(app, entries, app.sidebar_group_mode);
    if objects.is_empty()
        && !matches!(
            app.sidebar_group_mode,
            SidebarGroupMode::LinearTeam | SidebarGroupMode::RepoPr | SidebarGroupMode::Missive
        )
    {
        return;
    }
    let title = match app.sidebar_group_mode {
        SidebarGroupMode::LinearTeam | SidebarGroupMode::RepoPr | SidebarGroupMode::Missive => {
            NO_AGENT_YET_SECTION_TITLE
        }
        SidebarGroupMode::Repo | SidebarGroupMode::RepoWorktree | SidebarGroupMode::Spaces => {
            UNASSIGNED_SECTION_TITLE
        }
    };
    let collapsed = section_is_collapsed(app, title);
    rows.push(SidebarRow::SectionHeader {
        title,
        count: objects.len(),
        collapsed,
    });
    if collapsed {
        return;
    }
    if objects.is_empty() {
        rows.push(SidebarRow::NestedHeader {
            key: format!(
                "unassigned-empty:{}",
                app.sidebar_group_mode.collapse_namespace()
            ),
            action_key: None,
            title: unassigned_empty_text(app),
            count: 0,
            collapsed: false,
            dim: true,
            status: None,
            spawn: false,
        });
        return;
    }
    let expanded = app
        .sidebar_unassigned_expanded_views
        .contains(&app.sidebar_group_mode);
    let shown = if expanded {
        objects.len()
    } else {
        objects.len().min(UNASSIGNED_INITIAL_ROWS)
    };
    for object in objects.iter().take(shown) {
        let action_key = (object.key.starts_with("linear:")
            || object.key.starts_with("github:")
            || object.key.starts_with("missive:"))
        .then(|| object.key.clone());
        rows.push(SidebarRow::NestedHeader {
            key: object.key.clone(),
            action_key,
            title: object.title.clone(),
            count: 0,
            collapsed: false,
            dim: true,
            status: object.status,
            spawn: true,
        });
    }
    let remaining = objects.len().saturating_sub(shown);
    if remaining > 0 {
        rows.push(SidebarRow::NestedHeader {
            key: sidebar_show_more_key(app.sidebar_group_mode),
            action_key: None,
            title: format!("show {remaining} more…"),
            count: 0,
            collapsed: false,
            dim: true,
            status: None,
            spawn: false,
        });
    }
}

fn unassigned_empty_text(app: &AppState) -> String {
    let ownership_label = |ownership: crate::app::state::WorkOwnershipFilter,
                           authored: &'static str,
                           assigned: &'static str,
                           both: &'static str| {
        match ownership {
            crate::app::state::WorkOwnershipFilter::Assigned => assigned,
            crate::app::state::WorkOwnershipFilter::Authored => authored,
            crate::app::state::WorkOwnershipFilter::Both => both,
        }
    };
    let (source, fallback) = match app.sidebar_group_mode {
        SidebarGroupMode::RepoPr => (
            crate::work_index::WorkIndexSource::Github,
            format!(
                "no {} PRs for {} · {}",
                app.sidebar_work_filter.github.state.label(),
                app.sidebar_work_filter
                    .github
                    .assignee
                    .as_deref()
                    .unwrap_or("anyone"),
                ownership_label(
                    app.sidebar_work_filter.github.ownership,
                    "author",
                    "assignee",
                    "author or assignee"
                )
            ),
        ),
        SidebarGroupMode::LinearTeam => (
            crate::work_index::WorkIndexSource::Linear,
            format!(
                "no active tickets for {} · {}",
                app.sidebar_work_filter
                    .assignee
                    .as_deref()
                    .unwrap_or("anyone"),
                ownership_label(
                    app.sidebar_work_filter.linear_ownership,
                    "creator",
                    "assignee",
                    "creator or assignee"
                )
            ),
        ),
        SidebarGroupMode::Missive => (
            crate::work_index::WorkIndexSource::Missive,
            format!(
                "no open conversations for {} · assignee",
                app.sidebar_work_filter
                    .missive
                    .assignee
                    .as_deref()
                    .unwrap_or("anyone")
            ),
        ),
        SidebarGroupMode::Repo | SidebarGroupMode::RepoWorktree | SidebarGroupMode::Spaces => {
            return String::new()
        }
    };
    app.work_index_snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.unavailable_reason(source))
        .map(|reason| format!("{}: {reason}", source.label()))
        .unwrap_or(fallback)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SidebarFilterOption {
    LinearTeam(Option<String>),
    LinearOwnership(crate::app::state::WorkOwnershipFilter),
    LinearAssignee(Option<String>),
    LinearStatus(crate::app::state::LinearStatusFilter, bool),
    GithubAssignee(Option<String>),
    GithubOwnership(crate::app::state::WorkOwnershipFilter),
    GithubDrafts(bool),
    GithubState(crate::app::state::GithubStateFilter),
    MissiveTeam(Option<String>),
    MissiveAssignee(Option<String>),
    MissiveClosed(bool),
}

impl SidebarFilterOption {
    pub(crate) fn label(&self) -> String {
        match self {
            Self::LinearTeam(None) => "team: all".into(),
            Self::LinearTeam(Some(team)) => format!("team: {team}"),
            Self::LinearOwnership(scope) => format!("me: {}", scope.label()),
            Self::LinearAssignee(None) => "assignee: all".into(),
            Self::LinearAssignee(Some(assignee)) => format!("assignee: {assignee}"),
            Self::LinearStatus(status, selected) => format!(
                "{} status: {}",
                if *selected { "[x]" } else { "[ ]" },
                status.label()
            ),
            Self::GithubAssignee(None) => "assignee: all".into(),
            Self::GithubAssignee(Some(assignee)) => format!("assignee: {assignee}"),
            Self::GithubOwnership(scope) => format!("me: {}", scope.label()),
            Self::GithubDrafts(shown) => {
                format!("{} show drafts", if *shown { "[x]" } else { "[ ]" })
            }
            Self::GithubState(state) => format!("state: {}", state.label()),
            Self::MissiveTeam(None) => "team: all".into(),
            Self::MissiveTeam(Some(team)) => format!("team: {team}"),
            Self::MissiveAssignee(None) => "assignee: all".into(),
            Self::MissiveAssignee(Some(assignee)) => format!("assignee: {assignee}"),
            Self::MissiveClosed(shown) => {
                format!("{} show closed", if *shown { "[x]" } else { "[ ]" })
            }
        }
    }
}

pub(crate) fn sidebar_filter_options(app: &AppState) -> Vec<SidebarFilterOption> {
    match app.sidebar_group_mode {
        SidebarGroupMode::LinearTeam => {
            let mut teams = sidebar_linear_ticket_rows(app)
                .iter()
                .filter_map(|row| ticket_team(&row.ticket.identifier))
                .collect::<Vec<_>>();
            if let Some(team) = app.sidebar_work_filter.team.clone() {
                teams.push(team);
            }
            teams.sort();
            teams.dedup();
            let mut options = vec![SidebarFilterOption::LinearTeam(None)];
            options.extend(
                teams
                    .into_iter()
                    .map(|team| SidebarFilterOption::LinearTeam(Some(team))),
            );
            options.push(SidebarFilterOption::LinearAssignee(Some("me".into())));
            options.extend(
                crate::app::state::WorkOwnershipFilter::ALL
                    .into_iter()
                    .map(SidebarFilterOption::LinearOwnership),
            );
            options.push(SidebarFilterOption::LinearAssignee(None));
            options.extend(
                app.work_index_session
                    .linear
                    .assignees
                    .iter()
                    .filter(|assignee| assignee.as_str() != "me")
                    .cloned()
                    .map(|assignee| SidebarFilterOption::LinearAssignee(Some(assignee))),
            );
            options.extend(
                crate::app::state::LinearStatusFilter::ALL
                    .into_iter()
                    .map(|status| {
                        SidebarFilterOption::LinearStatus(
                            status,
                            app.sidebar_work_filter.linear_statuses.contains(&status),
                        )
                    }),
            );
            options
        }
        SidebarGroupMode::RepoPr => {
            let mut options = vec![SidebarFilterOption::GithubAssignee(Some("me".into()))];
            options.extend(
                crate::app::state::WorkOwnershipFilter::ALL
                    .into_iter()
                    .map(SidebarFilterOption::GithubOwnership),
            );
            options.push(SidebarFilterOption::GithubAssignee(None));
            options.extend(
                app.work_index_session
                    .github
                    .assignees
                    .iter()
                    .filter(|assignee| assignee.as_str() != "me")
                    .cloned()
                    .map(|assignee| SidebarFilterOption::GithubAssignee(Some(assignee))),
            );
            options.push(SidebarFilterOption::GithubDrafts(
                app.sidebar_work_filter.github.show_drafts,
            ));
            options.extend(
                crate::app::state::GithubStateFilter::ALL
                    .into_iter()
                    .map(SidebarFilterOption::GithubState),
            );
            options
        }
        SidebarGroupMode::Missive => {
            let mut teams = app
                .work_index_snapshot
                .as_ref()
                .into_iter()
                .flat_map(|snapshot| snapshot.conversations.iter())
                .filter_map(|conversation| conversation.team.as_ref().map(|team| team.name.clone()))
                .collect::<Vec<_>>();
            if let Some(team) = app.sidebar_work_filter.missive.team.clone() {
                teams.push(team);
            }
            teams.sort();
            teams.dedup();
            let mut options = vec![SidebarFilterOption::MissiveTeam(None)];
            options.extend(
                teams
                    .into_iter()
                    .map(|team| SidebarFilterOption::MissiveTeam(Some(team))),
            );
            options.extend([
                SidebarFilterOption::MissiveAssignee(Some("me".into())),
                SidebarFilterOption::MissiveAssignee(None),
            ]);
            options.extend(
                app.work_index_session
                    .missive
                    .assignees
                    .iter()
                    .filter(|assignee| {
                        app.work_index_session.missive.viewer.as_deref() != Some(assignee.as_str())
                    })
                    .cloned()
                    .map(|assignee| SidebarFilterOption::MissiveAssignee(Some(assignee))),
            );
            options.push(SidebarFilterOption::MissiveClosed(
                app.sidebar_work_filter.missive.show_closed,
            ));
            options
        }
        SidebarGroupMode::Repo | SidebarGroupMode::RepoWorktree | SidebarGroupMode::Spaces => {
            Vec::new()
        }
    }
}

/// The Symphony section is omitted entirely when no workflow is open, so a
/// user who never runs Symphony never pays a row for it. A runtime that is
/// merely unreachable stays silent too: the Symphony window reports that, and
/// a permanent error row in the sidebar would be noise on every frame.
fn append_symphony_rows(app: &AppState, rows: &mut Vec<SidebarRow>) {
    let workflows = &app.symphony_snapshot.workflows;
    // An unreachable or not-yet-polled runner stays silent: there is nothing
    // truthful to say about jobs we could not ask about. A reachable runner
    // keeps its header even at zero, so an empty Symphony is distinguishable
    // from a missing one.
    if workflows.is_empty() && !app.symphony_snapshot.is_reachable() {
        return;
    }
    let collapsed = section_is_collapsed(app, SYMPHONY_SECTION_TITLE);
    rows.push(SidebarRow::SectionHeader {
        title: SYMPHONY_SECTION_TITLE,
        count: workflows.len(),
        collapsed,
    });
    if collapsed {
        return;
    }
    if workflows.is_empty() {
        rows.push(SidebarRow::SymphonyEmpty);
        return;
    }
    rows.extend(
        workflows
            .iter()
            .enumerate()
            .map(|(index, workflow)| SidebarRow::SymphonyJob {
                index,
                name: workflow.name.clone(),
                phase: workflow.phase.clone(),
                wait: workflow.wait.clone(),
                started_at: workflow.started_at.clone(),
            }),
    );
}

fn append_recently_done_rows(
    app: &AppState,
    rows: &mut Vec<SidebarRow>,
    entries: Vec<AgentPanelEntry>,
) {
    if entries.is_empty() {
        return;
    }
    let collapsed = section_is_collapsed(app, RECENTLY_DONE_SECTION_TITLE);
    rows.push(SidebarRow::SectionHeader {
        title: RECENTLY_DONE_SECTION_TITLE,
        count: entries.len(),
        collapsed,
    });
    if !collapsed {
        rows.extend(entries.into_iter().map(|entry| SidebarRow::Agent {
            entry: Box::new(entry),
            depth: 0,
        }));
    }
}

fn entry_is_past_done_hide_threshold(app: &AppState, entry: &AgentPanelEntry) -> bool {
    entry.has_agent
        && !entry.seen
        && !entry.stale
        // Share the lifecycle definition of a stopped session. Hiding a row the
        // sidebar would otherwise paint red is how a question stops being asked.
        && crate::terminal::state::session_is_quiet(
            entry.state,
            entry_has_gate(entry) || entry.usage_limited,
            entry.active_subagents,
            entry.holds_shell,
        )
        && entry.done_since.is_some_and(|done_since| {
            app.view_observed_at.saturating_duration_since(done_since) > app.hide_done_after
        })
}

pub(super) fn sidebar_space_member_indices(app: &AppState, root_idx: usize) -> Vec<usize> {
    if workspace_parent_group_state(app, root_idx).is_none() {
        return vec![root_idx];
    }
    // Resolve the group once and compare against it. This runs per Space inside
    // render-path loops, so a per-member group lookup would make the sidebar's
    // label pass cubic in the number of Spaces.
    let Some((ident, _)) = workspace_group_ident(app, root_idx) else {
        return vec![root_idx];
    };
    (0..app.workspaces.len())
        .filter(|idx| workspace_joins_group(app, *idx, &ident))
        .collect()
}

pub(crate) fn normalized_workspace_scroll(app: &AppState, area: Rect, requested: usize) -> usize {
    let ws_area = workspace_list_rect_for_app(app, area);
    let body = workspace_list_body_rect(ws_area, false);
    if body.height == 0 {
        return requested;
    }

    if sidebar_rows(app).is_empty() {
        0
    } else {
        requested.min(workspace_list_bottom_start(app, ws_area))
    }
}

pub(crate) fn workspace_list_entries(app: &AppState) -> Vec<WorkspaceListEntry> {
    workspace_list_entries_inner(app, false, SidebarGroupMode::Repo)
}

/// Like [`workspace_list_entries`] but always expands worktree groups, ignoring
/// `collapsed_space_keys`. The mobile switcher has no collapse affordance and
/// always shows the full worktree tree.
pub(crate) fn workspace_list_entries_expanded(app: &AppState) -> Vec<WorkspaceListEntry> {
    workspace_list_entries_inner(app, true, SidebarGroupMode::Repo)
}

pub(crate) fn workspace_list_entries_for_mode(
    app: &AppState,
    force_expanded: bool,
    mode: SidebarGroupMode,
) -> Vec<WorkspaceListEntry> {
    workspace_list_entries_inner(app, force_expanded, mode)
}

fn workspace_list_entries_inner(
    app: &AppState,
    force_expanded: bool,
    mode: SidebarGroupMode,
) -> Vec<WorkspaceListEntry> {
    let repo_entries = workspace_list_entries_repo(app, force_expanded);
    match mode {
        SidebarGroupMode::Repo => repo_entries,
        // Spaces are the top level here: every Space is its own row, with no
        // repository grouping and no worktree indentation above it.
        SidebarGroupMode::Spaces => (0..app.workspaces.len())
            .map(|ws_idx| WorkspaceListEntry::Workspace {
                ws_idx,
                indented: false,
            })
            .collect(),
        SidebarGroupMode::LinearTeam | SidebarGroupMode::Missive => {
            // Work items cut across repositories, so they are the top level in
            // these modes and the repo tree does not appear at all.
            sidebar_work_groups(app, &sidebar_thread_entries(app), mode)
                .into_iter()
                .map(|group| WorkspaceListEntry::NestedHeader {
                    parent_ws_idx: group.entries.first().map(|entry| entry.ws_idx).unwrap_or(0),
                    key: group.key,
                    title: group.title,
                })
                .collect()
        }
        SidebarGroupMode::RepoPr | SidebarGroupMode::RepoWorktree => {
            let thread_entries = sidebar_thread_entries(app);
            let mut entries = Vec::new();
            for repo_entry in repo_entries {
                let WorkspaceListEntry::Workspace { ws_idx, indented } = repo_entry else {
                    continue;
                };
                if indented {
                    continue;
                }
                entries.push(WorkspaceListEntry::Workspace {
                    ws_idx,
                    indented: false,
                });
                let member_indices = sidebar_space_member_indices(app, ws_idx);
                let members = thread_entries
                    .iter()
                    .filter(|entry| member_indices.contains(&entry.ws_idx))
                    .cloned()
                    .collect::<Vec<_>>();
                for group in sidebar_tab_groups(app, &members, mode) {
                    entries.push(WorkspaceListEntry::NestedHeader {
                        parent_ws_idx: ws_idx,
                        key: group.key,
                        title: group.title,
                    });
                }
            }
            entries
        }
    }
}

fn workspace_list_entries_repo(app: &AppState, force_expanded: bool) -> Vec<WorkspaceListEntry> {
    let keys = (0..app.workspaces.len())
        .map(|ws_idx| workspace_group_ident(app, ws_idx).map(|(ident, home)| (ident.key(), home)))
        .collect::<Vec<_>>();
    let mut members_by_key = std::collections::HashMap::<String, Vec<usize>>::new();
    for (ws_idx, key) in keys.iter().enumerate() {
        if let Some((key, _)) = key {
            members_by_key.entry(key.clone()).or_default().push(ws_idx);
        }
    }
    let grouped_keys = members_by_key
        .iter()
        .filter(|(_, members)| {
            members.len() >= 2
                && members.iter().any(|idx| {
                    keys.get(*idx)
                        .and_then(Option::as_ref)
                        .is_some_and(|(_, home)| *home)
                })
        })
        .map(|(key, _)| key.clone())
        .collect::<std::collections::HashSet<_>>();

    let visible_group_idx = if matches!(app.mode, Mode::Navigate) {
        Some(app.selected)
    } else {
        app.active
    };
    let active_group = visible_group_idx
        .and_then(|idx| keys.get(idx).cloned().flatten())
        .map(|(key, _)| key);

    let mut emitted_groups = std::collections::HashSet::<String>::new();
    let mut entries = Vec::new();
    for ws_idx in 0..app.workspaces.len() {
        let Some(group_key) = keys
            .get(ws_idx)
            .and_then(Option::as_ref)
            .map(|(key, _)| key.clone())
            .filter(|key| grouped_keys.contains(key))
        else {
            entries.push(WorkspaceListEntry::Workspace {
                ws_idx,
                indented: false,
            });
            continue;
        };

        if !emitted_groups.insert(group_key.clone()) {
            continue;
        }

        let Some(members) = members_by_key.get(&group_key) else {
            continue;
        };
        let Some(parent_idx) = members.iter().copied().find(|idx| {
            keys.get(*idx)
                .and_then(Option::as_ref)
                .is_some_and(|(_, home)| *home)
        }) else {
            entries.push(WorkspaceListEntry::Workspace {
                ws_idx,
                indented: false,
            });
            continue;
        };
        let collapsed = !force_expanded && app.collapsed_space_keys.contains(&group_key);
        entries.push(WorkspaceListEntry::Workspace {
            ws_idx: parent_idx,
            indented: false,
        });

        if collapsed {
            if let Some(active_idx) = visible_group_idx
                .filter(|idx| *idx != parent_idx)
                .filter(|_| active_group.as_deref() == Some(group_key.as_str()))
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

/// The sidebar's content area minus the idle animation pinned to its bottom.
/// Everything above the animation - the workspace list and the notepad - is
/// laid out inside this, so both shrink with it.
fn expanded_sidebar_body(app: &AppState, area: Rect) -> Rect {
    let content = expanded_sidebar_content(area);
    let animation = crate::ui::hyperspace::animation_height(app, content);
    Rect::new(
        content.x,
        content.y,
        content.width,
        content.height.saturating_sub(animation),
    )
}

/// The workspace list gets the sidebar's content area minus whatever the
/// notepad panel reserved at the bottom, so every row geometry derived from
/// here shrinks with it.
pub(crate) fn workspace_list_rect_for_app(app: &AppState, area: Rect) -> Rect {
    let content = expanded_sidebar_body(app, area);
    let notepad = crate::ui::notepad::notepad_height(app, content);
    Rect::new(
        content.x,
        content.y,
        content.width,
        content.height.saturating_sub(notepad),
    )
}

/// The notepad panel's rows inside the sidebar.
pub(crate) fn sidebar_notepad_rect(app: &AppState, area: Rect) -> Rect {
    crate::ui::notepad::notepad_panel_rect(app, expanded_sidebar_body(app, area))
}

/// The idle animation's box, in the bottom-left corner of the sidebar's content
/// area and below the notepad.
pub(crate) fn sidebar_animation_rect(app: &AppState, area: Rect) -> Rect {
    crate::ui::hyperspace::animation_box_rect(app, expanded_sidebar_content(area))
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

fn agent_entry_height_in_body_at(
    _app: &AppState,
    _entry: &AgentPanelEntry,
    body_height: u16,
    _depth: u16,
) -> u16 {
    u16::from(body_height > 0)
}

fn sidebar_row_height(app: &AppState, row: &SidebarRow, body_height: u16) -> u16 {
    match row {
        SidebarRow::Workspace {
            ws_idx, indented, ..
        } => app
            .workspaces
            .get(*ws_idx)
            .map(|workspace| workspace_row_height_in_body(app, workspace, *indented, body_height))
            .unwrap_or(0),
        SidebarRow::Agent { entry, depth } => {
            agent_entry_height_in_body_at(app, entry, body_height, *depth)
        }
        SidebarRow::Tab { .. }
        | SidebarRow::SectionHeader { .. }
        | SidebarRow::NestedHeader { .. }
        | SidebarRow::SymphonyJob { .. }
        | SidebarRow::SymphonyEmpty => 1,
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
        (SidebarRow::Workspace { .. }, SidebarRow::NestedHeader { .. }) => 0,
        (SidebarRow::NestedHeader { .. }, SidebarRow::Tab { .. }) => 0,
        (SidebarRow::NestedHeader { .. }, SidebarRow::Agent { .. }) => 0,
        (SidebarRow::NestedHeader { .. }, SidebarRow::NestedHeader { .. }) => 0,
        (_, SidebarRow::NestedHeader { .. }) => 0,
        (SidebarRow::NestedHeader { .. }, SidebarRow::Workspace { .. }) => {
            app.sidebar_spaces.row_gap
        }
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
        // Symphony jobs are a dense read-only list, so they hug each other and
        // whatever follows them; the gap before the next header is enough.
        (SidebarRow::SymphonyJob { .. } | SidebarRow::SymphonyEmpty, _)
        | (_, SidebarRow::SymphonyJob { .. } | SidebarRow::SymphonyEmpty) => 0,
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
        SidebarRow::NestedHeader { .. } => false,
        // A Symphony workflow runs on a worker, not in a workspace.
        SidebarRow::SymphonyJob { .. } | SidebarRow::SymphonyEmpty => false,
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
            SidebarRow::Workspace {
                ws_idx, indented, ..
            } => {
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
            SidebarRow::Tab { .. }
            | SidebarRow::SectionHeader { .. }
            | SidebarRow::NestedHeader { .. }
            | SidebarRow::SymphonyJob { .. }
            | SidebarRow::SymphonyEmpty => {}
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct NestedHeaderArea {
    key: String,
    action_key: Option<String>,
    title: String,
    count: usize,
    collapsed: bool,
    dim: bool,
    status: Option<WorkGroupStatus>,
    spawn: bool,
    rect: Rect,
}

fn compute_sidebar_nested_header_areas(app: &AppState, area: Rect) -> Vec<NestedHeaderArea> {
    let ws_area = workspace_list_rect_for_app(app, area);
    let metrics = workspace_list_scroll_metrics(app, ws_area);
    let body = workspace_list_body_rect(ws_area, should_show_scrollbar(metrics));
    let rows = sidebar_rows(app);
    let scroll_skip = app.workspace_scroll.min(metrics.max_offset_from_bottom);
    nested_header_areas_from_rows(app, &rows, body, scroll_skip)
}

/// Header geometry from rows a caller already walked, so a frame that needs
/// both the headers and something else from the same rows pays for one build.
fn nested_header_areas_from_rows(
    app: &AppState,
    rows: &[SidebarRow],
    body: Rect,
    scroll_skip: usize,
) -> Vec<NestedHeaderArea> {
    let mut y = body.y;
    let mut out = Vec::new();
    for (idx, row) in rows.iter().enumerate().skip(scroll_skip) {
        let height = sidebar_row_height(app, row, body.height);
        if y.saturating_add(height) > body.bottom() {
            break;
        }
        if let SidebarRow::NestedHeader {
            key,
            action_key,
            title,
            count,
            collapsed,
            dim,
            status,
            spawn,
        } = row
        {
            out.push(NestedHeaderArea {
                key: key.clone(),
                action_key: action_key.clone(),
                title: title.clone(),
                count: *count,
                collapsed: *collapsed,
                dim: *dim,
                status: *status,
                spawn: *spawn,
                rect: Rect::new(body.x, y, body.width, height),
            });
        }
        y = y
            .saturating_add(height)
            .saturating_add(sidebar_row_gap(app, rows, idx));
    }
    out
}

/// What an agent dot is saying, in the row's own vocabulary. A pane can
/// override the words per status, so an operator who renamed "blocked" sees
/// their own label here rather than ours.
fn agent_dot_tooltip(entry: &AgentPanelEntry) -> String {
    if !entry.has_agent {
        return "No agent".to_string();
    }
    // A usage limit outranks every lifecycle label: no answer releases the
    // pane, only the reset window. A gate blocks only once work has stopped.
    let key = if entry.usage_limited {
        "usage"
    } else if entry_has_gate(entry) && entry.state != AgentState::Working {
        "blocked"
    } else {
        agent_panel_status_key(entry.state, entry.seen)
    };
    if let Some(label) = entry.state_labels.get(key) {
        return label.clone();
    }
    match key {
        "usage" => "Usage limit",
        "blocked" => "Blocked, waiting on you",
        "working" => "Working",
        "done" => "Done, unread",
        "idle" => "Idle",
        _ => "Unknown",
    }
    .to_string()
}

/// Hover explanations for the parts of a sidebar row that are a glyph or a
/// truncation rather than words: status glyphs, agent dots, and work titles the
/// row was too narrow to spell out.
///
/// One walk of the visible rows per frame, on the same geometry the row
/// renderers use, so an anchor cannot drift from what it explains.
pub(crate) fn compute_sidebar_hover_targets(
    app: &AppState,
    area: Rect,
) -> Vec<crate::app::state::SidebarHoverTarget> {
    let ws_area = workspace_list_rect_for_app(app, area);
    if ws_area == Rect::default() {
        return Vec::new();
    }
    let metrics = workspace_list_scroll_metrics(app, ws_area);
    let body = workspace_list_body_rect(ws_area, should_show_scrollbar(metrics));
    if body.width == 0 || body.height == 0 {
        return Vec::new();
    }

    let rows = sidebar_rows(app);
    let scroll_skip = app.workspace_scroll.min(metrics.max_offset_from_bottom);
    let mut visible = Vec::new();
    let mut y = body.y;
    for (idx, row) in rows.iter().enumerate().skip(scroll_skip) {
        let height = sidebar_row_height(app, row, body.height);
        if y.saturating_add(height) > body.bottom() {
            break;
        }
        visible.push((row, y));
        y = y
            .saturating_add(height)
            .saturating_add(sidebar_row_gap(app, &rows, idx));
    }

    // The narrow-view prefix is a property of the whole list, and the row
    // renderers only apply it when a tab card asked for it.
    let narrow_prefix = visible
        .iter()
        .any(|(row, _)| matches!(row, SidebarRow::Tab { .. }))
        .then(|| narrow_view_tab_prefix_from_rows(&rows, usize::from(body.width)))
        .flatten();

    let mut targets = Vec::new();
    for (row, row_y) in visible {
        match row {
            SidebarRow::Agent { entry, depth } | SidebarRow::Tab { entry, depth } => {
                let tab = matches!(row, SidebarRow::Tab { .. });
                let requested_prefix = narrow_prefix.unwrap_or_else(|| usize::from(*depth) * 3 + 1);
                let provider = compact_provider(entry);
                let title = compact_row_title_for_width(
                    compact_row_title(entry, tab),
                    &provider,
                    usize::from(body.width),
                    requested_prefix,
                );
                let prefix =
                    compact_row_widths(title, &provider, usize::from(body.width), requested_prefix)
                        .prefix;
                let Some(rect) = clamp_row_cells(body, row_y, prefix, SIDEBAR_DOT_FIELD_WIDTH)
                else {
                    continue;
                };
                targets.push(crate::app::state::SidebarHoverTarget {
                    rect,
                    label: agent_dot_tooltip(entry),
                });
            }
            _ => {}
        }
    }

    for header in nested_header_areas_from_rows(app, &rows, body, scroll_skip) {
        let spans = nested_header_spans(&header);
        if let (Some(glyph), Some(status)) = (spans.glyph, header.status) {
            if let Some(rect) = clamp_row_cells(
                body,
                header.rect.y,
                spans.prefix_width,
                display_width(glyph),
            ) {
                targets.push(crate::app::state::SidebarHoverTarget {
                    rect,
                    label: status.label().to_string(),
                });
            }
        }
        // The full `<id> · <title>` is only worth a tooltip when the row could
        // not show it; an untruncated title would repeat itself.
        if spans.title_truncated {
            if let Some(rect) = clamp_row_cells(
                body,
                header.rect.y,
                spans.prefix_width + spans.glyph_width,
                display_width(&spans.title),
            ) {
                targets.push(crate::app::state::SidebarHoverTarget {
                    rect,
                    label: header.title.clone(),
                });
            }
        }
    }

    targets
}

/// A cell span inside a sidebar row, clipped to the list body. `None` once the
/// span starts past the right edge or has no width left.
fn clamp_row_cells(body: Rect, row_y: u16, offset: usize, width: usize) -> Option<Rect> {
    if width == 0 || row_y < body.y || row_y >= body.bottom() {
        return None;
    }
    let x = body.x.saturating_add(u16::try_from(offset).ok()?);
    if x >= body.right() {
        return None;
    }
    let width = u16::try_from(width).ok()?.min(body.right() - x);
    (width > 0).then(|| Rect::new(x, row_y, width, 1))
}

pub(crate) fn sidebar_nested_header_at(app: &AppState, row: u16) -> Option<String> {
    compute_sidebar_nested_header_areas(app, app.view.sidebar_rect)
        .into_iter()
        .find(|header| row >= header.rect.y && row < header.rect.bottom())
        .filter(|header| !header.dim)
        .map(|header| header.key)
}

/// Canonical provider object on a nested-header row, independent of its
/// collapse key. Linked and unassigned rows share this identity.
pub(crate) fn sidebar_object_at(app: &AppState, row: u16) -> Option<String> {
    compute_sidebar_nested_header_areas(app, app.view.sidebar_rect)
        .into_iter()
        .find(|header| row >= header.rect.y && row < header.rect.bottom())
        .and_then(|header| header.action_key)
}

/// The trailing ellipsis cell of a Linear, GitHub, or Missive object row.
pub(crate) fn sidebar_object_action_at(app: &AppState, col: u16, row: u16) -> Option<String> {
    compute_sidebar_nested_header_areas(app, app.view.sidebar_rect)
        .into_iter()
        .find(|header| {
            header.action_key.is_some()
                && row >= header.rect.y
                && row < header.rect.bottom()
                && col == header.rect.right().saturating_sub(1)
        })
        .and_then(|header| header.action_key)
}

/// The dim work-item header at this row, if any. Dim headers do not collapse;
/// they select, and `Enter` starts a thread for them.
pub(crate) fn sidebar_dim_header_at(app: &AppState, row: u16) -> Option<String> {
    compute_sidebar_nested_header_areas(app, app.view.sidebar_rect)
        .into_iter()
        .find(|header| row >= header.rect.y && row < header.rect.bottom())
        .filter(|header| header.dim && header.spawn)
        .map(|header| header.key)
}

pub(crate) fn sidebar_show_more_at(app: &AppState, row: u16) -> bool {
    compute_sidebar_nested_header_areas(app, app.view.sidebar_rect)
        .into_iter()
        .any(|header| {
            row >= header.rect.y
                && row < header.rect.bottom()
                && header.key == sidebar_show_more_key(app.sidebar_group_mode)
        })
}

/// The trailing `+` cell of an unassigned object row.
pub(crate) fn sidebar_unassigned_spawn_at(app: &AppState, col: u16, row: u16) -> Option<String> {
    compute_sidebar_nested_header_areas(app, app.view.sidebar_rect)
        .into_iter()
        .find(|header| {
            header.spawn
                && row >= header.rect.y
                && row < header.rect.bottom()
                && col >= header.rect.right().saturating_sub(4)
                && col < header.rect.right().saturating_sub(2)
        })
        .map(|header| header.key)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SymphonyJobArea {
    index: usize,
    name: String,
    phase: String,
    wait: Option<String>,
    started_at: Option<String>,
    rect: Rect,
}

fn compute_symphony_job_areas(app: &AppState, area: Rect) -> Vec<SymphonyJobArea> {
    compute_symphony_areas(app, area).0
}

/// Job rows plus the rect of the empty-state placeholder, walked together so
/// the two can never disagree about where the section sits.
fn compute_symphony_areas(app: &AppState, area: Rect) -> (Vec<SymphonyJobArea>, Option<Rect>) {
    let ws_area = workspace_list_rect_for_app(app, area);
    let metrics = workspace_list_scroll_metrics(app, ws_area);
    let body = workspace_list_body_rect(ws_area, should_show_scrollbar(metrics));
    let mut y = body.y;
    let mut out = Vec::new();
    let mut empty = None;
    let rows = sidebar_rows(app);
    for (idx, row) in rows
        .iter()
        .enumerate()
        .skip(app.workspace_scroll.min(metrics.max_offset_from_bottom))
    {
        let height = sidebar_row_height(app, row, body.height);
        if y.saturating_add(height) > body.bottom() {
            break;
        }
        if matches!(row, SidebarRow::SymphonyEmpty) {
            empty = Some(Rect::new(body.x, y, body.width, height));
        }
        if let SidebarRow::SymphonyJob {
            index,
            name,
            phase,
            wait,
            started_at,
        } = row
        {
            out.push(SymphonyJobArea {
                index: *index,
                name: name.clone(),
                phase: phase.clone(),
                wait: wait.clone(),
                started_at: started_at.clone(),
                rect: Rect::new(body.x, y, body.width, height),
            });
        }
        y = y
            .saturating_add(height)
            .saturating_add(sidebar_row_gap(app, &rows, idx));
    }
    (out, empty)
}

/// Index into the Symphony snapshot for the job row at this screen row, if
/// any. Clicking one opens the Symphony window on that workflow.
pub(crate) fn sidebar_symphony_job_at(app: &AppState, row: u16) -> Option<usize> {
    compute_symphony_job_areas(app, app.view.sidebar_rect)
        .into_iter()
        .find(|job| row >= job.rect.y && row < job.rect.bottom())
        .map(|job| job.index)
}

/// The placeholder row. Deliberately dim and dotless: it names no workflow, so
/// it borrows none of the agent row's state vocabulary.
fn render_symphony_empty(app: &AppState, frame: &mut Frame, rect: Rect) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let indent = SYMPHONY_ROW_DEPTH * 2;
    let text = format!("{}{SYMPHONY_EMPTY_LABEL}", " ".repeat(indent));
    let line = Line::from(Span::styled(
        crate::ui::text::truncate_end(&text, usize::from(rect.width)),
        Style::default()
            .fg(app.palette.overlay0)
            .add_modifier(Modifier::DIM),
    ));
    frame.render_widget(Paragraph::new(line), rect);
}

fn render_symphony_job(
    app: &AppState,
    frame: &mut Frame,
    job: &SymphonyJobArea,
    now: std::time::SystemTime,
) {
    if job.rect.width == 0 || job.rect.height == 0 {
        return;
    }
    let p = &app.palette;
    let state = symphony_job_state(job.wait.as_deref());
    // Exactly the agent row's fields, sized by the agent row's own width rules:
    // dot, title, the status the row is in, and how long it has been in it. A
    // workflow is one more thing that is either running or waiting on you.
    let status = symphony_job_status(job.phase.as_str(), job.wait.as_deref());
    let width = usize::from(job.rect.width);
    let requested_prefix_width = SYMPHONY_ROW_DEPTH * 3 + 1;
    let widths = compact_row_widths(&job.name, &status, width, requested_prefix_width);
    let fixed_width = widths.prefix + SIDEBAR_DOT_FIELD_WIDTH + widths.provider + widths.age;
    let title_width = width.saturating_sub(fixed_width);
    let title = pad_right(&truncate_end(&job.name, title_width), title_width);
    let dot = pad_right(
        compact_dot_for_state(state, true, true, false, false),
        SIDEBAR_DOT_FIELD_WIDTH,
    );
    let status = pad_left(&status, widths.provider);
    let age = if widths.age > 0 {
        pad_left(
            &crate::ui::symphony::age_label_since(job.started_at.as_deref(), now),
            widths.age,
        )
    } else {
        String::new()
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" ".repeat(widths.prefix)),
            Span::styled(dot, Style::default().fg(state_label_color(state, true, p))),
            Span::styled(title, Style::default().fg(p.subtext0)),
            Span::styled(
                status,
                Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
            ),
            Span::styled(
                age,
                Style::default().fg(if state == AgentState::Working {
                    p.blue
                } else {
                    p.overlay0
                }),
            ),
        ])),
        Rect::new(job.rect.x, job.rect.y, job.rect.width, 1),
    );
}

/// Symphony rows sit one level under their section header, like an agent under
/// its space.
const SYMPHONY_ROW_DEPTH: usize = 1;

/// What the row's status column says: the named wait when the job is parked on
/// one, because that is the fact you act on, else the running phase.
fn symphony_job_status(phase: &str, wait: Option<&str>) -> String {
    match wait {
        Some(wait) if !wait.trim().is_empty() => wait.to_string(),
        _ => phase.to_string(),
    }
}

/// A workflow parked on a named wait owes a human an answer, which is exactly
/// what the blocked dot means for an agent row; anything else is running.
fn symphony_job_state(wait: Option<&str>) -> AgentState {
    match wait {
        Some(wait) if !wait.trim().is_empty() => AgentState::Blocked,
        _ => AgentState::Working,
    }
}

/// What a dim work-item header starts, resolved from the current projection.
pub(crate) fn sidebar_work_group_activation(
    app: &AppState,
    key: &str,
) -> Option<SidebarWorkGroupActivation> {
    if let Some(url) = key.strip_prefix("github:") {
        let item = app
            .work_index_snapshot
            .as_ref()?
            .items
            .iter()
            .find(|item| item.pr_url.as_deref() == Some(url))?;
        return github_activation(app, item, url);
    }
    let entries = sidebar_thread_entries(app);
    sidebar_unassigned_objects(app, &entries, app.sidebar_group_mode)
        .into_iter()
        .find(|object| object.key == key)
        .map(|object| object.activation)
        .or_else(|| {
            sidebar_work_groups(app, &entries, app.sidebar_group_mode)
                .into_iter()
                .find(|group| group.key == key)
                .and_then(|group| group.activation)
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarObjectMenuItem {
    PullRequest(PrActionKind),
    Ticket(crate::ui::ticket_actions::TicketAction),
    StartThread,
    CopyMissiveUrl,
}

impl SidebarObjectMenuItem {
    pub(crate) fn label(self) -> String {
        match self {
            Self::PullRequest(_) => String::new(),
            Self::Ticket(action) => action.label(),
            Self::StartThread => "Start thread".into(),
            Self::CopyMissiveUrl => "Open in Missive".into(),
        }
    }
}

fn sidebar_ticket_action_context(
    app: &AppState,
) -> Option<crate::ui::ticket_actions::TicketActionContext> {
    let identifier = sidebar_ticket_target(app)?;
    let ticket = app
        .work_index_snapshot
        .as_ref()?
        .items
        .iter()
        .flat_map(|item| item.ticket_details.iter())
        .find(|ticket| ticket.identifier.eq_ignore_ascii_case(&identifier))?;
    let key = crate::app::state::WorkItemKey {
        repo: String::new(),
        pr_number: None,
        pr_url: None,
        ticket_id: Some(identifier),
    };
    Some(crate::ui::ticket_actions::TicketActionContext::from_ticket(
        ticket,
        app.work_item_detail_cache.get(&key),
        app.work_index_session.linear.viewer.as_deref(),
        app.work_index_session.linear_viewer_identity(),
        crate::ui::dock::pr::focused_pr_key(app).is_some(),
    ))
}

pub(crate) fn sidebar_ticket_action_entries(
    app: &AppState,
) -> Vec<crate::ui::ticket_actions::TicketActionEntry> {
    let Some(menu) = app.sidebar_object_menu.as_ref() else {
        return Vec::new();
    };
    let Some(context) = sidebar_ticket_action_context(app) else {
        return Vec::new();
    };
    let page = match menu.page {
        crate::app::state::SidebarObjectMenuPage::TicketTransitions => {
            crate::ui::ticket_actions::TicketActionMenuPage::Transitions
        }
        crate::app::state::SidebarObjectMenuPage::TicketPriorities => {
            crate::ui::ticket_actions::TicketActionMenuPage::Priorities
        }
        _ => crate::ui::ticket_actions::TicketActionMenuPage::Actions,
    };
    crate::ui::ticket_actions::ticket_action_table(&context, page)
}

pub(crate) fn sidebar_object_menu_items(app: &AppState) -> Vec<SidebarObjectMenuItem> {
    use crate::app::state::SidebarObjectMenuPage;
    let Some(menu) = app.sidebar_object_menu.as_ref() else {
        return Vec::new();
    };
    if menu.page == SidebarObjectMenuPage::Confirmation {
        return Vec::new();
    }
    if menu.target.starts_with("github:") {
        sidebar_pull_request_actions(app)
            .into_iter()
            .map(|action| SidebarObjectMenuItem::PullRequest(action.kind))
            .collect()
    } else if menu.target.starts_with("linear:") {
        sidebar_ticket_action_entries(app)
            .into_iter()
            .map(|entry| SidebarObjectMenuItem::Ticket(entry.action))
            .collect()
    } else if menu.target.starts_with("missive:") {
        vec![
            SidebarObjectMenuItem::CopyMissiveUrl,
            SidebarObjectMenuItem::StartThread,
        ]
    } else {
        Vec::new()
    }
}

pub(crate) fn sidebar_pull_request_actions(app: &AppState) -> Vec<PrAction> {
    let Some(menu) = app.sidebar_object_menu.as_ref() else {
        return Vec::new();
    };
    let Some(target) = menu.target.strip_prefix("github:") else {
        return Vec::new();
    };
    let Some(summary) = app.work_index_snapshot.as_ref().and_then(|snapshot| {
        snapshot
            .items
            .iter()
            .find(|item| item.pr_url.as_deref() == Some(target))
    }) else {
        return Vec::new();
    };
    let key = crate::app::state::WorkItemKey {
        repo: summary.repo.clone(),
        pr_number: summary.pr_number,
        pr_url: summary.pr_url.clone(),
        ticket_id: None,
    };
    PrItem {
        summary,
        cached_detail: app.work_item_detail_cache.get(&key),
        observed_at: std::time::SystemTime::now(),
    }
    .action_table(
        app.pr_merge_method,
        app.work_item_detail_cache
            .get(&key)
            .and_then(|detail| detail.head_ref_name.as_ref())
            .or(summary.branch.as_ref())
            .is_some(),
    )
    .into_iter()
    .filter(|action| matches!(action.placement, PrActionPlacement::Menu { .. }))
    .collect()
}

pub(crate) fn sidebar_pull_request_key(app: &AppState) -> Option<crate::app::state::WorkItemKey> {
    let target = app
        .sidebar_object_menu
        .as_ref()?
        .target
        .strip_prefix("github:")?;
    let item = app
        .work_index_snapshot
        .as_ref()?
        .items
        .iter()
        .find(|item| item.pr_url.as_deref() == Some(target))?;
    Some(crate::app::state::WorkItemKey {
        repo: item.repo.clone(),
        pr_number: item.pr_number,
        pr_url: item.pr_url.clone(),
        ticket_id: None,
    })
}

pub(crate) fn sidebar_ticket_target(app: &AppState) -> Option<String> {
    app.sidebar_object_menu
        .as_ref()?
        .target
        .strip_prefix("linear:")
        .map(str::to_string)
}

pub(crate) fn sidebar_missive_copy_url(app: &AppState) -> Option<String> {
    let target = app
        .sidebar_object_menu
        .as_ref()?
        .target
        .strip_prefix("missive:")?;
    indexed_missive_conversation(app, target)
        .map(|conversation| conversation.app_url.clone())
        .or_else(|| Some(target.to_string()))
}

pub(crate) fn compute_sidebar_section_header_areas(
    app: &AppState,
    area: Rect,
) -> Vec<SectionHeaderArea> {
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
    has_agents: bool,
) -> Rect {
    if !has_agents || card.rect.width < 2 || card.rect.height == 0 {
        return Rect::default();
    }
    Rect::new(card.rect.x.saturating_add(1), card.rect.y, 1, 1)
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

fn workspace_selection_background(p: &Palette, is_active: bool) -> Color {
    if is_active && p.selection_bg == Color::Reset {
        p.active_row_bg
    } else {
        p.selection_bg
    }
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

/// Paint the sidebar's own background before any row renders.
///
/// Without this the sidebar — the widest themed surface in the UI — keeps
/// whatever background the terminal has, so a palette that disagrees with the
/// terminal draws its foregrounds onto a background it never chose. Painting
/// here bounds a mismatched theme to looking wrong instead of unreadable.
fn fill_sidebar_background(frame: &mut Frame, area: Rect, p: &Palette) {
    let bg = p.sidebar_background();
    if bg == Color::Reset {
        return;
    }
    let buf = frame.buffer_mut();
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            buf[(x, y)].set_style(Style::default().bg(bg));
        }
    }
}

pub(super) fn render_sidebar_collapsed(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let is_navigating = matches!(app.mode, Mode::Navigate);

    let p = &app.palette;
    fill_sidebar_background(frame, area, p);
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
            SidebarRow::Workspace {
                ws_idx, indented, ..
            } => {
                let Some(ws) = app.workspaces.get(*ws_idx) else {
                    continue;
                };
                let (agg_state, agg_seen) = ws.aggregate_state(&app.terminals);
                let has_agent = ws.tabs.iter().any(|tab| {
                    tab.panes.values().any(|pane| {
                        app.terminals
                            .get(&pane.attached_terminal_id)
                            .is_some_and(|terminal| terminal.agent_lifecycle_context().is_some())
                    })
                });
                let icon = compact_dot_for_state(agg_state, agg_seen, has_agent, false, false);
                let icon_style = Style::default().fg(if has_agent {
                    state_label_color(agg_state, agg_seen, p)
                } else {
                    p.overlay0
                });
                let is_selected = *ws_idx == app.selected && is_navigating;
                let is_active = Some(*ws_idx) == app.active;
                let selection_bg = workspace_selection_background(p, is_active);
                let row_style = if is_selected {
                    Style::default().bg(selection_bg)
                } else if is_active && is_navigating {
                    Style::default().bg(p.active_row_bg)
                } else {
                    Style::default()
                };
                let num_style = if is_selected {
                    Style::default()
                        .fg(p.text)
                        .bg(selection_bg)
                        .add_modifier(Modifier::BOLD)
                } else if is_active {
                    let style = Style::default().fg(p.text).add_modifier(Modifier::BOLD);
                    if is_navigating {
                        style.bg(p.active_row_bg)
                    } else {
                        style
                    }
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
                    ]))
                    .style(row_style),
                    Rect::new(ws_area.x, y, ws_area.width, 1),
                );
                if is_active && is_navigating {
                    let buf = frame.buffer_mut();
                    for x in ws_area.x..ws_area.x + ws_area.width {
                        buf[(x, y)].set_bg(selection_bg);
                    }
                }
            }
            SidebarRow::Agent { entry, depth } => {
                let icon = compact_row_dot(entry);
                let icon_style = Style::default().fg(compact_row_color(entry, p));
                let is_active = app.is_active_pane(entry.ws_idx, entry.tab_idx, entry.pane_id);
                let row_style = if is_active {
                    Style::default().bg(p.active_row_bg)
                } else {
                    Style::default()
                };
                let position_style = if is_active {
                    Style::default().fg(p.text).bg(p.active_row_bg)
                } else {
                    Style::default().fg(p.overlay0)
                };
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
                            position_style,
                        ),
                        Span::raw(" "),
                        Span::styled(icon, icon_style),
                    ]))
                    .style(row_style),
                    Rect::new(ws_area.x, y, ws_area.width, 1),
                );
                if is_active {
                    let buf = frame.buffer_mut();
                    for x in ws_area.x..ws_area.x + ws_area.width {
                        buf[(x, y)].set_bg(p.active_row_bg);
                    }
                }
                if app.pane_is_settled(entry.ws_idx, entry.pane_id) {
                    dim_settled_row(frame, Rect::new(ws_area.x, y, ws_area.width, 1), p.overlay0);
                }
            }
            SidebarRow::Tab { entry, .. } => {
                let icon = compact_row_dot(entry);
                let icon_style = Style::default().fg(compact_row_color(entry, p));
                let is_active = app.is_active_pane(entry.ws_idx, entry.tab_idx, entry.pane_id);
                let row_style = if is_active {
                    Style::default().bg(p.active_row_bg)
                } else {
                    Style::default()
                };
                let position_style = if is_active {
                    Style::default().fg(p.text).bg(p.active_row_bg)
                } else {
                    Style::default().fg(p.overlay0)
                };
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled("  ", position_style),
                        Span::styled(icon, icon_style),
                    ]))
                    .style(row_style),
                    Rect::new(ws_area.x, y, ws_area.width, 1),
                );
                if is_active {
                    let buf = frame.buffer_mut();
                    for x in ws_area.x..ws_area.x + ws_area.width {
                        buf[(x, y)].set_bg(p.active_row_bg);
                    }
                }
                if app.pane_is_settled(entry.ws_idx, entry.pane_id) {
                    dim_settled_row(frame, Rect::new(ws_area.x, y, ws_area.width, 1), p.overlay0);
                }
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
            SidebarRow::NestedHeader { dim, .. } => {
                let color = if *dim { p.overlay0 } else { p.accent };
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        "─".repeat(usize::from(ws_area.width)),
                        Style::default().fg(color),
                    ))),
                    Rect::new(ws_area.x, y, ws_area.width, 1),
                );
            }
            // Collapsed there is no room for a workflow name; the section rule
            // above already shows that a Symphony run is open.
            SidebarRow::SymphonyJob { .. } | SidebarRow::SymphonyEmpty => {}
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
                WorkspaceListEntry::NestedHeader { .. } => None,
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
        Some(WorkspaceListEntry::NestedHeader { .. }) => {
            crate::app::state::WorkspaceDropTarget::End
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
    fill_sidebar_background(frame, area, p);
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

    let ws_area = workspace_list_rect_for_app(app, area);
    render_workspace_list(app, terminal_runtimes, frame, ws_area, is_navigating);
    crate::ui::notepad::render_notepad(app, frame, sidebar_notepad_rect(app, area));
    crate::ui::hyperspace::render_animation(app, frame, sidebar_animation_rect(app, area));
    render_sidebar_header(app, frame, area, p);
    let settings = sidebar_footer_settings_hit_area(area);
    if settings.width > 0 {
        let style = sidebar_footer_style(
            app,
            crate::app::state::SidebarFooterItem::Settings,
            app.mode == Mode::Settings,
            p,
        );
        frame.render_widget(Paragraph::new(Span::styled("⚙ ", style)), settings);
    }
    let work = sidebar_footer_work_hit_area(area);
    if work.width > 0 {
        let style = sidebar_footer_style(
            app,
            crate::app::state::SidebarFooterItem::PullRequests,
            app.work_view.as_ref().is_some_and(|view| {
                view.projection == crate::app::state::WorkProjection::PullRequests
            }),
            p,
        );
        frame.render_widget(Paragraph::new(Span::styled("⑂ ", style)), work);
    }
    let usage = sidebar_footer_usage_hit_area(area);
    if usage.width > 0 {
        let style = sidebar_footer_style(
            app,
            crate::app::state::SidebarFooterItem::Usage,
            app.usage_view.is_some(),
            p,
        );
        frame.render_widget(Paragraph::new(Span::styled("▥ ", style)), usage);
    }
    let tickets = sidebar_footer_ticket_hit_area(area);
    if tickets.width > 0 {
        let style = sidebar_footer_style(
            app,
            crate::app::state::SidebarFooterItem::Linear,
            app.work_view
                .as_ref()
                .is_some_and(|view| view.projection == crate::app::state::WorkProjection::Tickets),
            p,
        );
        frame.render_widget(Paragraph::new(Span::styled("◎ ", style)), tickets);
    }
    let missive = sidebar_footer_missive_hit_area(area);
    if missive.width > 0 {
        let style = sidebar_footer_style(
            app,
            crate::app::state::SidebarFooterItem::Missive,
            app.work_view
                .as_ref()
                .is_some_and(|view| view.projection == crate::app::state::WorkProjection::Missive),
            p,
        );
        frame.render_widget(Paragraph::new(Span::styled("✉ ", style)), missive);
    }
    crate::ui::pomodoro::render_indicator(
        app,
        frame,
        crate::ui::pomodoro::pomodoro_hit_area(app, area),
        app.view_observed_at,
    );
    let refresh = sidebar_footer_refresh_hit_area(area);
    if refresh.width > 0 {
        let style = sidebar_footer_style(
            app,
            crate::app::state::SidebarFooterItem::Refresh,
            app.sidebar_refreshing,
            p,
        );
        frame.render_widget(Paragraph::new(Span::styled("⟳ ", style)), refresh);
    }
}

fn sidebar_footer_style(
    app: &AppState,
    item: crate::app::state::SidebarFooterItem,
    selected: bool,
    palette: &Palette,
) -> Style {
    if selected {
        Style::default()
            .fg(palette.accent)
            .bg(palette.surface0)
            .add_modifier(Modifier::BOLD)
    } else if app.hovered_control == Some(crate::app::state::ControlId::SidebarFooter(item)) {
        Style::default().fg(palette.text).bg(palette.surface0)
    } else {
        Style::default().fg(palette.overlay0)
    }
}

fn render_sidebar_header(app: &AppState, frame: &mut Frame, area: Rect, p: &Palette) {
    if area.width <= 1 || area.height == 0 {
        return;
    }
    let toggle = expanded_sidebar_toggle_rect(area);
    let search = sidebar_header_search_rect(area);
    let new_thread = sidebar_header_new_thread_rect(area);
    let new_menu = sidebar_header_new_menu_rect(area);
    let star_filter = sidebar_header_star_filter_rect(area);
    let overflow = sidebar_header_overflow_rect(area);
    frame.render_widget(
        Paragraph::new(Span::styled("«", Style::default().fg(p.overlay0))),
        toggle,
    );
    if search.width > 0 {
        let query = app.sidebar_work_filter.query.as_str();
        let text = if query.is_empty() && app.sidebar_search_active {
            "🔍 ▏".to_string()
        } else if query.is_empty() {
            "🔍 Search".to_string()
        } else {
            format!(
                "🔍 {query}{}",
                if app.sidebar_search_active { "▏" } else { "" }
            )
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_end(&text, usize::from(search.width)),
                Style::default().fg(if app.sidebar_search_active {
                    p.text
                } else {
                    p.overlay0
                }),
            )),
            search,
        );
    }
    if star_filter.width > 0 {
        // Filled glyph while the gate is on, hollow while it is off, so the
        // control reads as a state and not just as a button.
        let (glyph, style) = if app.sidebar_starred_only {
            (
                "\u{2605}",
                Style::default().fg(p.yellow).add_modifier(Modifier::BOLD),
            )
        } else {
            ("\u{2606}", Style::default().fg(p.overlay0))
        };
        frame.render_widget(Paragraph::new(Span::styled(glyph, style)), star_filter);
    }
    frame.render_widget(
        Paragraph::new(Span::styled("✎", Style::default().fg(p.accent))),
        new_thread,
    );
    frame.render_widget(
        Paragraph::new(Span::styled("+", Style::default().fg(p.accent))),
        new_menu,
    );
    let mode_anchor = sidebar_group_mode_anchor_rect(area);
    if mode_anchor.width > 0 {
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_end(
                    &sidebar_header_mode_label(app),
                    usize::from(mode_anchor.width),
                ),
                Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
            )),
            mode_anchor,
        );
    }
    let overflow_style = if app.global_menu_attention_badge_visible() {
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.overlay0)
    };
    frame.render_widget(Paragraph::new(Span::styled("…", overflow_style)), overflow);
}

#[cfg(test)]
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
            | ResolvedTokenKind::RequiredStateText(text)
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
    let has_required_state = resolved
        .iter()
        .any(|token| matches!(token.kind, ResolvedTokenKind::RequiredStateText(_)));
    let minimum_flexible_widths = resolved
        .iter()
        .enumerate()
        .map(|(index, token)| match &token.kind {
            ResolvedTokenKind::RequiredStateText(_) => flexible_widths[index].min(8),
            ResolvedTokenKind::Tab(_)
            | ResolvedTokenKind::Pane(_)
            | ResolvedTokenKind::TerminalTitle(_)
                if has_required_state =>
            {
                flexible_widths[index].min(4)
            }
            _ => usize::from(flexible_widths[index] > 0),
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
            .map(|index| fixed_widths[*index] + minimum_flexible_widths[*index])
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
        let mut activation_order = Vec::new();
        if has_required_state {
            activation_order.extend(resolved.iter().enumerate().filter_map(|(index, token)| {
                matches!(token.kind, ResolvedTokenKind::RequiredStateText(_)).then_some(index)
            }));
            activation_order.extend(resolved.iter().enumerate().filter_map(|(index, token)| {
                matches!(
                    token.kind,
                    ResolvedTokenKind::Tab(_)
                        | ResolvedTokenKind::Pane(_)
                        | ResolvedTokenKind::TerminalTitle(_)
                )
                .then_some(index)
            }));
        }
        let remaining = (0..resolved.len())
            .rev()
            .filter(|index| !activation_order.contains(index))
            .collect::<Vec<_>>();
        activation_order.extend(remaining);
        for index in activation_order {
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
        .map(|(index, _)| {
            if active[index] {
                minimum_flexible_widths[index]
            } else {
                0
            }
        })
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
            ResolvedTokenKind::RequiredStateText(text) => {
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

#[cfg(test)]
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

/// Where a nested header's cells land inside its row. Render and hover both
/// read this, so a tooltip anchor can never drift from the glyph or title it
/// points at.
struct NestedHeaderSpans {
    prefix_width: usize,
    glyph: Option<&'static str>,
    /// Cells the glyph occupies including its trailing gap, `0` without one.
    glyph_width: usize,
    title: String,
    title_truncated: bool,
}

fn nested_header_spans(header: &NestedHeaderArea) -> NestedHeaderSpans {
    let count_width = if header.dim {
        0
    } else {
        display_width(&format!(" ({})", header.count))
    };
    let action_width = usize::from(header.action_key.is_some()) * 2;
    let spawn_width = usize::from(header.spawn) * 2;
    let prefix_width = display_width(if header.dim { "   " } else { "  ▸ " });
    // The status glyph sits before the id, so it costs the title its width.
    let glyph = header.status.map(WorkGroupStatus::glyph);
    let glyph_width = glyph.map(|glyph| display_width(glyph) + 1).unwrap_or(0);
    let title = truncate_end(
        &header.title,
        usize::from(header.rect.width)
            .saturating_sub(prefix_width)
            .saturating_sub(glyph_width)
            .saturating_sub(count_width)
            .saturating_sub(action_width)
            .saturating_sub(spawn_width),
    );
    NestedHeaderSpans {
        prefix_width,
        glyph,
        glyph_width,
        title_truncated: display_width(&title) < display_width(&header.title),
        title,
    }
}

fn render_nested_header(app: &AppState, frame: &mut Frame, header: &NestedHeaderArea) {
    if header.rect.width == 0 || header.rect.height == 0 {
        return;
    }
    let p = &app.palette;
    let count_label = (!header.dim).then(|| format!(" ({})", header.count));
    let NestedHeaderSpans { glyph, title, .. } = nested_header_spans(header);
    // A dim header carries no live state colour: nothing is running under it.
    let color = if header.dim { p.overlay0 } else { p.subtext0 };
    let mut spans = vec![Span::raw(if header.dim {
        "   "
    } else if header.collapsed {
        "  ▸ "
    } else {
        "  ▾ "
    })];
    if let Some(glyph) = glyph {
        let status_color = header
            .status
            .map(|status| status.color(p))
            .unwrap_or(p.overlay0);
        spans.push(Span::styled(
            format!("{glyph} "),
            Style::default()
                .fg(status_color)
                .add_modifier(if header.dim {
                    Modifier::DIM
                } else {
                    Modifier::empty()
                }),
        ));
    }
    spans.push(Span::styled(
        title,
        Style::default().fg(color).add_modifier(if header.dim {
            Modifier::DIM
        } else {
            Modifier::BOLD
        }),
    ));
    if let Some(count_label) = count_label {
        spans.push(Span::styled(
            count_label,
            Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
        ));
    }
    if header.spawn {
        spans.push(Span::styled(
            " +",
            Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
        ));
    }
    if header.action_key.is_some() {
        spans.push(Span::styled(
            " …",
            Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
        ));
    }
    let selected = header
        .action_key
        .as_deref()
        .is_some_and(|key| app.sidebar_selected_work_group.as_deref() == Some(key));
    let paragraph = if selected {
        Paragraph::new(Line::from(spans)).style(Style::default().bg(p.surface1))
    } else {
        Paragraph::new(Line::from(spans))
    };
    frame.render_widget(
        paragraph,
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
    let workspace_labels = sidebar_workspace_labels(app, terminal_runtimes);

    let metrics = workspace_list_scroll_metrics(app, area);
    let row_entries = sidebar_rows_from(app, terminal_runtimes);
    let workspace_headers = row_entries
        .iter()
        .skip(app.workspace_scroll.min(metrics.max_offset_from_bottom))
        .filter_map(|row| match row {
            SidebarRow::Workspace { title, count, .. } => Some((title, *count)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let scrollbar_rect = workspace_list_scrollbar_rect(app, area);
    let sidebar_area = Rect::new(area.x, area.y, area.width.saturating_add(1), area.height);
    let computed_cards = compute_workspace_card_areas(app, sidebar_area);
    let cards = &computed_cards;
    for (card_index, card) in cards.iter().enumerate() {
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

        let header = workspace_headers.get(card_index);
        let (display_label, is_derived) =
            header.filter(|(title, _)| !title.is_empty()).map_or_else(
                || {
                    workspace_labels.get(&i).cloned().unwrap_or_else(|| {
                        (
                            ws.display_name_from(&app.terminals, terminal_runtimes),
                            true,
                        )
                    })
                },
                |(title, _)| ((*title).clone(), false),
            );
        let name_style = if selected || is_active || is_dragged {
            Style::default()
                .fg(active_sidebar_title_color(p))
                .add_modifier(Modifier::BOLD)
        } else if is_derived {
            Style::default().fg(p.overlay0).add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(p.subtext0)
        };

        let window_count = member_indices
            .iter()
            .filter_map(|member| app.workspaces.get(*member))
            .map(|workspace| workspace.tabs.len())
            .sum::<usize>();
        let agent_count = sidebar_thread_entries_from(app, terminal_runtimes)
            .into_iter()
            .filter(|entry| member_indices.contains(&entry.ws_idx) && entry.has_agent)
            .count();
        let count_label = header.and_then(|(_, count)| *count).map_or_else(
            || format!(" ({agent_count}/{window_count})"),
            |count| format!(" ({count})"),
        );
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

    let section_headers = compute_sidebar_section_header_areas(app, sidebar_area);
    let has_matching_rows = row_entries.iter().any(|row| {
        matches!(
            row,
            SidebarRow::Workspace { .. }
                | SidebarRow::Tab { .. }
                | SidebarRow::Agent { .. }
                | SidebarRow::NestedHeader { .. }
        )
    });
    if (!has_matching_rows && !app.sidebar_work_filter.query.is_empty())
        || (row_entries.is_empty() && !app.sidebar_shows_spaces_tree())
    {
        let body = workspace_list_body_rect(area, should_show_scrollbar(metrics));
        let empty_y = section_headers
            .iter()
            .map(|header| header.rect.bottom())
            .max()
            .unwrap_or(body.y);
        if body.width > 0 && empty_y < body.bottom() {
            frame.render_widget(
                Paragraph::new(" no matching agents")
                    .style(Style::default().fg(p.overlay0).add_modifier(Modifier::DIM)),
                Rect::new(body.x, empty_y, body.width, 1),
            );
        }
    }
    let agent_cards = if app.view.agent_card_areas.is_empty() {
        compute_agent_card_areas(app, sidebar_area)
    } else {
        app.view.agent_card_areas.clone()
    };
    for header in section_headers {
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
    for header in compute_sidebar_nested_header_areas(app, sidebar_area) {
        render_nested_header(app, frame, &header);
    }
    let tab_cards = compute_tab_card_areas(app, sidebar_area);
    let narrow_prefix = tab_cards
        .first()
        .and_then(|card| narrow_view_tab_prefix(app, usize::from(card.rect.width)));
    // One wall clock for the whole section, so two rows drawn in the same
    // frame can never disagree about how old they are.
    let symphony_now = std::time::SystemTime::now();
    let (symphony_jobs, symphony_empty) = compute_symphony_areas(app, sidebar_area);
    if let Some(rect) = symphony_empty {
        render_symphony_empty(app, frame, rect);
    }
    for job in symphony_jobs {
        render_symphony_job(app, frame, &job, symphony_now);
    }
    for card in tab_cards {
        render_tab_card(app, frame, &card, narrow_prefix, &row_entries);
    }
    for card in agent_cards {
        let Some((entry, depth)) = row_entries.get(card.row_idx).and_then(|row| match row {
            SidebarRow::Agent { entry, depth } => Some((entry, *depth)),
            _ => None,
        }) else {
            continue;
        };
        render_agent_card(app, frame, entry, card.rect, depth, narrow_prefix);
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

fn narrow_view_tab_prefix(app: &AppState, width: usize) -> Option<usize> {
    if width >= SIDEBAR_SPACE_SUFFIX_MIN_ROW_WIDTH {
        return None;
    }
    let rows = sidebar_rows(app);
    narrow_view_tab_prefix_from_rows(&rows, width)
}

fn narrow_view_tab_prefix_from_rows(rows: &[SidebarRow], width: usize) -> Option<usize> {
    let prefixes = rows
        .iter()
        .filter_map(|row| match row {
            SidebarRow::Tab { entry, depth } => Some((entry, depth, true)),
            SidebarRow::Agent { entry, depth } => Some((entry, depth, false)),
            _ => None,
        })
        .map(|(entry, depth, tab)| {
            let requested_prefix = usize::from(*depth) * 3 + 1;
            let provider = compact_provider(entry);
            let title = compact_row_title_for_width(
                compact_row_title(entry, tab),
                &provider,
                width,
                requested_prefix,
            );
            let prefix = compact_row_widths(title, &provider, width, requested_prefix).prefix;
            (prefix, requested_prefix)
        })
        .collect::<Vec<_>>();
    prefixes
        .iter()
        .any(|(prefix, requested)| prefix < requested)
        .then(|| prefixes.iter().map(|(prefix, _)| *prefix).min())
        .flatten()
}

fn render_tab_card(
    app: &AppState,
    frame: &mut Frame,
    card: &crate::app::state::TabCardArea,
    narrow_prefix: Option<usize>,
    rows: &[SidebarRow],
) {
    let entry = rows.iter().find_map(|row| match row {
        SidebarRow::Tab { entry, depth }
            if entry.ws_idx == card.ws_idx && entry.tab_idx == card.tab_idx =>
        {
            Some((entry.as_ref(), *depth))
        }
        _ => None,
    });
    let Some((entry, depth)) = entry else { return };
    render_compact_agent_row_with_prefix(
        app,
        frame,
        entry,
        card.rect,
        depth,
        true,
        None,
        narrow_prefix,
    );
    if app.pane_is_settled(entry.ws_idx, entry.pane_id) {
        dim_settled_row(frame, card.rect, app.palette.overlay0);
        let target = crate::app::state::PaneFocusTarget {
            workspace_id: app.workspaces[entry.ws_idx].id.clone(),
            pane_id: entry.pane_id,
        };
        if app.sidebar_selected_settled.as_ref() == Some(&target) {
            frame
                .buffer_mut()
                .set_style(card.rect, Style::default().bg(app.palette.surface1));
        }
    }
}

fn render_agent_card(
    app: &AppState,
    frame: &mut Frame,
    detail: &AgentPanelEntry,
    rect: Rect,
    depth: u16,
    narrow_prefix: Option<usize>,
) {
    render_compact_agent_row_with_prefix(
        app,
        frame,
        detail,
        rect,
        depth,
        false,
        None,
        narrow_prefix,
    );
    if app.pane_is_settled(detail.ws_idx, detail.pane_id) {
        dim_settled_row(frame, rect, app.palette.overlay0);
    }
}

/// Preserve the frozen row layout, then replace only its foreground styling.
fn dim_settled_row(frame: &mut Frame, rect: Rect, color: ratatui::style::Color) {
    let buffer = frame.buffer_mut();
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            buffer[(x, y)].set_style(Style::default().fg(color).add_modifier(Modifier::DIM));
        }
    }
}

pub(crate) fn visible_tab_activity_instants_from(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    cards: &[crate::app::state::TabCardArea],
) -> Vec<std::time::Instant> {
    let rows = sidebar_rows_from(app, terminal_runtimes);
    let narrow_prefix = cards
        .first()
        .and_then(|card| narrow_view_tab_prefix(app, usize::from(card.rect.width)));
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
                narrow_prefix.unwrap_or_else(|| usize::from(depth) * 3 + 1),
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

pub(crate) fn sidebar_header_new_menu_rect(area: Rect) -> Rect {
    if area.width < 6 || area.height == 0 {
        return Rect::default();
    }
    let trailing_width = if area.width < 20 { 4 } else { 5 };
    Rect::new(
        area.x + area.width.saturating_sub(trailing_width),
        area.y,
        2,
        1,
    )
}

pub(crate) fn sidebar_header_new_thread_rect(area: Rect) -> Rect {
    let next = sidebar_header_new_menu_rect(area);
    if next.width == 0 || next.x < area.x.saturating_add(3) {
        return Rect::default();
    }
    Rect::new(next.x.saturating_sub(3), area.y, 2, 1)
}

/// Header toggle that gates the tree down to starred sessions. Sits in the
/// control strip left of the new-thread icon, so the search box shrinks by its
/// width rather than the icons moving.
pub(crate) fn sidebar_header_star_filter_rect(area: Rect) -> Rect {
    let next = sidebar_header_new_thread_rect(area);
    if next.width == 0 || next.x < area.x.saturating_add(3) {
        return Rect::default();
    }
    Rect::new(next.x.saturating_sub(3), area.y, 2, 1)
}

pub(crate) fn sidebar_header_search_rect(area: Rect) -> Rect {
    let star = sidebar_header_star_filter_rect(area);
    let control = if star.width > 0 {
        star
    } else {
        sidebar_header_new_thread_rect(area)
    };
    if control.width == 0 || control.x <= area.x.saturating_add(1) {
        return Rect::default();
    }
    Rect::new(
        area.x.saturating_add(1),
        area.y,
        control.x.saturating_sub(area.x.saturating_add(2)),
        1,
    )
}

pub(crate) fn sidebar_header_mode_label(app: &AppState) -> String {
    let view = format!("View: {} ▾", app.sidebar_group_mode.view_label());
    let filters = match app.sidebar_group_mode {
        SidebarGroupMode::LinearTeam => Some(app.sidebar_work_filter.linear_label()),
        SidebarGroupMode::RepoPr => Some(app.sidebar_work_filter.github_label()),
        SidebarGroupMode::Missive => Some(app.sidebar_work_filter.missive_label()),
        SidebarGroupMode::Repo | SidebarGroupMode::RepoWorktree | SidebarGroupMode::Spaces => None,
    };
    match filters {
        Some(filters) => format!("{view} · {filters} ▾"),
        None => view,
    }
}

/// The `· <filter> ▾` half of the header line, which opens the filter dropdown.
/// Empty outside the work-item modes, where there is nothing to filter.
pub(crate) fn sidebar_filter_anchor_rect(app: &AppState, area: Rect) -> Rect {
    if matches!(
        app.sidebar_group_mode,
        SidebarGroupMode::Repo | SidebarGroupMode::RepoWorktree | SidebarGroupMode::Spaces
    ) {
        return Rect::default();
    }
    let mode_anchor = sidebar_group_mode_anchor_rect(area);
    if mode_anchor.width == 0 {
        return Rect::default();
    }
    let label = sidebar_header_mode_label(app);
    let Some(offset) = label.find('·').map(|byte| display_width(&label[..byte])) else {
        return Rect::default();
    };
    let offset = u16::try_from(offset).unwrap_or(u16::MAX);
    if offset >= mode_anchor.width {
        return Rect::default();
    }
    Rect::new(
        mode_anchor.x.saturating_add(offset),
        mode_anchor.y,
        mode_anchor.width.saturating_sub(offset),
        1,
    )
}

pub(crate) fn sidebar_group_mode_anchor_rect(area: Rect) -> Rect {
    if area.width < 8 || area.height < 2 {
        return Rect::default();
    }
    let x = area.x.saturating_add(if area.width < 20 { 1 } else { 2 });
    let right = area.right().saturating_sub(2);
    Rect::new(x, area.y.saturating_add(1), right.saturating_sub(x), 1)
}

pub(crate) fn sidebar_new_menu_layout(
    app: &AppState,
    area: Rect,
) -> Option<super::dropdown::DropdownLayout> {
    let menu = app.sidebar_new_menu?;
    let width = crate::app::state::SidebarNewMenuAction::ALL
        .iter()
        .map(|action| display_width(action.label()).saturating_add(2))
        .max()
        .unwrap_or(1);
    super::dropdown::layout_dropdown(
        &super::dropdown::DropdownSpec {
            anchor: sidebar_header_new_menu_rect(app.view.sidebar_rect),
            item_count: crate::app::state::SidebarNewMenuAction::ALL.len(),
            selected: menu.selected,
            has_filter: false,
            max_rows: crate::app::state::SidebarNewMenuAction::ALL.len(),
            min_width: u16::try_from(width).unwrap_or(u16::MAX),
        },
        area,
    )
}

pub(super) fn render_sidebar_new_menu(app: &AppState, frame: &mut Frame) {
    let Some(menu) = app.sidebar_new_menu else {
        return;
    };
    let Some(layout) = sidebar_new_menu_layout(app, frame.area()) else {
        return;
    };
    let rows = crate::app::state::SidebarNewMenuAction::ALL
        .iter()
        .map(|action| super::dropdown::DropdownMenuRow::Item {
            label: action.label().to_string(),
            enabled: true,
        })
        .collect::<Vec<_>>();
    super::dropdown::render_menu(&app.palette, frame, &layout, &rows, menu.selected);
}

fn sidebar_new_thread_labels(app: &AppState) -> Vec<String> {
    app.new_thread_options()
        .iter()
        .map(|option| {
            let path = crate::app::home::directory_label(&option.path);
            // The project name is part of the row so the filter matches it,
            // which is what makes typing a project narrow the list to it.
            match option.project.as_deref() {
                Some(project) => format!("📁 {}  {project} · {path}", option.name),
                None => format!("📁 {}  {path}", option.name),
            }
        })
        .collect()
}

pub(crate) fn sidebar_new_thread_matches(app: &AppState) -> Vec<(usize, String)> {
    let labels = sidebar_new_thread_labels(app);
    let query = app
        .sidebar_new_thread
        .as_ref()
        .map(|state| state.filter.query.as_str())
        .unwrap_or_default();
    super::dropdown::filter_items(&labels, query)
        .into_iter()
        .map(|(index, label)| (index, label.to_string()))
        .collect()
}

pub(crate) fn sidebar_new_thread_layout(
    app: &AppState,
    area: Rect,
) -> Option<super::dropdown::DropdownLayout> {
    let state = app.sidebar_new_thread.as_ref()?;
    let matches = sidebar_new_thread_matches(app);
    let width = matches
        .iter()
        .map(|(_, label)| display_width(label).saturating_add(4))
        .max()
        .unwrap_or(24);
    super::dropdown::layout_dropdown(
        &super::dropdown::DropdownSpec {
            anchor: sidebar_header_new_thread_rect(app.view.sidebar_rect),
            item_count: matches.len(),
            selected: state.filter.selected,
            has_filter: true,
            max_rows: 9,
            min_width: u16::try_from(width).unwrap_or(u16::MAX),
        },
        area,
    )
}

pub(super) fn render_sidebar_new_thread(app: &AppState, frame: &mut Frame) {
    let Some(state) = app.sidebar_new_thread.as_ref() else {
        return;
    };
    let Some(layout) = sidebar_new_thread_layout(app, frame.area()) else {
        return;
    };
    let matches = sidebar_new_thread_matches(app);
    frame.render_widget(ratatui::widgets::Clear, layout.rect);
    if let Some(filter) = layout.filter_rect {
        frame.render_widget(
            Paragraph::new(format!(" 🔍 {}▏", state.filter.query)).style(
                Style::default()
                    .fg(app.palette.text)
                    .bg(app.palette.panel_bg),
            ),
            filter,
        );
    }
    let lines = matches
        .iter()
        .enumerate()
        .skip(layout.first_visible)
        .take(layout.visible_rows)
        .map(|(position, (_, label))| {
            let selected = position == state.filter.selected;
            let accelerator = if position < 9 {
                char::from_digit((position + 1) as u32, 10).unwrap_or(' ')
            } else {
                ' '
            };
            let style = if selected {
                Style::default()
                    .fg(app.palette.text)
                    .bg(app.palette.surface1)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(app.palette.subtext0)
                    .bg(app.palette.panel_bg)
            };
            Line::from(Span::styled(
                format!("{} {accelerator} {label}", if selected { "▸" } else { " " }),
                style,
            ))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(app.palette.panel_bg)),
        layout.list_rect,
    );
}

pub(crate) fn sidebar_group_menu_layout(
    app: &AppState,
    area: Rect,
) -> Option<super::dropdown::DropdownLayout> {
    let anchor = sidebar_group_mode_anchor_rect(app.view.sidebar_rect);
    super::dropdown::layout_dropdown(
        &super::dropdown::DropdownSpec {
            anchor,
            item_count: SidebarGroupMode::VIEWS.len(),
            selected: app.sidebar_group_menu_selected,
            has_filter: false,
            max_rows: SidebarGroupMode::VIEWS.len(),
            min_width: 22,
        },
        area,
    )
}

pub(crate) fn sidebar_filter_menu_layout(
    app: &AppState,
    area: Rect,
) -> Option<super::dropdown::DropdownLayout> {
    let anchor = sidebar_filter_anchor_rect(app, app.view.sidebar_rect);
    let options = sidebar_filter_options(app);
    super::dropdown::layout_dropdown(
        &super::dropdown::DropdownSpec {
            anchor,
            item_count: options.len(),
            selected: app.sidebar_filter_menu_selected,
            has_filter: false,
            max_rows: options.len().min(10),
            min_width: 20,
        },
        area,
    )
}

pub(crate) fn sidebar_object_menu_anchor_rect(app: &AppState) -> Option<Rect> {
    let menu = app.sidebar_object_menu.as_ref()?;
    let headers = compute_sidebar_nested_header_areas(app, app.view.sidebar_rect);
    headers
        .iter()
        .find(|header| {
            header.action_key.as_ref() == Some(&menu.target)
                && menu.anchor_row == Some(header.rect.y)
        })
        .or_else(|| {
            headers
                .iter()
                .find(|header| header.action_key.as_ref() == Some(&menu.target))
        })
        .map(|header| Rect::new(header.rect.right().saturating_sub(1), header.rect.y, 1, 1))
}

pub(crate) fn sidebar_object_menu_labels(app: &AppState) -> Vec<String> {
    if app
        .sidebar_object_menu
        .as_ref()
        .is_some_and(|menu| menu.page == crate::app::state::SidebarObjectMenuPage::Confirmation)
    {
        return app
            .dock_pending_write
            .as_ref()
            .map(|write| vec![format!("Confirm {}? [y/N]", write.describe())])
            .unwrap_or_default();
    }
    if app
        .sidebar_object_menu
        .as_ref()
        .is_some_and(|menu| menu.target.starts_with("linear:"))
    {
        return sidebar_ticket_action_entries(app)
            .iter()
            .map(crate::ui::ticket_actions::TicketActionEntry::display_label)
            .collect();
    }
    if app
        .sidebar_object_menu
        .as_ref()
        .is_some_and(|menu| menu.target.starts_with("github:"))
    {
        return sidebar_pull_request_actions(app)
            .into_iter()
            .map(|action| action.label)
            .collect();
    }
    sidebar_object_menu_items(app)
        .into_iter()
        .map(SidebarObjectMenuItem::label)
        .collect()
}

pub(crate) fn sidebar_object_menu_layout(
    app: &AppState,
    area: Rect,
) -> Option<super::dropdown::DropdownLayout> {
    let anchor = sidebar_object_menu_anchor_rect(app)?;
    if app
        .sidebar_object_menu
        .as_ref()
        .is_some_and(|menu| menu.target.starts_with("github:"))
    {
        let actions = sidebar_pull_request_actions(app);
        let selected = app
            .sidebar_object_menu
            .as_ref()
            .map_or(0, |menu| menu.selected);
        return super::pr_actions::layout(
            area,
            anchor,
            &actions,
            crate::app::state::PrActionMenuState { selected },
        );
    }
    let labels = sidebar_object_menu_labels(app);
    let width = labels
        .iter()
        .map(|label| display_width(label).saturating_add(2))
        .max()
        .unwrap_or(1);
    let width = u16::try_from(width).unwrap_or(u16::MAX);
    super::dropdown::layout_dropdown(
        &super::dropdown::DropdownSpec {
            anchor,
            item_count: labels.len(),
            selected: app
                .sidebar_object_menu
                .as_ref()
                .map_or(0, |menu| menu.selected),
            has_filter: false,
            max_rows: labels.len(),
            min_width: width,
        },
        area,
    )
}

pub(crate) fn sidebar_object_menu_item_at(
    app: &AppState,
    area: Rect,
    col: u16,
    row: u16,
) -> Option<usize> {
    if app
        .sidebar_object_menu
        .as_ref()
        .is_some_and(|menu| menu.target.starts_with("github:"))
    {
        let anchor = sidebar_object_menu_anchor_rect(app)?;
        let actions = sidebar_pull_request_actions(app);
        let selected = app
            .sidebar_object_menu
            .as_ref()
            .map_or(0, |menu| menu.selected);
        return super::pr_actions::hit_test(
            area,
            anchor,
            &actions,
            crate::app::state::PrActionMenuState { selected },
            col,
            row,
        );
    }
    let layout = sidebar_object_menu_layout(app, area)?;
    super::dropdown::hit_test(&layout, col, row)
}

pub(super) fn render_sidebar_object_menu(app: &AppState, frame: &mut Frame) {
    let Some(menu) = app.sidebar_object_menu.as_ref() else {
        return;
    };
    if menu.target.starts_with("github:") {
        let Some(anchor) = sidebar_object_menu_anchor_rect(app) else {
            return;
        };
        let actions = sidebar_pull_request_actions(app);
        super::pr_actions::render(
            app,
            frame,
            frame.area(),
            anchor,
            &actions,
            crate::app::state::PrActionMenuState {
                selected: menu.selected,
            },
        );
        return;
    }
    let Some(layout) = sidebar_object_menu_layout(app, frame.area()) else {
        if let Some(anchor) = sidebar_object_menu_anchor_rect(app) {
            frame.render_widget(
                Paragraph::new("!").style(Style::default().fg(app.palette.red)),
                anchor,
            );
        }
        return;
    };
    let labels = sidebar_object_menu_labels(app);
    frame.render_widget(ratatui::widgets::Clear, layout.rect);
    let lines = labels
        .iter()
        .enumerate()
        .skip(layout.first_visible)
        .take(layout.visible_rows)
        .map(|(index, label)| {
            let selected = index == menu.selected;
            let enabled = if menu.target.starts_with("linear:") {
                sidebar_ticket_action_entries(app)
                    .get(index)
                    .is_some_and(crate::ui::ticket_actions::TicketActionEntry::enabled)
            } else {
                true
            };
            let style = if !enabled {
                Style::default()
                    .fg(app.palette.overlay0)
                    .bg(if selected {
                        app.palette.surface1
                    } else {
                        app.palette.panel_bg
                    })
                    .add_modifier(Modifier::DIM)
            } else if selected {
                Style::default()
                    .fg(app.palette.text)
                    .bg(app.palette.surface1)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(app.palette.subtext0)
                    .bg(app.palette.panel_bg)
            };
            Line::from(Span::styled(
                format!("{} {label}", if selected { "▸" } else { " " }),
                style,
            ))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(app.palette.panel_bg)),
        layout.list_rect,
    );
}

pub(crate) const SETTLED_MENU_LABELS: [&str; 4] = [
    "↺ Resume thread",
    "✎ New thread in same repo",
    "⎇ New thread, new worktree",
    "🗑 Delete",
];

/// The delete row's label, which asks once before it closes the pane while
/// `ui.confirm_close` is on.
pub(crate) fn settled_menu_labels(app: &AppState) -> [&'static str; 4] {
    let mut labels = SETTLED_MENU_LABELS;
    if app.sidebar_settled_menu_delete_armed {
        labels[3] = "🗑 Delete — press again to confirm";
    }
    labels
}

pub(crate) fn sidebar_settled_menu_layout(
    app: &AppState,
    area: Rect,
) -> Option<super::dropdown::DropdownLayout> {
    let target = app.sidebar_settled_menu_target.as_ref()?;
    let ws_idx = app
        .workspaces
        .iter()
        .position(|workspace| workspace.id == target.workspace_id)?;
    let anchor = compute_tab_card_areas(app, app.view.sidebar_rect)
        .into_iter()
        .find(|card| card.ws_idx == ws_idx && card.pane_id == target.pane_id)
        .map(|card| card.rect)
        .or_else(|| {
            compute_agent_card_areas(app, app.view.sidebar_rect)
                .into_iter()
                .find(|card| card.ws_idx == ws_idx && card.pane_id == target.pane_id)
                .map(|card| card.rect)
        })?;
    super::dropdown::layout_dropdown(
        &super::dropdown::DropdownSpec {
            anchor,
            item_count: SETTLED_MENU_LABELS.len(),
            selected: app.sidebar_settled_menu_selected,
            has_filter: false,
            max_rows: SETTLED_MENU_LABELS.len(),
            min_width: 31,
        },
        area,
    )
}

pub(super) fn render_sidebar_settled_menu(app: &AppState, frame: &mut Frame) {
    if app.sidebar_settled_menu_target.is_none() {
        return;
    }
    let Some(layout) = sidebar_settled_menu_layout(app, frame.area()) else {
        let Some(target) = app.sidebar_settled_menu_target.as_ref() else {
            return;
        };
        let Some(ws_idx) = app
            .workspaces
            .iter()
            .position(|workspace| workspace.id == target.workspace_id)
        else {
            return;
        };
        if let Some(anchor) = compute_tab_card_areas(app, app.view.sidebar_rect)
            .into_iter()
            .find(|card| card.ws_idx == ws_idx && card.pane_id == target.pane_id)
            .map(|card| card.rect)
        {
            frame.render_widget(
                Paragraph::new(" no space below").style(
                    Style::default()
                        .fg(app.palette.red)
                        .bg(app.palette.panel_bg),
                ),
                anchor,
            );
        }
        return;
    };
    frame.render_widget(ratatui::widgets::Clear, layout.rect);
    let lines = settled_menu_labels(app)
        .iter()
        .enumerate()
        .skip(layout.first_visible)
        .take(layout.visible_rows)
        .map(|(index, label)| {
            let selected = index == app.sidebar_settled_menu_selected;
            let style = if selected {
                Style::default()
                    .fg(app.palette.text)
                    .bg(app.palette.surface1)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(app.palette.subtext0)
                    .bg(app.palette.panel_bg)
            };
            Line::from(Span::styled(
                format!("{} {label}", if selected { "▸" } else { " " }),
                style,
            ))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(app.palette.panel_bg)),
        layout.list_rect,
    );
}

pub(super) fn render_sidebar_filter_menu(app: &AppState, frame: &mut Frame) {
    if !app.sidebar_filter_menu_open {
        return;
    }
    let Some(layout) = sidebar_filter_menu_layout(app, frame.area()) else {
        return;
    };
    let options = sidebar_filter_options(app);
    frame.render_widget(ratatui::widgets::Clear, layout.rect);
    let lines = options
        .iter()
        .enumerate()
        .skip(layout.first_visible)
        .take(layout.visible_rows)
        .map(|(index, option)| {
            let selected = index == app.sidebar_filter_menu_selected;
            let marker = if selected { "▸" } else { " " };
            let style = if selected {
                Style::default()
                    .fg(app.palette.text)
                    .bg(app.palette.surface1)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(app.palette.subtext0)
                    .bg(app.palette.panel_bg)
            };
            Line::from(Span::styled(
                super::dropdown::pad_menu_row(
                    &format!("{marker} {}", option.label()),
                    layout.list_rect.width,
                ),
                style,
            ))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(app.palette.panel_bg)),
        layout.list_rect,
    );
}

pub(super) fn render_sidebar_group_menu(app: &AppState, frame: &mut Frame) {
    if !app.sidebar_group_menu_open {
        return;
    }
    let Some(layout) = sidebar_group_menu_layout(app, frame.area()) else {
        return;
    };
    frame.render_widget(ratatui::widgets::Clear, layout.rect);
    let lines = SidebarGroupMode::VIEWS
        .iter()
        .enumerate()
        .skip(layout.first_visible)
        .take(layout.visible_rows)
        .map(|(index, mode)| {
            let selected = index == app.sidebar_group_menu_selected;
            let marker = if selected { "▸" } else { " " };
            let style = if selected {
                Style::default()
                    .fg(app.palette.text)
                    .bg(app.palette.surface1)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(app.palette.subtext0)
                    .bg(app.palette.panel_bg)
            };
            Line::from(Span::styled(
                super::dropdown::pad_menu_row(
                    &format!("{marker} View: {}", mode.view_label()),
                    layout.list_rect.width,
                ),
                style,
            ))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(app.palette.panel_bg)),
        layout.list_rect,
    );
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
pub(crate) mod tests {
    /// Build a workspace whose tabs each sit in a different directory. Separate
    /// tabs, because a tab rolls its panes into one sidebar row.
    fn app_with_unlinked_tab_directories(dirs: &[Option<&str>]) -> AppState {
        let mut app = AppState::test_new();
        let mut workspace = crate::workspace::Workspace::test_new("mixed");
        for index in 1..dirs.len() {
            workspace.test_add_tab(Some(&format!("tab{index}")));
        }
        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.ensure_test_terminals();
        for (tab_idx, dir) in dirs.iter().enumerate() {
            let terminal_id = {
                let tab = &app.workspaces[0].tabs[tab_idx];
                tab.panes[&tab.root_pane].attached_terminal_id.clone()
            };
            let terminal = app.terminals.get_mut(&terminal_id).expect("terminal state");
            terminal.cwd = dir.map(std::path::PathBuf::from).unwrap_or_default();
        }
        app
    }

    #[test]
    fn repo_and_tab_projections_group_unlinked_panes_by_directory_too() {
        let app = app_with_unlinked_tab_directories(&[Some("/work/alpha"), Some("/work/beta")]);
        let entries = sidebar_thread_entries(&app);

        let repo_titles = sidebar_repo_groups(&app, &entries)
            .into_iter()
            .map(|group| group.title)
            .collect::<Vec<_>>();
        assert_eq!(repo_titles, ["▫ alpha", "▫ beta"]);

        let tab_titles = sidebar_tab_groups(&app, &entries, SidebarGroupMode::RepoPr)
            .into_iter()
            .map(|group| group.title)
            .collect::<Vec<_>>();
        assert_eq!(tab_titles, ["▫ alpha", "▫ beta"]);
    }

    #[test]
    fn linking_a_pull_request_regroups_only_the_window_it_was_linked_to() {
        let mut app = app_with_unlinked_tab_directories(&[Some("/work/repo"), Some("/work/repo")]);
        // Both windows sit in one checkout, so the git tier observes the
        // branch's pull request for each of them.
        for tab_idx in 0..2 {
            let terminal_id = {
                let tab = &app.workspaces[0].tabs[tab_idx];
                tab.panes[&tab.root_pane].attached_terminal_id.clone()
            };
            app.terminals
                .get_mut(&terminal_id)
                .expect("terminal state")
                .replace_git_work_context(crate::work_context::PaneWorkContext {
                    pr_urls: vec!["https://github.com/o/r/pull/1".into()],
                    ..Default::default()
                })
                .expect("git observation");
        }
        let linked_terminal_id = {
            let tab = &app.workspaces[0].tabs[0];
            tab.panes[&tab.root_pane].attached_terminal_id.clone()
        };
        app.terminals
            .get_mut(&linked_terminal_id)
            .expect("terminal state")
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                pr_urls: Some(vec!["https://github.com/o/r/pull/2".into()]),
                ..Default::default()
            })
            .expect("manual link");

        let entries = sidebar_thread_entries(&app);
        let groups = sidebar_tab_groups(&app, &entries, SidebarGroupMode::RepoPr)
            .into_iter()
            .map(|group| (group.key, group.entries.len()))
            .collect::<Vec<_>>();
        assert_eq!(
            groups,
            vec![
                ("https://github.com/o/r/pull/2".to_string(), 1),
                ("https://github.com/o/r/pull/1".to_string(), 1),
            ],
            "the linked window leaves the branch's group instead of joining both"
        );
    }

    #[test]
    fn directories_sharing_a_basename_get_distinguishable_headers() {
        let titles = |dirs: &[Option<&str>]| {
            let app = app_with_unlinked_tab_directories(dirs);
            let entries = sidebar_thread_entries(&app);
            sidebar_work_groups(&app, &entries, SidebarGroupMode::RepoPr)
                .into_iter()
                .map(|group| group.title)
                .collect::<Vec<_>>()
        };

        assert_eq!(
            titles(&[Some("/a/project"), Some("/b/project"), Some("/c/other")]),
            ["▫ a/project", "▫ b/project", "▫ c/other"]
        );
        // One extra component is not enough here, so the titles deepen again.
        assert_eq!(
            titles(&[Some("/a/x/project"), Some("/b/x/project")]),
            ["▫ a/x/project", "▫ b/x/project"]
        );
        // Distinct basenames stay short.
        assert_eq!(
            titles(&[Some("/a/alpha"), Some("/b/beta")]),
            ["▫ alpha", "▫ beta"]
        );
        // Two spellings of one directory are one bucket, not two identical
        // headers: the key and the title must agree on what a directory is.
        assert_eq!(
            titles(&[Some("/a/project"), Some("/a/./project")]),
            ["▫ project"]
        );
    }

    /// `Path::display()` is lossy, so keying on it would merge two distinct
    /// non-UTF-8 paths into one bucket.
    #[cfg(unix)]
    #[test]
    fn non_utf8_directories_stay_in_separate_buckets() {
        use std::os::unix::ffi::OsStrExt;

        let left = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b"/work/\xff"));
        let right = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b"/work/\xfe"));
        assert_eq!(
            left.display().to_string(),
            right.display().to_string(),
            "the fixture only proves anything if display() collapses them"
        );

        assert_ne!(unlinked_group_key(&left), unlinked_group_key(&right));
    }

    /// A flat unlinked list is unreadable past a handful of panes, and the
    /// panes in it are not unrelated: they share a directory. Two directories
    /// must produce two buckets, and a pane with no resolvable directory keeps
    /// the plain one, last.
    #[test]
    fn unlinked_panes_group_by_their_working_directory() {
        let mut app = AppState::test_new();
        // Separate tabs, because a tab rolls its panes into one sidebar row.
        let mut workspace = crate::workspace::Workspace::test_new("mixed");
        workspace.test_add_tab(Some("beta"));
        workspace.test_add_tab(Some("rootless"));
        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.ensure_test_terminals();

        for (tab_idx, dir) in [
            (0usize, Some("/work/alpha")),
            (1, Some("/work/beta")),
            // An empty path is the "cannot resolve a directory" case.
            (2, None),
        ] {
            let terminal_id = {
                let tab = &app.workspaces[0].tabs[tab_idx];
                tab.panes[&tab.root_pane].attached_terminal_id.clone()
            };
            let terminal = app.terminals.get_mut(&terminal_id).expect("terminal state");
            terminal.cwd = dir.map(std::path::PathBuf::from).unwrap_or_default();
        }

        let entries = sidebar_thread_entries(&app);
        let groups = sidebar_work_groups(&app, &entries, SidebarGroupMode::RepoPr);
        let listed = groups
            .iter()
            .map(|group| (group.title.as_str(), group.entries.len()))
            .collect::<Vec<_>>();

        assert_eq!(
            listed,
            [("▫ alpha", 1), ("▫ beta", 1), ("unlinked", 1)],
            "{:?}",
            groups.iter().map(|group| &group.key).collect::<Vec<_>>()
        );
        assert!(groups.iter().all(|group| group.unlinked));
    }

    /// `main` is the most common branch name there is, so two repositories on it
    /// must stay two groups, and the titles have to say which is which.
    #[test]
    fn same_branch_in_two_repos_stays_two_named_groups() {
        let mut app = app_with_agents(&["herdr", "scalablev2"]);
        app.sidebar_group_mode = SidebarGroupMode::RepoPr;
        for (ws_idx, repo) in [(0usize, "herdrdev/herdr"), (1, "scalable-so/scalablev2")] {
            replace_tab_context(
                &mut app,
                ws_idx,
                0,
                crate::work_context::PaneWorkContext {
                    repo: Some(repo.into()),
                    branch: Some("main".into()),
                    ..Default::default()
                },
                Default::default(),
            );
        }

        let entries = sidebar_thread_entries(&app);
        let groups = sidebar_work_groups(&app, &entries, SidebarGroupMode::RepoPr);
        assert_eq!(
            groups
                .iter()
                .map(|group| (group.title.as_str(), group.entries.len()))
                .collect::<Vec<_>>(),
            [
                ("⎇ main · herdrdev/herdr", 1),
                ("⎇ main · scalable-so/scalablev2", 1),
            ],
            "{:?}",
            groups.iter().map(|group| &group.key).collect::<Vec<_>>()
        );
        assert_eq!(
            sidebar_tab_groups(&app, &entries, SidebarGroupMode::RepoPr)
                .iter()
                .map(|group| group.title.clone())
                .collect::<Vec<_>>(),
            [
                "⎇ main · herdrdev/herdr".to_string(),
                "⎇ main · scalable-so/scalablev2".to_string(),
            ]
        );
    }

    /// A Space holding panes on two repositories names neither: guessing one
    /// would file the Space under a repository half its work is not in, and the
    /// choice would flip as panes open and close.
    #[test]
    fn a_space_with_two_repos_joins_no_repo_group() {
        let mut app = app_with_agents(&["bound", "mixed"]);
        app.workspaces[0].repo_binding = Some("scalable-so/scalablev2".into());
        app.workspaces[1].test_add_tab(Some("second"));
        app.ensure_test_terminals();
        for (tab_idx, repo) in [(0usize, "scalable-so/scalablev2"), (1, "scalable-so/other")] {
            replace_tab_context(
                &mut app,
                1,
                tab_idx,
                crate::work_context::PaneWorkContext {
                    repo: Some(repo.into()),
                    ..Default::default()
                },
                Default::default(),
            );
        }

        assert_eq!(workspace_parent_group_state(&app, 0), None);
        assert_eq!(sidebar_space_member_indices(&app, 0), [0]);
        assert!(workspace_list_entries(&app).iter().all(|entry| matches!(
            entry,
            WorkspaceListEntry::Workspace {
                indented: false,
                ..
            }
        )));
    }

    /// Agent tooling creates a Space per checkout without worktree membership,
    /// so the Repo view used to list them flat, away from the Space bound to the
    /// repository they are checkouts of. A Space that resolves the bound repo now
    /// nests under it, and the bound Space is the home row.
    #[test]
    fn unbound_checkout_spaces_nest_under_the_space_bound_to_their_repo() {
        let mut app = app_with_agents(&["scalablev2", "ccm-scalablev2-worktree", "elsewhere"]);
        app.workspaces[0].repo_binding = Some("scalable-so/scalablev2".into());
        for (ws_idx, repo) in [(1usize, "scalable-so/scalablev2"), (2, "scalable-so/other")] {
            replace_tab_context(
                &mut app,
                ws_idx,
                0,
                crate::work_context::PaneWorkContext {
                    repo: Some(repo.into()),
                    ..Default::default()
                },
                Default::default(),
            );
        }

        assert_eq!(
            workspace_list_entries(&app),
            [
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 2,
                    indented: false,
                },
            ]
        );
        assert_eq!(sidebar_space_member_indices(&app, 0), [0, 1]);
        let (key, collapsed) =
            workspace_parent_group_state(&app, 0).expect("the bound Space heads the group");
        assert_eq!(key, "repo:scalable-so/scalablev2");
        assert!(!collapsed);
        // A checkout Space is never the home row, even alone with its repo.
        assert_eq!(workspace_parent_group_state(&app, 1), None);

        // Two loose checkouts of a repo no Space is bound to stay flat: the view
        // does not invent a header for a repository nobody claims.
        app.workspaces[0].repo_binding = None;
        replace_tab_context(
            &mut app,
            0,
            0,
            crate::work_context::PaneWorkContext {
                repo: Some("scalable-so/scalablev2".into()),
                ..Default::default()
            },
            Default::default(),
        );
        assert!(workspace_list_entries(&app).iter().all(|entry| matches!(
            entry,
            WorkspaceListEntry::Workspace {
                indented: false,
                ..
            }
        )));
    }

    /// A worktree session that has not opened a pull request is still work on
    /// the repo. In the GitHub view it groups under its branch, beside the PR
    /// groups, instead of landing in the trailing unlinked bucket. Only a
    /// session with no branch at all stays unlinked.
    #[test]
    fn repo_sessions_without_a_pull_request_group_under_their_branch() {
        let mut app = app_with_agents(&["reviewed", "worktree", "bare"]);
        app.sidebar_group_mode = SidebarGroupMode::RepoPr;
        replace_tab_context(
            &mut app,
            0,
            0,
            crate::work_context::PaneWorkContext {
                repo: Some("herdrdev/herdr".into()),
                branch: Some("fix/strip".into()),
                pr_urls: vec!["https://github.com/herdrdev/herdr/pull/12".into()],
                ..Default::default()
            },
            Default::default(),
        );
        replace_tab_context(
            &mut app,
            1,
            0,
            crate::work_context::PaneWorkContext {
                repo: Some("herdrdev/herdr".into()),
                branch: Some("feat/dock-toggle".into()),
                ..Default::default()
            },
            Default::default(),
        );

        let entries = sidebar_thread_entries(&app);
        let groups = sidebar_work_groups(&app, &entries, SidebarGroupMode::RepoPr);
        assert_eq!(
            groups
                .iter()
                .map(|group| (group.title.as_str(), group.entries.len(), group.unlinked))
                .collect::<Vec<_>>(),
            [
                ("#12", 1, false),
                ("⎇ feat/dock-toggle", 1, false),
                (unlinked_bucket_title().as_str(), 1, true),
            ],
            "{:?}",
            groups.iter().map(|group| &group.key).collect::<Vec<_>>()
        );

        // The same fallback applies to the groups nested under a repo header.
        assert_eq!(
            sidebar_tab_groups(&app, &entries, SidebarGroupMode::RepoPr)
                .iter()
                .map(|group| (group.title.as_str(), group.unlinked))
                .collect::<Vec<_>>(),
            [
                ("#12", false),
                ("⎇ feat/dock-toggle", false),
                (unlinked_bucket_title().as_str(), true),
            ]
        );
    }

    /// The unlinked bucket is named after the pane's working directory, and
    /// `Workspace::test_new` uses the process cwd, so tests derive the label
    /// rather than hardcoding this checkout's name.
    fn unlinked_bucket_title() -> String {
        let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
        let name = cwd
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| cwd.display().to_string());
        format!("▫ {name}")
    }

    use super::*;
    use crate::{
        api::schema::{
            AgentViewBuiltinField, AgentViewField, AgentViewFilter, AgentViewSetParams,
            AgentViewValue,
        },
        app::state::{AgentPanelSort, SidebarPresentationState, ViewLayout},
        config::BindingConfig,
        detect::Agent,
        layout::PaneId,
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
    fn sidebar_footer_renders_f17b_glyph_order() {
        let app = AppState::test_new();
        let area = Rect::new(0, 0, 26, 8);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("footer terminal");
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .expect("render footer");
        let footer = row_text(terminal.backend().buffer(), area.bottom() - 1, area.width);
        assert!(footer.contains("⚙ ⑂ ▥ ◎ ✉ ⟳"), "{footer:?}");
    }

    #[test]
    fn active_title_color_steps_away_from_the_panel_and_preserves_terminal_fallbacks() {
        // ac3: the selected title moves one third away from the panel
        // background -- darker on a light panel, brighter on a dark one -- so
        // the same rule reads as emphasis in both appearances. Bold supplies
        // the weight; the foreground supplies the contrast.
        let one_light = crate::app::state::Palette::one_light();
        assert_eq!(
            active_sidebar_title_color(&one_light),
            Color::Rgb(37, 38, 44)
        );

        let one_dark = crate::app::state::Palette::one_dark();
        assert_eq!(
            active_sidebar_title_color(&one_dark),
            Color::Rgb(199, 203, 212)
        );

        let terminal = crate::app::state::Palette::terminal();
        assert_eq!(active_sidebar_title_color(&terminal), terminal.text);

        let mut custom_reset = crate::app::state::Palette::one_light();
        custom_reset.panel_bg = Color::Reset;
        custom_reset.text = Color::Rgb(12, 34, 56);
        assert_eq!(active_sidebar_title_color(&custom_reset), custom_reset.text);
    }

    /// The regression this fixes: on every dark palette the selected Space and
    /// the current tab title were the *dimmest* text in the sidebar, below the
    /// unselected rows they had to stand out from.
    #[test]
    fn active_title_outshines_unselected_rows_on_every_dark_palette() {
        let luminance = |color: Color| match color {
            Color::Rgb(r, g, b) => u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114,
            other => panic!("expected an rgb color, got {other:?}"),
        };

        for palette in [
            crate::app::state::Palette::github_dark_high_contrast(),
            crate::app::state::Palette::one_dark(),
            crate::app::state::Palette::catppuccin(),
        ] {
            let selected = luminance(active_sidebar_title_color(&palette));
            assert!(
                selected > luminance(palette.subtext0),
                "selected title must outshine the unselected rows"
            );
            assert!(
                selected > luminance(palette.text),
                "selected title must outshine the authored text token"
            );
        }

        assert_eq!(
            active_sidebar_title_color(&crate::app::state::Palette::github_dark_high_contrast()),
            Color::Rgb(245, 247, 249)
        );
    }

    #[test]
    fn compact_row_grammar_uses_one_dot_and_the_provider_column_contract() {
        let app = AppState::test_new();
        let mut cases = Vec::new();

        let mut working = compact_test_entry("working", Some(Agent::Claude));
        working.state = AgentState::Working;
        cases.push(working);

        let mut gated = compact_test_entry("gated", Some(Agent::Claude));
        gated.state = AgentState::Working;
        gated.gate_count = 2;
        gated.open_blockers = true;
        cases.push(gated);

        let mut blocked = compact_test_entry("blocked", Some(Agent::Claude));
        blocked.state = AgentState::Blocked;
        cases.push(blocked);

        let mut unread = compact_test_entry("unread", Some(Agent::Claude));
        unread.seen = false;
        cases.push(unread);

        cases.push(compact_test_entry("idle", Some(Agent::Claude)));
        cases.push(compact_test_entry("shell", None));

        for entry in cases {
            let mut terminal = Terminal::new(TestBackend::new(40, 1)).unwrap();
            terminal
                .draw(|frame| {
                    render_compact_agent_row(
                        &app,
                        frame,
                        &entry,
                        Rect::new(0, 0, 40, 1),
                        0,
                        true,
                        None,
                    )
                })
                .unwrap();
            let rendered = row_text(terminal.backend().buffer(), 0, 40);
            let dot_count = rendered
                .chars()
                .filter(|character| matches!(character, '●' | '○' | '◆' | '·'))
                .count();
            assert_eq!(
                dot_count, 1,
                "compact row emitted multiple dots: {rendered:?}"
            );
        }

        let mut two = compact_test_entry("two", Some(Agent::Claude));
        two.active_subagents = Some(2);
        assert_eq!(compact_provider(&two), "cc+2");
        two.holds_shell = true;
        assert_eq!(compact_provider(&two), "cc+2 >_");

        let mut stale = two;
        stale.stale = true;
        assert_eq!(compact_provider(&stale), "cc >_");
    }

    #[test]
    fn sidebar_row_layout_frozen() {
        let palette = Palette::catppuccin();
        let now = std::time::Instant::now();
        let mut cases = Vec::new();

        for (title, state, seen, agent) in [
            (
                "working title remains readable",
                AgentState::Working,
                true,
                Some(Agent::Codex),
            ),
            (
                "blocked title",
                AgentState::Blocked,
                true,
                Some(Agent::Claude),
            ),
            ("done title", AgentState::Idle, false, Some(Agent::Pi)),
            ("idle title", AgentState::Idle, true, Some(Agent::Kimi)),
            ("shell title", AgentState::Unknown, true, None),
        ] {
            let mut entry = compact_test_entry(title, agent);
            entry.state = state;
            entry.seen = seen;
            entry.activity_at = Some(now - std::time::Duration::from_secs(125));
            let layout = tab_row_layout(
                &entry,
                now,
                32,
                3,
                &palette,
                StatusIndicatorStyle::default(),
            );
            cases.push((
                layout.dot,
                layout.title,
                layout.provider,
                layout.activity_age,
                compact_row_color(&entry, &palette),
                state_label_color(entry.state, entry.seen, &palette),
            ));
        }

        assert_eq!(
            cases,
            vec![
                (
                    "●".into(),
                    "working title rema…".into(),
                    "cx".into(),
                    Some("2m".into()),
                    Color::Rgb(137, 180, 250),
                    Color::Rgb(137, 180, 250),
                ),
                (
                    "○".into(),
                    "blocked title".into(),
                    "cc".into(),
                    Some("2m".into()),
                    Color::Rgb(243, 139, 168),
                    Color::Rgb(243, 139, 168),
                ),
                (
                    "○".into(),
                    "done title".into(),
                    "pi".into(),
                    Some("2m".into()),
                    Color::Rgb(148, 226, 213),
                    Color::Rgb(148, 226, 213),
                ),
                (
                    "○".into(),
                    "idle title".into(),
                    "ki".into(),
                    Some("2m".into()),
                    Color::Rgb(166, 227, 161),
                    Color::Rgb(166, 227, 161),
                ),
                (
                    "·".into(),
                    "shell title".into(),
                    ">_".into(),
                    Some("2m".into()),
                    Color::Rgb(108, 112, 134),
                    Color::Rgb(108, 112, 134),
                ),
            ]
        );
    }

    #[test]
    fn compact_provider_marks_plain_shells_and_kimi() {
        let plain = compact_test_entry("terminal", None);
        assert_eq!(compact_provider(&plain), ">_");
        assert_eq!(tab_agent_suffix(Some(Agent::Kimi)), Some("ki"));

        let kimi = compact_test_entry("task", Some(Agent::Kimi));
        assert_eq!(compact_provider(&kimi), "ki");
    }

    #[test]
    fn blocked_filter_keeps_spaces_and_only_red_rows() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            Workspace::test_new("working"),
            Workspace::test_new("blocked"),
            Workspace::test_new("idle"),
        ];
        app.ensure_test_terminals();
        app.active = Some(0);
        for (ws_idx, state) in [
            (0, AgentState::Working),
            (1, AgentState::Blocked),
            (2, AgentState::Idle),
        ] {
            let pane_id = app.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let terminal = app.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(Agent::Claude);
            terminal.state = state;
        }
        for pane in app.workspaces[2].tabs[0].panes.values_mut() {
            pane.seen = true;
        }
        app.reconcile_sidebar_presentation();
        app.blocked_filter = true;

        let rows = sidebar_rows(&app);
        assert!(rows.iter().any(|row| matches!(
            row,
            SidebarRow::SectionHeader {
                title: SPACES_SECTION_TITLE,
                count: 3,
                ..
            }
        )));
        let tab_entries = rows
            .iter()
            .filter_map(|row| match row {
                SidebarRow::Tab { entry, .. } => Some(entry),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(tab_entries.len(), 1);
        assert_eq!(tab_entries[0].ws_idx, 1);
        assert!(tab_entries.iter().all(|entry| entry_has_red_dot(entry)));
    }

    #[test]
    fn settled_section_is_last_and_keeps_active_group_mode() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            Workspace::test_new("active"),
            Workspace::test_new("settled"),
        ];
        app.ensure_test_terminals();
        app.reconcile_sidebar_presentation();
        let pane_id = app.workspaces[1].tabs[0].root_pane;
        app.workspaces[1].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("settled pane")
            .settled_at = Some(1_725_000_000);
        let terminal_id = app.workspaces[1].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .expect("terminal")
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                pr_urls: Some(vec!["https://github.com/owner/repo/pull/42".into()]),
                ..Default::default()
            })
            .expect("work context");
        app.sidebar_group_mode = SidebarGroupMode::RepoPr;

        let rows = sidebar_rows(&app);
        let settled = rows
            .iter()
            .position(|row| {
                matches!(
                    row,
                    SidebarRow::SectionHeader {
                        title: SETTLED_SECTION_TITLE,
                        count: 1,
                        ..
                    }
                )
            })
            .expect("Settled section");
        assert!(rows[settled + 1..].iter().any(|row| matches!(
            row,
            SidebarRow::NestedHeader { title, .. } if title.starts_with("#42")
        )));
        assert!(rows[settled + 1..].iter().any(|row| matches!(
            row,
            SidebarRow::Tab { entry, .. } if entry.pane_id == pane_id
        )));
        assert!(!rows[..settled].iter().any(|row| matches!(
            row,
            SidebarRow::Tab { entry, .. } if entry.pane_id == pane_id
        )));

        let area = Rect::new(0, 0, 106, 40);
        crate::ui::compute_view(&mut app, area);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
        terminal
            .draw(|frame| {
                render_sidebar(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    app.view.sidebar_rect,
                )
            })
            .expect("render sidebar");
        let card = compute_tab_card_areas(&app, app.view.sidebar_rect)
            .into_iter()
            .find(|card| card.pane_id == pane_id)
            .expect("settled card");
        let style = terminal.backend().buffer()[(card.rect.x + 2, card.rect.y)].style();
        assert_eq!(style.fg, Some(app.palette.overlay0));
        assert!(style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn space_names_use_server_label_and_never_adopt_agent_title() {
        let mut app = AppState::test_new();
        let mut agent_space = Workspace::test_new("ignored");
        agent_space.custom_name = None;
        agent_space.tabs[0].custom_name = Some("agent title".into());
        let mut manual_space = Workspace::test_new("manual");
        manual_space.custom_name = Some("manual label".into());
        manual_space.tabs[0].custom_name = Some("ignored agent title".into());
        let mut shell_one = Workspace::test_new("ignored");
        shell_one.custom_name = None;
        shell_one.cached_auto_label = "terminal space".into();
        let mut shell_two = Workspace::test_new("ignored");
        shell_two.custom_name = None;
        shell_two.cached_auto_label = "terminal space".into();
        app.workspaces = vec![agent_space, manual_space, shell_one, shell_two];
        app.ensure_test_terminals();
        let agent_pane = app.workspaces[0].tabs[0].root_pane;
        let agent_terminal = app.workspaces[0].tabs[0].panes[&agent_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&agent_terminal)
            .unwrap()
            .detected_agent = Some(Agent::Claude);

        let runtimes = TerminalRuntimeRegistry::new();
        let expected_server_label = app.workspaces[0].display_name_from(&app.terminals, &runtimes);
        let labels = sidebar_workspace_labels(&app, &runtimes);
        assert_eq!(labels[&0], (expected_server_label, true));
        assert_ne!(labels[&0].0, "agent title");
        assert_eq!(labels[&1], ("manual label".into(), false));
        assert_eq!(labels[&2], ("terminal space¹".into(), true));
        assert_eq!(labels[&3], ("terminal space²".into(), true));
    }

    fn compact_test_entry(title: &str, agent: Option<Agent>) -> AgentPanelEntry {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("test");
        workspace.tabs[0].custom_name = Some(title.into());
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&terminal_id).unwrap().detected_agent = agent;
        sidebar_thread_entries(&app)
            .into_iter()
            .next()
            .expect("compact test entry")
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

    fn set_active_subagents(app: &mut AppState, ws_idx: usize, value: Option<u32>) {
        let pane_id = app.workspaces[ws_idx].tabs[0].root_pane;
        let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .expect("test terminal")
            .set_active_subagents(value);
    }

    fn render_first_tab_row(app: &AppState, width: u16) -> String {
        let area = Rect::new(0, 0, width, 20);
        let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let card = compute_tab_card_areas(app, area)
            .into_iter()
            .next()
            .expect("tab card");
        row_text(terminal.backend().buffer(), card.rect.y, card.rect.width)
    }

    #[derive(Clone, Copy)]
    enum SpaceTagFixture {
        MatchingHeaders,
        DivergentHeaders,
        SingleSpace,
    }

    fn space_tag_fixture(mode: SidebarGroupMode, fixture: SpaceTagFixture) -> AppState {
        let group_titles = match mode {
            SidebarGroupMode::Spaces | SidebarGroupMode::RepoWorktree => {
                vec!["alpha".to_string(), "beta".to_string()]
            }
            SidebarGroupMode::Repo => vec!["repo-alpha".into(), "repo-beta".into()],
            SidebarGroupMode::RepoPr => vec!["#101 · alpha".into(), "#102 · beta".into()],
            SidebarGroupMode::LinearTeam => vec!["SCA-101".into(), "SCA-102".into()],
            SidebarGroupMode::Missive => {
                vec!["message-101 · alpha".into(), "message-102 · beta".into()]
            }
        };
        let labels = match fixture {
            SpaceTagFixture::MatchingHeaders => group_titles.clone(),
            SpaceTagFixture::DivergentHeaders => match mode {
                SidebarGroupMode::Spaces | SidebarGroupMode::RepoWorktree => {
                    vec!["same".into(), "same".into(), "other".into()]
                }
                _ => vec!["space-alpha".into(), "space-beta".into()],
            },
            SpaceTagFixture::SingleSpace => vec!["only-space".into()],
        };
        let label_refs = labels.iter().map(String::as_str).collect::<Vec<_>>();
        let mut app = app_with_agents(&label_refs);
        app.sidebar_group_mode = mode;

        for ws_idx in 0..app.workspaces.len() {
            let suffix = if ws_idx == 0 { "alpha" } else { "beta" };
            let context = match mode {
                SidebarGroupMode::Spaces | SidebarGroupMode::RepoWorktree => continue,
                SidebarGroupMode::Repo => crate::work_context::PaneWorkContext {
                    repo: Some(format!("repo-{suffix}")),
                    ..Default::default()
                },
                SidebarGroupMode::RepoPr => crate::work_context::PaneWorkContext {
                    pr_urls: vec![format!(
                        "https://github.com/scalable-so/herdr/pull/{}",
                        101 + ws_idx
                    )],
                    work_title: Some(suffix.into()),
                    ..Default::default()
                },
                SidebarGroupMode::LinearTeam => crate::work_context::PaneWorkContext {
                    ticket_ids: vec![format!("SCA-{}", 101 + ws_idx)],
                    ..Default::default()
                },
                SidebarGroupMode::Missive => crate::work_context::PaneWorkContext {
                    missive_urls: vec![format!(
                        "https://mail.missiveapp.com/#inbox/conversations/message-{}",
                        101 + ws_idx
                    )],
                    work_title: Some(suffix.into()),
                    ..Default::default()
                },
            };
            replace_tab_context(&mut app, ws_idx, 0, context, Default::default());
        }
        app.reconcile_sidebar_presentation();
        app
    }

    fn sidebar_tab_entries(app: &AppState) -> Vec<AgentPanelEntry> {
        sidebar_rows(app)
            .into_iter()
            .filter_map(|row| match row {
                SidebarRow::Tab { entry, .. } => Some(*entry),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn sidebar_entry_shows_only_positive_active_subagent_counts() {
        for (value, expected) in [(None, None), (Some(0), None), (Some(3), Some(3))] {
            let mut app = app_with_agents(&["one"]);
            if value.is_some() {
                set_active_subagents(&mut app, 0, value);
            }
            let entry = all_agent_panel_entries(&app).remove(0);
            assert_eq!(entry.active_subagents, expected, "value {value:?}");
        }
    }

    #[test]
    fn active_subagent_count_is_dimmed_and_right_aligned() {
        let mut app = app_with_agents(&["one"]);
        app.workspaces[0].tabs[0].custom_name = Some("render sidebar count".into());
        set_active_subagents(&mut app, 0, Some(3));
        let area = Rect::new(0, 0, 40, 20);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();

        let card = compute_tab_card_areas(&app, area)[0].clone();
        let buffer = terminal.backend().buffer();
        let rendered = row_text(buffer, card.rect.y, card.rect.width);
        assert!(rendered.contains("pi+3"), "{rendered:?}");
        let provider_x = rendered.find("pi+3").expect("provider mark") as u16;
        let provider_style = buffer[(provider_x, card.rect.y)].style();
        assert_eq!(provider_style.fg, Some(app.palette.mauve));
        assert!(provider_style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn zero_and_missing_subagent_counts_render_identically() {
        let mut missing = app_with_agents(&["one"]);
        missing.workspaces[0].tabs[0].custom_name = Some("quiet row".into());
        let mut zero = app_with_agents(&["one"]);
        zero.workspaces[0].tabs[0].custom_name = Some("quiet row".into());
        set_active_subagents(&mut zero, 0, Some(0));

        let missing_row = render_first_tab_row(&missing, 40);
        let zero_row = render_first_tab_row(&zero, 40);
        assert_eq!(zero_row, missing_row);
        assert!(!zero_row.contains(ACTIVE_SUBAGENT_GLYPH));
    }

    #[test]
    fn narrow_subagent_count_preserves_title_and_column_alignment() {
        let mut app = app_with_agents(&["one"]);
        app.workspaces[0].tabs[0].custom_name = Some("narrow sidebar title".into());
        set_active_subagents(&mut app, 0, Some(3));
        let area = Rect::new(0, 0, 18, 20);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();

        let card = compute_tab_card_areas(&app, area)[0].clone();
        let buffer = terminal.backend().buffer();
        let rendered = row_text(buffer, card.rect.y, card.rect.width);
        assert!(rendered.contains("pi+3"), "{rendered:?}");
        assert!(!rendered.contains("⚙"), "{rendered:?}");
        assert!(display_width(&rendered) <= usize::from(card.rect.width));
    }

    #[test]
    fn red_gate_dot_and_dimmed_subagent_count_share_the_row() {
        let mut app = app_with_agents(&["one"]);
        app.workspaces[0].tabs[0].custom_name = Some("blocked review".into());
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].terminal_id(pane_id).unwrap().clone();
        let terminal_state = app.terminals.get_mut(&terminal_id).unwrap();
        terminal_state.state = AgentState::Idle;
        terminal_state.apply_closing_block_payload(
            vec![crate::api::schema::ClosingBlockItem {
                n: 1,
                label: "Gate".into(),
                text: "Approve the PR".into(),
                pr: None,
                ticket: None,
                url: None,
                default: None,
                default_at: None,
            }],
            Vec::new(),
            Vec::new(),
        );
        set_active_subagents(&mut app, 0, Some(3));
        app.reconcile_sidebar_presentation();

        for width in [18, 40] {
            let area = Rect::new(0, 0, width, 20);
            let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
            terminal
                .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
                .unwrap();
            let card = compute_tab_card_areas(&app, area)[0].clone();
            let buffer = terminal.backend().buffer();
            let gate_x = (card.rect.x..card.rect.x + card.rect.width)
                .find(|x| {
                    let cell = &buffer[(*x, card.rect.y)];
                    cell.symbol() == "○" && cell.fg == app.palette.red
                })
                .unwrap_or_else(|| panic!("width {width} omitted red gate dot"));
            let gate_style = buffer[(gate_x, card.rect.y)].style();
            let rendered = row_text(buffer, card.rect.y, card.rect.width);
            assert!(rendered.contains("pi+3"), "width {width}: {rendered:?}");
            assert!(!gate_style.add_modifier.contains(Modifier::DIM));
        }
    }

    fn set_closing_agents_token(app: &mut AppState, ws_idx: usize, value: Option<&str>) {
        let pane_id = app.workspaces[ws_idx].tabs[0].root_pane;
        let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .expect("test terminal")
            .metadata_tokens
            .patch(
                std::collections::HashMap::from([(
                    "closing_agents".into(),
                    value.map(str::to_string),
                )]),
                None,
                std::time::Instant::now(),
            );
    }

    #[test]
    fn sidebar_entry_parses_only_positive_closing_agent_counts() {
        for (value, expected) in [
            (None, None),
            (Some(""), None),
            (Some("0"), None),
            (Some("invalid"), None),
            (Some("4294967296"), None),
            (Some("3"), Some(3)),
        ] {
            let mut app = app_with_agents(&["one"]);
            if value.is_some() {
                set_closing_agents_token(&mut app, 0, value);
            }
            let entry = all_agent_panel_entries(&app).remove(0);
            assert_eq!(entry.active_subagents, expected, "value {value:?}");
        }
    }
    fn configure_real_sidebar_agent(
        app: &mut AppState,
        ws_idx: usize,
        agent: Agent,
        provider: &str,
        version: &str,
        title: &str,
        state: AgentState,
        activity_at: std::time::Instant,
    ) {
        app.workspaces[ws_idx].tabs[0].custom_name = Some(title.to_string());
        let pane_id = app.workspaces[ws_idx].tabs[0].root_pane;
        let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).expect("test terminal");
        terminal.terminal_title = Some(title.to_string());
        terminal.set_detected_state_with_screen_signals_at(
            Some(agent),
            AgentState::Working,
            false,
            false,
            true,
            false,
            false,
            activity_at,
        );
        if state != AgentState::Working {
            terminal.set_detected_state_with_screen_signals_at(
                Some(agent),
                state,
                state == AgentState::Blocked,
                state == AgentState::Idle,
                false,
                false,
                false,
                activity_at + std::time::Duration::from_secs(1),
            );
        }
        terminal.detected_agent = Some(agent);
        terminal.state = state;
        terminal.foreground_process_name = Some(provider.to_string());
        terminal
            .set_agent_metadata(crate::terminal::AgentMetadataReport {
                source: format!("test:{provider}:presentation"),
                agent_label: None,
                applies_to_source: None,
                title: None,
                display_agent: Some(version.to_string()),
                state_labels: std::collections::HashMap::new(),
                clear_title: false,
                clear_display_agent: false,
                clear_state_labels: false,
                ttl: None,
                seq: None,
            })
            .expect("test presentation accepted");
    }

    fn app_for_real_sidebar_fixtures(names: &[&str]) -> AppState {
        let mut app = AppState::test_new();
        app.workspaces = names.iter().map(|name| Workspace::test_new(name)).collect();
        app.ensure_test_terminals();
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

    #[test]
    fn a_satisfied_contract_recolours_the_row_dot_without_a_new_shape() {
        let palette = Palette::one_dark();
        let mut entry = aggregation_entry(AgentState::Idle, false, None, "done");

        let done_unread = compact_row_color(&entry, &palette);
        assert_eq!(compact_row_dot(&entry), "\u{25cb}");

        entry.completion_tier = Some(CompletionTier::ContractSatisfied);
        assert_eq!(
            compact_row_color(&entry, &palette),
            palette.mauve,
            "a met contract is the one done you can act on"
        );
        assert_ne!(
            compact_row_color(&entry, &palette),
            done_unread,
            "it must not share a colour with plain done-unread"
        );
        assert_eq!(
            compact_row_dot(&entry),
            "\u{25cb}",
            "the shape stays hollow: colour carries the tier, not a fourth glyph"
        );

        entry.open_blockers = true;
        assert_eq!(
            compact_row_color(&entry, &palette),
            palette.red,
            "an open gate still outranks a satisfied contract"
        );
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
            usage_limited: false,
            ws_idx: 0,
            tab_idx: 0,
            pane_id: crate::layout::PaneId::alloc(),
            primary_label: "workspace".into(),
            space_label: String::new(),
            space_label_redundant: false,
            primary_tab_label: Some("tab".into()),
            tab_has_custom_name: false,
            tab_label_leads_with_agent: false,
            pane_label: None,
            pane_label_is_agent_identity: false,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_label: None,
            agent_kind_label: None,
            agent: None,
            foreground_process_name: foreground_process_name.map(str::to_string),
            agent_context: None,
            has_agent: true,
            prio: true,
            starred: false,
            state,
            open_blockers: false,
            completion_tier: None,
            active_subagents: None,
            holds_shell: false,
            gate_count: 0,
            seen,
            done_since: None,
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
                SidebarRow::NestedHeader { .. } => ('h', 0),
                SidebarRow::SymphonyJob { .. } | SidebarRow::SymphonyEmpty => ('s', 0),
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
            vec![('h', 0), ('w', 0), ('t', 0), ('w', 1), ('t', 1)]
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
        assert_eq!(text.iter().filter(|line| line.contains("Repo")).count(), 1);
        assert!(!text
            .iter()
            .any(|line| line.trim_start().starts_with("agents")));
        assert!(text.iter().any(|line| line.contains("one")));
        assert!(text.iter().any(|line| line.contains("pi")), "{text:?}");
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
        let dot = buffer
            .cell((card.rect.x + 4, card.rect.y))
            .expect("tab row dot");
        assert_eq!(dot.symbol(), "●");
        assert_ne!(dot.fg, app.palette.peach);
        assert!(row_text(buffer, card.rect.y, 39).contains("●"));
    }

    #[test]
    fn prio_panel_renders_flagged_tab_in_bottom_rows_with_workspace_context() {
        let mut app = app_with_agents(&["one", "two"]);
        app.workspaces[1].tabs[0].custom_name = Some("review auth".into());
        app.workspaces[1].tabs[0].set_prio(true);
        let area = Rect::new(0, 0, 40, 16);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let rendered = (0..area.height)
            .map(|row| row_text(terminal.backend().buffer(), row, area.width - 1))
            .collect::<Vec<_>>();
        assert!(!rendered.iter().any(|line| line.contains("PRIO")));
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
        assert_eq!(
            sidebar_entry.primary_label,
            runtime_cwd
                .file_name()
                .expect("live cwd filename")
                .to_string_lossy()
        );
        assert_ne!(
            sidebar_entry.primary_label,
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
    fn tab_aggregation_keeps_the_usage_panes_configured_label() {
        let ordinary_blocker =
            aggregation_entry(AgentState::Blocked, true, None, "answer required");
        let mut usage_limited = aggregation_entry(AgentState::Blocked, true, None, "blocked");
        usage_limited.usage_limited = true;
        usage_limited
            .state_labels
            .insert("usage".into(), "limit".into());

        let aggregated = aggregate_tab_entries(&[ordinary_blocker, usage_limited])
            .remove(&(0, 0))
            .expect("aggregated tab entry");

        assert!(aggregated.usage_limited);
        assert_eq!(
            aggregated.state_labels.get("usage").map(String::as_str),
            Some("limit")
        );
    }

    #[test]
    fn removing_priority_sections_keeps_the_sidebar_single_projection() {
        let app = app_with_agents(&["one", "two", "three"]);
        let area = Rect::new(0, 0, 40, 16);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        assert!(!(0..area.height)
            .map(|row| row_text(terminal.backend().buffer(), row, area.width - 1))
            .any(|line| line.contains("PRIO") || line.contains("Blocked")));
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
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..area.height)
            .map(|row| row_text(buffer, row, area.width - 1))
            .collect::<Vec<_>>();
        assert!(!rendered.iter().any(|line| line.contains("PRIO")));
    }

    #[test]
    fn empty_prio_panel_has_header_only_without_empty_body() {
        let app = app_with_agents(&["one"]);
        let area = Rect::new(0, 0, 40, 16);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        assert!(!(0..area.height)
            .map(|row| row_text(terminal.backend().buffer(), row, area.width - 1))
            .any(|line| line.contains("PRIO")));
    }

    #[test]
    fn collapsing_prio_panel_reclaims_rows_and_normalizes_scroll() {
        let mut app = app_with_agents(&["one", "two", "three", "four", "five", "six"]);
        for workspace in &mut app.workspaces {
            workspace.tabs[0].set_prio(true);
        }
        let area = Rect::new(0, 0, 40, 12);
        app.view.sidebar_rect = area;
        let expanded_visible_rows = workspace_list_visible_count(&app, area, 0);
        app.workspace_scroll = usize::MAX;

        let requested = app.workspace_scroll;
        app.toggle_prio_panel();

        assert!(app.prio_panel_collapsed);
        assert_eq!(
            workspace_list_visible_count(&app, area, 0),
            expanded_visible_rows
        );
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
        assert_eq!(
            buffer
                .cell((card.rect.x + 4, card.rect.y))
                .expect("unflagged dot cell")
                .symbol(),
            "●"
        );
        assert!(row_text(buffer, card.rect.y, 39).contains(title));
    }

    #[test]
    fn desktop_tab_gutters_render_prio_and_never_a_work_link_marker() {
        let mut app = app_with_agents(&["one", "two"]);
        add_work_link(&mut app, 0);
        app.view.terminal_area = Rect::new(40, 0, 80, 20);
        let area = Rect::new(0, 0, 40, 20);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();

        let buffer = terminal.backend().buffer();
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

        assert_eq!(display_width(&linked_layout.title), 9);
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
        assert!(rendered.contains('●'), "{rendered:?}");
        assert!(rendered.contains("pi"), "{rendered:?}");
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

        // Two shapes only: blocked and done-unread are both `○`, and the palette
        // separates them. Asserting the glyph alone would no longer distinguish them.
        assert!(text.iter().any(|line| line.contains('○')), "{text:?}");
        assert!(
            !text.iter().any(|line| line.contains('◆')),
            "done-unread must not reintroduce a third dot shape: {text:?}"
        );

        let buffer = terminal.backend().buffer();
        let dot_colors = (0..area.height)
            .filter_map(|row| {
                (0..area.width - 1)
                    .find(|x| buffer[(*x, row)].symbol() == "○")
                    .map(|x| buffer[(x, row)].style().fg)
            })
            .collect::<std::collections::HashSet<_>>();
        assert!(
            dot_colors.len() >= 2,
            "blocked and done-unread share a shape, so they must differ by colour: {dot_colors:?}"
        );
    }

    #[test]
    fn a_working_pane_with_persisted_gates_keeps_only_the_blue_working_dot() {
        let mut app = app_with_agents(&["one"]);
        let pane = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].terminal_id(pane).unwrap().clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.state = AgentState::Working;
        terminal.apply_closing_block_payload(
            vec![crate::api::schema::ClosingBlockItem {
                n: 1,
                label: "Gate".into(),
                text: "Approve the open PR".into(),
                pr: None,
                ticket: None,
                url: None,
                default: None,
                default_at: None,
            }],
            Vec::new(),
            Vec::new(),
        );
        app.reconcile_sidebar_presentation();

        let entry = sidebar_thread_entries(&app)
            .into_iter()
            .find(|entry| entry.pane_id == pane)
            .expect("pane entry");
        assert!(entry.open_blockers);
        assert_eq!(entry.state, AgentState::Working);
        assert!(!entry_is_blocked(&entry));
        assert!(!gate_overrides_label(&entry));

        let layout = tab_row_layout(
            &entry,
            app.view_observed_at,
            60,
            4,
            &app.palette,
            app.status_indicators,
        );
        assert_eq!(layout.dot, "●");
        assert_eq!(
            agent_panel_label_color(&entry, &app.palette),
            app.palette.blue
        );

        for width in [18, 60] {
            let area = Rect::new(0, 0, width, 12);
            let mut rendered = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
            rendered
                .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
                .unwrap();
            let buffer = rendered.backend().buffer();
            let colored_dots = |color| {
                (0..area.height)
                    .flat_map(|y| (0..area.width).map(move |x| (x, y)))
                    .filter(|(x, y)| {
                        let cell = &buffer[(*x, *y)];
                        cell.symbol() == "●" && cell.fg == color
                    })
                    .count()
            };
            assert_eq!(colored_dots(app.palette.red), 1, "width {width}");
            assert_eq!(colored_dots(app.palette.blue), 0, "width {width}");
        }

        // Clearing the gate does not change the already-correct working row.
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.apply_closing_block_payload(Vec::new(), Vec::new(), Vec::new());
        let entry = sidebar_thread_entries(&app)
            .into_iter()
            .find(|entry| entry.pane_id == pane)
            .expect("pane entry");
        assert!(!entry.open_blockers);
    }

    /// Owner correction to #77: the same latched gate is not blocking while
    /// work runs, then becomes blocking without a new gate report once work
    /// stops.
    #[test]
    fn the_blocked_section_counts_a_latched_gate_only_after_work_stops() {
        let mut app = app_with_agents(&["one"]);
        let pane = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].terminal_id(pane).unwrap().clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.state = AgentState::Working;
        terminal.apply_closing_block_payload(
            vec![crate::api::schema::ClosingBlockItem {
                n: 1,
                label: "Gate".into(),
                text: "Approve the open PR".into(),
                pr: None,
                ticket: None,
                url: None,
                default: None,
                default_at: None,
            }],
            Vec::new(),
            Vec::new(),
        );
        app.reconcile_sidebar_presentation();

        let entry = sidebar_thread_entries(&app)
            .into_iter()
            .find(|entry| entry.pane_id == pane)
            .expect("pane entry");
        assert!(entry.open_blockers);
        assert_eq!(entry.state, AgentState::Working);
        assert!(!entry_is_blocked(&entry));
        assert!(!gate_overrides_label(&entry));

        let blocked_summary = |rows: &[SidebarRow]| {
            let red_rows = rows
                .iter()
                .filter_map(|row| match row {
                    SidebarRow::Agent { entry, .. } | SidebarRow::Tab { entry, .. } => {
                        Some(entry_has_red_dot(entry))
                    }
                    _ => None,
                })
                .filter(|red| *red)
                .count();
            let has_blocked_header = rows.iter().any(|row| {
                matches!(row, SidebarRow::SectionHeader { title, .. } if *title == BLOCKED_SECTION_TITLE)
            });
            (has_blocked_header, red_rows)
        };
        assert_eq!(blocked_summary(&sidebar_rows(&app)), (false, 1));

        app.terminals.get_mut(&terminal_id).unwrap().state = AgentState::Idle;
        app.reconcile_sidebar_presentation();
        let entry = sidebar_thread_entries(&app)
            .into_iter()
            .find(|entry| entry.pane_id == pane)
            .expect("pane entry");
        assert!(entry.open_blockers, "the gate stays latched");
        assert_eq!(entry.state, AgentState::Idle);
        assert!(entry_is_blocked(&entry));
        assert!(gate_overrides_label(&entry));
        assert_eq!(blocked_summary(&sidebar_rows(&app)), (false, 1));
    }

    #[test]
    fn a_working_pane_with_persisted_gates_labels_as_working() {
        let mut app = app_with_agents(&["one"]);
        let pane = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].terminal_id(pane).unwrap().clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.state = AgentState::Working;
        terminal.apply_closing_block_payload(
            vec![crate::api::schema::ClosingBlockItem {
                n: 1,
                label: "Gate".into(),
                text: "Approve the open PR".into(),
                pr: None,
                ticket: None,
                url: None,
                default: None,
                default_at: None,
            }],
            Vec::new(),
            Vec::new(),
        );
        app.reconcile_sidebar_presentation();

        let mut entry = sidebar_thread_entries(&app)
            .into_iter()
            .find(|entry| entry.pane_id == pane)
            .expect("pane entry");
        assert!(entry.open_blockers);
        assert_eq!(entry.state, AgentState::Working);

        let layout = tab_row_layout(
            &entry,
            app.view_observed_at,
            60,
            4,
            &app.palette,
            app.status_indicators,
        );
        assert_eq!(layout.dot, "●");
        let mobile_layout = mobile_tab_row_layout(
            &entry,
            app.view_observed_at,
            18,
            4,
            &app.palette,
            app.status_indicators,
        );
        assert_eq!(mobile_layout.dot, "●");

        // A configured blocked label cannot hide the blue working state.
        entry.state_labels.insert("blocked".into(), "gate".into());
        entry
            .state_labels
            .insert("working".into(), "working".into());
        let layout = tab_row_layout(
            &entry,
            app.view_observed_at,
            60,
            4,
            &app.palette,
            app.status_indicators,
        );
        assert_eq!(layout.dot, "●");

        // Once the pane stops, the unchanged gate supplies that label.
        entry.state = AgentState::Idle;
        let layout = tab_row_layout(
            &entry,
            app.view_observed_at,
            60,
            4,
            &app.palette,
            app.status_indicators,
        );
        assert_eq!(layout.dot, "○");
        let mobile_layout = mobile_tab_row_layout(
            &entry,
            app.view_observed_at,
            18,
            4,
            &app.palette,
            app.status_indicators,
        );
        assert_eq!(mobile_layout.dot, "○");
    }

    #[test]
    fn a_usage_limited_pane_labels_as_usage_and_carries_the_blocker_dot() {
        let app = app_with_agents(&["one"]);
        let pane = app.workspaces[0].tabs[0].root_pane;
        let mut entry = sidebar_thread_entries(&app)
            .into_iter()
            .find(|entry| entry.pane_id == pane)
            .expect("pane entry");
        entry.state = AgentState::Blocked;
        entry.usage_limited = true;
        entry.open_blockers = false;

        let layout = tab_row_layout(
            &entry,
            app.view_observed_at,
            60,
            4,
            &app.palette,
            app.status_indicators,
        );
        assert_eq!(layout.dot, "○");
        assert_eq!(
            agent_panel_label_color(&entry, &app.palette),
            app.palette.red
        );

        // A configured label wins over the fallback.
        entry.state_labels.insert("usage".into(), "limit".into());
        let layout = tab_row_layout(
            &entry,
            app.view_observed_at,
            60,
            4,
            &app.palette,
            app.status_indicators,
        );
        assert_eq!(layout.dot, "○");

        // Live screen only: the moment the agent works again the row is working.
        entry.usage_limited = false;
        entry.state = AgentState::Working;
        let layout = tab_row_layout(
            &entry,
            app.view_observed_at,
            60,
            4,
            &app.palette,
            app.status_indicators,
        );
        assert_eq!(layout.dot, "●");
        assert_eq!(
            agent_panel_label_color(&entry, &app.palette),
            app.palette.blue
        );
    }

    #[test]
    fn usage_limited_worklist_rows_render_a_non_color_cue_at_supported_widths() {
        let mut app = app_with_agents(&["Wait for plan reset"]);
        app.workspaces[0].tabs[0].custom_name = Some("Wait for plan reset".into());
        let pane = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].terminal_id(pane).unwrap().clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal
            .set_agent_metadata(crate::terminal::AgentMetadataReport {
                source: "test:usage-limit-worklist".into(),
                agent_label: None,
                applies_to_source: None,
                title: None,
                display_agent: None,
                state_labels: std::collections::HashMap::from([("usage".into(), "limit".into())]),
                clear_title: false,
                clear_display_agent: false,
                clear_state_labels: false,
                ttl: None,
                seq: None,
            })
            .expect("test presentation accepted");
        terminal.state = AgentState::Blocked;
        terminal.usage_limited = true;
        app.reconcile_sidebar_presentation();

        for width in [18, 24, 60] {
            let area = Rect::new(0, 0, width, 12);
            let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
            terminal
                .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
                .unwrap();
            let rows = (0..area.height)
                .map(|row| row_text(terminal.backend().buffer(), row, area.width - 1))
                .collect::<Vec<_>>();
            let usage_row = rows
                .iter()
                .find(|row| row.contains('○') && row.contains("pi"))
                .unwrap_or_else(|| panic!("width {width} omitted usage cue: {rows:?}"));
            assert!(usage_row.contains('○'), "{usage_row:?}");
            assert!(!usage_row.contains("limit"), "{usage_row:?}");
            if width >= 24 {
                assert!(
                    usage_row.contains("Wai") || usage_row.contains("plan"),
                    "width {width} dropped the title before the provider mark: {usage_row:?}"
                );
            }
        }
    }

    #[test]
    fn workspace_agent_disclosure_toggles_only_owned_children() {
        let mut app = app_with_agents(&["one", "two"]);
        assert!(app.toggle_workspace_agent_disclosure(1));
        assert_eq!(
            row_kinds(&app),
            vec![('h', 0), ('w', 0), ('t', 0), ('w', 1)]
        );
        let collapsed_worktrees = app.collapsed_space_keys.clone();

        assert!(app.toggle_workspace_agent_disclosure(0));
        assert_eq!(row_kinds(&app), vec![('h', 0), ('w', 0), ('w', 1)]);
        assert!(!app.workspace_agents_expanded(1));
        assert_eq!(app.collapsed_space_keys, collapsed_worktrees);

        assert!(app.toggle_workspace_agent_disclosure(0));
        assert_eq!(
            row_kinds(&app),
            vec![('h', 0), ('w', 0), ('t', 0), ('w', 1)]
        );
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
        assert_eq!(entries[0].agent_label.as_deref(), Some(">_"));
        assert_eq!(row_kinds(&app), vec![('h', 0), ('w', 0), ('t', 0)]);
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
        assert_eq!(keys.previous_window, BindingConfig::one("prefix+p"));
        assert_eq!(keys.next_window, BindingConfig::one("prefix+n"));
        assert_eq!(keys.previous_tab, BindingConfig::one("prefix+ctrl+p"));
        assert_eq!(keys.next_tab, BindingConfig::one("prefix+ctrl+n"));
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
            vec![('h', 0), ('w', 0), ('t', 0), ('w', 1), ('t', 1)]
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
        let header_rows = rows
            .iter()
            .filter(|row| matches!(row, SidebarRow::SectionHeader { .. }))
            .count();

        assert_eq!(
            rows.len(),
            header_rows
                + workspace_cards.len()
                + compute_tab_card_areas(&app, area).len()
                + agent_cards.len()
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
                    SidebarRow::NestedHeader { .. } => ('h', 0),
                    SidebarRow::SymphonyJob { .. } | SidebarRow::SymphonyEmpty => ('s', 0),
                })
                .collect::<Vec<_>>(),
            vec![('h', 0), ('w', 0), ('t', 0), ('w', 1), ('t', 1)]
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
                | SidebarRow::SectionHeader { .. }
                | SidebarRow::NestedHeader { .. }
                | SidebarRow::SymphonyJob { .. }
                | SidebarRow::SymphonyEmpty => None,
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
                    SidebarRow::NestedHeader { .. } => ("section", 0, None, None),
                    SidebarRow::SymphonyJob { .. } | SidebarRow::SymphonyEmpty => {
                        ("symphony", 0, None, None)
                    }
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
            .any(|row| row.contains("Polish sidebar selection") && row.contains("cx")));
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
             <p>Actual Ratatui test buffer: selected titles, lifecycle dots, provider suffix,\
             live-shell marks, one row per tab, multi-pane roll-up, and compact spacing\
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

        // One Spaces projection and exactly one row per tab/window.
        assert_eq!(
            row_kinds(&app),
            vec![('h', 0), ('w', 0), ('t', 0), ('t', 0),]
        );
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

        // ac3: the lifecycle dot is left of the single title, with no model subtitle.
        let status_at = working.find('●').unwrap();
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
        assert!(tab_window.contains("Fix Billing Retry"), "{tab_window:?}");
        assert!(tab_window.contains("pi"), "{tab_window:?}");
        assert!(!first.contains("working"));
        assert!(tab_window.contains("●"));
        assert!(agent_cards.is_empty());

        let workspace_x = find_symbol_x(buffer, workspace_row, 59, "o");
        let workspace_style = buffer[(workspace_x, workspace_row)].style();
        // ac2: active titles are visibly darker than the prior One Light text.
        assert_eq!(workspace_style.fg, Some(Color::Rgb(37, 38, 44)));
        assert!(workspace_style.add_modifier.contains(Modifier::BOLD));
        assert!(!workspace_style.add_modifier.contains(Modifier::DIM));
        // Sidebar rows sit on the palette's own background, never the
        // terminal's: a row must stay readable under a mismatched theme.
        assert_eq!(workspace_style.bg, Some(app.palette.sidebar_background()));

        let title_x = find_symbol_x(buffer, tab_row, 59, "F");
        let title_style = buffer[(title_x, tab_row)].style();
        assert_eq!(title_style.fg, Some(Color::Rgb(37, 38, 44)));
        assert!(title_style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(title_style.bg, Some(app.palette.sidebar_background()));
    }

    #[test]
    fn tab_rows_show_working_then_done_lifecycle_dots() {
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
        assert!(busy_text.contains("●"), "{busy_text:?}");
        assert!(busy_text.ends_with('—'), "{busy_text:?}");
        assert!(!busy_text.contains(" · one"), "{busy_text:?}");
        let dot_x = find_symbol_x(busy.backend().buffer(), tab_row, 49, "●");
        assert_eq!(
            busy.backend().buffer()[(dot_x, tab_row)].style().fg,
            Some(app.palette.blue)
        );

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
        assert!(idle_text.contains("○"), "{idle_text:?}");
        assert!(idle_text.ends_with("5m"), "{idle_text:?}");
        assert!(!idle_text.contains(" · one"), "{idle_text:?}");
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
        assert!(rendered.ends_with("1m"), "{rendered:?}");
        assert!(!rendered.contains(" · one"), "{rendered:?}");
        assert!(!rendered.contains("idle"), "{rendered:?}");
        assert!(!rendered.contains("done"), "{rendered:?}");
        assert!(rendered.contains("○  Review release"), "{rendered:?}");
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

        assert!(rendered.ends_with("5m"), "{rendered:?}");
        assert!(!rendered.contains(" · one"), "{rendered:?}");
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
        assert!(rendered.contains("pi"), "{rendered:?}");
        assert!(!rendered.contains("reported"), "{rendered:?}");
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

        assert!(!rendered.contains("cargo"), "{rendered:?}");
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

        assert_eq!(layout.provider, "pi");
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
        entry.activity_at = Some(
            app.view_observed_at
                .checked_sub(std::time::Duration::from_secs(12 * 60))
                .expect("activity timestamp before observation time"),
        );

        // Activity age is the useful recency signal at this width, so the
        // foreground process and background count yield first.
        let layout = tab_row_layout(
            &entry,
            app.view_observed_at,
            32,
            1,
            &app.palette,
            app.status_indicators,
        );

        assert_eq!(layout.provider, "pi");
        assert!(layout.activity_age.is_some());
    }

    #[test]
    fn reported_at_age_refreshes_with_space_suffix() {
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
        assert_eq!(layout.provider, "pi");
        assert_eq!(layout.dot, "●");
        assert!(display_width(&layout.title) <= 32);
    }

    #[test]
    fn space_suffix_preserves_visible_activity_age_deadlines() {
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

    /// The reported defect: a Claude pane labelled with its worktree directory
    /// instead of the session Claude named, and a label that never moved when
    /// Claude renamed the session mid-run.
    #[test]
    fn session_rename_updates_the_sidebar_and_tab_bar_without_a_restart() {
        let mut app = app_with_agents(&["one"]);
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        {
            let terminal = app.terminals.get_mut(&terminal_id).unwrap();
            // Herdr tracks the tab's checkout while the agent runs in a
            // worktree below it, so the cwd-noise filter cannot recognise the
            // directory name the agent paints as its terminal title.
            terminal.cwd = std::path::PathBuf::from("/w/personal");
            terminal.set_terminal_title(Some("cc-personal-20260825-150216-8ada".into()));
        }

        let canonical = |app: &AppState| app.workspaces[0].tab_display_name_from(&app.terminals, 0);
        let sidebar_label = |app: &AppState| {
            sidebar_thread_entries(app)
                .into_iter()
                .next()
                .expect("sidebar tab entry")
                .primary_tab_label
        };
        let tab_bar_label = |app: &AppState| {
            crate::ui::tabs::tab_chrome_label(&app.workspaces[0], &app.terminals, 0, usize::MAX)
        };

        // AC4: with no session name the pre-existing derivation still produces
        // a non-empty label.
        let fallback = canonical(&app).expect("fallback tab name");
        assert_eq!(fallback, "cc-personal-20260825-150216-8ada");
        assert_eq!(sidebar_label(&app).as_deref(), Some(fallback.as_str()));

        // AC1: the name Claude gave the session outranks the directory label.
        let changed = app
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_hook_session_name(Some("Herdr fork sidebar naming".into()))
            .expect("session name accepted");
        assert!(changed);
        let named = canonical(&app).expect("named tab name");
        assert_eq!(named, "Herdr fork sidebar naming");
        assert_ne!(named, fallback);
        assert_eq!(sidebar_label(&app).as_deref(), Some(named.as_str()));
        // AC3: the tab bar resolves the same canonical name.
        assert_eq!(tab_bar_label(&app), named);

        // AC2: a rename on the same live pane moves both surfaces. Nothing is
        // rebuilt here — this is the same AppState the assertions above read.
        let changed = app
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_hook_session_name(Some("Session rename propagation".into()))
            .expect("rename accepted");
        assert!(changed);
        let renamed = canonical(&app).expect("renamed tab name");
        assert_eq!(renamed, "Session rename propagation");
        assert_ne!(renamed, named);
        assert_eq!(sidebar_label(&app).as_deref(), Some(renamed.as_str()));
        assert_eq!(tab_bar_label(&app), renamed);

        // The rendered sidebar row shows the new name, not the stale one.
        let area = Rect::new(0, 0, 60, 8);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let tab_row = compute_tab_card_areas(&app, area)[0].rect.y;
        let rendered = row_text(terminal.backend().buffer(), tab_row, area.width - 1);
        assert!(rendered.contains("Session rename"), "{rendered:?}");
        assert!(!rendered.contains("cc-personal"), "{rendered:?}");

        // A human rename still wins over the agent's session name.
        app.workspaces[0].tabs[0].set_user_custom_name("Human title".into());
        assert_eq!(canonical(&app).as_deref(), Some("Human title"));
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
        assert!(rendered.contains("Fix billing"), "{rendered:?}");
        assert!(rendered.contains("pi"), "{rendered:?}");
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

        let entry = sidebar_thread_entries(&app)
            .into_iter()
            .next()
            .expect("sidebar entry");
        assert!(!entry.tab_label_leads_with_agent);

        let rendered = render_first_tab_row(&app, 60);

        assert!(rendered.contains("Fix billing"), "{rendered:?}");
        assert!(rendered.contains("pi"), "{rendered:?}");
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

        let rendered = render_first_tab_row(&app, 60);

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
        assert_eq!(active.bg, Some(app.palette.sidebar_background()));

        let inactive = buffer[(find_symbol_x(buffer, second_row, 25, "t"), second_row)].style();
        assert_eq!(inactive.fg, Some(app.palette.subtext0));
        assert!(!inactive
            .add_modifier
            .intersects(Modifier::BOLD | Modifier::DIM));
        assert_eq!(inactive.bg, Some(app.palette.sidebar_background()));
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
        assert!(rendered.contains("one (0/1)"), "{rendered:?}");
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

        // Slice 10a adds the search row above the existing View row, so this
        // fixture needs enough height to retain its two-row list viewport.
        let area = Rect::new(0, 0, 20, 10);
        let ws_area = workspace_list_rect(area, app.sidebar_section_split);
        let metrics = workspace_list_scroll_metrics(&app, ws_area);
        let body = workspace_list_body_rect(ws_area, should_show_scrollbar(metrics));
        let mut terminal = Terminal::new(TestBackend::new(20, 10)).unwrap();
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
        let first = row_text(buffer, card.rect.y, 17);
        assert!(first.contains("pi"), "rendered row: {first:?}");
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

        // The search and View header rows leave two list rows here; legacy
        // space-row configuration still cannot turn either card multiline.
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
        app.active = Some(0);
        // Preserve the three-row panel body now that the header has search and
        // view rows.
        let panel = Rect::new(0, 0, 20, 6);
        let cards = compute_tab_card_areas(&app, panel);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].rect.height, 1);
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
                SidebarRow::Tab { entry, depth } if *depth == 1 => Some(entry),
                _ => None,
            })
            .expect("blocked space row should contain a tab row");

        assert_eq!(
            sidebar_row_height(
                &app,
                &SidebarRow::Tab {
                    entry: blocked.clone(),
                    depth: 1,
                },
                8
            ),
            1,
            "compact tab rows always occupy one painted line"
        );
        let card = compute_tab_card_areas(&app, area)
            .into_iter()
            .find(|card| card.ws_idx == blocked.ws_idx && card.pane_id == blocked.pane_id)
            .expect("blocked space row should have geometry");
        assert_eq!(card.rect.height, 1);
    }

    #[test]
    fn a_blocked_worklist_row_hides_its_redundant_space_suffix() {
        let mut app = priority_app_with_states(&[AgentState::Blocked, AgentState::Working]);
        app.sidebar_group_mode = SidebarGroupMode::Spaces;
        let blocked_pane = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].terminal_id(blocked_pane).unwrap().clone();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_terminal_title(Some("Review blocked thread".into()));
        let area = Rect::new(0, 0, 50, 12);
        let card = compute_tab_card_areas(&app, area)
            .into_iter()
            .find(|card| card.pane_id == blocked_pane)
            .expect("blocked compact row should render");
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("test terminal should initialize");
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .expect("sidebar should render");

        let rendered = row_text(terminal.backend().buffer(), card.rect.y, area.width - 1);
        assert!(rendered.contains("Review blocked thread"), "{rendered:?}");
        assert!(rendered.contains('—'), "{rendered:?}");
        assert!(!rendered.contains(" · ws0"), "{rendered:?}");
    }

    #[test]
    fn f19_1a_keeps_age_and_appends_divergent_space_suffix_when_wide() {
        let mut app = app_with_agents(&["one", "two"]);
        app.workspaces[0].custom_name = Some("t3-sample".into());
        app.workspaces[1].custom_name = Some("other-space".into());
        app.workspaces[0].tabs[0].custom_name = Some("sample-pr".into());
        app.sidebar_group_mode = SidebarGroupMode::Repo;
        replace_tab_context(
            &mut app,
            0,
            0,
            crate::work_context::PaneWorkContext {
                repo: Some("herdr".into()),
                ..Default::default()
            },
            Default::default(),
        );
        replace_tab_context(
            &mut app,
            1,
            0,
            crate::work_context::PaneWorkContext {
                repo: Some("growth".into()),
                ..Default::default()
            },
            Default::default(),
        );
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let started = std::time::Instant::now();
        app.terminals
            .get_mut(&terminal_id)
            .expect("test terminal")
            .set_detected_state_with_screen_signals_at(
                Some(Agent::Pi),
                AgentState::Working,
                false,
                false,
                true,
                false,
                false,
                started,
            );
        app.view_observed_at = started + std::time::Duration::from_secs(125);
        app.reconcile_sidebar_presentation();
        let entry = sidebar_thread_entries(&app)
            .into_iter()
            .next()
            .expect("thread entry");
        assert_eq!(entry.space_label, "t3-sample");
        assert!(!entry.space_label_redundant);

        let render_at_row_width = |entry: &AgentPanelEntry, width, depth| {
            let mut terminal =
                Terminal::new(TestBackend::new(width, 1)).expect("test terminal should initialize");
            terminal
                .draw(|frame| {
                    render_compact_agent_row(
                        &app,
                        frame,
                        entry,
                        Rect::new(0, 0, width, 1),
                        depth,
                        true,
                        None,
                    )
                })
                .expect("compact row should render");
            row_text(terminal.backend().buffer(), 0, width)
        };
        let below_suffix_threshold = render_at_row_width(&entry, 43, 0);
        assert!(
            !below_suffix_threshold.contains("· t3-sample"),
            "{below_suffix_threshold:?}"
        );
        let at_suffix_threshold = render_at_row_width(&entry, 44, 0);
        assert!(
            at_suffix_threshold.contains("2m · t3-sample"),
            "{at_suffix_threshold:?}"
        );

        let default_width = render_first_tab_row(&app, 40);
        assert!(default_width.contains("sample-pr"), "{default_width:?}");
        assert!(default_width.contains("2m"), "{default_width:?}");
        assert!(!default_width.contains("· t3-sample"), "{default_width:?}");

        let github_depth = render_at_row_width(&entry, 25, 1);
        assert_eq!(github_depth, "    ●  sample-pr   pi  2m");
        let repo_branch_depth = render_at_row_width(&entry, 25, 2);
        assert_eq!(repo_branch_depth, "      ●  sample-pr pi  2m");
        let mut ticket_entry = entry.clone();
        ticket_entry.primary_tab_label = Some("SCA-3165 · sample-linear".into());
        let nested_ticket = render_at_row_width(&ticket_entry, 25, 2);
        assert_eq!(nested_ticket, "   ●  sample-line… pi  2m");

        let area = Rect::new(0, 0, 80, 12);
        let cards = compute_tab_card_areas(&app, area);
        let card = cards[0].clone();
        let rect_width = usize::from(card.rect.width);
        let requested_prefix_width = usize::from(card.depth) * 3 + 1;
        let provider = compact_provider(&entry);
        let widths = compact_row_widths(
            compact_row_title(&entry, true),
            &provider,
            rect_width,
            requested_prefix_width,
        );
        let fixed_width = widths.prefix + SIDEBAR_DOT_FIELD_WIDTH + widths.provider + widths.age;
        let retained_title_width = rect_width
            .saturating_sub(fixed_width)
            .saturating_sub(display_width(" · t3-sample"));
        assert!(
            rect_width >= SIDEBAR_SPACE_SUFFIX_MIN_ROW_WIDTH,
            "{rect_width}"
        );
        assert!(
            retained_title_width >= SIDEBAR_SPACE_SUFFIX_MIN_TITLE_WIDTH,
            "{retained_title_width}"
        );

        let wide = render_first_tab_row(&app, 80);
        assert!(wide.contains("sample-pr"), "{wide:?}");
        assert!(wide.contains("2m · t3-sample"), "{wide:?}");
    }

    #[test]
    fn compact_rows_hide_space_suffix_when_group_header_repeats_it_in_every_mode() {
        for mode in SidebarGroupMode::ALL {
            let app = space_tag_fixture(mode, SpaceTagFixture::MatchingHeaders);
            let entries = sidebar_tab_entries(&app);
            assert!(!entries.is_empty(), "mode {mode:?}");
            let labels = entries
                .iter()
                .map(|entry| (entry.space_label.as_str(), entry.space_label_redundant))
                .collect::<Vec<_>>();
            assert!(
                entries.iter().all(|entry| entry.space_label_redundant),
                "mode {mode:?}: {labels:?}"
            );
            let first_label = &entries[0].space_label;
            let rendered = render_first_tab_row(&app, 120);
            assert!(
                !rendered.contains(&format!(" · {first_label}")),
                "{mode:?}: {rendered:?}"
            );
        }
    }

    #[test]
    fn compact_rows_keep_space_suffix_when_group_header_differs_in_every_mode() {
        for mode in SidebarGroupMode::ALL {
            let app = space_tag_fixture(mode, SpaceTagFixture::DivergentHeaders);
            let entries = sidebar_tab_entries(&app);
            assert!(!entries.is_empty(), "mode {mode:?}");
            let first = &entries[0];
            assert!(!first.space_label_redundant, "mode {mode:?}");
            let rendered = render_first_tab_row(&app, 120);
            assert!(
                rendered.contains(&format!(" · {}", first.space_label)),
                "{mode:?}: {rendered:?}"
            );
        }
    }

    #[test]
    fn compact_rows_hide_space_suffix_for_a_single_space_in_every_mode() {
        for mode in SidebarGroupMode::ALL {
            let app = space_tag_fixture(mode, SpaceTagFixture::SingleSpace);
            let entries = sidebar_tab_entries(&app);
            assert!(!entries.is_empty(), "mode {mode:?}");
            assert!(
                entries.iter().all(|entry| entry.space_label_redundant),
                "mode {mode:?}"
            );
            let rendered = render_first_tab_row(&app, 120);
            assert!(
                !rendered.contains(" · only-space"),
                "{mode:?}: {rendered:?}"
            );
        }
    }

    #[test]
    fn space_suffix_header_matching_covers_legacy_unlinked_branch_and_settled_groups() {
        let mut legacy = app_with_agents(&["alpha", "beta"]);
        legacy.sidebar_group_mode = SidebarGroupMode::Repo;
        assert!(sidebar_tab_entries(&legacy)
            .iter()
            .all(|entry| entry.space_label_redundant));

        let mut unlinked = app_with_agents(&["▫ alpha", "▫ beta"]);
        unlinked.sidebar_group_mode = SidebarGroupMode::RepoPr;
        for (ws_idx, directory) in ["/work/alpha", "/work/beta"].into_iter().enumerate() {
            let pane_id = unlinked.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = unlinked.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            unlinked
                .terminals
                .get_mut(&terminal_id)
                .expect("unlinked terminal")
                .cwd = directory.into();
        }
        assert!(sidebar_tab_entries(&unlinked)
            .iter()
            .all(|entry| entry.space_label_redundant));

        let mut branched = app_with_agents(&["⎇ main", "other"]);
        branched.sidebar_group_mode = SidebarGroupMode::Repo;
        for (ws_idx, branch) in ["main", "other"].into_iter().enumerate() {
            replace_tab_context(
                &mut branched,
                ws_idx,
                0,
                crate::work_context::PaneWorkContext {
                    repo: Some("herdr".into()),
                    branch: Some(branch.into()),
                    ..Default::default()
                },
                Default::default(),
            );
        }
        let branch_entry = sidebar_tab_entries(&branched)
            .into_iter()
            .find(|entry| entry.ws_idx == 0)
            .expect("main branch entry");
        assert!(branch_entry.space_label_redundant);

        let mut settled =
            space_tag_fixture(SidebarGroupMode::RepoPr, SpaceTagFixture::MatchingHeaders);
        let settled_pane = settled.workspaces[0].tabs[0].root_pane;
        settled.workspaces[0].tabs[0]
            .panes
            .get_mut(&settled_pane)
            .expect("settled pane")
            .settled_at = Some(1_725_000_000);
        let settled_entry = sidebar_tab_entries(&settled)
            .into_iter()
            .find(|entry| entry.pane_id == settled_pane)
            .expect("settled entry");
        assert!(settled_entry.space_label_redundant);
    }

    #[test]
    fn duplicate_space_headers_keep_space_suffix_for_disambiguation() {
        let mut app = app_with_agents(&["first", "second", "third"]);
        app.workspaces[0].custom_name = Some("same".into());
        app.workspaces[1].custom_name = Some("same".into());
        app.workspaces[2].custom_name = Some("other".into());
        app.sidebar_group_mode = SidebarGroupMode::Spaces;

        let labels = sidebar_workspace_labels(&app, &TerminalRuntimeRegistry::new());
        assert_eq!(labels[&0].0, "same¹");
        assert_eq!(labels[&1].0, "same²");
        assert!(sidebar_tab_entries(&app)
            .into_iter()
            .filter(|entry| entry.space_label == "same")
            .all(|entry| !entry.space_label_redundant));
        assert!(render_first_tab_row(&app, 80).contains(" · same"));
    }

    #[test]
    fn f19_8_narrow_rows_drop_identifiers_and_share_one_marker_column() {
        let app = AppState::test_new();
        let mut rows = Vec::new();
        for (title, depth, recently_done) in [
            ("SCA-3165 · sample-linear", 2, false),
            ("#159 · sample-pr", 1, false),
            ("sample-missive", 1, false),
            ("sample-settled", 0, true),
        ] {
            let mut entry = compact_test_entry(title, Some(Agent::Claude));
            entry.state = AgentState::Working;
            rows.push(if recently_done {
                SidebarRow::Agent {
                    entry: Box::new(entry),
                    depth,
                }
            } else {
                SidebarRow::Tab {
                    entry: Box::new(entry),
                    depth,
                }
            });
        }

        let prefix = narrow_view_tab_prefix_from_rows(&rows, 25);
        assert_eq!(prefix, Some(1));
        let mut marker_columns = Vec::new();
        for (row, expected_title) in rows.iter().zip([
            "sample-linear",
            "sample-pr",
            "sample-missive",
            "sample-settled",
        ]) {
            let (entry, depth, tab) = match row {
                SidebarRow::Tab { entry, depth } => (entry, depth, true),
                SidebarRow::Agent { entry, depth } => (entry, depth, false),
                _ => unreachable!("fixture contains only compact rows"),
            };
            let mut terminal =
                Terminal::new(TestBackend::new(25, 1)).expect("test terminal should initialize");
            terminal
                .draw(|frame| {
                    render_compact_agent_row_with_prefix(
                        &app,
                        frame,
                        entry,
                        Rect::new(0, 0, 25, 1),
                        *depth,
                        tab,
                        None,
                        prefix,
                    )
                })
                .expect("narrow compact row should render");
            let rendered = row_text(terminal.backend().buffer(), 0, 25);
            assert!(!rendered.contains("SCA-3165 ·"), "{rendered:?}");
            assert!(!rendered.contains("#159 ·"), "{rendered:?}");
            assert!(rendered.contains(expected_title), "{rendered:?}");
            marker_columns.push(rendered.find('●').expect("working marker"));
        }
        assert_eq!(marker_columns, vec![1, 1, 1, 1]);

        let short_github = compact_row_title_for_width("#1 · fix", "cc", 24, 7);
        assert_eq!(short_github, "fix");
        assert_eq!(compact_row_widths(short_github, "cc", 24, 7).prefix, 7);

        assert_eq!(
            narrow_view_tab_prefix_from_rows(&rows, 43),
            None,
            "nested rows keep their extra level when every title fits"
        );

        let mut rendered_app = app_with_agents(&["linear", "pr", "missive", "settled"]);
        for (workspace, title) in rendered_app.workspaces.iter_mut().zip([
            "sample-linear",
            "sample-pr",
            "sample-missive",
            "sample-settled",
        ]) {
            workspace.tabs[0].custom_name = Some(title.into());
        }
        replace_tab_context(
            &mut rendered_app,
            0,
            0,
            crate::work_context::PaneWorkContext {
                ticket_ids: vec!["SCA-3165".into()],
                work_title: Some("sample-linear".into()),
                ..Default::default()
            },
            Default::default(),
        );
        replace_tab_context(
            &mut rendered_app,
            1,
            0,
            crate::work_context::PaneWorkContext {
                pr_urls: vec!["https://github.com/herdrdev/herdr/pull/159".into()],
                work_title: Some("sample-pr".into()),
                ..Default::default()
            },
            Default::default(),
        );
        rendered_app.set_sidebar_group_mode(SidebarGroupMode::RepoPr);
        rendered_app.reconcile_sidebar_presentation();
        let area = Rect::new(0, 0, 26, 20);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
        terminal
            .draw(|frame| {
                render_sidebar(&rendered_app, &TerminalRuntimeRegistry::new(), frame, area)
            })
            .expect("narrow grouped view should render");
        let rendered_rows = (0..area.height)
            .map(|y| row_text(terminal.backend().buffer(), y, area.width - 1))
            .filter(|row| row.contains("sample-"))
            .collect::<Vec<_>>();
        assert_eq!(rendered_rows.len(), 4, "{rendered_rows:#?}");
        for expected_title in ["sample-line", "sample-pr", "sample-miss", "sample-sett"] {
            let row = rendered_rows
                .iter()
                .find(|row| row.contains(expected_title))
                .expect("readable seeded title fragment");
            assert_eq!(row.find('●'), Some(3), "{row:?}");
        }
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

            let controls = row_text(terminal.backend().buffer(), 0, width - 1);
            let view = row_text(terminal.backend().buffer(), 1, width - 1);
            assert!(controls.starts_with('«'));
            assert!(controls.contains('✎'));
            assert_eq!(controls.matches('+').count(), 1, "{controls:?}");
            assert!(!controls.contains('＋'));
            assert!(controls.contains('…'));
            assert!(view.contains("Repo"));
            assert!(view.contains('▾'));
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
                SidebarRow::NestedHeader { title, .. } => ("section", title),
                SidebarRow::SymphonyJob { name, .. } => ("symphony", name),
                SidebarRow::SymphonyEmpty => ("symphony", String::new()),
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
    fn a_pane_with_an_unanswered_gate_is_never_hidden_as_done() {
        let done_since = std::time::Instant::now();
        let mut app = priority_app_with_states(&[AgentState::Idle]);
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let pane = app.workspaces[0].tabs[0].panes.get_mut(&pane_id).unwrap();
        pane.seen = false;
        pane.done_since = Some(done_since);
        app.hide_done_after = std::time::Duration::from_secs(30 * 60);
        app.terminals.get_mut(&terminal_id).unwrap().closing_gates =
            vec![crate::api::schema::ClosingBlockItem {
                n: 1,
                label: "Gate".into(),
                text: "Choose the release path".into(),
                pr: None,
                ticket: None,
                url: None,
                default: None,
                default_at: None,
            }];

        app.view_observed_at =
            done_since + app.hide_done_after + std::time::Duration::from_secs(60);
        let rows = sidebar_rows(&app);
        assert!(
            !rows.iter().any(|row| matches!(
                row,
                SidebarRow::SectionHeader { title, .. } if *title == RECENTLY_DONE_SECTION_TITLE
            )),
            "a pane waiting on a human must not be filed under Recently done"
        );
        assert!(
            rows.iter().any(|row| matches!(row, SidebarRow::Tab { .. })),
            "and it must stay visible as a row"
        );
    }

    #[test]
    fn done_pane_moves_to_recently_done_only_after_hide_threshold() {
        let done_since = std::time::Instant::now();
        let mut app = priority_app_with_states(&[AgentState::Idle]);
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let pane = app.workspaces[0].tabs[0].panes.get_mut(&pane_id).unwrap();
        pane.seen = false;
        pane.done_since = Some(done_since);
        app.hide_done_after = std::time::Duration::from_secs(30 * 60);

        app.view_observed_at = done_since + app.hide_done_after;
        let boundary = sidebar_rows(&app);
        assert!(!boundary.iter().any(|row| matches!(
            row,
            SidebarRow::SectionHeader { title, .. }
                if *title == RECENTLY_DONE_SECTION_TITLE
        )));
        assert!(boundary
            .iter()
            .any(|row| matches!(row, SidebarRow::Tab { .. })));

        app.view_observed_at += std::time::Duration::from_nanos(1);
        let hidden = sidebar_rows(&app);
        assert!(hidden.iter().any(|row| matches!(
            row,
            SidebarRow::SectionHeader {
                title,
                count: 1,
                collapsed: true,
            } if *title == RECENTLY_DONE_SECTION_TITLE
        )));
        assert!(!hidden
            .iter()
            .any(|row| matches!(row, SidebarRow::Tab { .. })));

        app.collapsed_sidebar_groups.remove("repo:Recently done");
        let expanded = sidebar_rows(&app);
        assert!(expanded
            .iter()
            .any(|row| matches!(row, SidebarRow::Agent { .. })));
    }

    #[test]
    fn recently_done_header_renders_at_narrow_and_normal_widths() {
        let done_since = std::time::Instant::now();
        let mut app = priority_app_with_states(&[AgentState::Idle]);
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let pane = app.workspaces[0].tabs[0].panes.get_mut(&pane_id).unwrap();
        pane.seen = false;
        pane.done_since = Some(done_since);
        app.view_observed_at =
            done_since + app.hide_done_after + std::time::Duration::from_nanos(1);

        for width in [18, 40] {
            let expected_label = if width == 18 {
                "Recently"
            } else {
                RECENTLY_DONE_SECTION_TITLE
            };
            let area = Rect::new(0, 0, width, 20);
            let mut desktop = Terminal::new(TestBackend::new(width, area.height)).unwrap();
            desktop
                .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
                .unwrap();
            let desktop_text = (0..area.height)
                .map(|row| row_text(desktop.backend().buffer(), row, width))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(desktop_text.contains(expected_label), "{width}");

            app.view.mobile_header_rect = Rect::new(0, 0, width, 2);
            app.view.terminal_area = Rect::new(0, 2, width, 18);
            let mut mobile = Terminal::new(TestBackend::new(width, area.height)).unwrap();
            mobile
                .draw(|frame| {
                    super::super::mobile::render_mobile_panel(
                        &app,
                        &TerminalRuntimeRegistry::new(),
                        frame,
                        area,
                    )
                })
                .unwrap();
            let mobile_text = (0..area.height)
                .map(|row| row_text(mobile.backend().buffer(), row, width))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(mobile_text.contains(expected_label), "{width}");
        }
    }

    #[test]
    fn the_starred_only_gate_hides_every_unstarred_session() {
        let mut app =
            priority_app_with_states(&[AgentState::Working, AgentState::Idle, AgentState::Idle]);
        app.workspaces[1].tabs[0].starred = true;
        for ws_idx in 0..3 {
            app.toggle_workspace_agent_disclosure(ws_idx);
        }

        let all = sidebar_thread_entries(&app).len();
        assert!(all >= 3, "the fixture projects one entry per workspace");

        app.sidebar_starred_only = true;
        let visible: Vec<_> = sidebar_rows(&app)
            .into_iter()
            .filter_map(|row| match row {
                SidebarRow::Tab { entry, .. } => Some((entry.ws_idx, entry.starred)),
                SidebarRow::Agent { entry, .. } => Some((entry.ws_idx, entry.starred)),
                _ => None,
            })
            .collect();
        assert!(!visible.is_empty(), "the starred session survives the gate");
        assert!(
            visible
                .iter()
                .all(|(ws_idx, starred)| *ws_idx == 1 && *starred),
            "only the starred session remains, got {visible:?}"
        );

        app.sidebar_starred_only = false;
        let unfiltered = sidebar_rows(&app)
            .into_iter()
            .filter(|row| matches!(row, SidebarRow::Tab { .. } | SidebarRow::Agent { .. }))
            .count();
        assert!(
            unfiltered > visible.len(),
            "clearing the gate brings the other sessions back"
        );
    }

    #[test]
    fn a_starred_session_draws_its_star_directly_after_the_title() {
        let mut entry = compact_test_entry("focus-session", Some(Agent::Claude));
        entry.starred = true;
        let app = crate::app::state::AppState::test_new();
        let area = Rect::new(0, 0, 40, 1);

        let render = |entry: &AgentPanelEntry| {
            let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
            terminal
                .draw(|frame| {
                    render_compact_agent_row_with_prefix(
                        &app, frame, entry, area, 0, true, None, None,
                    )
                })
                .unwrap();
            row_text(terminal.backend().buffer(), 0, area.width)
        };

        let starred = render(&entry);
        let title_end =
            starred.find("focus-session").expect("title is drawn") + "focus-session".len();
        assert_eq!(
            &starred[title_end..title_end + SIDEBAR_STAR_SUFFIX.len()],
            SIDEBAR_STAR_SUFFIX,
            "the star follows the title immediately, got {starred:?}"
        );

        entry.starred = false;
        let plain = render(&entry);
        assert!(
            !plain.contains(SIDEBAR_STAR_SUFFIX.trim()),
            "an unstarred row draws no star, got {plain:?}"
        );
        assert_eq!(
            starred.chars().count(),
            plain.chars().count(),
            "the star is absorbed by the title field, not appended to the row"
        );
    }

    #[test]
    fn a_narrow_row_drops_the_star_rather_than_eating_the_title() {
        let mut entry = compact_test_entry("session", Some(Agent::Claude));
        entry.starred = true;
        let app = crate::app::state::AppState::test_new();
        let area = Rect::new(0, 0, 12, 1);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| {
                render_compact_agent_row_with_prefix(&app, frame, &entry, area, 0, true, None, None)
            })
            .unwrap();
        let text = row_text(terminal.backend().buffer(), 0, area.width);
        assert!(
            !text.contains(SIDEBAR_STAR_SUFFIX.trim()),
            "no star at this width, got {text:?}"
        );
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
        assert_eq!(shape[0], ("section", SPACES_SECTION_TITLE.to_string()));
        assert_eq!(
            shape.iter().filter(|(kind, _)| *kind == "section").count(),
            1
        );
        assert!(!shape
            .iter()
            .any(|(_, title)| title == BLOCKED_SECTION_TITLE));
        assert!(!shape.iter().any(|(_, title)| title == PINNED_SECTION_TITLE));
    }

    #[test]
    fn pinned_tabs_do_not_create_a_sidebar_section() {
        let mut app =
            priority_app_with_states(&[AgentState::Working, AgentState::Blocked, AgentState::Idle]);
        // ws0 is merely pinned; ws1 is pinned *and* blocked.
        app.workspaces[0].tabs[0].pinned = true;
        app.workspaces[1].tabs[0].pinned = true;

        let shape = priority_row_shape(&app);
        assert_eq!(shape[0], ("section", SPACES_SECTION_TITLE.to_string()));
        assert_eq!(
            shape.iter().filter(|(kind, _)| *kind == "section").count(),
            1
        );
        assert!(!shape.iter().any(|(_, title)| title == PINNED_SECTION_TITLE));
    }

    #[test]
    fn pin_changes_do_not_change_sidebar_sections() {
        let mut app = priority_app_with_states(&[AgentState::Working, AgentState::Idle]);
        app.workspaces[0].tabs[0].pinned = true;
        assert_eq!(
            sidebar_rows(&app)
                .iter()
                .filter(|row| matches!(row, SidebarRow::SectionHeader { .. }))
                .count(),
            1,
            "a pin does not open a separate sidebar group"
        );
        app.workspaces[0].tabs[0].pinned = false;
        // Unpinning leaves the single Spaces header unchanged.
        assert_eq!(
            sidebar_rows(&app)
                .iter()
                .filter_map(|row| match row {
                    SidebarRow::SectionHeader { title, .. } => Some(*title),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec![SPACES_SECTION_TITLE],
            "with nothing pinned only the Spaces header remains"
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
        assert_eq!(open[0], ("section", SPACES_SECTION_TITLE.to_string()));

        app.collapsed_sidebar_groups
            .insert("repo:Spaces".to_string());
        let folded = priority_row_shape(&app);
        assert_eq!(folded, vec![("section", SPACES_SECTION_TITLE.to_string())]);
        assert!(folded.len() < open.len());
    }

    #[test]
    fn folding_spaces_hides_the_whole_tree_but_keeps_the_worklist() {
        let mut app = priority_app_with_states(&[AgentState::Blocked, AgentState::Working]);
        app.collapsed_sidebar_groups
            .insert("repo:Spaces".to_string());
        let shape = priority_row_shape(&app);
        assert_eq!(shape[0], ("section", SPACES_SECTION_TITLE.to_string()));
        assert!(!shape.iter().any(|(kind, _)| *kind == "workspace"));
    }

    #[test]
    fn a_spaces_fold_hides_the_tree_even_with_nothing_blocked_or_pinned() {
        // The Spaces header remains the only tree control.
        let mut app = priority_app_with_states(&[AgentState::Working, AgentState::Idle]);
        app.collapsed_sidebar_groups
            .insert("repo:Spaces".to_string());
        let shape = priority_row_shape(&app);
        assert!(!shape.iter().any(|(kind, _)| *kind == "workspace"));
        assert_eq!(shape, vec![("section", SPACES_SECTION_TITLE.to_string())]);
    }

    #[test]
    fn nothing_blocked_leaves_only_the_spaces_header() {
        let app = priority_app_with_states(&[AgentState::Working, AgentState::Idle]);
        let rows = sidebar_rows(&app);
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, SidebarRow::SectionHeader { .. }))
                .count(),
            1,
            "the removed Blocked group does not cost a row"
        );
        assert!(matches!(
            rows[0],
            SidebarRow::SectionHeader { title, .. } if title == SPACES_SECTION_TITLE
        ));
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
        assert!(ordinals.is_empty());

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

    /// Two agent tabs in one workspace plus a second workspace, so the
    /// assertions can tell pane-level highlighting apart from workspace-level
    /// highlighting in the compact tab projection.
    fn collapsed_agent_app() -> (crate::app::state::AppState, PaneId, PaneId) {
        let mut app = crate::app::state::AppState::test_new();
        let mut first = Workspace::test_new("one");
        let second_tab = first.test_add_tab(None);
        let first_pane = first.tabs[0].root_pane;
        let second_pane = first.tabs[second_tab].root_pane;
        first.active_tab = second_tab;
        app.workspaces = vec![first, Workspace::test_new("two")];
        app.ensure_test_terminals();

        let terminal_ids: Vec<_> = app
            .workspaces
            .iter()
            .flat_map(|ws| ws.tabs.iter())
            .flat_map(|tab| tab.panes.values())
            .map(|pane| pane.attached_terminal_id.clone())
            .collect();
        for terminal_id in terminal_ids {
            app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Claude);
        }
        app.reconcile_sidebar_presentation();

        (app, first_pane, second_pane)
    }

    fn collapsed_agent_row_styles(
        app: &crate::app::state::AppState,
        area: Rect,
        rows: u16,
    ) -> Vec<Vec<ratatui::style::Style>> {
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("test terminal should initialize");
        terminal
            .draw(|frame| render_sidebar_collapsed(app, frame, area))
            .expect("collapsed sidebar should render");
        let buffer = terminal.backend().buffer();
        let (ws_area, _, _) = collapsed_sidebar_sections(area);
        let scroll = collapsed_sidebar_row_scroll(app, ws_area);
        sidebar_rows(app)
            .iter()
            .enumerate()
            .skip(scroll)
            .filter(|&(_, row)| matches!(row, SidebarRow::Agent { .. } | SidebarRow::Tab { .. }))
            .map(|(row_idx, _)| {
                let y = ws_area.y + (row_idx - scroll) as u16;
                (ws_area.x..ws_area.x + ws_area.width)
                    .map(|x| buffer[(x, y)].style())
                    .collect::<Vec<_>>()
            })
            .take(rows as usize)
            .collect()
    }

    #[test]
    fn collapsed_sidebar_highlights_only_the_focused_agent_pane() {
        let (mut app, first_pane, second_pane) = collapsed_agent_app();
        app.active = Some(0);
        app.workspaces[0].tabs[1].layout.focus_pane(second_pane);
        assert!(app.is_active_pane(0, 1, second_pane));
        assert!(!app.is_active_pane(0, 0, first_pane));

        let area = Rect::new(0, 0, 4, 14);
        let rows = collapsed_agent_row_styles(&app, area, 3);

        let highlighted: Vec<_> = rows
            .iter()
            .filter(|cells| {
                cells
                    .iter()
                    .all(|style| style.bg == Some(app.palette.active_row_bg))
            })
            .collect();
        assert_eq!(
            highlighted.len(),
            1,
            "only the focused agent pane should be highlighted, across the whole row"
        );
        assert_eq!(highlighted[0][0].fg, Some(app.palette.text));

        let muted = rows
            .iter()
            .filter(|cells| cells[0].fg == Some(app.palette.overlay0))
            .count();
        assert_eq!(
            muted, 2,
            "the sibling pane in the active workspace and the other workspace stay muted"
        );
    }

    #[test]
    fn collapsed_sidebar_does_not_highlight_agents_without_active_workspace() {
        let (mut app, _, _) = collapsed_agent_app();
        app.active = None;

        let area = Rect::new(0, 0, 4, 14);
        let rows = collapsed_agent_row_styles(&app, area, 3);

        for cells in rows {
            assert_eq!(cells[0].fg, Some(app.palette.overlay0));
            for style in cells {
                assert_ne!(style.bg, Some(app.palette.active_row_bg));
            }
        }
    }

    #[test]
    fn collapsed_sidebar_keeps_workspace_status_visible_for_two_digit_positions() {
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
        let (workspace_area, _, _) = collapsed_sidebar_sections(area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("test terminal should initialize");

        terminal
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .expect("collapsed sidebar should render");

        let rows = sidebar_rows(&app);
        let row_y = |ws_idx| {
            rows.iter()
                .enumerate()
                .find_map(|(row_idx, row)| {
                    matches!(row, SidebarRow::Workspace { ws_idx: row_ws_idx, .. } if *row_ws_idx == ws_idx)
                        .then_some(workspace_area.y + row_idx as u16)
                })
                .expect("workspace row should be visible")
        };
        let first_row = row_y(0);
        let tenth_row = row_y(9);
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(workspace_area.x, first_row)].symbol(), "1");
        assert_eq!(buffer[(workspace_area.x + 1, first_row)].symbol(), " ");
        assert_eq!(buffer[(workspace_area.x + 2, first_row)].symbol(), "·");
        assert_eq!(buffer[(workspace_area.x, tenth_row)].symbol(), "1");
        assert_eq!(buffer[(workspace_area.x + 1, tenth_row)].symbol(), "0");
        assert_eq!(buffer[(workspace_area.x + 2, tenth_row)].symbol(), "·");
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
        app.status_indicators = crate::config::StatusIndicatorStyle::Symbols;

        let set_state = |app: &mut crate::app::state::AppState, ws_idx: usize, pane_id, state| {
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let terminal = app.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(Agent::Claude);
            terminal.state = state;
        };
        set_state(&mut app, 0, first_pane, AgentState::Idle);
        set_state(&mut app, 1, second_pane, AgentState::Working);
        set_state(&mut app, 1, urgent_pane, AgentState::Blocked);
        app.workspaces[0].tabs[0]
            .panes
            .get_mut(&first_pane)
            .unwrap()
            .seen = false;

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

    fn work_ticket(
        identifier: &str,
        title: &str,
        assignee: &str,
        labels: &[&str],
    ) -> crate::work_index::WorkTicket {
        crate::work_index::WorkTicket {
            identifier: identifier.into(),
            title: Some(title.into()),
            description: None,
            state: Some("In Progress".into()),
            assignee: Some(assignee.into()),
            creator: None,
            priority: None,
            cycle: None,
            group: crate::work_index::TicketGroup::Assigned,
            created_at: None,
            updated_at: None,
            branch: None,
            labels: labels.iter().map(|label| (*label).to_string()).collect(),
            url: None,
            parent: None,
            relations: Vec::new(),
        }
    }

    fn work_item(
        repo: &str,
        pr_number: Option<u64>,
        tickets: Vec<crate::work_index::WorkTicket>,
    ) -> crate::work_index::WorkItem {
        crate::work_index::WorkItem {
            repo: repo.into(),
            pr_number,
            pr_url: pr_number.map(|number| format!("https://github.com/{repo}/pull/{number}")),
            pr_title: None,
            pr_state: pr_number.map(|_| "open".into()),
            draft: false,
            review_decision: None,
            created_at: None,
            updated_at: None,
            additions: 0,
            deletions: 0,
            author: None,
            assignees: Vec::new(),
            labels: Vec::new(),
            check_state: crate::work_index::PrCheckState::Unknown,
            audience: crate::work_index::PrAudience::Unclassified,
            cached_pr_detail: None,
            ticket_ids: tickets
                .iter()
                .map(|ticket| ticket.identifier.clone())
                .collect(),
            ticket_title: None,
            ticket_state: None,
            ticket_details: tickets,
            branch: None,
            preview_urls: Vec::new(),
            panes: Vec::new(),
            source: crate::work_index::WorkItemSource::default(),
        }
    }

    const CONVERSATION_A: &str = "https://mail.missiveapp.com/#inbox/conversations/aaa111";
    const CONVERSATION_B: &str = "https://mail.missiveapp.com/#inbox/conversations/bbb222";
    const CONVERSATION_C: &str = "https://mail.missiveapp.com/#inbox/conversations/ccc333";

    fn missive_conversation(
        id: &str,
        subject: &str,
        url: &str,
    ) -> crate::work_index::MissiveConversation {
        crate::work_index::MissiveConversation {
            id: id.into(),
            subject: subject.into(),
            app_url: url.into(),
            web_url: url.into(),
            team: None,
            assignees: Vec::new(),
            last_activity_at: None,
            closed: false,
            labels: Vec::new(),
            pane_bound: false,
            messages: Vec::new(),
            notes: Vec::new(),
            drafts: Vec::new(),
            posts: Vec::new(),
        }
    }

    /// Three panes: one on a ticket and two conversations, one on two tickets,
    /// one on nothing. The snapshot knows a fourth ticket nobody works on.
    pub(crate) fn sidebar_work_item_fixture() -> AppState {
        let mut workspace = Workspace::test_new("repo");
        workspace.test_add_tab(Some("addendum"));
        workspace.test_add_tab(Some("shell"));
        let mut app = AppState::test_new();
        // Most 2b characterization tests predate F12's narrowed defaults and
        // exercise the complete fixture unless they opt into a filter.
        app.sidebar_work_filter.team = None;
        app.sidebar_work_filter.assignee = None;
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        for (tab_idx, context) in [
            crate::work_context::PaneWorkContext {
                ticket_ids: vec!["SCA-3102".into()],
                missive_urls: vec![CONVERSATION_A.into(), CONVERSATION_B.into()],
                repo: Some("scalable-so/herdr".into()),
                work_title: Some("fix pricing".into()),
                ..Default::default()
            },
            crate::work_context::PaneWorkContext {
                ticket_ids: vec!["SCA-3165".into(), "SCA-3170".into()],
                ..Default::default()
            },
            crate::work_context::PaneWorkContext::default(),
        ]
        .into_iter()
        .enumerate()
        {
            let pane_id = app.workspaces[0].tabs[tab_idx].root_pane;
            let terminal_id = app.workspaces[0].tabs[tab_idx].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let terminal = app
                .terminals
                .get_mut(&terminal_id)
                .expect("fixture terminal");
            terminal.replace_prevalidated_manual_work_context(context);
            if tab_idx == 0 {
                terminal.cwd = std::path::PathBuf::from("/tmp/herdr-fixture/herdr");
            }
        }
        app.work_index_enabled = true;
        app.work_index_snapshot = Some(crate::work_index::Snapshot {
            items: vec![
                work_item(
                    "scalable-so/herdr",
                    Some(159),
                    vec![work_ticket(
                        "SCA-3102",
                        "annual credits",
                        "matthias",
                        &["P1"],
                    )],
                ),
                work_item(
                    "scalable-so/herdr",
                    None,
                    vec![
                        work_ticket("SCA-3165", "image-edit v3", "matthias", &["P2"]),
                        work_ticket("SCA-3170", "ads skill map", "jacob", &["P3"]),
                    ],
                ),
                work_item(
                    "scalable-so/110x",
                    None,
                    vec![work_ticket("OPS-12", "pixel EMQ drop", "jacob", &[])],
                ),
            ],
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: std::time::SystemTime::UNIX_EPOCH,
        });
        app.reconcile_sidebar_presentation();
        app
    }

    fn sidebar_order_fixture() -> AppState {
        let mut app = AppState::test_new();
        app.workspaces = ["alpha", "beta"]
            .into_iter()
            .map(|name| {
                let mut workspace = Workspace::test_new(name);
                workspace.test_add_tab(Some("two"));
                workspace.test_add_tab(Some("three"));
                workspace
            })
            .collect();
        app.ensure_test_terminals();
        for ws_idx in 0..app.workspaces.len() {
            for tab_idx in 0..app.workspaces[ws_idx].tabs.len() {
                let pane_id = app.workspaces[ws_idx].tabs[tab_idx].root_pane;
                let terminal_id = app.workspaces[ws_idx].tabs[tab_idx].panes[&pane_id]
                    .attached_terminal_id
                    .clone();
                app.terminals
                    .get_mut(&terminal_id)
                    .expect("fixture terminal")
                    .replace_prevalidated_manual_work_context(
                        crate::work_context::PaneWorkContext {
                            missive_urls: vec![format!(
                                "https://mail.missiveapp.com/#inbox/conversations/{ws_idx}{tab_idx}"
                            )],
                            ..Default::default()
                        },
                    );
            }
        }
        app.reconcile_sidebar_presentation();
        app
    }

    #[test]
    fn missive_groups_preserve_workspace_tab_order() {
        let mut app = sidebar_order_fixture();
        app.sidebar_group_mode = SidebarGroupMode::Missive;
        let expected = vec![(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2)];

        for _ in 0..64 {
            let actual = sidebar_rows(&app)
                .into_iter()
                .filter_map(|row| match row {
                    SidebarRow::Tab { entry, .. } => Some((entry.ws_idx, entry.tab_idx)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(actual, expected);
        }
    }

    /// `(title, pane count, dim)` for every work-item header, with the pane
    /// rows that sit under it.
    fn work_group_shape(app: &AppState) -> Vec<(String, usize, bool)> {
        sidebar_rows(app)
            .into_iter()
            .filter_map(|row| match row {
                SidebarRow::NestedHeader {
                    key,
                    title,
                    count,
                    dim,
                    ..
                } if !key.starts_with("unassigned-empty:") => Some((title, count, dim)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn linear_mode_joins_panes_with_the_projection_ticket_rows() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        assert_eq!(
            work_group_shape(&app),
            vec![
                ("SCA-3102 · annual credits".to_string(), 1, false),
                ("SCA-3165 · image-edit v3".to_string(), 1, false),
                // The two-ticket pane is listed under both of its tickets.
                ("SCA-3170 · ads skill map".to_string(), 1, false),
                ("unlinked".to_string(), 1, false),
                ("OPS-12 · pixel EMQ drop".to_string(), 0, true),
            ]
        );
    }

    #[test]
    fn linear_ticketless_panes_share_one_unlinked_bucket() {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("alpha"), Workspace::test_new("beta")];
        app.ensure_test_terminals();
        for (workspace, cwd) in app.workspaces.iter().zip(["/work/alpha", "/work/beta"]) {
            let pane_id = workspace.tabs[0].root_pane;
            let terminal_id = workspace.tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&terminal_id).unwrap().cwd = cwd.into();
        }

        let groups = sidebar_work_groups(
            &app,
            &sidebar_thread_entries(&app),
            SidebarGroupMode::LinearTeam,
        );
        assert_eq!(
            groups
                .iter()
                .map(|group| (
                    group.key.as_str(),
                    group.title.as_str(),
                    group.entries.len()
                ))
                .collect::<Vec<_>>(),
            [("unlinked", "unlinked", 2)]
        );
    }

    #[test]
    fn missive_mode_groups_panes_by_conversation() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::Missive;
        assert_eq!(
            work_group_shape(&app),
            vec![
                // The pane replies in both conversations, so it is listed under
                // both; its declared work title is each header's subject, and
                // the conversation id keeps the two headers apart.
                ("aaa111 · fix pricing".to_string(), 1, false),
                ("bbb222 · fix pricing".to_string(), 1, false),
                (unlinked_bucket_title(), 2, false),
            ]
        );
    }

    #[test]
    fn missive_headers_use_the_subject_the_context_dock_has() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::Missive;
        // A pane bound to one conversation and nothing else: its declared work
        // title is the subject the context dock shows for that conversation.
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .expect("fixture terminal")
            .replace_prevalidated_manual_work_context(crate::work_context::PaneWorkContext {
                missive_urls: vec![CONVERSATION_A.into()],
                work_title: Some("refund for invoice 42".into()),
                ..Default::default()
            });
        assert_eq!(
            work_group_shape(&app),
            vec![
                ("aaa111 · refund for invoice 42".to_string(), 1, false),
                (unlinked_bucket_title(), 2, false),
            ]
        );
    }

    #[test]
    fn multi_ticket_pane_appears_under_every_ticket() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        let panes_under = |app: &AppState, title: &str| {
            work_group_shape(app)
                .into_iter()
                .find(|(header, _, _)| header.starts_with(title))
                .map(|(_, count, _)| count)
        };
        // The fixture pane declares SCA-3165 and SCA-3170.
        assert_eq!(panes_under(&app, "SCA-3165"), Some(1));
        assert_eq!(panes_under(&app, "SCA-3170"), Some(1));
    }

    #[test]
    fn multi_link_pane_appears_under_every_missive_conversation() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::Missive;
        let groups =
            sidebar_work_groups(&app, &agent_panel_entries(&app), SidebarGroupMode::Missive);
        let entries_for = |key: &str| {
            groups
                .iter()
                .find(|group| group.key == key)
                .map(|group| group.entries.len())
        };
        assert_eq!(entries_for(&format!("missive:{CONVERSATION_A}")), Some(1));
        assert_eq!(entries_for(&format!("missive:{CONVERSATION_B}")), Some(1));
    }

    #[test]
    fn pane_url_absent_from_index_groups_and_resolves_label_then_title_then_tail() {
        let app = sidebar_work_item_fixture();
        let groups =
            sidebar_work_groups(&app, &agent_panel_entries(&app), SidebarGroupMode::Missive);
        let pane_only = groups
            .iter()
            .find(|group| group.key == format!("missive:{CONVERSATION_A}"))
            .expect("pane-only conversation group");
        assert_eq!(pane_only.title, "aaa111 · fix pricing");
        assert_eq!(pane_only.entries.len(), 1);

        // A cached link label wins over the pane's declared work title.
        assert_eq!(
            missive_subject_from(
                Some("   "),
                Some("refund for invoice 42"),
                Some("fix pricing"),
                CONVERSATION_A
            ),
            "refund for invoice 42"
        );
        // A label that only restates the URL is no subject at all.
        assert_eq!(
            missive_subject_from(
                None,
                Some("missive/aaa111"),
                Some("fix pricing"),
                CONVERSATION_A
            ),
            "fix pricing"
        );
        assert_eq!(
            missive_subject_from(None, None, None, CONVERSATION_A),
            "aaa111"
        );

        // The dock has no indexed subject yet, so real state follows the same
        // pane-title fallback.
        assert_eq!(missive_subject(&app, CONVERSATION_A), "fix pricing");
        assert_eq!(missive_subject(&app, CONVERSATION_C), "ccc333");
    }

    #[test]
    fn indexed_missive_conversation_without_pane_is_dim_and_enter_opens_home() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::Missive;
        app.sidebar_work_filter.missive.assignee = None;
        app.work_index_snapshot
            .as_mut()
            .expect("work index snapshot")
            .conversations = vec![missive_conversation(
            "ccc333",
            "Customer cannot update card",
            CONVERSATION_C,
        )];

        let header = sidebar_rows(&app)
            .into_iter()
            .find_map(|row| match row {
                SidebarRow::NestedHeader {
                    key,
                    title,
                    count,
                    dim,
                    ..
                } if key == format!("missive:{CONVERSATION_C}") => Some((title, count, dim)),
                _ => None,
            })
            .expect("indexed no-pane conversation header");
        assert_eq!(
            header,
            ("ccc333 · Customer cannot update card".into(), 0, true)
        );

        app.sidebar_selected_work_group = Some(format!("missive:{CONVERSATION_C}"));
        assert!(matches!(
            app.handle_sidebar_work_group_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty())),
            crate::app::SidebarWorkGroupKeyAction::Consumed
        ));
        let home = app
            .home
            .as_ref()
            .expect("conversation-linked Home composer");
        assert_eq!(
            home.missive
                .as_ref()
                .map(|conversation| conversation.web_url.as_str()),
            Some(CONVERSATION_C)
        );
        assert!(app.dock_object_preview.is_none());
    }

    #[test]
    fn indexed_missive_subject_overrides_pane_fallbacks() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_work_filter.missive.assignee = None;
        app.work_index_snapshot
            .as_mut()
            .expect("work index snapshot")
            .conversations = vec![missive_conversation(
            "aaa111",
            "Indexed refund subject",
            CONVERSATION_A,
        )];

        let group =
            sidebar_work_groups(&app, &agent_panel_entries(&app), SidebarGroupMode::Missive)
                .into_iter()
                .find(|group| group.key == format!("missive:{CONVERSATION_A}"))
                .expect("indexed conversation with pane");
        assert_eq!(group.title, "aaa111 · Indexed refund subject");
        assert_eq!(group.entries.len(), 1);

        assert_eq!(
            missive_subject_from(
                Some("Indexed refund subject"),
                Some("cached link label"),
                Some("declared pane title"),
                CONVERSATION_A,
            ),
            "Indexed refund subject"
        );
    }

    #[test]
    fn dim_work_item_rows_carry_no_state_colour() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        let dim_rows = sidebar_rows(&app)
            .into_iter()
            .filter(|row| matches!(row, SidebarRow::NestedHeader { dim: true, .. }))
            .count();
        assert_eq!(dim_rows, 1);

        let mut terminal =
            Terminal::new(TestBackend::new(106, 40)).expect("test terminal for dim rows");
        let area = Rect::new(0, 0, 106, 40);
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
            .expect("render sidebar");
        let buffer = terminal.backend().buffer();
        let headers = compute_sidebar_nested_header_areas(&app, app.view.sidebar_rect);
        let dim = headers
            .iter()
            .find(|header| header.dim)
            .expect("a dim header on screen");
        let line = row_text(buffer, dim.rect.y, dim.rect.width.saturating_sub(1));
        // The ticket's own state is not a live agent state: the header shows
        // the Linear glyph, and the text stays in the dim chrome tone.
        assert!(line.contains("◐ OPS-12 · pixel"), "{line:?}");
        let glyph = buffer[(dim.rect.x + 3, dim.rect.y)].style();
        assert_eq!(glyph.fg, Some(app.palette.work_status_active()));
        let styled = buffer[(dim.rect.x + 5, dim.rect.y)].style();
        assert_eq!(styled.fg, Some(app.palette.overlay0));
        for state in [AgentState::Working, AgentState::Blocked, AgentState::Idle] {
            assert_ne!(
                styled.fg,
                Some(state_label_color(state, true, &app.palette)),
                "dim rows must not take a live state colour"
            );
            assert_ne!(
                glyph.fg,
                Some(state_label_color(state, true, &app.palette)),
                "dim rows must not take a live state colour"
            );
        }
    }

    #[test]
    fn the_work_filter_narrows_unassigned_tickets_but_keeps_pane_links() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        app.sidebar_work_filter = crate::app::state::SidebarWorkFilter {
            team: Some("SCA".into()),
            assignee: None,
            ..Default::default()
        };
        assert_eq!(
            work_group_shape(&app)
                .into_iter()
                .map(|(title, ..)| title)
                .collect::<Vec<_>>(),
            vec![
                "SCA-3102 · annual credits",
                "SCA-3165 · image-edit v3",
                "SCA-3170 · ads skill map",
                "unlinked",
            ]
        );

        app.sidebar_work_filter.assignee = Some("jacob".into());
        // Every SCA ticket is pane-linked in this fixture, so the assignee
        // filter may only keep narrowing the unassigned section.
        assert_eq!(
            work_group_shape(&app),
            vec![
                ("SCA-3102 · annual credits".to_string(), 1, false),
                ("SCA-3165 · image-edit v3".to_string(), 1, false),
                ("SCA-3170 · ads skill map".to_string(), 1, false),
                ("unlinked".to_string(), 1, false),
            ]
        );
    }

    #[test]
    fn pane_bound_ticket_survives_filters_while_unassigned_items_do_not() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        app.sidebar_work_filter.team = Some("SCA".into());
        app.sidebar_work_filter.assignee = Some("nobody".into());
        let snapshot = app.work_index_snapshot.as_mut().expect("snapshot");
        snapshot
            .items
            .iter_mut()
            .find(|item| item.ticket_ids == ["OPS-12"])
            .expect("stale pane-bound ticket item")
            .source
            .pane = true;

        assert!(work_group_shape(&app)
            .iter()
            .any(|(title, count, dim)| title.starts_with("SCA-3102") && *count == 1 && !dim));
        assert!(sidebar_unassigned_objects(
            &app,
            &sidebar_thread_entries(&app),
            SidebarGroupMode::LinearTeam,
        )
        .is_empty());
    }

    #[test]
    fn pane_bound_pull_request_survives_filters_while_unassigned_items_do_not() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::RepoPr;
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app
            .terminals
            .get_mut(&terminal_id)
            .expect("fixture terminal");
        let mut context = terminal.effective_work_context().clone();
        context.pr_urls = vec!["https://github.com/scalable-so/herdr/pull/159".into()];
        terminal.replace_prevalidated_manual_work_context(context);
        let item = app
            .work_index_snapshot
            .as_mut()
            .expect("snapshot")
            .items
            .iter_mut()
            .find(|item| item.pr_number == Some(159))
            .expect("pane pull request");
        item.source.github = true;
        item.source.pane = true;
        item.pr_state = Some("merged".into());

        assert!(work_group_shape(&app)
            .iter()
            .any(|(title, count, dim)| title.starts_with("#159") && *count == 1 && !dim));
        assert!(sidebar_unassigned_objects(
            &app,
            &sidebar_thread_entries(&app),
            SidebarGroupMode::RepoPr,
        )
        .is_empty());
    }

    #[test]
    fn pane_bound_conversation_survives_filters_while_unassigned_items_do_not() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::Missive;
        let mut linked = missive_conversation("aaa111", "Linked", CONVERSATION_A);
        linked.closed = true;
        linked.pane_bound = true;
        let mut stale = missive_conversation("ccc333", "Stale", CONVERSATION_C);
        stale.closed = true;
        stale.pane_bound = true;
        app.work_index_snapshot
            .as_mut()
            .expect("snapshot")
            .conversations = vec![linked, stale];

        assert!(work_group_shape(&app)
            .iter()
            .any(|(title, count, dim)| title.starts_with("aaa111") && *count == 1 && !dim));
        assert!(sidebar_unassigned_objects(
            &app,
            &sidebar_thread_entries(&app),
            SidebarGroupMode::Missive,
        )
        .is_empty());
    }

    #[test]
    fn f27_no_agent_yet_titles_each_provider_view_and_keeps_filter_text() {
        let mut app = sidebar_work_item_fixture();
        app.work_index_session.github.viewer = Some("matthias-scale".into());
        for mode in [
            SidebarGroupMode::LinearTeam,
            SidebarGroupMode::RepoPr,
            SidebarGroupMode::Missive,
        ] {
            app.sidebar_group_mode = mode;
            assert!(
                sidebar_rows(&app).iter().any(|row| matches!(
                    row,
                    SidebarRow::SectionHeader { title, .. }
                        if *title == NO_AGENT_YET_SECTION_TITLE
                )),
                "missing provider title for {mode:?}"
            );
        }
        app.sidebar_group_mode = SidebarGroupMode::RepoPr;
        assert!(sidebar_rows(&app).iter().any(|row| matches!(
            row,
            SidebarRow::NestedHeader { key, title, .. }
                if key.starts_with("unassigned-empty:")
                    && title == "no open PRs for me · author or assignee"
        )));
    }

    #[test]
    fn f20_unassigned_empty_section_uses_short_degradation_text() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        let snapshot = app.work_index_snapshot.as_mut().expect("snapshot");
        snapshot.items.clear();
        snapshot.unavailable = Some(crate::work_index::WorkIndexUnavailable::only(
            crate::work_index::WorkIndexSource::Linear,
            "rate limited · retry in 12m",
        ));

        assert!(sidebar_rows(&app).iter().any(|row| matches!(
            row,
            SidebarRow::NestedHeader { key, title, .. }
                if key.starts_with("unassigned-empty:")
                    && title == "Linear: rate limited · retry in 12m"
        )));
    }

    #[test]
    fn the_linear_filter_dropdown_lists_defaults_users_and_statuses() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        app.work_index_session.linear.assignees = vec!["Ada".into(), "Matthias".into()];
        let labels = sidebar_filter_options(&app)
            .into_iter()
            .map(|option| option.label())
            .collect::<Vec<_>>();
        assert_eq!(
            &labels[..7],
            [
                "team: all",
                "team: OPS",
                "team: SCA",
                "assignee: me",
                "me: assigned",
                "me: authored",
                "me: both",
            ]
        );
        assert!(labels.contains(&"assignee: all".into()));
        assert!(labels.contains(&"assignee: Ada".into()));
        assert!(labels.contains(&"assignee: Matthias".into()));
        assert!(labels.contains(&"[x] status: In Progress".into()));
        assert!(labels.contains(&"[ ] status: Canceled".into()));
        assert!(labels.contains(&"[ ] status: Duplicate".into()));
    }

    #[test]
    fn github_and_missive_filter_dropdowns_list_their_controls() {
        let mut app = sidebar_work_item_fixture();
        app.work_index_session.github.assignees = vec!["Ada".into()];
        app.sidebar_group_mode = SidebarGroupMode::RepoPr;
        assert_eq!(
            sidebar_filter_options(&app)
                .into_iter()
                .map(|option| option.label())
                .collect::<Vec<_>>(),
            [
                "assignee: me",
                "me: assigned",
                "me: authored",
                "me: both",
                "assignee: all",
                "assignee: Ada",
                "[ ] show drafts",
                "state: open",
                "state: merged",
                "state: closed",
            ]
        );

        app.sidebar_group_mode = SidebarGroupMode::Missive;
        app.work_index_session.missive.viewer = Some("Mina".into());
        app.work_index_session.missive.assignees = vec!["Ada".into(), "Mina".into()];
        assert_eq!(
            sidebar_filter_options(&app)
                .into_iter()
                .map(|option| option.label())
                .collect::<Vec<_>>(),
            [
                "team: all",
                "assignee: me",
                "assignee: all",
                "assignee: Ada",
                "[ ] show closed",
            ]
        );
    }

    fn open_object_menu(app: &mut AppState, target: &str) {
        app.sidebar_object_menu = Some(crate::app::state::SidebarObjectMenuState {
            target: target.into(),
            anchor_row: None,
            page: crate::app::state::SidebarObjectMenuPage::Actions,
            selected: 0,
        });
    }

    #[test]
    fn sidebar_object_menu_contents_match_each_provider() {
        let mut app = sidebar_work_item_fixture();
        for (target, expected) in [
            (
                "github:https://github.com/scalable-so/herdr/pull/159",
                vec![
                    "Refresh",
                    "Ask a question",
                    "Explain this PR",
                    "Fix findings in a thread",
                    "Convert to draft",
                    "Enable auto-merge",
                    "Merge",
                    "Squash",
                    "Rebase",
                    "Open on GitHub",
                    "Copy link",
                    "Close pull request",
                ],
            ),
            (
                "linear:SCA-3102",
                vec![
                    "Refresh",
                    "Ask a question",
                    "Explain this ticket",
                    "Work on it in a thread",
                    "Transition ▸",
                    "Assign to me · viewer unavailable",
                    "Priority ▸",
                    "Link PR · no PR in pane",
                    "Comment",
                    "Open in Linear",
                    "Copy link",
                    "Copy identifier",
                    "Cancel ticket",
                ],
            ),
            (
                &format!("missive:{CONVERSATION_A}"),
                vec!["Open in Missive", "Start thread"],
            ),
        ] {
            open_object_menu(&mut app, target);
            assert_eq!(
                sidebar_object_menu_labels(&app),
                expected,
                "menu for {target}"
            );
        }

        open_object_menu(&mut app, "linear:SCA-3102");
        app.sidebar_object_menu.as_mut().expect("ticket menu").page =
            crate::app::state::SidebarObjectMenuPage::TicketTransitions;
        assert_eq!(
            sidebar_object_menu_labels(&app),
            ["Todo", "In Progress · current state", "In Review", "Done"]
        );
    }

    #[test]
    fn sidebar_object_menu_opens_downward_and_clamps() {
        let mut app = sidebar_work_item_fixture();
        app.work_index_snapshot
            .as_mut()
            .and_then(|snapshot| snapshot.items.first_mut())
            .expect("pull request fixture")
            .source
            .github = true;
        app.sidebar_group_mode = SidebarGroupMode::RepoPr;
        app.sidebar_work_filter.github.assignee = None;
        crate::ui::compute_view(&mut app, Rect::new(0, 0, 80, 24));
        let target = "github:https://github.com/scalable-so/herdr/pull/159";
        open_object_menu(&mut app, target);
        let anchor = sidebar_object_menu_anchor_rect(&app).expect("action anchor");
        let area = Rect::new(0, 0, 80, anchor.bottom().saturating_add(2));
        let layout = sidebar_object_menu_layout(&app, area).expect("clamped dropdown");
        assert_eq!(layout.rect.y, anchor.bottom());
        assert_eq!(layout.visible_rows, 2);
        assert!(layout.rect.y >= anchor.bottom());
        assert!(layout.rect.bottom() <= area.bottom());
    }

    #[test]
    fn sidebar_ticket_action_menu_opens_downward_and_clamps() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        crate::ui::compute_view(&mut app, Rect::new(0, 0, 80, 24));
        open_object_menu(&mut app, "linear:SCA-3102");
        let anchor = sidebar_object_menu_anchor_rect(&app).expect("ticket action anchor");
        let area = Rect::new(0, 0, 80, anchor.bottom().saturating_add(3));
        let layout = sidebar_object_menu_layout(&app, area).expect("clamped ticket dropdown");
        assert_eq!(layout.rect.y, anchor.bottom());
        assert_eq!(layout.visible_rows, 3);
        assert!(layout.rect.bottom() <= area.bottom());
    }

    #[test]
    fn unassigned_row_keeps_separate_spawn_and_action_hit_areas() {
        let mut app = sidebar_work_item_fixture();
        app.work_index_snapshot
            .as_mut()
            .and_then(|snapshot| snapshot.items.first_mut())
            .expect("pull request fixture")
            .source
            .github = true;
        app.sidebar_group_mode = SidebarGroupMode::RepoPr;
        app.sidebar_work_filter.github.assignee = None;
        crate::ui::compute_view(&mut app, Rect::new(0, 0, 80, 24));
        let header = compute_sidebar_nested_header_areas(&app, app.view.sidebar_rect)
            .into_iter()
            .find(|header| header.spawn)
            .expect("unassigned pull request row");
        let target = header.action_key.clone().expect("action target");
        assert_eq!(
            sidebar_unassigned_spawn_at(&app, header.rect.right().saturating_sub(3), header.rect.y),
            Some(target.clone())
        );
        assert_eq!(
            sidebar_object_action_at(&app, header.rect.right().saturating_sub(1), header.rect.y),
            Some(target)
        );
        assert_eq!(
            sidebar_unassigned_spawn_at(&app, header.rect.right().saturating_sub(1), header.rect.y),
            None
        );
    }

    #[test]
    fn the_linear_header_names_the_team_and_the_filter() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        app.sidebar_work_filter = crate::app::state::SidebarWorkFilter::default();
        assert_eq!(
            sidebar_header_mode_label(&app),
            "View: Linear ▾ · SCA · me · active ▾"
        );
        app.sidebar_work_filter = crate::app::state::SidebarWorkFilter {
            team: Some("SCA".into()),
            assignee: Some("matthias".into()),
            ..Default::default()
        };
        assert_eq!(
            sidebar_header_mode_label(&app),
            "View: Linear ▾ · SCA · matthias · active ▾"
        );
    }

    #[test]
    fn the_view_picker_lists_the_f12_labels_and_spaces() {
        let labels = SidebarGroupMode::VIEWS.map(|mode| format!("View: {}", mode.view_label()));
        assert_eq!(
            labels,
            [
                "View: Repo",
                "View: Spaces",
                "View: Linear",
                "View: GitHub",
                "View: Missive"
            ]
        );
        assert_eq!(SidebarGroupMode::default(), SidebarGroupMode::Repo);
    }

    #[test]
    fn f20_2_view_picker_only_focuses_an_existing_object_tab() {
        let mut app = AppState::test_new();
        app.dock_collapsed = true;

        app.set_sidebar_group_mode(SidebarGroupMode::LinearTeam);
        assert!(app.dock_collapsed);
        assert_eq!(app.dock_tab, None);

        app.open_dock_surface(crate::app::DockSurface::Files);
        assert_eq!(app.dock_tab, Some(crate::app::DockSurface::Files));

        app.set_sidebar_group_mode(SidebarGroupMode::RepoPr);
        assert_eq!(app.dock_tab, Some(crate::app::DockSurface::Files));
    }

    #[test]
    fn per_view_filter_defaults_match_f20() {
        let filters = crate::app::state::SidebarWorkFilter::default();
        assert_eq!(filters.team.as_deref(), Some("SCA"));
        assert_eq!(filters.assignee.as_deref(), Some("me"));
        assert_eq!(
            filters.linear_ownership,
            crate::app::state::WorkOwnershipFilter::Both
        );
        assert_eq!(filters.linear_statuses.len(), 8);
        assert!(!filters
            .linear_statuses
            .contains(&crate::app::state::LinearStatusFilter::Canceled));
        assert!(!filters
            .linear_statuses
            .contains(&crate::app::state::LinearStatusFilter::Duplicate));
        assert_eq!(filters.github.assignee.as_deref(), Some("me"));
        assert_eq!(
            filters.github.ownership,
            crate::app::state::WorkOwnershipFilter::Both
        );
        assert!(!filters.github.show_drafts);
        assert_eq!(
            filters.github.state,
            crate::app::state::GithubStateFilter::Open
        );
        assert_eq!(filters.missive.assignee.as_deref(), Some("me"));
        assert_eq!(filters.missive.team, None);
        assert!(!filters.missive.show_closed);
    }

    #[test]
    fn missive_team_filter_uses_observed_and_selected_teams_and_persists() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::Missive;
        let mut support = missive_conversation("support", "Support request", CONVERSATION_A);
        support.team = Some(crate::work_index::MissiveTeam {
            id: "team-support".into(),
            name: "Support".into(),
            organization: Some("organization-example".into()),
        });
        let mut billing = missive_conversation("billing", "Billing request", CONVERSATION_B);
        billing.team = Some(crate::work_index::MissiveTeam {
            id: "team-billing".into(),
            name: "Billing".into(),
            organization: Some("organization-example".into()),
        });
        app.work_index_snapshot
            .as_mut()
            .expect("work index fixture")
            .conversations = vec![support.clone(), billing.clone()];
        app.sidebar_work_filter.missive.team = Some("Escalations".into());
        app.sidebar_work_filter.missive.assignee = None;

        let options = sidebar_filter_options(&app);
        assert_eq!(
            options
                .iter()
                .filter_map(|option| match option {
                    SidebarFilterOption::MissiveTeam(team) => Some(team.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec![
                None,
                Some("Billing".into()),
                Some("Escalations".into()),
                Some("Support".into()),
            ]
        );

        let support_option = options
            .iter()
            .position(|option| *option == SidebarFilterOption::MissiveTeam(Some("Support".into())))
            .expect("Support team option");
        app.select_sidebar_filter_option(support_option);
        assert!(app
            .sidebar_work_filter
            .matches_missive_conversation(Some(&support), &app.work_index_session));
        assert!(!app
            .sidebar_work_filter
            .matches_missive_conversation(Some(&billing), &app.work_index_session));
        assert_eq!(
            app.sidebar_work_filter.missive_label(),
            "Support · all · closed hidden"
        );
        assert_eq!(
            app.take_sidebar_work_filter_persistence_request()
                .and_then(|filter| filter.missive.team),
            Some("Support".into())
        );
    }

    #[test]
    fn injected_me_identity_filters_unlinked_objects_but_keeps_pane_links() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        app.sidebar_work_filter = crate::app::state::SidebarWorkFilter::default();
        app.work_index_session.linear.viewer = Some("matthias".into());
        let linear_titles = work_group_shape(&app)
            .into_iter()
            .map(|(title, ..)| title)
            .collect::<Vec<_>>();
        assert_eq!(
            linear_titles,
            [
                "SCA-3102 · annual credits",
                "SCA-3165 · image-edit v3",
                "SCA-3170 · ads skill map",
                "unlinked",
            ]
        );

        let mut github = work_item("scalable-so/herdr", Some(159), Vec::new());
        github.source.github = true;
        github.assignees = vec!["matthias-scale".into()];
        app.work_index_session.github.viewer = Some("matthias-scale".into());
        assert!(app
            .sidebar_work_filter
            .matches_github(&github, &app.work_index_session));
        github.assignees.clear();
        github.author = Some("matthias-scale".into());
        assert!(app
            .sidebar_work_filter
            .matches_github(&github, &app.work_index_session));
        app.sidebar_work_filter.github.ownership = crate::app::state::WorkOwnershipFilter::Assigned;
        assert!(!app
            .sidebar_work_filter
            .matches_github(&github, &app.work_index_session));
        app.sidebar_work_filter.github.ownership = crate::app::state::WorkOwnershipFilter::Authored;
        assert!(app
            .sidebar_work_filter
            .matches_github(&github, &app.work_index_session));
        app.sidebar_work_filter.github.ownership = crate::app::state::WorkOwnershipFilter::Both;
        github.draft = true;
        assert!(!app
            .sidebar_work_filter
            .matches_github(&github, &app.work_index_session));
        github.draft = false;
        github.pr_state = Some("merged".into());
        assert!(!app
            .sidebar_work_filter
            .matches_github(&github, &app.work_index_session));
    }

    #[test]
    fn absent_missive_filter_fields_show_every_conversation() {
        let app = AppState::test_new();
        assert!(app
            .sidebar_work_filter
            .matches_missive_conversation(None, &app.work_index_session));
    }

    #[test]
    fn choosing_a_filter_option_narrows_and_asks_to_be_persisted() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        let options = sidebar_filter_options(&app);
        let team = options
            .iter()
            .position(|option| *option == SidebarFilterOption::LinearTeam(Some("SCA".into())))
            .expect("SCA in the options");
        app.select_sidebar_filter_option(team);
        assert_eq!(app.sidebar_work_filter.team.as_deref(), Some("SCA"));

        let assignee = options
            .iter()
            .position(|option| *option == SidebarFilterOption::LinearAssignee(Some("me".into())))
            .expect("me in the options");
        app.select_sidebar_filter_option(assignee);
        // The two narrowings compose instead of resetting each other.
        assert_eq!(app.sidebar_work_filter.team.as_deref(), Some("SCA"));
        assert_eq!(app.sidebar_work_filter.assignee.as_deref(), Some("me"));
        assert_eq!(
            app.take_sidebar_work_filter_persistence_request(),
            Some(crate::app::state::SidebarWorkFilter {
                team: Some("SCA".into()),
                assignee: Some("me".into()),
                ..Default::default()
            })
        );

        let all_teams = options
            .iter()
            .position(|option| *option == SidebarFilterOption::LinearTeam(None))
            .expect("all teams in the options");
        app.select_sidebar_filter_option(all_teams);
        assert_eq!(app.sidebar_work_filter.team, None);
        assert_eq!(app.sidebar_work_filter.assignee.as_deref(), Some("me"));
    }

    #[test]
    fn the_filter_dropdown_opens_below_its_anchor() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        let area = Rect::new(0, 0, 106, 40);
        crate::ui::compute_view(&mut app, area);
        let anchor = sidebar_filter_anchor_rect(&app, app.view.sidebar_rect);
        assert!(anchor.width > 0);
        let layout = sidebar_filter_menu_layout(&app, area).expect("filter dropdown layout");
        assert_eq!(layout.rect.y, anchor.bottom());
        assert!(layout.rect.bottom() <= area.bottom());
    }

    #[test]
    fn linear_no_agent_yet_includes_ticket_linked_to_pull_request() {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("repo")];
        app.ensure_test_terminals();
        app.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        app.sidebar_work_filter.team = None;
        app.sidebar_work_filter.assignee = None;
        let mut ticket = work_ticket("SCA-9999", "linked without agent", "jacob", &[]);
        ticket.url = Some("https://linear.app/scalable/issue/SCA-9999".into());
        app.work_index_snapshot = Some(crate::work_index::Snapshot {
            items: vec![work_item("scalable-so/herdr", Some(42), vec![ticket])],
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: std::time::SystemTime::UNIX_EPOCH,
        });

        let objects = sidebar_unassigned_objects(
            &app,
            &sidebar_thread_entries(&app),
            SidebarGroupMode::LinearTeam,
        );
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0].key, "linear:SCA-9999");
        assert_eq!(objects[0].title, "SCA-9999 · linked without agent");
        assert_eq!(
            objects[0].activation.work_context_patch.repo.as_deref(),
            Some("scalable-so/herdr")
        );
    }

    #[test]
    fn f27_enter_on_no_agent_yet_rows_opens_linked_home_without_a_pane() {
        use crate::app::SidebarWorkGroupKeyAction;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::empty());
        // Nothing selected: the sidebar must not swallow the keystroke.
        assert!(matches!(
            app.handle_sidebar_work_group_key(enter),
            SidebarWorkGroupKeyAction::Ignored
        ));

        let mut unassigned_ticket = work_ticket("SCA-9999", "unassigned", "jacob", &[]);
        unassigned_ticket.url = Some("https://linear.app/scalable/issue/SCA-9999".into());
        app.work_index_snapshot
            .as_mut()
            .expect("work index fixture")
            .items
            .push(work_item(
                "scalable-so/herdr",
                None,
                vec![unassigned_ticket],
            ));
        app.sidebar_selected_work_group = Some("linear:SCA-9999".into());
        let pane_count = app.workspaces[0].tabs[0].panes.len();
        assert!(matches!(
            app.handle_sidebar_work_group_key(enter),
            SidebarWorkGroupKeyAction::Consumed
        ));
        assert_eq!(app.sidebar_selected_work_group, None);
        assert_eq!(app.workspaces[0].tabs[0].panes.len(), pane_count);
        assert!(app.dock_object_preview.is_none());
        let home = app.home.as_ref().expect("linked Home composer");
        assert!(home.prompt.contains("SCA-9999: unassigned"));
        assert_eq!(
            home.ticket
                .as_ref()
                .map(|ticket| ticket.identifier.as_str()),
            Some("SCA-9999")
        );
        let plan = home.dispatch_plan().expect("prefilled Home dispatch plan");
        assert_eq!(
            plan.work_context_patch.ticket_ids,
            Some(vec!["SCA-9999".into()])
        );
        assert_eq!(
            plan.work_context_patch.work_title.as_deref(),
            Some("unassigned")
        );

        app.sidebar_group_mode = SidebarGroupMode::Missive;
        app.work_index_snapshot
            .as_mut()
            .expect("work index fixture")
            .conversations
            .push(missive_conversation("ccc333", "new lead", CONVERSATION_C));
        app.sidebar_selected_work_group = Some(format!("missive:{CONVERSATION_C}"));
        assert!(matches!(
            app.handle_sidebar_work_group_key(enter),
            SidebarWorkGroupKeyAction::Consumed
        ));
        let home = app.home.as_ref().expect("Missive-linked Home composer");
        assert_eq!(
            home.missive
                .as_ref()
                .map(|conversation| conversation.web_url.as_str()),
            Some(CONVERSATION_C)
        );
        assert_eq!(
            home.dispatch_plan()
                .expect("prefilled Missive plan")
                .work_context_patch,
            crate::work_context::PaneWorkContextPatch {
                missive_urls: Some(vec![CONVERSATION_C.into()]),
                work_title: Some("new lead".into()),
                ..Default::default()
            }
        );

        app.sidebar_group_mode = SidebarGroupMode::RepoPr;
        app.sidebar_work_filter.github.assignee = None;
        let github = app
            .work_index_snapshot
            .as_mut()
            .expect("work index fixture")
            .items
            .iter_mut()
            .find(|item| item.pr_number == Some(159))
            .expect("pull request fixture");
        github.source.github = true;
        github.pr_title = Some("sidebar review".into());
        app.sidebar_selected_work_group =
            Some("github:https://github.com/scalable-so/herdr/pull/159".into());
        assert!(matches!(
            app.handle_sidebar_work_group_key(enter),
            SidebarWorkGroupKeyAction::Consumed
        ));
        let home = app
            .home
            .as_ref()
            .expect("pull-request-linked Home composer");
        assert_eq!(
            home.pr.as_ref().map(|pr| (pr.repo.as_str(), pr.number)),
            Some(("scalable-so/herdr", 159))
        );
        assert_eq!(
            home.directory,
            std::path::PathBuf::from("/tmp/herdr-fixture/herdr")
        );
        assert_eq!(
            home.dispatch_plan()
                .expect("prefilled pull request plan")
                .work_context_patch
                .pr_urls,
            Some(vec!["https://github.com/scalable-so/herdr/pull/159".into()])
        );
    }

    #[test]
    fn f27_enter_on_no_agent_yet_row_opens_home_when_dock_is_open() {
        use crate::app::SidebarWorkGroupKeyAction;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        let mut ticket = work_ticket("SCA-9999", "unassigned", "jacob", &[]);
        ticket.url = Some("https://linear.app/scalable/issue/SCA-9999".into());
        app.work_index_snapshot
            .as_mut()
            .expect("work index fixture")
            .items
            .push(work_item("scalable-so/herdr", None, vec![ticket]));
        let pane_count = app.workspaces[0].tabs[0].panes.len();
        app.dock_collapsed = false;
        app.sidebar_selected_work_group = Some("linear:SCA-9999".into());

        assert!(matches!(
            app.handle_sidebar_work_group_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty())),
            SidebarWorkGroupKeyAction::Consumed
        ));
        assert_eq!(app.workspaces[0].tabs[0].panes.len(), pane_count);
        assert!(app.home.is_some());
        assert!(app.dock_object_preview.is_none());
        assert_eq!(
            app.home
                .as_ref()
                .and_then(|home| home.ticket.as_ref())
                .map(|ticket| ticket.identifier.as_str()),
            Some("SCA-9999")
        );
    }

    #[test]
    fn n_on_unassigned_rows_dispatches_with_the_work_context_patch() {
        use crate::app::SidebarWorkGroupKeyAction;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app = sidebar_work_item_fixture();
        app.home_agent_choices = vec![crate::app::home::HomeAgentChoice {
            agent: crate::detect::Agent::Claude,
            model: "default".into(),
            effort: Some("auto".into()),
            context_window: None,
            access: Some(crate::app::home::HomeAccess::ClaudeBypass),
        }];
        app.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        let mut unassigned_ticket = work_ticket("SCA-9999", "unassigned", "jacob", &[]);
        unassigned_ticket.url = Some("https://linear.app/scalable/issue/SCA-9999".into());
        app.work_index_snapshot
            .as_mut()
            .expect("work index fixture")
            .items
            .push(work_item(
                "scalable-so/herdr",
                None,
                vec![unassigned_ticket],
            ));
        app.sidebar_selected_work_group = Some("linear:SCA-9999".into());
        let SidebarWorkGroupKeyAction::Dispatch(plan) = app.handle_sidebar_work_group_key(
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::empty()),
        ) else {
            panic!("n should dispatch the selected unassigned ticket");
        };
        assert!(plan.prompt.contains("SCA-9999: unassigned"));
        assert!(plan.prompt.contains("https://linear.app/"));
        assert_eq!(
            plan.work_context_patch.ticket_ids,
            Some(vec!["SCA-9999".into()])
        );
        assert_eq!(
            plan.argv,
            [
                "claude",
                "--dangerously-skip-permissions",
                plan.prompt.as_str()
            ]
        );

        app.sidebar_group_mode = SidebarGroupMode::Missive;
        app.sidebar_selected_work_group = Some(format!("missive:{CONVERSATION_B}"));
        let SidebarWorkGroupKeyAction::Dispatch(plan) = app.handle_sidebar_work_group_key(
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::empty()),
        ) else {
            panic!("n should dispatch the selected conversation");
        };
        assert_eq!(
            plan.prompt,
            format!("bbb222: fix pricing\n{CONVERSATION_B}")
        );
        assert_eq!(
            plan.work_context_patch.missive_urls,
            Some(vec![CONVERSATION_B.into()])
        );
    }

    #[test]
    fn plus_target_on_unassigned_row_builds_direct_spawn_with_work_context_patch() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        crate::ui::compute_view(&mut app, Rect::new(0, 0, 120, 40));
        let header = compute_sidebar_nested_header_areas(&app, app.view.sidebar_rect)
            .into_iter()
            .find(|header| header.key == "linear:OPS-12")
            .expect("unassigned ticket row");

        let key =
            sidebar_unassigned_spawn_at(&app, header.rect.right().saturating_sub(3), header.rect.y)
                .expect("trailing plus target");
        let plan = app
            .sidebar_unassigned_dispatch_plan(&key)
            .expect("direct spawn plan");
        assert!(plan.prompt.contains("OPS-12: pixel EMQ drop"));
        assert!(plan.prompt.ends_with("\nOPS-12"));
        assert_eq!(
            plan.work_context_patch.ticket_ids,
            Some(vec!["OPS-12".into()])
        );
    }

    #[test]
    fn f12_7_n_on_unassigned_pull_request_builds_a_spawn_plan() {
        use crate::app::SidebarWorkGroupKeyAction;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app = sidebar_work_item_fixture();

        app.sidebar_group_mode = SidebarGroupMode::RepoPr;
        app.sidebar_work_filter.github.assignee = None;
        app.work_index_snapshot
            .as_mut()
            .expect("work index fixture")
            .items
            .iter_mut()
            .find(|item| item.pr_number == Some(159))
            .expect("pull request fixture")
            .source
            .github = true;
        app.sidebar_selected_work_group =
            Some("github:https://github.com/scalable-so/herdr/pull/159".into());
        let SidebarWorkGroupKeyAction::Dispatch(plan) = app.handle_sidebar_work_group_key(
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::empty()),
        ) else {
            panic!("n should dispatch the selected unassigned pull request");
        };
        assert_eq!(
            plan.directory,
            std::path::PathBuf::from("/tmp/herdr-fixture/herdr")
        );
        assert_eq!(
            plan.work_context_patch.pr_urls,
            Some(vec!["https://github.com/scalable-so/herdr/pull/159".into()])
        );
    }

    #[test]
    fn f12_7a_limits_then_expands_and_scrolls_105_unassigned_prs() {
        use crate::app::SidebarWorkGroupKeyAction;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app = AppState::test_new();
        app.sidebar_group_mode = SidebarGroupMode::RepoPr;
        app.sidebar_work_filter.github.assignee = None;
        app.work_index_enabled = true;
        app.work_index_snapshot = Some(crate::work_index::Snapshot {
            items: (1..=105)
                .map(|number| {
                    let mut item = work_item("owner/repo", Some(number), Vec::new());
                    item.pr_title = Some(format!("pull request {number}"));
                    item.created_at = Some(
                        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(number),
                    );
                    item.source.github = true;
                    item
                })
                .collect(),
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: std::time::SystemTime::UNIX_EPOCH,
        });

        let mut rows = Vec::new();
        append_unassigned_rows(&app, &mut rows, &[]);
        let titles = rows
            .iter()
            .filter_map(|row| match row {
                SidebarRow::NestedHeader { title, .. } => Some(title.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(titles.len(), 11);
        assert_eq!(titles.first().copied(), Some("#105 pull request 105"));
        assert_eq!(titles.last().copied(), Some("show 95 more…"));

        app.sidebar_selected_work_group = Some(sidebar_show_more_key(SidebarGroupMode::RepoPr));
        assert!(matches!(
            app.handle_sidebar_work_group_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty())),
            SidebarWorkGroupKeyAction::Consumed
        ));
        let mut expanded = Vec::new();
        append_unassigned_rows(&app, &mut expanded, &[]);
        assert_eq!(
            expanded
                .iter()
                .filter(|row| matches!(row, SidebarRow::NestedHeader { spawn: true, .. }))
                .count(),
            105
        );

        crate::ui::compute_view(&mut app, Rect::new(0, 0, 80, 24));
        app.workspace_scroll = normalized_workspace_scroll(&app, app.view.sidebar_rect, usize::MAX);
        let visible = compute_sidebar_nested_header_areas(&app, app.view.sidebar_rect);
        assert!(
            visible.iter().any(|header| header.key.ends_with("/pull/1")),
            "the oldest row remains reachable at the bottom of a 105-row list: {:?}",
            visible.iter().map(|header| &header.key).collect::<Vec<_>>()
        );
    }

    #[test]
    fn f20_missive_unassigned_headers_use_one_capped_projection() {
        use crate::app::SidebarWorkGroupKeyAction;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app = AppState::test_new();
        app.sidebar_group_mode = SidebarGroupMode::Missive;
        app.sidebar_work_filter.missive.assignee = None;
        app.work_index_snapshot = Some(crate::work_index::Snapshot {
            items: Vec::new(),
            conversations: (1..=12)
                .map(|number| {
                    let url = format!("https://mail.missiveapp.com/#inbox/conversations/c{number}");
                    let mut conversation = missive_conversation(
                        &format!("c{number}"),
                        &format!("subject {number}"),
                        &url,
                    );
                    conversation.last_activity_at = Some(
                        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(number),
                    );
                    conversation
                })
                .collect(),
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: std::time::SystemTime::UNIX_EPOCH,
        });

        let rows = sidebar_rows(&app);
        assert!(rows.iter().any(|row| {
            matches!(
                row,
                SidebarRow::SectionHeader {
                    title: NO_AGENT_YET_SECTION_TITLE,
                    ..
                }
            )
        }));
        let dim_titles = rows
            .iter()
            .filter_map(|row| match row {
                SidebarRow::NestedHeader {
                    title, dim: true, ..
                } => Some(title.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(dim_titles.len(), 11);
        assert_eq!(dim_titles.first().copied(), Some("c12 · subject 12"));
        assert_eq!(dim_titles.last().copied(), Some("show 2 more…"));

        app.sidebar_selected_work_group = Some(sidebar_show_more_key(SidebarGroupMode::Missive));
        assert!(matches!(
            app.handle_sidebar_work_group_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty())),
            SidebarWorkGroupKeyAction::Consumed
        ));
        assert_eq!(
            sidebar_rows(&app)
                .iter()
                .filter(|row| matches!(
                    row,
                    SidebarRow::NestedHeader {
                        dim: true,
                        spawn: true,
                        ..
                    }
                ))
                .count(),
            12
        );
    }

    #[test]
    fn repo_unassigned_spawn_uses_the_bound_checkout_path() {
        let mut workspace = Workspace::test_new("repo");
        workspace.repo_binding = Some("owner/repo".into());
        let checkout = workspace.resolved_identity_cwd().expect("checkout path");
        let mut app = AppState::test_new();
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.work_index_snapshot = Some(crate::work_index::Snapshot {
            items: vec![work_item("owner/repo", Some(42), Vec::new())],
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: std::time::SystemTime::UNIX_EPOCH,
        });

        let objects =
            sidebar_unassigned_objects(&app, &sidebar_thread_entries(&app), SidebarGroupMode::Repo);
        assert_eq!(objects.len(), 1);
        assert_eq!(
            objects[0].activation.object_link,
            checkout.display().to_string()
        );
        let plan = app
            .sidebar_unassigned_dispatch_plan(&objects[0].key)
            .expect("repo spawn plan");
        assert_eq!(plan.directory, checkout);
        assert_eq!(plan.prompt, plan.directory.display().to_string());
        assert_eq!(plan.work_context_patch.repo.as_deref(), Some("owner/repo"));
    }

    #[test]
    fn a_dim_ticket_header_activates_with_its_identifier_title_and_repo() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        let activation =
            sidebar_work_group_activation(&app, "linear:SCA-3102").expect("ticket activation");
        assert!(activation
            .spawn_prompt
            .starts_with("SCA-3102: annual credits\n"));
        assert_eq!(
            activation.directory,
            Some(std::path::PathBuf::from("/tmp/herdr-fixture/herdr"))
        );

        // A ticket with no linked pull request leaves the directory alone.
        let unlinked =
            sidebar_work_group_activation(&app, "linear:SCA-3170").expect("ticket activation");
        assert!(unlinked
            .spawn_prompt
            .starts_with("SCA-3170: ads skill map\n"));
        assert_eq!(unlinked.directory, None);

        app.sidebar_group_mode = SidebarGroupMode::Missive;
        let conversation =
            sidebar_work_group_activation(&app, &format!("missive:{CONVERSATION_B}"))
                .expect("conversation activation");
        assert_eq!(
            conversation.spawn_prompt,
            format!("bbb222: fix pricing\n{CONVERSATION_B}")
        );
    }

    fn sidebar_grouping_fixture() -> AppState {
        let mut workspace = Workspace::test_new("repo");
        workspace.test_add_tab(Some("review"));
        workspace.test_add_tab(Some("unlinked"));
        workspace.test_add_tab(Some("session"));
        let mut app = AppState::test_new();
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        for (tab_idx, context) in [
            crate::work_context::PaneWorkContext {
                pr_urls: vec!["https://github.com/scalable-so/herdr/pull/159".into()],
                branch: Some("feature/pricing".into()),
                work_title: Some("pricing".into()),
                ..Default::default()
            },
            crate::work_context::PaneWorkContext {
                pr_urls: vec!["https://github.com/scalable-so/herdr/pull/159".into()],
                branch: Some("review/pricing".into()),
                work_title: Some("pricing".into()),
                ..Default::default()
            },
            crate::work_context::PaneWorkContext::default(),
            crate::work_context::PaneWorkContext {
                pr_urls: vec!["https://github.com/scalable-so/herdr/pull/160".into()],
                branch: Some("session/branch".into()),
                session_name: Some("session fallback".into()),
                ..Default::default()
            },
        ]
        .into_iter()
        .enumerate()
        {
            let pane_id = app.workspaces[0].tabs[tab_idx].root_pane;
            let terminal_id = app.workspaces[0].tabs[tab_idx].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.terminals
                .get_mut(&terminal_id)
                .expect("fixture terminal")
                .replace_prevalidated_manual_work_context(context);
        }
        app.reconcile_sidebar_presentation();
        app
    }

    fn replace_tab_context(
        app: &mut AppState,
        ws_idx: usize,
        tab_idx: usize,
        manual: crate::work_context::PaneWorkContext,
        inferred: crate::work_context::PaneWorkContext,
    ) {
        let pane_id = app.workspaces[ws_idx].tabs[tab_idx].root_pane;
        let terminal_id = app.workspaces[ws_idx].tabs[tab_idx].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app
            .terminals
            .get_mut(&terminal_id)
            .expect("fixture terminal");
        terminal
            .replace_git_work_context(inferred)
            .expect("valid inferred context");
        terminal.replace_prevalidated_manual_work_context(manual);
    }

    #[test]
    fn f19_repo_groups_by_repo_then_branch_with_server_space_suffix() {
        let mut first = Workspace::test_new("first");
        first.custom_name = Some("Server Alpha".into());
        first.test_add_tab(Some("second"));
        first.test_add_tab(Some("loose"));
        let mut second = Workspace::test_new("second");
        second.custom_name = Some("Server Beta".into());
        let mut app = AppState::test_new();
        app.workspaces = vec![first, second];
        app.ensure_test_terminals();
        replace_tab_context(
            &mut app,
            0,
            0,
            crate::work_context::PaneWorkContext {
                repo: Some("scalable-so/herdr".into()),
                branch: Some("main".into()),
                ..Default::default()
            },
            Default::default(),
        );
        replace_tab_context(
            &mut app,
            0,
            1,
            crate::work_context::PaneWorkContext {
                repo: Some("scalable-so/herdr".into()),
                branch: Some("feature/sidebar".into()),
                ..Default::default()
            },
            Default::default(),
        );
        replace_tab_context(
            &mut app,
            0,
            2,
            crate::work_context::PaneWorkContext {
                work_title: Some("unbound work".into()),
                ..Default::default()
            },
            Default::default(),
        );
        replace_tab_context(
            &mut app,
            1,
            0,
            crate::work_context::PaneWorkContext {
                repo: Some("scalable-so/growth".into()),
                branch: Some("main".into()),
                ..Default::default()
            },
            Default::default(),
        );
        app.reconcile_sidebar_presentation();

        let rows = sidebar_rows(&app);
        let repo_headers = rows
            .iter()
            .filter_map(|row| match row {
                SidebarRow::Workspace { title, count, .. } => Some((title.as_str(), *count)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            repo_headers,
            [
                ("scalable-so/herdr", Some(2)),
                ("scalable-so/growth", Some(1))
            ]
        );
        let nested = rows
            .iter()
            .filter_map(|row| match row {
                SidebarRow::NestedHeader { title, count, .. } => Some((title.as_str(), *count)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            nested,
            [
                ("⎇ main", 1),
                ("⎇ feature/sidebar", 1),
                (unlinked_bucket_title().as_str(), 1)
            ]
        );
        let suffixes = rows
            .iter()
            .filter_map(|row| match row {
                SidebarRow::Tab { entry, .. } => Some(entry.space_label.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            suffixes,
            [
                "Server Alpha",
                "Server Alpha",
                "Server Beta",
                "Server Alpha"
            ]
        );
        assert!(matches!(
            rows.last(),
            Some(SidebarRow::Tab { entry, .. }) if entry.ws_idx == 0 && entry.tab_idx == 2
        ));
    }

    #[test]
    fn f19_identifier_prefix_deduplicates_supported_separators() {
        for title in [
            "SCA-3165: Studio edit",
            "sca-3165 Studio edit",
            "SCA-3165 · Studio edit",
            "SCA-3165- Studio edit",
        ] {
            assert_eq!(
                work_group_header_title("SCA-3165", Some(title)),
                "SCA-3165 · Studio edit"
            );
        }
        assert_eq!(
            work_group_header_title("SCA-3165", Some("Studio edit")),
            "SCA-3165 · Studio edit"
        );
        assert_eq!(work_group_header_title("SCA-3165", None), "SCA-3165");

        let row_title = |title: &str| {
            crate::workspace::session_title(
                Some(&crate::workspace::TabDisplayProjection::Derived {
                    agent: None,
                    ticket: Some("SCA-3165".into()),
                    binding: None,
                    title: Some(title.into()),
                }),
                None,
            )
            .expect("derived row title")
        };
        assert_eq!(row_title("SCA-3165: Studio edit"), "SCA-3165 · Studio edit");
        assert_eq!(row_title("sca-3165 Studio edit"), "SCA-3165 · Studio edit");
        assert_eq!(row_title("Studio edit"), "SCA-3165 · Studio edit");
    }

    #[test]
    fn agent_row_title_omits_worktree_binding() {
        let projection = crate::workspace::TabDisplayProjection::Derived {
            agent: Some("codex".into()),
            ticket: Some("SCA-3165".into()),
            binding: Some("sidebar-view-fixes".into()),
            title: Some("Fix sidebar rows".into()),
        };

        assert_eq!(
            crate::workspace::session_title(Some(&projection), None).as_deref(),
            Some("SCA-3165 · Fix sidebar rows")
        );

        let binding_only = crate::workspace::TabDisplayProjection::Derived {
            agent: Some("codex".into()),
            ticket: None,
            binding: Some("sidebar-view-fixes".into()),
            title: None,
        };
        assert_eq!(
            crate::workspace::session_title(Some(&binding_only), Some("New Thread".into()))
                .as_deref(),
            Some("codex")
        );
    }

    #[test]
    fn f19_object_groups_are_top_level_with_one_trailing_unlinked_bucket() {
        let mut workspace = Workspace::test_new("repo");
        workspace.custom_name = Some("Server Space".into());
        workspace.test_add_tab(Some("second"));
        workspace.test_add_tab(Some("loose"));
        let mut app = AppState::test_new();
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        for (tab_idx, context) in [
            crate::work_context::PaneWorkContext {
                pr_urls: vec!["https://github.com/herdrdev/herdr/pull/206".into()],
                work_title: Some("#206 feat sidebar".into()),
                ..Default::default()
            },
            crate::work_context::PaneWorkContext {
                pr_urls: vec!["https://github.com/herdrdev/herdr/pull/159".into()],
                work_title: Some("pricing".into()),
                ..Default::default()
            },
            crate::work_context::PaneWorkContext {
                work_title: Some("unbound".into()),
                ..Default::default()
            },
        ]
        .into_iter()
        .enumerate()
        {
            replace_tab_context(&mut app, 0, tab_idx, context, Default::default());
        }
        app.set_sidebar_group_mode(SidebarGroupMode::RepoPr);

        let rows = sidebar_rows(&app);
        assert!(!rows
            .iter()
            .any(|row| matches!(row, SidebarRow::Workspace { .. })));
        let headers = rows
            .iter()
            .filter_map(|row| match row {
                SidebarRow::NestedHeader {
                    title,
                    count,
                    dim: false,
                    ..
                } => Some((title.as_str(), *count)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            headers,
            [
                ("#206 · feat sidebar", 1),
                ("#159 · pricing", 1),
                (unlinked_bucket_title().as_str(), 1)
            ]
        );
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, SidebarRow::NestedHeader { title, .. } if *title == unlinked_bucket_title()))
                .count(),
            1
        );
        for (index, row) in rows.iter().enumerate() {
            let SidebarRow::NestedHeader { count, .. } = row else {
                continue;
            };
            let rendered = rows[index + 1..]
                .iter()
                .take_while(|row| matches!(row, SidebarRow::Tab { .. }))
                .count();
            assert_eq!(*count, rendered);
        }

        let key = "github:https://github.com/herdrdev/herdr/pull/206";
        app.toggle_sidebar_group(key);
        let collapsed = sidebar_rows(&app);
        assert!(collapsed.iter().any(|row| matches!(
            row,
            SidebarRow::NestedHeader {
                key: candidate,
                collapsed: true,
                ..
            } if candidate == key
        )));
        assert!(!collapsed.windows(2).any(|pair| matches!(
            pair,
            [
                SidebarRow::NestedHeader { key: candidate, .. },
                SidebarRow::Tab { .. }
            ] if candidate == key
        )));
    }

    #[test]
    fn f19_multiple_declared_objects_render_once_under_each_object() {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("declared")];
        app.ensure_test_terminals();
        replace_tab_context(
            &mut app,
            0,
            0,
            crate::work_context::PaneWorkContext {
                pr_urls: vec![
                    "https://github.com/herdrdev/herdr/pull/159".into(),
                    "https://github.com/herdrdev/herdr/pull/160".into(),
                ],
                ..Default::default()
            },
            crate::work_context::PaneWorkContext {
                pr_urls: vec!["https://github.com/herdrdev/herdr/pull/206".into()],
                ..Default::default()
            },
        );
        let groups = sidebar_work_groups(
            &app,
            &sidebar_thread_entries(&app),
            SidebarGroupMode::RepoPr,
        );
        assert_eq!(
            groups
                .iter()
                .map(|group| (group.title.as_str(), group.entries.len()))
                .collect::<Vec<_>>(),
            [("#159", 1), ("#160", 1)]
        );
    }

    #[test]
    fn f19_declared_bindings_override_seed_branch_inference() {
        const INFERRED_PR: &str = "https://github.com/herdrdev/herdr/pull/206";
        const DECLARED_PR: &str = "https://github.com/herdrdev/herdr/pull/159";
        let mut workspace = Workspace::test_new("seed");
        for index in 1..5 {
            workspace.test_add_tab(Some(&format!("seed-{index}")));
        }
        let mut app = AppState::test_new();
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        for tab_idx in 0..5 {
            let manual = if tab_idx == 4 {
                crate::work_context::PaneWorkContext {
                    pr_urls: vec![DECLARED_PR.into()],
                    ticket_ids: vec!["SCA-159".into()],
                    work_title: Some("declared sample".into()),
                    ..Default::default()
                }
            } else {
                Default::default()
            };
            replace_tab_context(
                &mut app,
                0,
                tab_idx,
                manual,
                crate::work_context::PaneWorkContext {
                    pr_urls: vec![INFERRED_PR.into()],
                    ticket_ids: vec!["SCA-206".into()],
                    repo: Some("herdrdev/herdr".into()),
                    branch: Some("t3/integration2".into()),
                    work_title: Some("inferred integration".into()),
                    ..Default::default()
                },
            );
        }

        app.set_sidebar_group_mode(SidebarGroupMode::RepoPr);
        let github = sidebar_work_groups(
            &app,
            &sidebar_thread_entries(&app),
            SidebarGroupMode::RepoPr,
        );
        let github_counts = github
            .iter()
            .map(|group| (group.key.clone(), group.entries.len()))
            .collect::<Vec<_>>();
        assert_eq!(
            github_counts,
            [
                (format!("github:{INFERRED_PR}"), 4),
                (format!("github:{DECLARED_PR}"), 1),
            ]
        );

        app.set_sidebar_group_mode(SidebarGroupMode::LinearTeam);
        let linear = sidebar_work_groups(
            &app,
            &sidebar_thread_entries(&app),
            SidebarGroupMode::LinearTeam,
        );
        let mut linear_counts = linear
            .iter()
            .map(|group| (group.key.as_str(), group.entries.len()))
            .collect::<Vec<_>>();
        linear_counts.sort_unstable();
        assert_eq!(
            linear_counts,
            [("linear:SCA-159", 1), ("linear:SCA-206", 4)]
        );
    }

    fn workspace_entry_tree(entries: Vec<WorkspaceListEntry>) -> Vec<String> {
        entries
            .into_iter()
            .map(|entry| match entry {
                WorkspaceListEntry::Workspace { ws_idx, indented } => {
                    format!("repo:{ws_idx}:{indented}")
                }
                WorkspaceListEntry::NestedHeader { title, .. } => title,
            })
            .collect()
    }

    #[test]
    fn repo_mode_matches_frozen_workspace_entries() {
        let app = sidebar_grouping_fixture();
        let before = workspace_list_entries(&app);
        let after = workspace_list_entries_for_mode(&app, false, SidebarGroupMode::Repo);
        assert_eq!(before, after);
        assert_eq!(
            workspace_entry_tree(after),
            vec!["repo:0:false".to_string()]
        );
    }

    #[test]
    fn repo_view_keeps_owner_repo_and_elides_it_at_minimum_width() {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("repo")];
        app.ensure_test_terminals();
        replace_tab_context(
            &mut app,
            0,
            0,
            crate::work_context::PaneWorkContext {
                repo: Some("matthias-scale/herdr".into()),
                work_title: Some("Sidebar fixes".into()),
                ..Default::default()
            },
            Default::default(),
        );
        app.set_sidebar_group_mode(SidebarGroupMode::Repo);

        let rows = sidebar_rows(&app);
        assert!(rows.iter().any(|row| matches!(
            row,
            SidebarRow::Workspace { title, .. } if title == "matthias-scale/herdr"
        )));

        let area = Rect::new(0, 0, 18, 12);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let header = compute_workspace_card_areas(&app, area)
            .into_iter()
            .next()
            .expect("repo header");
        let rendered = row_text(
            terminal.backend().buffer(),
            header.rect.y,
            header.rect.width,
        );
        assert!(rendered.contains("matthias"), "{rendered:?}");
        assert!(rendered.contains('…'), "{rendered:?}");
        assert!(display_width(&rendered) <= usize::from(header.rect.width));
    }

    #[test]
    fn parent_rows_start_left_of_children_at_supported_widths() {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("repo")];
        app.ensure_test_terminals();
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Codex);
        replace_tab_context(
            &mut app,
            0,
            0,
            crate::work_context::PaneWorkContext {
                pr_urls: vec!["https://github.com/herdrdev/herdr/pull/42".into()],
                work_title: Some("Fix nested row hierarchy".into()),
                ..Default::default()
            },
            Default::default(),
        );
        app.set_sidebar_group_mode(SidebarGroupMode::RepoPr);

        for width in [18, 30] {
            let area = Rect::new(0, 0, width, 12);
            let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
            terminal
                .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
                .unwrap();
            let header = compute_sidebar_nested_header_areas(&app, area)
                .into_iter()
                .find(|header| !header.dim)
                .expect("parent header");
            let child = compute_tab_card_areas(&app, area)
                .into_iter()
                .next()
                .expect("child row");
            let first_non_space = |y, row_width| {
                row_text(terminal.backend().buffer(), y, row_width)
                    .chars()
                    .position(|character| !character.is_whitespace())
                    .expect("visible row content")
            };

            assert!(
                first_non_space(header.rect.y, header.rect.width)
                    < first_non_space(child.rect.y, child.rect.width),
                "parent must start left of child at width {width}"
            );
        }
    }

    #[test]
    fn sidebar_grouping_modes_render_expected_header_tree() {
        let mut app = sidebar_grouping_fixture();
        let tree = |mode| workspace_entry_tree(workspace_list_entries_for_mode(&app, false, mode));
        assert_eq!(tree(SidebarGroupMode::Repo), ["repo:0:false"]);
        assert_eq!(
            tree(SidebarGroupMode::RepoPr),
            [
                "repo:0:false",
                "#159 · pricing",
                "#160 · session fallback",
                unlinked_bucket_title().as_str(),
            ]
        );
        assert_eq!(
            tree(SidebarGroupMode::RepoWorktree),
            [
                "repo:0:false",
                "⎇ feature/pricing",
                "⎇ review/pricing",
                "⎇ session/branch",
                unlinked_bucket_title().as_str(),
            ]
        );
        // Work items are the top level in these modes; this fixture binds none,
        // so every pane lands in the unlinked bucket and no repo header shows.
        assert_eq!(tree(SidebarGroupMode::LinearTeam), ["unlinked"]);
        assert_eq!(tree(SidebarGroupMode::Missive), [unlinked_bucket_title()]);

        let mut tab_sets = Vec::new();
        for mode in SidebarGroupMode::ALL {
            app.set_sidebar_group_mode(mode);
            let rows = sidebar_rows(&app);
            let mut tabs = rows
                .iter()
                .filter_map(|row| match row {
                    SidebarRow::Tab { entry, .. } => Some((entry.ws_idx, entry.tab_idx)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            tabs.sort_unstable();
            tab_sets.push(tabs);
            let nested = rows
                .iter()
                .filter_map(|row| match row {
                    SidebarRow::NestedHeader { title, .. } => Some(title.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            match mode {
                SidebarGroupMode::Repo => assert_eq!(nested, [unlinked_bucket_title()]),
                SidebarGroupMode::Spaces => assert!(nested.is_empty()),
                SidebarGroupMode::RepoPr => {
                    assert_eq!(
                        nested,
                        [
                            "#159 · pricing",
                            "#160 · session fallback",
                            unlinked_bucket_title().as_str(),
                            "no open PRs for me · author or assignee",
                        ]
                    )
                }
                SidebarGroupMode::RepoWorktree => assert_eq!(
                    nested,
                    [
                        "⎇ feature/pricing",
                        "⎇ review/pricing",
                        "⎇ session/branch",
                        unlinked_bucket_title().as_str(),
                    ]
                ),
                SidebarGroupMode::LinearTeam => assert_eq!(
                    nested,
                    [
                        "unlinked".to_string(),
                        "no active tickets for me · creator or assignee".to_string()
                    ]
                ),
                SidebarGroupMode::Missive => assert_eq!(
                    nested,
                    [
                        unlinked_bucket_title(),
                        "no open conversations for me · assignee".to_string()
                    ]
                ),
            }
        }
        assert!(tab_sets.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn sidebar_group_collapse_survives_mode_switch() {
        let mut app = sidebar_grouping_fixture();
        app.set_sidebar_group_mode(SidebarGroupMode::RepoPr);
        let pr_key = "0:https://github.com/scalable-so/herdr/pull/159";
        app.toggle_sidebar_group(pr_key);
        assert!(section_is_collapsed(&app, pr_key));

        app.set_sidebar_group_mode(SidebarGroupMode::RepoWorktree);
        let branch_key = "0:feature/pricing";
        assert!(!section_is_collapsed(&app, branch_key));
        app.toggle_sidebar_group(branch_key);
        assert!(section_is_collapsed(&app, branch_key));

        app.set_sidebar_group_mode(SidebarGroupMode::RepoPr);
        assert!(section_is_collapsed(&app, pr_key));
        assert_eq!(
            app.collapsed_sidebar_groups,
            std::collections::HashSet::from([
                "repo:Recently done".to_string(),
                format!("repo_pr:{pr_key}"),
                format!("repo_worktree:{branch_key}"),
            ])
        );
    }

    #[test]
    fn same_entry_uses_identical_row_layout_under_all_group_modes() {
        let mut app = sidebar_grouping_fixture();
        let mut rendered_rows = Vec::new();
        for mode in SidebarGroupMode::ALL {
            app.set_sidebar_group_mode(mode);
            let entry = sidebar_rows(&app)
                .into_iter()
                .find_map(|row| match row {
                    SidebarRow::Tab { entry, .. } if entry.ws_idx == 0 && entry.tab_idx == 0 => {
                        Some(entry)
                    }
                    _ => None,
                })
                .expect("first tab row");
            let mut terminal = Terminal::new(TestBackend::new(40, 1)).expect("test terminal");
            terminal
                .draw(|frame| {
                    render_compact_agent_row(
                        &app,
                        frame,
                        &entry,
                        Rect::new(0, 0, 40, 1),
                        1,
                        true,
                        None,
                    );
                })
                .expect("render tab row");
            rendered_rows.push(row_text(terminal.backend().buffer(), 0, 40));
        }
        assert!(
            rendered_rows.windows(2).all(|pair| pair[0] == pair[1]),
            "{rendered_rows:?}"
        );
    }

    #[test]
    fn sidebar_mode_dropdown_rect_is_below_its_anchor() {
        let mut app = AppState::test_new();
        app.view.sidebar_rect = Rect::new(0, 2, 28, 20);
        let anchor = sidebar_group_mode_anchor_rect(app.view.sidebar_rect);
        let layout =
            sidebar_group_menu_layout(&app, Rect::new(0, 0, 120, 40)).expect("mode dropdown fits");
        assert_eq!(layout.rect.y, anchor.bottom());
        assert!(layout.rect.bottom() <= 40);
    }

    #[test]
    fn spaces_view_lists_every_space_flat_without_repo_grouping() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            workspace_with_worktree_space("review", Some("repo-key"), "/repo/herdr-review"),
            Workspace::test_new("notes"),
        ];

        // Repo view keeps the worktree group: one parent row plus indented
        // members, and the ungrouped Space beside it.
        let repo = workspace_list_entries_for_mode(&app, false, SidebarGroupMode::Repo);
        assert!(
            repo.iter()
                .any(|entry| matches!(entry, WorkspaceListEntry::Workspace { indented: true, .. })),
            "{repo:?}"
        );

        let spaces = workspace_list_entries_for_mode(&app, false, SidebarGroupMode::Spaces);
        assert_eq!(
            spaces
                .iter()
                .filter_map(|entry| match entry {
                    WorkspaceListEntry::Workspace { ws_idx, indented } =>
                        Some((*ws_idx, *indented)),
                    WorkspaceListEntry::NestedHeader { .. } => None,
                })
                .collect::<Vec<_>>(),
            vec![(0, false), (1, false), (2, false), (3, false)],
        );
    }

    #[test]
    fn spaces_view_without_repositories_keeps_one_row_per_space() {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("alpha"), Workspace::test_new("beta")];
        app.ensure_test_terminals();
        app.set_sidebar_group_mode(SidebarGroupMode::Spaces);

        let rows = sidebar_rows(&app);
        let spaces = rows
            .iter()
            .filter_map(|row| match row {
                SidebarRow::Workspace { ws_idx, .. } => Some(*ws_idx),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(spaces, [0, 1]);
        assert!(!rows.iter().any(|row| matches!(
            row,
            SidebarRow::NestedHeader { title, .. } if *title == unlinked_bucket_title()
        )));
    }

    #[test]
    fn spaces_view_is_a_view_entry_but_not_the_default() {
        assert_eq!(SidebarGroupMode::default(), SidebarGroupMode::Repo);
        assert!(SidebarGroupMode::VIEWS.contains(&SidebarGroupMode::Spaces));
        assert_eq!(SidebarGroupMode::Spaces.view_label(), "Spaces");
        assert_eq!(SidebarGroupMode::Spaces.collapse_namespace(), "spaces");
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
        assert!(grouped.contains("main (0/3)"), "{grouped:?}");
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
        let title = buffer[(find_symbol_x(buffer, row, area.width, "m"), row)].style();

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
                    SidebarRow::NestedHeader { title, .. } => format!("nested:{title}"),
                    SidebarRow::SymphonyJob { name, .. } => format!("symphony:{name}"),
                    SidebarRow::SymphonyEmpty => "symphony:empty".to_string(),
                })
                .collect::<Vec<_>>(),
            vec![
                "section:Spaces",
                "space:0",
                "window:0:0",
                "window:0:1",
                "window:1:0"
            ]
        );

        assert!(app.toggle_workspace_agent_disclosure(0));
        assert!(matches!(
            sidebar_rows(&app).as_slice(),
            [
                SidebarRow::SectionHeader { .. },
                SidebarRow::Workspace { ws_idx: 0, .. }
            ]
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
    fn tab_shell_state_sums_across_panes_without_adding_rows() {
        let mut app = app_with_agents(&["one"]);
        let second = app.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.ensure_test_terminals();
        let first = app.workspaces[0].tabs[0].root_pane;
        for pane in [first, second] {
            let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&terminal_id).unwrap().holds_shell = true;
        }

        let tabs = sidebar_rows(&app)
            .into_iter()
            .filter_map(|row| match row {
                SidebarRow::Tab { entry, .. } => Some(entry),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(tabs.len(), 1);
        assert!(tabs[0].holds_shell);
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
                false,
                started + std::time::Duration::from_secs(1),
            );
        app.terminals.get_mut(&first_terminal).unwrap();
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
                false,
                blocked_at,
            );
        app.terminals.get_mut(&second_terminal).unwrap();
        app.view_observed_at = blocked_at + std::time::Duration::from_secs(60);
        app.reconcile_sidebar_presentation();

        let sidebar_tab = sidebar_rows(&app)
            .into_iter()
            .find_map(|row| match row {
                SidebarRow::Tab { entry, .. } => Some(entry),
                _ => None,
            })
            .expect("sidebar tab row");
        assert_eq!(sidebar_tab.state, AgentState::Blocked);
        assert_eq!(
            tab_row_layout(
                &sidebar_tab,
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
    fn tab_shell_marker_renders_in_the_provider_column() {
        let mut app = app_with_agents(&["one"]);
        app.workspaces[0].tabs[0].custom_name = Some("Use Repository Instructions".into());
        let pane = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&terminal_id).unwrap().holds_shell = true;
        app.reconcile_sidebar_presentation();

        let area = Rect::new(0, 0, 60, 10);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let row = compute_tab_card_areas(&app, area)[0].rect.y;
        let rendered = row_text(terminal.backend().buffer(), row, area.width - 1);

        assert!(
            rendered.contains("Use Repository Instructions"),
            "{rendered:?}"
        );
        assert!(rendered.contains("pi >_"), "{rendered:?}");
        assert!(!rendered.contains("  2 >_"), "{rendered:?}");
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
            rendered
                .iter()
                .any(|row| row.contains("Codex task") && row.contains("cx")),
            "{rendered:?}"
        );
        assert!(
            rendered
                .iter()
                .any(|row| row.contains("Claude task") && row.contains("cc")),
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
            false,
            started,
        );
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
            assert!(rendered.contains("cx"), "{width}: {rendered:?}");
            let dot = rendered.find('●').unwrap();
            let suffix = rendered.find("cx").unwrap();
            assert!(suffix > dot + '●'.len_utf8() + 1, "{width}: {rendered:?}");
            if width == 18 {
                assert!(!rendered.contains("working"), "{rendered:?}");
                assert!(!rendered.contains(">_"), "{rendered:?}");
                assert!(!rendered.contains("ago"), "{rendered:?}");
            } else {
                assert!(rendered.ends_with("1m"), "{rendered:?}");
                assert!(!rendered.contains(" · one"), "{rendered:?}");
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
            false,
            started,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Idle,
            false,
            true,
            false,
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
            false,
            started,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
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
        // Tall enough to clear the always-present Blocked and Spaces headers
        // above the single grouped workspace row.
        let area = Rect::new(0, 0, 30, 6);
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

        // The workspace row now sits below the always-present Blocked and
        // Spaces header rows.
        let workspace_row = app.view.workspace_card_areas[0].rect.y;
        let rendered = row_text(terminal.backend().buffer(), workspace_row, area.width);
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
        let spacious_metrics = workspace_list_scroll_metrics(&app, Rect::new(0, 0, 30, 6));
        assert_eq!(spacious_metrics.viewport_rows, 2);
        assert_eq!(spacious_metrics.max_offset_from_bottom, 1);

        app.sidebar_spaces.row_gap = 0;
        let (packed, _) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 30));
        assert!(packed
            .windows(2)
            .all(|pair| pair[1].rect.y == pair[0].rect.y + pair[0].rect.height));
        let packed_metrics = workspace_list_scroll_metrics(&app, Rect::new(0, 0, 30, 6));
        assert_eq!(packed_metrics.viewport_rows, 3);
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
                source_id: 0,
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

        // Search adds one header row without changing the three-row viewport
        // this metric contract exercises.
        let ws_area = Rect::new(0, 0, 30, 6);
        let metrics = workspace_list_scroll_metrics(&app, ws_area);

        assert_eq!(metrics.viewport_rows, 3);
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
        // Land on "notes" without coupling the fixture to the set of section
        // headers that precede the Spaces tree.
        app.workspace_scroll = sidebar_rows(&app)
            .iter()
            .position(|row| matches!(row, SidebarRow::Workspace { ws_idx: 2, .. }))
            .expect("notes workspace row");

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
        assert_eq!(layout.dot, "●");
        // The derived projection already leads with the agent, so no provider chip.
        assert_eq!(layout.provider, "cx");

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
        assert_eq!(layout.dot, "●");
        // The label no longer names the agent, so the provider chip returns.
        assert_eq!(layout.provider, "cx");
    }

    #[test]
    fn sidebar_section_order_preserves_collapsed_state() {
        let observed_at = std::time::Instant::now();
        let mut app = app_for_real_sidebar_fixtures(&["blocked", "priority", "ordinary"]);
        configure_real_sidebar_agent(
            &mut app,
            0,
            Agent::Claude,
            "claude",
            "2.1.245",
            "Approve native tool use",
            AgentState::Blocked,
            observed_at,
        );
        configure_real_sidebar_agent(
            &mut app,
            1,
            Agent::Codex,
            "codex",
            "0.42.0",
            "Review sidebar ordering",
            AgentState::Working,
            observed_at,
        );
        app.workspaces[1].tabs[0].prio = true;
        app.workspaces[1].tabs[0].pinned = true;
        app.collapsed_sidebar_groups
            .insert("repo:Pinned".to_string());
        app.collapsed_space_keys.insert("repo-key".into());

        let area = Rect::new(0, 0, 48, 24);
        let headers = compute_sidebar_section_header_areas(&app, area);
        assert!(!headers
            .iter()
            .any(|header| header.title == PINNED_SECTION_TITLE));
        assert!(headers
            .iter()
            .any(|header| header.title == SPACES_SECTION_TITLE));
        assert!(section_is_collapsed(&app, PINNED_SECTION_TITLE));
        assert!(app.collapsed_space_keys.contains("repo-key"));

        let snapshot = crate::persist::SessionSnapshot {
            generation: None,
            version: 3,
            workspaces: Vec::new(),
            active: None,
            selected: 0,
            sidebar_width: Some(48),
            sidebar_section_split: Some(app.sidebar_section_split),
            collapsed_space_keys: app.collapsed_space_keys.clone(),
            prio_panel_collapsed: app.prio_panel_collapsed,
        };
        let restored: crate::persist::SessionSnapshot =
            serde_json::from_value(serde_json::to_value(snapshot).unwrap()).unwrap();
        assert_eq!(restored.version, 3);
        assert_eq!(restored.collapsed_space_keys, app.collapsed_space_keys);
        assert_eq!(restored.prio_panel_collapsed, app.prio_panel_collapsed);
    }

    #[test]
    fn sidebar_agent_rows_omit_version_without_data_loss() {
        let activity_at = std::time::Instant::now();
        let mut app = app_for_real_sidebar_fixtures(&["auth"]);
        app.agent_panel_sort = AgentPanelSort::Priority;
        configure_real_sidebar_agent(
            &mut app,
            0,
            Agent::Claude,
            "claude",
            "2.1.245",
            "Repair OAuth prompt",
            AgentState::Blocked,
            activity_at,
        );
        app.view_observed_at = activity_at + std::time::Duration::from_secs(15 * 60);

        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        assert_eq!(
            app.terminals[&terminal_id]
                .effective_display_agent()
                .as_deref(),
            Some("2.1.245"),
            "detail and API source data must retain the reported version"
        );

        for width in [24, 60] {
            let area = Rect::new(0, 0, width, 12);
            let mut terminal = Terminal::new(TestBackend::new(width, 12)).unwrap();
            terminal
                .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
                .unwrap();
            let rendered = (0..area.height)
                .map(|row| row_text(terminal.backend().buffer(), row, width - 1))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(!rendered.contains("2.1.245"), "width {width}: {rendered:?}");
        }

        app.workspaces[0].tabs[0].custom_name = None;
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.terminal_title = None;
        let area = Rect::new(0, 0, 40, 12);
        let mut rendered = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        rendered
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let rows = (0..area.height)
            .map(|row| row_text(rendered.backend().buffer(), row, area.width - 1))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !rows.contains("2.1.245"),
            "display-agent fallback leaked the version: {rows:?}"
        );

        let mut suffixless = app_for_real_sidebar_fixtures(&["gemini"]);
        configure_real_sidebar_agent(
            &mut suffixless,
            0,
            Agent::Gemini,
            "gemini",
            "0.9.3",
            "temporary title",
            AgentState::Working,
            activity_at,
        );
        let pane_id = suffixless.workspaces[0].tabs[0].root_pane;
        let terminal_id = suffixless.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        suffixless.workspaces[0].tabs[0].custom_name = None;
        suffixless
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .terminal_title = None;
        let area = Rect::new(0, 0, 40, 12);
        let mut rendered = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        rendered
            .draw(|frame| render_sidebar(&suffixless, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let rows = (0..area.height)
            .map(|row| row_text(rendered.backend().buffer(), row, area.width - 1))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !rows.contains("0.9.3"),
            "suffixless version leaked: {rows:?}"
        );
        assert!(
            rows.contains("gemini"),
            "provider identity missing: {rows:?}"
        );

        suffixless.workspaces[0].tabs[0].custom_name = Some("2026.08.26".into());
        let area = Rect::new(0, 0, 60, 12);
        let mut rendered = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        rendered
            .draw(|frame| render_sidebar(&suffixless, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let rows = (0..area.height)
            .map(|row| row_text(rendered.backend().buffer(), row, area.width - 1))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            rows.contains("2026.08.26"),
            "semantic dotted title was hidden: {rows:?}"
        );
    }

    #[test]
    fn sidebar_agent_rows_suppress_redundant_identity() {
        let activity_at = std::time::Instant::now();
        let mut app = app_for_real_sidebar_fixtures(&["claude", "gemini"]);
        app.agent_panel_sort = AgentPanelSort::Priority;
        configure_real_sidebar_agent(
            &mut app,
            0,
            Agent::Claude,
            "ClAuDe",
            "2.1.245",
            "Approve Bash command",
            AgentState::Blocked,
            activity_at,
        );
        configure_real_sidebar_agent(
            &mut app,
            1,
            Agent::Gemini,
            "Gemini",
            "0.9.3",
            "Review release notes",
            AgentState::Working,
            activity_at,
        );
        app.view_observed_at = activity_at + std::time::Duration::from_secs(60);

        let identities = agent_panel_entries(&app)
            .into_iter()
            .map(|entry| {
                (
                    entry.primary_label.clone(),
                    entry.agent,
                    compact_agent_identity(
                        &entry,
                        entry
                            .primary_tab_label
                            .as_deref()
                            .unwrap_or(DEFAULT_THREAD_TITLE),
                    )
                    .map(str::to_string),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(identities[0].1, Some(Agent::Claude), "{identities:?}");
        assert_eq!(identities[0].2.as_deref(), Some("cc"), "{identities:?}");
        assert_eq!(identities[1].1, Some(Agent::Gemini), "{identities:?}");
        assert_eq!(identities[1].2.as_deref(), Some("gemini"), "{identities:?}");

        let area = Rect::new(0, 0, 60, 16);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let rendered = (0..area.height)
            .map(|row| row_text(terminal.backend().buffer(), row, area.width - 1))
            .collect::<Vec<_>>();
        let claude = rendered
            .iter()
            .find(|row| row.contains("Approve Bash command"))
            .expect("Claude row");
        assert!(claude.contains("cc"), "{claude:?}");
        let claude_row_without_space = claude.split(" · ").next().expect("row title and provider");
        assert!(
            !claude_row_without_space
                .to_ascii_lowercase()
                .contains("claude"),
            "{claude:?}"
        );
        let gemini = rendered
            .iter()
            .find(|row| row.contains("Review release notes"))
            .expect("Gemini row");
        let gemini_row_without_space = gemini.split(" · ").next().expect("row title and provider");
        assert!(!gemini_row_without_space.contains("gemini"), "{gemini:?}");
    }

    #[test]
    fn sidebar_real_fixture_rows_are_compact() {
        let activity_at = std::time::Instant::now();
        let mut app = app_for_real_sidebar_fixtures(&["claude-code", "codex"]);
        configure_real_sidebar_agent(
            &mut app,
            0,
            Agent::Claude,
            "claude",
            "2.1.237",
            "Native permission fixture",
            AgentState::Blocked,
            activity_at,
        );
        configure_real_sidebar_agent(
            &mut app,
            1,
            Agent::Codex,
            "codex",
            "0.42.0",
            "Cross-space navigation",
            AgentState::Working,
            activity_at,
        );
        app.view_observed_at = activity_at + std::time::Duration::from_secs(60);
        app.sidebar_group_mode = SidebarGroupMode::Repo;
        replace_tab_context(
            &mut app,
            0,
            0,
            crate::work_context::PaneWorkContext {
                repo: Some("herdr".into()),
                ..Default::default()
            },
            Default::default(),
        );
        replace_tab_context(
            &mut app,
            1,
            0,
            crate::work_context::PaneWorkContext {
                repo: Some("growth".into()),
                ..Default::default()
            },
            Default::default(),
        );

        for width in [26, 60] {
            let area = Rect::new(0, 0, width, 18);
            let mut terminal = Terminal::new(TestBackend::new(width, area.height)).unwrap();
            terminal
                .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
                .unwrap();
            for card in compute_tab_card_areas(&app, area) {
                let row = row_text(terminal.backend().buffer(), card.rect.y, width - 1);
                let suffix = if card.ws_idx == 0 { "cc" } else { "cx" };
                let title = if card.ws_idx == 0 {
                    "Native permission fixture"
                } else {
                    "Cross-space navigation"
                };
                let title_start = row
                    .find(title)
                    .or_else(|| row.find(title.chars().next().unwrap()))
                    .unwrap();
                let suffix_start = row.find(suffix).expect("agent suffix");
                assert!(title_start < suffix_start, "{row:?}");
                let space = if card.ws_idx == 0 {
                    "claude-code"
                } else {
                    "codex"
                };
                if width >= SIDEBAR_SPACE_SUFFIX_MIN_ROW_WIDTH as u16 {
                    let space_start = row
                        .rfind(&format!(" · {space}"))
                        .unwrap_or_else(|| panic!("Space suffix at width {width}: {row:?}"));
                    assert!(suffix_start < space_start, "{row:?}");
                } else {
                    assert!(!row.contains(&format!(" · {space}")), "{row:?}");
                }
                assert!(
                    !row.contains("2.1.237") && !row.contains("0.42.0"),
                    "{row:?}"
                );
                assert!(
                    !row.contains("reported") && !row.contains(" ago"),
                    "{row:?}"
                );
            }
        }
    }

    /// Every nested header on screen as `(line, glyph colour)`. The glyph sits
    /// right after the disclosure prefix, which is one cell narrower on a dim
    /// header because a header nobody can collapse shows no arrow.
    fn rendered_nested_headers(
        app: &mut AppState,
        width: u16,
        height: u16,
    ) -> Vec<(String, Option<Color>)> {
        let mut terminal =
            Terminal::new(TestBackend::new(width, height)).expect("test terminal for headers");
        crate::ui::compute_view(app, Rect::new(0, 0, width, height));
        terminal
            .draw(|frame| {
                render_sidebar(
                    app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    app.view.sidebar_rect,
                )
            })
            .expect("render sidebar");
        let buffer = terminal.backend().buffer().clone();
        compute_sidebar_nested_header_areas(app, app.view.sidebar_rect)
            .into_iter()
            .map(|header| {
                let glyph_x = header.rect.x + if header.dim { 3 } else { 4 };
                (
                    row_text(&buffer, header.rect.y, header.rect.width),
                    buffer[(glyph_x, header.rect.y)].style().fg,
                )
            })
            .collect()
    }

    /// One unworked ticket per state, so every Linear status class is on
    /// screen in the same render.
    fn linear_state_fixture(states: &[(&str, &str)]) -> AppState {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        let items = states
            .iter()
            .map(|(identifier, state)| {
                let mut ticket = work_ticket(identifier, "pixel EMQ drop", "matthias", &[]);
                ticket.state = Some((*state).to_string());
                work_item("scalable-so/110x", None, vec![ticket])
            })
            .collect();
        app.work_index_snapshot = Some(crate::work_index::Snapshot {
            items,
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: std::time::SystemTime::UNIX_EPOCH,
        });
        app.reconcile_sidebar_presentation();
        app
    }

    #[test]
    fn linear_group_headers_render_a_glyph_and_palette_colour_per_state() {
        let states = [
            ("OPS-1", "Backlog"),
            ("OPS-2", "Todo"),
            ("OPS-3", "In Progress"),
            ("OPS-4", "In Review"),
            ("OPS-5", "Done"),
            ("OPS-6", "Canceled"),
            ("OPS-7", "Triage"),
        ];
        let mut app = linear_state_fixture(&states);
        // The default Linear filter hides Canceled tickets (F12-5); this test
        // checks every state glyph, so widen the filter to all statuses.
        app.sidebar_work_filter.linear_statuses = crate::app::state::LinearStatusFilter::ALL
            .into_iter()
            .collect();
        let palette = app.palette.clone();
        let expected = [
            ("◌ OPS-1 · pixel", palette.work_status_neutral()),
            ("○ OPS-2 · pixel", palette.work_status_neutral()),
            ("◐ OPS-3 · pixel", palette.work_status_active()),
            ("◑ OPS-4 · pixel", palette.work_status_review()),
            ("● OPS-5 · pixel", palette.work_status_done()),
            ("⊗ OPS-6 · pixel", palette.work_status_neutral()),
            ("◍ OPS-7 · pixel", palette.work_status_triage()),
        ];

        let headers = rendered_nested_headers(&mut app, 120, 40);
        for (text, color) in expected {
            let header = headers
                .iter()
                .find(|(line, _)| line.contains(text))
                .unwrap_or_else(|| panic!("header {text:?} in {headers:?}"));
            assert_eq!(header.1, Some(color), "colour for {text:?}");
        }
    }

    /// The PR headers read their state from the work-index cache, which is the
    /// only place a pane's declared PR URL gains a state at all.
    fn pull_request_state_fixture(state: &str, draft: bool) -> AppState {
        let mut app = sidebar_grouping_fixture();
        app.set_sidebar_group_mode(SidebarGroupMode::RepoPr);
        let mut item = work_item("scalable-so/herdr", Some(159), Vec::new());
        item.pr_state = Some(state.to_string());
        item.draft = draft;
        app.work_index_enabled = true;
        app.work_index_snapshot = Some(crate::work_index::Snapshot {
            items: vec![item],
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: std::time::SystemTime::UNIX_EPOCH,
        });
        app.reconcile_sidebar_presentation();
        app
    }

    #[test]
    fn pull_request_group_headers_render_a_glyph_and_palette_colour_per_state() {
        for (state, draft, glyph, color) in [
            ("open", false, "○", Palette::catppuccin().work_status_open()),
            (
                "merged",
                false,
                "●",
                Palette::catppuccin().work_status_merged(),
            ),
            (
                "open",
                true,
                "◌",
                Palette::catppuccin().work_status_neutral(),
            ),
            (
                "closed",
                false,
                "⊗",
                Palette::catppuccin().work_status_neutral(),
            ),
        ] {
            let mut app = pull_request_state_fixture(state, draft);
            let headers = rendered_nested_headers(&mut app, 120, 40);
            let header = headers
                .iter()
                .find(|(line, _)| line.contains("#159 · pric"))
                .unwrap_or_else(|| panic!("#159 header in {headers:?}"));
            assert!(
                header.0.contains(&format!("{glyph} #159 · pric")),
                "{state} draft={draft}: {:?}",
                header.0
            );
            assert_eq!(header.1, Some(color), "colour for {state} draft={draft}");
        }
    }

    #[test]
    fn missive_group_headers_render_the_conversation_glyph_and_palette_colour() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::Missive;
        let expected = app.palette.work_status_open();

        let headers = rendered_nested_headers(&mut app, 120, 40);

        let header = headers
            .iter()
            .find(|(line, _)| line.contains("aaa111 · fix"))
            .unwrap_or_else(|| panic!("conversation header in {headers:?}"));
        assert!(header.0.contains("○ aaa111 · fix"), "{:?}", header.0);
        assert_eq!(header.1, Some(expected));
        // Nothing caches conversation state on this base, so no header may
        // claim a closed or unassigned conversation.
        assert!(
            headers.iter().all(|(line, _)| !line.contains('●')),
            "{headers:?}"
        );
    }

    #[test]
    fn missive_group_headers_use_indexed_subject_assignment_and_state() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::Missive;
        app.work_index_snapshot
            .as_mut()
            .expect("work index snapshot")
            .conversations = vec![
            crate::work_index::MissiveConversation {
                id: "aaa111".into(),
                subject: "Refund approved".into(),
                app_url: CONVERSATION_A.into(),
                web_url: CONVERSATION_A.into(),
                team: None,
                assignees: vec![crate::work_index::MissiveUser {
                    id: "mina".into(),
                    name: "Mina".into(),
                    email: None,
                    is_me: true,
                }],
                last_activity_at: None,
                closed: true,
                labels: Vec::new(),
                pane_bound: false,
                messages: Vec::new(),
                notes: Vec::new(),
                drafts: Vec::new(),
                posts: Vec::new(),
            },
            crate::work_index::MissiveConversation {
                id: "bbb222".into(),
                subject: "Needs owner".into(),
                app_url: CONVERSATION_B.into(),
                web_url: CONVERSATION_B.into(),
                team: None,
                assignees: vec![crate::work_index::MissiveUser {
                    id: "ada".into(),
                    name: "Ada".into(),
                    email: None,
                    is_me: false,
                }],
                last_activity_at: None,
                closed: false,
                labels: Vec::new(),
                pane_bound: false,
                messages: Vec::new(),
                notes: Vec::new(),
                drafts: Vec::new(),
                posts: Vec::new(),
            },
        ];

        app.work_index_session.missive.viewer = Some("Mina".into());
        assert_eq!(
            work_group_shape(&app),
            vec![
                ("aaa111 · Refund approved".to_string(), 1, false),
                ("bbb222 · Needs owner".to_string(), 1, false),
                (unlinked_bucket_title(), 2, false),
            ],
            "pane-linked conversations stay visible through sidebar filters"
        );
        app.sidebar_work_filter.missive.assignee = None;
        app.sidebar_work_filter.missive.show_closed = true;

        let headers = rendered_nested_headers(&mut app, 120, 40);
        let closed = headers
            .iter()
            .find(|(line, _)| line.contains("aaa111 · Ref"))
            .unwrap_or_else(|| panic!("indexed closed conversation header in {headers:?}"));
        assert!(closed.0.contains("● aaa111"), "{:?}", closed.0);
        assert_eq!(closed.1, Some(app.palette.work_status_done()));
        let open = headers
            .iter()
            .find(|(line, _)| line.contains("bbb222 · Nee"))
            .expect("indexed open conversation header");
        assert!(open.0.contains("○ bbb222"), "{:?}", open.0);
        assert_eq!(open.1, Some(app.palette.work_status_open()));
    }

    #[test]
    fn narrow_group_headers_truncate_the_title_and_keep_the_glyph_and_id() {
        let mut app = linear_state_fixture(&[("OPS-3", "In Progress")]);
        app.dock_width = 26;

        let headers = rendered_nested_headers(&mut app, 80, 24);

        let header = headers
            .iter()
            .find(|(line, _)| line.contains("OPS-3"))
            .unwrap_or_else(|| panic!("ticket header in {headers:?}"));
        assert!(header.0.starts_with("   ◐ OPS-3 · pixel"), "{:?}", header.0);
        assert!(header.0.ends_with("… + …"), "{:?}", header.0);
        assert!(
            crate::ui::text::display_width(&header.0) <= 26,
            "{:?}",
            header.0
        );
        assert_eq!(header.1, Some(app.palette.work_status_active()));
    }

    fn visible_sidebar_panes(
        app: &AppState,
    ) -> std::collections::BTreeSet<(usize, crate::layout::PaneId)> {
        sidebar_rows(app)
            .into_iter()
            .filter_map(|row| match row {
                SidebarRow::Tab { entry, .. } | SidebarRow::Agent { entry, .. } => {
                    Some((entry.ws_idx, entry.pane_id))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn sidebar_search_matches_the_same_pane_in_every_view_and_settled() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_work_filter.query = "addendum".into();
        let target = app.workspaces[0].tabs[1].root_pane;
        let expected = std::collections::BTreeSet::from([(0, target)]);

        for mode in SidebarGroupMode::ALL {
            app.sidebar_group_mode = mode;
            assert_eq!(visible_sidebar_panes(&app), expected, "view {mode:?}");
        }

        app.workspaces[0].tabs[1]
            .panes
            .get_mut(&target)
            .expect("query target pane")
            .settled_at = Some(1_725_000_000);
        for mode in SidebarGroupMode::ALL {
            app.sidebar_group_mode = mode;
            assert_eq!(visible_sidebar_panes(&app), expected, "settled {mode:?}");
        }

        app.sidebar_work_filter.query = "does-not-exist".into();
        assert!(sidebar_rows(&app).is_empty(), "empty headers must collapse");
    }

    #[test]
    fn sidebar_label_query_filters_linear_and_missive_sources() {
        let mut app = sidebar_work_item_fixture();
        app.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        app.sidebar_work_filter.query = "label:p1".into();
        assert_eq!(
            work_group_shape(&app)
                .into_iter()
                .map(|(title, ..)| title)
                .collect::<Vec<_>>(),
            vec!["SCA-3102 · annual credits"]
        );

        let mut billing = missive_conversation("aaa111", "Billing", CONVERSATION_A);
        billing.labels = vec!["billing".into(), "customer".into()];
        let mut support = missive_conversation("bbb222", "Support", CONVERSATION_B);
        support.labels = vec!["support".into()];
        app.work_index_snapshot
            .as_mut()
            .expect("work index snapshot")
            .conversations = vec![billing, support];
        app.sidebar_group_mode = SidebarGroupMode::Missive;
        app.sidebar_work_filter.missive.assignee = None;
        app.sidebar_work_filter.query = "label:billing".into();
        assert_eq!(
            work_group_shape(&app)
                .into_iter()
                .map(|(title, ..)| title)
                .collect::<Vec<_>>(),
            vec!["aaa111 · Billing"]
        );

        app.sidebar_work_filter.query = "label:unknown".into();
        assert!(visible_sidebar_panes(&app).is_empty());
        assert!(work_group_shape(&app).is_empty());

        let area = Rect::new(0, 0, 40, 12);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("label empty-state terminal");
        terminal
            .draw(|frame| {
                render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area);
            })
            .expect("render label empty state");
        let rendered = (0..area.height)
            .map(|row| row_text(terminal.backend().buffer(), row, area.width))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            rendered.contains("no open conversations for anyone"),
            "{rendered}"
        );
        assert!(!rendered.contains("Spaces (0)agents"), "{rendered}");
    }

    #[test]
    fn sidebar_header_controls_keep_hit_areas_and_menus_open_downward() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app = AppState::test_new();
        let mut alpha = Workspace::test_new("alpha");
        alpha.identity_cwd = std::path::PathBuf::from("/tmp/t3-10a/alpha");
        let mut beta = Workspace::test_new("beta");
        beta.identity_cwd = std::path::PathBuf::from("/tmp/t3-10a/beta");
        app.workspaces = vec![alpha, beta];
        let area = Rect::new(0, 0, 120, 40);
        crate::ui::compute_view(&mut app, area);

        let edit = sidebar_header_new_thread_rect(app.view.sidebar_rect);
        let add = sidebar_header_new_menu_rect(app.view.sidebar_rect);
        assert_eq!((edit.width, add.width), (2, 2));
        assert_eq!(edit.right().saturating_add(1), add.x);

        app.open_sidebar_new_menu();
        let new_layout = sidebar_new_menu_layout(&app, area).expect("new dropdown");
        assert_eq!(new_layout.rect.y, add.bottom());
        assert_eq!(
            crate::app::state::SidebarNewMenuAction::ALL
                .iter()
                .map(|action| action.label())
                .collect::<Vec<_>>(),
            vec!["New space", "Add project…", "New thread", "Open folder…"]
        );

        app.open_sidebar_new_thread();
        let layout = sidebar_new_thread_layout(&app, area).expect("recent-project dropdown");
        assert_eq!(layout.rect.y, edit.bottom());
        assert!(layout.rect.bottom() <= area.bottom());
        app.sidebar_new_thread
            .as_mut()
            .expect("recent-project picker")
            .filter
            .set_query("alpha");
        assert_eq!(
            sidebar_new_thread_matches(&app)
                .into_iter()
                .map(|(_, label)| label)
                .collect::<Vec<_>>(),
            vec!["📁 alpha  /tmp/t3-10a/alpha"]
        );
        assert!(app.handle_sidebar_new_thread_key(KeyEvent::new(
            KeyCode::Char('1'),
            KeyModifiers::empty(),
        )));
        assert_eq!(
            app.home.as_ref().map(|home| home.directory.as_path()),
            Some(std::path::Path::new("/tmp/t3-10a/alpha"))
        );

        app.open_sidebar_new_thread();
        assert!(
            app.handle_sidebar_new_thread_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty(),))
        );
        assert!(app.sidebar_new_thread.is_none());
    }

    #[test]
    fn new_thread_picker_offers_configured_projects_and_filters_by_project_name() {
        use crate::app::projects::{Project, ProjectRepo};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app = AppState::test_new();
        let mut space = Workspace::test_new("worktree-space");
        space.identity_cwd = std::path::PathBuf::from("/tmp/t3-projects/herdr-improve");
        app.workspaces = vec![space];
        app.projects = vec![
            Project {
                id: "scalable".into(),
                label: "scalable".into(),
                repos: vec![ProjectRepo {
                    name: "110x".into(),
                    path: "/tmp/t3-projects/110x".into(),
                }],
            },
            Project {
                id: "personal".into(),
                label: "personal".into(),
                repos: vec![ProjectRepo {
                    name: "agent-box-bootstrap".into(),
                    path: "/tmp/t3-projects/agent-box-bootstrap".into(),
                }],
            },
        ];

        app.open_sidebar_new_thread();
        let labels = sidebar_new_thread_matches(&app)
            .into_iter()
            .map(|(_, label)| label)
            .collect::<Vec<_>>();
        assert_eq!(
            labels.first().map(String::as_str),
            Some("📁 110x  scalable · /tmp/t3-projects/110x"),
            "a configured project repo leads the picker: {labels:?}"
        );
        assert!(
            labels
                .iter()
                .any(|label| label.contains("/tmp/t3-projects/herdr-improve")),
            "a checkout no project scans stays reachable: {labels:?}"
        );

        app.sidebar_new_thread
            .as_mut()
            .expect("project picker")
            .filter
            .set_query("personal");
        assert_eq!(
            sidebar_new_thread_matches(&app)
                .into_iter()
                .map(|(_, label)| label)
                .collect::<Vec<_>>(),
            vec!["📁 agent-box-bootstrap  personal · /tmp/t3-projects/agent-box-bootstrap"],
            "typing a project name narrows the list to that project"
        );

        assert!(app
            .handle_sidebar_new_thread_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty(),)));
        assert_eq!(
            app.home.as_ref().map(|home| home.directory.as_path()),
            Some(std::path::Path::new("/tmp/t3-projects/agent-box-bootstrap")),
            "the picker spawns into the selected project, not the filtered position"
        );
    }

    #[test]
    fn sidebar_new_menu_dispatches_all_four_actions() {
        use crate::app::home::HomePicker;
        use crate::app::state::SidebarNewMenuAction;

        let mut app = AppState::test_new();
        app.dispatch_sidebar_new_menu_action(SidebarNewMenuAction::NewSpace);
        assert!(app.request_new_workspace);

        app.request_new_workspace = false;
        app.dispatch_sidebar_new_menu_action(SidebarNewMenuAction::AddProject);
        assert!(app.add_project_active());

        app.clear_home();
        app.dispatch_sidebar_new_menu_action(SidebarNewMenuAction::NewThread);
        assert!(app.sidebar_new_thread.is_some());

        app.sidebar_new_thread = None;
        app.dispatch_sidebar_new_menu_action(SidebarNewMenuAction::OpenFolder);
        let home = app.home.as_ref().expect("folder browser home");
        assert_eq!(home.picker, Some(HomePicker::Directory));
        assert!(home.browse.is_some());
    }

    #[test]
    fn sidebar_add_project_reuses_the_existing_modal() {
        let mut app = AppState::test_new();
        app.open_add_project_from_sidebar();
        assert!(app.add_project_active());
        assert!(app
            .home
            .as_ref()
            .and_then(|home| home.add_project.as_ref())
            .is_some());
    }

    fn symphony_workflow(name: &str) -> crate::symphony::Workflow {
        crate::symphony::Workflow {
            workflow_id: format!("wf-{name}"),
            run_id: format!("run-{name}"),
            name: name.to_string(),
            phase: "runFlowStep".to_string(),
            wait: None,
            started_at: None,
            ticket: None,
            repo: None,
            pr: None,
            receipts: None,
        }
    }

    fn app_with_symphony(names: &[&str]) -> AppState {
        let mut app = app_with_agents(&["one"]);
        app.symphony_snapshot = crate::symphony::Snapshot {
            workflows: names.iter().copied().map(symphony_workflow).collect(),
            unavailable: None,
            polled: true,
        };
        app
    }

    #[test]
    fn symphony_section_is_absent_without_open_workflows() {
        let app = app_with_agents(&["one"]);
        assert!(!sidebar_rows(&app)
            .iter()
            .any(|row| matches!(row, SidebarRow::SectionHeader { title, .. }
                if *title == SYMPHONY_SECTION_TITLE)));

        // An unreachable runtime is reported by the Symphony window, not by a
        // permanent error row in the sidebar.
        let mut unavailable = app;
        unavailable.symphony_snapshot = crate::symphony::Snapshot {
            workflows: Vec::new(),
            unavailable: Some("Temporal runtime is unreachable".to_string()),
            polled: true,
        };
        let rows = sidebar_rows(&unavailable);
        assert!(!rows
            .iter()
            .any(|row| matches!(row, SidebarRow::SymphonyJob { .. })));
        assert!(!rows
            .iter()
            .any(|row| matches!(row, SidebarRow::SymphonyEmpty)));
        assert!(!rows
            .iter()
            .any(|row| matches!(row, SidebarRow::SectionHeader { title, .. }
            if *title == SYMPHONY_SECTION_TITLE)));
    }

    #[test]
    fn symphony_section_says_it_is_empty_once_the_runner_answers() {
        // A section that vanishes when empty cannot be told apart from one that
        // is broken, so a reachable runner keeps its header and says so.
        let mut app = app_with_agents(&["one"]);
        app.symphony_snapshot = crate::symphony::Snapshot {
            workflows: Vec::new(),
            unavailable: None,
            polled: true,
        };
        let rows = sidebar_rows(&app);
        let header = rows.iter().position(|row| {
            matches!(row, SidebarRow::SectionHeader { title, count, .. }
            if *title == SYMPHONY_SECTION_TITLE && *count == 0)
        });
        let header = header.expect("reachable runner keeps its header at zero jobs");
        assert!(matches!(
            rows.get(header + 1),
            Some(SidebarRow::SymphonyEmpty)
        ));

        // The placeholder owns no pane, so like the headers it must never become
        // a focusable card.
        let area = Rect::new(0, 0, 40, 24);
        assert!(compute_symphony_job_areas(&app, area).is_empty());
        assert!(compute_symphony_areas(&app, area).1.is_some());

        // Collapsing hides the placeholder with everything else.
        app.collapsed_sidebar_groups.insert(format!(
            "{}:{SYMPHONY_SECTION_TITLE}",
            app.sidebar_group_mode.collapse_namespace()
        ));
        assert!(!sidebar_rows(&app)
            .iter()
            .any(|row| matches!(row, SidebarRow::SymphonyEmpty)));
    }

    #[test]
    fn symphony_section_lists_open_workflows_and_collapses() {
        let mut app = app_with_symphony(&["blocker dashboard", "docs sync"]);
        let rows = sidebar_rows(&app);
        let position = |wanted: &str| {
            rows.iter().position(
                |row| matches!(row, SidebarRow::SectionHeader { title, .. } if *title == wanted),
            )
        };
        let symphony = position(SYMPHONY_SECTION_TITLE).expect("symphony section");
        assert!(
            position(SPACES_SECTION_TITLE).is_some_and(|spaces| spaces < symphony),
            "symphony belongs under the spaces list"
        );
        assert!(matches!(
            rows.get(symphony),
            Some(SidebarRow::SectionHeader {
                count: 2,
                collapsed: false,
                ..
            })
        ));
        assert_eq!(
            rows.iter()
                .filter_map(|row| match row {
                    SidebarRow::SymphonyJob { index, name, .. } => Some((*index, name.clone())),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec![
                (0, "blocker dashboard".to_string()),
                (1, "docs sync".to_string())
            ]
        );

        app.toggle_sidebar_group(SYMPHONY_SECTION_TITLE);
        let rows = sidebar_rows(&app);
        assert!(!rows
            .iter()
            .any(|row| matches!(row, SidebarRow::SymphonyJob { .. })));
        assert!(rows.iter().any(|row| matches!(
            row,
            SidebarRow::SectionHeader { title, collapsed: true, .. }
                if *title == SYMPHONY_SECTION_TITLE
        )));
    }

    #[test]
    fn symphony_row_dot_follows_the_agent_vocabulary_for_named_waits() {
        let mut app = app_with_symphony(&["sync"]);
        app.symphony_snapshot.workflows[0].wait = Some("plan-sign-off".to_string());
        let area = Rect::new(0, 0, 40, 20);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let row = (0..area.height)
            .find(|row| row_text(buffer, *row, area.width - 1).contains("sync"))
            .expect("symphony row");
        let dot = (0..area.width)
            .find_map(|column| {
                let cell = buffer.cell((column, row))?;
                (cell.symbol() == "\u{25cb}").then(|| cell.clone())
            })
            .unwrap_or_else(|| panic!("{:?}", row_text(buffer, row, area.width - 1)));
        assert_eq!(
            dot.fg, app.palette.red,
            "a named wait owes a human an answer"
        );
    }

    #[test]
    fn symphony_rows_render_name_and_phase_and_never_become_cards() {
        let app = app_with_symphony(&["blocker dash"]);
        let area = Rect::new(0, 0, 40, 20);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let text = (0..area.height)
            .map(|row| row_text(terminal.backend().buffer(), row, area.width - 1))
            .collect::<Vec<_>>();
        assert!(
            text.iter()
                .any(|line| line.contains(SYMPHONY_SECTION_TITLE)),
            "{text:?}"
        );
        let job_line = text
            .iter()
            .find(|line| line.contains("blocker"))
            .unwrap_or_else(|| panic!("{text:?}"));
        // The status column is the agent row's provider column, so a long phase
        // is truncated to it rather than pushing the age out of alignment.
        assert!(job_line.contains("runFlowStep"), "{job_line:?}");
        // Dot, title, status and age land in the agent row's columns.
        let agent_line = text
            .iter()
            .find(|line| line.contains("pi"))
            .unwrap_or_else(|| panic!("{text:?}"));
        assert_eq!(
            job_line.find('\u{25cf}'),
            agent_line.find('\u{25cf}'),
            "{job_line:?} vs {agent_line:?}"
        );
        // Same dot vocabulary as an agent row: running is a filled dot.
        assert!(job_line.contains('\u{25cf}'), "{job_line:?}");

        // A workflow owns no pane, so it must stay out of every focusable list.
        let job_row = compute_symphony_job_areas(&app, area)
            .first()
            .cloned()
            .expect("symphony job area");
        assert!(compute_agent_card_areas(&app, area)
            .iter()
            .all(|card| card.rect.y != job_row.rect.y));
        assert!(compute_tab_card_areas(&app, area)
            .iter()
            .all(|card| card.rect.y != job_row.rect.y));
    }

    fn test_nested_header(
        title: &str,
        status: Option<WorkGroupStatus>,
        width: u16,
    ) -> NestedHeaderArea {
        NestedHeaderArea {
            key: "key".into(),
            action_key: Some("key".into()),
            title: title.into(),
            count: 2,
            collapsed: false,
            dim: false,
            status,
            spawn: false,
            rect: Rect::new(0, 0, width, 1),
        }
    }

    #[test]
    fn nested_header_spans_place_the_glyph_before_the_title() {
        let header = test_nested_header(
            "SCA-1 \u{b7} short",
            Some(WorkGroupStatus::TicketInProgress),
            60,
        );
        let spans = nested_header_spans(&header);

        assert_eq!(spans.prefix_width, 4);
        assert_eq!(spans.glyph, Some("\u{25d0}"));
        assert_eq!(spans.glyph_width, 2);
        assert_eq!(spans.title, header.title);
        assert!(!spans.title_truncated);
    }

    #[test]
    fn a_narrow_header_reports_its_title_as_truncated() {
        let header = test_nested_header(
            "SCA-3296 \u{b7} enhance text tool to preserve stored ratio",
            Some(WorkGroupStatus::TicketInReview),
            24,
        );
        let spans = nested_header_spans(&header);

        assert!(spans.title_truncated);
        assert!(display_width(&spans.title) < display_width(&header.title));
    }

    #[test]
    fn agent_dot_tooltip_says_what_the_dot_means() {
        let mut entry = compact_test_entry("task", Some(Agent::Claude));
        entry.state = AgentState::Working;
        assert_eq!(agent_dot_tooltip(&entry), "Working");

        entry.state = AgentState::Blocked;
        assert_eq!(agent_dot_tooltip(&entry), "Blocked, waiting on you");

        // A gate on a working pane does not steal the working label; a gate on
        // a stopped pane is the thing blocking it.
        entry.state = AgentState::Working;
        entry.open_blockers = true;
        assert_eq!(agent_dot_tooltip(&entry), "Working");
        entry.state = AgentState::Idle;
        assert_eq!(agent_dot_tooltip(&entry), "Blocked, waiting on you");

        // A usage limit outranks every lifecycle label.
        entry.usage_limited = true;
        assert_eq!(agent_dot_tooltip(&entry), "Usage limit");

        // A pane that renamed its own status is quoted, not overridden.
        entry
            .state_labels
            .insert("usage".into(), "resets 14:00".into());
        assert_eq!(agent_dot_tooltip(&entry), "resets 14:00");

        let shell = compact_test_entry("terminal", None);
        assert_eq!(agent_dot_tooltip(&shell), "No agent");
    }

    #[test]
    fn hover_targets_anchor_on_the_agent_dot_a_row_actually_drew() {
        let app = app_with_agents(&["alpha"]);
        let area = Rect::new(0, 0, 32, 20);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();

        let card = compute_tab_card_areas(&app, area)[0].clone();
        let target = compute_sidebar_hover_targets(&app, area)
            .into_iter()
            .find(|target| target.rect.y == card.rect.y)
            .expect("dot target on the agent row");

        assert_eq!(target.label, "Working");
        assert_eq!(target.rect.height, 1);
        // The anchor sits on the dot the row drew, not on its left edge.
        let rendered = row_text(terminal.backend().buffer(), card.rect.y, card.rect.width);
        assert_eq!(
            rendered.chars().nth(usize::from(target.rect.x)),
            Some('\u{25cf}'),
            "{rendered:?}"
        );
    }

    #[test]
    fn a_truncated_work_header_offers_its_full_title_on_hover() {
        let header = test_nested_header(
            "SCA-3296 \u{b7} enhance text tool to preserve stored ratio",
            Some(WorkGroupStatus::TicketInReview),
            24,
        );
        let spans = nested_header_spans(&header);
        let glyph = clamp_row_cells(header.rect, header.rect.y, spans.prefix_width, 1)
            .expect("glyph anchor");
        let title = clamp_row_cells(
            header.rect,
            header.rect.y,
            spans.prefix_width + spans.glyph_width,
            display_width(&spans.title),
        )
        .expect("title anchor");

        // The two anchors explain different things and must not overlap.
        assert!(glyph.right() <= title.x);
        assert!(title.right() <= header.rect.right());
        assert_eq!(WorkGroupStatus::TicketInReview.label(), "In Review");
    }

    #[test]
    fn a_row_span_past_the_right_edge_has_no_anchor() {
        let body = Rect::new(0, 0, 10, 3);

        assert!(clamp_row_cells(body, 0, 10, 3).is_none());
        assert!(clamp_row_cells(body, 0, 0, 0).is_none());
        assert!(clamp_row_cells(body, 9, 0, 3).is_none());
        assert_eq!(
            clamp_row_cells(body, 0, 8, 3),
            Some(Rect::new(8, 0, 2, 1)),
            "a span is clipped to the body, never past it"
        );
    }
}
