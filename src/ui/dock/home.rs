use std::time::SystemTime;

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::super::markdown;
use super::super::text::{display_width, truncate_end};
use crate::{
    app::{
        state::{DockHomeDetailTab, DockHomeSection, WorkItemKey},
        AppState,
    },
    work_index::{WorkItem, WorkItemDetail},
    work_projection::{compact_elapsed, DockHomeProjection, DockHomeRow, DockHomeTicketRow},
};

const TAB_ROWS: u16 = 2;
const FOOTER_ROWS: u16 = 4;

fn tab_label(row: &DockHomeRow) -> String {
    format!("{} #{}", row.glyph, row.number)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HiddenTabCount {
    count: usize,
    area: Rect,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TabWindow {
    pub(crate) tabs: Vec<(usize, Rect)>,
    leading_hidden: Option<HiddenTabCount>,
    trailing_hidden: Option<HiddenTabCount>,
}

fn hidden_count_label(count: usize, leading: bool) -> String {
    if leading {
        format!("‹ {count}")
    } else {
        format!("{count} ›")
    }
}

fn hidden_count_width(count: usize, leading: bool) -> u16 {
    let gap = usize::from(leading);
    u16::try_from(display_width(&hidden_count_label(count, leading)).saturating_add(gap))
        .unwrap_or(u16::MAX)
}

/// Returns a contiguous selection-following window. Every admitted tab gets
/// its complete label plus a separating column. Hidden-count labels are part
/// of the fit calculation, so the strip never claims space it cannot render.
fn selection_following_tab_window(
    tab_bar: Rect,
    label_widths: &[u16],
    selected: usize,
) -> TabWindow {
    if label_widths.is_empty() || tab_bar.width == 0 || tab_bar.height == 0 {
        return TabWindow::default();
    }

    let selected = selected.min(label_widths.len() - 1);
    let mut best: Option<(usize, usize, usize)> = None;
    for start in 0..=selected {
        for end in selected + 1..=label_widths.len() {
            let tabs_width = label_widths[start..end]
                .iter()
                .copied()
                .fold(0u16, u16::saturating_add);
            let leading_width = if start > 0 {
                hidden_count_width(start, true)
            } else {
                0
            };
            let trailing_count = label_widths.len().saturating_sub(end);
            let trailing_width = if trailing_count > 0 {
                hidden_count_width(trailing_count, false)
            } else {
                0
            };
            if tabs_width
                .saturating_add(leading_width)
                .saturating_add(trailing_width)
                > tab_bar.width
            {
                continue;
            }
            let count = end - start;
            let imbalance = (selected - start).abs_diff(end - selected - 1);
            if best.is_none_or(|(best_start, best_end, best_imbalance)| {
                count > best_end - best_start
                    || (count == best_end - best_start && imbalance < best_imbalance)
                    || (count == best_end - best_start
                        && imbalance == best_imbalance
                        && start > best_start)
            }) {
                best = Some((start, end, imbalance));
            }
        }
    }

    let Some((start, end, _)) = best else {
        return TabWindow::default();
    };
    let mut x = tab_bar.x;
    let leading_hidden = (start > 0).then(|| {
        let width = hidden_count_width(start, true);
        let hidden = HiddenTabCount {
            count: start,
            area: Rect::new(x, tab_bar.y, width, 1),
        };
        x = x.saturating_add(width);
        hidden
    });
    let tabs = label_widths[start..end]
        .iter()
        .copied()
        .enumerate()
        .map(|(offset, width)| {
            let area = Rect::new(x, tab_bar.y, width, 1);
            x = x.saturating_add(width);
            (start + offset, area)
        })
        .collect();
    let trailing_count = label_widths.len().saturating_sub(end);
    let trailing_hidden = (trailing_count > 0).then(|| HiddenTabCount {
        count: trailing_count,
        area: Rect::new(x, tab_bar.y, hidden_count_width(trailing_count, false), 1),
    });
    TabWindow {
        tabs,
        leading_hidden,
        trailing_hidden,
    }
}

fn tab_cell_width(label: &str) -> u16 {
    u16::try_from(display_width(label).saturating_add(1)).unwrap_or(u16::MAX)
}

pub(crate) fn tab_layouts(
    app: &AppState,
    projection: &DockHomeProjection,
    area: Rect,
) -> TabWindow {
    let widths = projection
        .rows
        .iter()
        .map(|row| tab_cell_width(&tab_label(row)))
        .collect::<Vec<_>>();
    selection_following_tab_window(
        Rect::new(area.x, area.y.saturating_add(1), area.width, 1),
        &widths,
        app.dock_home_selected_index(projection).unwrap_or(0),
    )
}

pub(crate) fn poll_tab_layouts(
    app: &AppState,
    projection: &DockHomeProjection,
    area: Rect,
) -> TabWindow {
    let widths = projection
        .poll_rows
        .iter()
        .map(|row| tab_cell_width(&poll_tab_label(row)))
        .collect::<Vec<_>>();
    selection_following_tab_window(
        Rect::new(area.x, area.y.saturating_add(1), area.width, 1),
        &widths,
        app.dock_home_selected_poll_index(projection).unwrap_or(0),
    )
}

/// A poll's tab reads `<agent> N`, which is short enough for the strip and
/// still says which agent is waiting.
pub(crate) fn poll_tab_label(row: &crate::work_projection::DockHomePollRow) -> String {
    format!("{} {}", row.agent_label, row.item.n)
}

pub(crate) fn ticket_tab_layouts(
    app: &AppState,
    projection: &DockHomeProjection,
    area: Rect,
) -> TabWindow {
    let widths = projection
        .ticket_rows
        .iter()
        .map(|row| tab_cell_width(&row.ticket.identifier))
        .collect::<Vec<_>>();
    selection_following_tab_window(
        Rect::new(area.x, area.y.saturating_add(1), area.width, 1),
        &widths,
        app.dock_home_selected_ticket_index(projection).unwrap_or(0),
    )
}

pub(crate) fn section_layouts(area: Rect) -> [Rect; 3] {
    let row = Rect::new(area.x, area.y, area.width, 1);
    let areas = crate::ui::horizontal_tab_hit_areas(row, &[4, 8, 8]);
    [
        areas.first().copied().unwrap_or_default(),
        areas.get(1).copied().unwrap_or_default(),
        areas.get(2).copied().unwrap_or_default(),
    ]
}

pub(crate) fn detail_tab_layouts(area: Rect) -> [Rect; 6] {
    let mut x = area.x;
    let mut y = area.y.saturating_add(3);
    std::array::from_fn(|index| {
        let label = DockHomeDetailTab::ALL[index].label();
        let padding = usize::from(index + 1 < DockHomeDetailTab::ALL.len());
        let width = u16::try_from(display_width(label).saturating_add(padding)).unwrap_or(u16::MAX);
        if x > area.x && x.saturating_add(width) > area.right() {
            x = area.x;
            y = y.saturating_add(1);
        }
        let visible_width = width.min(area.right().saturating_sub(x));
        let rect = Rect::new(x, y, visible_width, 1);
        x = x.saturating_add(width);
        rect
    })
}

fn pr_detail_pinned_rows(area: Rect) -> u16 {
    detail_tab_layouts(area)
        .iter()
        .map(|rect| rect.bottom())
        .max()
        .unwrap_or_else(|| area.y.saturating_add(3))
        .saturating_sub(area.y)
}

fn work_item_for_key<'a>(app: &'a AppState, key: &WorkItemKey) -> Option<&'a WorkItem> {
    app.work_index_snapshot.as_ref()?.items.iter().find(|item| {
        (key.pr_number.is_some()
            && item.pr_number == key.pr_number
            && item.repo.eq_ignore_ascii_case(&key.repo))
            || (key.pr_url.is_some() && item.pr_url == key.pr_url)
            || key.ticket_id.as_ref().is_some_and(|ticket_id| {
                item.ticket_ids
                    .iter()
                    .any(|candidate| candidate == ticket_id)
            })
    })
}

fn missing(value: Option<&str>) -> &str {
    value
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("—")
}

fn elapsed_from(timestamp: Option<SystemTime>, now: SystemTime) -> String {
    timestamp
        .and_then(|timestamp| now.duration_since(timestamp).ok())
        .map(compact_elapsed)
        .unwrap_or_else(|| "—".to_string())
}

