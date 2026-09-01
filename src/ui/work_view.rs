use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::{
    app::state::{Palette, WorkProjection, WorkViewState},
    work_projection::{project_pull_requests, project_review_queue, WorkPrRow, WorkReviewQueueRow},
};

pub(crate) fn render(palette: &Palette, state: &WorkViewState, area: Rect, frame: &mut Frame) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let sections = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
    match state.projection {
        WorkProjection::PullRequests => render_pull_requests(palette, state, sections[0], frame),
        WorkProjection::ReviewQueue => render_review_queue(palette, state, sections[0], frame),
        projection => render_placeholder(palette, projection, sections[0], frame),
    }
    render_footer(palette, state, sections[1], frame);
}

fn render_review_queue(palette: &Palette, state: &WorkViewState, area: Rect, frame: &mut Frame) {
    let scope = state
        .repo_filter
        .as_deref()
        .map(short_repo_name)
        .unwrap_or("all repos");
    let projection = state
        .snapshot
        .as_ref()
        .filter(|_| state.enabled)
        .map(|snapshot| project_review_queue(snapshot, state.repo_filter.as_deref()));
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" work · review queue · {scope} "))
        .border_style(Style::default().fg(palette.accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let message = if !state.enabled {
        Some("work index disabled")
    } else if state.snapshot.is_none() {
        Some("work index not yet collected")
    } else {
        state
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.unavailable.as_deref())
    };
    if let Some(message) = message {
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(palette.subtext0)),
            inner,
        );
        return;
    }

    let Some(projection) = projection else {
        return;
    };
    let mut lines = vec![Line::styled(
        format_review_queue_summary(
            projection.awaiting_review_count,
            projection.ticket_in_review_count,
            inner.width,
        ),
        Style::default()
            .fg(palette.subtext0)
            .add_modifier(Modifier::BOLD),
    )];
    if projection.rows.is_empty() {
        lines.push(Line::styled(
            "  no PRs awaiting review",
            Style::default().fg(palette.subtext0),
        ));
    } else {
        let selected = state.selected_review_queue_index(&projection).unwrap_or(0);
        for (index, row) in projection.rows.iter().enumerate() {
            let style = if index == selected {
                Style::default()
                    .fg(palette.text)
                    .bg(palette.surface0)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.text)
            };
            lines.push(Line::styled(
                format_review_queue_row(row, inner.width),
                style,
            ));
        }
    }
    lines.push(Line::styled(
        "─".repeat(inner.width as usize),
        Style::default().fg(palette.surface1),
    ));
    lines.push(Line::styled(
        format!(
            "  drift {} PRs awaiting review whose ticket is not In Review",
            projection.drift_count
        ),
        Style::default().fg(palette.subtext0),
    ));
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_pull_requests(palette: &Palette, state: &WorkViewState, area: Rect, frame: &mut Frame) {
    let scope = state
        .repo_filter
        .as_deref()
        .map(short_repo_name)
        .unwrap_or("all repos");
    let projection = state
        .snapshot
        .as_ref()
        .filter(|_| state.enabled)
        .map(|snapshot| project_pull_requests(snapshot, state.repo_filter.as_deref()));
    let count = projection
        .as_ref()
        .map_or(0, |projection| projection.row_count);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" work · PRs · {scope} · {count} open "))
        .border_style(Style::default().fg(palette.accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let message = if !state.enabled {
        Some("work index disabled")
    } else if state.snapshot.is_none() {
        Some("work index not yet collected")
    } else {
        state
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.unavailable.as_deref())
    };
    if let Some(message) = message {
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(palette.subtext0)),
            inner,
        );
        return;
    }

    let Some(projection) = projection else {
        return;
    };
    if projection.row_count == 0 {
        frame.render_widget(
            Paragraph::new("no open pull requests").style(Style::default().fg(palette.subtext0)),
            inner,
        );
        return;
    }

    let selected = state.selected_index(&projection).unwrap_or(0);
    let mut lines = Vec::new();
    let mut row_index = 0;
    for group in &projection.groups {
        if group.no_ticket {
            if !lines.is_empty() {
                lines.push(Line::styled(
                    "─".repeat(inner.width as usize),
                    Style::default().fg(palette.surface1),
                ));
            }
            lines.push(group_header(palette, &group.header));
        } else if state.repo_filter.is_none() {
            lines.push(group_header(palette, &group.header));
        }
        for row in &group.rows {
            let is_selected = row_index == selected;
            let style = if is_selected {
                Style::default()
                    .fg(palette.text)
                    .bg(palette.surface0)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.text)
            };
            lines.push(Line::styled(
                format_pull_request_row(row, is_selected, inner.width),
                style,
            ));
            row_index += 1;
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn group_header<'a>(palette: &Palette, value: &'a str) -> Line<'a> {
    Line::styled(
        format!("  {value}"),
        Style::default()
            .fg(palette.subtext0)
            .add_modifier(Modifier::BOLD),
    )
}

