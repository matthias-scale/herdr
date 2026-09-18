//! The notepad's agent tab: the focused pane's server-owned agent state as a
//! read-only list of collapsible Status, Tasks, Subagents and Links sections.
//!
//! Rows are derived once per frame in view computation and stored on the view
//! (`ViewState::notepad_agent_rows`), so the text the operator reads and the
//! row a click resolves to can never disagree. Render only slices them.

use std::time::SystemTime;

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::text::{display_width, display_width_u16, truncate_end};
use crate::agent_state::{AgentLink, AgentStateSnapshot, AgentTaskStatus};
use crate::api::schema::AgentStatus;
use crate::app::state::{AppState, Palette};
use crate::notepad::AgentSection;

/// The header label of the agent tab, shown after the note names.
pub(crate) const TAB_LABEL: &str = "agent";

/// What a click on an agent-tab body row does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NotepadAgentAction {
    None,
    ToggleSection(AgentSection),
    /// The subagent's own pane by public id. `None` is a native Claude
    /// subagent, which lives inside the pane the tab is showing.
    FocusSubagent(Option<String>),
    /// Copy the link's full URL; the row only shows a short label.
    CopyLink(String),
}

/// One body row of the agent tab.
#[derive(Debug, Clone)]
pub(crate) struct NotepadAgentRow {
    pub(crate) line: Line<'static>,
    pub(crate) action: NotepadAgentAction,
    /// Link rows only: the label's column offset inside the body and its drawn
    /// text, so the OSC 8 cells can be derived without re-laying out the row.
    pub(crate) link_label: Option<(u16, String)>,
}

impl NotepadAgentRow {
    fn plain(line: Line<'static>) -> Self {
        Self {
            line,
            action: NotepadAgentAction::None,
            link_label: None,
        }
    }
}

/// The focused pane's snapshot, or `None` when that pane runs no agent.
fn focused_agent_snapshot(app: &AppState) -> Option<AgentStateSnapshot> {
    let workspace = app.active.and_then(|index| app.workspaces.get(index))?;
    let pane_id = workspace.focused_pane_id()?;
    let terminal = app.terminals.get(workspace.terminal_id(pane_id)?)?;
    if !terminal.is_agent_terminal() {
        return None;
    }
    let status = workspace
        .pane_state(pane_id)
        .map(|pane| {
            crate::app::pane_agent_status_with_stale(
                terminal.raw_agent_state(),
                pane.seen,
                terminal.supervisor_stale,
            )
        })
        .unwrap_or(AgentStatus::Unknown);
    Some(app.agent_states.snapshot(pane_id, status))
}

pub(crate) fn status_label(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Idle => "idle",
        AgentStatus::Working => "working",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Stale => "stale",
        AgentStatus::Unknown => "unknown",
    }
}

/// Ages come from the server-owned wall clock snapshot; render never reads
/// the clock itself.
fn age_label(now_unix: Option<i64>, rfc3339: &str) -> Option<String> {
    let now = now_unix?;
    let then = crate::agent_state::parse_rfc3339(rfc3339)?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?;
    let seconds = (now - then.as_secs() as i64).max(0) as u64;
    Some(if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 60 * 60 {
        format!("{}m", seconds / 60)
    } else if seconds < 24 * 60 * 60 {
        format!("{}h", seconds / (60 * 60))
    } else {
        format!("{}d", (seconds / (24 * 60 * 60)).min(999))
    })
}