fn status_line(app: &AppState, detail: &WorkItemDetail) -> Line<'static> {
    let mut values = Vec::new();
    if detail.is_draft == Some(true) {
        values.push(Span::styled(
            "draft",
            Style::default().fg(app.palette.overlay0),
        ));
    }
    if let Some(review) = detail
        .review_decision
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        if !values.is_empty() {
            values.push(Span::styled(
                " · ",
                Style::default().fg(app.palette.overlay0),
            ));
        }
        let color = match review {
            "REVIEW_REQUIRED" => app.palette.peach,
            "APPROVED" => app.palette.green,
            "CHANGES_REQUESTED" => app.palette.red,
            _ => app.palette.text,
        };
        let token = match review {
            "REVIEW_REQUIRED" => "RR".to_string(),
            "APPROVED" => "approved".to_string(),
            "CHANGES_REQUESTED" => "changes requested".to_string(),
            _ => review.to_ascii_lowercase().replace('_', " "),
        };
        values.push(Span::styled(token, Style::default().fg(color)));
    }
    for label in detail.labels.iter().filter(|label| !label.is_empty()) {
        if !values.is_empty() {
            values.push(Span::styled(
                " · ",
                Style::default().fg(app.palette.overlay0),
            ));
        }
        values.push(Span::styled(
            label.clone(),
            Style::default().fg(app.palette.text),
        ));
    }
    if values.is_empty() {
        Line::from(Span::styled("—", Style::default().fg(app.palette.text)))
    } else {
        Line::from(values)
    }
}

/// Colour for a check-run conclusion. GitHub's vocabulary is small and stable,
/// so an unknown value stays neutral rather than guessing.
fn action_state_color(app: &AppState, state: &str) -> ratatui::style::Color {
    match state.to_ascii_uppercase().as_str() {
        "SUCCESS" | "NEUTRAL" => app.palette.green,
        "FAILURE" | "TIMED_OUT" | "STARTUP_FAILURE" | "ACTION_REQUIRED" => app.palette.red,
        "CANCELLED" | "SKIPPED" | "STALE" => app.palette.overlay0,
        "IN_PROGRESS" | "QUEUED" | "PENDING" | "WAITING" | "REQUESTED" => app.palette.yellow,
        _ => app.palette.text,
    }
}

fn heading(app: &AppState, text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(
        text.into(),
        Style::default()
            .fg(app.palette.accent)
            .add_modifier(Modifier::BOLD),
    ))
}

fn value_line(app: &AppState, text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(
        text.into(),
        Style::default().fg(app.palette.text),
    ))
}

fn field_line(
    app: &AppState,
    fields: impl IntoIterator<Item = (String, String, ratatui::style::Color)>,
) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, (label, value, color)) in fields.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(
                " · ",
                Style::default().fg(app.palette.overlay0),
            ));
        }
        spans.push(Span::styled(
            label,
            Style::default().fg(app.palette.overlay0),
        ));
        spans.push(Span::styled(value, Style::default().fg(color)));
    }
    Line::from(spans)
}

fn link_color(app: &AppState, value: &str) -> ratatui::style::Color {
    if value == "—" {
        app.palette.text
    } else {
        app.palette.blue
    }
}

fn loaded_detail_lines(
    app: &AppState,
    row: &DockHomeRow,
    detail: &WorkItemDetail,
    width: usize,
) -> Vec<Line<'static>> {
    if let Some(reason) = detail.unavailable.as_deref() {
        return vec![value_line(app, format!("unavailable: {reason}"))];
    }
    let item = work_item_for_key(app, &row.key);
    let now = SystemTime::now();
    let ticket = if row.ticket_ids.is_empty() {
        "—".to_string()
    } else {
        row.ticket_ids.join(", ")
    };
    let author = missing(detail.author.as_deref());
    let opened = elapsed_from(detail.created_at, now);
    let updated = elapsed_from(detail.updated_at, now);
    let preview = row.preview_urls.first().map(String::as_str).unwrap_or("—");
    let pr_url = detail
        .url
        .as_deref()
        .or(row.key.pr_url.as_deref())
        .unwrap_or("—");
    let review_threads = detail
        .unresolved_review_threads
        .map(|count| format!("{count} unresolved"))
        .unwrap_or_else(|| "unknown".to_string());
    let checks = detail
        .checks
        .as_ref()
        .map(|checks| format!("{} failing of {}", checks.failing, checks.total))
        .unwrap_or_else(|| "—".to_string());
    let mut lines = vec![
        status_line(app, detail),
        value_line(app, missing(detail.title.as_deref())),
        field_line(
            app,
            [
                ("ticket ".into(), ticket, app.palette.text),
                ("author ".into(), author.into(), app.palette.text),
                ("opened ".into(), opened, app.palette.text),
                ("updated ".into(), updated, app.palette.text),
            ],
        ),
        Line::from(Span::styled(
            missing(detail.head_ref_name.as_deref()).to_string(),
            Style::default().fg(app.palette.mauve),
        )),
        Line::from(vec![
            Span::styled("  → ", Style::default().fg(app.palette.overlay0)),
            Span::styled(
                missing(detail.base_ref_name.as_deref()).to_string(),
                Style::default().fg(app.palette.mauve),
            ),
        ]),
        Line::default(),
        heading(app, "links"),
        field_line(
            app,
            [(
                " preview   ".into(),
                preview.into(),
                link_color(app, preview),
            )],
        ),
        field_line(
            app,
            [(" pr        ".into(), pr_url.into(), link_color(app, pr_url))],
        ),
    ];
    lines.push(Line::default());
    lines.push(heading(app, "tickets"));
    match row.ticket_ids.as_slice() {
        [] => lines.push(value_line(app, " —")),
        ticket_ids => {
            for ticket_id in ticket_ids {
                let title = if ticket_ids.len() == 1 {
                    item.and_then(|item| item.ticket_title.as_deref())
                        .unwrap_or("—")
                } else {
                    "—"
                };
                lines.push(value_line(app, format!(" {ticket_id}  {title}")));
            }
        }
    }
    lines.push(Line::default());
    lines.push(field_line(
        app,
        [(
            "● review threads  ".into(),
            review_threads,
            app.palette.text,
        )],
    ));
    let check_color = detail.checks.as_ref().map_or(app.palette.text, |summary| {
        if summary.failing == 0 {
            app.palette.green
        } else {
            app.palette.red
        }
    });
    lines.push(field_line(
        app,
        [("● checks  ".into(), checks, check_color)],
    ));
    lines.push(Line::default());
    lines.push(heading(app, "what it does"));
    lines.extend(markdown::body_lines(
        &app.palette,
        detail.body.as_deref(),
        width.saturating_sub(1),
        " ",
    ));
    lines
}

fn detail_lines(app: &AppState, row: &DockHomeRow, width: usize) -> Vec<Line<'static>> {
    if app.work_item_detail_loading.contains(&row.key) {
        return vec![value_line(app, "loading…")];
    }
    app.work_item_detail_cache
        .get(&row.key)
        .map(|detail| detail_tab_lines(app, row, detail, width))
        .unwrap_or_else(|| vec![value_line(app, "loading…")])
}

fn subtab_lines(
    app: &AppState,
    heading_text: &str,
    values: impl IntoIterator<Item = Line<'static>>,
    empty: &str,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::default(), heading(app, heading_text)];
    let values = values.into_iter().collect::<Vec<_>>();
    if values.is_empty() {
        lines.push(value_line(app, format!(" {empty}")));
    } else {
        lines.extend(values);
    }
    lines
}

fn detail_tab_lines(
    app: &AppState,
    row: &DockHomeRow,
    detail: &WorkItemDetail,
    width: usize,
) -> Vec<Line<'static>> {
    if let Some(reason) = detail.unavailable.as_deref() {
        return vec![value_line(app, format!("unavailable: {reason}"))];
    }
    match app.dock_home_detail_tab {
        DockHomeDetailTab::Overview => loaded_detail_lines(app, row, detail, width),
        DockHomeDetailTab::Comments => subtab_lines(
            app,
            "comments",
            detail.comments.iter().flat_map(|comment| {
                let author = missing(comment.author.as_deref()).to_string();
                let mut lines = vec![field_line(
                    app,
                    [(" author ".into(), author, app.palette.text)],
                )];
                lines.extend(markdown::body_lines(
                    &app.palette,
                    Some(&comment.body),
                    width.saturating_sub(1),
                    " ",
                ));
                lines
            }),
            "no comments",
        ),
        DockHomeDetailTab::Actions => subtab_lines(
            app,
            "actions",
            detail.actions.iter().map(|action| {
                Line::from(vec![
                    Span::styled(
                        format!(" {:<14}", action.state.to_ascii_lowercase()),
                        Style::default()
                            .fg(action_state_color(app, &action.state))
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(action.name.clone(), Style::default().fg(app.palette.text)),
                ])
            }),
            "no check runs",
        ),
        DockHomeDetailTab::Files => subtab_lines(
            app,
            "files",
            detail.files.iter().map(|file| {
                Line::from(vec![
                    Span::styled(
                        format!(" +{:<5}", file.additions),
                        Style::default().fg(app.palette.green),
                    ),
                    Span::styled(
                        format!("-{:<5}", file.deletions),
                        Style::default().fg(app.palette.red),
                    ),
                    Span::styled(file.path.clone(), Style::default().fg(app.palette.text)),
                ])
            }),
            "no changed files",
        ),
        DockHomeDetailTab::Commits => subtab_lines(
            app,
            "commits",
            detail.commits.iter().map(|commit| {
                Line::from(vec![
                    Span::styled(
                        format!(" {}  ", commit.short_id),
                        Style::default().fg(app.palette.mauve),
                    ),
                    Span::styled(
                        commit.subject.clone(),
                        Style::default().fg(app.palette.text),
                    ),
                ])
            }),
            "no commits",
        ),
        DockHomeDetailTab::Ticket => ticket_subtab_lines(app, row, width),
    }
}

