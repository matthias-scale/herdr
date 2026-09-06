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
use super::text::truncate_end;
use super::widgets::{
    centered_popup_rect, panel_contrast_fg, render_modal_header, render_modal_shell,
};

const POPUP_WIDTH: u16 = 64;
const POPUP_HEIGHT: u16 = 18;

#[derive(Debug, Clone, Default)]
pub(crate) struct AddProjectLayout {
    pub(crate) close: Rect,
    pub(crate) tabs: Vec<(AddProjectTab, Rect)>,
    pub(crate) input: Rect,
    pub(crate) list: Option<DropdownLayout>,
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
    let close = Rect::new(inner.right().saturating_sub(1), inner.y, 1, 1);
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
    let input = Rect::new(inner.x, inner.y.saturating_add(5), inner.width, 1);
    let list_bottom = inner.bottom().saturating_sub(2);
    let list_area = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        list_bottom.saturating_sub(inner.y),
    );
    let (item_count, selected) = match project.tab {
        AddProjectTab::LocalFolder => (project.browse.children.len(), 0),
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
                    max_rows: 8,
                    min_width: input.width,
                },
                list_area,
            )
        })
        .flatten();
    AddProjectLayout {
        close,
        tabs,
        input,
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
    render_modal_header(
        frame,
        Rect::new(inner.x, inner.y, inner.width, 1),
        "Add project",
        &app.palette,
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
        AddProjectTab::LocalFolder => "Folder",
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
        AddProjectTab::LocalFolder => project.browse.input.as_str(),
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

    if let Some(dropdown) = &layout.list {
        let rows = match project.tab {
            AddProjectTab::LocalFolder => project
                .browse
                .children
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            AddProjectTab::GitHub => project
                .github_matches()
                .into_iter()
                .map(|(_, repo)| repo)
                .collect::<Vec<_>>(),
            AddProjectTab::GitUrl => Vec::new(),
        };
        let lines = rows
            .into_iter()
            .enumerate()
            .skip(dropdown.first_visible)
            .take(dropdown.visible_rows)
            .map(|(index, row)| {
                let selected =
                    project.tab == AddProjectTab::GitHub && index == project.github_filter.selected;
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
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(lines).style(Style::default().bg(app.palette.panel_bg)),
            dropdown.list_rect,
        );
    }

    let status = if let Some(error) = project.error.as_deref() {
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
        Paragraph::new(" ←/→ tabs  Enter select  Esc close")
            .style(Style::default().fg(app.palette.overlay0)),
        Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::home::HomeBrowse;

    #[test]
    fn project_lists_open_below_the_input_at_both_capture_sizes() {
        for area in [Rect::new(0, 0, 120, 40), Rect::new(0, 0, 80, 24)] {
            let mut project = AddProjectState::starting_at(std::path::Path::new("/tmp"));
            project.browse = HomeBrowse {
                input: "/tmp/".into(),
                children: vec!["one".into(), "two".into()],
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
}