fn section_header(
    palette: &Palette,
    section: AgentSection,
    title: String,
    collapsed: bool,
    value: Option<String>,
    width: u16,
) -> NotepadAgentRow {
    let glyph = if collapsed { "▸" } else { "▾" };
    let title = format!("{glyph} {title}");
    let title_width = display_width(&title);
    let mut spans = vec![Span::styled(
        title,
        Style::default()
            .fg(palette.text)
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(value) = value {
        let pad = usize::from(width).saturating_sub(title_width + display_width(&value));
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(value, Style::default().fg(palette.overlay0)));
    }
    NotepadAgentRow {
        line: Line::from(spans),
        action: NotepadAgentAction::ToggleSection(section),
        link_label: None,
    }
}

fn none_row(palette: &Palette, width: u16) -> NotepadAgentRow {
    NotepadAgentRow::plain(Line::from(Span::styled(
        truncate_end("  none", usize::from(width)),
        Style::default().fg(palette.overlay0),
    )))
}

fn push_status_rows(
    rows: &mut Vec<NotepadAgentRow>,
    palette: &Palette,
    snapshot: &AgentStateSnapshot,
    collapsed: bool,
    now_unix: Option<i64>,
    width: u16,
) {
    let mut value = status_label(snapshot.status).to_string();
    if let Some(age) = snapshot
        .last_acted_at
        .as_deref()
        .and_then(|at| age_label(now_unix, at))
    {
        value = format!("{value} · {age} ago");
    }
    rows.push(section_header(
        palette,
        AgentSection::Status,
        "Status".to_string(),
        collapsed,
        Some(value),
        width,
    ));
    if collapsed {
        return;
    }
    if let Some(goal) = &snapshot.goal {
        let budget = usize::from(width).saturating_sub(8);
        rows.push(NotepadAgentRow::plain(Line::from(vec![
            Span::styled("  goal: ", Style::default().fg(palette.overlay0)),
            Span::styled(
                truncate_end(goal, budget),
                Style::default().fg(palette.text),
            ),
        ])));
    }
    if let Some(text) = &snapshot.status_text {
        rows.push(NotepadAgentRow::plain(Line::from(Span::styled(
            truncate_end(&format!("  {text}"), usize::from(width)),
            Style::default().fg(palette.overlay1),
        ))));
    }
}

fn push_task_rows(
    rows: &mut Vec<NotepadAgentRow>,
    palette: &Palette,
    snapshot: &AgentStateSnapshot,
    collapsed: bool,
    width: u16,
) {
    rows.push(section_header(
        palette,
        AgentSection::Tasks,
        "Tasks".to_string(),
        collapsed,
        None,
        width,
    ));
    if collapsed {
        return;
    }
    if snapshot.tasks.is_empty() {
        rows.push(none_row(palette, width));
        return;
    }
    let budget = usize::from(width).saturating_sub(4);
    for task in &snapshot.tasks {
        let (glyph, glyph_color) = match task.status {
            AgentTaskStatus::Completed => ("✓", palette.green),
            AgentTaskStatus::InProgress => ("▶", palette.yellow),
            AgentTaskStatus::Pending => ("○", palette.overlay0),
        };
        rows.push(NotepadAgentRow::plain(Line::from(vec![
            Span::raw("  "),
            Span::styled(glyph, Style::default().fg(glyph_color)),
            Span::raw(" "),
            Span::styled(
                truncate_end(&task.text, budget),
                Style::default().fg(palette.text),
            ),
        ])));
    }
}

fn push_subagent_rows(
    rows: &mut Vec<NotepadAgentRow>,
    palette: &Palette,
    snapshot: &AgentStateSnapshot,
    collapsed: bool,
    now_unix: Option<i64>,
    width: u16,
) {
    rows.push(section_header(
        palette,
        AgentSection::Subagents,
        format!("Subagents ({})", snapshot.subagents.len()),
        collapsed,
        None,
        width,
    ));
    if collapsed {
        return;
    }
    if snapshot.subagents.is_empty() {
        rows.push(none_row(palette, width));
        return;
    }
    for subagent in &snapshot.subagents {
        let mut meta = status_label(subagent.status).to_string();
        if let Some(age) = subagent
            .last_active_at
            .as_deref()
            .and_then(|at| age_label(now_unix, at))
        {
            meta = format!("{meta} · {age} ago");
        }
        let meta_text = format!(" · {meta}");
        let budget = usize::from(width).saturating_sub(2 + display_width(&meta_text));
        rows.push(NotepadAgentRow {
            line: Line::from(vec![
                Span::styled(
                    truncate_end(&format!("  {}", subagent.name), budget.saturating_add(2)),
                    Style::default().fg(palette.text),
                ),
                Span::styled(meta_text, Style::default().fg(palette.overlay0)),
            ]),
            action: NotepadAgentAction::FocusSubagent(subagent.pane_id.clone()),
            link_label: None,
        });
    }
}

/// The row label: `pull/N` for a GitHub pull request, the ticket key for a
/// Linear issue, otherwise the URL path. The full URL stays on the row as the
/// click target and OSC 8 hyperlink, so the label never has to wrap.
fn link_label(link: &AgentLink) -> String {
    let path = link
        .url
        .split_once("://")
        .and_then(|(_, rest)| rest.split_once('/').map(|(_, path)| path))
        .unwrap_or("");
    let segments: Vec<&str> = path.split('/').collect();
    if link.domain.ends_with("github.com") {
        if let Some(pos) = segments.iter().position(|segment| *segment == "pull") {
            if let Some(number) = segments.get(pos + 1) {
                let number = number.split(['?', '#']).next().unwrap_or(number);
                if !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()) {
                    return format!("pull/{number}");
                }
            }
        }
    }
    if link.domain.ends_with("linear.app") {
        if let Some(pos) = segments.iter().position(|segment| *segment == "issue") {
            if let Some(key) = segments.get(pos + 1) {
                let key = key.split(['?', '#']).next().unwrap_or(key);
                if !key.is_empty() {
                    return key.to_string();
                }
            }
        }
    }
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        link.domain.clone()
    } else {
        trimmed.to_string()
    }
}

