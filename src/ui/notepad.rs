//! The sidebar notepad panel: a header naming the active note and the editable
//! note body under it, pinned to the bottom of the sidebar above the footer
//! icons.
//!
//! Geometry is pure so the panel's rows can be reserved before anything draws;
//! [`super::sidebar::workspace_list_rect_for_app`] subtracts them from the
//! workspace list.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::text::{display_width_u16, truncate_end};
use crate::app::state::Palette;
use crate::app::AppState;

/// Rows the workspace list keeps for itself before the notepad may claim any.
const MIN_LIST_ROWS_BESIDE_NOTEPAD: u16 = 6;
/// Header row plus at least one body row.
const MIN_NOTEPAD_ROWS: u16 = 2;

/// How many rows of `content` the notepad panel occupies. Zero whenever it is
/// off, the sidebar is collapsed, or the list would be squeezed below
/// [`MIN_LIST_ROWS_BESIDE_NOTEPAD`].
pub(crate) fn notepad_height(app: &AppState, content: Rect) -> u16 {
    if !app.notepad.enabled || app.sidebar_collapsed || content.width == 0 {
        return 0;
    }
    let spare = content.height.saturating_sub(MIN_LIST_ROWS_BESIDE_NOTEPAD);
    if spare < MIN_NOTEPAD_ROWS {
        return 0;
    }
    app.notepad.height.min(spare)
}

/// The panel itself, at the bottom of the sidebar's content area.
pub(crate) fn notepad_panel_rect(app: &AppState, content: Rect) -> Rect {
    let height = notepad_height(app, content);
    if height == 0 {
        return Rect::default();
    }
    Rect::new(
        content.x,
        content.bottom().saturating_sub(height),
        content.width,
        height,
    )
}

/// The note body rows inside the panel, header excluded.
pub(crate) fn notepad_body_rect(panel: Rect) -> Rect {
    if panel.height <= 1 {
        return Rect::default();
    }
    Rect::new(
        panel.x,
        panel.y.saturating_add(1),
        panel.width,
        panel.height.saturating_sub(1),
    )
}

/// Clickable name segments in the header, one per note, left to right. Only the
/// names that fit are returned, so a click can never resolve to a note the
/// operator cannot see.
pub(crate) fn notepad_tab_hit_areas(app: &AppState, panel: Rect) -> Vec<(usize, Rect)> {
    if panel.width == 0 || panel.height == 0 {
        return Vec::new();
    }
    let mut areas = Vec::new();
    let mut x = panel.x.saturating_add(2);
    let right = panel.right();
    for (index, file) in app.notepad.files.iter().enumerate() {
        let width = display_width_u16(&file.name);
        if width == 0 || x.saturating_add(width) > right {
            break;
        }
        areas.push((index, Rect::new(x, panel.y, width, 1)));
        x = x.saturating_add(width).saturating_add(1);
    }
    areas
}

fn header_spans<'a>(app: &'a AppState, palette: &Palette, width: u16) -> Line<'a> {
    let focused = app.notepad.focused;
    let mut spans = vec![Span::styled(
        "▤ ",
        Style::default().fg(if focused {
            palette.accent
        } else {
            palette.overlay0
        }),
    )];
    let mut used = 2u16;
    for (index, file) in app.notepad.files.iter().enumerate() {
        let name_width = display_width_u16(&file.name);
        if used.saturating_add(name_width) > width {
            break;
        }
        let active = index == app.notepad.active;
        let style = if active && focused {
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else if active {
            Style::default().fg(palette.text)
        } else {
            Style::default().fg(palette.overlay0)
        };
        spans.push(Span::styled(file.name.as_str(), style));
        used = used.saturating_add(name_width);
        if used < width {
            spans.push(Span::raw(" "));
            used = used.saturating_add(1);
        }
    }
    if app.notepad.files.is_empty() {
        spans.push(Span::styled(
            "no notes",
            Style::default().fg(palette.overlay0),
        ));
    }
    // Unsaved and diverged are the two facts the operator cannot recover by
    // looking at the body, so they get the remaining space.
    let marker = if app.notepad.error.is_some() {
        Some(("!", palette.red))
    } else if app.notepad.diverged {
        Some(("⇅", palette.peach))
    } else if app.notepad.dirty {
        Some(("•", palette.yellow))
    } else {
        None
    };
    if let Some((glyph, color)) = marker {
        if used < width {
            spans.push(Span::styled(glyph, Style::default().fg(color)));
        }
    }
    Line::from(spans)
}

