use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::app::{
    home::{HomePicker, HomeWorkspace},
    spawn_dock::{SpawnDockField, SpawnDockState},
    state::AppState,
};

const DOCK_HEIGHT: u16 = 14;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HitTarget {
    Field(SpawnDockField),
    Spawn,
    Picker(super::account_picker::PickerHit),
}

pub(crate) fn hit_test(app: &AppState, column: u16, row: u16) -> Option<HitTarget> {
    let dock = app.spawn_dock.as_ref()?;
    let area = app.view.terminal_area;
    if dock.home.account_picker.active {
        return super::account_picker::hit_test(
            &dock.home,
            area,
            account_picker_anchor(&dock.home, area),
            column,
            row,
        )
        .map(HitTarget::Picker);
    }
    if area.height < 5 || area.width < 30 {
        return None;
    }
    let dock_area = Rect::new(
        area.x,
        area.y + area.height.saturating_sub(DOCK_HEIGHT.min(area.height)),
        area.width,
        DOCK_HEIGHT.min(area.height),
    );
    if !contains(dock_area, column, row) {
        return None;
    }
    let content_area = Rect::new(
        dock_area.x,
        dock_area.y.saturating_add(1),
        dock_area.width,
        dock_area.height.saturating_sub(1),
    );
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(2)])
        .split(content_area);
    let field_areas = field_rects(rows[0]);
    let fields = [
        SpawnDockField::Project,
        SpawnDockField::Host,
        SpawnDockField::Profile,
        SpawnDockField::Model,
        SpawnDockField::Effort,
        SpawnDockField::Worktree,
    ];
    if let Some((index, _)) = field_areas
        .iter()
        .enumerate()
        .find(|(_, area)| contains(**area, column, row))
    {
        return fields.get(index).copied().map(HitTarget::Field);
    }
    if dock.picker.is_some() || !contains(rows[1], column, row) {
        return None;
    }
    let prompt_area = Block::default()
        .borders(Borders::ALL)
        .padding(ratatui::widgets::Padding::uniform(1))
        .inner(rows[1]);
    let button = format!(
        "⇧⏎ Spawn on {}",
        dock.home
            .machine()
            .map(|machine| machine.name.as_str())
            .unwrap_or("host")
    );
    let width = button.chars().count() as u16 + 3;
    let button_area = Rect::new(
        prompt_area.x + prompt_area.width.saturating_sub(width),
        prompt_area.y + prompt_area.height.saturating_sub(3),
        width.min(prompt_area.width),
        3.min(prompt_area.height),
    );
    if contains(button_area, column, row) {
        Some(HitTarget::Spawn)
    } else {
        Some(HitTarget::Field(SpawnDockField::Prompt))
    }
}

fn field_rects(area: Rect) -> [Rect; 6] {
    Layout::horizontal([
        Constraint::Percentage(22),
        Constraint::Percentage(14),
        Constraint::Percentage(22),
        Constraint::Percentage(16),
        Constraint::Percentage(10),
        Constraint::Percentage(16),
    ])
    .areas(area)
}

pub(crate) fn account_picker_anchor(home: &crate::app::home::HomeState, area: Rect) -> Rect {
    let height = DOCK_HEIGHT.min(area.height);
    let content = Rect::new(
        area.x,
        area.bottom().saturating_sub(height).saturating_add(1),
        area.width,
        height.saturating_sub(1),
    );
    let rows = Layout::vertical([Constraint::Length(3), Constraint::Min(2)]).split(content);
    let fields = field_rects(rows[0]);
    fields[if home.picker == Some(HomePicker::Agent) {
        2
    } else {
        3
    }]
}

fn contains(rect: Rect, column: u16, row: u16) -> bool {
    column >= rect.x && column < rect.right() && row >= rect.y && row < rect.bottom()
}