fn push_link_rows(
    rows: &mut Vec<NotepadAgentRow>,
    palette: &Palette,
    snapshot: &AgentStateSnapshot,
    collapsed: bool,
    now_unix: Option<i64>,
    width: u16,
) {
    rows.push(section_header(
        palette,
        AgentSection::Links,
        "Links".to_string(),
        collapsed,
        None,
        width,
    ));
    if collapsed {
        return;
    }
    if snapshot.links.is_empty() {
        rows.push(none_row(palette, width));
        return;
    }
    // Newest last_seen first, both between groups and inside one. The RFC3339
    // strings are compared as parsed timestamps: fractional digits make a
    // lexical order wrong.
    let mut links: Vec<(&AgentLink, SystemTime)> = snapshot
        .links
        .iter()
        .filter_map(|link| {
            crate::agent_state::parse_rfc3339(&link.last_seen).map(|seen| (link, seen))
        })
        .collect();
    links.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| left.0.url.cmp(&right.0.url))
    });
    let mut by_domain: std::collections::BTreeMap<&str, Vec<(&AgentLink, SystemTime)>> =
        std::collections::BTreeMap::new();
    for link in links {
        by_domain
            .entry(link.0.domain.as_str())
            .or_default()
            .push(link);
    }
    let mut groups: Vec<_> = by_domain.into_iter().collect();
    groups.sort_by(|left, right| {
        let newest = |group: &Vec<(&AgentLink, SystemTime)>| group.first().map(|link| link.1);
        newest(&right.1)
            .cmp(&newest(&left.1))
            .then_with(|| left.0.cmp(right.0))
    });
    for (domain, group) in groups {
        rows.push(NotepadAgentRow::plain(Line::from(Span::styled(
            truncate_end(&format!("  {domain}"), usize::from(width)),
            Style::default().fg(palette.overlay0),
        ))));
        for (link, _) in group {
            let age = age_label(now_unix, &link.last_seen).unwrap_or_default();
            let age_width = display_width(&age);
            let budget = usize::from(width).saturating_sub(4 + 1 + age_width);
            let label = truncate_end(&link_label(link), budget);
            let label_width = display_width(&label);
            let pad = usize::from(width).saturating_sub(4 + label_width + 1 + age_width);
            rows.push(NotepadAgentRow {
                line: Line::from(vec![
                    Span::raw("    "),
                    Span::styled(label.clone(), Style::default().fg(palette.text)),
                    Span::raw(format!("{} ", " ".repeat(pad))),
                    Span::styled(age, Style::default().fg(palette.overlay0)),
                ]),
                action: NotepadAgentAction::CopyLink(link.url.clone()),
                link_label: Some((4, label)),
            });
        }
    }
}

/// All body rows of the agent tab for the current focus, truncated to `width`.
pub(crate) fn agent_rows(app: &AppState, width: u16) -> Vec<NotepadAgentRow> {
    let palette = &app.palette;
    let Some(snapshot) = focused_agent_snapshot(app) else {
        return vec![NotepadAgentRow::plain(Line::from(Span::styled(
            truncate_end("no agent in this pane", usize::from(width)),
            Style::default().fg(palette.overlay0),
        )))];
    };
    let now_unix = app.status_now_unix;
    let collapsed = &app.notepad.agent_collapsed;
    let mut rows = Vec::new();
    push_status_rows(
        &mut rows,
        palette,
        &snapshot,
        collapsed.collapsed(AgentSection::Status),
        now_unix,
        width,
    );
    push_task_rows(
        &mut rows,
        palette,
        &snapshot,
        collapsed.collapsed(AgentSection::Tasks),
        width,
    );
    push_subagent_rows(
        &mut rows,
        palette,
        &snapshot,
        collapsed.collapsed(AgentSection::Subagents),
        now_unix,
        width,
    );
    push_link_rows(
        &mut rows,
        palette,
        &snapshot,
        collapsed.collapsed(AgentSection::Links),
        now_unix,
        width,
    );
    rows
}

