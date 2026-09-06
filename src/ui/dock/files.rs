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
use crate::files::{FileTreeRow, FileTreeRowKind, FileTreeSnapshot, FileTreeSource};

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

/// Rows the tree gives up to the header: the search line, plus the fallback
/// banner when the listing did not come from git.
pub(crate) fn header_rows(app: &AppState) -> u16 {
    1 + u16::from(
        active_snapshot(app).is_some_and(|snapshot| snapshot.source == FileTreeSource::Directory),
    )
}

pub(crate) fn row_hit_areas(app: &AppState, area: Rect) -> Vec<DockFileRowHitArea> {
    let header = header_rows(app);
    if area.height <= header || area.width == 0 {
        return Vec::new();
    }
    if active_snapshot(app).is_some_and(|snapshot| snapshot.error.is_some()) {
        return Vec::new();
    }
    let rows = visible_rows(app);
    let height = usize::from(area.height - header);
    let scroll = usize::from(app.dock_scroll).min(rows.len().saturating_sub(height));
    rows.into_iter()
        .skip(scroll)
        .take(height)
        .enumerate()
        .map(|(index, row)| DockFileRowHitArea {
            path: row.path,
            kind: row.kind,
            rect: Rect::new(area.x, area.y + header + index as u16, area.width, 1),
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
    let Some(snapshot) = active_snapshot(app) else {
        render_message(app, frame, area, 1, "loading files…");
        return;
    };
    if snapshot.source == FileTreeSource::Directory {
        render_message(
            app,
            frame,
            area,
            1,
            &fallback_notice(&snapshot.root, area.width),
        );
    }
    let header = header_rows(app);
    if let Some(error) = snapshot.error.as_deref() {
        render_message(app, frame, area, header, error);
        return;
    }
    if area.height <= header {
        return;
    }
    let rows = visible_rows(app);
    if rows.is_empty() {
        let message = if app.dock_files_filter.is_empty() {
            "no files"
        } else {
            "no matching files"
        };
        render_message(app, frame, area, header, message);
        return;
    }

    let height = usize::from(area.height - header);
    let scroll = usize::from(app.dock_scroll).min(rows.len().saturating_sub(height));
    for (index, row) in rows.iter().skip(scroll).take(height).enumerate() {
        render_row(
            app,
            frame,
            Rect::new(area.x, area.y + header + index as u16, area.width, 1),
            row,
        );
    }
}

/// What the surface says when the pane cwd is not a git repository. The dock is
/// narrow, so the notice gives up the head of the path, then the path itself,
/// before it gives up the sentence.
pub(crate) fn fallback_notice(root: &Path, width: u16) -> String {
    const LEAD: &str = "not a git repository";
    // The row is drawn with one leading space.
    let width = usize::from(width).saturating_sub(1);
    let directory = root.to_string_lossy().into_owned();
    let name = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| directory.clone());

    let full = format!("{LEAD} · showing {directory}");
    if full.chars().count() <= width {
        return full;
    }
    if let Some(notice) = with_tail(&format!("{LEAD} · showing …"), &directory, width) {
        return notice;
    }
    let named = format!("{LEAD} · {name}");
    if named.chars().count() <= width {
        return named;
    }
    if let Some(notice) = with_tail(&format!("{LEAD} · …"), &name, width) {
        return notice;
    }
    LEAD.chars().take(width).collect()
}

/// `prefix` followed by as much of `value`'s tail as `width` allows, or `None`
/// when that would leave less than three characters of it.
fn with_tail(prefix: &str, value: &str, width: usize) -> Option<String> {
    let room = width.saturating_sub(prefix.chars().count());
    if room < 3 {
        return None;
    }
    let skip = value.chars().count().saturating_sub(room);
    Some(format!(
        "{prefix}{}",
        value.chars().skip(skip).collect::<String>()
    ))
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

fn render_message(app: &AppState, frame: &mut Frame, area: Rect, row: u16, message: &str) {
    if row >= area.height {
        return;
    }
    frame.render_widget(
        Paragraph::new(format!(" {message}")).style(Style::default().fg(app.palette.overlay0)),
        Rect::new(area.x, area.y + row, area.width, 1),
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

    fn app_with(snapshot: FileTreeSnapshot) -> AppState {
        let mut app = AppState::test_new();
        app.dock_collapsed = false;
        app.dock_tab = Some(crate::app::DockSurface::Files);
        app.dock_files_root = Some(snapshot.root.clone());
        app.dock_file_cache.insert(snapshot.root.clone(), snapshot);
        app
    }

    fn walk_snapshot(error: Option<&str>) -> FileTreeSnapshot {
        FileTreeSnapshot {
            root: PathBuf::from("/home/agent"),
            files: vec![crate::files::FileRecord {
                path: PathBuf::from("notes.md"),
                status: None,
            }],
            fingerprint: 1,
            source: FileTreeSource::Directory,
            error: error.map(str::to_string),
        }
    }

    fn rendered(app: &AppState, area: Rect) -> Vec<String> {
        let backend = ratatui::backend::TestBackend::new(area.width, area.height);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render_files(app, frame, area))
            .expect("render files");
        let buffer = terminal.backend().buffer().clone();
        (0..area.height)
            .map(|row| {
                (0..area.width)
                    .map(|col| buffer[(col, row)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn a_cwd_outside_a_repository_says_so_above_the_walked_tree() {
        let app = app_with(walk_snapshot(None));
        let area = Rect::new(0, 0, 60, 6);

        let screen = rendered(&app, area);

        assert_eq!(
            screen[1].trim_end(),
            " not a git repository · showing /home/agent"
        );
        assert!(
            screen[2].contains("notes.md"),
            "the walked tree follows the notice: {screen:?}"
        );
        assert_eq!(
            row_hit_areas(&app, area).first().map(|hit| hit.rect.y),
            Some(2),
            "the tree rows move down with the notice"
        );
    }

    #[test]
    fn a_walk_that_failed_shows_the_error_instead_of_the_spinner() {
        let app = app_with(walk_snapshot(Some("listing timed out after 5s")));
        let area = Rect::new(0, 0, 40, 6);

        let screen = rendered(&app, area);

        assert_eq!(screen[2].trim_end(), " listing timed out after 5s");
        assert!(
            !screen.iter().any(|line| line.contains("loading files…")),
            "the spinner is gone: {screen:?}"
        );
        assert!(row_hit_areas(&app, area).is_empty());
    }

    #[test]
    fn the_fallback_notice_keeps_the_tail_of_a_long_path() {
        let notice = fallback_notice(Path::new("/home/agent/deeply/nested/place"), 44);

        assert!(notice.chars().count() <= 43, "{notice:?}");
        assert!(notice.starts_with("not a git repository · showing …"));
        assert!(notice.ends_with("place"), "{notice:?}");

        // Too narrow for the sentence and the path: the directory name stays.
        let narrow = fallback_notice(Path::new("/home/agent/deeply/nested/place"), 30);
        assert_eq!(narrow, "not a git repository · place");
        let narrower = fallback_notice(Path::new("/home/agent/deeply/nested/place"), 28);
        assert_eq!(narrower, "not a git repository · …ace");
        let tiny = fallback_notice(Path::new("/home/agent/deeply/nested/place"), 12);
        assert_eq!(tiny, "not a git r");
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
                source: crate::files::FileTreeSource::Git,
                error: None,
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
