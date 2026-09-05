use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::state::{AppState, DockFileRowHitArea};
use crate::files::{FileTreeRow, FileTreeRowKind, FileTreeSnapshot};

pub(crate) fn active_snapshot(app: &AppState) -> Option<&FileTreeSnapshot> {
    let root = app.dock_files_root.as_ref()?;
    app.dock_file_cache.get(root)
}

pub(crate) fn visible_rows(app: &AppState) -> Vec<FileTreeRow> {
    let Some(snapshot) = active_snapshot(app) else {
        return Vec::new();
    };
    let matched = matched_files(snapshot, &app.dock_files_filter);
    snapshot.rows(&app.dock_files_collapsed, matched.as_ref())
}

fn matched_files(snapshot: &FileTreeSnapshot, query: &str) -> Option<HashSet<PathBuf>> {
    if query.is_empty() {
        return None;
    }
    let paths = snapshot
        .files
        .iter()
        .map(|file| file.path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    Some(
        crate::ui::dropdown::filter_items(&paths, query)
            .into_iter()
            .filter_map(|(index, _)| snapshot.files.get(index).map(|file| file.path.clone()))
            .collect(),
    )
}

pub(crate) fn row_hit_areas(app: &AppState, area: Rect) -> Vec<DockFileRowHitArea> {
    if area.height <= 1 || area.width == 0 {
        return Vec::new();
    }
    let rows = visible_rows(app);
    let height = usize::from(area.height - 1);
    let scroll = usize::from(app.dock_scroll).min(rows.len().saturating_sub(height));
    rows.into_iter()
        .skip(scroll)
        .take(usize::from(area.height - 1))
        .enumerate()
        .map(|(index, row)| DockFileRowHitArea {
            path: row.path,
            kind: row.kind,
            rect: Rect::new(area.x, area.y + 1 + index as u16, area.width, 1),
        })
        .collect()
}

pub(super) fn render_files(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    render_search(app, frame, Rect::new(area.x, area.y, area.width, 1));
    if area.height <= 1 {
        return;
    }
    let rows = visible_rows(app);
    if active_snapshot(app).is_none() {
        render_message(app, frame, area, "loading files…");
        return;
    }
    if rows.is_empty() {
        let message = if app.dock_files_filter.is_empty() {
            "no files"
        } else {
            "no matching files"
        };
        render_message(app, frame, area, message);
        return;
    }

    let height = usize::from(area.height - 1);
    let scroll = usize::from(app.dock_scroll).min(rows.len().saturating_sub(height));
    for (index, row) in rows
        .iter()
        .skip(scroll)
        .take(usize::from(area.height - 1))
        .enumerate()
    {
        render_row(
            app,
            frame,
            Rect::new(area.x, area.y + 1 + index as u16, area.width, 1),
            row,
        );
    }
}

fn render_search(app: &AppState, frame: &mut Frame, area: Rect) {
    let text = if app.dock_files_filter.is_empty() {
        " ⟳ / search files…".to_string()
    } else {
        format!(" ⟳ / {}", app.dock_files_filter)
    };
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(if app.dock_files_filter.is_empty() {
            app.palette.overlay0
        } else {
            app.palette.text
        })),
        area,
    );
}

fn render_message(app: &AppState, frame: &mut Frame, area: Rect, message: &str) {
    frame.render_widget(
        Paragraph::new(format!(" {message}")).style(Style::default().fg(app.palette.overlay0)),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );
}

