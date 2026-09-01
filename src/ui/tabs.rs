use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    widgets::Paragraph,
    Frame,
};

use super::text::{display_width, display_width_u16, truncate_end};
use super::widgets::panel_contrast_fg;
use crate::app::AppState;

/// The pin lives in the leading pad column every tab cell already renders, so
/// the affordance costs no width and never touches the label: a pinned tab
/// shows `*` where an unpinned one shows the blank that was always there. The
/// click target is that column either way -- an icon you can only hit once it
/// is already on would be no way to turn it on.
pub(crate) const PIN_GLYPH_ON: char = '*';
pub(crate) const PIN_GLYPH_OFF: char = ' ';

const MIN_TAB_WIDTH: u16 = 8;
const NEW_TAB_WIDTH: u16 = 3;
const TAB_SCROLL_BUTTON_WIDTH: u16 = 3;
const ZOOM_INDICATOR: &str = "ZOOM";
// The narrowest overflowing tab strip worth keeping interactive: one
// minimum-width tab, both scroll controls, and the new-tab control.
const MIN_TAB_STRIP_WIDTH: u16 =
    MIN_TAB_WIDTH + NEW_TAB_WIDTH + TAB_SCROLL_BUTTON_WIDTH.saturating_mul(2);

#[derive(Debug, Clone, Default)]
pub(crate) struct TabBarView {
    pub scroll: usize,
    pub tab_hit_areas: Vec<Rect>,
    pub scroll_left_hit_area: Rect,
    pub scroll_right_hit_area: Rect,
    pub new_tab_hit_area: Rect,
}

fn tab_width(
    ws: &crate::workspace::Workspace,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    tab_idx: usize,
) -> u16 {
    display_width_u16(&tab_chrome_label(ws, terminals, tab_idx, u16::MAX as usize))
        .saturating_add(4)
        .max(MIN_TAB_WIDTH)
}

pub(crate) fn fit_tab_display_projection(
    projection: crate::workspace::TabDisplayProjection,
    max_width: usize,
) -> String {
    use crate::workspace::TabDisplayProjection;

    let candidates = match projection {
        TabDisplayProjection::Manual(name) | TabDisplayProjection::Fallback(name) => vec![name],
        TabDisplayProjection::Derived {
            agent,
            ticket,
            binding,
            title,
        } => {
            let full = [&ticket, &binding, &title]
                .into_iter()
                .filter_map(|part| part.as_deref())
                .collect::<Vec<_>>()
                .join(" · ");
            let mut candidates = Vec::new();
            if !full.is_empty() {
                candidates.push(full);
            }
            if let Some(ticket) = ticket {
                if !candidates.contains(&ticket) {
                    candidates.push(ticket);
                }
            }
            if let Some(binding) = binding {
                if !candidates.contains(&binding) {
                    candidates.push(binding);
                }
            }
            if let Some(title) = title {
                if !candidates.contains(&title) {
                    candidates.push(title);
                }
            }
            if let Some(agent) = agent {
                if !candidates.contains(&agent) {
                    candidates.push(agent);
                }
            }
            candidates
        }
    };
    candidates
        .iter()
        .find(|candidate| display_width(candidate) <= max_width)
        .cloned()
        .unwrap_or_else(|| {
            truncate_end(
                candidates.last().map(String::as_str).unwrap_or(""),
                max_width,
            )
        })
}

pub(crate) fn tab_chrome_label(
    ws: &crate::workspace::Workspace,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    tab_idx: usize,
    max_width: usize,
) -> String {
    let zoomed = ws.tabs.get(tab_idx).is_some_and(|tab| tab.zoomed);
    // Same budgeting rule as the zoom marker: the glyph is paid for out of the
    // label, never out of the cell, so a pin can never widen or shove a tab.
    let label_width = max_width.saturating_sub(if zoomed { 2 } else { 0 });
    let name = ws
        .tab_display_projection(terminals, tab_idx)
        .map(|projection| fit_tab_display_projection(projection, label_width))
        .unwrap_or_else(|| truncate_end(&(tab_idx + 1).to_string(), label_width));
    if zoomed {
        format!("{name} Z")
    } else {
        name
    }
}

#[derive(Clone, Copy)]
struct VisibleStatusSegment<'a> {
    text: &'a str,
    accent: bool,
}

