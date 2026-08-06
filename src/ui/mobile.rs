use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame,
};

#[cfg(test)]
use super::sidebar::agent_panel_entries;
use super::sidebar::{
    mobile_sidebar_rows, mobile_sidebar_rows_from, sidebar_row_belongs_to_workspace,
    sidebar_space_member_indices, tab_row_layout, AgentPanelEntry, SidebarRow,
};
use super::status::{state_icon, state_icon_symbol, state_label_color};
use super::text::{display_width, display_width_u16, truncate_end};
use crate::app::state::{Palette, ToastKind, ToastNotification};
use crate::app::AppState;
use crate::config::StatusIndicatorStyle;
use crate::detect::AgentState;
use crate::layout::PaneId;
use crate::terminal::TerminalRuntimeRegistry;

const SWITCH_BUTTON_WIDTH: u16 = 10;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MobileHeaderHitAreas {
    pub menu: Rect,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MobileSwitcherAreas {
    pub close: Rect,
    pub viewport: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MobileSwitcherTarget {
    NewWorkspace,
    Workspace(usize),
    WorkspaceDisclosure(usize),
    NewTab,
    Tab(usize),
    SidebarTab {
        ws_idx: usize,
        tab_idx: usize,
    },
    Agent {
        ws_idx: usize,
        tab_idx: usize,
        pane_id: PaneId,
    },
    Menu(usize),
}

/// Columns of the Space disclosure immediately before its title. Render and
/// hit-testing share this range so the control never moves to the row's right
/// edge as the title or window count changes.
fn mobile_space_disclosure_columns(content: Rect) -> Option<std::ops::Range<u16>> {
    if content.width < 3 {
        return None;
    }
    let start = content.x + 2;
    Some(start..start + 1)
}

pub(crate) fn is_mobile_width(area: Rect, threshold: u16) -> bool {
    area.width > 0 && area.width <= threshold
}

pub(crate) fn compute_mobile_header_hit_areas(_app: &AppState, area: Rect) -> MobileHeaderHitAreas {
    if area.width == 0 || area.height == 0 {
        return MobileHeaderHitAreas::default();
    }

    let width = SWITCH_BUTTON_WIDTH.min(area.width);
    let switch = Rect::new(
        area.x + area.width.saturating_sub(width),
        area.y,
        width,
        area.height,
    );

    MobileHeaderHitAreas { menu: switch }
}

pub(crate) fn mobile_switcher_areas(app: &AppState) -> MobileSwitcherAreas {
    let screen = mobile_screen_rect(app);
    if screen.width == 0 || screen.height <= 2 {
        return MobileSwitcherAreas::default();
    }

    let header_h = screen.height.min(2);
    let close_w = 10u16.min(screen.width);
    let close = Rect::new(
        screen.x + screen.width.saturating_sub(close_w),
        screen.y,
        close_w,
        header_h,
    );
    let viewport = Rect::new(
        screen.x,
        screen.y + header_h + 1,
        screen.width,
        screen.height.saturating_sub(header_h + 1),
    );

    MobileSwitcherAreas { close, viewport }
}

pub(crate) fn mobile_switcher_max_scroll_for_height(app: &AppState, viewport_height: u16) -> usize {
    mobile_switcher_content_height(app).saturating_sub(viewport_height as usize)
}

/// Doc row the sidebar row list starts at: the section title, plus the
/// "+ new workspace" affordance in the Spaces tree or the empty-state line in a
/// flat projection with no rows.
fn mobile_sidebar_rows_start(app: &AppState, rows: &[SidebarRow]) -> usize {
    let mut start = 1;
    if app.sidebar_shows_spaces_tree() || rows.is_empty() {
        start += 1;
    }
    start
}

fn mobile_sidebar_row_height(row: &SidebarRow) -> usize {
    match row {
        SidebarRow::Workspace { .. } | SidebarRow::Tab { .. } => 1,
        SidebarRow::Agent { .. } => 2,
    }
}

fn mobile_sidebar_block_height(app: &AppState) -> usize {
    let rows = mobile_sidebar_rows(app);
    mobile_sidebar_rows_start(app, &rows)
        + rows.iter().map(mobile_sidebar_row_height).sum::<usize>()
}

pub(crate) fn mobile_switcher_workspace_doc_range(
    app: &AppState,
    idx: usize,
) -> Option<std::ops::Range<usize>> {
    // Spaces render in grouped order, so a workspace's row position is its index
    // in the row list, not its raw array index. Flat projections have no
    // workspace rows, so the workspace's first agent row stands in for it.
    let rows = mobile_sidebar_rows(app);
    let pos = rows
        .iter()
        .position(|row| sidebar_row_belongs_to_workspace(row, idx))?;
    let start = mobile_sidebar_rows_start(app, &rows)
        + rows[..pos]
            .iter()
            .map(mobile_sidebar_row_height)
            .sum::<usize>();
    Some(start..start + mobile_sidebar_row_height(&rows[pos]))
}

pub(crate) fn mobile_switcher_max_scroll(app: &AppState) -> usize {
    mobile_switcher_max_scroll_for_height(app, mobile_switcher_areas(app).viewport.height)
}

pub(crate) fn visible_tab_activity_instants_from(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    viewport: Rect,
) -> Vec<std::time::Instant> {
    let content = inset_for_left_scrollbar(viewport);
    if content == Rect::default() {
        return Vec::new();
    }
    let rows = mobile_sidebar_rows_from(app, terminal_runtimes);
    let visible_start = app.mobile_switcher_scroll;
    let visible_end = visible_start.saturating_add(usize::from(viewport.height));
    let mut doc_y = mobile_sidebar_rows_start(app, &rows);

    rows.iter()
        .filter_map(|row| {
            let row_start = doc_y;
            doc_y = doc_y.saturating_add(mobile_sidebar_row_height(row));
            if row_start >= visible_end || doc_y <= visible_start {
                return None;
            }
            let SidebarRow::Tab { entry, depth } = row else {
                return None;
            };
            let indent = " ".repeat(2 + usize::from(*depth) * 3);
            tab_row_layout(
                entry,
                app.view_observed_at,
                usize::from(content.width),
                display_width(&indent),
                &app.palette,
                app.status_indicators,
            )
            .activity_age
            .and(entry.activity_at)
        })
        .collect()
}

pub(crate) fn mobile_switcher_target_at(
    app: &AppState,
    col: u16,
    row: u16,
) -> Option<MobileSwitcherTarget> {
    let areas = mobile_switcher_areas(app);
    let content = inset_for_left_scrollbar(areas.viewport);
    if !rect_contains(content, col, row) {
        return None;
    }

    let scroll = app
        .mobile_switcher_scroll
        .min(mobile_switcher_max_scroll_for_height(
            app,
            areas.viewport.height,
        ));
    let doc_row = scroll.saturating_add(row.saturating_sub(areas.viewport.y) as usize);

    let rows = mobile_sidebar_rows(app);
    if app.sidebar_shows_spaces_tree() && doc_row == 1 {
        return Some(MobileSwitcherTarget::NewWorkspace);
    }
    let mut cursor = mobile_sidebar_rows_start(app, &rows);
    for entry in &rows {
        let row_height = mobile_sidebar_row_height(entry);
        if doc_row >= cursor && doc_row < cursor + row_height {
            let on_title_line = doc_row == cursor;
            return Some(match entry {
                SidebarRow::Workspace { ws_idx, .. } => {
                    let on_disclosure = on_title_line
                        && mobile_space_disclosure_columns(content)
                            .is_some_and(|columns| columns.contains(&col));
                    if on_disclosure {
                        MobileSwitcherTarget::WorkspaceDisclosure(*ws_idx)
                    } else {
                        MobileSwitcherTarget::Workspace(*ws_idx)
                    }
                }
                SidebarRow::Agent { entry, .. } => MobileSwitcherTarget::Agent {
                    ws_idx: entry.ws_idx,
                    tab_idx: entry.tab_idx,
                    pane_id: entry.pane_id,
                },
                SidebarRow::Tab { entry, .. } => MobileSwitcherTarget::SidebarTab {
                    ws_idx: entry.ws_idx,
                    tab_idx: entry.tab_idx,
                },
            });
        }
        cursor += row_height;
    }

    if let Some(ws) = app.active.and_then(|idx| app.workspaces.get(idx)) {
        cursor += 1; // tabs title
        if doc_row == cursor {
            return Some(MobileSwitcherTarget::NewTab);
        }
        cursor += 1;
        let tabs_end = cursor + ws.tabs.len();
        if doc_row >= cursor && doc_row < tabs_end {
            return Some(MobileSwitcherTarget::Tab(doc_row - cursor));
        }
        cursor = tabs_end;
    }

    cursor += 1; // menu title
    let menu_idx = doc_row.checked_sub(cursor)?;
    (menu_idx < app.global_menu_labels().len()).then_some(MobileSwitcherTarget::Menu(menu_idx))
}

pub(crate) fn render_mobile_header(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let p = &app.palette;
    fill_rect(frame, area, Style::default().bg(p.panel_bg));

    let switch = app.view.mobile_menu_hit_area;
    let status_w = switch.x.saturating_sub(area.x).saturating_sub(1);
    let status = Rect::new(area.x, area.y, status_w, area.height);

    render_header_status(app, terminal_runtimes, frame, status);
    render_switch_button(app, frame, switch);
}

pub(crate) fn mobile_toast_banner_rect(area: Rect, offset_for_warning: bool) -> Rect {
    if area.width == 0 || area.height == 0 {
        return Rect::default();
    }

    let y = area.y
        + area
            .height
            .saturating_sub(1 + if offset_for_warning { 1 } else { 0 });
    Rect::new(area.x, y, area.width, 1)
}

pub(crate) fn render_mobile_toast_banner(
    frame: &mut Frame,
    area: Rect,
    toast: &ToastNotification,
    offset_for_warning: bool,
    p: &Palette,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let dot_color = match toast.kind {
        ToastKind::NeedsAttention => p.red,
        ToastKind::Finished => p.blue,
        ToastKind::UpdateInstalled => p.accent,
    };
    let banner = mobile_toast_banner_rect(area, offset_for_warning);
    let bg = p.surface0;

    frame.render_widget(Clear, banner);
    fill_rect(frame, banner, Style::default().bg(bg));
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ", Style::default().bg(bg)),
            Span::styled("●", Style::default().fg(dot_color).bg(bg)),
            Span::styled(" ", Style::default().bg(bg)),
            Span::styled(
                mobile_toast_title(toast),
                Style::default()
                    .fg(p.text)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" · ", Style::default().fg(p.overlay0).bg(bg)),
            Span::styled(&toast.context, Style::default().fg(p.overlay0).bg(bg)),
        ])),
        banner,
    );
}