pub(crate) fn render_agent_body(app: &AppState, frame: &mut Frame, body: Rect) {
    let visible = usize::from(body.height);
    for (offset, row) in app
        .view
        .notepad_agent_rows
        .iter()
        .skip(app.notepad.agent_scroll)
        .take(visible)
        .enumerate()
    {
        let y = body.y.saturating_add(offset as u16);
        frame.render_widget(
            Paragraph::new(row.line.clone()),
            Rect::new(body.x, y, body.width, 1),
        );
    }
}

/// The visible link labels as `((x, y), cell symbol, url)` triples, merged into
/// the frame's hyperlink list so the outer terminal makes them OSC 8 links.
pub(crate) fn hyperlink_cells(app: &AppState) -> Vec<((u16, u16), String, String)> {
    if !app.notepad.agent_tab {
        return Vec::new();
    }
    let body = super::notepad::notepad_body_rect(app.view.notepad_rect);
    if body.width == 0 || body.height == 0 {
        return Vec::new();
    }
    let mut cells = Vec::new();
    for (offset, row) in app
        .view
        .notepad_agent_rows
        .iter()
        .skip(app.notepad.agent_scroll)
        .take(usize::from(body.height))
        .enumerate()
    {
        let (Some((rel_x, label)), NotepadAgentAction::CopyLink(url)) =
            (&row.link_label, &row.action)
        else {
            continue;
        };
        let y = body.y.saturating_add(offset as u16);
        let mut x = body.x.saturating_add(*rel_x);
        for ch in label.chars() {
            if x >= body.right() {
                break;
            }
            let symbol = ch.to_string();
            let advance = display_width_u16(&symbol).max(1);
            cells.push(((x, y), symbol, url.clone()));
            x = x.saturating_add(advance);
        }
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_state::{
        AgentLinkSource, AgentReportPayload, AgentSubagent, AgentSubagentSource, AgentTask,
    };
    use crate::detect::{Agent, AgentState};
    use crate::workspace::Workspace;
    use ratatui::{backend::TestBackend, Terminal};
    use std::time::{Duration, SystemTime};

    const BASE_SECS: u64 = 1_760_000_000;

    fn base() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(BASE_SECS)
    }

    fn row_text(row: &NotepadAgentRow) -> String {
        row.line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn app_with_agent() -> (AppState, crate::layout::PaneId) {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("alpha")];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.notepad.enabled = true;
        app.notepad.height = 18;
        app.status_now_unix = Some(BASE_SECS as i64 + 600);
        let pane_id = app.workspaces[0].focused_pane_id().unwrap();
        let terminal_id = app.workspaces[0].terminal_id(pane_id).unwrap().clone();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Codex), AgentState::Working);
        (app, pane_id)
    }

    fn report(app: &mut AppState, pane_id: crate::layout::PaneId) {
        app.agent_states
            .report(
                pane_id,
                AgentReportPayload {
                    status_text: Some("checking tests".into()),
                    goal: Some("ship MAT-160".into()),
                    tasks: vec![
                        AgentTask {
                            text: "read transcript".into(),
                            status: AgentTaskStatus::Completed,
                        },
                        AgentTask {
                            text: "build link cache".into(),
                            status: AgentTaskStatus::InProgress,
                        },
                    ],
                    subagents: vec![
                        AgentSubagent {
                            name: "native worker".into(),
                            status: AgentStatus::Working,
                            last_active_at: None,
                            pane_id: None,
                            source: AgentSubagentSource::Observed,
                        },
                        AgentSubagent {
                            name: "reported reviewer".into(),
                            status: AgentStatus::Blocked,
                            last_active_at: None,
                            pane_id: Some("w1:p2".into()),
                            source: AgentSubagentSource::Reported,
                        },
                    ],
                },
                base(),
            )
            .expect("valid report");
    }

    /// UI2: all four sections render expanded with their collapse glyphs, the
    /// subagent header carries the count, and folding a section hides its rows.
    #[test]
    fn sections_render_expanded_and_fold_around_their_rows() {
        let (mut app, pane_id) = app_with_agent();
        report(&mut app, pane_id);

        let rows = agent_rows(&app, 40);
        let texts: Vec<String> = rows.iter().map(row_text).collect();
        let text = texts.join("\n");
        assert!(text.contains("▾ Status"), "{text}");
        assert!(text.contains("working · 10m ago"), "{text}");
        assert!(text.contains("goal: ship MAT-160"), "{text}");
        assert!(text.contains("checking tests"), "{text}");
        assert!(text.contains("▾ Tasks"), "{text}");
        assert!(text.contains("✓ read transcript"), "{text}");
        assert!(text.contains("▶ build link cache"), "{text}");
        assert!(text.contains("▾ Subagents (2)"), "{text}");
        assert!(text.contains("native worker · working"), "{text}");
        assert!(text.contains("▾ Links"), "{text}");

        app.notepad.toggle_agent_section(AgentSection::Tasks);
        let rows = agent_rows(&app, 40);
        let texts: Vec<String> = rows.iter().map(row_text).collect();
        let text = texts.join("\n");
        assert!(text.contains("▸ Tasks"), "{text}");
        assert!(!text.contains("read transcript"), "{text}");
        assert!(
            text.contains("▾ Subagents (2)"),
            "other sections stay expanded: {text}"
        );
        assert_eq!(
            rows.iter()
                .find(|row| row_text(row).contains("Tasks"))
                .map(|row| &row.action),
            Some(&NotepadAgentAction::ToggleSection(AgentSection::Tasks))
        );
    }

    /// UI4: links group by domain, newest last_seen first at both levels, and
    /// each row shows only a short label while the action carries the full URL.
    #[test]
    fn links_group_by_domain_newest_first_with_short_labels() {
        let (mut app, pane_id) = app_with_agent();
        app.agent_states.observe_links(
            pane_id,
            [
                "https://github.com/owner/repo/pull/1259".to_string(),
                "https://linear.app/scalable/issue/MAT-160/agent-tab".to_string(),
                "https://github.com/owner/repo/actions/runs/42".to_string(),
            ],
            AgentLinkSource::Output,
            base(),
        );
        app.agent_states.observe_links(
            pane_id,
            ["https://linear.app/scalable/issue/MAT-160/agent-tab".to_string()],
            AgentLinkSource::Output,
            base() + Duration::from_secs(300),
        );

        let rows = agent_rows(&app, 40);
        let text: Vec<String> = rows.iter().map(row_text).collect();
        let section = text.iter().position(|row| row.contains("Links")).unwrap();
        let section = &text[section..];
        let linear = section
            .iter()
            .position(|row| row.trim() == "linear.app")
            .unwrap();
        let github = section
            .iter()
            .position(|row| row.trim() == "github.com")
            .unwrap();
        assert!(linear < github, "linear's link is newest: {section:?}");
        let ticket = &section[linear + 1];
        assert!(ticket.contains("MAT-160"), "{ticket}");
        assert!(ticket.trim_end().ends_with("5m"), "{ticket}");
        // Same last_seen inside one group falls back to URL order.
        let actions = &section[github + 1];
        assert!(actions.contains("owner/repo/actions/runs/42"), "{actions}");
        let pull = &section[github + 2];
        assert!(pull.contains("pull/1259"), "{pull}");

        let link_rows: Vec<&NotepadAgentRow> = rows
            .iter()
            .filter(|row| matches!(row.action, NotepadAgentAction::CopyLink(_)))
            .collect();
        assert_eq!(link_rows.len(), 3);
        assert_eq!(
            link_rows[0].action,
            NotepadAgentAction::CopyLink(
                "https://linear.app/scalable/issue/MAT-160/agent-tab".into()
            )
        );
        assert_eq!(
            link_rows[0].link_label,
            Some((4, "MAT-160".to_string())),
            "the label is the short form; the URL stays on the action"
        );
    }

    /// UI5: a pane without an agent says so, and an agent without data shows
    /// one `none` per section.
    #[test]
    fn empty_states_are_explicit() {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("alpha")];
        app.ensure_test_terminals();
        app.active = Some(0);
        let rows = agent_rows(&app, 40);
        assert_eq!(rows.len(), 1);
        assert_eq!(row_text(&rows[0]), "no agent in this pane");

        let (app, _pane_id) = app_with_agent();
        let texts: Vec<String> = agent_rows(&app, 40).iter().map(row_text).collect();
        let text = texts.join("\n");
        assert!(text.contains("▾ Status"), "{text}");
        assert_eq!(
            texts.iter().filter(|row| row.trim() == "none").count(),
            3,
            "tasks, subagents and links each say none: {text}"
        );
    }

    /// UI1: the header gains `│ agent`, and the body is the focused pane's
    /// state, switching when focus moves to a pane without an agent.
    #[test]
    fn the_agent_tab_follows_focus_into_a_real_frame() {
        const WIDTH: u16 = 120;
        const HEIGHT: u16 = 40;
        let (mut app, pane_id) = app_with_agent();
        report(&mut app, pane_id);
        app.notepad.set_files(vec![crate::notepad::NotepadFile {
            path: "/notes/todo.md".into(),
            name: "todo".into(),
        }]);
        app.notepad.select_agent_tab();

        crate::ui::compute_view(&mut app, Rect::new(0, 0, WIDTH, HEIGHT));
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("test terminal");
        terminal
            .draw(|frame| crate::ui::render(&app, frame))
            .expect("render");
        let buffer = terminal.backend().buffer();
        let panel = app.view.notepad_rect;
        let panel_text = |row: u16| {
            (panel.x..panel.right())
                .map(|x| buffer[(x, row)].symbol())
                .collect::<String>()
        };
        let header = panel_text(panel.y);
        assert!(header.contains("todo"), "{header}");
        assert!(header.contains("│ agent"), "{header}");
        let body: Vec<String> = (panel.y + 1..panel.bottom()).map(panel_text).collect();
        let body_text = body.join("\n");
        assert!(body_text.contains("working · 10m ago"), "{body_text}");
        assert!(body_text.contains("goal: ship MAT-160"), "{body_text}");
        assert!(
            !body_text.contains("type a note"),
            "the editor placeholder must not show: {body_text}"
        );

        let plain = app.workspaces[0].test_split(ratatui::layout::Direction::Vertical);
        app.ensure_test_terminals();
        assert_eq!(app.workspaces[0].focused_pane_id(), Some(plain));
        crate::ui::compute_view(&mut app, Rect::new(0, 0, WIDTH, HEIGHT));
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("test terminal");
        terminal
            .draw(|frame| crate::ui::render(&app, frame))
            .expect("render");
        let buffer = terminal.backend().buffer();
        let body: Vec<String> = (panel.y + 1..panel.bottom())
            .map(|row| {
                (panel.x..panel.right())
                    .map(|x| buffer[(x, row)].symbol())
                    .collect::<String>()
            })
            .collect();
        assert!(
            body.join("\n").contains("no agent in this pane"),
            "{}",
            body.join("\n")
        );
    }

    /// The OSC 8 cells track the visible window: each label cell carries the
    /// full URL, and scrolling moves them with the rows.
    #[test]
    fn hyperlink_cells_cover_each_visible_link_label() {
        let (mut app, pane_id) = app_with_agent();
        app.notepad.height = 16;
        for index in 0..6 {
            app.agent_states.observe_links(
                pane_id,
                [format!("https://github.com/owner/repo/pull/{index}")],
                AgentLinkSource::Output,
                base() + Duration::from_secs(index),
            );
        }
        app.notepad.select_agent_tab();
        crate::ui::compute_view(&mut app, Rect::new(0, 0, 120, 40));
        let body = crate::ui::notepad::notepad_body_rect(app.view.notepad_rect);

        let cells = hyperlink_cells(&app);
        assert!(!cells.is_empty());
        let (link_index, row) = app
            .view
            .notepad_agent_rows
            .iter()
            .enumerate()
            .find(|(_, row)| matches!(row.action, NotepadAgentAction::CopyLink(_)))
            .expect("one visible link row");
        let y = body.y + link_index as u16;
        let label = row.link_label.clone().expect("link label").1;
        let first = cells.iter().find(|((_, cy), _, _)| *cy == y).unwrap();
        assert_eq!(first.0 .0, body.x + 4);
        assert_eq!(first.1, label.chars().next().unwrap().to_string());
        assert!(first.2.starts_with("https://github.com/owner/repo/pull/"));
        let row_cells = cells.iter().filter(|((_, cy), _, _)| *cy == y).count();
        assert_eq!(row_cells, label.chars().count());

        app.notepad.agent_scroll_by(1, 10);
        let scrolled = hyperlink_cells(&app);
        assert!(
            scrolled
                .iter()
                .any(|cell| cell.2 == first.2 && cell.0 .1 == y - 1),
            "scrolling moves the row's hyperlink cells up with it"
        );
    }
}
