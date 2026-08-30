use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{
    home::{
        effort_options, model_options, HomeCounts, HomeFocus, HomePicker, HomeState, HomeTarget,
    },
    inbox::BlockedAgent,
    state::{HomeHitArea, HomeHitTarget},
    AppState,
};

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

fn hint_line(
    app: &AppState,
    hidden_above: usize,
    hidden_below: usize,
    composer_visible: bool,
) -> Line<'static> {
    let mut hint = if composer_visible {
        " ↑↓ browse · tab focus · ⏎ dispatch · esc back".to_string()
    } else {
        " ↑↓ browse · ⏎ jump · esc closes".to_string()
    };
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ComposerBands {
    prompt: Rect,
    chips: Rect,
    target: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HomeBands {
    header: Rect,
    gap: Rect,
    body: Rect,
    composer: Option<ComposerBands>,
    hint: Rect,
}

/// The bands home draws into: header, gap, queue, composer, hint.
///
/// Shared with hit-testing so a click can never land on a row the renderer put
/// somewhere else.
fn bands(area: Rect) -> HomeBands {
    if area.height >= crate::app::home::HOME_COMPOSER_MIN_HEIGHT {
        let [header, gap, body, prompt, chips, target, hint] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);
        HomeBands {
            header,
            gap,
            body,
            composer: Some(ComposerBands {
                prompt,
                chips,
                target,
            }),
            hint,
        }
    } else {
        let [header, gap, body, hint] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(area);
        HomeBands {
            header,
            gap,
            body,
            composer: None,
            hint,
        }
    }
}

fn chip_specs(home: &HomeState) -> Vec<(HomeFocus, String)> {
    let mut specs = vec![
        (
            HomeFocus::Agent,
            format!("{} ▾", crate::detect::agent_label(home.agent)),
        ),
        (HomeFocus::Model, format!("{} ▾", home.model)),
    ];
    if let Some(effort) = &home.effort {
        specs.push((HomeFocus::Effort, format!("{} ▾", effort)));
    }
    specs.push((
        HomeFocus::Directory,
        format!("{} ▾", display_path(&home.directory)),
    ));
    specs
}

fn chip_rects(home: &HomeState, area: Rect) -> Vec<(HomeFocus, Rect)> {
    let mut x = area.x;
    let right = area.right();
    chip_specs(home)
        .into_iter()
        .filter_map(|(focus, label)| {
            if x >= right {
                return None;
            }
            let width = u16::try_from(label.chars().count().saturating_add(1))
                .unwrap_or(u16::MAX)
                .min(right - x);
            if width == 0 {
                return None;
            }
            let rect = Rect::new(x, area.y, width, area.height);
            x += width;
            Some((focus, rect))
        })
        .collect()
}

fn display_path(path: &std::path::Path) -> String {
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return path.display().to_string();
    };
    path.strip_prefix(&home)
        .map(|relative| {
            if relative.as_os_str().is_empty() {
                "~".to_string()
            } else {
                format!("~/{}", relative.display())
            }
        })
        .unwrap_or_else(|_| path.display().to_string())
}

fn target_label(app: &AppState, target: &HomeTarget) -> String {
    match target {
        HomeTarget::NewSpace => "new space".into(),
        HomeTarget::Existing(id) => app
            .workspaces
            .iter()
            .find(|workspace| workspace.id == *id)
            .map(|workspace| format!("space {}", workspace.id))
            .unwrap_or_else(|| "missing space".into()),
    }
}

fn picker_labels(app: &AppState, home: &HomeState, picker: HomePicker) -> Vec<String> {
    match picker {
        HomePicker::Agent => crate::app::home::dispatchable_agents()
            .iter()
            .map(|agent| crate::detect::agent_label(*agent).to_string())
            .collect(),
        HomePicker::Model => model_options(home.agent)
            .iter()
            .map(|model| (*model).to_string())
            .collect(),
        HomePicker::Effort => effort_options(home.agent)
            .iter()
            .map(|effort| (*effort).to_string())
            .collect(),
        HomePicker::Directory => app
            .home_directory_options()
            .iter()
            .map(|directory| display_path(directory))
            .collect(),
        HomePicker::Target => app
            .home_target_options()
            .iter()
            .map(|target| target_label(app, target))
            .collect(),
    }
}