pub(crate) fn render_mobile_panel(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let p = &app.palette;
    frame.render_widget(Clear, area);
    fill_rect(frame, area, Style::default().bg(p.panel_bg));

    let areas = mobile_switcher_areas(app);
    frame.render_widget(
        Paragraph::new(" switch").style(
            Style::default()
                .fg(p.text)
                .bg(p.panel_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Rect::new(area.x, area.y, areas.close.x.saturating_sub(area.x), 1),
    );
    render_close_button(app, frame, areas.close);

    if area.height > areas.close.height {
        draw_horizontal_rule(
            frame,
            Rect::new(area.x, area.y + areas.close.height, area.width, 1),
            p,
        );
    }

    render_mobile_switcher_content(app, terminal_runtimes, frame, areas.viewport);
}

fn render_header_status(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let p = &app.palette;
    let Some(ws) = app.active.and_then(|idx| app.workspaces.get(idx)) else {
        frame.render_widget(Paragraph::new(" no workspace"), area);
        return;
    };

    let (state, seen) = ws.aggregate_state(&app.terminals);
    let (dot, dot_style) = state_icon(state, seen, app.status_indicators, p);
    let tab_label = mobile_tab_status(ws, &app.terminals, area.width.saturating_sub(6) as usize);
    let row1 = Rect::new(area.x, area.y, area.width, 1);
    let tab_w = display_width_u16(&tab_label)
        .saturating_add(1)
        .min(area.width);
    let name_w = area.width.saturating_sub(tab_w);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(dot, dot_style.bg(p.panel_bg)),
            Span::raw(" "),
            Span::styled(
                truncate_end(
                    &ws.display_name_from(&app.terminals, terminal_runtimes),
                    name_w.saturating_sub(4) as usize,
                ),
                Style::default()
                    .fg(p.text)
                    .bg(p.panel_bg)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        Rect::new(row1.x, row1.y, name_w, 1),
    );
    frame.render_widget(
        Paragraph::new(tab_label)
            .style(Style::default().fg(p.overlay1).bg(p.panel_bg))
            .alignment(Alignment::Right),
        Rect::new(row1.x + name_w, row1.y, tab_w, 1),
    );

    if area.height > 1 {
        frame.render_widget(
            Paragraph::new(agent_summary_line(app, p, area.width)),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
    }
}

fn mobile_tab_status(
    ws: &crate::workspace::Workspace,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    max_width: usize,
) -> String {
    let prefix = "tab ";
    let suffix = if ws.tabs.len() > 1 {
        format!(" · {}/{}", ws.active_tab + 1, ws.tabs.len())
    } else {
        String::new()
    };
    let label_width = max_width
        .saturating_sub(display_width(prefix))
        .saturating_sub(display_width(&suffix));
    let tab_label = ws
        .tab_display_projection(terminals, ws.active_tab)
        .map(|projection| super::tabs::fit_tab_display_projection(projection, label_width))
        .unwrap_or_else(|| truncate_end(&(ws.active_tab + 1).to_string(), label_width));
    truncate_end(&format!("{prefix}{tab_label}{suffix}"), max_width)
}

fn render_switch_button(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let p = &app.palette;
    fill_rect(frame, area, Style::default().bg(p.surface0));
    for y in area.y..area.y + area.height {
        frame.buffer_mut()[(area.x, y)]
            .set_symbol("│")
            .set_style(Style::default().fg(p.surface_dim).bg(p.surface0));
    }
    let label_y = if area.height > 1 { area.y + 1 } else { area.y };
    frame.render_widget(
        Paragraph::new("switch")
            .style(
                Style::default()
                    .fg(p.text)
                    .bg(p.surface0)
                    .add_modifier(Modifier::BOLD),
            )
            .alignment(Alignment::Center),
        Rect::new(area.x + 1, label_y, area.width.saturating_sub(1), 1),
    );

    // Attention badge: a blocked agent anywhere makes the button itself read as
    // "tap me" without the user reading the summary row.
    if global_agent_counts(app).blocked > 0 {
        let bx = area.x + area.width.saturating_sub(1);
        let (symbol, style) = state_icon(AgentState::Blocked, true, app.status_indicators, p);
        frame.buffer_mut()[(bx, area.y)]
            .set_symbol(symbol)
            .set_style(style.bg(p.surface0));
    }
}

fn render_close_button(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let p = &app.palette;
    fill_rect(frame, area, Style::default().bg(p.surface0));
    for y in area.y..area.y + area.height {
        frame.buffer_mut()[(area.x, y)]
            .set_symbol("│")
            .set_style(Style::default().fg(p.surface_dim).bg(p.surface0));
    }
    frame.render_widget(
        Paragraph::new("close")
            .style(
                Style::default()
                    .fg(p.overlay1)
                    .bg(p.surface0)
                    .add_modifier(Modifier::BOLD),
            )
            .alignment(Alignment::Center),
        Rect::new(area.x + 1, area.y, area.width.saturating_sub(1), 1),
    );
    if area.height > 1 {
        frame.render_widget(
            Paragraph::new("×")
                .style(
                    Style::default()
                        .fg(p.text)
                        .bg(p.surface0)
                        .add_modifier(Modifier::BOLD),
                )
                .alignment(Alignment::Center),
            Rect::new(area.x + 1, area.y + 1, area.width.saturating_sub(1), 1),
        );
    }
}

fn mobile_switcher_content_height(app: &AppState) -> usize {
    let tabs_h = app
        .active
        .and_then(|idx| app.workspaces.get(idx))
        .map(|ws| 2 + ws.tabs.len())
        .unwrap_or(0);
    let menu_h = 1 + app.global_menu_labels().len();
    mobile_sidebar_block_height(app) + tabs_h + menu_h
}

fn render_mobile_switcher_content(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    viewport: Rect,
) {
    if viewport.width == 0 || viewport.height == 0 {
        return;
    }

    let p = &app.palette;
    let total_height = mobile_switcher_content_height(app);
    render_left_scrollbar(
        frame,
        viewport,
        total_height,
        viewport.height as usize,
        app.mobile_switcher_scroll,
        p,
    );
    let content = inset_for_left_scrollbar(viewport);
    if content == Rect::default() {
        return;
    }

    let mut doc_y = 0usize;

    let rows = mobile_sidebar_rows_from(app, terminal_runtimes);
    let title = if app.sidebar_shows_spaces_tree() {
        "spaces".to_string()
    } else {
        app.agent_view_override
            .as_ref()
            .map(|view| format!("agents · {}", view.label.as_deref().unwrap_or("filtered")))
            .unwrap_or_else(|| "agents · priority".to_string())
    };
    render_section_title_at(
        frame,
        viewport,
        content,
        doc_y,
        app.mobile_switcher_scroll,
        &title,
        p,
    );
    doc_y += 1;
    if app.sidebar_shows_spaces_tree() {
        render_action_row_at(
            frame,
            viewport,
            content,
            doc_y,
            app.mobile_switcher_scroll,
            "+ new workspace",
            p,
        );
        doc_y += 1;
    } else if rows.is_empty() {
        render_one_line_item(
            frame,
            viewport,
            content,
            doc_y,
            app.mobile_switcher_scroll,
            ratatui::style::Color::Reset,
            Line::from(Span::styled(
                "  no matching agents",
                Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
            )),
        );
        doc_y += 1;
    }

    let focused_agent = app.active.and_then(|ws_idx| {
        let ws = app.workspaces.get(ws_idx)?;
        ws.focused_pane_id()
            .map(|pane_id| (ws_idx, ws.active_tab, pane_id))
    });
    for row in &rows {
        match row {
            SidebarRow::Workspace { ws_idx, .. } => {
                let Some(ws) = app.workspaces.get(*ws_idx) else {
                    continue;
                };
                let member_indices = sidebar_space_member_indices(app, *ws_idx);
                let active = app
                    .active
                    .is_some_and(|active| member_indices.contains(&active));
                let selected = member_indices.contains(&app.selected);
                let bg = mobile_item_bg(selected, active, p);
                let mut title_spans = vec![Span::styled("  ", Style::default().bg(bg))];
                let expanded = app.workspace_agents_expanded(*ws_idx);
                title_spans.push(Span::styled(
                    if expanded { "▾" } else { "▸" },
                    Style::default().fg(p.accent).bg(bg),
                ));
                title_spans.push(Span::styled(" ", Style::default().bg(bg)));
                let name = if crate::ui::workspace_parent_group_state(app, *ws_idx).is_some() {
                    ws.worktree_space()
                        .map(|space| space.label.clone())
                        .unwrap_or_else(|| ws.display_name_from(&app.terminals, terminal_runtimes))
                } else {
                    ws.display_name_from(&app.terminals, terminal_runtimes)
                };
                let window_count = member_indices
                    .iter()
                    .filter_map(|member| app.workspaces.get(*member))
                    .map(|workspace| workspace.tabs.len())
                    .sum::<usize>();
                let count_label = format!(" ({window_count})");
                let fixed_width = 4u16.saturating_add(display_width_u16(&count_label));
                let name_width = content.width.saturating_sub(fixed_width);
                title_spans.push(Span::styled(
                    truncate_end(&name, name_width as usize),
                    mobile_item_title_style(selected, active, p).bg(bg),
                ));
                title_spans.push(Span::styled(
                    count_label,
                    Style::default()
                        .fg(p.overlay0)
                        .bg(bg)
                        .add_modifier(Modifier::DIM),
                ));
                render_one_line_item(
                    frame,
                    viewport,
                    content,
                    doc_y,
                    app.mobile_switcher_scroll,
                    bg,
                    Line::from(title_spans),
                );
            }
            SidebarRow::Agent { entry, depth } => {
                let active = focused_agent.is_some_and(|(ws_idx, tab_idx, pane_id)| {
                    entry.ws_idx == ws_idx && entry.tab_idx == tab_idx && entry.pane_id == pane_id
                });
                let bg = mobile_item_bg(false, active, p);
                let (icon, icon_style) =
                    state_icon(entry.state, entry.seen, app.status_indicators, p);
                let indent = " ".repeat(2 + usize::from(*depth) * 3);
                let title = Line::from(vec![
                    Span::styled(indent, Style::default().bg(bg)),
                    Span::styled(icon, icon_style.bg(bg)),
                    Span::styled(" ", Style::default().bg(bg)),
                    Span::styled(
                        truncate_end(
                            entry.pane_label.as_deref().unwrap_or("Pane"),
                            content.width.saturating_sub(5) as usize,
                        ),
                        mobile_item_title_style(false, active, p).bg(bg),
                    ),
                ]);
                render_two_line_item(
                    frame,
                    viewport,
                    content,
                    doc_y,
                    app.mobile_switcher_scroll,
                    bg,
                    title,
                    truncate_end(&mobile_agent_detail(entry), content.width as usize),
                    p.overlay0,
                );
            }
            SidebarRow::Tab { entry, depth } => {
                let active = app.active == Some(entry.ws_idx)
                    && app
                        .workspaces
                        .get(entry.ws_idx)
                        .is_some_and(|ws| ws.active_tab_index() == entry.tab_idx);
                let bg = mobile_item_bg(false, active, p);
                let indent = " ".repeat(2 + usize::from(*depth) * 3);
                let layout = tab_row_layout(
                    entry,
                    app.view_observed_at,
                    usize::from(content.width),
                    display_width(&indent),
                    p,
                    app.status_indicators,
                );
                let mut spans = vec![Span::styled(indent, Style::default().bg(bg))];
                if let Some(state) = layout.state.as_deref() {
                    let (icon, icon_style) =
                        state_icon(entry.state, entry.seen, app.status_indicators, p);
                    spans.push(Span::styled(icon, icon_style.bg(bg)));
                    if layout.show_state_label {
                        spans.extend([
                            Span::styled(
                                format!(" {state}"),
                                Style::default()
                                    .fg(state_label_color(entry.state, entry.seen, p))
                                    .bg(bg),
                            ),
                            Span::styled(" · ", Style::default().fg(p.overlay0).bg(bg)),
                        ]);
                    } else {
                        spans.push(Span::styled(" ", Style::default().bg(bg)));
                    }
                }
                spans.push(Span::styled(
                    layout.title,
                    mobile_item_title_style(false, active, p).bg(bg),
                ));
                if let Some(agent_suffix) = layout.agent_suffix {
                    spans.push(Span::styled(
                        agent_suffix,
                        Style::default().fg(p.overlay1).bg(bg),
                    ));
                }
                if let Some(background_jobs) = layout.background_jobs {
                    spans.push(Span::styled(
                        background_jobs,
                        Style::default().fg(p.overlay0).bg(bg),
                    ));
                }
                if let Some(activity_age) = layout.activity_age {
                    let used_width = spans
                        .iter()
                        .map(|span| display_width(span.content.as_ref()))
                        .sum::<usize>();
                    let padding = usize::from(content.width)
                        .saturating_sub(used_width)
                        .saturating_sub(display_width(&activity_age));
                    spans.push(Span::styled(" ".repeat(padding), Style::default().bg(bg)));
                    let activity_color = if entry.state == AgentState::Working {
                        p.blue
                    } else {
                        p.green
                    };
                    spans.push(Span::styled(
                        activity_age,
                        Style::default().fg(activity_color).bg(bg),
                    ));
                }
                let title = Line::from(spans);
                render_one_line_item(
                    frame,
                    viewport,
                    content,
                    doc_y,
                    app.mobile_switcher_scroll,
                    bg,
                    title,
                );
            }
        }
        doc_y += mobile_sidebar_row_height(row);
    }

    if let Some(ws) = app.active.and_then(|idx| app.workspaces.get(idx)) {
        render_section_title_at(
            frame,
            viewport,
            content,
            doc_y,
            app.mobile_switcher_scroll,
            "tabs",
            p,
        );
        doc_y += 1;
        render_action_row_at(
            frame,
            viewport,
            content,
            doc_y,
            app.mobile_switcher_scroll,
            "+ new tab",
            p,
        );
        doc_y += 1;
        for (idx, tab) in ws.tabs.iter().enumerate() {
            let active = idx == ws.active_tab;
            let bg = mobile_item_bg(false, active, p);
            let label_prefix = if tab.is_auto_named() {
                "tab ".to_string()
            } else {
                format!("{} · ", idx + 1)
            };
            let label_width = content
                .width
                .saturating_sub(3)
                .saturating_sub(display_width_u16(&label_prefix))
                as usize;
            let display_name = ws
                .tab_display_projection(&app.terminals, idx)
                .map(|projection| super::tabs::fit_tab_display_projection(projection, label_width))
                .unwrap_or_else(|| truncate_end(&(idx + 1).to_string(), label_width));
            let label = format!("{label_prefix}{display_name}");
            let title = Line::from(vec![
                Span::styled("  ", Style::default().bg(bg)),
                Span::styled(
                    truncate_end(&label, content.width.saturating_sub(3) as usize),
                    mobile_item_title_style(false, active, p).bg(bg),
                ),
            ]);
            render_one_line_item(
                frame,
                viewport,
                content,
                doc_y,
                app.mobile_switcher_scroll,
                bg,
                title,
            );
            doc_y += 1;
        }
    }

    render_section_title_at(
        frame,
        viewport,
        content,
        doc_y,
        app.mobile_switcher_scroll,
        "menu",
        p,
    );
    doc_y += 1;
    for label in app.global_menu_labels() {
        if let Some(y) = visible_y(viewport, app.mobile_switcher_scroll, doc_y) {
            frame.render_widget(
                Paragraph::new(format!("  {label}"))
                    .style(Style::default().fg(p.overlay1).bg(p.panel_bg)),
                Rect::new(content.x, y, content.width, 1),
            );
        }
        doc_y += 1;
    }
}

fn mobile_agent_detail(entry: &AgentPanelEntry) -> String {
    let mut parts = Vec::new();
    let status = entry
        .state_labels
        .get(super::sidebar::agent_panel_status_key(
            entry.state,
            entry.seen,
        ))
        .cloned()
        .unwrap_or_else(|| match entry.state {
            AgentState::Idle => "done".to_string(),
            _ => super::status::state_label(entry.state, entry.seen).to_string(),
        });
    parts.push(status);
    if let Some(agent_label) = entry.agent_label.as_deref() {
        parts.push(agent_label.to_string());
    }
    format!("  {}", parts.join(" · "))
}

fn render_section_title_at(
    frame: &mut Frame,
    viewport: Rect,
    content: Rect,
    doc_y: usize,
    scroll: usize,
    title: &str,
    p: &Palette,
) {
    let Some(y) = visible_y(viewport, scroll, doc_y) else {
        return;
    };
    render_section_title(
        frame,
        Rect::new(content.x, y, content.width.saturating_sub(1), 1),
        title,
        p,
    );
}

fn render_action_row_at(
    frame: &mut Frame,
    viewport: Rect,
    content: Rect,
    doc_y: usize,
    scroll: usize,
    label: &str,
    p: &Palette,
) {
    let Some(y) = visible_y(viewport, scroll, doc_y) else {
        return;
    };
    render_action_row(frame, Rect::new(content.x, y, content.width, 1), label, p);
}

fn render_one_line_item(
    frame: &mut Frame,
    viewport: Rect,
    content: Rect,
    doc_y: usize,
    scroll: usize,
    bg: ratatui::style::Color,
    title: Line<'_>,
) {
    fill_visible_doc_rect(
        frame,
        viewport,
        content,
        doc_y,
        1,
        Style::default().bg(bg),
        scroll,
    );
    if let Some(y) = visible_y(viewport, scroll, doc_y) {
        frame.render_widget(
            Paragraph::new(title),
            Rect::new(content.x, y, content.width, 1),
        );
    }
}

fn render_two_line_item(
    frame: &mut Frame,
    viewport: Rect,
    content: Rect,
    doc_y: usize,
    scroll: usize,
    bg: ratatui::style::Color,
    title: Line<'_>,
    detail: String,
    detail_fg: ratatui::style::Color,
) {
    fill_visible_doc_rect(
        frame,
        viewport,
        content,
        doc_y,
        2,
        Style::default().bg(bg),
        scroll,
    );
    if let Some(y) = visible_y(viewport, scroll, doc_y) {
        frame.render_widget(
            Paragraph::new(title),
            Rect::new(content.x, y, content.width, 1),
        );
    }
    if let Some(y) = visible_y(viewport, scroll, doc_y + 1) {
        frame.render_widget(
            Paragraph::new(detail).style(Style::default().fg(detail_fg).bg(bg)),
            Rect::new(content.x, y, content.width, 1),
        );
    }
}

fn visible_y(viewport: Rect, scroll: usize, doc_y: usize) -> Option<u16> {
    let offset = doc_y.checked_sub(scroll)?;
    (offset < viewport.height as usize).then_some(viewport.y + offset as u16)
}

fn fill_visible_doc_rect(
    frame: &mut Frame,
    viewport: Rect,
    content: Rect,
    doc_y: usize,
    height: usize,
    style: Style,
    scroll: usize,
) {
    for offset in 0..height {
        if let Some(y) = visible_y(viewport, scroll, doc_y + offset) {
            fill_rect(frame, Rect::new(content.x, y, content.width, 1), style);
        }
    }
}

fn mobile_item_bg(_selected: bool, _active: bool, p: &Palette) -> ratatui::style::Color {
    p.panel_bg
}

fn mobile_item_title_style(selected: bool, active: bool, p: &Palette) -> Style {
    if selected || active {
        Style::default()
            .fg(super::sidebar::active_sidebar_title_color(p))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.subtext0)
    }
}

fn inset_for_left_scrollbar(area: Rect) -> Rect {
    if area.width <= 1 {
        return Rect::default();
    }
    Rect::new(area.x + 1, area.y, area.width - 1, area.height)
}

fn render_left_scrollbar(
    frame: &mut Frame,
    area: Rect,
    total_rows: usize,
    visible_rows: usize,
    scroll: usize,
    p: &Palette,
) {
    if area.width == 0 || area.height == 0 || visible_rows == 0 || total_rows <= visible_rows {
        return;
    }

    let track = Rect::new(area.x, area.y, 1, area.height);
    let max_scroll = total_rows.saturating_sub(visible_rows);
    let thumb_len = ((track.height as usize * visible_rows).div_ceil(total_rows))
        .max(1)
        .min(track.height as usize) as u16;
    let travel = track.height.saturating_sub(thumb_len);
    let thumb_top = track.y + ((travel as usize * scroll.min(max_scroll)) / max_scroll) as u16;

    for y in track.y..track.y + track.height {
        let is_thumb = y >= thumb_top && y < thumb_top + thumb_len;
        frame.buffer_mut()[(track.x, y)]
            .set_symbol(if is_thumb { "▌" } else { "│" })
            .set_style(
                Style::default()
                    .fg(if is_thumb { p.accent } else { p.surface_dim })
                    .bg(p.panel_bg),
            );
    }
}

fn render_section_title(frame: &mut Frame, area: Rect, title: &str, p: &Palette) {
    frame.render_widget(
        Paragraph::new(format!(" {title} ")).style(
            Style::default()
                .fg(p.overlay1)
                .bg(p.panel_bg)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        ),
        Rect::new(area.x, area.y, area.width, 1),
    );
}

fn render_action_row(frame: &mut Frame, area: Rect, label: &str, p: &Palette) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(format!("  {label}")).style(
            Style::default()
                .fg(p.accent)
                .bg(p.panel_bg)
                .add_modifier(Modifier::BOLD),
        ),
        area,
    );
}

fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && col >= rect.x
        && col < rect.x + rect.width
        && row >= rect.y
        && row < rect.y + rect.height
}

fn mobile_screen_rect(app: &AppState) -> Rect {
    let header = app.view.mobile_header_rect;
    let terminal = app.view.terminal_area;
    let x = header.x.min(terminal.x);
    let y = header.y.min(terminal.y);
    let right = (header.x + header.width).max(terminal.x + terminal.width);
    let bottom = (header.y + header.height).max(terminal.y + terminal.height);
    Rect::new(x, y, right.saturating_sub(x), bottom.saturating_sub(y))
}

/// Agent state counts across every workspace. The mobile header is global on
/// purpose: while you stare at one terminal, a blocked agent anywhere should
/// still surface.
#[derive(Debug, Default, Clone, Copy)]
struct GlobalAgentCounts {
    blocked: usize,
    done: usize,
    working: usize,
    idle: usize,
}

impl GlobalAgentCounts {
    fn total(&self) -> usize {
        self.blocked + self.done + self.working + self.idle
    }

    fn any_pending(&self) -> bool {
        self.blocked > 0 || self.done > 0 || self.working > 0
    }
}

fn global_agent_counts(app: &AppState) -> GlobalAgentCounts {
    let mut counts = GlobalAgentCounts::default();
    for entry in crate::ui::all_agent_panel_entries(app) {
        match (entry.state, entry.seen) {
            (AgentState::Blocked, _) => counts.blocked += 1,
            (AgentState::Idle, false) => counts.done += 1,
            (AgentState::Working, _) => counts.working += 1,
            (AgentState::Idle, true) => counts.idle += 1,
            (AgentState::Unknown, _) => {}
        }
    }
    counts
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SummaryTone {
    Blocked,
    Done,
    Working,
    Idle,
    Muted,
}

/// Ordered, non-zero breakdown for the header roll-up: attention states lead
/// (blocked → done → working → idle). Pure so it can be unit-tested.
fn agent_summary_segments(
    counts: GlobalAgentCounts,
    indicator_style: StatusIndicatorStyle,
) -> Vec<(String, SummaryTone)> {
    if counts.total() == 0 {
        return vec![("no agents".to_string(), SummaryTone::Muted)];
    }
    if !counts.any_pending() {
        return vec![("all idle".to_string(), SummaryTone::Muted)];
    }
    let mut segments = Vec::new();
    if counts.blocked > 0 {
        segments.push((
            agent_summary_text(
                indicator_style,
                AgentState::Blocked,
                true,
                Some("◉"),
                counts.blocked,
                "blocked",
            ),
            SummaryTone::Blocked,
        ));
    }
    if counts.done > 0 {
        segments.push((
            agent_summary_text(
                indicator_style,
                AgentState::Idle,
                false,
                Some("●"),
                counts.done,
                "done",
            ),
            SummaryTone::Done,
        ));
    }
    if counts.working > 0 {
        segments.push((
            agent_summary_text(
                indicator_style,
                AgentState::Working,
                true,
                None,
                counts.working,
                "working",
            ),
            SummaryTone::Working,
        ));
    }
    if counts.idle > 0 {
        segments.push((
            agent_summary_text(
                indicator_style,
                AgentState::Idle,
                true,
                None,
                counts.idle,
                "idle",
            ),
            SummaryTone::Idle,
        ));
    }
    segments
}

fn agent_summary_text(
    indicator_style: StatusIndicatorStyle,
    state: AgentState,
    seen: bool,
    dot_style_symbol: Option<&str>,
    count: usize,
    label: &str,
) -> String {
    let symbol = match indicator_style {
        StatusIndicatorStyle::Dots => dot_style_symbol,
        StatusIndicatorStyle::Symbols => Some(state_icon_symbol(state, seen, indicator_style)),
    };
    match symbol {
        Some(symbol) => format!("{symbol} {count} {label}"),
        None => format!("{count} {label}"),
    }
}

/// Greedily keep the most-urgent segments that fit `max_width` (counting the
/// leading space and " · " separators) and report whether any were dropped.
/// Segments are ordered by urgency, so the dropped tail is always the least
/// important state.
fn fit_summary_segments(
    segments: Vec<(String, SummaryTone)>,
    max_width: usize,
) -> (Vec<(String, SummaryTone)>, bool) {
    let mut shown = Vec::new();
    let mut used = 1usize; // leading space
    for (idx, segment) in segments.iter().enumerate() {
        let sep = if idx > 0 { 3 } else { 0 }; // " · "
        let seg_w = segment.0.chars().count();
        if used + sep + seg_w > max_width {
            break;
        }
        used += sep + seg_w;
        shown.push(segment.clone());
    }
    let truncated = shown.len() < segments.len();
    (shown, truncated)
}

fn agent_summary_line(app: &AppState, p: &Palette, max_width: u16) -> Line<'static> {
    let segments = agent_summary_segments(global_agent_counts(app), app.status_indicators);
    let (shown, truncated) = fit_summary_segments(segments, max_width as usize);

    let mut spans = vec![Span::styled(" ", Style::default().bg(p.panel_bg))];
    let mut used = 1usize;
    for (idx, (text, tone)) in shown.into_iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled(
                " · ",
                Style::default().fg(p.overlay0).bg(p.panel_bg),
            ));
            used += 3;
        }
        // The leading (most urgent) segment is bold. Working retains its blue
        // activity accent even when a higher-priority segment precedes it.
        let style = summary_tone_style(tone, p, idx == 0);
        used += text.chars().count();
        spans.push(Span::styled(text, style));
    }
    if truncated && used + 2 <= max_width as usize {
        spans.push(Span::styled(
            " …",
            Style::default().fg(p.overlay0).bg(p.panel_bg),
        ));
    }
    Line::from(spans)
}

