use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::{
    app::state::{AppState, Palette, PrDetailTab, WorkProjection, WorkViewState},
    ui::work_list_detail::{sorted_filtered_prs, WorkItem as _, WorkRow},
    work_projection::{project_review_queue, WorkReviewQueueRow},
};

pub(crate) fn render(app: &AppState, area: Rect, frame: &mut Frame) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(state) = app.work_view.as_ref() else {
        return;
    };
    let palette = &app.palette;
    let sections = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
    match state.projection {
        WorkProjection::PullRequests => render_pull_requests(app, state, sections[0], frame),
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

fn render_pull_requests(app: &AppState, state: &WorkViewState, area: Rect, frame: &mut Frame) {
    let palette = &app.palette;
    let observed_at = state
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.observed_at)
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    let items = state.snapshot.as_ref().map(|snapshot| {
        sorted_filtered_prs(
            &snapshot.items,
            &app.work_item_detail_cache,
            &state.search,
            state.sort,
            state.open_only,
            observed_at,
            &app.land_approval_label,
        )
    });
    let refresh = if state.refreshing || !app.work_item_detail_loading.is_empty() {
        " · refreshing…"
    } else {
        ""
    };
    let columns = if area.width >= 72 {
        Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)]).split(area)
    } else {
        Layout::vertical([Constraint::Percentage(48), Constraint::Percentage(52)]).split(area)
    };
    let left = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Pull requests{refresh} "))
        .border_style(Style::default().fg(palette.accent));
    let left_inner = left.inner(columns[0]);
    frame.render_widget(left, columns[0]);
    let filter = if state.open_only { "open" } else { "all" };
    let cursor = if state.search_focused { "▏" } else { "" };
    let mut lines = vec![Line::styled(
        format!(
            " 🔍 {}{cursor}   ⇅ {}   ⚲ {filter}",
            if state.search.is_empty() {
                "search or label:bug"
            } else {
                &state.search
            },
            state.sort.label()
        ),
        Style::default().fg(palette.subtext0),
    )];
    let message = if !state.enabled {
        Some("work index disabled".to_string())
    } else if state.snapshot.is_none() {
        Some("work index not yet collected".to_string())
    } else {
        state
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.unavailable.clone())
    };
    if let Some(message) = message {
        lines.push(Line::styled(message, Style::default().fg(palette.subtext0)));
        frame.render_widget(Paragraph::new(lines), left_inner);
        return;
    }
    let items = items.unwrap_or_default();
    let selected = state
        .selected
        .as_ref()
        .and_then(|key| {
            let selected_key = format!("{}#{}", key.repo, key.pr_number.unwrap_or_default());
            items.iter().position(|item| item.key() == selected_key)
        })
        .unwrap_or(0);
    let mut current_group = "";
    for (index, item) in items.iter().enumerate() {
        let row = item.row();
        if row.group != current_group {
            current_group = row.group;
            lines.push(Line::styled(
                format!(" {current_group}"),
                Style::default()
                    .fg(palette.subtext0)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        push_pr_row(
            &mut lines,
            &row,
            index == selected,
            palette,
            left_inner.width,
        );
    }
    if items.is_empty() {
        lines.push(Line::styled(
            " no matching pull requests",
            Style::default().fg(palette.subtext0),
        ));
    }
    frame.render_widget(Paragraph::new(lines), left_inner);

    let detail_block = Block::default()
        .borders(Borders::ALL)
        .title(" Pull request ")
        .border_style(Style::default().fg(palette.accent));
    let detail_inner = detail_block.inner(columns[1]);
    frame.render_widget(detail_block, columns[1]);
    if let Some(item) = items.get(selected) {
        render_pr_detail(app, state, item, detail_inner, frame);
    }
}

fn push_pr_row(
    lines: &mut Vec<Line<'static>>,
    row: &WorkRow,
    selected: bool,
    palette: &Palette,
    width: u16,
) {
    let style = if selected {
        Style::default()
            .fg(palette.text)
            .bg(palette.surface0)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.text)
    };
    let available = usize::from(width).saturating_sub(row.age.chars().count() + 6);
    lines.push(Line::styled(
        format!(
            " {} {}  {:>3}",
            row.glyph,
            fit_cell(&row.title, available),
            row.age
        ),
        style,
    ));
    lines.push(Line::styled(
        format!("   {}  {}", row.metadata, row.changes),
        Style::default().fg(palette.subtext0),
    ));
}

fn render_pr_detail(
    app: &AppState,
    state: &WorkViewState,
    item: &crate::ui::work_list_detail::PrItem<'_>,
    area: Rect,
    frame: &mut Frame,
) {
    let palette = &app.palette;
    let detail = item.detail();
    let land_enabled = item.actions().iter().any(|action| {
        action.kind == crate::ui::work_list_detail::WorkActionKind::Land && action.enabled
    });
    let land_status = item.land_status();
    let awaiting_approval =
        land_status == crate::ui::work_list_detail::PrLandStatus::AwaitingApproval;
    let (land_label, land_style) = match (land_enabled, &land_status) {
        (true, crate::ui::work_list_detail::PrLandStatus::Enabled(_)) => (
            "[Land]",
            Style::default()
                .fg(palette.text)
                .add_modifier(Modifier::BOLD),
        ),
        (false, crate::ui::work_list_detail::PrLandStatus::AwaitingApproval) => (
            "[Land]",
            Style::default()
                .fg(palette.overlay0)
                .add_modifier(Modifier::DIM),
        ),
        _ => (
            "[Land disabled]",
            Style::default()
                .fg(palette.overlay0)
                .add_modifier(Modifier::DIM),
        ),
    };
    let mut lines = vec![
        Line::from(vec![
            ratatui::text::Span::styled(
                format!(" {}     [Check out ▾] ", detail.heading),
                Style::default()
                    .fg(palette.text)
                    .add_modifier(Modifier::BOLD),
            ),
            ratatui::text::Span::styled(land_label, land_style),
        ]),
        Line::styled(
            format!(" {}", detail.title),
            Style::default()
                .fg(palette.text)
                .add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            format!(" {}", detail.byline),
            Style::default().fg(palette.subtext0),
        ),
        Line::styled(
            format!(" {}", detail.branches),
            Style::default().fg(palette.subtext0),
        ),
        Line::styled(
            format!(" [Summary] [Timeline] [Code]   {}", detail.checks_summary),
            Style::default().fg(palette.accent),
        ),
    ];
    if awaiting_approval {
        lines.insert(
            1,
            Line::styled(
                " awaiting approval",
                Style::default()
                    .fg(palette.overlay0)
                    .add_modifier(Modifier::DIM),
            ),
        );
    }
    match state.detail_tab {
        PrDetailTab::Summary => {
            lines.push(Line::styled(
                format!(" Reviewers  {}", detail.reviewers),
                Style::default().fg(palette.text),
            ));
            lines.push(Line::styled(
                " Description",
                Style::default()
                    .fg(palette.text)
                    .add_modifier(Modifier::BOLD),
            ));
            lines.extend(crate::ui::markdown::body_lines(
                palette,
                detail.description.as_deref(),
                usize::from(area.width.saturating_sub(2)),
                " ",
            ));
            lines.push(Line::styled(
                format!(" Checks  {}", detail.checks.len()),
                Style::default()
                    .fg(palette.text)
                    .add_modifier(Modifier::BOLD),
            ));
            for (name, status) in &detail.checks {
                let glyph = if status == "SUCCESS" {
                    "✓"
                } else if status == "FAILURE" {
                    "✗"
                } else {
                    "◌"
                };
                lines.push(Line::styled(
                    format!("  {glyph} {name}  {status}"),
                    Style::default().fg(palette.subtext0),
                ));
            }
            lines.push(Line::styled(
                format!(" Comments  {}  newest first", detail.comments.len()),
                Style::default()
                    .fg(palette.text)
                    .add_modifier(Modifier::BOLD),
            ));
            for comment in detail.comments.iter().take(3) {
                lines.push(Line::styled(
                    format!(
                        "  ✦ {}  [Fix in a thread]",
                        comment.author.as_deref().unwrap_or("unknown")
                    ),
                    Style::default().fg(palette.subtext0),
                ));
                lines.extend(crate::ui::markdown::body_lines(
                    palette,
                    Some(&comment.body),
                    usize::from(area.width.saturating_sub(4)),
                    "    ",
                ));
            }
        }
        PrDetailTab::Timeline => {
            lines.push(Line::styled(
                " Timeline · newest first",
                Style::default()
                    .fg(palette.text)
                    .add_modifier(Modifier::BOLD),
            ));
            for comment in &detail.comments {
                lines.push(Line::styled(
                    format!("  {}", comment.author.as_deref().unwrap_or("unknown")),
                    Style::default().fg(palette.subtext0),
                ));
                lines.extend(crate::ui::markdown::body_lines(
                    palette,
                    Some(&comment.body),
                    usize::from(area.width.saturating_sub(4)),
                    "    ",
                ));
            }
        }
        PrDetailTab::Code => crate::ui::dock::diff::render_diff(app, frame, area),
    }
    if let Some(confirm) = state.pending_land.as_ref() {
        lines.push(Line::styled(
            format!(
                " Confirm Land {}#{} via {} at {}? [y/N]",
                confirm.repo, confirm.number, confirm.approval_signal, confirm.head_sha
            ),
            Style::default()
                .fg(palette.yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if state.detail_tab != PrDetailTab::Code {
        frame.render_widget(Paragraph::new(lines), area);
    }
    if let Some(choice) = state.checkout_menu {
        if let Some(layout) = checkout_menu_layout(area, choice) {
            let options = [
                crate::app::state::PrCheckoutChoice::CurrentCheckout,
                crate::app::state::PrCheckoutChoice::NewWorktree,
            ];
            let menu = options
                .iter()
                .map(|option| {
                    Line::styled(
                        format!(
                            "{} {}",
                            if *option == choice { "▸" } else { " " },
                            match option {
                                crate::app::state::PrCheckoutChoice::CurrentCheckout =>
                                    "Current checkout",
                                crate::app::state::PrCheckoutChoice::NewWorktree => "New worktree",
                            }
                        ),
                        Style::default().fg(palette.text).bg(palette.panel_bg),
                    )
                })
                .collect::<Vec<_>>();
            frame.render_widget(Paragraph::new(menu), layout.rect);
        } else {
            frame.render_widget(
                Paragraph::new("checkout menu needs space below")
                    .style(Style::default().fg(palette.red)),
                Rect::new(area.x, area.y, area.width, 1),
            );
        }
    }
}

fn checkout_menu_layout(
    area: Rect,
    selected: crate::app::state::PrCheckoutChoice,
) -> Option<crate::ui::dropdown::DropdownLayout> {
    let anchor = Rect::new(area.x.saturating_add(1), area.y, 14.min(area.width), 1);
    crate::ui::dropdown::layout_dropdown(
        &crate::ui::dropdown::DropdownSpec {
            anchor,
            item_count: 2,
            selected: usize::from(selected == crate::app::state::PrCheckoutChoice::NewWorktree),
            has_filter: false,
            max_rows: 2,
            min_width: 18,
        },
        area,
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
            " / search   ↑/↓ move   s sort   f open/all   Tab Summary/Timeline/Code   c checkout   l Land   x fix"
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
            created_at: None,
            updated_at: None,
            additions: 0,
            deletions: 0,
            author: None,
            labels: Vec::new(),
            check_state: crate::work_index::PrCheckState::Unknown,
            audience: crate::work_index::PrAudience::Other,
            ticket_ids: tickets.iter().map(|ticket| (*ticket).to_string()).collect(),
            ticket_title: None,
            ticket_state: None,
            ticket_details: Vec::new(),
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
        let mut app = AppState::test_new();
        app.work_view = Some(state.clone());
        rendered_app_text(&app)
    }

    fn rendered_app_text(app: &AppState) -> String {
        rendered_app_text_at(app, 100, 12)
    }

    fn rendered_app_text_at(app: &AppState, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render(app, frame.area(), frame))
            .expect("render work view");
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn land_row_and_confirmation_name_the_approval_state() {
        let item = pr("owner/repo", 42, &[]);
        let key = crate::app::state::WorkItemKey {
            repo: item.repo.clone(),
            pr_number: item.pr_number,
            pr_url: item.pr_url.clone(),
            ticket_id: None,
        };
        let mut detail = crate::work_index::WorkItemDetail::empty();
        detail.number = Some(42);
        detail.actions = vec![crate::work_index::WorkItemAction {
            name: "test".into(),
            state: "SUCCESS".into(),
        }];
        detail.merge_state_status = Some("CLEAN".into());
        detail.head_sha = Some("abc123".into());
        let mut app = AppState::test_new();
        app.work_item_detail_cache.insert(key, detail);
        app.work_view = Some(WorkViewState::new(true, Some(snapshot(vec![item]))));

        let awaiting = rendered_app_text_at(&app, 120, 24);
        assert!(awaiting.contains("[Land]"));
        assert!(awaiting.contains("awaiting approval"));

        app.work_view.as_mut().expect("work view").pending_land =
            Some(crate::app::state::PrLandConfirmation {
                repo: "owner/repo".into(),
                number: 42,
                head_sha: "abc123".into(),
                approval_signal: "approved review".into(),
            });
        let confirmation = rendered_app_text_at(&app, 120, 24);
        assert!(confirmation.contains("via approved review at abc123"));
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
        assert!(text.contains("Pull requests"));
        assert!(text.contains("search or label:bug"));
        assert!(text.contains("Others"));
        assert!(text.contains("#3226"));
        assert!(text.contains("#3244"));
        assert!(text.contains("Tab Summary/Timeline/Code"));
    }

    #[test]
    fn checkout_dropdown_opens_downward_and_clamps() {
        let area = Rect::new(40, 3, 50, 4);
        let layout = checkout_menu_layout(area, crate::app::state::PrCheckoutChoice::NewWorktree)
            .expect("two rows fit below the action");
        assert_eq!(layout.rect.y, area.y + 1);
        assert_eq!(layout.visible_rows, 2);

        assert!(checkout_menu_layout(
            Rect::new(40, 6, 50, 1),
            crate::app::state::PrCheckoutChoice::CurrentCheckout,
        )
        .is_none());
    }
}