pub(super) fn render(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(dock) = app.spawn_dock.as_ref() else {
        return;
    };
    if area.height < 5 || area.width < 30 {
        return;
    }
    let dock_area = Rect::new(
        area.x,
        area.y + area.height.saturating_sub(DOCK_HEIGHT.min(area.height)),
        area.width,
        DOCK_HEIGHT.min(area.height),
    );
    let title = dock
        .restored_age
        .as_ref()
        .map(|age| format!("draft restored · {age}"));
    frame.render_widget(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(app.palette.surface_dim))
            .title(title.unwrap_or_default())
            .style(Style::default().bg(app.palette.panel_bg)),
        dock_area,
    );
    let content_area = Rect::new(
        dock_area.x,
        dock_area.y.saturating_add(1),
        dock_area.width,
        dock_area.height.saturating_sub(1),
    );
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(2)])
        .split(content_area);
    let field_areas = field_rects(rows[0]);
    let fields = [
        SpawnDockField::Project,
        SpawnDockField::Host,
        SpawnDockField::Profile,
        SpawnDockField::Model,
        SpawnDockField::Effort,
        SpawnDockField::Worktree,
    ];
    for (field, rect) in fields.into_iter().zip(field_areas.iter()) {
        render_field(app, dock, field, *rect, frame);
    }
    if let Some(picker) = dock.picker.filter(|_| !dock.home.account_picker.active) {
        render_picker(app, dock, picker, rows[1], frame);
    } else {
        render_prompt(app, dock, rows[1], frame);
    }
    super::account_picker::render(
        app,
        &dock.home,
        frame,
        area,
        account_picker_anchor(&dock.home, area),
    );
}

fn render_field(
    app: &AppState,
    dock: &SpawnDockState,
    field: SpawnDockField,
    rect: Rect,
    frame: &mut Frame,
) {
    let focused = dock.focus == field;
    let profile_label = (field == SpawnDockField::Profile)
        .then(|| super::account_picker::selected_label(app, &dock.home));
    let value = match field {
        SpawnDockField::Project => dock
            .home
            .project()
            .map(|p| p.label.as_str())
            .unwrap_or("project"),
        SpawnDockField::Host => {
            if dock.auto_host {
                "auto"
            } else {
                dock.home
                    .machine()
                    .map(|m| m.name.as_str())
                    .unwrap_or("auto")
            }
        }
        SpawnDockField::Profile => profile_label.as_deref().unwrap_or_default(),
        SpawnDockField::Model => dock.home.model_display_name(),
        SpawnDockField::Effort => dock.home.effort.as_deref().unwrap_or("auto"),
        SpawnDockField::Worktree => match &dock.home.workspace {
            HomeWorkspace::CurrentCheckout => "current checkout",
            HomeWorkspace::NewWorktree => "new worktree",
            HomeWorkspace::PreviousWorktree(path) => path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("worktree"),
        },
        SpawnDockField::Prompt => "",
    };
    let title = match field {
        SpawnDockField::Project => "⌂ project",
        SpawnDockField::Host => "host",
        SpawnDockField::Profile => "profile",
        SpawnDockField::Model => "model",
        SpawnDockField::Effort => "effort",
        SpawnDockField::Worktree => "worktree",
        SpawnDockField::Prompt => "",
    };
    let border = if focused {
        app.palette.accent
    } else {
        app.palette.surface_dim
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(if focused {
            app.palette.selection_bg
        } else {
            app.palette.panel_bg
        }))
        .title(Span::styled(
            title,
            Style::default().fg(app.palette.subtext0),
        ));
    let content = if field == SpawnDockField::Profile {
        let usage = dock.home.selected_account_usage(&app.provider_usage);
        let lines =
            crate::app::account_picker::quota_lines(usage, app.provider_usage.collected_unix);
        format!("{value} · {}", lines.join(" · "))
    } else {
        value.to_string()
    };
    let value_style = Style::default()
        .fg(app.palette.text)
        .add_modifier(Modifier::BOLD);
    let line = match field {
        SpawnDockField::Profile => {
            super::account_picker::selected_label_line(app, &dock.home, &content, value_style)
        }
        SpawnDockField::Model => Line::from(Span::styled(
            content,
            value_style.fg(super::icons::agent_color(
                dock.home.profile().agent,
                &app.palette,
            )),
        )),
        _ => Line::from(Span::styled(content, value_style)),
    };
    frame.render_widget(Paragraph::new(line).style(value_style).block(block), rect);
}