fn composer_field_rect(
    home: &HomeState,
    composer: ComposerBands,
    focus: HomeFocus,
) -> Option<Rect> {
    if focus == HomeFocus::Prompt {
        return (composer.prompt.width > 0).then_some(composer.prompt);
    }
    if focus == HomeFocus::Target {
        return (composer.target.width > 0).then_some(composer.target);
    }
    chip_rects(home, composer.chips)
        .into_iter()
        .find_map(|(chip_focus, rect)| (chip_focus == focus).then_some(rect))
}

fn picker_popup_rect(
    app: &AppState,
    home: &HomeState,
    composer: ComposerBands,
    area: Rect,
) -> Option<Rect> {
    let picker = home.picker?;
    let field = composer_field_rect(home, composer, home.focus?)?;
    let labels = picker_labels(app, home, picker);
    if labels.is_empty() || area.width == 0 || area.height == 0 {
        return None;
    }
    let width = labels
        .iter()
        .map(|label| label.chars().count())
        .max()
        .unwrap_or(0)
        .saturating_add(2)
        .try_into()
        .unwrap_or(u16::MAX)
        .min(area.width);
    let height = labels
        .len()
        .try_into()
        .unwrap_or(u16::MAX)
        .min(field.y)
        .min(area.height);
    if width == 0 || height == 0 {
        return None;
    }
    let x = field.x.min(area.right().saturating_sub(width));
    Some(Rect::new(x, field.y - height, width, height))
}