pub(crate) fn render_notepad(app: &AppState, frame: &mut Frame, panel: Rect) {
    if panel.width == 0 || panel.height == 0 {
        return;
    }
    let palette = &app.palette;
    let separator = Style::default().fg(palette.surface_dim);
    {
        let buf = frame.buffer_mut();
        for x in panel.x..panel.right() {
            buf[(x, panel.y)].set_symbol("─");
            buf[(x, panel.y)].set_style(separator);
        }
    }
    frame.render_widget(
        Paragraph::new(header_spans(app, palette, panel.width)),
        Rect::new(panel.x, panel.y, panel.width, 1),
    );

    let body = notepad_body_rect(panel);
    if body.height == 0 {
        return;
    }
    if let Some(error) = &app.notepad.error {
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_end(error, usize::from(body.width)),
                Style::default().fg(palette.red),
            )),
            Rect::new(body.x, body.y, body.width, 1),
        );
        return;
    }

    let text_style = Style::default().fg(palette.text);
    let empty_style = Style::default().fg(palette.overlay0);
    let visible = usize::from(body.height);
    // The hint is for an untouched note; once the caret is in it, it is noise.
    let placeholder = !app.notepad.focused && app.notepad.lines.iter().all(String::is_empty);
    for row in 0..visible {
        let line_index = app.notepad.scroll + row;
        let y = body.y.saturating_add(row as u16);
        if placeholder && row == 0 {
            frame.render_widget(
                Paragraph::new(Span::styled("type a note…", empty_style)),
                Rect::new(body.x, y, body.width, 1),
            );
            continue;
        }
        let Some(line) = app.notepad.lines.get(line_index) else {
            break;
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                truncate_end(line, usize::from(body.width)),
                text_style,
            )),
            Rect::new(body.x, y, body.width, 1),
        );
    }
}

/// Where the note's caret belongs on screen, or `None` when the notepad does
/// not hold it. The caret carries the IME composition preview, so it has to be
/// a real host cursor rather than a highlighted cell.
pub(crate) fn notepad_caret_position(app: &AppState, panel: Rect) -> Option<(u16, u16)> {
    if !app.notepad.focused || app.notepad.error.is_some() {
        return None;
    }
    let body = notepad_body_rect(panel);
    if body.width == 0 || body.height == 0 {
        return None;
    }
    let row = app.notepad.cursor_line.saturating_sub(app.notepad.scroll);
    if row >= usize::from(body.height) {
        return None;
    }
    let prefix: String = app
        .notepad
        .lines
        .get(app.notepad.cursor_line)
        .map(|line| line.chars().take(app.notepad.cursor_col).collect())
        .unwrap_or_default();
    let x = body
        .x
        .saturating_add(display_width_u16(&prefix))
        .min(body.right().saturating_sub(1));
    Some((x, body.y.saturating_add(row as u16)))
}

