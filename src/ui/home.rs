use ratatui::{
    layout::{Constraint, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
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
    lens_visible: bool,
) -> Line<'static> {
    let mut hint = if composer_visible {
        " ↑↓ browse · tab focus · ⏎ dispatch · esc back".to_string()
    } else if lens_visible {
        " ↑↓ browse · tab reply · ⏎ jump · d detach · esc closes".to_string()
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
    /// The card drawn around the composer, border included.
    frame: Rect,
    prompt: Rect,
    chips: Rect,
    target: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LensBands {
    frame: Rect,
    title: Rect,
    output: Rect,
    reply: Rect,
    detach: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HomeBands {
    header: Rect,
    gap: Rect,
    body: Rect,
    composer: Option<ComposerBands>,
    lens: Option<LensBands>,
    hint: Rect,
}

/// The bands home draws into: header, gap, queue, composer, hint.
///
/// Shared with hit-testing so a click can never land on a row the renderer put
/// somewhere else.
/// The composer card: a border row, three input rows, a border row.
const COMPOSER_CARD_ROWS: u16 = 5;

/// Rows the queue needs, clamped so the trailing bands always fit.
fn body_rows(area: Rect, queue_rows: usize, trailing: u16) -> u16 {
    let available = area
        .height
        .saturating_sub(2)
        .saturating_sub(trailing)
        .max(1);
    // At least one row, so the "nothing is waiting on you" line keeps its place.
    u16::try_from(queue_rows.max(1))
        .unwrap_or(u16::MAX)
        .clamp(1, available)
}

fn bands_for(area: Rect, lens_requested: bool, queue_rows: usize) -> HomeBands {
    if lens_requested && area.height >= crate::app::home::HOME_LENS_MIN_HEIGHT {
        let [header, gap, body, frame, _slack, hint] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(body_rows(area, queue_rows, 7)),
            Constraint::Length(6),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .areas(area);
        let inner = frame.inner(Margin::new(1, 1));
        let [title, output, reply] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(inner);
        let detach_width = 12.min(inner.width);
        let detach = Rect::new(
            inner.right().saturating_sub(detach_width),
            title.y,
            detach_width,
            title.height,
        );
        let title = Rect::new(
            title.x,
            title.y,
            title.width.saturating_sub(detach_width.saturating_add(1)),
            title.height,
        );
        HomeBands {
            header,
            gap,
            body,
            composer: None,
            lens: Some(LensBands {
                frame,
                title,
                output,
                reply,
                detach,
            }),
            hint,
        }
    } else if !lens_requested && area.height >= crate::app::home::HOME_COMPOSER_MIN_HEIGHT {
        // The composer sits directly under the queue; the slack goes below it,
        // so a short queue no longer strands the prompt at the pane floor.
        // The composer is a card directly under the queue: one border row above
        // and below the three input rows, with the slack beneath it.
        let [header, gap, body, frame, _slack, hint] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(body_rows(area, queue_rows, COMPOSER_CARD_ROWS + 1)),
            Constraint::Length(COMPOSER_CARD_ROWS),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .areas(area);
        let inner = frame.inner(Margin::new(1, 1));
        let [prompt, chips, target] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(inner);
        HomeBands {
            header,
            gap,
            body,
            composer: Some(ComposerBands {
                frame,
                prompt,
                chips,
                target,
            }),
            lens: None,
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
            lens: None,
            hint,
        }
    }
}

#[cfg(test)]
fn bands(area: Rect, queue_rows: usize) -> HomeBands {
    bands_for(area, false, queue_rows)
}

fn lens_requested(app: &AppState, queue: &[BlockedAgent]) -> bool {
    app.home.as_ref().is_some_and(|home| {
        !queue.is_empty()
            && home.current(queue).is_some()
            && matches!(home.focus, None | Some(HomeFocus::Reply))
    })
}

fn home_bands(app: &AppState, queue: &[BlockedAgent], area: Rect) -> HomeBands {
    bands_for(area, lens_requested(app, queue), queue.len())
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
    let specs = chip_specs(home);
    let label_width = specs
        .iter()
        .map(|(_, label)| label.chars().count())
        .sum::<usize>();
    let gaps = specs.len().saturating_sub(1);
    let gap = if label_width.saturating_add(gaps.saturating_mul(3)) <= area.width as usize {
        3
    } else {
        1
    };
    let mut x = area.x;
    let right = area.right();
    specs
        .into_iter()
        .filter_map(|(focus, label)| {
            if x >= right {
                return None;
            }
            let width = u16::try_from(label.chars().count())
                .unwrap_or(u16::MAX)
                .min(right - x);
            if width == 0 {
                return None;
            }
            let rect = Rect::new(x, area.y, width, area.height);
            x = x.saturating_add(width).saturating_add(gap);
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct LensSnapshot {
    title: String,
    output: String,
    reason: Option<String>,
}

fn lens_snapshot(
    app: &AppState,
    terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
    queue: &[BlockedAgent],
    output_lines: u16,
) -> Option<LensSnapshot> {
    let home = app.home.as_ref()?;
    let agent = home.current(queue)?;
    let title = format!(
        "{} · {}:p{}",
        agent.agent_label,
        agent.workspace_label,
        agent.pane_id.raw()
    );
    let Some(terminal) = app.terminals.get(&agent.terminal_id) else {
        return Some(LensSnapshot {
            title,
            output: String::new(),
            reason: Some("pane is no longer available".into()),
        });
    };
    if !crate::terminal::counts_as_blocked(
        terminal.state,
        !terminal.closing_gates.is_empty(),
        terminal.usage_limited,
    ) {
        return Some(LensSnapshot {
            title,
            output: String::new(),
            reason: Some("pane is no longer blocked".into()),
        });
    }
    let Some(runtime) =
        app.runtime_for_pane_in_workspace(terminal_runtimes, agent.ws_idx, agent.pane_id)
    else {
        return Some(LensSnapshot {
            title,
            output: String::new(),
            reason: Some("pane output is unavailable".into()),
        });
    };
    // Keep the lens on the same read contract as `pane.read`. Recent is the
    // normal tail; visible is the API's bounded fallback for a pane with no
    // retained history yet.
    let recent = crate::app::read_terminal_snapshot(
        runtime,
        crate::api::schema::ReadSource::Recent,
        crate::api::schema::ReadFormat::Text,
        Some(u32::from(output_lines.max(1))),
    );
    let snapshot = if recent.text.trim().is_empty() {
        crate::app::read_terminal_snapshot(
            runtime,
            crate::api::schema::ReadSource::Visible,
            crate::api::schema::ReadFormat::Text,
            Some(u32::from(output_lines.max(1))),
        )
    } else {
        recent
    };
    let pending_draft = app
        .pending_human_drafts
        .get(&agent.pane_id)
        .is_some_and(|draft| !draft.is_empty());
    let reason = home
        .reply_error
        .clone()
        .or_else(|| pending_draft.then_some("human draft pending · clear it in the pane".into()));
    let reason = reason.or_else(|| {
        snapshot
            .text
            .trim()
            .is_empty()
            .then_some("no output yet".into())
    });
    Some(LensSnapshot {
        title,
        output: snapshot.text,
        reason,
    })
}

fn render_lens(
    app: &AppState,
    terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
    queue: &[BlockedAgent],
    bands: LensBands,
    frame: &mut Frame,
) {
    let Some(home) = app.home.as_ref() else {
        return;
    };
    let Some(snapshot) = lens_snapshot(app, terminal_runtimes, queue, bands.output.height) else {
        return;
    };
    let border = Style::default().fg(app.palette.surface_dim);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border)
            .style(Style::default().bg(app.palette.panel_bg)),
        bands.frame,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate(&snapshot.title, bands.title.width as usize),
            Style::default()
                .fg(app.palette.text)
                .add_modifier(Modifier::BOLD),
        )))
        .style(Style::default().bg(app.palette.panel_bg)),
        bands.title,
    );
    let mut output = snapshot.output;
    if let Some(reason) = snapshot.reason {
        output = if output.is_empty() {
            reason
        } else {
            format!("{reason}\n{output}")
        };
    }
    frame.render_widget(
        Paragraph::new(output).style(
            Style::default()
                .fg(app.palette.overlay1)
                .add_modifier(Modifier::DIM),
        ),
        bands.output,
    );
    let focused = home.focus == Some(HomeFocus::Reply);
    let reply = if home.reply.is_empty() {
        "type a reply".to_string()
    } else {
        home.reply.clone()
    };
    let suffix = if focused { "_" } else { "" };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("> {reply}{suffix}"),
            if focused {
                Style::default()
                    .fg(app.palette.text)
                    .bg(app.palette.surface1)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.palette.subtext0)
            },
        )))
        .style(Style::default().bg(app.palette.panel_bg)),
        bands.reply,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate("detach [d] →", bands.detach.width as usize),
            Style::default().fg(app.palette.accent),
        )))
        .style(Style::default().bg(app.palette.panel_bg)),
        bands.detach,
    );
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
    let wanted: u16 = labels.len().try_into().unwrap_or(u16::MAX).min(area.height);
    let above = field.y.saturating_sub(area.y);
    let below = area.bottom().saturating_sub(field.y.saturating_add(1));
    // Dropdowns open downward. Opening upward is only a fallback for when there
    // is genuinely more room above, which with the composer near the top of the
    // pane is rare.
    let (height, y) = if wanted <= below {
        (wanted, field.y.saturating_add(1))
    } else if above > below {
        (wanted.min(above), field.y.saturating_sub(wanted.min(above)))
    } else {
        (below, field.y.saturating_add(1))
    };
    if width == 0 || height == 0 {
        return None;
    }
    let x = field.x.min(area.right().saturating_sub(width));
    Some(Rect::new(x, y, width, height))
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
    queue_row_hit_areas(app, queue, area, home_bands(app, queue, area).body)
}