fn ticket_subtab_lines(app: &AppState, row: &DockHomeRow, width: usize) -> Vec<Line<'static>> {
    if row.ticket_ids.is_empty() {
        return subtab_lines(app, "ticket", Vec::new(), "no linked ticket");
    }
    let Some(item) = work_item_for_key(app, &row.key) else {
        return vec![value_line(
            app,
            "unavailable: linked ticket detail is absent from the work index",
        )];
    };
    if item.ticket_details.is_empty() {
        return vec![value_line(
            app,
            "unavailable: Linear returned no linked ticket detail",
        )];
    }
    subtab_lines(
        app,
        "ticket",
        item.ticket_details.iter().flat_map(|ticket| {
            let mut lines = vec![value_line(
                app,
                format!(
                    " {}  {}",
                    ticket.identifier,
                    missing(ticket.title.as_deref())
                ),
            )];
            lines.extend(markdown::body_lines(
                &app.palette,
                ticket.description.as_deref(),
                width.saturating_sub(1),
                " ",
            ));
            lines
        }),
        "no linked ticket",
    )
}

/// One poll: the gate's own question, its options and its default, plus the
/// pane that raised it. The question and options run through the markdown
/// renderer, so an `(a-rec)` block keeps its emphasis.
fn poll_detail_lines(
    app: &AppState,
    row: &crate::work_projection::DockHomePollRow,
    width: usize,
) -> Vec<Line<'static>> {
    let item = &row.item;
    let mut lines = vec![
        Line::from(Span::styled(
            format!("{}  {}", item.n, item.label),
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD),
        )),
        field_line(
            app,
            [
                ("agent ".into(), row.agent_label.clone(), app.palette.text),
                (
                    "space ".into(),
                    row.workspace_label.clone(),
                    app.palette.text,
                ),
            ],
        ),
        Line::default(),
        heading(app, "asks"),
    ];
    lines.extend(markdown::body_lines(
        &app.palette,
        Some(item.text.as_str()),
        width.saturating_sub(1),
        " ",
    ));
    lines.push(Line::default());
    lines.push(heading(app, "default"));
    lines.extend(markdown::body_lines(
        &app.palette,
        item.default.as_deref(),
        width.saturating_sub(1),
        " ",
    ));
    lines.push(Line::default());
    lines.push(heading(app, "links"));
    lines.push(field_line(
        app,
        [(
            "pr ".into(),
            item.pr
                .map(|number| format!("#{number}"))
                .unwrap_or_else(|| "—".to_string()),
            app.palette.text,
        )],
    ));
    lines.push(field_line(
        app,
        [(
            "ticket ".into(),
            missing(item.ticket.as_deref()).to_string(),
            app.palette.text,
        )],
    ));
    lines.push(field_line(
        app,
        [(
            "url ".into(),
            missing(item.url.as_deref()).to_string(),
            link_color(app, missing(item.url.as_deref())),
        )],
    ));
    lines
}

fn ticket_detail_lines(
    app: &AppState,
    row: &DockHomeTicketRow,
    width: usize,
) -> Vec<Line<'static>> {
    let ticket = &row.ticket;
    let now = SystemTime::now();
    let labels = if ticket.labels.is_empty() {
        "—".to_string()
    } else {
        ticket.labels.join(" · ")
    };
    let linear_url = missing(ticket.url.as_deref());
    let pr_url = missing(row.linked_pr_url.as_deref());
    let mut lines = vec![
        Line::from(Span::styled(
            format!("{}  overview", ticket.identifier),
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD),
        )),
        value_line(app, labels),
        value_line(app, missing(ticket.title.as_deref())),
        field_line(
            app,
            [
                (
                    "state ".into(),
                    missing(ticket.state.as_deref()).into(),
                    app.palette.text,
                ),
                (
                    "assignee ".into(),
                    missing(ticket.assignee.as_deref()).into(),
                    app.palette.text,
                ),
                (
                    "opened ".into(),
                    elapsed_from(ticket.created_at, now),
                    app.palette.text,
                ),
                (
                    "updated ".into(),
                    elapsed_from(ticket.updated_at, now),
                    app.palette.text,
                ),
            ],
        ),
        Line::from(Span::styled(
            missing(ticket.branch.as_deref()).to_string(),
            Style::default().fg(app.palette.mauve),
        )),
        Line::default(),
        heading(app, "links"),
        field_line(
            app,
            [(
                " linear    ".into(),
                linear_url.into(),
                link_color(app, linear_url),
            )],
        ),
        field_line(
            app,
            [(" pr        ".into(), pr_url.into(), link_color(app, pr_url))],
        ),
    ];
    lines.push(Line::default());
    lines.push(heading(app, "parent"));
    lines.push(value_line(
        app,
        format!(" {}", missing(ticket.parent.as_deref())),
    ));
    lines.push(Line::default());
    lines.push(heading(app, "relations"));
    if ticket.relations.is_empty() {
        lines.push(value_line(app, " —"));
    } else {
        lines.extend(
            ticket
                .relations
                .iter()
                .map(|relation| value_line(app, format!(" {relation}"))),
        );
    }
    lines.push(Line::default());
    lines.push(heading(app, "what it does"));
    lines.extend(markdown::body_lines(
        &app.palette,
        ticket.description.as_deref(),
        width.saturating_sub(1),
        " ",
    ));
    lines
}

fn observed_line(projection: &DockHomeProjection, now: SystemTime) -> String {
    if let Some(reason) = projection.unavailable.as_deref() {
        return format!("unavailable: {reason}");
    }
    let Some(observed_at) = projection.observed_at else {
        // No observation at all. Name the cause instead of reporting one
        // indistinguishable `unknown` for a disabled index and a pending fetch.
        return if projection.index_enabled {
            "work index: no observation yet".to_string()
        } else {
            "work index off — set work_index.enabled".to_string()
        };
    };
    match now.duration_since(observed_at) {
        Ok(elapsed) => format!("observed {} ago", compact_elapsed(elapsed)),
        Err(_) => "observed unknown".to_string(),
    }
}

fn render_line(frame: &mut Frame, area: Rect, y: u16, text: String, style: Style) {
    if y >= area.bottom() {
        return;
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate_end(&text, usize::from(area.width)),
            style,
        ))),
        Rect::new(area.x, y, area.width, 1),
    );
}

fn render_hidden_tab_counts(app: &AppState, frame: &mut Frame, window: &TabWindow) {
    for (hidden, leading) in [
        (window.leading_hidden, true),
        (window.trailing_hidden, false),
    ] {
        let Some(hidden) = hidden else {
            continue;
        };
        frame.render_widget(
            Paragraph::new(hidden_count_label(hidden.count, leading))
                .style(Style::default().fg(app.palette.overlay0)),
            hidden.area,
        );
    }
}

fn render_footer(
    app: &AppState,
    projection: &DockHomeProjection,
    frame: &mut Frame,
    area: Rect,
    start_y: u16,
) {
    let style = Style::default().fg(app.palette.overlay0);
    render_line(
        frame,
        area,
        start_y,
        "─".repeat(usize::from(area.width)),
        Style::default().fg(app.palette.surface_dim),
    );
    let prs = projection
        .unbound_prs
        .map(|count| count.to_string())
        .unwrap_or_else(|| "—".to_string());
    let tickets = projection
        .unbound_tickets
        .map(|count| count.to_string())
        .unwrap_or_else(|| "—".to_string());
    render_line(
        frame,
        area,
        start_y.saturating_add(1),
        format!("unbound {prs} prs · {tickets} tickets"),
        style,
    );
    render_line(
        frame,
        area,
        start_y.saturating_add(2),
        observed_line(projection, SystemTime::now()),
        style,
    );
    render_line(
        frame,
        area,
        start_y.saturating_add(3),
        "age=pr open · rv=?/—/D/RR/✓/✗".to_string(),
        style,
    );
}

