use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::{
    app::state::{
        AppState, Palette, PrDetailTab, TicketTransitionChoice, WorkProjection, WorkViewState,
    },
    ui::work_list_detail::{
        comment_header, section_separator, sorted_filtered_conversations, sorted_filtered_prs,
        sorted_filtered_tickets, ConversationItem, TicketItem, WorkItem as _, WorkRow,
    },
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
        WorkProjection::Tickets => render_tickets(app, state, sections[0], frame),
        WorkProjection::Missive => render_missive(app, state, sections[0], frame),
        WorkProjection::ReviewQueue => render_review_queue(palette, state, sections[0], frame),
        projection => render_placeholder(palette, projection, sections[0], frame),
    }
    render_footer(palette, state, sections[1], frame);
}

fn render_missive(app: &AppState, state: &WorkViewState, area: Rect, frame: &mut Frame) {
    let palette = &app.palette;
    let observed_at = state
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.observed_at)
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    let items = state.snapshot.as_ref().map(|snapshot| {
        sorted_filtered_conversations(
            &snapshot.conversations,
            &state.search,
            !state.open_only,
            observed_at,
        )
    });
    let columns = if area.width >= 96 {
        Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)]).split(area)
    } else {
        Layout::vertical([Constraint::Percentage(48), Constraint::Percentage(52)]).split(area)
    };
    let refresh = if state.refreshing {
        " · refreshing…"
    } else {
        ""
    };
    let left = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Missive{refresh} "))
        .border_style(Style::default().fg(palette.accent));
    let left_inner = left.inner(columns[0]);
    frame.render_widget(left, columns[0]);
    let cursor = if state.search_focused { "▏" } else { "" };
    let query = if state.search.is_empty() {
        "search conversations"
    } else {
        &state.search
    };
    let filter = if state.open_only { "open" } else { "all" };
    let query_width = usize::from(left_inner.width).saturating_sub(filter.chars().count() + 9);
    let mut lines = vec![Line::styled(
        format!(" 🔍 {}{cursor}  ⚲ {filter}", fit_cell(query, query_width)),
        Style::default().fg(palette.subtext0),
    )];
    let blocking_message = if !state.enabled {
        Some("work index disabled".to_string())
    } else if state.snapshot.is_none() {
        Some("work index not yet collected".to_string())
    } else {
        None
    };
    if let Some(message) = blocking_message {
        lines.push(Line::styled(message, Style::default().fg(palette.subtext0)));
        frame.render_widget(Paragraph::new(lines), left_inner);
        return;
    }
    if let Some(reason) = state.snapshot.as_ref().and_then(|snapshot| {
        snapshot.unavailable_reason(crate::work_index::WorkIndexSource::Missive)
    }) {
        lines.push(Line::styled(
            format!("Missive: {reason}"),
            Style::default().fg(palette.subtext0),
        ));
    }
    let items = items.unwrap_or_default();
    let selected = state
        .selected_missive
        .as_deref()
        .and_then(|id| items.iter().position(|item| item.key() == id))
        .unwrap_or(0);
    let mut current_group = "";
    let mut selected_line = 0usize;
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
        if index == selected {
            selected_line = lines.len();
        }
        push_missive_row(
            &mut lines,
            item,
            &row,
            index == selected,
            palette,
            left_inner.width,
        );
    }
    if items.is_empty() {
        lines.push(Line::styled(
            " no matching conversations",
            Style::default().fg(palette.subtext0),
        ));
    }
    let list_height = usize::from(left_inner.height);
    let list_scroll = selected_line
        .saturating_add(1)
        .saturating_sub(list_height)
        .min(usize::from(u16::MAX)) as u16;
    frame.render_widget(Paragraph::new(lines).scroll((list_scroll, 0)), left_inner);

    let detail_block = Block::default()
        .borders(Borders::ALL)
        .title(" Conversation ")
        .border_style(Style::default().fg(palette.accent));
    let detail_inner = detail_block.inner(columns[1]);
    frame.render_widget(detail_block, columns[1]);
    if let Some(item) = items.get(selected) {
        render_missive_detail(app, state, item, detail_inner, frame);
    }
}

fn render_missive_detail(
    app: &AppState,
    state: &WorkViewState,
    item: &ConversationItem<'_>,
    area: Rect,
    frame: &mut Frame,
) {
    let detail = item.detail();
    let mut lines = vec![
        Line::styled(
            format!(" {}   [Start thread ▾] [Open in Missive]", detail.heading),
            Style::default()
                .fg(app.palette.text)
                .add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            format!(" {}", detail.title),
            Style::default()
                .fg(app.palette.text)
                .add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            format!(" {}", detail.byline),
            Style::default().fg(app.palette.subtext0),
        ),
    ];
    let visible_sections = detail
        .sections
        .iter()
        .filter(|section| !section.entries.is_empty())
        .collect::<Vec<_>>();
    for section in &visible_sections {
        lines.push(Line::styled(
            format!(" {}  {}", section.label, section.entries.len()),
            Style::default()
                .fg(app.palette.text)
                .add_modifier(Modifier::BOLD),
        ));
        for entry in &section.entries {
            lines.push(Line::styled(
                format!(
                    "  {} · {}",
                    entry.author.as_deref().unwrap_or("unknown"),
                    relative_time(entry.created_at, item.observed_at)
                ),
                Style::default().fg(app.palette.subtext0),
            ));
            lines.extend(crate::ui::markdown::body_lines(
                &app.palette,
                Some(&entry.body),
                usize::from(area.width.saturating_sub(4)),
                "    ",
            ));
        }
    }
    if visible_sections.is_empty() {
        lines.push(Line::styled(
            " no conversation entries indexed",
            Style::default().fg(app.palette.subtext0),
        ));
    }
    if let Some(hint) = state.hint.as_deref() {
        lines.push(Line::styled(
            format!(" {hint}"),
            Style::default().fg(app.palette.accent),
        ));
    }
    let max_scroll = lines.len().saturating_sub(usize::from(area.height));
    let scroll = usize::from(state.missive_detail_scroll).min(max_scroll) as u16;
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), area);

    if let Some(choice) = state.missive_start_menu {
        render_missive_start_menu(app, frame, area, choice);
    }
}

fn relative_time(then: Option<std::time::SystemTime>, now: std::time::SystemTime) -> String {
    let seconds = then
        .and_then(|then| now.duration_since(then).ok())
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    if seconds >= 86_400 {
        format!("{}d ago", seconds / 86_400)
    } else if seconds >= 3_600 {
        format!("{}h ago", seconds / 3_600)
    } else {
        format!("{}m ago", seconds / 60)
    }
}