fn visible_status_segments(app: &AppState) -> Vec<VisibleStatusSegment<'_>> {
    let zoomed = app
        .active
        .and_then(|index| app.workspaces.get(index))
        .is_some_and(|workspace| workspace.zoomed);
    app.tab_bar_right
        .iter()
        .filter_map(|segment| match segment {
            crate::app::state::TabBarStatusSegment::Zoom if zoomed => Some(VisibleStatusSegment {
                text: ZOOM_INDICATOR,
                accent: true,
            }),
            crate::app::state::TabBarStatusSegment::Text(Some(text))
                if display_width_u16(text) > 0 =>
            {
                Some(VisibleStatusSegment {
                    text,
                    accent: false,
                })
            }
            crate::app::state::TabBarStatusSegment::Zoom
            | crate::app::state::TabBarStatusSegment::Text(_) => None,
        })
        .collect()
}

fn tab_bar_status_width(app: &AppState) -> u16 {
    let segments = visible_status_segments(app);
    let content_width = segments.iter().fold(0_u16, |width, segment| {
        width.saturating_add(display_width_u16(segment.text))
    });
    let separators = u16::try_from(segments.len().saturating_sub(1)).unwrap_or(u16::MAX);
    content_width
        .saturating_add(display_width_u16(&app.tab_bar_right_separator).saturating_mul(separators))
}

fn tab_bar_status_area(app: &AppState, area: Rect) -> Option<Rect> {
    let width = tab_bar_status_width(app);
    if width == 0 {
        return None;
    }
    let reserved = width.saturating_add(1);
    (area.width.saturating_sub(reserved) >= MIN_TAB_STRIP_WIDTH)
        .then(|| Rect::new(area.x + area.width.saturating_sub(width), area.y, width, 1))
}

pub(crate) fn tab_bar_content_area(app: &AppState, area: Rect) -> Rect {
    let reserved = tab_bar_status_area(app, area)
        .map(|status| status.width.saturating_add(1))
        .unwrap_or(0);
    Rect {
        width: area.width.saturating_sub(reserved),
        ..area
    }
}

fn layout_tab_hit_areas(
    ws: &crate::workspace::Workspace,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    area: Rect,
    scroll: usize,
) -> Vec<Rect> {
    let mut rects = vec![Rect::default(); ws.tabs.len()];
    if area.width == 0 || area.height == 0 {
        return rects;
    }

    let mut x = area.x;
    let right = area.x + area.width;
    for (idx, rect) in rects.iter_mut().enumerate().skip(scroll) {
        if x >= right {
            break;
        }
        let desired = tab_width(ws, terminals, idx);
        let remaining = right.saturating_sub(x);
        let width = desired.min(remaining).max(1);
        *rect = Rect::new(x, area.y, width, 1);
        x = x.saturating_add(width + 1);
    }
    rects
}

fn centered_tab_scroll(
    ws: &crate::workspace::Workspace,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    area: Rect,
) -> usize {
    let mut best_scroll = ws.active_tab;
    let mut best_distance = u16::MAX;
    let viewport_center = area.x.saturating_mul(2).saturating_add(area.width);

    for scroll in 0..=ws.active_tab {
        let rects = layout_tab_hit_areas(ws, terminals, area, scroll);
        let Some(active_rect) = rects.get(ws.active_tab).copied() else {
            continue;
        };
        if active_rect.width == 0 {
            continue;
        }

        let active_center = active_rect
            .x
            .saturating_mul(2)
            .saturating_add(active_rect.width);
        let distance = active_center.abs_diff(viewport_center);
        if distance <= best_distance {
            best_distance = distance;
            best_scroll = scroll;
        }
    }

    best_scroll
}

fn trailing_tab_controls_x(tab_hit_areas: &[Rect], fallback_x: u16) -> u16 {
    tab_hit_areas
        .iter()
        .rev()
        .find(|rect| rect.width > 0)
        .map(|rect| rect.x + rect.width)
        .unwrap_or(fallback_x)
}

fn max_tab_scroll(
    ws: &crate::workspace::Workspace,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    area: Rect,
) -> usize {
    (0..ws.tabs.len())
        .find(|&scroll| {
            layout_tab_hit_areas(ws, terminals, area, scroll)
                .last()
                .is_some_and(|rect| rect.width > 0)
        })
        .unwrap_or(0)
}