fn summary_tone_color(tone: SummaryTone, p: &Palette) -> Color {
    match tone {
        SummaryTone::Blocked => p.red,
        SummaryTone::Done | SummaryTone::Working => p.blue,
        SummaryTone::Idle | SummaryTone::Muted => p.overlay1,
    }
}

fn summary_tone_style(tone: SummaryTone, p: &Palette, leading: bool) -> Style {
    let color = if leading || tone == SummaryTone::Working {
        summary_tone_color(tone, p)
    } else {
        p.overlay1
    };
    let style = Style::default().fg(color).bg(p.panel_bg);
    if leading && tone != SummaryTone::Muted {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

fn mobile_toast_title(toast: &ToastNotification) -> String {
    match toast.kind {
        ToastKind::NeedsAttention => toast
            .title
            .strip_suffix(" needs attention")
            .map(|agent| format!("{agent} waiting"))
            .unwrap_or_else(|| toast.title.clone()),
        ToastKind::Finished => toast
            .title
            .strip_suffix(" finished")
            .map(|agent| format!("{agent} done"))
            .unwrap_or_else(|| toast.title.clone()),
        ToastKind::UpdateInstalled => "update ready".to_string(),
    }
}

fn fill_rect(frame: &mut Frame, area: Rect, style: Style) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            buf[(x, y)].set_symbol(" ");
            buf[(x, y)].set_style(style);
        }
    }
}

