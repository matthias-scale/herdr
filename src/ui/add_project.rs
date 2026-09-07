use std::path::{Path, PathBuf};

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph, Wrap},
    Frame,
};

use crate::app::home::{AddProjectState, AddProjectTab};
use crate::app::AppState;

use super::dropdown::{layout_dropdown, DropdownLayout, DropdownSpec};
use super::text::{display_width, display_width_u16, middle_elide, truncate_end};
use super::widgets::{
    centered_popup_rect, modal_stack_areas, panel_contrast_fg, render_modal_shell,
};

const POPUP_WIDTH: u16 = 78;
const POPUP_HEIGHT: u16 = 24;
const TITLE_PREFIX: &str = "Add project  ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BreadcrumbSegment {
    pub(crate) label: String,
    pub(crate) path: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AddProjectLayout {
    pub(crate) close: Rect,
    pub(crate) tabs: Vec<(AddProjectTab, Rect)>,
    pub(crate) input: Rect,
    pub(crate) hidden_toggle: Rect,
    pub(crate) breadcrumbs: Vec<(PathBuf, Rect)>,
    pub(crate) list: Option<DropdownLayout>,
}

pub(crate) fn folder_breadcrumbs(path: &Path, max_width: usize) -> Vec<BreadcrumbSegment> {
    let all = path
        .ancestors()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|ancestor| BreadcrumbSegment {
            label: ancestor
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| std::path::MAIN_SEPARATOR.to_string()),
            path: ancestor.to_path_buf(),
        })
        .collect::<Vec<_>>();
    let full_width = all
        .iter()
        .map(|segment| display_width(&segment.label))
        .sum::<usize>()
        .saturating_add(all.len().saturating_sub(1).saturating_mul(3));
    let mut visible = if full_width <= max_width || all.len() <= 2 {
        all
    } else {
        let mut tail = Vec::new();
        let root_width = display_width(&all[0].label);
        let mut used = root_width.saturating_add(6);
        for segment in all.iter().skip(1).rev() {
            let needed = display_width(&segment.label).saturating_add(3);
            if !tail.is_empty() && used.saturating_add(needed) > max_width {
                break;
            }
            tail.push(segment.clone());
            used = used.saturating_add(needed);
        }
        tail.reverse();
        let mut visible = vec![
            all[0].clone(),
            BreadcrumbSegment {
                label: "…".into(),
                path: tail
                    .first()
                    .and_then(|segment| segment.path.parent())
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| all[0].path.clone()),
            },
        ];
        visible.extend(tail);
        visible
    };
    let fixed_width = visible
        .iter()
        .take(visible.len().saturating_sub(1))
        .map(|segment| display_width(&segment.label))
        .sum::<usize>()
        .saturating_add(visible.len().saturating_sub(1).saturating_mul(3));
    if let Some(last) = visible.last_mut() {
        last.label = middle_elide(&last.label, max_width.saturating_sub(fixed_width));
    }
    visible
}