fn render_prompt(app: &AppState, dock: &SpawnDockState, rect: Rect, frame: &mut Frame) {
    let focused = dock.focus == SpawnDockField::Prompt;
    let border = if focused {
        app.palette.accent
    } else {
        app.palette.surface_dim
    };
    let button = format!(
        "⇧⏎ Spawn on {}",
        dock.home
            .machine()
            .map(|m| m.name.as_str())
            .unwrap_or("host")
    );
    let width = button.chars().count() as u16 + 3;
    let inner = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(if focused {
            app.palette.selection_bg
        } else {
            app.palette.panel_bg
        }))
        .padding(ratatui::widgets::Padding::uniform(1));
    let prompt_area = inner.inner(rect);
    frame.render_widget(inner, rect);
    let button_area = Rect::new(
        prompt_area.x + prompt_area.width.saturating_sub(width),
        prompt_area.y + prompt_area.height.saturating_sub(3),
        width.min(prompt_area.width),
        3.min(prompt_area.height),
    );
    frame.render_widget(
        Paragraph::new(button)
            .style(
                Style::default()
                    .fg(app.palette.accent)
                    .add_modifier(Modifier::BOLD),
            )
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(ratatui::widgets::BorderType::Rounded)
                    .border_style(Style::default().fg(app.palette.accent))
                    .style(Style::default().bg(app.palette.panel_bg)),
            ),
        button_area,
    );
    let error = dock
        .home
        .account_launch_env()
        .err()
        .or_else(|| dock.home.dispatch_error.clone());
    let error_height = u16::from(error.is_some() && prompt_area.height > 0);
    if let Some(error) = error {
        frame.render_widget(
            Paragraph::new(crate::ui::text::truncate_end(
                &error,
                prompt_area.width.saturating_sub(width) as usize,
            ))
            .style(Style::default().fg(app.palette.red)),
            Rect::new(
                prompt_area.x,
                prompt_area.y,
                prompt_area.width.saturating_sub(width),
                error_height,
            ),
        );
    }
    let text_area = Rect::new(
        prompt_area.x,
        prompt_area.y.saturating_add(error_height),
        prompt_area.width.saturating_sub(width),
        prompt_area.height.saturating_sub(2 + error_height),
    );
    let text = if dock.home.prompt.is_empty() {
        vec![Line::from(Span::styled(
            "What should the agent work on?",
            Style::default().fg(app.palette.subtext0),
        ))]
    } else {
        dock.home.prompt.lines().map(Line::from).collect()
    };
    frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), text_area);
}