pub(crate) fn compute_tab_bar_view(
    ws: &crate::workspace::Workspace,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    area: Rect,
    current_scroll: usize,
    follow_active: bool,
    mouse_chrome: bool,
) -> TabBarView {
    if area.width == 0 || area.height == 0 {
        return TabBarView::default();
    }

    if !mouse_chrome {
        let max_scroll = max_tab_scroll(ws, terminals, area);
        let scroll = if follow_active {
            centered_tab_scroll(ws, terminals, area).min(max_scroll)
        } else {
            current_scroll.min(max_scroll)
        };
        return TabBarView {
            scroll,
            tab_hit_areas: layout_tab_hit_areas(ws, terminals, area, scroll),
            scroll_left_hit_area: Rect::default(),
            scroll_right_hit_area: Rect::default(),
            new_tab_hit_area: Rect::default(),
        };
    }

    let area_right = area.x + area.width;
    let all_tabs_area = Rect::new(
        area.x,
        area.y,
        area.width.saturating_sub(NEW_TAB_WIDTH),
        area.height,
    );
    let all_tabs = layout_tab_hit_areas(ws, terminals, all_tabs_area, 0);
    let overflow = all_tabs.iter().any(|rect| rect.width == 0);
    if !overflow {
        let new_tab_x = trailing_tab_controls_x(&all_tabs, area.x);
        let new_tab_hit_area = Rect::new(
            new_tab_x,
            area.y,
            area_right.saturating_sub(new_tab_x).min(NEW_TAB_WIDTH),
            1,
        );
        return TabBarView {
            scroll: 0,
            tab_hit_areas: all_tabs,
            scroll_left_hit_area: Rect::default(),
            scroll_right_hit_area: Rect::default(),
            new_tab_hit_area,
        };
    }

    let left_hit_area = Rect::new(area.x, area.y, TAB_SCROLL_BUTTON_WIDTH.min(area.width), 1);
    let tab_area_x = left_hit_area.x + left_hit_area.width;
    let reserved_trailing_width = NEW_TAB_WIDTH.saturating_add(TAB_SCROLL_BUTTON_WIDTH);
    let tab_area_right = area_right.saturating_sub(reserved_trailing_width);
    let tab_area = Rect::new(
        tab_area_x,
        area.y,
        tab_area_right.saturating_sub(tab_area_x),
        area.height,
    );

    let max_scroll = max_tab_scroll(ws, terminals, tab_area);
    let scroll = if follow_active {
        centered_tab_scroll(ws, terminals, tab_area).min(max_scroll)
    } else {
        current_scroll.min(max_scroll)
    };
    let tab_hit_areas = layout_tab_hit_areas(ws, terminals, tab_area, scroll);
    let trailing_x = trailing_tab_controls_x(&tab_hit_areas, tab_area_x).min(tab_area_right);
    let right_hit_area = Rect::new(
        trailing_x,
        area.y,
        area_right
            .saturating_sub(trailing_x)
            .min(TAB_SCROLL_BUTTON_WIDTH),
        1,
    );
    let new_tab_x = right_hit_area.x + right_hit_area.width;
    let new_tab_hit_area = Rect::new(
        new_tab_x,
        area.y,
        area_right.saturating_sub(new_tab_x).min(NEW_TAB_WIDTH),
        1,
    );

    TabBarView {
        scroll,
        tab_hit_areas,
        scroll_left_hit_area: left_hit_area,
        scroll_right_hit_area: right_hit_area,
        new_tab_hit_area,
    }
}

fn tab_drop_indicator_x(
    app: &AppState,
    ws: &crate::workspace::Workspace,
    insert_idx: usize,
) -> Option<u16> {
    let mut visible_tabs = app
        .view
        .tab_hit_areas
        .iter()
        .enumerate()
        .filter(|(_, rect)| rect.width > 0);
    let first_visible = visible_tabs.clone().next()?;
    let last_visible = visible_tabs.next_back().unwrap_or(first_visible);

    if insert_idx == 0 {
        return Some(if first_visible.0 == 0 {
            first_visible.1.x
        } else {
            app.view.tab_scroll_left_hit_area.x + app.view.tab_scroll_left_hit_area.width
        });
    }

    if let Some((_, rect)) = app
        .view
        .tab_hit_areas
        .iter()
        .enumerate()
        .find(|(idx, rect)| *idx == insert_idx && rect.width > 0)
    {
        return Some(rect.x.saturating_sub(1));
    }

    if insert_idx >= ws.tabs.len() {
        return Some(if last_visible.0 + 1 >= ws.tabs.len() {
            last_visible.1.x + last_visible.1.width
        } else {
            app.view.tab_scroll_right_hit_area.x.saturating_sub(1)
        });
    }

    None
}