pub(crate) fn add_project_layout(
    area: Rect,
    project: Option<&AddProjectState>,
) -> AddProjectLayout {
    let Some(project) = project else {
        return AddProjectLayout::default();
    };
    let Some(popup) = centered_popup_rect(area, POPUP_WIDTH, POPUP_HEIGHT) else {
        return AddProjectLayout::default();
    };
    let inner = Rect::new(
        popup.x.saturating_add(1),
        popup.y.saturating_add(1),
        popup.width.saturating_sub(2),
        popup.height.saturating_sub(2),
    );
    if inner.width < 24 || inner.height < 10 {
        return AddProjectLayout::default();
    }
    let stack = modal_stack_areas(inner, 1, 1, 0, 1);
    let close = Rect::new(stack.header.right().saturating_sub(1), stack.header.y, 1, 1);
    let breadcrumb_width = stack
        .header
        .width
        .saturating_sub(display_width_u16(TITLE_PREFIX))
        .saturating_sub(2);
    let segments = folder_breadcrumbs(&project.browse.directory, breadcrumb_width as usize);
    let mut crumb_x = stack
        .header
        .x
        .saturating_add(display_width_u16(TITLE_PREFIX));
    let breadcrumbs = segments
        .into_iter()
        .enumerate()
        .map(|(index, segment)| {
            if index > 0 {
                crumb_x = crumb_x.saturating_add(3);
            }
            let width =
                display_width_u16(&segment.label).min(stack.header.right().saturating_sub(crumb_x));
            let rect = Rect::new(crumb_x, stack.header.y, width, 1);
            crumb_x = crumb_x.saturating_add(width);
            (segment.path, rect)
        })
        .collect();
    let tab_row = Rect::new(inner.x, inner.y.saturating_add(2), inner.width, 1);
    let tab_areas = Layout::horizontal([
        Constraint::Ratio(1, 3),
        Constraint::Ratio(1, 3),
        Constraint::Ratio(1, 3),
    ])
    .split(tab_row);
    let tabs = AddProjectTab::ALL
        .into_iter()
        .zip(tab_areas.iter().copied())
        .collect::<Vec<_>>();
    let input_y = inner.y.saturating_add(5);
    let hidden_width = if project.tab == AddProjectTab::LocalFolder {
        12.min(inner.width / 3)
    } else {
        0
    };
    let input = Rect::new(
        inner.x,
        input_y,
        inner
            .width
            .saturating_sub(hidden_width.saturating_add(u16::from(hidden_width > 0))),
        1,
    );
    let hidden_toggle = Rect::new(
        input.right().saturating_add(u16::from(hidden_width > 0)),
        input_y,
        hidden_width,
        1,
    );
    let list_bottom = stack.footer.map_or(inner.bottom(), |footer| footer.y);
    let list_area = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        list_bottom.saturating_sub(inner.y),
    );
    let (item_count, selected) = match project.tab {
        AddProjectTab::LocalFolder => (
            project.browse.entries.len().saturating_add(1),
            project.browse.selected.saturating_add(1),
        ),
        AddProjectTab::GitUrl => (0, 0),
        AddProjectTab::GitHub => (
            project.github_matches().len(),
            project.github_filter.selected,
        ),
    };
    let list = (project.tab != AddProjectTab::GitUrl)
        .then(|| {
            layout_dropdown(
                &DropdownSpec {
                    anchor: input,
                    item_count,
                    selected,
                    has_filter: false,
                    max_rows: usize::from(list_bottom.saturating_sub(input.bottom())),
                    min_width: inner.width,
                },
                list_area,
            )
        })
        .flatten();
    AddProjectLayout {
        close,
        tabs,
        input,
        hidden_toggle,
        breadcrumbs,
        list,
    }
}