fn draw_horizontal_rule(frame: &mut Frame, area: Rect, p: &Palette) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let buf = frame.buffer_mut();
    for x in area.x..area.x + area.width {
        buf[(x, area.y)]
            .set_symbol("─")
            .set_style(Style::default().fg(p.surface_dim).bg(p.panel_bg));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent_entry(primary_tab_label: Option<&str>, agent_label: Option<&str>) -> AgentPanelEntry {
        AgentPanelEntry {
            ws_idx: 0,
            tab_idx: 0,
            pane_id: PaneId::from_raw(1),
            primary_label: "herdr".into(),
            primary_tab_label: primary_tab_label.map(str::to_string),
            pane_label: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_label: agent_label.map(str::to_string),
            agent_kind_label: agent_label.map(str::to_string),
            agent: agent_label.and_then(crate::detect::parse_agent_label),
            agent_context: agent_label.and_then(crate::detect::parse_agent_label),
            has_agent: agent_label.is_some(),
            state: AgentState::Idle,
            background_job_count: None,
            seen: true,
            last_agent_state_change_seq: None,
            activity_at: None,
            state_labels: std::collections::HashMap::new(),
            tokens: std::collections::HashMap::new(),
            tab_first_pane: false,
        }
    }

    #[test]
    fn global_agent_counts_ignore_active_agent_view_filter() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            crate::workspace::Workspace::test_new("blocked"),
            crate::workspace::Workspace::test_new("working"),
        ];
        app.ensure_test_terminals();
        for (ws_idx, state) in [(0, AgentState::Blocked), (1, AgentState::Working)] {
            let pane_id = app.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let terminal = app.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(crate::detect::Agent::Claude);
            terminal.state = state;
        }
        app.agent_view_override = Some(crate::api::schema::AgentViewSetParams {
            source: "example.views".to_string(),
            label: None,
            filter: Some(crate::api::schema::AgentViewFilter::Eq {
                field: crate::api::schema::AgentViewField::Builtin(
                    crate::api::schema::AgentViewBuiltinField::Status,
                ),
                value: crate::api::schema::AgentViewValue::String("working".to_string()),
            }),
            sort: Vec::new(),
        });

        let counts = global_agent_counts(&app);
        assert_eq!(counts.blocked, 1);
        assert_eq!(counts.working, 1);
    }

    #[test]
    fn agent_summary_leads_with_attention_states_in_priority_order() {
        let counts = GlobalAgentCounts {
            blocked: 2,
            done: 1,
            working: 2,
            idle: 1,
        };
        let segments = agent_summary_segments(counts, StatusIndicatorStyle::Dots);
        let labels: Vec<&str> = segments.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(
            labels,
            vec!["◉ 2 blocked", "● 1 done", "2 working", "1 idle"]
        );
        assert_eq!(segments[0].1, SummaryTone::Blocked);
    }

    #[test]
    fn distinct_agent_summary_uses_configured_symbols_for_every_state() {
        let counts = GlobalAgentCounts {
            blocked: 2,
            done: 1,
            working: 2,
            idle: 1,
        };
        let labels: Vec<String> = agent_summary_segments(counts, StatusIndicatorStyle::Symbols)
            .into_iter()
            .map(|(text, _)| text)
            .collect();
        assert_eq!(
            labels,
            ["× 2 blocked", "✓ 1 done", "◐ 2 working", "○ 1 idle"]
        );
    }

    #[test]
    fn distinct_status_style_updates_mobile_blocked_badge() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("blocked")];
        app.ensure_test_terminals();
        app.status_indicators = StatusIndicatorStyle::Symbols;
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal_state = app.terminals.get_mut(&terminal_id).unwrap();
        terminal_state.detected_agent = Some(crate::detect::Agent::Claude);
        terminal_state.state = AgentState::Blocked;

        let area = Rect::new(0, 0, 12, 2);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(area.width, area.height))
                .unwrap();
        terminal
            .draw(|frame| render_switch_button(&app, frame, area))
            .unwrap();

        assert_eq!(
            terminal.backend().buffer()[(area.width - 1, 0)].symbol(),
            "×"
        );
    }

    #[test]
    fn working_summary_uses_blue_activity_accent() {
        let palette = Palette::catppuccin();
        assert_eq!(
            summary_tone_color(SummaryTone::Working, &palette),
            palette.blue
        );
        // ac7: Working stays blue behind a higher-priority leading segment.
        assert_eq!(
            summary_tone_style(SummaryTone::Working, &palette, false).fg,
            Some(palette.blue)
        );
        assert_eq!(
            summary_tone_style(SummaryTone::Done, &palette, false).fg,
            Some(palette.overlay1)
        );
    }

    #[test]
    fn agent_summary_hides_empty_categories() {
        let counts = GlobalAgentCounts {
            done: 1,
            working: 2,
            ..Default::default()
        };
        let labels: Vec<String> = agent_summary_segments(counts, StatusIndicatorStyle::Dots)
            .into_iter()
            .map(|(text, _)| text)
            .collect();
        assert_eq!(
            labels,
            vec!["● 1 done".to_string(), "2 working".to_string()]
        );
    }

    #[test]
    fn agent_summary_collapses_to_all_idle_without_attention() {
        let counts = GlobalAgentCounts {
            idle: 3,
            ..Default::default()
        };
        assert_eq!(
            agent_summary_segments(counts, StatusIndicatorStyle::Dots),
            vec![("all idle".to_string(), SummaryTone::Muted)]
        );
    }

    #[test]
    fn agent_summary_drops_least_urgent_segments_when_narrow() {
        let counts = GlobalAgentCounts {
            blocked: 2,
            done: 1,
            working: 2,
            idle: 1,
        };
        let (shown, truncated) = fit_summary_segments(
            agent_summary_segments(counts, StatusIndicatorStyle::Dots),
            24,
        );
        let labels: Vec<&str> = shown.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(labels, vec!["◉ 2 blocked", "● 1 done"]);
        assert!(truncated);
    }

    #[test]
    fn agent_summary_keeps_all_segments_when_wide_enough() {
        let counts = GlobalAgentCounts {
            blocked: 2,
            done: 1,
            working: 2,
            idle: 1,
        };
        let (shown, truncated) = fit_summary_segments(
            agent_summary_segments(counts, StatusIndicatorStyle::Dots),
            60,
        );
        assert_eq!(shown.len(), 4);
        assert!(!truncated);
    }

    #[test]
    fn agent_summary_reports_no_agents_when_empty() {
        assert_eq!(
            agent_summary_segments(GlobalAgentCounts::default(), StatusIndicatorStyle::Dots),
            vec![("no agents".to_string(), SummaryTone::Muted)]
        );
    }

    #[test]
    fn switcher_uses_spaces_tree_before_tabs_and_menu() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = crate::workspace::Workspace::test_new("agents-first");
        workspace.test_add_tab(None); // two tabs -> two agent panes
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        for terminal in app.terminals.values_mut() {
            terminal.agent_name = Some("pi".to_string());
            terminal.state = AgentState::Working;
        }
        app.active = Some(0);
        app.selected = 0;
        app.view.mobile_header_rect = Rect::new(0, 0, 40, 2);
        app.view.terminal_area = Rect::new(0, 2, 40, 18);

        assert_eq!(agent_panel_entries(&app).len(), 2);
        // Spaces title + new-workspace action precede the workspace, followed
        // immediately by its two disclosed single-line tab rows.
        assert_eq!(
            mobile_switcher_workspace_doc_range(&app, 0)
                .expect("workspace row")
                .start,
            2
        );

        let viewport = mobile_switcher_areas(&app).viewport;
        let workspace_hit = mobile_switcher_target_at(&app, viewport.x + 2, viewport.y + 2);
        assert_eq!(workspace_hit, Some(MobileSwitcherTarget::Workspace(0)));
        assert_eq!(
            mobile_switcher_target_at(&app, viewport.x + 2, viewport.y + 3),
            Some(MobileSwitcherTarget::SidebarTab {
                ws_idx: 0,
                tab_idx: 0
            })
        );
        assert_eq!(
            mobile_switcher_target_at(&app, viewport.x + 2, viewport.y + 4),
            Some(MobileSwitcherTarget::SidebarTab {
                ws_idx: 0,
                tab_idx: 1
            })
        );
    }

    #[test]
    fn review_findings_mobile_workspace_name_reserves_disclosure_columns() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new(
            "workspace-name-that-is-much-too-long",
        )];
        app.ensure_test_terminals();
        for terminal in app.terminals.values_mut() {
            terminal.agent_name = Some("pi".to_string());
            terminal.state = AgentState::Working;
        }
        app.active = Some(0);
        app.selected = 0;
        app.mode = crate::app::Mode::Navigate;
        app.view.layout = crate::app::state::ViewLayout::Mobile;
        app.view.mobile_header_rect = Rect::new(0, 0, 24, 2);
        app.view.terminal_area = Rect::new(0, 2, 24, 16);

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(24, 18)).unwrap();
        terminal
            .draw(|frame| {
                render_mobile_panel(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, 24, 18),
                )
            })
            .unwrap();

        let viewport = mobile_switcher_areas(&app).viewport;
        let content = inset_for_left_scrollbar(viewport);
        let columns = mobile_space_disclosure_columns(content).unwrap();
        let disclosure_start = columns.start;
        let row = viewport.y + mobile_switcher_workspace_doc_range(&app, 0).unwrap().start as u16;
        let disclosure = columns
            .map(|x| terminal.backend().buffer()[(x, row)].symbol())
            .collect::<String>();
        assert_eq!(disclosure, "▾");
        assert_eq!(
            mobile_switcher_target_at(&app, disclosure_start, row),
            Some(MobileSwitcherTarget::WorkspaceDisclosure(0))
        );
    }

    #[test]
    fn mobile_disclosure_stays_top_left_in_narrow_layout() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("narrow")];
        app.ensure_test_terminals();
        for terminal in app.terminals.values_mut() {
            terminal.agent_name = Some("pi".to_string());
            terminal.state = AgentState::Working;
        }
        app.active = Some(0);
        app.selected = 0;
        app.mode = crate::app::Mode::Navigate;
        app.view.layout = crate::app::state::ViewLayout::Mobile;
        app.view.mobile_header_rect = Rect::new(0, 0, 6, 2);
        app.view.terminal_area = Rect::new(0, 2, 6, 8);

        let viewport = mobile_switcher_areas(&app).viewport;
        let content = inset_for_left_scrollbar(viewport);
        assert_eq!(content.width, 5);
        let columns = mobile_space_disclosure_columns(content).unwrap();
        assert_eq!(columns.start, content.x + 2);

        let row = viewport.y + mobile_switcher_workspace_doc_range(&app, 0).unwrap().start as u16;
        assert_eq!(
            mobile_switcher_target_at(&app, columns.start, row),
            Some(MobileSwitcherTarget::WorkspaceDisclosure(0))
        );
        assert_eq!(
            mobile_switcher_target_at(&app, content.x + content.width - 1, row),
            Some(MobileSwitcherTarget::Workspace(0))
        );
    }

    fn worktree_workspace(name: &str, key: &str, linked: bool) -> crate::workspace::Workspace {
        let mut ws = crate::workspace::Workspace::test_new(name);
        ws.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: key.into(),
            label: "herdr".into(),
            repo_root: std::path::PathBuf::from("/repo/herdr"),
            checkout_path: std::path::PathBuf::from(format!("/repo/{name}")),
            is_linked_worktree: linked,
        });
        ws
    }

    #[test]
    fn switcher_spaces_follow_grouped_worktree_order() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![
            worktree_workspace("main", "repo-key", false),
            crate::workspace::Workspace::test_new("other"),
            worktree_workspace("feature", "repo-key", true),
        ];
        app.active = Some(0);
        app.selected = 0;
        app.ensure_test_terminals();
        app.reconcile_sidebar_presentation();
        app.view.mobile_header_rect = Rect::new(0, 0, 40, 2);
        app.view.terminal_area = Rect::new(0, 2, 40, 18);

        // Linked worktrees no longer create an intermediate Space row. Their
        // windows remain direct children of the root Space.
        assert_eq!(
            mobile_switcher_workspace_doc_range(&app, 2)
                .expect("linked worktree window row")
                .start,
            4
        );
        assert_eq!(
            mobile_switcher_workspace_doc_range(&app, 1)
                .expect("workspace row")
                .start,
            5
        );

        let viewport = mobile_switcher_areas(&app).viewport;
        let hit = mobile_switcher_target_at(&app, viewport.x + 2, viewport.y + 4);
        assert_eq!(
            hit,
            Some(MobileSwitcherTarget::SidebarTab {
                ws_idx: 2,
                tab_idx: 0
            })
        );

        // Legacy worktree-group collapse does not restore an intermediate row.
        app.collapsed_space_keys.insert("repo-key".to_string());
        assert_eq!(
            mobile_switcher_workspace_doc_range(&app, 2)
                .expect("linked worktree window row")
                .start,
            4
        );
        let hit = mobile_switcher_target_at(&app, viewport.x + 2, viewport.y + 4);
        assert!(matches!(
            hit,
            Some(MobileSwitcherTarget::SidebarTab {
                ws_idx: 2,
                tab_idx: 0
            })
        ));
    }

    #[test]
    fn switcher_without_agents_keeps_spaces_first() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("shell-only")];
        app.active = Some(0);
        app.selected = 0;

        // No attached terminals -> no agents -> no agents header, spaces lead.
        assert_eq!(agent_panel_entries(&app).len(), 0);
        assert_eq!(
            mobile_switcher_workspace_doc_range(&app, 0)
                .expect("workspace row")
                .start,
            2
        );
    }

    #[test]
    fn mobile_agent_detail_keeps_tab_title_owned_by_the_tab_row() {
        let entry = agent_entry(Some("mobile-state"), Some("pi"));

        assert_eq!(mobile_agent_detail(&entry), "  done · pi");
    }

    #[test]
    fn mobile_agent_detail_keeps_existing_compact_detail_without_tab_context() {
        let entry = agent_entry(None, Some("pi"));

        assert_eq!(mobile_agent_detail(&entry), "  done · pi");
    }

    #[test]
    fn mobile_agent_detail_keeps_completed_idle_panes_done_after_viewing() {
        let mut entry = agent_entry(None, Some("pi"));
        entry.seen = true;
        entry.state_labels.clear();

        assert_eq!(mobile_agent_detail(&entry), "  done · pi");
    }

    #[test]
    fn mobile_item_titles_distinguish_selection_without_background_weight() {
        let palette = Palette::catppuccin();
        let inactive = mobile_item_title_style(false, false, &palette);
        let selected = mobile_item_title_style(true, false, &palette);

        assert_eq!(inactive.fg, Some(palette.subtext0));
        assert!(!inactive.add_modifier.contains(Modifier::BOLD));
        assert_eq!(
            selected.fg,
            Some(crate::ui::sidebar::active_sidebar_title_color(&palette))
        );
        assert!(selected.add_modifier.contains(Modifier::BOLD));
        assert_eq!(mobile_item_bg(true, false, &palette), palette.panel_bg);
    }

    #[test]
    fn mobile_tab_status_uses_compact_tab_label_and_position() {
        let mut workspace = crate::workspace::Workspace::test_new("mobile-tabs");
        let removed_tab = workspace.test_add_tab(None);
        workspace.test_add_tab(None);
        assert!(workspace.close_tab(removed_tab));
        workspace.active_tab = 1;

        assert_eq!(
            mobile_tab_status(&workspace, &Default::default(), 40),
            "tab 2 · 2/2"
        );
    }

    #[test]
    fn ac1_mobile_tab_status_uses_context_order_and_narrow_ticket_fallback() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("mobile-tabs")];
        app.ensure_test_terminals();
        let pane = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].terminal_id(pane).cloned().unwrap();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.agent_name = Some("Claude".into());
        terminal
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                ticket_ids: Some(vec!["SCA-42".into()]),
                work_title: Some("repair login regression".into()),
                ..Default::default()
            })
            .unwrap();

        assert_eq!(
            mobile_tab_status(&app.workspaces[0], &app.terminals, 80),
            "tab Claude · SCA-42 · repair login regression"
        );
        assert_eq!(
            mobile_tab_status(&app.workspaces[0], &app.terminals, 10),
            "tab SCA-42"
        );
    }

    #[test]
    fn mobile_switcher_uses_safe_default_for_auto_tab_titles() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = crate::workspace::Workspace::test_new("mobile-tabs");
        let removed_tab = workspace.test_add_tab(None);
        workspace.test_add_tab(None);
        assert!(workspace.close_tab(removed_tab));
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.selected = 0;
        app.view.mobile_header_rect = Rect::new(0, 0, 40, 2);
        app.view.terminal_area = Rect::new(0, 2, 40, 18);

        let backend = ratatui::backend::TestBackend::new(40, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_mobile_panel(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, 40, 20),
                )
            })
            .unwrap();

        let rows = (0..20)
            .map(|y| {
                (0..40)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let tab_row = rows
            .iter()
            .find(|row| row.contains("New Thread"))
            .expect("explicit mobile tab row");

        assert!(!tab_row.contains("tab 2"), "mobile tab row: {tab_row:?}");
    }

    #[test]
    fn ac1_ac2_ac3_mobile_tabs_are_status_first_single_line_rows() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = crate::workspace::Workspace::test_new("mobile-tabs");
        workspace.tabs[0].custom_name = Some("First task".into());
        workspace.test_add_tab(Some("Second task"));
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let observed_at = std::time::Instant::now();
        for terminal in app.terminals.values_mut() {
            terminal.set_detected_state_with_screen_signals_at(
                Some(crate::detect::Agent::Codex),
                AgentState::Working,
                false,
                false,
                true,
                false,
                observed_at,
            );
        }
        let first_pane = app.workspaces[0].tabs[0].root_pane;
        let first_terminal = app.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&first_terminal)
            .unwrap()
            .background_job_count = Some(2);
        app.active = Some(0);
        app.selected = 0;
        app.view.mobile_header_rect = Rect::new(0, 0, 40, 2);
        app.view.terminal_area = Rect::new(0, 2, 40, 18);
        app.reconcile_sidebar_presentation();

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 20)).unwrap();
        terminal
            .draw(|frame| {
                render_mobile_panel(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, 40, 20),
                )
            })
            .unwrap();

        let rows = (0..20)
            .map(|y| {
                (0..40)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let first = rows
            .iter()
            .position(|row| row.contains("working") && row.contains("Fir… · cx  2 >_"))
            .unwrap_or_else(|| panic!("missing status-first First task row: {rows:?}"));
        let second = rows
            .iter()
            .position(|row| row.contains("working") && row.contains("Second ta… · cx"))
            .unwrap_or_else(|| panic!("missing status-first Second task row: {rows:?}"));
        assert!(rows[first].find("working").unwrap() < rows[first].find("Fir…").unwrap());
        assert!(
            rows[first].contains("Fir… · cx  2 >_"),
            "mobile tab row must place the provider suffix and badge after its title: {:?}",
            rows[first]
        );
        assert!(
            rows[second].contains("Second ta… · cx"),
            "mobile tab row must show the provider suffix: {:?}",
            rows[second]
        );
        assert!(rows[second].find("working").unwrap() < rows[second].find("Second ta…").unwrap());
        assert_eq!(
            second,
            first + 1,
            "tab rows must not reserve subtitle lines"
        );
        assert!(!rows[first].contains("codex"));
        assert!(
            rows.iter().any(|row| row.contains("▾ mobile-tabs (2)")),
            "mobile Space row must use disclosure, title, and window count: {rows:?}"
        );
        assert!(
            rows.iter().all(|row| !row.contains("shell · tab")),
            "mobile Space row must not render a branch subtitle: {rows:?}"
        );
    }

    #[test]
    fn mobile_tab_rows_follow_field_priority_at_minimum_and_normal_widths() {
        let started = std::time::Instant::now();
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = crate::workspace::Workspace::test_new("mobile-tabs");
        workspace.tabs[0].custom_name = Some("Investigate release regression".into());
        let pane = workspace.tabs[0].root_pane;
        let terminal_id = workspace.tabs[0].panes[&pane].attached_terminal_id.clone();
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_state = app.terminals.get_mut(&terminal_id).unwrap();
        terminal_state.set_detected_state_with_screen_signals_at(
            Some(crate::detect::Agent::Codex),
            AgentState::Working,
            false,
            false,
            true,
            false,
            started,
        );
        terminal_state.background_job_count = Some(2);
        app.view_observed_at = started + std::time::Duration::from_secs(65);
        app.active = Some(0);
        app.selected = 0;
        app.reconcile_sidebar_presentation();

        for width in [18, 40] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 20)).unwrap();
            terminal
                .draw(|frame| {
                    render_mobile_switcher_content(
                        &app,
                        &TerminalRuntimeRegistry::new(),
                        frame,
                        Rect::new(0, 0, width, 20),
                    )
                })
                .unwrap();
            let rows = (0..20)
                .map(|y| {
                    (0..width)
                        .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>();
            let rendered = rows
                .iter()
                .find(|row| row.contains(" · cx"))
                .unwrap_or_else(|| panic!("missing provider row at {width}: {rows:?}"));

            assert!(rendered.contains('●'), "{width}: {rendered:?}");
            let dot = rendered.find('●').unwrap();
            let suffix = rendered.find(" · cx").unwrap();
            assert!(suffix > dot + '●'.len_utf8() + 1, "{width}: {rendered:?}");
            if width == 18 {
                assert!(!rendered.contains("working"), "{rendered:?}");
                assert!(!rendered.contains(">_"), "{rendered:?}");
                assert!(!rendered.contains("ago"), "{rendered:?}");
            } else {
                assert!(rendered.contains("working"), "{rendered:?}");
                assert!(rendered.contains("· cx  2 >_"), "{rendered:?}");
                assert!(rendered.ends_with("1m ago"), "{rendered:?}");
            }
        }
    }

    #[test]
    fn mobile_activity_deadlines_follow_visible_age_fields() {
        let started = std::time::Instant::now() - std::time::Duration::from_secs(65);
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = crate::workspace::Workspace::test_new("mobile-tabs");
        workspace.tabs[0].custom_name = Some("Investigate release regression".into());
        let pane = workspace.tabs[0].root_pane;
        let terminal_id = workspace.tabs[0].panes[&pane].attached_terminal_id.clone();
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state_with_screen_signals_at(
                Some(crate::detect::Agent::Codex),
                AgentState::Working,
                false,
                false,
                true,
                false,
                started,
            );
        app.active = Some(0);
        app.selected = 0;
        app.mode = crate::app::Mode::Navigate;
        app.status_bar_enabled = false;
        app.mobile_width_threshold = 80;
        let runtimes = TerminalRuntimeRegistry::new();

        crate::ui::compute_view_with_runtime_registry(&mut app, &runtimes, Rect::new(0, 0, 40, 20));
        assert_eq!(app.view.visible_agent_activity_instants, vec![started]);

        crate::ui::compute_view_with_runtime_registry(&mut app, &runtimes, Rect::new(0, 0, 18, 20));
        assert!(app.view.visible_agent_activity_instants.is_empty());

        crate::ui::compute_view_with_runtime_registry(&mut app, &runtimes, Rect::new(0, 0, 40, 4));
        assert!(app.view.visible_agent_activity_instants.is_empty());

        app.mode = crate::app::Mode::Terminal;
        crate::ui::compute_view_with_runtime_registry(&mut app, &runtimes, Rect::new(0, 0, 40, 20));
        assert!(app.view.visible_agent_activity_instants.is_empty());
    }

    #[test]
    fn seen_idle_mobile_tab_omits_status_segment() {
        let started = std::time::Instant::now();
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = crate::workspace::Workspace::test_new("one");
        workspace.tabs[0].custom_name = Some("Review release".into());
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.selected = 0;
        app.view.mobile_header_rect = Rect::new(0, 0, 40, 2);
        app.view.terminal_area = Rect::new(0, 2, 40, 10);
        app.reconcile_sidebar_presentation();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state_with_screen_signals_at(
                Some(crate::detect::Agent::Pi),
                crate::detect::AgentState::Working,
                false,
                false,
                true,
                false,
                started,
            );
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state_with_screen_signals_at(
                Some(crate::detect::Agent::Pi),
                crate::detect::AgentState::Idle,
                false,
                true,
                false,
                false,
                started + std::time::Duration::from_secs(1),
            );
        app.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .unwrap()
            .seen = true;

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 12)).unwrap();
        terminal
            .draw(|frame| {
                render_mobile_panel(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, 40, 12),
                )
            })
            .unwrap();
        let rows = (0..12)
            .map(|y| {
                (0..40)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let row = rows
            .iter()
            .find(|row| row.contains("Review release"))
            .unwrap_or_else(|| panic!("missing title row: {rows:?}"));

        assert!(!row.contains("idle"), "{row:?}");
        assert!(!row.contains("done"), "{row:?}");
        assert!(!row.contains(" · Review release"), "{row:?}");
    }

    #[test]
    fn unseen_agentless_mobile_tab_omits_lifecycle_status() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = crate::workspace::Workspace::test_new("one");
        workspace.tabs[0].custom_name = Some("Agent task".into());
        workspace.test_add_tab(Some("Agentless window"));
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let first_pane = app.workspaces[0].tabs[0].root_pane;
        let first_terminal = app.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&first_terminal)
            .unwrap()
            .detected_agent = Some(crate::detect::Agent::Codex);
        app.active = Some(0);
        app.selected = 0;
        app.view.mobile_header_rect = Rect::new(0, 0, 40, 2);
        app.view.terminal_area = Rect::new(0, 2, 40, 10);
        app.reconcile_sidebar_presentation();

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 12)).unwrap();
        terminal
            .draw(|frame| {
                render_mobile_panel(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, 40, 12),
                )
            })
            .unwrap();
        let rendered = (0..12)
            .map(|y| {
                (0..40)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .find(|row| row.contains("Agentless window"))
            .expect("agentless mobile tab row");

        for lifecycle in ["idle", "done", "working", "blocked", "unknown"] {
            assert!(!rendered.contains(lifecycle), "{rendered:?}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn mobile_header_uses_live_root_runtime_cwd_for_workspace_label() {
        let unique = format!(
            "herdr-mobile-header-runtime-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let stale_cwd = root.join("issue-264-nix-support");
        let live_cwd = root.join("herdr");
        std::fs::create_dir_all(stale_cwd.join(".git")).unwrap();
        std::fs::create_dir_all(live_cwd.join(".git")).unwrap();

        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = crate::workspace::Workspace::test_new("stale-name");
        workspace.custom_name = None;
        workspace.identity_cwd = stale_cwd.clone();
        let pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&terminal_id).unwrap().cwd = stale_cwd;
        app.active = Some(0);
        app.selected = 0;
        app.view.mobile_menu_hit_area = Rect::new(30, 0, 10, 2);

        let (events, _) = tokio::sync::mpsc::channel(4);
        let runtime = crate::terminal::TerminalRuntime::spawn(
            pane,
            24,
            80,
            live_cwd.clone(),
            0,
            crate::terminal_theme::TerminalTheme::default(),
            None,
            crate::pane::PaneShellConfig::new("/bin/sh", crate::config::ShellModeConfig::NonLogin),
            &crate::pane::PaneLaunchEnv::default(),
            events,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(crate::render_signal::RenderSignal::new()),
        )
        .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while runtime.cwd() != Some(live_cwd.clone()) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let mut runtime_registry = TerminalRuntimeRegistry::new();
        runtime_registry.insert(terminal_id, runtime);
        let backend = ratatui::backend::TestBackend::new(40, 2);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_mobile_header(&app, &runtime_registry, frame, Rect::new(0, 0, 40, 2))
            })
            .unwrap();
        let row = (0..40)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect::<String>();

        for (_, runtime) in runtime_registry.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_dir_all(root);

        assert!(row.contains("herdr"), "header row: {row:?}");
        assert!(
            !row.contains("issue-264-nix-support"),
            "header row: {row:?}"
        );
    }
}