fn queue_row_hit_areas(
    app: &AppState,
    queue: &[BlockedAgent],
    area: Rect,
    body: Rect,
) -> Vec<(usize, Rect)> {
    let Some(home) = app.home.as_ref() else {
        return Vec::new();
    };
    if queue.is_empty() || area.width == 0 || area.height < 4 {
        return Vec::new();
    }
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

/// One `(queue index, rect)` per row home is currently showing.
///
/// Empty when home is closed or the queue is, so the click handler needs no
/// separate "is home open" test.
pub(super) fn row_hit_areas(
    app: &AppState,
    queue: &[BlockedAgent],
    area: Rect,
) -> Vec<(usize, Rect)> {
    queue_row_hit_areas(app, queue, area, bands(area).body)
}

pub(super) fn home_hit_areas(
    app: &AppState,
    queue: &[BlockedAgent],
    area: Rect,
) -> Vec<HomeHitArea> {
    let Some(home) = app.home.as_ref() else {
        return Vec::new();
    };
    let layout = bands(area);
    let mut hits = Vec::new();
    if let Some(composer) = layout.composer {
        if let Some(popup) = picker_popup_rect(app, home, composer, area) {
            for offset in 0..popup.height {
                hits.push(HomeHitArea {
                    target: HomeHitTarget::PickerOption(offset as usize),
                    rect: Rect::new(popup.x, popup.y + offset, popup.width, 1),
                });
            }
        }
        hits.extend(
            queue_row_hit_areas(app, queue, area, layout.body)
                .into_iter()
                .map(|(index, rect)| HomeHitArea {
                    target: HomeHitTarget::QueueRow(index),
                    rect,
                }),
        );
        if composer.prompt.width > 0 {
            hits.push(HomeHitArea {
                target: HomeHitTarget::Prompt,
                rect: composer.prompt,
            });
        }
        for (focus, rect) in chip_rects(home, composer.chips) {
            hits.push(HomeHitArea {
                target: match focus {
                    HomeFocus::Agent => HomeHitTarget::Agent,
                    HomeFocus::Model => HomeHitTarget::Model,
                    HomeFocus::Effort => HomeHitTarget::Effort,
                    HomeFocus::Directory => HomeHitTarget::Directory,
                    HomeFocus::Prompt | HomeFocus::Target => continue,
                },
                rect,
            });
        }
        if composer.target.width > 0 {
            hits.push(HomeHitArea {
                target: HomeHitTarget::Target,
                rect: composer.target,
            });
        }
    } else {
        hits.extend(
            queue_row_hit_areas(app, queue, area, layout.body)
                .into_iter()
                .map(|(index, rect)| HomeHitArea {
                    target: HomeHitTarget::QueueRow(index),
                    rect,
                }),
        );
    }
    hits
}

pub(super) fn render_home(
    app: &AppState,
    queue: &[BlockedAgent],
    counts: HomeCounts,
    area: Rect,
    frame: &mut Frame,
) {
    let layout = bands(area);
    let HomeBands {
        header,
        body,
        composer,
        hint,
        ..
    } = layout;

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

    if let Some(composer) = composer {
        let Some(home) = app.home.as_ref() else {
            return;
        };
        let prompt = if home.prompt.is_empty() {
            "type a prompt".to_string()
        } else {
            home.prompt.clone()
        };
        let prompt_style = if home.focus == Some(HomeFocus::Prompt) {
            Style::default()
                .fg(app.palette.text)
                .bg(app.palette.surface1)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.palette.subtext0)
        };
        let prompt_suffix = if home.focus == Some(HomeFocus::Prompt) {
            "_"
        } else {
            ""
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("> {prompt}{prompt_suffix}"),
                prompt_style,
            )))
            .style(Style::default().bg(app.palette.panel_bg)),
            composer.prompt,
        );

        for (focus, rect) in chip_rects(home, composer.chips) {
            let label = chip_specs(home)
                .into_iter()
                .find_map(|(candidate, label)| (candidate == focus).then_some(label))
                .unwrap_or_default();
            let style = if home.focus == Some(focus) {
                Style::default()
                    .fg(app.palette.text)
                    .bg(app.palette.surface1)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.palette.subtext0)
            };
            frame.render_widget(Paragraph::new(label).style(style), rect);
        }

        let target_style = if home.focus == Some(HomeFocus::Target) {
            Style::default()
                .fg(app.palette.text)
                .bg(app.palette.surface1)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.palette.subtext0)
        };
        frame.render_widget(
            Paragraph::new(format!("→ {} ▾", target_label(app, &home.target))).style(target_style),
            composer.target,
        );

        if let Some(popup) = picker_popup_rect(app, home, composer, area) {
            if let Some(picker) = home.picker {
                let labels = picker_labels(app, home, picker);
                let lines = labels
                    .into_iter()
                    .enumerate()
                    .map(|(index, label)| {
                        let style = if index == home.picker_selected {
                            Style::default()
                                .fg(app.palette.text)
                                .bg(app.palette.surface1)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                                .fg(app.palette.subtext0)
                                .bg(app.palette.panel_bg)
                        };
                        Line::from(Span::styled(format!(" {label}"), style))
                    })
                    .collect::<Vec<_>>();
                frame.render_widget(
                    Paragraph::new(lines).style(Style::default().bg(app.palette.panel_bg)),
                    popup,
                );
            }
        }
    }

    let hidden_below = queue.len().saturating_sub(scroll + visible);
    frame.render_widget(
        Paragraph::new(hint_line(app, scroll, hidden_below, composer.is_some())),
        hint,
    );
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
        let area = Rect::new(0, 0, 60, 6);

        let buffer = draw_home(&app, &queue, area);
        let body_rows: Vec<String> = (2..5).map(|row| row_text(&buffer, area, row)).collect();

        assert!(body_rows[0].contains("agent4"), "{body_rows:?}");
        assert!(body_rows[1].contains("agent5"), "{body_rows:?}");
        assert!(body_rows[2].contains("agent6"), "{body_rows:?}");
        assert!(body_rows.iter().all(|row| !row.contains("agent3")));
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

    #[test]
    fn composer_geometry_matches_the_hit_areas_used_by_clicks() {
        let mut app = AppState::test_new();
        app.home = Some(HomeState::default());
        let queue = vec![blocked(0)];
        let area = Rect::new(0, 0, 100, 12);
        let layout = bands(area);
        let composer = layout.composer.expect("composer should fit");
        let home = app.home.as_ref().expect("home");
        let hits = home_hit_areas(&app, &queue, area);

        assert_eq!(
            hits.iter()
                .find(|hit| hit.target == HomeHitTarget::Prompt)
                .map(|hit| hit.rect),
            Some(composer.prompt)
        );
        assert_eq!(
            hits.iter()
                .find(|hit| hit.target == HomeHitTarget::Target)
                .map(|hit| hit.rect),
            Some(composer.target)
        );
        for (focus, rect) in chip_rects(home, composer.chips) {
            let target = match focus {
                HomeFocus::Agent => HomeHitTarget::Agent,
                HomeFocus::Model => HomeHitTarget::Model,
                HomeFocus::Effort => HomeHitTarget::Effort,
                HomeFocus::Directory => HomeHitTarget::Directory,
                HomeFocus::Prompt | HomeFocus::Target => continue,
            };
            assert_eq!(
                hits.iter()
                    .find(|hit| hit.target == target)
                    .map(|hit| hit.rect),
                Some(rect),
                "hit area for {focus:?} drifted from its chip"
            );
        }
    }

    #[test]
    fn open_picker_options_use_the_same_popup_rows_as_the_click_targets() {
        let mut app = AppState::test_new();
        let mut home = HomeState::default();
        home.focus = Some(HomeFocus::Agent);
        home.picker = Some(HomePicker::Agent);
        app.home = Some(home);
        let queue = vec![blocked(0)];
        let area = Rect::new(0, 0, 100, 12);
        let layout = bands(area);
        let composer = layout.composer.expect("composer should fit");
        let home = app.home.as_ref().expect("home");
        let popup = picker_popup_rect(&app, home, composer, area).expect("picker should fit");
        let option_hits: Vec<_> = home_hit_areas(&app, &queue, area)
            .into_iter()
            .filter(|hit| matches!(hit.target, HomeHitTarget::PickerOption(_)))
            .collect();

        assert_eq!(option_hits.len(), 2);
        for (offset, hit) in option_hits.iter().enumerate() {
            assert_eq!(
                hit.rect,
                Rect::new(popup.x, popup.y + offset as u16, popup.width, 1)
            );
        }
    }

    #[test]
    fn a_short_home_frame_omits_the_composer_without_panic_or_overlap() {
        let mut app = AppState::test_new();
        app.home = Some(HomeState::default());
        let queue = vec![blocked(0)];
        let area = Rect::new(0, 0, 60, 6);

        let buffer = draw_home(&app, &queue, area);
        let hits = home_hit_areas(&app, &queue, area);
        let text = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(bands(area).composer.is_none());
        assert!(hits
            .iter()
            .all(|hit| matches!(hit.target, HomeHitTarget::QueueRow(_))));
        assert!(!text.contains("type a prompt"), "{text:?}");
    }

    #[test]
    fn home_tab_order_skips_effort_when_codex_does_not_support_it() {
        let mut home = HomeState::default();
        let claude_order = [
            HomeFocus::Agent,
            HomeFocus::Model,
            HomeFocus::Effort,
            HomeFocus::Directory,
            HomeFocus::Target,
            HomeFocus::Prompt,
        ];
        for expected in claude_order {
            home.move_focus(false);
            assert_eq!(home.focus, Some(expected));
        }

        home.set_agent(crate::detect::Agent::Codex);
        home.focus = Some(HomeFocus::Model);
        home.move_focus(false);
        assert_eq!(home.focus, Some(HomeFocus::Directory));
        home.move_focus(true);
        assert_eq!(home.focus, Some(HomeFocus::Model));
    }

    #[test]
    fn enter_dispatch_plan_preserves_selected_agent_model_effort_directory_and_target() {
        let mut home = HomeState::default();
        home.prompt = "implement the retry cap".into();
        home.model = "sonnet".into();
        home.effort = Some("high".into());
        home.directory = std::path::PathBuf::from("/tmp/herdr");
        home.target = HomeTarget::Existing("space-2".into());

        let plan = home.dispatch_plan().expect("prompt should dispatch");

        assert_eq!(plan.agent, crate::detect::Agent::Claude);
        assert_eq!(plan.model, "sonnet");
        assert_eq!(plan.effort.as_deref(), Some("high"));
        assert_eq!(plan.directory, std::path::PathBuf::from("/tmp/herdr"));
        assert_eq!(plan.target, HomeTarget::Existing("space-2".into()));
        assert_eq!(
            plan.argv,
            vec![
                "claude",
                "--model",
                "sonnet",
                "--effort",
                "high",
                "implement the retry cap"
            ]
        );
    }

    #[test]
    fn escape_leaves_the_composer_before_closing_home() {
        let mut home = HomeState::default();

        assert!(!home.close_composer_or_home());
        assert_eq!(home.focus, None);
        assert!(home.picker.is_none());
        assert!(home.close_composer_or_home());
    }

    #[test]
    fn typing_in_the_prompt_does_not_move_queue_selection() {
        let queue = vec![blocked(0), blocked(1)];
        let mut home = HomeState::default();
        home.select(1);

        home.append_prompt('j');

        assert_eq!(home.prompt, "j");
        assert_eq!(home.selected(&queue), 1);
    }
}