pub(super) fn render_add_project_overlay(app: &AppState, frame: &mut Frame) {
    let Some(project) = app.home.as_ref().and_then(|home| home.add_project.as_ref()) else {
        return;
    };
    super::dim_background(frame, frame.area());
    let Some(inner) =
        render_modal_shell(frame, frame.area(), POPUP_WIDTH, POPUP_HEIGHT, &app.palette)
    else {
        return;
    };
    let layout = add_project_layout(frame.area(), Some(project));
    let mut title = vec![Span::styled(
        TITLE_PREFIX,
        Style::default()
            .fg(app.palette.text)
            .add_modifier(Modifier::BOLD),
    )];
    let breadcrumb_segments = folder_breadcrumbs(
        &project.browse.directory,
        inner
            .width
            .saturating_sub(display_width_u16(TITLE_PREFIX))
            .saturating_sub(2) as usize,
    );
    for (index, segment) in breadcrumb_segments.iter().enumerate() {
        if index > 0 {
            title.push(Span::styled(
                " › ",
                Style::default().fg(app.palette.overlay0),
            ));
        }
        title.push(Span::styled(
            segment.label.clone(),
            Style::default().fg(if index + 1 == breadcrumb_segments.len() {
                app.palette.text
            } else {
                app.palette.accent
            }),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(title)),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    frame.render_widget(
        Paragraph::new("×").style(Style::default().fg(app.palette.overlay1)),
        layout.close,
    );

    for (tab, rect) in &layout.tabs {
        let active = *tab == project.tab;
        let style = if active {
            Style::default()
                .fg(panel_contrast_fg(&app.palette))
                .bg(app.palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(app.palette.subtext0)
                .bg(app.palette.surface0)
        };
        frame.render_widget(
            Paragraph::new(format!(" {} ", tab.label())).style(style),
            *rect,
        );
    }

    let label = match project.tab {
        AddProjectTab::LocalFolder => "Filter",
        AddProjectTab::GitUrl => "Repository URL",
        AddProjectTab::GitHub => "Owner / search",
    };
    frame.render_widget(
        Paragraph::new(label).style(Style::default().fg(app.palette.overlay0)),
        Rect::new(
            layout.input.x,
            layout.input.y.saturating_sub(1),
            layout.input.width,
            1,
        ),
    );
    let value = match project.tab {
        AddProjectTab::LocalFolder => project.browse.filter.as_str(),
        AddProjectTab::GitUrl => project.git_url.as_str(),
        AddProjectTab::GitHub => project.github_query.as_str(),
    };
    let cursor = if project.clone_pending { "" } else { "█" };
    frame.render_widget(Clear, layout.input);
    frame.render_widget(
        Paragraph::new(format!(
            " {}{cursor}",
            truncate_end(value, layout.input.width.saturating_sub(2) as usize)
        ))
        .style(
            Style::default()
                .fg(app.palette.text)
                .bg(app.palette.surface1),
        ),
        layout.input,
    );
    if project.tab == AddProjectTab::LocalFolder {
        let style = if project.browse.show_hidden {
            Style::default()
                .fg(panel_contrast_fg(&app.palette))
                .bg(app.palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(app.palette.overlay1)
                .bg(app.palette.surface0)
        };
        frame.render_widget(
            Paragraph::new(" · hidden ").style(style),
            layout.hidden_toggle,
        );
    }

    if let Some(dropdown) = &layout.list {
        let rows = match project.tab {
            AddProjectTab::LocalFolder => Vec::new(),
            AddProjectTab::GitHub => project
                .github_matches()
                .into_iter()
                .map(|(_, repo)| repo)
                .collect::<Vec<_>>(),
            AddProjectTab::GitUrl => Vec::new(),
        };
        let lines = if project.tab == AddProjectTab::LocalFolder {
            let current_name = project
                .browse
                .directory
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| project.browse.directory.display().to_string());
            std::iter::once(Line::from(Span::styled(
                format!(
                    " ▾ 📁 {}",
                    middle_elide(
                        &current_name,
                        dropdown.list_rect.width.saturating_sub(7) as usize
                    )
                ),
                Style::default()
                    .fg(app.palette.subtext0)
                    .bg(app.palette.panel_bg),
            )))
            .chain(
                project
                    .browse
                    .entries
                    .iter()
                    .enumerate()
                    .map(|(index, entry)| {
                        let selected = index == project.browse.selected;
                        let prefix = if entry.is_dir {
                            "   ▸ 📁 "
                        } else {
                            "     · "
                        };
                        let suffix = entry
                            .branch
                            .as_deref()
                            .map(|branch| format!("  ⎇ {branch}"))
                            .unwrap_or_default();
                        let available = dropdown
                            .list_rect
                            .width
                            .saturating_sub(display_width_u16(prefix))
                            .saturating_sub(display_width_u16(&suffix))
                            as usize;
                        let text =
                            format!("{prefix}{}{suffix}", truncate_end(&entry.name, available));
                        let mut style = Style::default()
                            .fg(if entry.is_dir {
                                app.palette.subtext0
                            } else {
                                app.palette.overlay0
                            })
                            .bg(if selected {
                                app.palette.surface1
                            } else {
                                app.palette.panel_bg
                            });
                        if selected {
                            style = style.fg(app.palette.text).add_modifier(Modifier::BOLD);
                        } else if !entry.is_dir {
                            style = style.add_modifier(Modifier::DIM);
                        }
                        Line::from(Span::styled(text, style))
                    }),
            )
            .skip(dropdown.first_visible)
            .take(dropdown.visible_rows)
            .collect::<Vec<_>>()
        } else {
            rows.into_iter()
                .enumerate()
                .skip(dropdown.first_visible)
                .take(dropdown.visible_rows)
                .map(|(index, row)| {
                    let selected = project.tab == AddProjectTab::GitHub
                        && index == project.github_filter.selected;
                    Line::from(Span::styled(
                        format!(" {row}"),
                        if selected {
                            Style::default()
                                .fg(app.palette.text)
                                .bg(app.palette.surface1)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                                .fg(app.palette.subtext0)
                                .bg(app.palette.panel_bg)
                        },
                    ))
                })
                .collect::<Vec<_>>()
        };
        frame.render_widget(
            Paragraph::new(lines).style(Style::default().bg(app.palette.panel_bg)),
            dropdown.list_rect,
        );
    }

    let status = if let Some(error) = project.error.as_deref().or(project.browse.error.as_deref()) {
        Some((error, app.palette.red))
    } else if project.clone_pending {
        Some(("Cloning in bottom pane…", app.palette.accent))
    } else if project.github_loading && project.tab == AddProjectTab::GitHub {
        Some(("Loading repositories…", app.palette.overlay1))
    } else if project.tab == AddProjectTab::GitHub && project.github_repos.is_empty() {
        Some(("No repositories found", app.palette.overlay1))
    } else {
        None
    };
    if let Some((text, color)) = status {
        let status_area = Rect::new(inner.x, inner.bottom().saturating_sub(2), inner.width, 1);
        frame.render_widget(
            Paragraph::new(text)
                .style(Style::default().fg(color))
                .wrap(Wrap { trim: true }),
            status_area,
        );
    }
    frame.render_widget(
        Paragraph::new(if project.tab == AddProjectTab::LocalFolder {
            " Enter open · Space select · Backspace up · Esc cancel"
        } else {
            " ←/→ tabs  Enter select  Esc close"
        })
        .style(Style::default().fg(app.palette.overlay0)),
        Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::home::{FolderBrowser, FolderBrowserEntry};

    #[test]
    fn project_lists_open_below_the_input_at_both_capture_sizes() {
        for area in [Rect::new(0, 0, 120, 40), Rect::new(0, 0, 80, 24)] {
            let mut project = AddProjectState::starting_at(std::path::Path::new("/tmp"));
            project.browse = FolderBrowser {
                directory: "/tmp".into(),
                filter: String::new(),
                entries: vec![
                    FolderBrowserEntry {
                        name: "one".into(),
                        is_dir: true,
                        branch: None,
                    },
                    FolderBrowserEntry {
                        name: "two".into(),
                        is_dir: true,
                        branch: None,
                    },
                ],
                selected: 0,
                show_hidden: false,
                error: None,
            };
            let layout = add_project_layout(area, Some(&project));
            let list = layout.list.expect("local folder list");

            assert_eq!(list.rect.y, layout.input.bottom());
            assert!(list.rect.bottom() <= area.bottom());
        }
    }

    #[test]
    fn github_layout_uses_shared_filtered_selection() {
        let mut project = AddProjectState::starting_at(std::path::Path::new("/tmp"));
        project.tab = AddProjectTab::GitHub;
        project.github_repos = vec!["acme/alpha".into(), "acme/project".into()];
        project.github_filter.set_query("prj");

        let layout = add_project_layout(Rect::new(0, 0, 80, 24), Some(&project));

        assert_eq!(project.github_matches(), vec![(1, "acme/project")]);
        assert_eq!(layout.list.expect("filtered list").visible_rows, 1);
    }

    #[test]
    fn folder_breadcrumb_layout_elides_the_middle_and_keeps_clickable_ends() {
        let path = Path::new("/home/matthias/Repos/client/worktree/project");

        let segments = folder_breadcrumbs(path, 24);
        let labels = segments
            .iter()
            .map(|segment| segment.label.as_str())
            .collect::<Vec<_>>();

        assert_eq!(labels.first(), Some(&"/"));
        assert!(labels.contains(&"…"));
        assert_eq!(labels.last(), Some(&"project"));
        assert_eq!(
            segments.last().map(|segment| segment.path.as_path()),
            Some(path)
        );

        let long = folder_breadcrumbs(
            Path::new("/home/a-repository-name-that-is-far-too-wide"),
            18,
        );
        let width = long
            .iter()
            .map(|segment| display_width(&segment.label))
            .sum::<usize>()
            .saturating_add(long.len().saturating_sub(1) * 3);
        assert!(width <= 18);
        assert!(long
            .last()
            .is_some_and(|segment| segment.label.contains('…')));
    }
}