fn render_placeholder(
    palette: &Palette,
    projection: WorkProjection,
    area: Rect,
    frame: &mut Frame,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" work · {} ", projection.label()))
        .border_style(Style::default().fg(palette.accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(format!("{} not yet available", projection.label()))
            .style(Style::default().fg(palette.subtext0)),
        inner,
    );
}

fn render_footer(palette: &Palette, state: &WorkViewState, area: Rect, frame: &mut Frame) {
    let base = match state.projection {
        WorkProjection::PullRequests => {
            " ←/→ view [PRs] tickets agents   ↑/↓ move   ⏎ attach agent   f filter repo   RR=review req"
        }
        WorkProjection::Tickets => " ←/→ view PRs [tickets] agents   not yet available",
        WorkProjection::Agents => " ←/→ view PRs tickets [agents]   not yet available",
        WorkProjection::ReviewQueue => {
            " ←/→ view PRs tickets agents [review queue]   ↑/↓ move   f filter repo"
        }
    };
    let text = state
        .hint
        .as_deref()
        .map_or_else(|| base.to_string(), |hint| format!(" {hint}"));
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(palette.subtext0)),
        area,
    );
}

fn format_review_queue_summary(awaiting: usize, in_review: usize, width: u16) -> String {
    let left = format!("  awaiting review · {awaiting}");
    let right = format!("ticket says \"In Review\" · {in_review}");
    let width = usize::from(width);
    let left_width = width.div_ceil(2);
    format!(
        "{}{}",
        fit_cell(&left, left_width),
        fit_cell(&right, width.saturating_sub(left_width))
    )
}

fn format_review_queue_row(row: &WorkReviewQueueRow, width: u16) -> String {
    let ticket_width = 11;
    let state_width = 13;
    let verdict_width = 13;
    let fixed_width = 2 + 6 + 2 + 2 + ticket_width + 2 + state_width + 2 + verdict_width;
    let title_width = usize::from(width).saturating_sub(fixed_width).max(8);
    format!(
        "  {:<6}  {}  {}  {}  {}",
        row.number,
        fit_cell(&row.title, title_width),
        fit_cell(&row.ticket, ticket_width),
        fit_cell(&row.ticket_state, state_width),
        fit_cell(row.verdict.label(), verdict_width),
    )
}

fn format_pull_request_row(row: &WorkPrRow, selected: bool, width: u16) -> String {
    let owner_width = 15;
    let ticket_width = 11;
    let fixed_width = 2 + 6 + 2 + 2 + owner_width + 2 + ticket_width + 2 + 2;
    let title_width = usize::from(width).saturating_sub(fixed_width).max(8);
    let marker = if selected { "▸ " } else { "  " };
    let owner = match row.owner.as_deref() {
        Some(owner) if row.extra_panes > 0 => format!("{owner} +{}", row.extra_panes),
        Some(owner) => owner.to_string(),
        None if row.extra_panes > 0 => format!("— +{}", row.extra_panes),
        None => "—".to_string(),
    };
    format!(
        "{marker}{:<6}  {}  {}  {}  {:>2}",
        row.number,
        fit_cell(&row.title, title_width),
        fit_cell(&owner, owner_width),
        fit_cell(&row.ticket, ticket_width),
        row.review,
    )
}

fn fit_cell(value: &str, width: usize) -> String {
    let mut chars = value.chars();
    let mut cell = chars.by_ref().take(width).collect::<String>();
    if chars.next().is_some() && width > 0 {
        cell.pop();
        cell.push('…');
    }
    let len = cell.chars().count();
    cell.extend(std::iter::repeat_n(' ', width.saturating_sub(len)));
    cell
}

