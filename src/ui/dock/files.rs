use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
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
    snapshot.rows_sorted(
        &app.dock_files_collapsed,
        matched.as_ref(),
        app.dock_files_sort,
    )
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
    let (refresh, sort) = header_hit_areas(app, area);
    let text = if app.dock_files_filter.is_empty() {
        "/ search files…".to_string()
    } else {
        format!("/ {}", app.dock_files_filter)
    };
    frame.render_widget(
        Paragraph::new(" ⟳ ").style(Style::default().fg(app.palette.overlay0)),
        refresh,
    );
    let search = Rect::new(
        refresh.x.saturating_add(refresh.width),
        area.y,
        area.width
            .saturating_sub(refresh.width)
            .saturating_sub(sort.width),
        1,
    );
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(if app.dock_files_filter.is_empty() {
            app.palette.overlay0
        } else {
            app.palette.text
        })),
        search,
    );
    if sort.width > 0 {
        frame.render_widget(
            Paragraph::new(format!(" ⇅ {} ", app.dock_files_sort.label()))
                .style(Style::default().fg(app.palette.overlay0)),
            sort,
        );
    }
}

pub(crate) fn header_hit_areas(app: &AppState, area: Rect) -> (Rect, Rect) {
    if area.width == 0 || area.height == 0 {
        return (Rect::default(), Rect::default());
    }
    let refresh = Rect::new(area.x, area.y, area.width.min(3), 1);
    let label = format!(" ⇅ {} ", app.dock_files_sort.label());
    let sort_width = u16::try_from(crate::ui::text::display_width(&label)).unwrap_or(u16::MAX);
    let sort = if area.width >= refresh.width.saturating_add(sort_width) {
        Rect::new(
            area.x.saturating_add(area.width.saturating_sub(sort_width)),
            area.y,
            sort_width,
            1,
        )
    } else {
        Rect::default()
    };
    (refresh, sort)
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
    let collapsed = row.kind == FileTreeRowKind::Directory
        && app.dock_files_filter.is_empty()
        && app.dock_files_collapsed.contains(&row.path);
    let disclosure = match row.kind {
        FileTreeRowKind::Directory if collapsed => "▸ ",
        FileTreeRowKind::Directory => "▾ ",
        FileTreeRowKind::File | FileTreeRowKind::Symlink => "  ",
    };
    let kind = file_icon_kind(&row.path, row.kind);
    let nerd = app.nerd_font && app.files_icons == crate::config::FilesIconConfig::Nerd;
    let icon = file_icon(kind, nerd, !collapsed);
    let name = file_name(&row.path);
    let gutter = row.status.map(|status| status.gutter()).unwrap_or(' ');
    let reserved = 2usize;
    let available = usize::from(area.width).saturating_sub(reserved);
    let prefix = format!(" {indent}{disclosure}");
    let fixed_width = crate::ui::text::display_width(&prefix)
        .saturating_add(crate::ui::text::display_width(icon))
        .saturating_add(1);
    let name = crate::ui::text::truncate_end(&name, available.saturating_sub(fixed_width));
    let mut spans = vec![
        Span::styled(prefix, style),
        Span::styled(icon, style.fg(file_icon_color(kind, app))),
        Span::styled(format!(" {name}"), style),
    ];
    let used = spans
        .iter()
        .map(|span| crate::ui::text::display_width(span.content.as_ref()))
        .sum::<usize>();
    spans.push(Span::raw(" ".repeat(available.saturating_sub(used))));
    spans.push(Span::styled(
        gutter.to_string(),
        Style::default().fg(match gutter {
            'A' => app.palette.green,
            '?' => app.palette.yellow,
            'M' => app.palette.peach,
            'D' => app.palette.red,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileIconKind {
    Directory,
    Symlink,
    Rust,
    Toml,
    Markdown,
    Json,
    TypeScript,
    Python,
    Lock,
    Dotfile,
    File,
}

pub(crate) fn file_icon_kind(path: &Path, row_kind: FileTreeRowKind) -> FileIconKind {
    if row_kind == FileTreeRowKind::Directory {
        return FileIconKind::Directory;
    }
    if row_kind == FileTreeRowKind::Symlink {
        return FileIconKind::Symlink;
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if name.eq_ignore_ascii_case("Cargo.lock") {
        return FileIconKind::Lock;
    }
    if name.starts_with('.') {
        return FileIconKind::Dotfile;
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "rs" => FileIconKind::Rust,
        "toml" => FileIconKind::Toml,
        "md" | "markdown" => FileIconKind::Markdown,
        "json" | "yaml" | "yml" => FileIconKind::Json,
        "ts" | "tsx" => FileIconKind::TypeScript,
        "py" => FileIconKind::Python,
        _ => FileIconKind::File,
    }
}

pub(crate) fn file_icon(kind: FileIconKind, nerd: bool, expanded: bool) -> &'static str {
    if nerd {
        return match kind {
            FileIconKind::Directory if expanded => "",
            FileIconKind::Directory => "",
            FileIconKind::Symlink => "",
            FileIconKind::Rust => "",
            FileIconKind::Toml => "",
            FileIconKind::Markdown => "󰍔",
            FileIconKind::Json => "",
            FileIconKind::TypeScript => "",
            FileIconKind::Python => "",
            FileIconKind::Lock => "󰌾",
            FileIconKind::Dotfile => "󰘓",
            FileIconKind::File => "󰈔",
        };
    }
    match kind {
        FileIconKind::Directory => "d ",
        FileIconKind::Symlink => "->",
        FileIconKind::Rust => "rs",
        FileIconKind::Toml => "tm",
        FileIconKind::Markdown => "md",
        FileIconKind::Json => "{}",
        FileIconKind::TypeScript => "ts",
        FileIconKind::Python => "py",
        FileIconKind::Lock => "lk",
        FileIconKind::Dotfile => ". ",
        FileIconKind::File => "--",
    }
}

fn file_icon_color(kind: FileIconKind, app: &AppState) -> Color {
    match kind {
        FileIconKind::Directory | FileIconKind::TypeScript => app.palette.blue,
        FileIconKind::Symlink => app.palette.teal,
        FileIconKind::Rust => app.palette.peach,
        FileIconKind::Toml => app.palette.red,
        FileIconKind::Markdown => app.palette.mauve,
        FileIconKind::Json | FileIconKind::Python => app.palette.yellow,
        FileIconKind::Lock => app.palette.green,
        FileIconKind::Dotfile => app.palette.overlay1,
        FileIconKind::File => app.palette.text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_mapping_covers_names_extensions_directories_and_symlinks() {
        let cases = [
            (
                "src",
                FileTreeRowKind::Directory,
                FileIconKind::Directory,
                "d ",
            ),
            (
                "link",
                FileTreeRowKind::Symlink,
                FileIconKind::Symlink,
                "->",
            ),
            ("lib.rs", FileTreeRowKind::File, FileIconKind::Rust, "rs"),
            (
                "Cargo.toml",
                FileTreeRowKind::File,
                FileIconKind::Toml,
                "tm",
            ),
            (
                "README.md",
                FileTreeRowKind::File,
                FileIconKind::Markdown,
                "md",
            ),
            ("data.json", FileTreeRowKind::File, FileIconKind::Json, "{}"),
            (
                "app.ts",
                FileTreeRowKind::File,
                FileIconKind::TypeScript,
                "ts",
            ),
            ("tool.py", FileTreeRowKind::File, FileIconKind::Python, "py"),
            (
                "Cargo.lock",
                FileTreeRowKind::File,
                FileIconKind::Lock,
                "lk",
            ),
            (
                ".gitignore",
                FileTreeRowKind::File,
                FileIconKind::Dotfile,
                ". ",
            ),
        ];
        for (path, row_kind, icon_kind, ascii) in cases {
            assert_eq!(
                file_icon_kind(Path::new(path), row_kind),
                icon_kind,
                "{path}"
            );
            assert_eq!(file_icon(icon_kind, false, false), ascii, "{path}");
            assert_ne!(file_icon(icon_kind, true, false), ascii, "{path}");
        }
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
                kind: FileTreeRowKind::File,
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
                        kind: FileTreeRowKind::File,
                    },
                    crate::files::FileRecord {
                        path: PathBuf::from("docs/readme.md"),
                        status: None,
                        kind: FileTreeRowKind::File,
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

    #[test]
    fn files_header_exposes_refresh_and_sort_hit_areas() {
        let app = AppState::test_new();
        let area = Rect::new(20, 4, 42, 8);

        let (refresh, sort) = header_hit_areas(&app, area);
        let screen = rendered(&app, Rect::new(0, 0, 42, 2));

        assert_eq!(refresh, Rect::new(20, 4, 3, 1));
        assert_eq!(sort.y, 4);
        assert_eq!(sort.x.saturating_add(sort.width), 62);
        assert!(screen[0].contains("⟳"));
        assert!(screen[0].contains("⇅ name"));
    }
}
