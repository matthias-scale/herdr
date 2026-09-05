use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::diff::{DiffLine, DiffLineKind};
use crate::app::state::{AppState, DiffCacheEntry};

const WHITESPACE_TOGGLE_WIDTH: u16 = 8;

pub(super) fn render_diff(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(key) = app.dock_diff_active_key.as_ref() else {
        frame.render_widget(
            Paragraph::new(" loading diff…").style(Style::default().fg(app.palette.overlay1)),
            area,
        );
        return;
    };
    let Some(entry) = app.dock_diff_cache.get(key) else {
        return;
    };
    if let Some(error) = entry.error.as_deref() {
        frame.render_widget(
            Paragraph::new(format!(" diff unavailable: {error}"))
                .style(Style::default().fg(app.palette.red)),
            area,
        );
        return;
    }

    let additions = entry.files.iter().map(|file| file.additions).sum::<usize>();
    let deletions = entry.files.iter().map(|file| file.deletions).sum::<usize>();
    let file_word = if entry.files.len() == 1 {
        "file"
    } else {
        "files"
    };
    let whitespace = if app.dock_diff_ignore_whitespace {
        "[w -w]"
    } else {
        "[w all]"
    };
    let mut lines = vec![Line::from(Span::styled(
        format!(
            " base {} ← {}   {} {file_word} +{additions} −{deletions}",
            key.base,
            entry.branch,
            entry.files.len()
        ),
        Style::default()
            .fg(app.palette.text)
            .add_modifier(Modifier::BOLD),
    ))];

    for (index, file) in entry.files.iter().enumerate() {
        let collapsed = app.dock_diff_collapsed.contains(&file.path);
        let marker = if collapsed { '▸' } else { '▾' };
        let counts = if file.binary {
            "binary".to_string()
        } else {
            format!("+{} −{}", file.additions, file.deletions)
        };
        let style = if index == app.dock_diff_selected && app.dock_diff_focused {
            Style::default()
                .fg(app.palette.text)
                .bg(app.palette.surface0)
        } else {
            Style::default().fg(app.palette.text)
        };
        lines.push(Line::from(Span::styled(
            format!(" {marker} {}   {counts}", file.display_path),
            style,
        )));
        if collapsed {
            continue;
        }
        match entry.contents.get(&file.path) {
            Some(content) => {
                lines.extend(
                    content
                        .committed
                        .iter()
                        .map(|line| styled_diff_line(app, line)),
                );
                if !content.uncommitted.is_empty() {
                    lines.push(Line::from(Span::styled(
                        "   uncommitted",
                        Style::default()
                            .fg(app.palette.overlay1)
                            .add_modifier(Modifier::BOLD),
                    )));
                    lines.extend(
                        content
                            .uncommitted
                            .iter()
                            .map(|line| styled_diff_line(app, line)),
                    );
                }
            }
            None => lines.push(Line::from(Span::styled(
                "   loading…",
                Style::default().fg(app.palette.overlay0),
            ))),
        }
    }
    if entry.files.is_empty() {
        lines.push(Line::from(Span::styled(
            " no changes",
            Style::default().fg(app.palette.overlay1),
        )));
    }
    frame.render_widget(Paragraph::new(lines).scroll((app.dock_scroll, 0)), area);
    frame.render_widget(
        Paragraph::new(whitespace)
            .alignment(Alignment::Right)
            .style(Style::default().fg(app.palette.overlay1)),
        Rect::new(
            area.x
                .saturating_add(area.width.saturating_sub(WHITESPACE_TOGGLE_WIDTH)),
            area.y,
            WHITESPACE_TOGGLE_WIDTH.min(area.width),
            1,
        ),
    );
}

fn styled_diff_line(app: &AppState, line: &DiffLine) -> Line<'static> {
    let color = match line.kind {
        DiffLineKind::Added => app.palette.green,
        DiffLineKind::Removed => app.palette.red,
        DiffLineKind::Hunk | DiffLineKind::Binary => app.palette.overlay1,
        DiffLineKind::Context => app.palette.text,
    };
    Line::from(Span::styled(
        format!("   {}", line.text),
        Style::default().fg(color),
    ))
}

pub(crate) fn whitespace_toggle_at(area: Rect, col: u16, row: u16) -> bool {
    row == area.y
        && col
            >= area
                .x
                .saturating_add(area.width.saturating_sub(WHITESPACE_TOGGLE_WIDTH))
        && col < area.right()
}

