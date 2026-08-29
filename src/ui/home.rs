use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{home::HomeCounts, inbox::BlockedAgent, AppState};

/// How long this agent has been waiting, in the sidebar's own age vocabulary.
fn waited_label(agent: &BlockedAgent) -> String {
    match agent.blocked_since {
        Some(since) => crate::activity_age::coarse_label(Some(since), std::time::Instant::now()),
        // The transition was never observed. Saying so beats inventing a duration.
        None => "—".to_string(),
    }
}

/// `● 4 blocked` on the left, the fleet's size on the right.
///
/// Blocked leads and is the only figure with a marker: it is the one number that
/// means somebody is waiting. The rest is context for reading it.
fn header_line(app: &AppState, counts: HomeCounts, width: u16) -> Line<'static> {
    let left = format!(" ● {} blocked", counts.blocked);
    let right = format!("{} agents · {} spaces ", counts.agents, counts.spaces,);
    let gap = (width as usize).saturating_sub(left.chars().count() + right.chars().count());
    Line::from(vec![
        Span::styled(
            left,
            Style::default()
                .fg(if counts.blocked > 0 {
                    app.palette.accent
                } else {
                    app.palette.overlay0
                })
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ".repeat(gap)),
        Span::styled(right, Style::default().fg(app.palette.overlay0)),
    ])
}

/// `▸  workspace       what it is asking            18m`
fn agent_line(app: &AppState, agent: &BlockedAgent, selected: bool, width: u16) -> Line<'static> {
    let bullet = if selected { " ▸  " } else { " ·  " };
    let age = waited_label(agent);
    let label_width = 16usize;
    let workspace = truncate(&agent.workspace_label, label_width);
    // Whatever the ask consumes, the age keeps its column: the list is sorted by
    // it, so a ragged right edge would hide the ordering the sort exists for.
    let ask_width = (width as usize)
        .saturating_sub(bullet.chars().count() + label_width + 1 + age.chars().count() + 2);
    let ask = truncate(&agent.agent_label, ask_width);
    let style = if selected {
        Style::default()
            .fg(app.palette.text)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(app.palette.subtext0)
    };
    Line::from(vec![
        Span::styled(bullet.to_string(), style),
        Span::styled(format!("{workspace:<label_width$} "), style),
        Span::styled(format!("{ask:<ask_width$}"), style),
        Span::styled(
            format!("{age:>width$} ", width = age.chars().count() + 1),
            Style::default().fg(app.palette.overlay0),
        ),
    ])
}

fn truncate(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if s.chars().count() <= width {
        return s.to_string();
    }
    let mut out: String = s.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn empty_line(app: &AppState) -> Line<'static> {
    Line::from(Span::styled(
        " nothing is waiting on you",
        Style::default()
            .fg(app.palette.overlay0)
            .add_modifier(Modifier::DIM),
    ))
}

fn hint_line(app: &AppState, hidden_above: usize, hidden_below: usize) -> Line<'static> {
    let mut hint = " ↑↓ browse · ⏎ jump · esc closes".to_string();
    // Only mention what is off-screen when something is, so a list that fits
    // carries no chrome about scrolling.
    if hidden_above + hidden_below > 0 {
        hint.push_str(&format!(" · {} more", hidden_above + hidden_below));
    }
    Line::from(Span::styled(
        hint,
        Style::default()
            .fg(app.palette.overlay0)
            .add_modifier(Modifier::DIM),
    ))
}

pub(super) fn render_home(
    app: &AppState,
    queue: &[BlockedAgent],
    counts: HomeCounts,
    area: Rect,
    frame: &mut Frame,
) {
    let [header, _, body, hint] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);

    frame.render_widget(Paragraph::new(header_line(app, counts, area.width)), header);

    let visible = body.height as usize;
    let scroll = app
        .home
        .as_ref()
        .map(|home| home.scroll(queue, visible))
        .unwrap_or(0);
    let selected = app
        .home
        .as_ref()
        .map(|home| home.selected(queue))
        .unwrap_or(0);

    let lines: Vec<Line<'static>> = if queue.is_empty() {
        vec![empty_line(app)]
    } else {
        queue
            .iter()
            .enumerate()
            .skip(scroll)
            .take(visible)
            .map(|(idx, agent)| agent_line(app, agent, idx == selected, body.width))
            .collect()
    };
    frame.render_widget(Paragraph::new(lines), body);

    let hidden_below = queue.len().saturating_sub(scroll + visible);
    frame.render_widget(Paragraph::new(hint_line(app, scroll, hidden_below)), hint);
}
