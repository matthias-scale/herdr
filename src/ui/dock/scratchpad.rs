use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};

use super::scrollbar::{release_notes_scrollbar_rect, render_scrollbar};
use crate::{
    app::{state::ScratchpadLinkRow, AppState},
    scratchpad::SCRATCHPAD_RELATIVE_PATH,
};

fn dim_line(app: &AppState, message: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!(" {message}"),
        Style::default()
            .fg(app.palette.overlay0)
            .add_modifier(Modifier::DIM),
    ))
}

/// The message shown instead of a body, or `None` when there is a body to render.
fn empty_state(app: &AppState) -> Option<Line<'static>> {
    let doc = &app.scratchpad;
    if doc.path.is_none() {
        return Some(dim_line(app, "focused pane belongs to no repository"));
    }
    if let Some(error) = doc.error.as_deref() {
        return Some(dim_line(app, &format!("scratchpad unreadable: {error}")));
    }
    if !doc.exists || doc.body.trim().is_empty() {
        return Some(dim_line(
            app,
            &format!("no notes yet · {SCRATCHPAD_RELATIVE_PATH}"),
        ));
    }
    None
}

/// Markdown is rendered as styled plain text rather than parsed: the scratchpad is
/// a working note, and a heading standing out is the whole of what reading it needs.
fn body_lines(app: &AppState) -> Vec<Line<'static>> {
    app.scratchpad
        .body
        .lines()
        .map(|raw| {
            let trimmed = raw.trim_start();
            let style = if trimmed.starts_with('#') {
                Style::default()
                    .fg(app.palette.accent)
                    .add_modifier(Modifier::BOLD)
            } else if trimmed.starts_with("- [ ]") {
                Style::default().fg(app.palette.text)
            } else if trimmed.starts_with("- [x]") || trimmed.starts_with("- [X]") {
                Style::default()
                    .fg(app.palette.overlay0)
                    .add_modifier(Modifier::DIM)
            } else {
                Style::default().fg(app.palette.text)
            };
            Line::from(Span::styled(format!(" {raw}"), style))
        })
        .collect()
}

/// Links sit in a fixed block at the foot of the tab for the same reason the
/// Context tab puts them there: a wrapped paragraph's rows cannot be located
/// again from outside, so a click would have nothing stable to hit.
fn split_off_links_area(area: Rect, link_count: usize) -> (Rect, Option<Rect>) {
    if link_count == 0 || area.height < 4 {
        return (area, None);
    }
    let wanted = u16::try_from(link_count.saturating_add(1))
        .unwrap_or(u16::MAX)
        .min(area.height / 2);
    let body_height = area.height.saturating_sub(wanted);
    (
        Rect::new(area.x, area.y, area.width, body_height),
        Some(Rect::new(
            area.x,
            area.y.saturating_add(body_height),
            area.width,
            wanted,
        )),
    )
}

fn link_row_rects(links_area: Rect, link_count: usize) -> Vec<Rect> {
    (0..link_count)
        .map_while(|index| {
            let y = links_area.y.saturating_add(1).checked_add(index as u16)?;
            (y < links_area.y.saturating_add(links_area.height))
                .then(|| Rect::new(links_area.x, y, links_area.width, 1))
        })
        .collect()
}

/// Registered for the click path that opens a URL externally.
pub(crate) fn scratchpad_link_rows(app: &AppState, area: Rect) -> Vec<ScratchpadLinkRow> {
    if area.width == 0 || area.height == 0 || empty_state(app).is_some() {
        return Vec::new();
    }
    let candidates = app.scratchpad.link_candidates();
    let (_, Some(links_area)) = split_off_links_area(area, candidates.len()) else {
        return Vec::new();
    };
    candidates
        .into_iter()
        .zip(link_row_rects(links_area, usize::from(links_area.height)))
        .map(|(candidate, rect)| ScratchpadLinkRow {
            rect,
            url: candidate.url,
        })
        .collect()
}

fn render_links(app: &AppState, frame: &mut Frame, links_area: Rect) {
    let candidates = app.scratchpad.link_candidates();
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " LINKS",
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD),
        ))),
        Rect::new(links_area.x, links_area.y, links_area.width, 1),
    );
    for (index, (candidate, rect)) in candidates
        .iter()
        .zip(link_row_rects(links_area, usize::from(links_area.height)))
        .enumerate()
    {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(" {} ", index + 1),
                    Style::default().fg(app.palette.overlay0),
                ),
                Span::styled(
                    candidate.label.clone(),
                    Style::default().fg(app.palette.text),
                ),
            ])),
            rect,
        );
    }
}