pub(crate) fn file_index_at(app: &AppState, area: Rect, col: u16, row: u16) -> Option<usize> {
    if col < area.x || col >= area.right() || row < area.y || row >= area.bottom() {
        return None;
    }
    let key = app.dock_diff_active_key.as_ref()?;
    let entry = app.dock_diff_cache.get(key)?;
    let wanted = usize::from(row.saturating_sub(area.y)) + usize::from(app.dock_scroll);
    let mut display_row = 1_usize;
    for (index, file) in entry.files.iter().enumerate() {
        if display_row == wanted {
            return Some(index);
        }
        display_row += file_display_height(app, entry, &file.path);
    }
    None
}

fn file_display_height(app: &AppState, entry: &DiffCacheEntry, path: &str) -> usize {
    if app.dock_diff_collapsed.contains(path) {
        return 1;
    }
    let content_rows = entry.contents.get(path).map_or(1, |content| {
        content.committed.len()
            + content.uncommitted.len()
            + usize::from(!content.uncommitted.is_empty())
    });
    1 + content_rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::{DiffCacheKey, DiffFileContent, DiffFileSummary};
    use ratatui::{backend::TestBackend, Terminal};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn diff_app() -> AppState {
        let mut app = AppState::test_new();
        let key = DiffCacheKey {
            root: PathBuf::from("/repo"),
            base: "main".into(),
            ignore_whitespace: false,
        };
        app.dock_diff_cache.insert(
            key.clone(),
            DiffCacheEntry {
                branch: "feature".into(),
                files: vec![
                    DiffFileSummary {
                        path: "one.rs".into(),
                        display_path: "one.rs".into(),
                        additions: 1,
                        deletions: 0,
                        binary: false,
                    },
                    DiffFileSummary {
                        path: "two.rs".into(),
                        display_path: "two.rs".into(),
                        additions: 0,
                        deletions: 1,
                        binary: false,
                    },
                ],
                contents: HashMap::from([(
                    "one.rs".into(),
                    DiffFileContent {
                        committed: vec![DiffLine {
                            text: "+one".into(),
                            kind: DiffLineKind::Added,
                        }],
                        uncommitted: Vec::new(),
                    },
                )]),
                error: None,
            },
        );
        app.dock_diff_active_key = Some(key);
        app
    }

    #[test]
    fn collapsed_file_does_not_hide_the_next_file_row() {
        let mut app = diff_app();
        let area = Rect::new(10, 3, 40, 10);
        assert_eq!(file_index_at(&app, area, 12, 6), Some(1));
        app.dock_diff_collapsed.insert("one.rs".into());
        assert_eq!(file_index_at(&app, area, 12, 5), Some(1));
        assert_eq!(file_index_at(&app, area, 12, 6), None);
    }

    #[test]
    fn diff_renderer_shows_header_sections_and_hunk_palette_colors() {
        let mut app = diff_app();
        let key = app.dock_diff_active_key.clone().expect("active diff");
        let entry = app.dock_diff_cache.get_mut(&key).expect("cached diff");
        entry.contents.insert(
            "one.rs".into(),
            DiffFileContent {
                committed: vec![
                    DiffLine {
                        text: "@@ -1 +1 @@".into(),
                        kind: DiffLineKind::Hunk,
                    },
                    DiffLine {
                        text: "-old".into(),
                        kind: DiffLineKind::Removed,
                    },
                    DiffLine {
                        text: "+new".into(),
                        kind: DiffLineKind::Added,
                    },
                ],
                uncommitted: vec![DiffLine {
                    text: "+later".into(),
                    kind: DiffLineKind::Added,
                }],
            },
        );
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render_diff(&app, frame, Rect::new(0, 0, 80, 12)))
            .expect("render diff");
        let buffer = terminal.backend().buffer();
        let row_text = |row| {
            (0..80)
                .map(|col| buffer[(col, row)].symbol())
                .collect::<String>()
        };

        assert!(row_text(0).contains("base main ← feature   2 files +1 −1"));
        assert!(row_text(1).contains("▾ one.rs   +1 −0"));
        assert!(row_text(5).contains("uncommitted"));
        assert_eq!(buffer[(3, 2)].fg, app.palette.overlay1);
        assert_eq!(buffer[(3, 3)].fg, app.palette.red);
        assert_eq!(buffer[(3, 4)].fg, app.palette.green);
        assert_eq!(buffer[(3, 6)].fg, app.palette.green);
    }
}