pub(super) fn render_tab_bar(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(active_ws_idx) = app.active else {
        return;
    };
    let Some(ws) = app.workspaces.get(active_ws_idx) else {
        return;
    };
    let p = &app.palette;

    frame.render_widget(
        Paragraph::new(" ".repeat(area.width as usize)).style(Style::default().bg(p.panel_bg)),
        area,
    );

    let first_visible_idx = app
        .view
        .tab_hit_areas
        .iter()
        .enumerate()
        .find(|(_, rect)| rect.width > 0)
        .map(|(idx, _)| idx);
    let last_visible_idx = app
        .view
        .tab_hit_areas
        .iter()
        .enumerate()
        .rev()
        .find(|(_, rect)| rect.width > 0)
        .map(|(idx, _)| idx);
    let can_scroll_left = app.view.tab_scroll_left_hit_area.width > 0 && app.tab_scroll > 0;
    let can_scroll_right = app.view.tab_scroll_right_hit_area.width > 0
        && last_visible_idx.is_some_and(|idx| idx + 1 < ws.tabs.len());

    if app.mouse_capture && app.view.tab_scroll_left_hit_area.width > 0 {
        let style = if can_scroll_left {
            Style::default().fg(p.overlay1).bg(p.surface0)
        } else {
            Style::default()
                .fg(p.overlay0)
                .bg(p.surface0)
                .add_modifier(Modifier::DIM)
        };
        frame.render_widget(
            Paragraph::new(" < ").style(style),
            app.view.tab_scroll_left_hit_area,
        );
    }

    if app.mouse_capture && app.view.tab_scroll_right_hit_area.width > 0 {
        let style = if can_scroll_right {
            Style::default().fg(p.overlay1).bg(p.surface0)
        } else {
            Style::default()
                .fg(p.overlay0)
                .bg(p.surface0)
                .add_modifier(Modifier::DIM)
        };
        frame.render_widget(
            Paragraph::new(" > ").style(style),
            app.view.tab_scroll_right_hit_area,
        );
    }

    for (idx, tab) in ws.tabs.iter().enumerate() {
        let Some(rect) = app.view.tab_hit_areas.get(idx).copied() else {
            break;
        };
        if rect.width == 0 {
            continue;
        }
        let active = idx == ws.active_tab;
        let style = if active {
            let base = Style::default().fg(panel_contrast_fg(p)).bg(p.accent);
            if tab.is_auto_named() {
                base
            } else {
                base.add_modifier(Modifier::BOLD)
            }
        } else if tab.is_auto_named() {
            Style::default()
                .fg(p.overlay0)
                .bg(p.surface0)
                .add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(p.overlay1).bg(p.surface0)
        };
        let width = rect.width as usize;
        let name = tab_chrome_label(ws, &app.terminals, idx, width.saturating_sub(1));
        let pin = if tab.pinned {
            PIN_GLYPH_ON
        } else {
            PIN_GLYPH_OFF
        };
        let padding = width.saturating_sub(1).saturating_sub(display_width(&name));
        let left = padding / 2;
        let text = format!(
            "{pin}{left_pad}{name}{right_pad}",
            left_pad = " ".repeat(left),
            right_pad = " ".repeat(padding - left),
        );
        frame.render_widget(Paragraph::new(text).style(style), rect);
    }

    if let Some(crate::app::state::DragState {
        target:
            crate::app::state::DragTarget::TabReorder {
                ws_idx,
                insert_idx: Some(insert_idx),
                ..
            },
    }) = &app.drag
    {
        if *ws_idx == active_ws_idx {
            if let Some(x) = tab_drop_indicator_x(app, ws, *insert_idx) {
                frame.buffer_mut()[(x.min(area.x + area.width.saturating_sub(1)), area.y)]
                    .set_symbol("│")
                    .set_style(Style::default().fg(p.accent));
            }
        }
    }

    if app.mouse_capture && app.view.new_tab_hit_area.width > 0 {
        frame.render_widget(
            Paragraph::new(" + ").style(Style::default().fg(p.overlay1)),
            app.view.new_tab_hit_area,
        );
    }

    if first_visible_idx.is_some_and(|idx| idx > 0) {
        let x = if app.mouse_capture && app.view.tab_scroll_left_hit_area.width > 0 {
            app.view.tab_scroll_left_hit_area.x + app.view.tab_scroll_left_hit_area.width
        } else {
            area.x
        };
        if x < area.x + area.width {
            frame.buffer_mut()[(x, area.y)]
                .set_symbol("…")
                .set_style(Style::default().fg(p.overlay0));
        }
    }
    if last_visible_idx.is_some_and(|idx| idx + 1 < ws.tabs.len()) {
        let content = tab_bar_content_area(app, area);
        let content_right = content.x + content.width;
        let x = if app.mouse_capture && app.view.tab_scroll_right_hit_area.width > 0 {
            app.view.tab_scroll_right_hit_area.x.saturating_sub(1)
        } else {
            content_right.saturating_sub(1)
        };
        if x >= area.x && x < area.x + area.width {
            frame.buffer_mut()[(x, area.y)]
                .set_symbol("…")
                .set_style(Style::default().fg(p.overlay0));
        }
    }

    if let Some(status_area) = tab_bar_status_area(app, area) {
        let segments = visible_status_segments(app);
        let separator_width = display_width_u16(&app.tab_bar_right_separator);
        let mut x = status_area.x;
        for (index, segment) in segments.iter().enumerate() {
            if index > 0 && separator_width > 0 {
                let rect = Rect::new(x, area.y, separator_width, 1);
                frame.render_widget(
                    Paragraph::new(app.tab_bar_right_separator.as_str())
                        .style(Style::default().fg(p.overlay0).bg(p.panel_bg)),
                    rect,
                );
                x = x.saturating_add(separator_width);
            }

            let width = display_width_u16(segment.text);
            let rect = Rect::new(x, area.y, width, 1);
            let style = if segment.accent {
                Style::default()
                    .fg(panel_contrast_fg(p))
                    .bg(p.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(p.overlay1).bg(p.panel_bg)
            };
            frame.render_widget(Paragraph::new(segment.text).style(style), rect);
            x = x.saturating_add(width);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::AppState;
    use crate::workspace::Workspace;
    use ratatui::{backend::TestBackend, Terminal};

    fn buffer_row_text(buffer: &ratatui::buffer::Buffer, area: Rect, row: u16) -> String {
        (area.x..area.x + area.width)
            .map(|x| buffer[(x, row)].symbol())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn tab_bar_marks_zoomed_tabs_without_renaming_them() {
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].zoomed = true;
        let custom_tab = ws.test_add_tab(Some("test"));
        ws.tabs[custom_tab].zoomed = true;

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.view.tab_bar_rect = Rect::new(0, 0, 30, 1);
        let view = compute_tab_bar_view(
            &app.workspaces[0],
            &app.terminals,
            app.view.tab_bar_rect,
            0,
            true,
            false,
        );
        app.view.tab_hit_areas = view.tab_hit_areas;

        let backend = TestBackend::new(30, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tab_bar(&app, frame, app.view.tab_bar_rect))
            .unwrap();

        let row = buffer_row_text(terminal.backend().buffer(), app.view.tab_bar_rect, 0);
        assert!(row.contains(" 1 Z"), "tab row: {row:?}");
        assert!(row.contains(" test Z"), "tab row: {row:?}");
        assert_eq!(
            app.workspaces[0]
                .tab_display_projection(&app.terminals, 0)
                .map(|projection| projection.full_label())
                .as_deref(),
            Some("1")
        );
        assert_eq!(
            app.workspaces[0]
                .tab_display_projection(&app.terminals, custom_tab)
                .map(|projection| projection.full_label())
                .as_deref(),
            Some("test")
        );
    }

    #[test]
    fn tab_bar_renders_ordered_status_entries_with_separator() {
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].zoomed = true;
        app.tab_bar_right = vec![
            crate::app::state::TabBarStatusSegment::Zoom,
            crate::app::state::TabBarStatusSegment::Text(Some("wintermute".into())),
            crate::app::state::TabBarStatusSegment::Text(Some("14:30".into())),
        ];
        app.tab_bar_right_separator = " · ".into();

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.view.tab_bar_rect = Rect::new(0, 0, 60, 1);
        let content = tab_bar_content_area(&app, app.view.tab_bar_rect);
        let view =
            compute_tab_bar_view(&app.workspaces[0], &app.terminals, content, 0, true, false);
        app.view.tab_hit_areas = view.tab_hit_areas.clone();

        let backend = TestBackend::new(60, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tab_bar(&app, frame, app.view.tab_bar_rect))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let row = buffer_row_text(buffer, app.view.tab_bar_rect, 0);
        assert!(
            row.ends_with("ZOOM · wintermute · 14:30"),
            "tab row: {row:?}"
        );
        let status_x = 60 - display_width_u16("ZOOM · wintermute · 14:30");
        assert_eq!(buffer[(status_x, 0)].style().bg, Some(app.palette.accent));
        for rect in &view.tab_hit_areas {
            assert!(rect.x + rect.width <= content.x + content.width);
        }
    }

    #[test]
    fn hidden_status_entries_do_not_leave_dangling_separators() {
        let mut app = AppState::test_new();
        app.tab_bar_right = vec![
            crate::app::state::TabBarStatusSegment::Zoom,
            crate::app::state::TabBarStatusSegment::Text(None),
            crate::app::state::TabBarStatusSegment::Text(Some("wintermute".into())),
        ];
        app.tab_bar_right_separator = " | ".into();
        app.workspaces = vec![Workspace::test_new("test")];
        app.active = Some(0);
        app.view.tab_bar_rect = Rect::new(0, 0, 40, 1);
        let content = tab_bar_content_area(&app, app.view.tab_bar_rect);
        let view =
            compute_tab_bar_view(&app.workspaces[0], &app.terminals, content, 0, true, false);
        app.view.tab_hit_areas = view.tab_hit_areas;

        let backend = TestBackend::new(40, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tab_bar(&app, frame, app.view.tab_bar_rect))
            .unwrap();

        let row = buffer_row_text(terminal.backend().buffer(), app.view.tab_bar_rect, 0);
        assert!(row.ends_with("wintermute"), "tab row: {row:?}");
        assert!(!row.contains(" | "), "tab row: {row:?}");
    }

    #[test]
    fn status_reservation_keeps_a_minimum_width_tab_between_scroll_controls() {
        let mut app = AppState::test_new();
        app.tab_bar_right = vec![crate::app::state::TabBarStatusSegment::Text(Some(
            "x".into(),
        ))];
        let mut workspace = Workspace::test_new("test");
        workspace.test_add_tab(None);
        workspace.test_add_tab(None);
        app.workspaces = vec![workspace];
        app.active = Some(0);

        let too_narrow = Rect::new(0, 0, MIN_TAB_STRIP_WIDTH + 1, 1);
        assert_eq!(tab_bar_content_area(&app, too_narrow), too_narrow);

        let wide_enough = Rect::new(0, 0, MIN_TAB_STRIP_WIDTH + 2, 1);
        let content = tab_bar_content_area(&app, wide_enough);
        assert_eq!(content.width, MIN_TAB_STRIP_WIDTH);
        let view = compute_tab_bar_view(&app.workspaces[0], &app.terminals, content, 0, true, true);
        assert!(view.tab_hit_areas[0].width >= MIN_TAB_WIDTH);
    }

    #[test]
    fn combined_status_entries_yield_to_tab_controls_on_narrow_rows() {
        let mut app = AppState::test_new();
        app.tab_bar_right = vec![
            crate::app::state::TabBarStatusSegment::Text(Some(
                "a-hostname-wider-than-the-whole-bar".into(),
            )),
            crate::app::state::TabBarStatusSegment::Text(Some("14:30".into())),
        ];
        app.workspaces = vec![Workspace::test_new("test")];
        app.active = Some(0);
        app.view.tab_bar_rect = Rect::new(0, 0, 30, 1);

        assert_eq!(
            tab_bar_content_area(&app, app.view.tab_bar_rect),
            app.view.tab_bar_rect
        );
        assert_eq!(tab_bar_status_area(&app, app.view.tab_bar_rect), None);

        let view = compute_tab_bar_view(
            &app.workspaces[0],
            &app.terminals,
            tab_bar_content_area(&app, app.view.tab_bar_rect),
            0,
            true,
            true,
        );
        assert!(view.tab_hit_areas[0].width > 0);
        assert!(view.new_tab_hit_area.width > 0);
    }

    #[test]
    fn cjk_tab_labels_are_centered_by_display_width() {
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("提交 herdr 的反馈".into());

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.view.tab_bar_rect = Rect::new(0, 0, 30, 1);
        let view = compute_tab_bar_view(
            &app.workspaces[0],
            &app.terminals,
            app.view.tab_bar_rect,
            0,
            true,
            false,
        );
        app.view.tab_hit_areas = view.tab_hit_areas;

        let backend = TestBackend::new(30, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tab_bar(&app, frame, app.view.tab_bar_rect))
            .unwrap();

        // 17 display columns + 4 padding: two columns each side, wide glyphs
        // starting right after the left padding.
        let rect = app.view.tab_hit_areas[0];
        assert_eq!(rect.width, 21);
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(rect.x, rect.y)].symbol(), " ");
        assert_eq!(buffer[(rect.x + 1, rect.y)].symbol(), " ");
        assert_eq!(buffer[(rect.x + 2, rect.y)].symbol(), "提");
        assert_eq!(buffer[(rect.x + rect.width - 2, rect.y)].symbol(), " ");
        assert_eq!(buffer[(rect.x + rect.width - 1, rect.y)].symbol(), " ");
    }

    #[test]
    fn tab_labels_are_centered_in_their_cells() {
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("omarchy".into());

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.view.tab_bar_rect = Rect::new(0, 0, 30, 1);
        let view = compute_tab_bar_view(
            &app.workspaces[0],
            &app.terminals,
            app.view.tab_bar_rect,
            0,
            true,
            false,
        );
        app.view.tab_hit_areas = view.tab_hit_areas;

        let backend = TestBackend::new(30, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tab_bar(&app, frame, app.view.tab_bar_rect))
            .unwrap();

        let rect = app.view.tab_hit_areas[0];
        let buffer = terminal.backend().buffer();
        let cell: String = (rect.x..rect.x + rect.width)
            .map(|x| buffer[(x, rect.y)].symbol())
            .collect();
        assert_eq!(cell, "  omarchy  ");
    }

    #[test]
    fn active_auto_named_tab_keeps_readable_weight() {
        let mut app = AppState::test_new();
        let ws = Workspace::test_new("test");

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.view.tab_bar_rect = Rect::new(0, 0, 30, 1);
        let view = compute_tab_bar_view(
            &app.workspaces[0],
            &app.terminals,
            app.view.tab_bar_rect,
            0,
            true,
            false,
        );
        app.view.tab_hit_areas = view.tab_hit_areas;

        let backend = TestBackend::new(30, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tab_bar(&app, frame, app.view.tab_bar_rect))
            .unwrap();

        let tab_rect = app.view.tab_hit_areas[0];
        let style = terminal.backend().buffer()[(tab_rect.x + 1, tab_rect.y)].style();

        assert_eq!(style.bg, Some(app.palette.accent));
        assert!(!style.add_modifier.contains(Modifier::DIM));
        assert!(!style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn zoom_marker_counts_toward_tab_width() {
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("abcdefgh".into());
        ws.tabs[0].zoomed = true;

        assert_eq!(tab_width(&ws, &Default::default(), 0), 14);
    }

    #[test]
    fn tab_projection_prefers_detected_agent_title_and_degrades_by_width() {
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("test");
        let other = ws.test_split(ratatui::layout::Direction::Horizontal);
        ws.tabs[0].layout.focus_pane(other);
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.ensure_test_terminals();

        let root = app.workspaces[0].tabs[0].root_pane;
        let root_terminal = app.workspaces[0].terminal_id(root).cloned().unwrap();
        app.terminals
            .get_mut(&root_terminal)
            .unwrap()
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                ticket_ids: Some(vec!["SCA-1".into()]),
                work_title: Some("wrong pane".into()),
                ..Default::default()
            })
            .unwrap();

        let focused_terminal = app.workspaces[0].terminal_id(other).cloned().unwrap();
        let focused = app.terminals.get_mut(&focused_terminal).unwrap();
        focused.agent_name = Some("Claude".into());
        focused.detected_agent = Some(crate::detect::Agent::Claude);
        focused.set_terminal_title(Some("⠋ Add Subabe management token to Doppler".into()));
        focused
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                ticket_ids: Some(vec!["SCA-42".into()]),
                work_title: Some("Review If Context Correctly Enriched Herdr Plus".into()),
                ..Default::default()
            })
            .unwrap();

        assert_eq!(
            tab_chrome_label(&app.workspaces[0], &app.terminals, 0, 80),
            "SCA-42 · Add Subabe management token to Doppler"
        );
        assert_eq!(
            tab_chrome_label(&app.workspaces[0], &app.terminals, 0, 15),
            "SCA-42"
        );
        assert_eq!(
            tab_chrome_label(&app.workspaces[0], &app.terminals, 0, 8),
            "SCA-42"
        );

        app.workspaces[0].tabs[0].set_custom_name("手动名称".into());
        assert_eq!(
            tab_chrome_label(&app.workspaces[0], &app.terminals, 0, 7),
            "手动名…"
        );
    }

    #[test]
    fn fit_tab_display_projection_truncates_only_after_component_fallbacks() {
        use crate::workspace::TabDisplayProjection;

        let projection = TabDisplayProjection::Derived {
            agent: Some("Claude".into()),
            ticket: Some("SCA-42".into()),
            binding: None,
            title: Some("repair login regression".into()),
        };

        assert_eq!(
            fit_tab_display_projection(projection.clone(), 80),
            "SCA-42 · repair login regression"
        );
        assert_eq!(fit_tab_display_projection(projection.clone(), 8), "SCA-42");
        assert_eq!(fit_tab_display_projection(projection, 4), "Cla…");

        let narrow_projection = TabDisplayProjection::Derived {
            agent: Some("cx".into()),
            ticket: Some("LONG-TICKET".into()),
            binding: None,
            title: Some("task".into()),
        };
        assert_eq!(
            fit_tab_display_projection(narrow_projection.clone(), 4),
            "task"
        );
        assert_eq!(fit_tab_display_projection(narrow_projection, 2), "cx");
    }

    #[test]
    fn derived_projection_never_prefixes_the_agent_onto_a_title() {
        use crate::workspace::TabDisplayProjection;

        let titled = TabDisplayProjection::Derived {
            agent: Some("claude".into()),
            ticket: None,
            binding: None,
            title: Some("Create Python Script".into()),
        };
        assert_eq!(
            fit_tab_display_projection(titled, 80),
            "Create Python Script"
        );

        let agent_only = TabDisplayProjection::Derived {
            agent: Some("codex".into()),
            ticket: None,
            binding: None,
            title: None,
        };
        assert_eq!(fit_tab_display_projection(agent_only, 80), "codex");
    }

    #[test]
    fn tab_projection_distinguishes_owner_history_and_unowned_pr() {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("owner"), Workspace::test_new("review")];
        app.ensure_test_terminals();
        let terminal_ids = app
            .workspaces
            .iter()
            .map(|workspace| {
                workspace
                    .terminal_id(workspace.tabs[0].root_pane)
                    .cloned()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        for (terminal_id, role, active_owner) in [
            (
                &terminal_ids[0],
                crate::work_context::PaneWorkRole::Ship,
                true,
            ),
            (
                &terminal_ids[1],
                crate::work_context::PaneWorkRole::Review,
                false,
            ),
        ] {
            let context = crate::work_context::PaneWorkContext {
                pr_urls: vec!["https://github.com/o/r/pull/42".into()],
                role: Some(role),
                active_owner,
                ..Default::default()
            }
            .normalized_spawn_binding()
            .unwrap();
            app.terminals
                .get_mut(terminal_id)
                .unwrap()
                .replace_prevalidated_manual_work_context(context);
        }

        assert_eq!(
            tab_chrome_label(&app.workspaces[0], &app.terminals, 0, 80),
            "owner/ship"
        );
        assert_eq!(
            tab_chrome_label(&app.workspaces[1], &app.terminals, 0, 80),
            "history/review"
        );
        app.terminals
            .get_mut(&terminal_ids[0])
            .unwrap()
            .clear_active_work_owner();
        assert_eq!(
            tab_chrome_label(&app.workspaces[1], &app.terminals, 0, 80),
            "history/review · un-owned"
        );
    }

    #[test]
    fn plain_shell_tab_bar_uses_number_instead_of_cwd_title() {
        let mut app = AppState::test_new();
        let ws = Workspace::test_new("test");
        app.workspaces = vec![ws];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0]
            .terminal_id(app.workspaces[0].tabs[0].layout.focused())
            .cloned()
            .unwrap();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_terminal_title(Some("cx-personal-20260801-feature".into()));

        assert_eq!(
            tab_chrome_label(&app.workspaces[0], &app.terminals, 0, 80),
            "1"
        );
    }

    #[test]
    fn tab_width_uses_display_width_for_cjk_labels() {
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("提交 herdr 的反馈".into());

        assert_eq!(
            tab_width(&ws, &Default::default(), 0),
            display_width_u16("提交 herdr 的反馈") + 4
        );
    }

    #[test]
    fn tab_bar_renders_trailing_cjk_character() {
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("提交 herdr 的反馈".into());

        app.active = Some(0);
        app.workspaces = vec![ws];
        app.view.tab_bar_rect = Rect::new(0, 0, 30, 1);
        let view = compute_tab_bar_view(
            &app.workspaces[0],
            &app.terminals,
            app.view.tab_bar_rect,
            0,
            true,
            false,
        );
        app.view.tab_hit_areas = view.tab_hit_areas;

        let backend = TestBackend::new(30, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tab_bar(&app, frame, app.view.tab_bar_rect))
            .unwrap();

        let row = buffer_row_text(terminal.backend().buffer(), app.view.tab_bar_rect, 0);
        assert!(row.contains('馈'), "tab row: {row:?}");
    }
}