pub(super) fn render_scratchpad(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if let Some(message) = empty_state(app) {
        frame.render_widget(Paragraph::new(message).wrap(Wrap { trim: false }), area);
        return;
    }

    let link_count = app.scratchpad.link_candidates().len();
    let (area, links_area) = split_off_links_area(area, link_count);
    if let Some(links_area) = links_area {
        render_links(app, frame, links_area);
    }

    let lines = body_lines(app);
    let viewport = area.height as usize;
    let initial_rows = Paragraph::new(lines.clone())
        .wrap(Wrap { trim: false })
        .line_count(area.width.max(1));
    let needs_scrollbar = initial_rows > viewport && area.width > 1;
    let text_width = if needs_scrollbar {
        area.width.saturating_sub(1)
    } else {
        area.width
    };
    let total_rows = Paragraph::new(lines.clone())
        .wrap(Wrap { trim: false })
        .line_count(text_width.max(1));
    let max_scroll = total_rows.saturating_sub(viewport);
    let scroll = usize::from(app.dock_scroll).min(max_scroll);
    let metrics = crate::pane::ScrollMetrics {
        offset_from_bottom: max_scroll.saturating_sub(scroll),
        max_offset_from_bottom: max_scroll,
        viewport_rows: viewport.max(1),
    };
    let track = release_notes_scrollbar_rect(area, metrics);
    let text_area = track
        .map(|_| Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height))
        .unwrap_or(area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll.min(u16::MAX as usize) as u16, 0)),
        text_area,
    );
    if let Some(track) = track {
        render_scrollbar(
            frame,
            metrics,
            track,
            app.palette.overlay0,
            app.palette.overlay1,
            "▐",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratchpad::ScratchpadDoc;
    use ratatui::{backend::TestBackend, Terminal};
    use std::path::PathBuf;

    fn rendered_text(area: Rect, app: &AppState) -> String {
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render_scratchpad(app, frame, area))
            .expect("render scratchpad");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn doc(body: &str) -> ScratchpadDoc {
        ScratchpadDoc {
            path: Some(PathBuf::from("/repo/.herdr/scratchpad.md")),
            body: body.to_string(),
            error: None,
            exists: true,
        }
    }

    #[test]
    fn a_pane_outside_a_repository_says_so() {
        let app = AppState::test_new();
        let text = rendered_text(Rect::new(0, 0, 40, 3), &app);
        assert!(text.contains("no repository"), "rendered: {text:?}");
    }

    #[test]
    fn a_repository_without_notes_names_the_path_it_expects() {
        let mut app = AppState::test_new();
        app.scratchpad = ScratchpadDoc {
            path: Some(PathBuf::from("/repo/.herdr/scratchpad.md")),
            ..ScratchpadDoc::default()
        };
        let text = rendered_text(Rect::new(0, 0, 40, 3), &app);
        assert!(text.contains("no notes yet"), "rendered: {text:?}");
    }

    #[test]
    fn an_unreadable_scratchpad_reports_its_error() {
        let mut app = AppState::test_new();
        app.scratchpad = ScratchpadDoc {
            path: Some(PathBuf::from("/repo/.herdr/scratchpad.md")),
            error: Some("permission denied".to_string()),
            exists: true,
            ..ScratchpadDoc::default()
        };
        let text = rendered_text(Rect::new(0, 0, 60, 3), &app);
        assert!(text.contains("permission denied"), "rendered: {text:?}");
    }

    #[test]
    fn the_body_renders_verbatim() {
        let mut app = AppState::test_new();
        app.scratchpad = doc("## Progress\nrebased onto main\n");
        let text = rendered_text(Rect::new(0, 0, 40, 6), &app);
        assert!(text.contains("Progress"), "rendered: {text:?}");
        assert!(text.contains("rebased onto main"), "rendered: {text:?}");
    }

    #[test]
    fn links_in_the_body_become_numbered_rows_that_carry_their_url() {
        let mut app = AppState::test_new();
        app.scratchpad = doc("see https://github.com/o/r/pull/7 before merging\n");
        let area = Rect::new(0, 0, 60, 8);
        let rows = scratchpad_link_rows(&app, area);
        assert_eq!(rows.len(), 1, "rows: {rows:?}");
        assert_eq!(rows[0].url, "https://github.com/o/r/pull/7");
        let text = rendered_text(area, &app);
        assert!(text.contains("LINKS"), "rendered: {text:?}");
    }

    #[test]
    fn an_empty_scratchpad_registers_no_clickable_rows() {
        let app = AppState::test_new();
        assert!(scratchpad_link_rows(&app, Rect::new(0, 0, 40, 8)).is_empty());
    }
}