pub(super) fn home_hit_areas(
    app: &AppState,
    queue: &[BlockedAgent],
    area: Rect,
) -> Vec<HomeHitArea> {
    let Some(home) = app.home.as_ref() else {
        return Vec::new();
    };
    let layout = home_bands(app, queue, area);
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
                    HomeFocus::Reply | HomeFocus::Prompt | HomeFocus::Target => continue,
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
    } else if let Some(lens) = layout.lens {
        hits.extend(
            queue_row_hit_areas(app, queue, area, layout.body)
                .into_iter()
                .map(|(index, rect)| HomeHitArea {
                    target: HomeHitTarget::QueueRow(index),
                    rect,
                }),
        );
        if lens.reply.width > 0 {
            hits.push(HomeHitArea {
                target: HomeHitTarget::Reply,
                rect: lens.reply,
            });
        }
        if lens.detach.width > 0 {
            hits.push(HomeHitArea {
                target: HomeHitTarget::Detach,
                rect: lens.detach,
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
    terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
    queue: &[BlockedAgent],
    counts: HomeCounts,
    area: Rect,
    frame: &mut Frame,
) {
    let layout = home_bands(app, queue, area);
    let HomeBands {
        header,
        body,
        composer,
        lens,
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
        // The card, drawn first so the input rows sit inside it. Focused
        // borders take the accent so the composer reads as active.
        let focused = home.focus.is_some();
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if focused {
                    app.palette.accent
                } else {
                    app.palette.surface_dim
                }))
                .style(Style::default().bg(app.palette.panel_bg)),
            composer.frame,
        );
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

    if let Some(lens) = lens {
        render_lens(app, terminal_runtimes, queue, lens, frame);
    }

    let hidden_below = queue.len().saturating_sub(scroll + visible);
    frame.render_widget(
        Paragraph::new(hint_line(
            app,
            scroll,
            hidden_below,
            composer.is_some(),
            lens.is_some(),
        )),
        hint,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::PaneId;
    use crate::terminal::{TerminalId, TerminalRuntime};
    use crate::workspace::Workspace;
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
                    &crate::terminal::TerminalRuntimeRegistry::new(),
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

    fn app_with_lens_screen(output: &[u8]) -> AppState {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("lens");
        let pane_id = workspace.tabs[0].root_pane;
        workspace.insert_test_runtime(
            pane_id,
            TerminalRuntime::test_with_scrollback_bytes(80, 8, 1024, output),
        );
        let terminal_id = workspace.tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.terminals
            .get_mut(&terminal_id)
            .expect("test terminal")
            .state = crate::detect::AgentState::Blocked;
        let mut home = HomeState::default();
        home.focus = None;
        app.home = Some(home);
        app
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
    fn the_composer_is_drawn_as_a_card_around_its_three_rows() {
        let mut app = AppState::test_new();
        app.home = Some(HomeState::default());
        let queue = vec![blocked(0)];
        let area = Rect::new(0, 0, 60, 14);
        let layout = bands(area, queue.len());
        let composer = layout.composer.expect("composer should fit");
        let buffer = draw_home(&app, &queue, area);

        assert_eq!(composer.frame.height, COMPOSER_CARD_ROWS);
        assert_eq!(buffer[(composer.frame.x, composer.frame.y)].symbol(), "┌");
        assert_eq!(
            buffer[(composer.frame.right() - 1, composer.frame.y)].symbol(),
            "┐"
        );
        assert_eq!(
            buffer[(composer.frame.x, composer.frame.bottom() - 1)].symbol(),
            "└"
        );
        // The three input rows sit inside the border, not on it.
        assert!(composer.prompt.y > composer.frame.y);
        assert!(composer.target.y < composer.frame.bottom() - 1);
        // And the card follows the queue rather than the pane floor.
        assert_eq!(composer.frame.y, layout.body.bottom());
    }

    #[test]
    fn dropdowns_open_downward_from_their_field() {
        let mut app = AppState::test_new();
        let mut home = HomeState::default();
        home.focus = Some(HomeFocus::Agent);
        home.picker = Some(HomePicker::Agent);
        app.home = Some(home);
        let queue = [blocked(0)];
        let area = Rect::new(0, 0, 60, 24);
        let layout = bands(area, queue.len());
        let composer = layout.composer.expect("composer should fit");
        let home = app.home.as_ref().expect("home");
        let field = composer_field_rect(home, composer, HomeFocus::Agent).expect("agent field");
        let popup = picker_popup_rect(&app, home, composer, area).expect("an open picker");

        assert_eq!(
            popup.y,
            field.y + 1,
            "a dropdown opens below its field, not above it"
        );
        assert!(popup.bottom() <= area.bottom());
    }

    #[test]
    fn composer_inset_matches_queue_content_and_click_targets() {
        let mut app = AppState::test_new();
        app.home = Some(HomeState::default());
        let queue = vec![blocked(0)];
        let area = Rect::new(0, 0, 100, 12);
        let layout = bands(area, queue.len());
        let composer = layout.composer.expect("composer should fit");
        let home = app.home.as_ref().expect("home");
        let buffer = draw_home(&app, &queue, area);
        let hits = home_hit_areas(&app, &queue, area);

        assert_eq!(buffer[(area.x + 1, layout.body.y)].symbol(), "▸");
        assert_eq!(buffer[(area.x + 1, composer.prompt.y)].symbol(), ">");
        assert_eq!(buffer[(area.x + 1, composer.chips.y)].symbol(), "c");
        assert_eq!(buffer[(area.x + 1, composer.target.y)].symbol(), "→");
        assert_eq!(composer.prompt.x, area.x + 1);
        assert_eq!(composer.chips.x, area.x + 1);
        assert_eq!(composer.target.x, area.x + 1);
        let wide_chip_rects = chip_rects(home, composer.chips);
        assert!(wide_chip_rects
            .windows(2)
            .all(|pair| pair[0].1.right() + 3 == pair[1].1.x));
        assert!(
            row_text(&buffer, area, composer.chips.y).contains("claude ▾   opus ▾   medium ▾"),
            "wide composer should keep the intended chip spacing"
        );
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
                HomeFocus::Reply | HomeFocus::Prompt | HomeFocus::Target => continue,
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
        let layout = bands(area, queue.len());
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

        assert!(bands(area, queue.len()).composer.is_none());
        assert!(hits
            .iter()
            .all(|hit| matches!(hit.target, HomeHitTarget::QueueRow(_))));
        assert!(!text.contains("type a prompt"), "{text:?}");
    }

    #[test]
    fn home_tab_order_includes_each_agents_supported_effort_options() {
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
        assert_eq!(
            effort_options(crate::detect::Agent::Codex),
            &["low", "medium", "high", "xhigh"]
        );
        home.focus = Some(HomeFocus::Model);
        home.move_focus(false);
        assert_eq!(home.focus, Some(HomeFocus::Effort));
        home.move_focus(false);
        assert_eq!(home.focus, Some(HomeFocus::Directory));
        home.move_focus(true);
        assert_eq!(home.focus, Some(HomeFocus::Effort));
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
    fn codex_dispatch_plan_uses_the_reasoning_effort_config_override() {
        let mut home = HomeState::default();
        home.prompt = "implement the retry cap".into();
        home.set_agent(crate::detect::Agent::Codex);
        home.model = "gpt-5.3-codex".into();
        home.effort = Some("xhigh".into());

        let plan = home.dispatch_plan().expect("prompt should dispatch");

        assert_eq!(plan.agent, crate::detect::Agent::Codex);
        assert_eq!(plan.effort.as_deref(), Some("xhigh"));
        assert_eq!(
            plan.argv,
            vec![
                "codex",
                "--model",
                "gpt-5.3-codex",
                "-c",
                "model_reasoning_effort=xhigh",
                "implement the retry cap"
            ]
        );
    }

    #[test]
    fn narrow_composer_collapses_chip_gaps_and_keeps_click_targets_in_bounds() {
        let mut app = AppState::test_new();
        app.home = Some(HomeState::default());
        let queue = vec![blocked(0)];
        let area = Rect::new(0, 0, 24, 12);
        let composer = bands(area, queue.len())
            .composer
            .expect("composer should fit");
        let home = app.home.as_ref().expect("home");
        let rects = chip_rects(home, composer.chips);
        let buffer = draw_home(&app, &queue, area);
        let chip_row = row_text(&buffer, area, composer.chips.y);

        // The composer is a card, so its rows open on the left border.
        assert!(
            chip_row.starts_with("│claude ▾ opus ▾ medium"),
            "{chip_row:?}"
        );
        assert_eq!(rects[0].1.right() + 1, rects[1].1.x);
        assert!(rects.windows(2).all(|pair| pair[0].1.right() < pair[1].1.x));
        assert!(rects.iter().all(|(_, rect)| rect.right() <= area.right()));

        let hits = home_hit_areas(&app, &queue, area);
        for (focus, rect) in rects {
            let target = match focus {
                HomeFocus::Agent => HomeHitTarget::Agent,
                HomeFocus::Model => HomeHitTarget::Model,
                HomeFocus::Effort => HomeHitTarget::Effort,
                HomeFocus::Directory => HomeHitTarget::Directory,
                HomeFocus::Reply | HomeFocus::Prompt | HomeFocus::Target => continue,
            };
            assert_eq!(
                hits.iter()
                    .find(|hit| hit.target == target)
                    .map(|hit| hit.rect),
                Some(rect)
            );
        }
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

    #[test]
    fn the_lens_follows_the_queue_cursor() {
        let mut app = AppState::test_new();
        let queue = vec![blocked(0), blocked(1)];
        let mut home = HomeState::default();
        home.focus = None;
        app.home = Some(home);
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();

        let first = lens_snapshot(&app, &runtimes, &queue, 2).expect("first lens");
        app.home.as_mut().expect("home").select(1);
        let second = lens_snapshot(&app, &runtimes, &queue, 2).expect("second lens");

        assert!(first.title.contains("agent0"));
        assert!(second.title.contains("agent1"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_lens_renders_the_tail_returned_by_pane_read() {
        let app = app_with_lens_screen(b"old line\r\nretry cap\r\nwhich do you want?\r\n");
        let queue = app.blocked_agents();
        let area = Rect::new(0, 0, 90, 12);
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let expected = lens_snapshot(&app, &runtimes, &queue, 2)
            .expect("lens")
            .output;
        let buffer = draw_home(&app, &queue, area);
        let rendered = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(expected.contains("which do you want?"), "{expected:?}");
        assert!(rendered.contains("which do you want?"), "{rendered:?}");
    }

    #[test]
    fn a_short_lens_frame_omits_the_lens_without_panic() {
        let mut app = AppState::test_new();
        let queue = vec![blocked(0)];
        let mut home = HomeState::default();
        home.focus = None;
        app.home = Some(home);
        let area = Rect::new(0, 0, 60, 6);

        let buffer = draw_home(&app, &queue, area);
        let layout = home_bands(&app, &queue, area);
        let hits = home_hit_areas(&app, &queue, area);

        assert!(layout.lens.is_none());
        assert!(hits
            .iter()
            .all(|hit| matches!(hit.target, HomeHitTarget::QueueRow(_))));
        assert_eq!(*buffer.area(), area);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_missing_or_empty_lens_degrades_to_a_reason() {
        let mut gone = AppState::test_new();
        let queue = vec![blocked(0)];
        let mut home = HomeState::default();
        home.focus = None;
        gone.home = Some(home);
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();

        assert_eq!(
            lens_snapshot(&gone, &runtimes, &queue, 2)
                .expect("lens")
                .reason
                .as_deref(),
            Some("pane is no longer available")
        );

        let empty = app_with_lens_screen(b"");
        let empty_queue = empty.blocked_agents();
        assert_eq!(
            lens_snapshot(&empty, &runtimes, &empty_queue, 2)
                .expect("lens")
                .reason
                .as_deref(),
            Some("no output yet")
        );

        let mut no_longer_blocked = app_with_lens_screen(b"");
        let stale_queue = no_longer_blocked.blocked_agents();
        no_longer_blocked
            .terminals
            .get_mut(&stale_queue[0].terminal_id)
            .expect("test terminal")
            .state = crate::detect::AgentState::Idle;
        assert_eq!(
            lens_snapshot(&no_longer_blocked, &runtimes, &stale_queue, 2)
                .expect("lens")
                .reason
                .as_deref(),
            Some("pane is no longer blocked")
        );
    }

    #[test]
    fn composer_and_lens_precedence_is_exclusive_and_focus_driven() {
        let mut app = AppState::test_new();
        let queue = vec![blocked(0)];
        app.home = Some(HomeState::default());
        let area = Rect::new(0, 0, 90, 12);

        let composer = home_bands(&app, &queue, area);
        assert!(composer.composer.is_some());
        assert!(composer.lens.is_none());

        app.home.as_mut().expect("home").focus = None;
        let lens = home_bands(&app, &queue, area);
        assert!(lens.composer.is_none());
        assert!(lens.lens.is_some());

        app.home.as_mut().expect("home").focus = Some(HomeFocus::Agent);
        let composer_again = home_bands(&app, &queue, area);
        assert!(composer_again.composer.is_some());
        assert!(composer_again.lens.is_none());
    }
}