fn short_repo_name(repo: &str) -> &str {
    repo.rsplit('/').next().unwrap_or(repo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::AppState,
        work_index::{Snapshot, WorkItem, WorkItemSource},
    };
    use ratatui::{backend::TestBackend, Terminal};
    use std::time::SystemTime;

    fn pr(repo: &str, number: u64, tickets: &[&str]) -> WorkItem {
        WorkItem {
            repo: repo.to_string(),
            pr_number: Some(number),
            pr_url: Some(format!("https://github.com/{repo}/pull/{number}")),
            pr_title: Some(format!("PR {number}")),
            pr_state: Some("open".to_string()),
            draft: false,
            review_decision: Some("REVIEW_REQUIRED".to_string()),
            ticket_ids: tickets.iter().map(|ticket| (*ticket).to_string()).collect(),
            ticket_title: None,
            ticket_state: None,
            branch: None,
            preview_urls: Vec::new(),
            panes: Vec::new(),
            source: WorkItemSource::default(),
        }
    }

    fn review_pr(
        repo: &str,
        number: u64,
        title: &str,
        tickets: &[&str],
        ticket_state: Option<&str>,
    ) -> WorkItem {
        let mut item = pr(repo, number, tickets);
        item.pr_title = Some(title.to_string());
        item.ticket_state = ticket_state.map(str::to_string);
        item
    }

    fn snapshot(items: Vec<WorkItem>) -> Snapshot {
        Snapshot {
            items,
            unavailable: None,
            observed_at: SystemTime::UNIX_EPOCH,
        }
    }

    fn rendered_text(state: &WorkViewState) -> String {
        let app = AppState::test_new();
        let backend = TestBackend::new(100, 12);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render(&app.palette, state, frame.area(), frame))
            .expect("render work view");
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(100)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn disabled_not_collected_and_unavailable_render_safely() {
        assert!(rendered_text(&WorkViewState::new(false, None)).contains("work index disabled"));
        assert!(
            rendered_text(&WorkViewState::new(true, None)).contains("work index not yet collected")
        );
        assert!(rendered_text(&WorkViewState::new(
            true,
            Some(Snapshot {
                items: Vec::new(),
                unavailable: Some("GitHub observation failed".to_string()),
                observed_at: SystemTime::UNIX_EPOCH,
            }),
        ))
        .contains("GitHub observation failed"));

        let mut placeholder = WorkViewState::new(true, Some(snapshot(Vec::new())));
        placeholder.projection = WorkProjection::Tickets;
        assert!(rendered_text(&placeholder).contains("tickets not yet available"));
    }

    #[test]
    fn review_queue_degraded_states_render_safely() {
        let mut disabled = WorkViewState::new(false, None);
        disabled.projection = WorkProjection::ReviewQueue;
        assert!(rendered_text(&disabled).contains("work index disabled"));

        let mut not_collected = WorkViewState::new(true, None);
        not_collected.projection = WorkProjection::ReviewQueue;
        assert!(rendered_text(&not_collected).contains("work index not yet collected"));

        let mut unavailable = WorkViewState::new(
            true,
            Some(Snapshot {
                items: Vec::new(),
                unavailable: Some("unavailable".to_string()),
                observed_at: SystemTime::UNIX_EPOCH,
            }),
        );
        unavailable.projection = WorkProjection::ReviewQueue;
        assert!(rendered_text(&unavailable).contains("unavailable"));
    }

    #[test]
    fn review_queue_fixture_renders_all_verdicts_and_matching_counts() {
        let mut state = WorkViewState::new(
            true,
            Some(snapshot(vec![
                review_pr(
                    "scalablev2",
                    3226,
                    "ci(preview): allowlist",
                    &["SCA-2462"],
                    Some("In Progress"),
                ),
                review_pr(
                    "scalablev2",
                    3214,
                    "feat(image): restore prompt access",
                    &["SCA-2462", "SCA-2463", "SCA-2464"],
                    Some("In Progress"),
                ),
                review_pr(
                    "scalablev2",
                    3211,
                    "fix(onboarding): retry dispatch",
                    &[],
                    None,
                ),
                review_pr(
                    "scalablev2",
                    2531,
                    "fix(SCA-2462): renewal reconcile",
                    &["SCA-2462"],
                    Some("In Review"),
                ),
            ])),
        );
        state.projection = WorkProjection::ReviewQueue;
        state.repo_filter = Some("scalablev2".to_string());

        let text = rendered_text(&state);
        assert!(text.contains("work · review queue · scalablev2"));
        assert!(text.contains("awaiting review · 4"));
        assert!(text.contains("ticket says \"In Review\" · 1"));
        assert!(text.contains("3226"));
        assert!(text.contains("SCA-2462"));
        assert!(text.contains("3 tickets"));
        assert!(text.contains("In Progress"));
        assert!(text.contains("⚠ state drift"));
        assert!(text.contains("no ticket"));
        assert!(text.contains("⚠ untracked"));
        assert!(text.contains("In Review"));
        assert!(text.contains("✓"));
        assert!(text.contains("drift 3 PRs awaiting review whose ticket is not In Review"));
        assert!(text.contains("←/→ view PRs tickets agents [review queue]"));
        assert!(text.contains("↑/↓ move"));
        assert!(text.contains("f filter repo"));
    }

    #[test]
    fn fixture_matches_required_group_and_footer_contract() {
        let text = rendered_text(&WorkViewState::new(
            true,
            Some(snapshot(vec![
                pr("scalablev2", 3226, &["SCA-2462"]),
                pr("scalablev2", 3244, &[]),
            ])),
        ));
        assert!(text.contains("work · PRs · all repos · 2 open"));
        assert!(text.contains("scalablev2 (1)"));
        assert!(text.contains("3226"));
        assert!(text.contains("SCA-2462"));
        assert!(text.contains("no ticket (1)"));
        assert!(text.contains("3244"));
        assert!(text.contains("←/→ view [PRs] tickets agents"));
        assert!(text.contains("RR=review req"));
    }
}