pub(super) fn render_home(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let projection = app.dock_home_projection();

    for (section, label, section_area) in [
        (DockHomeSection::Prs, "prs", section_layouts(area)[0]),
        (
            DockHomeSection::Tickets,
            "tickets",
            section_layouts(area)[1],
        ),
        (DockHomeSection::XPolls, "x-polls", section_layouts(area)[2]),
    ] {
        let style = if app.dock_home_section == section {
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.palette.overlay0)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(label, style))),
            section_area,
        );
    }

    let active_empty = match app.dock_home_section {
        DockHomeSection::Prs => projection.rows.is_empty(),
        DockHomeSection::Tickets => projection.ticket_rows.is_empty(),
        DockHomeSection::XPolls => projection.poll_rows.is_empty(),
    };
    if active_empty {
        let reason = if let Some(reason) = projection.unavailable.as_deref() {
            format!("unavailable: {reason}")
        } else {
            match app.dock_home_section {
                DockHomeSection::Prs => "no pr-bound panes",
                DockHomeSection::Tickets if !app.work_index_linear_team_configured => {
                    "tickets off — set work_index.linear_team"
                }
                DockHomeSection::Tickets => "no matching tickets",
                DockHomeSection::XPolls => "no open polls",
            }
            .to_string()
        };
        render_line(
            frame,
            area,
            area.y.saturating_add(TAB_ROWS),
            reason,
            Style::default().fg(app.palette.subtext0),
        );
        if app.dock_home_section == DockHomeSection::Prs {
            render_line(
                frame,
                area,
                area.y.saturating_add(TAB_ROWS + 1),
                "bind: herdr tab create --pr <url> --role review".to_string(),
                Style::default().fg(app.palette.overlay0),
            );
        }
        render_footer(
            app,
            &projection,
            frame,
            area,
            area.y.saturating_add(TAB_ROWS + 2),
        );
        return;
    }

    let selected_pr = app.dock_home_selected_index(&projection);
    let selected_ticket = app.dock_home_selected_ticket_index(&projection);
    let selected_poll = app.dock_home_selected_poll_index(&projection);
    match app.dock_home_section {
        DockHomeSection::Prs => {
            let window = tab_layouts(app, &projection, area);
            render_hidden_tab_counts(app, frame, &window);
            for (index, tab_area) in window.tabs {
                let row = &projection.rows[index];
                let style = if selected_pr == Some(index) {
                    Style::default()
                        .fg(app.palette.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.palette.overlay0)
                };
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(tab_label(row), style))),
                    tab_area,
                );
            }
        }
        DockHomeSection::Tickets => {
            let window = ticket_tab_layouts(app, &projection, area);
            render_hidden_tab_counts(app, frame, &window);
            for (index, tab_area) in window.tabs {
                let row = &projection.ticket_rows[index];
                let style = if selected_ticket == Some(index) {
                    Style::default()
                        .fg(app.palette.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.palette.overlay0)
                };
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        row.ticket.identifier.clone(),
                        style,
                    ))),
                    tab_area,
                );
            }
        }
        DockHomeSection::XPolls => {
            let window = poll_tab_layouts(app, &projection, area);
            render_hidden_tab_counts(app, frame, &window);
            for (index, tab_area) in window.tabs {
                let row = &projection.poll_rows[index];
                let style = if selected_poll == Some(index) {
                    Style::default()
                        .fg(app.palette.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.palette.overlay0)
                };
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(poll_tab_label(row), style))),
                    tab_area,
                );
            }
        }
    }

    if app.dock_home_section == DockHomeSection::Prs {
        if let Some(row) = selected_pr.and_then(|index| projection.rows.get(index)) {
            let identifier = row
                .key
                .pr_number
                .map(|number| format!("#{number}"))
                .unwrap_or_else(|| "#—".to_string());
            render_line(
                frame,
                area,
                area.y.saturating_add(TAB_ROWS),
                identifier,
                Style::default()
                    .fg(app.palette.accent)
                    .add_modifier(Modifier::BOLD),
            );
            for (tab, tab_area) in DockHomeDetailTab::ALL
                .into_iter()
                .zip(detail_tab_layouts(area))
            {
                let style = if app.dock_home_detail_tab == tab {
                    Style::default()
                        .fg(app.palette.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.palette.overlay0)
                };
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(tab.label(), style))),
                    tab_area,
                );
            }
        }
    }

    let detail_y = area
        .y
        .saturating_add(if app.dock_home_section == DockHomeSection::Prs {
            pr_detail_pinned_rows(area)
        } else {
            TAB_ROWS
        });
    // The footer follows the content rather than the pane floor, so a short
    // detail does not leave a gap between the last row and the legend. It only
    // reaches the floor when the body actually fills the available height.
    let footer_floor = area.bottom().saturating_sub(FOOTER_ROWS).max(detail_y);
    let detail_height = footer_floor.saturating_sub(detail_y);
    let lines = match app.dock_home_section {
        DockHomeSection::Prs => selected_pr
            .and_then(|index| projection.rows.get(index))
            .map(|row| detail_lines(app, row, usize::from(area.width))),
        DockHomeSection::Tickets => selected_ticket
            .and_then(|index| projection.ticket_rows.get(index))
            .map(|row| ticket_detail_lines(app, row, usize::from(area.width))),
        DockHomeSection::XPolls => selected_poll
            .and_then(|index| projection.poll_rows.get(index))
            .map(|row| poll_detail_lines(app, row, usize::from(area.width))),
    };
    let mut rendered_rows = 0u16;
    if let Some(lines) = lines {
        let max_scroll = lines.len().saturating_sub(usize::from(detail_height));
        let scroll = usize::from(app.dock_scroll).min(max_scroll);
        for (offset, line) in lines
            .into_iter()
            .skip(scroll)
            .take(usize::from(detail_height))
            .enumerate()
        {
            rendered_rows = rendered_rows.saturating_add(1);
            frame.render_widget(
                Paragraph::new(line),
                Rect::new(
                    area.x,
                    detail_y.saturating_add(offset as u16),
                    area.width,
                    1,
                ),
            );
        }
    }
    let footer_y = detail_y.saturating_add(rendered_rows).min(footer_floor);
    render_footer(app, &projection, frame, area, footer_y);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, style::Color, Terminal};
    use std::time::Duration;

    fn full_detail() -> crate::work_index::WorkItemDetail {
        crate::work_index::WorkItemDetail {
            number: Some(125),
            title: Some("detail panel for every pull request".into()),
            body: Some(
                "<!-- template instructions -->\n## Summary\n- Shows the complete pull request context without leaving Herdr.\n- Keeps long body content readable in the dock."
                    .into(),
            ),
            author: Some("ms".into()),
            base_ref_name: Some("main".into()),
            head_ref_name: Some("feat/pr-detail".into()),
            created_at: SystemTime::now().checked_sub(Duration::from_secs(14 * 86_400)),
            updated_at: SystemTime::now().checked_sub(Duration::from_secs(19 * 60)),
            labels: vec!["high-risk".into(), "risk:large-diff".into()],
            url: Some("https://github.com/herdrdev/herdr/pull/125".into()),
            review_decision: Some("REVIEW_REQUIRED".into()),
            is_draft: Some(false),
            checks: Some(crate::work_index::WorkItemCheckSummary {
                failing: 2,
                total: 8,
            }),
            comments: vec![crate::work_index::WorkItemComment {
                author: Some("reviewer".into()),
                body: "Please keep the body scrollable.".into(),
            }],
            actions: vec![crate::work_index::WorkItemAction {
                name: "test".into(),
                state: "FAILURE".into(),
            }],
            files: vec![crate::work_index::WorkItemFile {
                path: "src/ui/dock/home.rs".into(),
                additions: 42,
                deletions: 6,
            }],
            commits: vec![crate::work_index::WorkItemCommit {
                short_id: "abc1234".into(),
                subject: "fix dock detail".into(),
            }],
            unresolved_review_threads: None,
            unavailable: None,
            observed_at: SystemTime::now(),
        }
    }

    fn missing_optional_detail() -> crate::work_index::WorkItemDetail {
        crate::work_index::WorkItemDetail {
            number: None,
            title: None,
            body: None,
            author: None,
            base_ref_name: None,
            head_ref_name: None,
            created_at: None,
            updated_at: None,
            labels: Vec::new(),
            url: None,
            review_decision: None,
            is_draft: None,
            checks: None,
            comments: Vec::new(),
            actions: Vec::new(),
            files: Vec::new(),
            commits: Vec::new(),
            unresolved_review_threads: None,
            unavailable: None,
            observed_at: SystemTime::now(),
        }
    }

    fn bound_app(fetched: bool) -> AppState {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("review")];
        app.active = Some(0);
        app.ensure_test_terminals();
        let pane_id = app.workspaces[0].focused_pane_id().expect("pane");
        let terminal_id = app.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("terminal");
        let terminal = app.terminals.get_mut(&terminal_id).expect("terminal state");
        terminal.detected_agent = Some(crate::detect::Agent::Codex);
        terminal.state = crate::detect::AgentState::Working;
        terminal
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                pr_urls: Some(vec!["https://github.com/herdrdev/herdr/pull/125".into()]),
                ticket_ids: Some(vec!["MAT-124".into()]),
                work_title: Some("work view pr projection".into()),
                role: Some(crate::work_context::PaneWorkRole::Review),
                active_owner: Some(true),
                ..Default::default()
            })
            .expect("work context");
        if fetched {
            app.work_index_snapshot = Some(crate::work_index::Snapshot {
                items: vec![crate::work_index::WorkItem {
                    repo: "herdrdev/herdr".into(),
                    pr_number: Some(125),
                    pr_url: Some("https://github.com/herdrdev/herdr/pull/125".into()),
                    pr_title: Some("work view pr projection".into()),
                    pr_state: Some("open".into()),
                    draft: false,
                    review_decision: Some("REVIEW_REQUIRED".into()),
                    created_at: SystemTime::now().checked_sub(Duration::from_secs(241)),
                    ticket_ids: vec!["MAT-125".into()],
                    ticket_title: None,
                    ticket_state: None,
                    ticket_details: vec![crate::work_index::WorkTicket {
                        identifier: "MAT-125".into(),
                        title: Some("linked ticket".into()),
                        description: Some("Ticket body from Linear.".into()),
                        state: Some("In Progress".into()),
                        assignee: None,
                        created_at: None,
                        updated_at: None,
                        branch: None,
                        labels: Vec::new(),
                        url: None,
                        parent: None,
                        relations: Vec::new(),
                    }],
                    branch: None,
                    preview_urls: Vec::new(),
                    panes: vec![crate::work_index::WorkItemPane {
                        pane_id: pane_id.raw().to_string(),
                        agent_label: Some("codex".into()),
                        workspace_id: "ws".into(),
                        tab_id: "tab".into(),
                        role: None,
                        active_owner: true,
                        agent_status: crate::api::schema::AgentStatus::Working,
                    }],
                    source: crate::work_index::WorkItemSource::default(),
                }],
                unavailable: None,
                observed_at: SystemTime::now(),
            });
        }
        app
    }

    fn bound_app_with_prs(prs: &[(u64, crate::detect::AgentState)]) -> AppState {
        let mut app = AppState::test_new();
        app.workspaces = prs
            .iter()
            .map(|(number, _)| crate::workspace::Workspace::test_new(&format!("pr-{number}")))
            .collect();
        app.active = (!app.workspaces.is_empty()).then_some(0);
        app.ensure_test_terminals();
        for (ws_idx, (number, state)) in prs.iter().enumerate() {
            let pane_id = app.workspaces[ws_idx].focused_pane_id().expect("pane");
            let terminal_id = app.workspaces[ws_idx]
                .terminal_id(pane_id)
                .cloned()
                .expect("terminal");
            let terminal = app.terminals.get_mut(&terminal_id).expect("terminal state");
            terminal.detected_agent = Some(crate::detect::Agent::Codex);
            terminal.state = *state;
            terminal
                .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                    pr_urls: Some(vec![format!(
                        "https://github.com/herdrdev/herdr/pull/{number}"
                    )]),
                    work_title: Some(format!("pull request {number}")),
                    ..Default::default()
                })
                .expect("work context");
        }
        app
    }

    fn ticket_app(full: bool) -> AppState {
        let mut app = AppState::test_new();
        app.work_index_enabled = true;
        app.work_index_linear_team_configured = true;
        app.dock_home_section = DockHomeSection::Tickets;
        let ticket = crate::work_index::WorkTicket {
            identifier: "SCA-3084".into(),
            title: full.then(|| "ticket detail in the dock".into()),
            description: full.then(|| "A complete Linear description body.".into()),
            state: full.then(|| "In Progress".into()),
            assignee: full.then(|| "Matthias".into()),
            created_at: full.then(|| SystemTime::UNIX_EPOCH + Duration::from_secs(60)),
            updated_at: full.then(|| SystemTime::UNIX_EPOCH + Duration::from_secs(120)),
            branch: full.then(|| "sca-3084-dock-ticket".into()),
            labels: if full {
                vec!["fleet".into()]
            } else {
                Vec::new()
            },
            url: full.then(|| "https://linear.app/scalable/issue/SCA-3084".into()),
            parent: full.then(|| "SCA-3000  dock home".into()),
            relations: if full {
                vec!["blocks  SCA-3090  preload".into()]
            } else {
                Vec::new()
            },
        };
        let mut items = vec![crate::work_index::WorkItem {
            repo: "owner/repo".into(),
            pr_number: None,
            pr_url: None,
            pr_title: None,
            pr_state: None,
            draft: false,
            review_decision: None,
            created_at: None,
            ticket_ids: vec![ticket.identifier.clone()],
            ticket_title: ticket.title.clone(),
            ticket_state: ticket.state.clone(),
            ticket_details: vec![ticket],
            branch: None,
            preview_urls: Vec::new(),
            panes: Vec::new(),
            source: crate::work_index::WorkItemSource::default(),
        }];
        if full {
            items.push(crate::work_index::WorkItem {
                repo: "owner/repo".into(),
                pr_number: Some(42),
                pr_url: Some("https://github.com/owner/repo/pull/42".into()),
                pr_title: Some("linked pull request".into()),
                pr_state: Some("open".into()),
                draft: false,
                review_decision: None,
                created_at: None,
                ticket_ids: vec!["SCA-3084".into()],
                ticket_title: None,
                ticket_state: None,
                ticket_details: Vec::new(),
                branch: None,
                preview_urls: Vec::new(),
                panes: Vec::new(),
                source: crate::work_index::WorkItemSource::default(),
            });
        }
        app.work_index_snapshot = Some(crate::work_index::Snapshot {
            items,
            unavailable: None,
            observed_at: SystemTime::now(),
        });
        app
    }

    fn ticket_app_with_identifiers(identifiers: &[&str]) -> AppState {
        let mut app = ticket_app(false);
        app.work_index_snapshot = Some(crate::work_index::Snapshot {
            items: identifiers
                .iter()
                .map(|identifier| {
                    let ticket = crate::work_index::WorkTicket {
                        identifier: (*identifier).into(),
                        title: None,
                        description: None,
                        state: None,
                        assignee: None,
                        created_at: None,
                        updated_at: None,
                        branch: None,
                        labels: Vec::new(),
                        url: None,
                        parent: None,
                        relations: Vec::new(),
                    };
                    crate::work_index::WorkItem {
                        repo: "owner/repo".into(),
                        pr_number: None,
                        pr_url: None,
                        pr_title: None,
                        pr_state: None,
                        draft: false,
                        review_decision: None,
                        created_at: None,
                        ticket_ids: vec![ticket.identifier.clone()],
                        ticket_title: None,
                        ticket_state: None,
                        ticket_details: vec![ticket],
                        branch: None,
                        preview_urls: Vec::new(),
                        panes: Vec::new(),
                        source: crate::work_index::WorkItemSource::default(),
                    }
                })
                .collect(),
            unavailable: None,
            observed_at: SystemTime::now(),
        });
        app
    }

    fn selected_with_detail(
        mut app: AppState,
        detail: Option<crate::work_index::WorkItemDetail>,
    ) -> AppState {
        app.work_index_enabled = true;
        app.dock_home_focused = false;
        let key = app
            .dock_home_selected_row()
            .expect("selected detail row")
            .key;
        app.dock_home_selection = Some(key.clone());
        match detail {
            Some(detail) => app.work_item_detail_cache.insert(key, detail),
            None => {
                app.work_item_detail_loading.insert(key);
            }
        }
        app
    }

    fn render(app: &AppState, area: Rect) -> Terminal<TestBackend> {
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
        terminal
            .draw(|frame| render_home(app, frame, area))
            .expect("render home");
        terminal
    }

    fn text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn line_text(terminal: &Terminal<TestBackend>, y: u16) -> String {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect()
    }

    fn text_position(terminal: &Terminal<TestBackend>, needle: &str) -> (u16, u16) {
        let area = terminal.backend().buffer().area;
        for y in 0..area.height {
            let line = line_text(terminal, y);
            if let Some(byte_offset) = line.find(needle) {
                let x = display_width(&line[..byte_offset]) as u16;
                return (x, y);
            }
        }
        panic!("missing {needle:?}");
    }

    #[test]
    fn renders_the_empty_state_and_bind_hint() {
        let terminal = render(&AppState::test_new(), Rect::new(0, 0, 30, 10));
        let text = text(&terminal);
        assert!(text.contains("no pr-bound panes"), "{text:?}");
        assert!(text.contains("bind: herdr tab create --pr"), "{text:?}");
    }

    fn app_with_gate() -> AppState {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("alpha")];
        app.active = Some(0);
        app.selected = 0;
        app.ensure_test_terminals();
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0]
            .terminal_id(pane_id)
            .expect("root terminal")
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .closing_gates = vec![crate::api::schema::ClosingBlockItem {
            n: 1,
            label: "Gate".to_string(),
            text: "Rotate the **shared** token?".to_string(),
            pr: Some(42),
            ticket: Some("SCA-100".to_string()),
            url: None,
            default: Some("none".to_string()),
            default_at: None,
        }];
        app.dock_home_section = DockHomeSection::XPolls;
        app
    }

    #[test]
    fn the_x_polls_section_is_selectable_beside_prs_and_tickets() {
        let body_width = crate::ui::DOCK_DEFAULT_WIDTH.saturating_sub(2);
        let terminal = render(&AppState::test_new(), Rect::new(0, 0, body_width, 12));
        assert!(line_text(&terminal, 0).contains("x-polls"));
        assert_eq!(section_layouts(Rect::new(0, 0, body_width, 12)).len(), 3);
    }

    #[test]
    fn an_empty_x_polls_section_says_there_are_no_open_polls() {
        let mut app = AppState::test_new();
        app.dock_home_section = DockHomeSection::XPolls;
        let terminal = render(&app, Rect::new(0, 0, 60, 12));
        assert!(text(&terminal).contains("no open polls"));
    }

    #[test]
    fn a_poll_detail_names_the_ask_its_default_and_its_links() {
        let app = app_with_gate();
        let terminal = render(&app, Rect::new(0, 0, 60, 24));
        let rendered = text(&terminal);

        assert!(rendered.contains("Gate"), "{rendered}");
        assert!(rendered.contains("asks"), "{rendered}");
        // The markdown renderer drops the emphasis markers rather than printing them.
        assert!(rendered.contains("Rotate the shared token?"), "{rendered}");
        assert!(rendered.contains("default"), "{rendered}");
        assert!(rendered.contains("#42"), "{rendered}");
        assert!(rendered.contains("SCA-100"), "{rendered}");
    }

    #[test]
    fn section_selector_defaults_to_prs() {
        let app = AppState::test_new();
        assert_eq!(app.dock_home_section, DockHomeSection::Prs);
        let terminal = render(&app, Rect::new(0, 0, 30, 10));
        assert!(line_text(&terminal, 0).starts_with("prs tickets"));
    }

    #[test]
    fn ticket_detail_renders_every_linear_field() {
        let terminal = render(&ticket_app(true), Rect::new(0, 0, 64, 40));
        let rendered = text(&terminal);
        for expected in [
            "SCA-3084  overview",
            "fleet",
            "ticket detail in the dock",
            "state In Progress · assignee Matthias",
            "sca-3084-dock-ticket",
            "https://linear.app/scalable/issue/SCA-3084",
            "https://github.com/owner/repo/pull/42",
            "A complete Linear description body.",
            "SCA-3000  dock home",
            "blocks  SCA-3090  preload",
        ] {
            assert!(
                rendered.contains(expected),
                "missing {expected:?}: {rendered:?}"
            );
        }
    }

    #[test]
    fn absent_ticket_fields_render_explicit_dashes_without_panicking() {
        let rendered = text(&render(&ticket_app(false), Rect::new(0, 0, 50, 40)));
        assert!(
            rendered.contains("state — · assignee — · opened — · updated —"),
            "{rendered:?}"
        );
        assert!(rendered.contains("linear    —"), "{rendered:?}");
        assert!(rendered.contains("parent"), "{rendered:?}");
        assert!(rendered.contains("relations"), "{rendered:?}");
    }

    #[test]
    fn empty_ticket_states_name_configuration_and_no_matches_separately() {
        let mut app = AppState::test_new();
        app.dock_home_section = DockHomeSection::Tickets;
        let off = text(&render(&app, Rect::new(0, 0, 50, 12)));
        assert!(off.contains("tickets off — set work_index.linear_team"));

        app.work_index_linear_team_configured = true;
        let empty = text(&render(&app, Rect::new(0, 0, 50, 12)));
        assert!(empty.contains("no matching tickets"));
        assert!(!empty.contains("tickets off"));

        app.work_index_snapshot = Some(crate::work_index::Snapshot {
            items: Vec::new(),
            unavailable: Some("Linear observation timed out".into()),
            observed_at: SystemTime::now(),
        });
        let unavailable = text(&render(&app, Rect::new(0, 0, 50, 12)));
        assert!(
            unavailable.contains("unavailable: Linear observation timed out"),
            "{unavailable:?}"
        );
        assert!(!unavailable.contains("no matching tickets"));
    }

    #[test]
    fn renders_one_tab_per_bound_pr_in_canonical_order() {
        let app = bound_app_with_prs(&[
            (129, crate::detect::AgentState::Working),
            (128, crate::detect::AgentState::Idle),
            (3487, crate::detect::AgentState::Blocked),
        ]);
        let projection = app.dock_home_projection();
        assert_eq!(
            projection
                .rows
                .iter()
                .map(|row| row.number.as_str())
                .collect::<Vec<_>>(),
            vec!["129", "128", "3487"]
        );
        let strip = line_text(&render(&app, Rect::new(0, 0, 60, 10)), 1);
        let first = strip.find("● #129").expect("first canonical tab");
        let second = strip.find("○ #128").expect("second canonical tab");
        let third = strip.find("○ #3487").expect("third canonical tab");
        assert!(first < second && second < third, "{strip:?}");
    }

    #[test]
    fn unavailable_reason_replaces_observed_footer() {
        let mut app = AppState::test_new();
        app.work_index_snapshot = Some(crate::work_index::Snapshot {
            items: Vec::new(),
            unavailable: Some("github timed out".into()),
            observed_at: SystemTime::now(),
        });
        let terminal = render(&app, Rect::new(0, 0, 30, 10));
        let text = text(&terminal);
        assert!(text.contains("unavailable: github timed out"), "{text:?}");
        assert!(!text.contains("observed"), "{text:?}");
    }

    #[test]
    fn footer_names_pr_open_age_and_review_codes() {
        let body_width = crate::ui::DOCK_DEFAULT_WIDTH.saturating_sub(2);
        let terminal = render(&AppState::test_new(), Rect::new(0, 0, body_width, 10));
        let legend = line_text(&terminal, 7);
        assert_eq!(
            legend.trim_end(),
            "age=pr open · rv=?/—/D/RR/✓/✗",
            "{legend:?}"
        );
    }

    #[test]
    fn future_observation_time_renders_unknown_instead_of_guessing_zero() {
        let projection = DockHomeProjection {
            rows: Vec::new(),
            ticket_rows: Vec::new(),
            poll_rows: Vec::new(),
            unbound_prs: None,
            unbound_tickets: None,
            observed_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(60)),
            unavailable: None,
            index_enabled: true,
        };

        assert_eq!(
            observed_line(&projection, SystemTime::UNIX_EPOCH),
            "observed unknown"
        );
    }

    #[test]
    fn selected_tab_uses_dock_accent_and_bold_without_a_background() {
        let app = bound_app(true);
        let terminal = render(&app, Rect::new(0, 0, 30, 10));
        let cell = &terminal.backend().buffer()[(0, 0)];
        assert_eq!(cell.fg, app.palette.accent);
        assert!(cell.modifier.contains(Modifier::BOLD));
        assert_eq!(cell.bg, Color::Reset);
    }

    #[test]
    fn detail_hierarchy_uses_palette_roles_and_keeps_check_text() {
        let app = selected_with_detail(bound_app(true), Some(full_detail()));
        let terminal = render(&app, Rect::new(0, 0, 100, 40));
        let buffer = terminal.backend().buffer();

        let header = (0, 2);
        assert!(line_text(&terminal, header.1).starts_with("#125"));
        assert_eq!(buffer[header].fg, app.palette.accent);
        assert!(buffer[header].modifier.contains(Modifier::BOLD));

        let heading = text_position(&terminal, "links");
        assert_eq!(buffer[heading].fg, app.palette.accent);
        assert!(buffer[heading].modifier.contains(Modifier::BOLD));
        assert_eq!(
            buffer[(heading.0, heading.1.saturating_sub(1))].symbol(),
            " "
        );

        let url = text_position(&terminal, "https://github.com/herdrdev/herdr/pull/125");
        assert_eq!(buffer[url].fg, app.palette.blue);

        let checks = text_position(&terminal, "2 failing of 8");
        assert_eq!(buffer[checks].fg, app.palette.red);
        assert!(line_text(&terminal, checks.1).contains("● checks  2 failing of 8"));
    }

    #[test]
    fn review_and_check_states_keep_text_tokens_while_palette_changes() {
        let mut detail = full_detail();
        let app = selected_with_detail(bound_app(true), Some(detail.clone()));
        let terminal = render(&app, Rect::new(0, 0, 100, 40));
        let review_required = text_position(&terminal, "RR");
        assert_eq!(
            terminal.backend().buffer()[review_required].fg,
            app.palette.peach
        );

        detail.review_decision = Some("APPROVED".into());
        detail.checks = Some(crate::work_index::WorkItemCheckSummary {
            failing: 0,
            total: 8,
        });
        let approved_app = selected_with_detail(bound_app(true), Some(detail.clone()));
        let approved = render(&approved_app, Rect::new(0, 0, 100, 40));
        let approved_token = text_position(&approved, "approved");
        assert_eq!(
            approved.backend().buffer()[approved_token].fg,
            approved_app.palette.green
        );
        let passing = text_position(&approved, "0 failing of 8");
        assert_eq!(
            approved.backend().buffer()[passing].fg,
            approved_app.palette.green
        );

        detail.is_draft = Some(true);
        detail.review_decision = None;
        let draft_app = selected_with_detail(bound_app(true), Some(detail));
        let draft = render(&draft_app, Rect::new(0, 0, 100, 40));
        let draft_token = text_position(&draft, "draft");
        assert_eq!(
            draft.backend().buffer()[draft_token].fg,
            draft_app.palette.overlay0
        );
    }

    #[test]
    fn tab_hit_areas_match_every_rendered_tab_on_one_row() {
        let app = bound_app_with_prs(&[
            (129, crate::detect::AgentState::Working),
            (128, crate::detect::AgentState::Idle),
        ]);
        let projection = app.dock_home_projection();
        let areas = tab_layouts(&app, &projection, Rect::new(4, 7, 30, 10))
            .tabs
            .into_iter()
            .map(|(_, area)| area)
            .collect::<Vec<_>>();
        assert_eq!(areas.len(), projection.rows.len());
        assert_eq!(areas, vec![Rect::new(4, 8, 7, 1), Rect::new(11, 8, 7, 1)]);
    }

    #[test]
    fn the_footer_sits_directly_under_a_short_detail_instead_of_the_pane_floor() {
        let app = selected_with_detail(bound_app(true), Some(full_detail()));
        let terminal = render(&app, Rect::new(0, 0, 60, 40));
        let (_, rule_y) = text_position(&terminal, "unbound");
        let last_content = (0..rule_y)
            .rev()
            .find(|y| !line_text(&terminal, *y).trim().is_empty())
            .expect("a content row above the footer");

        assert_eq!(
            rule_y,
            last_content.saturating_add(1),
            "the footer rule should start on the row after the last content row"
        );
        assert!(
            rule_y < 40 - FOOTER_ROWS,
            "a short detail must not pin the footer to the pane floor"
        );
    }

    #[test]
    fn a_detail_that_fills_the_pane_keeps_the_footer_at_the_floor() {
        let app = selected_with_detail(bound_app(true), Some(full_detail()));
        let terminal = render(&app, Rect::new(0, 0, 60, 12));
        let (_, rule_y) = text_position(&terminal, "unbound");

        assert_eq!(rule_y, 12 - FOOTER_ROWS + 1);
    }

    #[test]
    fn renders_full_pull_request_detail_in_ghx_field_order() {
        let app = selected_with_detail(bound_app(true), Some(full_detail()));
        let terminal = render(&app, Rect::new(0, 0, 60, 40));
        let text = text(&terminal);
        let overview = text.find("overview comments").expect("overview sub-tab");
        let links = text.find("links").expect("links");
        let tickets = text[links..]
            .find("tickets")
            .map(|offset| links + offset)
            .expect("tickets heading");
        let review = text
            .find("review threads  unknown")
            .expect("review threads");
        let checks = text.find("checks  2 failing of 8").expect("checks");
        let body = text.find("what it does").expect("body heading");

        assert!(overview < links && links < tickets && tickets < review);
        assert!(review < checks && checks < body);
        assert!(text.contains("feat/pr-detail"));
        assert!(text.contains("preview   —"));
        assert!(!text.contains("template instructions"));
    }

    #[test]
    fn optional_detail_fields_render_explicit_unknown_values() {
        let app = selected_with_detail(bound_app(false), Some(missing_optional_detail()));
        let terminal = render(&app, Rect::new(0, 0, 50, 35));
        let text = text(&terminal);

        assert!(line_text(&terminal, 2).starts_with("#125"), "{text:?}");
        assert!(line_text(&terminal, 3).starts_with("overview"), "{text:?}");
        assert!(text.contains("opened — · updated —"), "{text:?}");
        assert!(text.contains("review threads  unknown"), "{text:?}");
        assert!(text.contains("checks  —"), "{text:?}");
    }

    #[test]
    fn in_flight_detail_renders_loading_state() {
        let app = selected_with_detail(bound_app(true), None);
        let terminal = render(&app, Rect::new(0, 0, 30, 12));

        assert!(text(&terminal).contains("loading…"));
    }

    #[test]
    fn prefetched_entry_renders_immediately_without_loading() {
        let app = selected_with_detail(bound_app(true), Some(full_detail()));
        let rendered = text(&render(&app, Rect::new(0, 0, 60, 40)));

        assert!(
            rendered.contains("overview comments actions"),
            "{rendered:?}"
        );
        assert!(line_text(&render(&app, Rect::new(0, 0, 60, 40)), 2).starts_with("#125"));
        assert!(!rendered.contains("loading…"), "{rendered:?}");
    }

    #[test]
    fn minimum_dock_width_detail_truncates_without_horizontal_wrap() {
        let app = selected_with_detail(bound_app(true), Some(full_detail()));
        let body_width = crate::ui::DOCK_MIN_WIDTH.saturating_sub(2);
        let terminal = render(&app, Rect::new(0, 0, body_width, 40));
        let rendered = text(&terminal);

        for y in 0..terminal.backend().buffer().area.height {
            assert_eq!(
                display_width(&line_text(&terminal, y)),
                usize::from(body_width),
                "line {y} escaped the dock"
            );
        }
        assert!(rendered.contains('…'), "{rendered:?}");
        assert!(!rendered.contains("template instructions"));
    }

    #[test]
    fn ordinary_height_detail_scroll_reaches_body_after_summary_fields() {
        let mut app = selected_with_detail(bound_app(true), Some(full_detail()));
        let area = Rect::new(0, 0, 40, 24);
        let initial_terminal = render(&app, area);
        let initial = text(&initial_terminal);
        assert!(initial.contains("checks  2 failing of 8"), "{initial:?}");
        assert!(!initial.contains("Keeps long body content"), "{initial:?}");

        app.dock_scroll = u16::MAX;
        let scrolled_terminal = render(&app, area);
        let scrolled = text(&scrolled_terminal);
        assert!(scrolled.contains("what it does"), "{scrolled:?}");
        assert!(scrolled.contains("Keeps long body content"), "{scrolled:?}");
        assert_eq!(
            line_text(&scrolled_terminal, 2),
            line_text(&initial_terminal, 2)
        );
        assert_eq!(
            line_text(&scrolled_terminal, 3),
            line_text(&initial_terminal, 3)
        );
    }

    #[test]
    fn selected_tab_always_renders_its_detail_body() {
        let mut app = bound_app_with_prs(&[
            (129, crate::detect::AgentState::Working),
            (128, crate::detect::AgentState::Idle),
        ]);
        let projection = app.dock_home_projection();
        let selected = projection.rows[1].key.clone();
        app.dock_home_selection = Some(selected.clone());
        app.dock_home_focused = false;
        let mut detail = full_detail();
        detail.number = Some(128);
        detail.title = Some("selected pull request".into());
        app.work_item_detail_cache.insert(selected, detail);

        let rendered = text(&render(&app, Rect::new(0, 0, 60, 40)));
        let terminal = render(&app, Rect::new(0, 0, 60, 40));
        assert!(line_text(&terminal, 2).starts_with("#128"), "{rendered:?}");
        assert!(!line_text(&terminal, 2).contains("#129"), "{rendered:?}");
    }

    #[test]
    fn lifecycle_glyph_changes_do_not_move_tabs() {
        let mut app = bound_app_with_prs(&[
            (129, crate::detect::AgentState::Working),
            (128, crate::detect::AgentState::Idle),
        ]);
        let area = Rect::new(0, 0, 30, 12);
        let before = line_text(&render(&app, area), 1);
        let before_129 = before.find("#129").expect("#129 tab");
        let before_128 = before.find("#128").expect("#128 tab");

        let terminal_ids = app
            .workspaces
            .iter()
            .map(|workspace| {
                let pane_id = workspace.focused_pane_id().expect("pane");
                workspace.terminal_id(pane_id).cloned().expect("terminal")
            })
            .collect::<Vec<_>>();
        app.terminals
            .get_mut(&terminal_ids[0])
            .expect("first terminal")
            .state = crate::detect::AgentState::Idle;
        app.terminals
            .get_mut(&terminal_ids[1])
            .expect("second terminal")
            .state = crate::detect::AgentState::Working;

        let after = line_text(&render(&app, area), 1);
        assert_eq!(after.find("#129"), Some(before_129));
        assert_eq!(after.find("#128"), Some(before_128));
        assert!(after.contains("○ #129"), "{after:?}");
        assert!(after.contains("● #128"), "{after:?}");
    }

    #[test]
    fn high_count_pr_window_keeps_full_labels_and_reports_hidden_counts() {
        let mut app = bound_app_with_prs(&[
            (2412, crate::detect::AgentState::Working),
            (2413, crate::detect::AgentState::Working),
            (2414, crate::detect::AgentState::Blocked),
            (2415, crate::detect::AgentState::Idle),
            (2416, crate::detect::AgentState::Unknown),
            (2417, crate::detect::AgentState::Working),
        ]);
        let body_width = crate::ui::DOCK_MIN_WIDTH.saturating_sub(2);
        let area = Rect::new(0, 0, body_width, 12);
        let projection = app.dock_home_projection();
        let initial = tab_layouts(&app, &projection, area);
        assert_eq!(initial.leading_hidden, None);
        assert_eq!(initial.trailing_hidden.map(|hidden| hidden.count), Some(5));
        assert_eq!(
            initial
                .tabs
                .iter()
                .map(|(index, _)| *index)
                .collect::<Vec<_>>(),
            vec![0]
        );
        assert!(line_text(&render(&app, area), 1).contains("5 ›"));

        app.move_dock_home_selection(5);
        let window = tab_layouts(&app, &projection, area);
        assert_eq!(window.leading_hidden.map(|hidden| hidden.count), Some(5));
        assert_eq!(window.trailing_hidden, None);
        assert_eq!(
            window
                .tabs
                .iter()
                .map(|(index, _)| *index)
                .collect::<Vec<_>>(),
            vec![5]
        );
        let terminal = render(&app, area);
        let strip = line_text(&terminal, 1);
        assert!(strip.contains("‹ 5"), "{strip:?}");
        assert!(strip.contains("#2417"), "{strip:?}");
        assert!(!strip.contains('…'), "{strip:?}");

        for (index, tab_area) in window.tabs {
            let row = &projection.rows[index];
            let actual = (tab_area.x..tab_area.right())
                .map(|x| terminal.backend().buffer()[(x, tab_area.y)].symbol())
                .collect::<String>();
            assert_eq!(tab_area.width, tab_cell_width(&tab_label(row)));
            assert_eq!(actual.trim_end(), tab_label(row), "tab #{}", row.number);
        }
    }

    #[test]
    fn high_count_ticket_window_uses_the_same_full_identifier_rule() {
        let mut app = ticket_app_with_identifiers(&[
            "SCA-2412", "SCA-2413", "SCA-2414", "SCA-2415", "SCA-2416", "SCA-2417",
        ]);
        let area = Rect::new(0, 0, crate::ui::DOCK_MIN_WIDTH.saturating_sub(2), 12);
        let projection = app.dock_home_projection();

        let initial = ticket_tab_layouts(&app, &projection, area);
        assert_eq!(initial.tabs.len(), 1);
        assert_eq!(initial.trailing_hidden.map(|hidden| hidden.count), Some(5));
        assert_eq!(initial.tabs[0].1.width, tab_cell_width("SCA-2412"));

        app.move_dock_home_selection(5);
        let window = ticket_tab_layouts(&app, &projection, area);
        assert_eq!(
            window
                .tabs
                .iter()
                .map(|(index, _)| *index)
                .collect::<Vec<_>>(),
            vec![5]
        );
        assert_eq!(window.leading_hidden.map(|hidden| hidden.count), Some(5));
        let strip = line_text(&render(&app, area), 1);
        assert!(strip.contains("‹ 5"), "{strip:?}");
        assert!(strip.contains("SCA-2417"), "{strip:?}");
        assert!(!strip.contains('…'), "{strip:?}");
    }

    #[test]
    fn allocator_omits_a_tab_when_its_full_identifier_cannot_fit() {
        let window = selection_following_tab_window(Rect::new(0, 0, 7, 1), &[9], 0);
        assert!(window.tabs.is_empty());
    }

    #[test]
    fn full_body_scroll_reaches_last_line_and_keeps_pinned_rows() {
        let mut detail = full_detail();
        detail.body = Some(
            (0..30)
                .map(|index| format!("body-line-{index:02}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let mut app = selected_with_detail(bound_app(true), Some(detail));
        let area = Rect::new(0, 0, 60, 28);
        let initial = render(&app, area);
        assert!(text(&initial).contains("body-line-00"));
        assert!(!text(&initial).contains("body-line-29"));
        let pinned_header = line_text(&initial, 2);
        let pinned_tabs = line_text(&initial, 3);

        app.dock_scroll = u16::MAX;
        let scrolled = render(&app, area);
        assert!(text(&scrolled).contains("body-line-29"));
        assert_eq!(line_text(&scrolled, 2), pinned_header);
        assert_eq!(line_text(&scrolled, 3), pinned_tabs);
    }

    #[test]
    fn sub_tab_rows_are_colour_coded_rather_than_plain_text() {
        let mut app = selected_with_detail(bound_app(true), Some(full_detail()));
        app.dock_home_detail_tab = DockHomeDetailTab::Actions;
        let terminal = render(&app, Rect::new(0, 0, 80, 24));
        let buffer = terminal.backend().buffer();

        let failed = (0..buffer.area.height).any(|y| {
            (0..buffer.area.width).any(|x| buffer[(x, y)].style().fg == Some(app.palette.red))
        });
        assert!(failed, "a failing check run should be red");

        app.dock_home_detail_tab = DockHomeDetailTab::Files;
        let terminal = render(&app, Rect::new(0, 0, 80, 24));
        let buffer = terminal.backend().buffer();
        let added = (0..buffer.area.height).any(|y| {
            (0..buffer.area.width).any(|x| buffer[(x, y)].style().fg == Some(app.palette.green))
        });
        assert!(added, "additions should be green");
    }

    #[test]
    fn every_fetched_sub_tab_renders_loaded_data() {
        for (tab, expected) in [
            (
                DockHomeDetailTab::Comments,
                "Please keep the body scrollable.",
            ),
            // The state, counts and short id are colour-coded and column-aligned,
            // so each row is scannable rather than a run of plain text.
            (DockHomeDetailTab::Actions, "failure"),
            (DockHomeDetailTab::Files, "src/ui/dock/home.rs"),
            (DockHomeDetailTab::Commits, "abc1234"),
            (DockHomeDetailTab::Ticket, "MAT-125  linked ticket"),
        ] {
            let mut app = selected_with_detail(bound_app(true), Some(full_detail()));
            app.dock_home_detail_tab = tab;
            let rendered = text(&render(&app, Rect::new(0, 0, 80, 24)));
            assert!(rendered.contains(expected), "{tab:?}: {rendered:?}");
        }
    }

    #[test]
    fn detail_sub_tabs_wrap_at_default_width_without_elision() {
        let app = selected_with_detail(bound_app(true), Some(full_detail()));
        let area = Rect::new(0, 0, crate::ui::DOCK_DEFAULT_WIDTH.saturating_sub(2), 20);
        let terminal = render(&app, area);

        for (tab, tab_area) in DockHomeDetailTab::ALL
            .into_iter()
            .zip(detail_tab_layouts(area))
        {
            let rendered = (tab_area.x..tab_area.right())
                .map(|x| terminal.backend().buffer()[(x, tab_area.y)].symbol())
                .collect::<String>();
            assert_eq!(rendered.trim_end(), tab.label(), "{tab:?}");
        }
    }

    #[test]
    fn every_fetched_sub_tab_names_loading_and_empty_states() {
        for tab in [
            DockHomeDetailTab::Comments,
            DockHomeDetailTab::Actions,
            DockHomeDetailTab::Files,
            DockHomeDetailTab::Commits,
            DockHomeDetailTab::Ticket,
        ] {
            let mut loading = selected_with_detail(bound_app(true), None);
            loading.dock_home_detail_tab = tab;
            assert!(
                text(&render(&loading, Rect::new(0, 0, 80, 16))).contains("loading…"),
                "{tab:?}"
            );
        }

        let mut empty_detail = full_detail();
        empty_detail.comments.clear();
        empty_detail.actions.clear();
        empty_detail.files.clear();
        empty_detail.commits.clear();
        for (tab, expected) in [
            (DockHomeDetailTab::Comments, "no comments"),
            (DockHomeDetailTab::Actions, "no check runs"),
            (DockHomeDetailTab::Files, "no changed files"),
            (DockHomeDetailTab::Commits, "no commits"),
        ] {
            let mut app = selected_with_detail(bound_app(true), Some(empty_detail.clone()));
            app.dock_home_detail_tab = tab;
            let rendered = text(&render(&app, Rect::new(0, 0, 80, 16)));
            assert!(rendered.contains(expected), "{tab:?}: {rendered:?}");
        }

        let mut no_ticket = selected_with_detail(
            bound_app_with_prs(&[(125, crate::detect::AgentState::Working)]),
            Some(empty_detail),
        );
        no_ticket.dock_home_detail_tab = DockHomeDetailTab::Ticket;
        assert!(text(&render(&no_ticket, Rect::new(0, 0, 80, 16))).contains("no linked ticket"));
    }
}