fn render_picker(
    app: &AppState,
    dock: &SpawnDockState,
    picker: HomePicker,
    rect: Rect,
    frame: &mut Frame,
) {
    let items: Vec<(String, Line<'static>)> = match picker {
        HomePicker::Project => dock
            .home
            .projects()
            .iter()
            .enumerate()
            .map(|(index, p)| {
                let icon = p.label.chars().next().unwrap_or('⌂');
                let path = p
                    .repos
                    .first()
                    .map(|repo| repo.path.display().to_string())
                    .unwrap_or_default();
                let count = dock
                    .project_agent_counts
                    .get(index)
                    .copied()
                    .unwrap_or_default();
                let search = format!("{icon} {}  {path}  agents:{count}", p.label);
                let line = Line::from(vec![
                    Span::styled(
                        format!(" {icon} "),
                        Style::default()
                            .fg(app.palette.panel_bg)
                            .bg(app.palette.blue),
                    ),
                    Span::raw(format!(" {}  {path}  agents:{count}", p.label)),
                ]);
                (search, line)
            })
            .collect(),
        HomePicker::Agent => {
            dock.home
                .profiles()
                .iter()
                .map(|p| {
                    let summary = p.quota.map(|q| q.usage(&app.provider_usage));
                    let usage = crate::app::account_picker::quota_lines(
                        summary,
                        app.provider_usage.collected_unix,
                    )
                    .join(" · ");
                    picker_row(format!(
                        "{}  {}{}  models:{}",
                        match p.agent {
                            crate::detect::Agent::Claude => "✳",
                            crate::detect::Agent::Codex => "◎",
                            _ => crate::ui::icons::agent_label(p.agent, app.nerd_font)
                                .unwrap_or("agent"),
                        },
                        p.label,
                        usage,
                        dock.home.profile_models(p.agent)
                    ))
                })
                .collect()
        }
        HomePicker::Model => dock
            .home
            .model_options()
            .iter()
            .map(|m| picker_row(m.display_name.clone()))
            .collect(),
        HomePicker::Effort => dock
            .home
            .effort_options()
            .iter()
            .cloned()
            .map(picker_row)
            .collect(),
        HomePicker::Workspace => dock
            .home
            .workspace_options()
            .iter()
            .map(HomeWorkspace::label)
            .map(picker_row)
            .collect(),
        _ => Vec::new(),
    };
    let query = dock.filter.to_lowercase();
    let matches: Vec<(usize, &(String, Line<'static>))> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.0.to_lowercase().contains(&query))
        .collect();
    let selected = dock.home.picker_selected;
    let lines = matches
        .iter()
        .take(rect.height.saturating_sub(1) as usize)
        .enumerate()
        .map(|(i, (_, (_, line)))| {
            let active = i == selected;
            let style = if active {
                Style::default()
                    .fg(app.palette.text)
                    .bg(app.palette.selection_bg)
            } else {
                Style::default().fg(app.palette.text)
            };
            let mut line = line.clone();
            line.style = style;
            line
        })
        .collect::<Vec<_>>();
    let title = format!(
        "{}  {}",
        match picker {
            HomePicker::Project => "project",
            HomePicker::Agent => "profile",
            HomePicker::Model => "model",
            HomePicker::Effort => "effort",
            _ => "worktree",
        },
        dock.filter
    );
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(app.palette.accent))
                .style(Style::default().bg(app.palette.panel_bg)),
        ),
        rect,
    );
}