/// Place the caret after every other surface has drawn. The focused pane, the
/// home composer and the sidebar all claim the one host cursor, and the last
/// claim wins; the notepad takes keys ahead of all of them, so it has to be the
/// one that speaks last. Only the break prompt, which takes keys ahead of the
/// notepad, may still overwrite it.
pub(crate) fn render_notepad_caret(app: &AppState, frame: &mut Frame) {
    if let Some(position) = notepad_caret_position(app, app.view.notepad_rect) {
        frame.set_cursor_position(position);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppState;

    fn state() -> AppState {
        let mut app = AppState::test_new();
        app.notepad.enabled = true;
        app.notepad.height = 8;
        app
    }

    #[test]
    fn a_disabled_notepad_reserves_no_rows() {
        let mut app = state();
        app.notepad.enabled = false;
        assert_eq!(notepad_height(&app, Rect::new(0, 0, 26, 40)), 0);
    }

    #[test]
    fn the_panel_never_squeezes_the_workspace_list_below_its_floor() {
        let app = state();
        assert_eq!(notepad_height(&app, Rect::new(0, 0, 26, 8)), 2);
        assert_eq!(notepad_height(&app, Rect::new(0, 0, 26, 13)), 7);
        assert_eq!(notepad_height(&app, Rect::new(0, 0, 26, 7)), 0);
        assert_eq!(notepad_height(&app, Rect::new(0, 0, 26, 40)), 8);
    }

    #[test]
    fn the_panel_sits_at_the_bottom_of_the_content_area() {
        let app = state();
        let content = Rect::new(0, 1, 26, 40);
        let panel = notepad_panel_rect(&app, content);
        assert_eq!(panel, Rect::new(0, 33, 26, 8));
        assert_eq!(notepad_body_rect(panel), Rect::new(0, 34, 26, 7));
    }

    #[test]
    fn header_hit_areas_stop_at_the_panel_edge() {
        let mut app = state();
        app.notepad.set_files(vec![
            crate::notepad::NotepadFile {
                path: "/notes/todo.md".into(),
                name: "todo".into(),
            },
            crate::notepad::NotepadFile {
                path: "/notes/ideas.md".into(),
                name: "ideas".into(),
            },
            crate::notepad::NotepadFile {
                path: "/notes/verylongnotename.md".into(),
                name: "verylongnotename".into(),
            },
        ]);
        let areas = notepad_tab_hit_areas(&app, Rect::new(0, 10, 14, 8));
        assert_eq!(areas.len(), 2);
        assert_eq!(areas[0].1, Rect::new(2, 10, 4, 1));
        assert_eq!(areas[1].1, Rect::new(7, 10, 5, 1));
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn buffer_text(terminal: &Terminal<TestBackend>) -> Vec<String> {
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(usize::from(terminal.backend().buffer().area.width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect()
    }

    /// The panel has to reach a real frame, not just its geometry helpers: a
    /// note the operator cannot see is not a notepad.
    #[test]
    fn the_panel_draws_its_note_name_and_body_into_the_frame() {
        const WIDTH: u16 = 120;
        const HEIGHT: u16 = 40;
        let mut app = AppState::test_new();
        app.notepad.enabled = true;
        app.notepad.height = 6;
        app.notepad.set_files(vec![
            crate::notepad::NotepadFile {
                path: "/notes/todo.md".into(),
                name: "todo".into(),
            },
            crate::notepad::NotepadFile {
                path: "/notes/ideas.md".into(),
                name: "ideas".into(),
            },
        ]);
        app.notepad.set_body("- ship the notepad\n- take a break");

        crate::ui::compute_view(&mut app, Rect::new(0, 0, WIDTH, HEIGHT));
        let sidebar = app.view.sidebar_rect;
        assert!(sidebar.width > 0, "the wide layout keeps a sidebar");
        assert_eq!(app.view.notepad_rect.height, 6);
        assert_eq!(app.view.notepad_rect.bottom(), sidebar.bottom() - 1);
        assert_eq!(app.view.notepad_tab_hit_areas.len(), 2);

        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("test terminal");
        terminal
            .draw(|frame| crate::ui::render(&app, frame))
            .expect("render");
        let rows = buffer_text(&terminal);
        let panel_rows = &rows
            [usize::from(app.view.notepad_rect.y)..usize::from(app.view.notepad_rect.bottom())];
        assert!(
            panel_rows[0].contains("todo") && panel_rows[0].contains("ideas"),
            "header lists the notes: {:?}",
            panel_rows[0]
        );
        assert!(
            panel_rows
                .iter()
                .any(|row| row.contains("- ship the notepad")),
            "body is drawn: {panel_rows:?}"
        );
    }

    #[test]
    fn the_workspace_list_gives_up_exactly_the_rows_the_panel_takes() {
        let mut app = AppState::test_new();
        let sidebar = Rect::new(0, 1, 26, 30);
        let without = crate::ui::workspace_list_rect_for_app(&app, sidebar);
        app.notepad.enabled = true;
        app.notepad.height = 8;
        let with = crate::ui::workspace_list_rect_for_app(&app, sidebar);
        assert_eq!(without.height - with.height, 8);
        assert_eq!(with.y, without.y);
    }
    /// The focused pane, the home composer and the notepad all claim the one
    /// host cursor, and the last claim of the frame wins. The notepad takes
    /// keys ahead of both, so it has to win.
    #[test]
    fn the_notepad_caret_outranks_every_other_cursor_claim() {
        const WIDTH: u16 = 120;
        const HEIGHT: u16 = 40;
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("alpha")];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.notepad.enabled = true;
        app.notepad.height = 6;
        app.notepad.set_files(vec![crate::notepad::NotepadFile {
            path: "/notes/todo.md".into(),
            name: "todo".into(),
        }]);
        app.notepad.set_body("- ship the caret");
        app.notepad.focused = true;
        app.notepad.cursor_line = 0;
        app.notepad.cursor_col = "- ship".chars().count();

        crate::ui::compute_view(&mut app, Rect::new(0, 0, WIDTH, HEIGHT));
        let expected = notepad_caret_position(&app, app.view.notepad_rect).expect("caret");
        let body = notepad_body_rect(app.view.notepad_rect);
        assert_eq!(expected, (body.x + 6, body.y));

        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("test terminal");
        terminal
            .draw(|frame| crate::ui::render(&app, frame))
            .expect("render");
        let caret = terminal.get_cursor_position().expect("host cursor");
        assert_eq!((caret.x, caret.y), expected);
    }

    #[test]
    fn an_unfocused_or_unreachable_notepad_claims_no_caret() {
        let mut app = AppState::test_new();
        app.notepad.enabled = true;
        app.notepad.height = 8;
        app.notepad.set_body("note");
        let panel = notepad_panel_rect(&app, Rect::new(0, 1, 26, 40));

        assert!(notepad_caret_position(&app, panel).is_none());

        app.notepad.focused = true;
        assert!(notepad_caret_position(&app, panel).is_some());

        // A caret scrolled out of the panel's rows is not drawn at its edge.
        app.notepad.cursor_line = 99;
        assert!(notepad_caret_position(&app, panel).is_none());

        // The error row replaces the body, so there is nothing to point at.
        app.notepad.cursor_line = 0;
        app.notepad.error = Some("cannot save".into());
        assert!(notepad_caret_position(&app, panel).is_none());

        // No panel, no caret.
        app.notepad.error = None;
        assert!(notepad_caret_position(&app, Rect::default()).is_none());
    }
}
