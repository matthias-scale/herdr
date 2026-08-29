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
                // The palette reserves `red` for needs-attention/blocked, and
                // the sidebar already says blocked in it. Accent is the generic
                // highlight colour and read as "selected", not "waiting".
                .fg(if counts.blocked > 0 {
                    app.palette.red
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
    // Bold-vs-dim alone was not readable as a cursor. A filled row is, and it
    // is the same surface the sidebar uses for its selection.
    let style = if selected {
        Style::default()
            .fg(app.palette.text)
            .bg(app.palette.surface0)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(app.palette.subtext0)
    };
    let age_style = if selected {
        Style::default()
            .fg(app.palette.overlay1)
            .bg(app.palette.surface0)
    } else {
        Style::default().fg(app.palette.overlay0)
    };
    Line::from(vec![
        Span::styled(bullet.to_string(), style),
        Span::styled(format!("{workspace:<label_width$} "), style),
        Span::styled(format!("{ask:<ask_width$}"), style),
        Span::styled(
            format!("{age:>width$} ", width = age.chars().count() + 1),
            age_style,
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

/// The four bands home draws into: header, gap, body, hint.
///
/// Shared with hit-testing so a click can never land on a row the renderer put
/// somewhere else.
fn bands(area: Rect) -> [Rect; 4] {
    Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area)
}

/// One `(queue index, rect)` per row home is currently showing.
///
/// Empty when home is closed or the queue is, so the click handler needs no
/// separate "is home open" test.
pub(super) fn row_hit_areas(
    app: &AppState,
    queue: &[BlockedAgent],
    area: Rect,
) -> Vec<(usize, Rect)> {
    let Some(home) = app.home.as_ref() else {
        return Vec::new();
    };
    if queue.is_empty() || area.width == 0 || area.height < 4 {
        return Vec::new();
    }
    let [_, _, body, _] = bands(area);
    let visible = body.height as usize;
    let scroll = home.scroll(queue, visible);
    (scroll..queue.len().min(scroll + visible))
        .enumerate()
        .map(|(offset, index)| {
            (
                index,
                Rect::new(body.x, body.y + offset as u16, body.width, 1),
            )
        })
        .collect()
}

pub(super) fn render_home(
    app: &AppState,
    queue: &[BlockedAgent],
    counts: HomeCounts,
    area: Rect,
    frame: &mut Frame,
) {
    let [header, _, body, hint] = bands(area);

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::PaneId;
    use crate::terminal::TerminalId;
    use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};

    fn blocked(index: usize) -> BlockedAgent {
        BlockedAgent {
            ws_idx: 0,
            pane_id: PaneId::alloc(),
            terminal_id: TerminalId::alloc(),
            workspace_label: format!("ws{index}"),
            agent_label: format!("agent{index}"),
            blocked_since: None,
            seq: None,
        }
    }

    fn draw_home(app: &AppState, queue: &[BlockedAgent], area: Rect) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).expect("term");
        terminal
            .draw(|frame| {
                render_home(
                    app,
                    queue,
                    HomeCounts {
                        blocked: queue.len(),
                        agents: queue.len(),
                        spaces: 1,
                    },
                    area,
                    frame,
                );
            })
            .expect("render home");
        terminal.backend().buffer().clone()
    }

    fn row_text(buffer: &Buffer, area: Rect, row: u16) -> String {
        (area.x..area.x + area.width)
            .map(|column| buffer[(column, row)].symbol())
            .collect()
    }

    #[test]
    fn home_renders_blocked_rows_and_marks_the_selected_row_with_bold_text() {
        let mut app = AppState::test_new();
        app.home = Some(crate::app::home::HomeState::default());
        let queue = vec![blocked(0), blocked(1)];
        let area = Rect::new(0, 0, 60, 6);

        let buffer = draw_home(&app, &queue, area);

        assert!(row_text(&buffer, area, 2).contains("agent0"));
        assert!(row_text(&buffer, area, 3).contains("agent1"));
        assert_eq!(buffer[(1, 2)].symbol(), "▸");
        assert_eq!(buffer[(1, 3)].symbol(), "·");
        assert_eq!(
            buffer[(1, 2)].style().add_modifier(Modifier::BOLD),
            buffer[(1, 2)].style()
        );
        assert_ne!(
            buffer[(1, 3)].style().add_modifier(Modifier::BOLD),
            buffer[(1, 3)].style()
        );
    }

    #[test]
    fn the_blocked_count_is_drawn_in_the_needs_attention_colour() {
        let mut app = AppState::test_new();
        app.home = Some(crate::app::home::HomeState::default());
        let queue = vec![blocked(0)];
        let area = Rect::new(0, 0, 60, 6);

        let buffer = draw_home(&app, &queue, area);

        // The palette reserves `red` for blocked; `accent` is the generic
        // highlight and reads as "selected" rather than "waiting".
        assert_eq!(buffer[(1, 0)].style().fg, Some(app.palette.red));
        assert_ne!(app.palette.red, app.palette.accent);

        // With nothing waiting the count goes quiet rather than staying loud.
        let buffer = draw_home(&app, &[], area);
        assert_eq!(buffer[(1, 0)].style().fg, Some(app.palette.overlay0));
    }

    #[test]
    fn the_selected_row_is_filled_across_its_full_width_not_only_emboldened() {
        let mut app = AppState::test_new();
        app.home = Some(crate::app::home::HomeState::default());
        let queue = vec![blocked(0), blocked(1)];
        let area = Rect::new(0, 0, 60, 6);

        let buffer = draw_home(&app, &queue, area);

        // Every cell of the cursor row carries the surface, so the cursor is
        // legible as a band rather than as a weight difference.
        for column in area.x..area.right() {
            assert_eq!(
                buffer[(column, 2)].style().bg,
                Some(app.palette.surface0),
                "column {column} of the selected row is not filled"
            );
        }
        assert_ne!(buffer[(1, 3)].style().bg, Some(app.palette.surface0));
    }

    #[test]
    fn home_row_hit_areas_line_up_with_the_rows_that_were_drawn() {
        let mut app = AppState::test_new();
        app.home = Some(crate::app::home::HomeState::default());
        let queue = vec![blocked(0), blocked(1)];
        let area = Rect::new(0, 0, 60, 6);

        let buffer = draw_home(&app, &queue, area);
        let hits = row_hit_areas(&app, &queue, area);

        assert_eq!(hits.len(), 2);
        for (index, rect) in &hits {
            assert_eq!(rect.height, 1);
            assert!(
                row_text(&buffer, area, rect.y).contains(&format!("agent{index}")),
                "hit area for row {index} is not where agent{index} was drawn"
            );
        }
    }

    #[test]
    fn home_offers_no_hit_areas_when_it_is_closed_or_has_nothing_to_show() {
        let mut app = AppState::test_new();
        let queue = vec![blocked(0)];
        let area = Rect::new(0, 0, 60, 6);

        // Closed: the click handler needs no separate "is home open" test.
        assert!(row_hit_areas(&app, &queue, area).is_empty());

        app.home = Some(crate::app::home::HomeState::default());
        assert!(row_hit_areas(&app, &[], area).is_empty());
        // Too short to have a body band at all.
        assert!(row_hit_areas(&app, &queue, Rect::new(0, 0, 60, 3)).is_empty());
    }

    #[test]
    fn a_scrolled_home_reports_hit_areas_for_the_rows_actually_on_screen() {
        let mut app = AppState::test_new();
        let mut home = crate::app::home::HomeState::default();
        let queue: Vec<BlockedAgent> = (0..10).map(blocked).collect();
        home.select(9);
        app.home = Some(home);
        let area = Rect::new(0, 0, 60, 6);

        let buffer = draw_home(&app, &queue, area);
        let hits = row_hit_areas(&app, &queue, area);

        // Body is 3 rows tall, and the cursor is on the last agent, so the
        // reported indices are the tail of the queue rather than its head.
        assert_eq!(hits.len(), 3);
        assert_eq!(
            hits.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
            vec![7, 8, 9]
        );
        for (index, rect) in &hits {
            assert!(row_text(&buffer, area, rect.y).contains(&format!("agent{index}")));
        }
    }

    #[test]
    fn an_empty_home_queue_renders_the_waiting_message_instead_of_a_blank_body() {
        let mut app = AppState::test_new();
        app.home = Some(crate::app::home::HomeState::default());
        let area = Rect::new(0, 0, 60, 6);

        let buffer = draw_home(&app, &[], area);
        let text = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(text.contains("nothing is waiting on you"), "{text:?}");
    }

    #[test]
    fn a_scrolled_home_queue_renders_only_the_rows_in_the_body_from_the_scroll_offset() {
        let mut app = AppState::test_new();
        let queue: Vec<_> = (0..8).map(blocked).collect();
        let mut home = crate::app::home::HomeState::default();
        for _ in 0..6 {
            home.select_next(&queue);
        }
        app.home = Some(home);
        let area = Rect::new(0, 0, 60, 7);

        let buffer = draw_home(&app, &queue, area);
        let body_rows: Vec<String> = (2..6).map(|row| row_text(&buffer, area, row)).collect();

        assert!(body_rows[0].contains("agent3"), "{body_rows:?}");
        assert!(body_rows[1].contains("agent4"), "{body_rows:?}");
        assert!(body_rows[2].contains("agent5"), "{body_rows:?}");
        assert!(body_rows[3].contains("agent6"), "{body_rows:?}");
        assert!(body_rows.iter().all(|row| !row.contains("agent2")));
        assert!(body_rows.iter().all(|row| !row.contains("agent7")));
    }

    #[test]
    fn a_one_cell_home_area_does_not_panic_or_write_outside_its_width() {
        let mut app = AppState::test_new();
        app.home = Some(crate::app::home::HomeState::default());
        let queue = vec![blocked(0)];
        let area = Rect::new(0, 0, 1, 6);

        let buffer = draw_home(&app, &queue, area);

        assert_eq!(*buffer.area(), area);
        for row in 0..area.height {
            assert_eq!(row_text(&buffer, area, row).chars().count(), 1);
        }
    }
}