fn picker_row(label: String) -> (String, Line<'static>) {
    (label.clone(), Line::from(label))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::home::HomeState;
    use ratatui::{backend::TestBackend, Terminal};
    #[test]
    fn account_picker_selected_spawn_profile_summary_uses_same_icon_and_account() {
        use crate::provider_usage::{ProviderAccountUsage, QuotaProvider};
        for nerd_font in [false, true] {
            for (width, height) in [(194, 120), (52, 36)] {
                let mut app = AppState::test_new();
                app.nerd_font = nerd_font;
                app.provider_usage.accounts.push(ProviderAccountUsage {
                    provider: QuotaProvider::Claude,
                    profile_id: "scalablehq".into(),
                    label: "SHQ".into(),
                    usage: Default::default(),
                });
                let mut home = HomeState::default();
                home.refresh_account_rows(&app.provider_usage);
                home.select_account_row(1);
                app.spawn_dock = Some(SpawnDockState::new(home));
                let area = Rect::new(0, 0, width, height);
                app.view.terminal_area = area;
                let dock = app.spawn_dock.as_ref().expect("dock");
                let mut original = dock.home.clone();
                original.picker = Some(HomePicker::Agent);
                let rect = account_picker_anchor(&original, area);
                let expected = super::super::account_picker::selected_label(&app, &dock.home);
                let mut terminal =
                    Terminal::new(TestBackend::new(width, height)).expect("terminal");
                terminal
                    .draw(|frame| render(&app, frame, area))
                    .expect("render");
                let inner = Block::default().borders(Borders::ALL).inner(rect);
                let buffer = terminal.backend().buffer();
                let rendered: String = (inner.x..inner.right())
                    .map(|x| buffer[(x, inner.y)].symbol())
                    .collect();
                assert!(
                    rendered.contains(&expected),
                    "{rendered:?} must show {expected}"
                );
                assert!(!rendered.to_ascii_lowercase().contains("claude"));
                assert_eq!(
                    buffer[(inner.x, inner.y)].fg,
                    super::super::icons::agent_color(crate::detect::Agent::Claude, &app.palette),
                    "spawn profile icon color"
                );
                let dock_fields = field_rects(Rect::new(
                    area.x,
                    area.bottom().saturating_sub(DOCK_HEIGHT).saturating_add(1),
                    area.width,
                    3,
                ));
                let model_inner = Block::default().borders(Borders::ALL).inner(dock_fields[3]);
                assert_eq!(
                    buffer[(model_inner.x, model_inner.y)].fg,
                    super::super::icons::agent_color(crate::detect::Agent::Claude, &app.palette),
                    "spawn model label color"
                );
                assert_eq!(
                    hit_test(&app, rect.x, rect.y),
                    Some(HitTarget::Field(SpawnDockField::Profile))
                );
                assert_eq!(
                    dock.home
                        .account_selection
                        .as_ref()
                        .expect("selection")
                        .account,
                    "scalablehq"
                );
            }
        }
    }

    #[test]
    fn account_picker_spawn_render_and_actual_hit_test_share_scrolled_rows() {
        for theme in ["catppuccin", "catppuccin-latte"] {
            for (width, height) in [(194, 120), (100, 30), (52, 36), (40, 24)] {
                for picker in [HomePicker::Agent, HomePicker::Model] {
                    let mut app = AppState::test_new();
                    app.palette = crate::app::state::Palette::from_name(theme).expect("theme");
                    let area = Rect::new(0, 0, width, height);
                    app.view.terminal_area = area;
                    let mut home = HomeState::default();
                    home.refresh_account_rows(&Default::default());
                    home.open_account_picker(picker);
                    let mut dock = SpawnDockState::new(home);
                    dock.picker = Some(picker);
                    app.spawn_dock = Some(dock);
                    let mut terminal =
                        Terminal::new(TestBackend::new(width, height)).expect("terminal");
                    terminal
                        .draw(|frame| render(&app, frame, area))
                        .expect("render");
                    let home = &app.spawn_dock.as_ref().expect("dock").home;
                    let anchor = account_picker_anchor(home, area);
                    let g = super::super::account_picker::geometry(home, area, anchor);
                    assert!(
                        g.popup.bottom() <= anchor.y,
                        "Spawn flips above bottom dock"
                    );
                    let buffer = terminal.backend().buffer();
                    assert_eq!(buffer[(g.popup.x, g.popup.y)].bg, app.palette.panel_bg);
                    assert_eq!(
                        buffer[(g.accounts.x, g.accounts.y)].bg,
                        app.palette.selection_bg
                    );
                    assert_eq!(
                        hit_test(&app, g.accounts.x, g.accounts.y),
                        Some(HitTarget::Picker(
                            super::super::account_picker::PickerHit::Account(0)
                        ))
                    );
                    let models = home.filtered_account_models();
                    let start = super::super::account_picker::scroll_start(
                        home.account_picker.model_cursor,
                        g.models.height,
                        models.len(),
                    );
                    let expected = home.model_options()[models[start]].short_name();
                    let rendered: String = (g.models.x..g.models.right())
                        .map(|x| buffer[(x, g.models.y)].symbol())
                        .collect();
                    assert!(
                        rendered.contains(expected),
                        "rendered {rendered:?}, expected {expected}"
                    );
                    let hit = hit_test(&app, g.models.x, g.models.y);
                    assert_eq!(
                        hit,
                        Some(HitTarget::Picker(
                            super::super::account_picker::PickerHit::Model(start)
                        ))
                    );
                    let mut clicked = home.clone();
                    super::super::account_picker::apply_hit(
                        &mut clicked,
                        super::super::account_picker::PickerHit::Model(start),
                    );
                    assert_eq!(clicked.model, home.model_options()[models[start]].id);
                }
            }
        }
    }
}