fn render_row(app: &AppState, frame: &mut Frame, area: Rect, row: &FileTreeRow) {
    let selected = app.dock_files_selection.as_ref() == Some(&row.path);
    let style = if selected && app.dock_files_focused {
        Style::default()
            .fg(app.palette.text)
            .bg(app.palette.surface0)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(app.palette.text)
    };
    let indent = "  ".repeat(row.depth);
    let (marker, name) = match row.kind {
        FileTreeRowKind::Directory => {
            let marker = if app.dock_files_filter.is_empty()
                && app.dock_files_collapsed.contains(&row.path)
            {
                "▸ "
            } else {
                "▾ "
            };
            (marker.to_string(), file_name(&row.path))
        }
        FileTreeRowKind::File => (
            format!("{} ", badge(&row.path, app.files_icons)),
            file_name(&row.path),
        ),
    };
    let gutter = row.status.map(|status| status.gutter()).unwrap_or(' ');
    let reserved = 2usize;
    let available = usize::from(area.width).saturating_sub(reserved);
    let prefix = format!(" {indent}{marker}");
    let name_width = available.saturating_sub(prefix.chars().count());
    let name = truncate(&name, name_width);
    let mut spans = vec![Span::styled(format!("{prefix}{name}"), style)];
    let used = spans[0].content.chars().count();
    spans.push(Span::raw(" ".repeat(available.saturating_sub(used))));
    spans.push(Span::styled(
        gutter.to_string(),
        Style::default().fg(match gutter {
            'A' => app.palette.green,
            '?' => app.palette.yellow,
            'M' => app.palette.peach,
            _ => app.palette.overlay0,
        }),
    ));
    spans.push(Span::raw(" "));
    frame.render_widget(Paragraph::new(Line::from(spans)).style(style), area);
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn truncate(value: &str, width: usize) -> String {
    value.chars().take(width).collect()
}

pub(crate) fn badge(path: &Path, icons: crate::config::FilesIconConfig) -> &'static str {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if icons == crate::config::FilesIconConfig::Nerd {
        return match extension.as_str() {
            "rs" => "",
            "md" => "󰍔",
            "json" | "toml" | "yaml" | "yml" => "",
            "sh" => "",
            "ts" | "tsx" => "",
            "py" => "",
            _ => "󰈔",
        };
    }
    match extension.as_str() {
        "rs" => "rs",
        "md" => "md",
        "json" | "toml" | "yaml" | "yml" => "{}",
        "sh" => "sh",
        "ts" | "tsx" => "ts",
        "py" => "py",
        _ => "··",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badges_are_portable_until_nerd_icons_are_opted_in() {
        assert_eq!(
            badge(
                Path::new("src/lib.rs"),
                crate::config::FilesIconConfig::Badges
            ),
            "rs"
        );
        assert_eq!(
            badge(
                Path::new("Cargo.toml"),
                crate::config::FilesIconConfig::Badges
            ),
            "{}"
        );
        assert_eq!(
            badge(
                Path::new("image.bin"),
                crate::config::FilesIconConfig::Badges
            ),
            "··"
        );
        assert_ne!(
            badge(
                Path::new("src/lib.rs"),
                crate::config::FilesIconConfig::Nerd
            ),
            "rs"
        );
    }

    #[test]
    fn search_uses_subsequence_filter_and_expands_matching_parents() {
        let mut app = AppState::test_new();
        let root = PathBuf::from("/repo");
        app.dock_files_root = Some(root.clone());
        app.dock_file_cache.insert(
            root.clone(),
            FileTreeSnapshot {
                root,
                files: vec![
                    crate::files::FileRecord {
                        path: PathBuf::from("src/ui/sidebar.rs"),
                        status: None,
                    },
                    crate::files::FileRecord {
                        path: PathBuf::from("docs/readme.md"),
                        status: None,
                    },
                ],
                fingerprint: 1,
            },
        );
        app.dock_files_collapsed.insert(PathBuf::from("src"));
        app.dock_files_filter = "sbr".to_string();

        let rows = visible_rows(&app);

        assert_eq!(
            rows.iter()
                .map(|row| row.path.as_path())
                .collect::<Vec<_>>(),
            vec![
                Path::new("src"),
                Path::new("src/ui"),
                Path::new("src/ui/sidebar.rs")
            ]
        );
    }
}