fn render_missive_start_menu(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    choice: crate::app::state::PrCheckoutChoice,
) {
    let selected = usize::from(choice == crate::app::state::PrCheckoutChoice::NewWorktree);
    let Some(layout) = ticket_menu_layout(area, 12, 2, selected, 24) else {
        return;
    };
    let labels = ["Current checkout", "New worktree"];
    frame.render_widget(
        Paragraph::new(
            labels
                .iter()
                .enumerate()
                .map(|(index, label)| {
                    Line::styled(
                        format!("{} {label}", if index == selected { "▸" } else { " " }),
                        Style::default()
                            .fg(app.palette.text)
                            .bg(app.palette.panel_bg),
                    )
                })
                .collect::<Vec<_>>(),
        ),
        layout.rect,
    );
}

fn render_tickets(app: &AppState, state: &WorkViewState, area: Rect, frame: &mut Frame) {
    if state.ticket_layout == crate::app::state::LinearViewLayout::Board && !state.board_detail_open
    {
        render_ticket_board(app, state, area, frame);
        return;
    }
    let palette = &app.palette;
    let observed_at = state
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.observed_at)
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    let has_context_pr = super::dock::pr::focused_pr_key(app).is_some();
    let items = state.snapshot.as_ref().map(|snapshot| {
        sorted_filtered_tickets(
            &snapshot.items,
            &app.work_item_detail_cache,
            &state.search,
            state.ticket_sort,
            state.ticket_open_only,
            observed_at,
            has_context_pr,
            Some((&app.sidebar_work_filter, &app.work_index_session)),
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
    let layout_label = match state.ticket_layout {
        crate::app::state::LinearViewLayout::List => "[List] Board",
        crate::app::state::LinearViewLayout::Board => "List [Board]",
    };
    let left = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Tickets{refresh} · {layout_label} "))
        .border_style(Style::default().fg(palette.accent));
    let left_inner = left.inner(columns[0]);
    frame.render_widget(left, columns[0]);
    let filter = if state.ticket_open_only {
        "open"
    } else {
        "all"
    };
    let cursor = if state.search_focused { "▏" } else { "" };
    let mut lines = vec![Line::styled(
        format!(
            " 🔍 {}{cursor}   ⇅ {}   ⚲ {filter}",
            if state.search.is_empty() {
                "search or label:bug"
            } else {
                &state.search
            },
            state.ticket_sort.label()
        ),
        Style::default().fg(palette.subtext0),
    )];
    let blocking_message = if !state.enabled {
        Some("work index disabled".to_string())
    } else if state.snapshot.is_none() {
        Some("work index not yet collected".to_string())
    } else {
        None
    };
    if let Some(message) = blocking_message {
        lines.push(Line::styled(message, Style::default().fg(palette.subtext0)));
        frame.render_widget(Paragraph::new(lines), left_inner);
        return;
    }
    if let Some(reason) = state.snapshot.as_ref().and_then(|snapshot| {
        snapshot.unavailable_reason(crate::work_index::WorkIndexSource::Linear)
    }) {
        lines.push(Line::styled(
            format!("Linear: {reason}"),
            Style::default().fg(palette.subtext0),
        ));
    }
    let items = items.unwrap_or_default();
    let selected = state
        .selected
        .as_ref()
        .and_then(|key| {
            key.ticket_id
                .as_deref()
                .and_then(|ticket| items.iter().position(|item| item.key() == ticket))
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
        push_work_row(
            &mut lines,
            &row,
            index == selected,
            palette,
            left_inner.width,
            false,
        );
    }
    if items.is_empty() {
        lines.push(Line::styled(
            " no matching tickets",
            Style::default().fg(palette.subtext0),
        ));
    }
    frame.render_widget(Paragraph::new(lines), left_inner);

    let detail_block = Block::default()
        .borders(Borders::ALL)
        .title(" Ticket ")
        .border_style(Style::default().fg(palette.accent));
    let detail_inner = detail_block.inner(columns[1]);
    frame.render_widget(detail_block, columns[1]);
    if let Some(item) = items.get(selected) {
        render_ticket_detail(app, state, item, detail_inner, frame);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinearBoardColumn {
    Triage,
    Todo,
    InProgress,
    InReview,
    Done,
}

impl LinearBoardColumn {
    pub(crate) const ALL: [Self; 5] = [
        Self::Triage,
        Self::Todo,
        Self::InProgress,
        Self::InReview,
        Self::Done,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Triage => "Triage",
            Self::Todo => "Backlog / Todo",
            Self::InProgress => "In Progress",
            Self::InReview => "In Review",
            Self::Done => "Done",
        }
    }

    pub(crate) fn for_ticket(ticket: &crate::work_index::WorkTicket) -> Self {
        let state = ticket.state.as_deref().unwrap_or_default().to_lowercase();
        if ticket.group == crate::work_index::TicketGroup::Triage || state.contains("triage") {
            Self::Triage
        } else if state.contains("done") || state.contains("complete") {
            Self::Done
        } else if state.contains("review") {
            Self::InReview
        } else if state.contains("progress") || state.contains("started") {
            Self::InProgress
        } else {
            Self::Todo
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoardCardHitArea {
    pub(crate) key: crate::app::state::WorkItemKey,
    pub(crate) column: usize,
    pub(crate) row: usize,
    pub(crate) rect: Rect,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TicketBoardLayout {
    pub(crate) list_toggle: Rect,
    pub(crate) board_toggle: Rect,
    pub(crate) visible_columns: std::ops::Range<usize>,
    pub(crate) cards: Vec<BoardCardHitArea>,
}

pub(crate) fn ticket_board_page_size(width: u16) -> usize {
    if width < 100 {
        2
    } else {
        5
    }
}

pub(crate) fn ticket_board_columns(
    app: &AppState,
    state: &WorkViewState,
) -> [Vec<crate::app::state::WorkItemKey>; 5] {
    let mut columns: [Vec<crate::app::state::WorkItemKey>; 5] = std::array::from_fn(|_| Vec::new());
    let observed_at = state
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.observed_at)
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    let has_context_pr = super::dock::pr::focused_pr_key(app).is_some();
    let items = state
        .snapshot
        .as_ref()
        .map(|snapshot| {
            sorted_filtered_tickets(
                &snapshot.items,
                &app.work_item_detail_cache,
                &state.search,
                state.ticket_sort,
                state.ticket_open_only,
                observed_at,
                has_context_pr,
                Some((&app.sidebar_work_filter, &app.work_index_session)),
            )
        })
        .unwrap_or_default();
    for item in items {
        let column = LinearBoardColumn::for_ticket(item.summary) as usize;
        columns[column].push(crate::app::state::WorkItemKey {
            repo: String::new(),
            pr_number: None,
            pr_url: None,
            ticket_id: Some(item.summary.identifier.clone()),
        });
    }
    columns
}

pub(crate) fn ticket_board_layout(
    app: &AppState,
    state: &WorkViewState,
    area: Rect,
) -> TicketBoardLayout {
    if area.width == 0 || area.height < 2 {
        return TicketBoardLayout::default();
    }
    let viewport_width = [
        area.right(),
        app.view.sidebar_rect.right(),
        app.view.dock_rect.right(),
    ]
    .into_iter()
    .max()
    .unwrap_or(area.right());
    let page_size = ticket_board_page_size(viewport_width.max(area.width));
    let page_start = ((state.board_column / page_size) * page_size)
        .min(LinearBoardColumn::ALL.len().saturating_sub(page_size));
    let page_end = (page_start + page_size).min(LinearBoardColumn::ALL.len());
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
    let constraints = (page_start..page_end)
        .map(|_| Constraint::Ratio(1, page_size as u32))
        .collect::<Vec<_>>();
    let column_areas = Layout::horizontal(constraints).split(body);
    let columns = ticket_board_columns(app, state);
    let mut cards = Vec::new();
    for (visible_index, column_index) in (page_start..page_end).enumerate() {
        let inner = Block::default()
            .borders(Borders::ALL)
            .inner(column_areas[visible_index]);
        let capacity = usize::from(inner.height) / 4;
        let first = state.board_scroll[column_index]
            .min(columns[column_index].len().saturating_sub(capacity.max(1)));
        for (visible_row, row) in (first..columns[column_index].len())
            .take(capacity)
            .enumerate()
        {
            cards.push(BoardCardHitArea {
                key: columns[column_index][row].clone(),
                column: column_index,
                row,
                rect: Rect::new(inner.x, inner.y + visible_row as u16 * 4, inner.width, 4),
            });
        }
    }
    TicketBoardLayout {
        list_toggle: Rect::new(area.x + 10.min(area.width), area.y, 6.min(area.width), 1),
        board_toggle: Rect::new(area.x + 17.min(area.width), area.y, 7.min(area.width), 1),
        visible_columns: page_start..page_end,
        cards,
    }
}

fn render_ticket_board(app: &AppState, state: &WorkViewState, area: Rect, frame: &mut Frame) {
    let refresh = if state.refreshing || !app.work_item_detail_loading.is_empty() {
        " · refreshing…"
    } else {
        ""
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            ratatui::text::Span::styled(
                "Tickets ",
                Style::default()
                    .fg(app.palette.text)
                    .add_modifier(Modifier::BOLD),
            ),
            ratatui::text::Span::styled("List", Style::default().fg(app.palette.subtext0)),
            ratatui::text::Span::raw(" | "),
            ratatui::text::Span::styled(
                "Board",
                Style::default()
                    .fg(app.palette.accent)
                    .bg(app.palette.surface0)
                    .add_modifier(Modifier::BOLD),
            ),
            ratatui::text::Span::styled(refresh, Style::default().fg(app.palette.subtext0)),
        ])),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let layout = ticket_board_layout(app, state, area);
    let columns = ticket_board_columns(app, state);
    let body = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    );
    let constraints = layout
        .visible_columns
        .clone()
        .map(|_| Constraint::Ratio(1, layout.visible_columns.len() as u32))
        .collect::<Vec<_>>();
    let column_areas = Layout::horizontal(constraints).split(body);
    for (visible_index, column_index) in layout.visible_columns.clone().enumerate() {
        let column = LinearBoardColumn::ALL[column_index];
        let selected_column = column_index == state.board_column;
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(
                " {} · {} ",
                column.label(),
                columns[column_index].len()
            ))
            .border_style(Style::default().fg(if selected_column {
                app.palette.accent
            } else {
                app.palette.surface_dim
            }));
        let inner = block.inner(column_areas[visible_index]);
        frame.render_widget(block, column_areas[visible_index]);
        if columns[column_index].is_empty() {
            frame.render_widget(
                Paragraph::new(Line::styled(
                    " —",
                    Style::default().fg(app.palette.overlay0),
                )),
                inner,
            );
            continue;
        }
        let first = state.board_scroll[column_index];
        let capacity = usize::from(inner.height) / 4;
        for (visible_row, key) in columns[column_index]
            .iter()
            .skip(first)
            .take(capacity)
            .enumerate()
        {
            let Some(ticket_id) = key.ticket_id.as_deref() else {
                continue;
            };
            let Some(ticket) = state.snapshot.as_ref().and_then(|snapshot| {
                snapshot
                    .items
                    .iter()
                    .flat_map(|item| &item.ticket_details)
                    .find(|ticket| ticket.identifier.eq_ignore_ascii_case(ticket_id))
            }) else {
                continue;
            };
            let selected = selected_column && state.board_rows[column_index] == first + visible_row;
            let style = if selected {
                Style::default()
                    .fg(app.palette.text)
                    .bg(app.palette.surface0)
            } else {
                Style::default().fg(app.palette.text)
            };
            let glyph = match LinearBoardColumn::for_ticket(ticket) {
                LinearBoardColumn::Triage => "●",
                LinearBoardColumn::Todo => "○",
                LinearBoardColumn::InProgress | LinearBoardColumn::InReview => "◐",
                LinearBoardColumn::Done => "✓",
            };
            let priority = ticket
                .priority
                .map_or("P—".into(), |value| format!("P{value}"));
            let width = usize::from(inner.width.saturating_sub(2)).max(1);
            let title = wrap_board_title(ticket.title.as_deref().unwrap_or("(untitled)"), width);
            let initials = assignee_initials(ticket.assignee.as_deref());
            let marker = if selected { "▸" } else { " " };
            let lines = vec![
                Line::styled(
                    format!("{marker}{glyph} {} {priority}", ticket.identifier),
                    style,
                ),
                Line::styled(format!(" {}", title[0]), style),
                Line::styled(format!(" {}", title[1]), style),
                Line::styled(
                    format!(" {initials}"),
                    Style::default().fg(app.palette.subtext0).bg(if selected {
                        app.palette.surface0
                    } else {
                        app.palette.panel_bg
                    }),
                ),
            ];
            frame.render_widget(
                Paragraph::new(lines),
                Rect::new(inner.x, inner.y + visible_row as u16 * 4, inner.width, 4),
            );
        }
    }
}

fn wrap_board_title(title: &str, width: usize) -> [String; 2] {
    let mut lines = [String::new(), String::new()];
    let mut line = 0;
    for word in title.split_whitespace() {
        let needed = word.chars().count() + usize::from(!lines[line].is_empty());
        if lines[line].chars().count() + needed > width && line == 0 {
            line = 1;
        }
        if lines[line].chars().count() + needed > width {
            break;
        }
        if !lines[line].is_empty() {
            lines[line].push(' ');
        }
        lines[line].push_str(word);
    }
    lines
}

fn assignee_initials(assignee: Option<&str>) -> String {
    let initials = assignee
        .unwrap_or("unassigned")
        .split(|character: char| character.is_whitespace() || character == '.' || character == '-')
        .filter_map(|part| part.chars().next())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect::<String>();
    if initials.is_empty() {
        "—".into()
    } else {
        initials
    }
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

    let blocking_message = if !state.enabled {
        Some("work index disabled".to_string())
    } else if state.snapshot.is_none() {
        Some("work index not yet collected".to_string())
    } else {
        None
    };
    if let Some(message) = blocking_message {
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(palette.subtext0)),
            inner,
        );
        return;
    }

    let Some(projection) = projection else {
        return;
    };
    let mut lines = Vec::new();
    if let Some(reason) = state.snapshot.as_ref().and_then(|snapshot| {
        snapshot.unavailable_reason(crate::work_index::WorkIndexSource::Github)
    }) {
        lines.push(Line::styled(
            format!("GitHub: {reason}"),
            Style::default().fg(palette.subtext0),
        ));
    }
    lines.push(Line::styled(
        format_review_queue_summary(
            projection.awaiting_review_count,
            projection.ticket_in_review_count,
            inner.width,
        ),
        Style::default()
            .fg(palette.subtext0)
            .add_modifier(Modifier::BOLD),
    ));
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
            Some((&app.sidebar_work_filter, &app.work_index_session)),
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
    let blocking_message = if !state.enabled {
        Some("work index disabled".to_string())
    } else if state.snapshot.is_none() {
        Some("work index not yet collected".to_string())
    } else {
        None
    };
    if let Some(message) = blocking_message {
        lines.push(Line::styled(message, Style::default().fg(palette.subtext0)));
        frame.render_widget(Paragraph::new(lines), left_inner);
        return;
    }
    if let Some(reason) = state.snapshot.as_ref().and_then(|snapshot| {
        snapshot.unavailable_reason(crate::work_index::WorkIndexSource::Github)
    }) {
        lines.push(Line::styled(
            format!("GitHub: {reason}"),
            Style::default().fg(palette.subtext0),
        ));
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
        push_work_row(
            &mut lines,
            &row,
            index == selected,
            palette,
            left_inner.width,
            true,
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

fn push_work_row(
    lines: &mut Vec<Line<'static>>,
    row: &WorkRow,
    selected: bool,
    palette: &Palette,
    width: u16,
    metadata_on_second_line: bool,
) {
    let style = if selected {
        Style::default()
            .fg(palette.text)
            .bg(palette.surface0)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.text)
    };
    let suffix = if metadata_on_second_line {
        row.age.clone()
    } else {
        format!("{}  {}", row.changes, row.age)
    };
    let prefix = if metadata_on_second_line {
        String::new()
    } else {
        format!("{} ", row.metadata)
    };
    let available =
        usize::from(width).saturating_sub(prefix.chars().count() + suffix.chars().count() + 5);
    lines.push(Line::styled(
        format!(
            " {} {}{}  {}",
            row.glyph,
            prefix,
            fit_cell(&row.title, available),
            suffix
        ),
        style,
    ));
    if metadata_on_second_line {
        lines.push(Line::styled(
            format!("   {}  {}", row.metadata, row.changes),
            Style::default().fg(palette.subtext0),
        ));
    }
}

fn push_missive_row(
    lines: &mut Vec<Line<'static>>,
    item: &ConversationItem<'_>,
    row: &WorkRow,
    selected: bool,
    palette: &Palette,
    width: u16,
) {
    let base = if selected {
        Style::default()
            .fg(palette.text)
            .bg(palette.surface0)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.text)
    };
    let status = crate::ui::work_status::WorkGroupStatus::from_conversation(
        item.summary.closed,
        !item.summary.assignees.is_empty(),
    );
    let available = usize::from(width).saturating_sub(row.age.chars().count() + 5);
    lines.push(Line::from(vec![
        ratatui::text::Span::styled(" ", base),
        ratatui::text::Span::styled(status.glyph(), base.fg(status.color(palette))),
        ratatui::text::Span::styled(
            format!(" {}  {}", fit_cell(&row.title, available), row.age),
            base,
        ),
    ]));
    lines.push(Line::styled(
        format!("   {}", row.metadata),
        if selected {
            Style::default().fg(palette.subtext0).bg(palette.surface0)
        } else {
            Style::default().fg(palette.subtext0)
        },
    ));
}

fn render_ticket_detail(
    app: &AppState,
    state: &WorkViewState,
    item: &TicketItem<'_>,
    area: Rect,
    frame: &mut Frame,
) {
    let palette = &app.palette;
    let detail = item.detail();
    let mut lines = vec![
        Line::from(vec![ratatui::text::Span::styled(
            format!(" {}   [Start thread ▾] [⋯]", detail.heading),
            Style::default()
                .fg(palette.text)
                .add_modifier(Modifier::BOLD),
        )]),
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
    ];
    lines.extend(section_separator(
        palette,
        format!("Linked PRs  {}", detail.linked_prs.len()),
        area.width,
    ));
    for pr in &detail.linked_prs {
        let check = match pr.check_state {
            crate::work_index::PrCheckState::Passing => "✓",
            crate::work_index::PrCheckState::Failing => "✗",
            crate::work_index::PrCheckState::Pending => "◌",
            crate::work_index::PrCheckState::Unknown => "—",
        };
        lines.push(Line::styled(
            format!("  ⑂ #{} {}  {check}", pr.number, pr.title),
            Style::default().fg(palette.subtext0),
        ));
    }
    lines.extend(section_separator(palette, "Description", area.width));
    lines.extend(crate::ui::markdown::body_lines(
        palette,
        crate::ui::work_list_detail::description_without_checklist(detail.description.as_deref())
            .as_deref(),
        usize::from(area.width.saturating_sub(2)),
        " ",
    ));
    if !detail.checks.is_empty() {
        lines.extend(section_separator(
            palette,
            "Acceptance criteria",
            area.width,
        ));
        for (text, state) in &detail.checks {
            lines.push(Line::styled(
                format!("  {} {text}", if state == "done" { "✓" } else { "✗" }),
                Style::default().fg(palette.subtext0),
            ));
        }
    }
    lines.extend(section_separator(
        palette,
        format!("Comments  {}  newest first", detail.comments.len()),
        area.width,
    ));
    for (index, comment) in detail.comments.iter().enumerate() {
        if index > 0 {
            lines.push(Line::default());
        }
        lines.push(Line::styled(
            format!("  {}", comment_header(comment, item.observed_at)),
            Style::default()
                .fg(palette.subtext0)
                .add_modifier(Modifier::DIM),
        ));
        lines.extend(crate::ui::markdown::body_lines(
            palette,
            Some(&comment.body),
            usize::from(area.width.saturating_sub(4)),
            "    ",
        ));
    }
    if let Some(draft) = state.ticket_comment_draft.as_deref() {
        lines.push(Line::styled(
            format!(" Comment: {draft}▏  Enter to stage · Esc cancel"),
            Style::default().fg(palette.yellow),
        ));
    }
    if let Some(write) = state.pending_write.as_ref() {
        lines.push(Line::styled(
            format!(" Confirm {}? [y/N]", write.describe()),
            Style::default()
                .fg(palette.yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    frame.render_widget(Paragraph::new(lines), area);

    if let Some(choice) = state.ticket_start_menu {
        render_ticket_start_menu(app, frame, area, item, choice);
    } else if let Some(choice) = state.ticket_transition_menu {
        render_ticket_transition_menu(app, frame, area, item, choice);
    } else if let Some(menu) = state.ticket_more_menu {
        let context = crate::ui::ticket_actions::TicketActionContext::from_ticket(
            item.summary,
            item.cached_detail,
            app.work_index_session.linear.viewer.as_deref(),
            app.work_index_session.linear_viewer_identity(),
            item.has_context_pr,
        );
        crate::ui::ticket_actions::render_ticket_action_menu(
            palette,
            frame,
            area,
            ticket_action_menu_anchor(area),
            &context,
            menu,
        );
    }
}

fn ticket_action_menu_anchor(area: Rect) -> Rect {
    Rect::new(area.right().saturating_sub(3), area.y, 3.min(area.width), 1)
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
            lines.extend(section_separator(palette, "Description", area.width));
            lines.extend(crate::ui::markdown::body_lines(
                palette,
                detail.description.as_deref(),
                usize::from(area.width.saturating_sub(2)),
                " ",
            ));
            lines.extend(section_separator(
                palette,
                format!("Checks  {}", detail.checks.len()),
                area.width,
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
            lines.extend(section_separator(
                palette,
                format!("Comments  {}  newest first", detail.comments.len()),
                area.width,
            ));
            for (index, comment) in detail.comments.iter().take(3).enumerate() {
                if index > 0 {
                    lines.push(Line::default());
                }
                lines.push(Line::styled(
                    format!(
                        "  ✦ {}  [Fix in a thread]",
                        comment_header(comment, item.observed_at)
                    ),
                    Style::default()
                        .fg(palette.subtext0)
                        .add_modifier(Modifier::DIM),
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
            lines.extend(section_separator(
                palette,
                "Timeline · newest first",
                area.width,
            ));
            for (index, comment) in detail.comments.iter().enumerate() {
                if index > 0 {
                    lines.push(Line::default());
                }
                lines.push(Line::styled(
                    format!("  {}", comment_header(comment, item.observed_at)),
                    Style::default()
                        .fg(palette.subtext0)
                        .add_modifier(Modifier::DIM),
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

fn ticket_menu_layout(
    area: Rect,
    anchor_x: u16,
    item_count: usize,
    selected: usize,
    width: u16,
) -> Option<crate::ui::dropdown::DropdownLayout> {
    crate::ui::dropdown::layout_dropdown(
        &crate::ui::dropdown::DropdownSpec {
            anchor: Rect::new(
                area.x.saturating_add(anchor_x).min(area.right()),
                area.y,
                width.min(area.width),
                1,
            ),
            item_count,
            selected,
            has_filter: false,
            max_rows: item_count,
            min_width: width,
        },
        area,
    )
}

fn render_ticket_start_menu(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    item: &TicketItem<'_>,
    choice: crate::app::state::PrCheckoutChoice,
) {
    let selected = usize::from(choice == crate::app::state::PrCheckoutChoice::NewWorktree);
    let Some(layout) = ticket_menu_layout(area, 12, 2, selected, 42) else {
        return;
    };
    let branch = crate::ui::work_list_detail::ticket_worktree_branch(
        &app.branch_prefix,
        &item.summary.identifier,
        item.summary.title.as_deref().unwrap_or_default(),
    );
    let options = [
        (
            crate::app::state::PrCheckoutChoice::CurrentCheckout,
            "Current checkout".to_string(),
        ),
        (
            crate::app::state::PrCheckoutChoice::NewWorktree,
            format!("New worktree {branch}"),
        ),
    ];
    frame.render_widget(
        Paragraph::new(
            options
                .into_iter()
                .map(|(option, label)| {
                    Line::styled(
                        format!("{} {label}", if option == choice { "▸" } else { " " }),
                        Style::default()
                            .fg(app.palette.text)
                            .bg(app.palette.panel_bg),
                    )
                })
                .collect::<Vec<_>>(),
        ),
        layout.rect,
    );
}

fn render_ticket_transition_menu(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    item: &TicketItem<'_>,
    choice: TicketTransitionChoice,
) {
    let selected = TicketTransitionChoice::ALL
        .iter()
        .position(|option| *option == choice)
        .unwrap_or(0);
    let Some(layout) = ticket_menu_layout(area, 29, 4, selected, 20) else {
        return;
    };
    frame.render_widget(
        Paragraph::new(
            TicketTransitionChoice::ALL
                .into_iter()
                .map(|option| {
                    let enabled = item.transition_enabled(option.label());
                    Line::styled(
                        format!(
                            "{} {}{}",
                            if option == choice { "▸" } else { " " },
                            option.label(),
                            if enabled { "" } else { " · current" }
                        ),
                        Style::default()
                            .fg(if enabled {
                                app.palette.text
                            } else {
                                app.palette.overlay0
                            })
                            .bg(app.palette.panel_bg),
                    )
                })
                .collect::<Vec<_>>(),
        ),
        layout.rect,
    );
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
        WorkProjection::Tickets
            if state.ticket_layout == crate::app::state::LinearViewLayout::Board
                && !state.board_detail_open =>
        {
            " ←/→ columns   ↑/↓ cards   Enter detail   t transition   v list"
        }
        WorkProjection::Tickets => {
            " / search   ↑/↓ move   s sort   f open/all   c start   t transition   l link PR   m more   v board"
        }
        WorkProjection::Missive => {
            let wide = " / search   ↑/↓ move   PgUp/PgDn detail   f open/all   c start thread   o Open in Missive   r refresh";
            if crate::ui::text::display_width(wide) <= usize::from(area.width) {
                wide
            } else {
                " / search  ↑/↓ conversation  PgUp/PgDn detail  c start  o copy  r refresh"
            }
        }
        WorkProjection::Agents => " ←/→ view PRs tickets Missive [agents]   not yet available",
        WorkProjection::ReviewQueue => {
            " ←/→ view PRs tickets Missive agents [review queue]   ↑/↓ move   f filter repo"
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
            assignees: Vec::new(),
            labels: Vec::new(),
            check_state: crate::work_index::PrCheckState::Unknown,
            audience: crate::work_index::PrAudience::Other,
            cached_pr_detail: None,
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
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: SystemTime::UNIX_EPOCH,
        }
    }

    fn missive_conversation(message_count: usize) -> crate::work_index::MissiveConversation {
        crate::work_index::MissiveConversation {
            id: "sample".into(),
            subject: "Billing question that stays readable".into(),
            app_url: "missive://mail.missiveapp.com/#inbox/conversations/sample".into(),
            web_url: "https://mail.missiveapp.com/#inbox/conversations/sample".into(),
            assignees: vec![crate::work_index::MissiveUser {
                id: "ada".into(),
                name: "Ada Lovelace".into(),
                email: None,
                is_me: true,
            }],
            last_activity_at: Some(SystemTime::UNIX_EPOCH),
            closed: false,
            labels: Vec::new(),
            pane_bound: false,
            messages: (0..message_count)
                .map(|index| crate::work_index::MissiveEntry {
                    id: format!("message-{index}"),
                    author: Some("Customer".into()),
                    preview: format!("message body {index}"),
                    created_at: Some(SystemTime::UNIX_EPOCH),
                })
                .collect(),
            notes: Vec::new(),
            drafts: Vec::new(),
            posts: Vec::new(),
        }
    }

    fn missive_state(message_count: usize) -> WorkViewState {
        let mut state = WorkViewState::new(
            true,
            Some(Snapshot {
                items: Vec::new(),
                conversations: vec![missive_conversation(message_count)],
                missive_users: Vec::new(),
                unavailable: None,
                observed_at: SystemTime::UNIX_EPOCH,
            }),
        );
        state.projection = WorkProjection::Missive;
        state
    }

    fn ticket(identifier: &str, group: crate::work_index::TicketGroup) -> WorkItem {
        WorkItem {
            repo: "owner/repo".into(),
            pr_number: None,
            pr_url: None,
            pr_title: None,
            pr_state: None,
            draft: false,
            review_decision: None,
            created_at: None,
            updated_at: None,
            additions: 0,
            deletions: 0,
            author: None,
            assignees: Vec::new(),
            labels: Vec::new(),
            check_state: crate::work_index::PrCheckState::Unknown,
            audience: crate::work_index::PrAudience::Unclassified,
            cached_pr_detail: None,
            ticket_ids: vec![identifier.into()],
            ticket_title: Some("ticket title".into()),
            ticket_state: Some("In Progress".into()),
            ticket_details: vec![crate::work_index::WorkTicket {
                identifier: identifier.into(),
                title: Some("ticket title".into()),
                description: Some(
                    "Ticket body with https://example.invalid/a/very/long/path/that/must/wrap.\n- [x] done\n- [ ] left"
                        .into(),
                ),
                state: Some("In Progress".into()),
                assignee: Some("matthias".into()),
                priority: Some(2),
                cycle: Some("cycle 34".into()),
                group,
                created_at: None,
                updated_at: None,
                branch: None,
                labels: vec!["bug".into()],
                url: Some(format!("https://linear.app/acme/issue/{identifier}")),
                parent: None,
                relations: Vec::new(),
            }],
            branch: None,
            preview_urls: Vec::new(),
            panes: Vec::new(),
            source: WorkItemSource::default(),
        }
    }

    fn board_state() -> WorkViewState {
        let cases = [
            ("SCA-1", "Triage", crate::work_index::TicketGroup::Triage),
            ("SCA-2", "Todo", crate::work_index::TicketGroup::Assigned),
            (
                "SCA-3",
                "In Progress",
                crate::work_index::TicketGroup::Assigned,
            ),
            (
                "SCA-4",
                "In Review",
                crate::work_index::TicketGroup::Assigned,
            ),
            (
                "SCA-5",
                "Done",
                crate::work_index::TicketGroup::DoneThisCycle,
            ),
        ];
        let items = cases
            .into_iter()
            .map(|(identifier, state, group)| {
                let mut item = ticket(identifier, group);
                item.ticket_state = Some(state.into());
                item.ticket_details[0].state = Some(state.into());
                item.ticket_details[0].title = Some(format!("{state} ticket with a wrapped title"));
                item
            })
            .collect();
        let mut state = WorkViewState::new(true, Some(snapshot(items)));
        state.projection = WorkProjection::Tickets;
        state.ticket_layout = crate::app::state::LinearViewLayout::Board;
        state
    }

    #[test]
    fn board_maps_injected_workflow_states_to_team_columns() {
        let app = AppState::test_new();
        let state = board_state();
        let columns = ticket_board_columns(&app, &state);
        let identifiers = columns.map(|column| {
            column
                .into_iter()
                .filter_map(|key| key.ticket_id)
                .collect::<Vec<_>>()
        });
        assert_eq!(
            identifiers,
            [
                vec![String::from("SCA-1")],
                vec![String::from("SCA-2")],
                vec![String::from("SCA-3")],
                vec![String::from("SCA-4")],
                vec![String::from("SCA-5")],
            ]
        );
    }

    #[test]
    fn board_pages_two_columns_at_eighty_and_renders_all_at_one_twenty() {
        let app = AppState::test_new();
        let mut state = board_state();
        let narrow = ticket_board_layout(&app, &state, Rect::new(26, 2, 53, 20));
        assert_eq!(narrow.visible_columns, 0..2);
        state.board_column = 4;
        let last_page = ticket_board_layout(&app, &state, Rect::new(26, 2, 53, 20));
        assert_eq!(last_page.visible_columns, 3..5);
        let wide = ticket_board_layout(&app, &state, Rect::new(26, 2, 93, 36));
        assert_eq!(wide.visible_columns, 0..5);

        state.board_column = 0;
        let text = rendered_text_at(&state, 120, 40);
        assert!(text.contains("List | Board"), "{text}");
        assert!(text.contains("In Progress"), "{text}");
        assert!(text.contains("SCA-5"), "{text}");

        state.refreshing = true;
        let narrow_text = rendered_text_at(&state, 80, 24);
        assert!(
            narrow_text.contains("Tickets List | Board"),
            "{narrow_text}"
        );
    }

    #[test]
    fn successful_transition_reprojects_the_card_into_its_new_column() {
        let app = AppState::test_new();
        let mut state = board_state();
        let before = ticket_board_columns(&app, &state);
        assert!(before[2]
            .iter()
            .any(|key| key.ticket_id.as_deref() == Some("SCA-3")));
        let ticket = state
            .snapshot
            .as_mut()
            .and_then(|snapshot| snapshot.items.get_mut(2))
            .and_then(|item| item.ticket_details.first_mut())
            .expect("transitioned ticket");
        ticket.state = Some("Done".into());
        let after = ticket_board_columns(&app, &state);
        assert!(!after[2]
            .iter()
            .any(|key| key.ticket_id.as_deref() == Some("SCA-3")));
        assert!(after[4]
            .iter()
            .any(|key| key.ticket_id.as_deref() == Some("SCA-3")));
    }

    fn rendered_text(state: &WorkViewState) -> String {
        let mut app = AppState::test_new();
        app.work_view = Some(state.clone());
        rendered_app_text(&app)
    }

    fn rendered_text_at(state: &WorkViewState, width: u16, height: u16) -> String {
        let mut app = AppState::test_new();
        app.work_view = Some(state.clone());
        rendered_app_text_at(&app, width, height)
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
                conversations: Vec::new(),
                missive_users: Vec::new(),
                unavailable: Some(crate::work_index::WorkIndexUnavailable::only(
                    crate::work_index::WorkIndexSource::Github,
                    "observation failed",
                )),
                observed_at: SystemTime::UNIX_EPOCH,
            }),
        ))
        .contains("GitHub: observation failed"));

        let mut placeholder = WorkViewState::new(true, Some(snapshot(Vec::new())));
        placeholder.projection = WorkProjection::Tickets;
        assert!(rendered_text(&placeholder).contains("no matching tickets"));
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
                conversations: Vec::new(),
                missive_users: Vec::new(),
                unavailable: Some(crate::work_index::WorkIndexUnavailable::only(
                    crate::work_index::WorkIndexSource::Github,
                    "unavailable",
                )),
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
        assert!(text.contains("←/→ view PRs tickets Missive agents [review queue]"));
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

    #[test]
    fn ticket_fixture_renders_list_detail_and_refresh_state() {
        let mut state = WorkViewState::new(
            true,
            Some(snapshot(vec![
                ticket("SCA-3165", crate::work_index::TicketGroup::Assigned),
                ticket("SCA-3180", crate::work_index::TicketGroup::Triage),
            ])),
        );
        state.projection = WorkProjection::Tickets;
        state.refreshing = true;
        let text = rendered_text_at(&state, 100, 24);
        assert!(text.contains("Tickets · refreshing…"), "{text}");
        assert!(text.contains("Assigned to me"), "{text}");
        assert!(text.contains("Triage"), "{text}");
        assert!(text.contains("SCA-3165"), "{text}");
        assert!(
            text.contains("In Progress · P2 · matthias · cycle 34"),
            "{text}"
        );
        assert!(text.contains("Ticket body with"), "{text}");
        assert!(text.contains("https://example.invalid"), "{text}");
        assert!(text.contains("Acceptance criteria"), "{text}");
        assert!(text.contains("[Start thread ▾] [⋯]"), "{text}");
    }

    #[test]
    fn full_ticket_action_menu_uses_shared_rows_below_header() {
        let mut state = WorkViewState::new(
            true,
            Some(snapshot(vec![ticket(
                "SCA-3165",
                crate::work_index::TicketGroup::Assigned,
            )])),
        );
        state.projection = WorkProjection::Tickets;
        state.ticket_more_menu = Some(Default::default());
        let text = rendered_text_at(&state, 120, 40);
        for label in [
            "Refresh",
            "Ask a question",
            "Explain this ticket",
            "Transition ▸",
            "Priority ▸",
            "Copy identifier",
            "Cancel ticket",
        ] {
            assert!(text.contains(label), "missing {label}: {text}");
        }
    }

    #[test]
    fn full_ticket_action_menu_opens_downward_and_clamps() {
        let item = ticket("SCA-3165", crate::work_index::TicketGroup::Assigned);
        let ticket = item.ticket_details.first().expect("ticket fixture");
        let context = crate::ui::ticket_actions::TicketActionContext::from_ticket(
            ticket, None, None, None, false,
        );
        let area = Rect::new(40, 3, 50, 5);
        let anchor = ticket_action_menu_anchor(area);
        let layout = crate::ui::ticket_actions::ticket_action_menu_layout(
            anchor,
            area,
            &context,
            crate::ui::ticket_actions::TicketActionMenuState {
                selected: 12,
                ..Default::default()
            },
        )
        .expect("rows below the ticket header");
        assert_eq!(layout.rect.y, anchor.bottom());
        assert_eq!(layout.rect.bottom(), area.bottom());
        assert!(layout.first_visible <= 12);
        assert!(12 < layout.first_visible + layout.visible_rows);
    }

    #[test]
    fn github_degradation_does_not_hide_linear_items() {
        let mut degraded = snapshot(vec![ticket(
            "SCA-3165",
            crate::work_index::TicketGroup::Assigned,
        )]);
        degraded.unavailable = Some(crate::work_index::WorkIndexUnavailable::only(
            crate::work_index::WorkIndexSource::Github,
            "rate limited",
        ));
        let mut state = WorkViewState::new(true, Some(degraded));
        state.projection = WorkProjection::Tickets;

        let text = rendered_text_at(&state, 100, 24);
        assert!(text.contains("SCA-3165"), "{text}");
        assert!(!text.contains("GitHub: rate limited"), "{text}");
    }

    #[test]
    fn degraded_sources_keep_their_previous_rows_visible() {
        let mut github_snapshot = snapshot(vec![pr("owner/repo", 77, &[])]);
        github_snapshot.unavailable = Some(crate::work_index::WorkIndexUnavailable::only(
            crate::work_index::WorkIndexSource::Github,
            "rate limited",
        ));
        let github = rendered_text_at(&WorkViewState::new(true, Some(github_snapshot)), 100, 24);
        assert!(github.contains("GitHub: rate limited"), "{github}");
        assert!(github.contains("PR 77"), "{github}");

        let mut linear_snapshot = snapshot(vec![ticket(
            "SCA-3165",
            crate::work_index::TicketGroup::Assigned,
        )]);
        linear_snapshot.unavailable = Some(crate::work_index::WorkIndexUnavailable::only(
            crate::work_index::WorkIndexSource::Linear,
            "rate limited",
        ));
        let mut linear_state = WorkViewState::new(true, Some(linear_snapshot));
        linear_state.projection = WorkProjection::Tickets;
        let linear = rendered_text_at(&linear_state, 100, 24);
        assert!(linear.contains("Linear: rate limited"), "{linear}");
        assert!(linear.contains("SCA-3165"), "{linear}");

        let mut missive_snapshot = snapshot(Vec::new());
        missive_snapshot.conversations = vec![missive_conversation(1)];
        missive_snapshot.unavailable = Some(crate::work_index::WorkIndexUnavailable::only(
            crate::work_index::WorkIndexSource::Missive,
            "rate limited",
        ));
        let mut missive_state = WorkViewState::new(true, Some(missive_snapshot));
        missive_state.projection = WorkProjection::Missive;
        let missive = rendered_text_at(&missive_state, 100, 24);
        assert!(missive.contains("Missive: rate limited"), "{missive}");
        assert!(missive.contains("Billing question"), "{missive}");
    }

    #[test]
    fn ticket_dropdowns_open_downward_and_clamp() {
        let area = Rect::new(10, 4, 60, 8);
        for (anchor_x, count, selected, width) in [(12, 2, 1, 42), (29, 4, 3, 20), (54, 3, 2, 20)] {
            let layout = ticket_menu_layout(area, anchor_x, count, selected, width)
                .expect("ticket menu fits below header");
            assert_eq!(layout.rect.y, area.y + 1);
            assert!(layout.rect.bottom() <= area.bottom());
        }
        assert!(ticket_menu_layout(Rect::new(0, 2, 40, 1), 1, 2, 0, 20).is_none());
    }

    #[test]
    fn missive_fixture_renders_list_detail_actions_and_footer() {
        let conversation = crate::work_index::MissiveConversation {
            id: "sample".into(),
            subject: "Billing question".into(),
            app_url: "missive://mail.missiveapp.com/#inbox/conversations/sample".into(),
            web_url: "https://mail.missiveapp.com/#inbox/conversations/sample".into(),
            assignees: vec![crate::work_index::MissiveUser {
                id: "ada".into(),
                name: "Ada".into(),
                email: None,
                is_me: true,
            }],
            last_activity_at: Some(SystemTime::UNIX_EPOCH),
            closed: false,
            labels: Vec::new(),
            pane_bound: false,
            messages: vec![crate::work_index::MissiveEntry {
                id: "message".into(),
                author: Some("Customer".into()),
                preview: "invoice preview".into(),
                created_at: Some(SystemTime::UNIX_EPOCH),
            }],
            notes: vec![crate::work_index::MissiveEntry {
                id: "note".into(),
                author: Some("Ada".into()),
                preview: "internal note".into(),
                created_at: Some(SystemTime::UNIX_EPOCH),
            }],
            drafts: Vec::new(),
            posts: Vec::new(),
        };
        let mut state = WorkViewState::new(
            true,
            Some(Snapshot {
                items: Vec::new(),
                conversations: vec![conversation],
                missive_users: Vec::new(),
                unavailable: None,
                observed_at: SystemTime::UNIX_EPOCH,
            }),
        );
        state.projection = WorkProjection::Missive;
        let text = rendered_app_text_at(
            &AppState {
                work_view: Some(state),
                ..AppState::test_new()
            },
            120,
            40,
        );
        assert!(text.contains("Missive"), "{text}");
        assert!(text.contains("Billing question"), "{text}");
        assert!(text.contains("Customer · 0m ago"), "{text}");
        assert!(text.contains("invoice preview"), "{text}");
        assert!(text.contains("Internal notes  1"), "{text}");
        assert!(
            text.contains("[Start thread ▾] [Open in Missive]"),
            "{text}"
        );
        assert!(text.contains("o Open in Missive"), "{text}");
    }

    #[test]
    fn missive_full_view_at_eighty_columns_keeps_subject_actions_and_scroll_hint() {
        let text = rendered_text_at(&missive_state(8), 80, 24);
        assert!(text.contains("Billing question"), "{text}");
        assert!(
            text.contains("[Start thread ▾] [Open in Missive]"),
            "{text}"
        );
        assert!(text.contains("PgUp/PgDn detail"), "{text}");
        assert!(text.contains("o copy"), "{text}");
    }

    #[test]
    fn missive_detail_scroll_reaches_later_messages_independently() {
        let mut state = missive_state(10);
        let initial = rendered_text_at(&state, 80, 24);
        assert!(initial.contains("message body 0"), "{initial}");
        assert!(!initial.contains("message body 6"), "{initial}");

        state.missive_detail_scroll = 13;
        let scrolled = rendered_text_at(&state, 80, 24);
        assert!(scrolled.contains("message body 6"), "{scrolled}");
        assert!(!scrolled.contains("message body 0"), "{scrolled}");

        state.missive_detail_scroll = u16::MAX;
        let clamped = rendered_text_at(&state, 80, 24);
        assert!(clamped.contains("message body 9"), "{clamped}");
    }

    #[test]
    fn missive_controls_fit_each_responsive_layout() {
        for width in [96, 100, 128] {
            let text = rendered_text_at(&missive_state(1), width, 24);
            assert!(text.contains("⚲ open"), "width {width}: {text}");
            assert!(text.contains("r refresh"), "width {width}: {text}");
            assert!(!text.contains("teammates"), "width {width}: {text}");
        }
    }

    #[test]
    fn missive_list_status_glyph_uses_palette_state_colour() {
        let state = missive_state(1);
        let app = AppState {
            work_view: Some(state),
            ..AppState::test_new()
        };
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render(&app, frame.area(), frame))
            .expect("render Missive view");
        let glyph = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .find(|cell| cell.symbol() == "○")
            .expect("open conversation glyph");
        assert_eq!(glyph.fg, app.palette.work_status_open());
    }

    #[test]
    fn missive_start_dropdown_opens_downward_and_clamps() {
        let area = Rect::new(20, 3, 50, 4);
        let layout = ticket_menu_layout(area, 12, 2, 1, 24).expect("menu fits below anchor");
        assert_eq!(layout.rect.y, area.y + 1);
        assert!(layout.rect.bottom() <= area.bottom());
        assert!(ticket_menu_layout(Rect::new(0, 2, 40, 1), 12, 2, 0, 24).is_none());
    }

    #[test]
    fn missive_view_distinguishes_collecting_failure_and_empty_states() {
        let mut state = WorkViewState::new(true, None);
        state.projection = WorkProjection::Missive;
        let collecting = rendered_text(&state);
        assert!(
            collecting.contains("work index not yet collected"),
            "{collecting}"
        );

        state.snapshot = Some(Snapshot {
            items: Vec::new(),
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: Some(crate::work_index::WorkIndexUnavailable::only(
                crate::work_index::WorkIndexSource::Missive,
                "observation timed out",
            )),
            observed_at: SystemTime::UNIX_EPOCH,
        });
        let failed = rendered_text(&state);
        assert!(
            failed.contains("Missive: observation timed out"),
            "{failed}"
        );

        state.snapshot.as_mut().expect("snapshot").unavailable = None;
        let empty = rendered_text(&state);
        assert!(empty.contains("no matching conversations"), "{empty}");
    }
}
