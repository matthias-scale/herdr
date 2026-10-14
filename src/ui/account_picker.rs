//! One geometry contract for Home/Spawn rendering and mouse targets.
use super::text::truncate_end;
use crate::app::{
    account_picker::{quota_lines, PickerFocus},
    home::HomeState,
    state::AppState,
};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickerHit {
    Account(usize),
    Model(usize),
    Close,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PickerGeometry {
    pub(crate) popup: Rect,
    pub(crate) accounts: Rect,
    pub(crate) search: Rect,
    pub(crate) models: Rect,
    pub(crate) usage: Rect,
    pub(crate) footer: Rect,
}

pub(crate) fn geometry(area: Rect) -> PickerGeometry {
    let width = area.width.min(88);
    let height = area.height.min(24);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let inner = Block::default().borders(Borders::ALL).inner(popup);
    let footer = Rect::new(
        inner.x,
        inner.bottom().saturating_sub(1).max(inner.y),
        inner.width,
        u16::from(inner.height > 0),
    );
    let usage_height = inner.height.saturating_sub(4).min(6);
    let usage = Rect::new(
        inner.x,
        footer.y.saturating_sub(usage_height).max(inner.y),
        inner.width,
        usage_height,
    );
    let body_height = usage.y.saturating_sub(inner.y);
    let (accounts, search, models) = if width >= 64 {
        let rail_width = (inner.width / 3).min(30);
        let accounts = Rect::new(inner.x, inner.y, rail_width, body_height);
        let model_x = accounts.right().min(inner.right());
        let model_width = inner.right().saturating_sub(model_x);
        (
            accounts,
            Rect::new(model_x, inner.y, model_width, u16::from(body_height > 0)),
            Rect::new(
                model_x,
                inner.y.saturating_add(1).min(usage.y),
                model_width,
                body_height.saturating_sub(1),
            ),
        )
    } else {
        let rail_height = (body_height / 2).max(u16::from(body_height > 0));
        let accounts = Rect::new(inner.x, inner.y, inner.width, rail_height);
        let search_height = u16::from(body_height > rail_height);
        let search = Rect::new(inner.x, accounts.bottom(), inner.width, search_height);
        let models = Rect::new(
            inner.x,
            search.bottom(),
            inner.width,
            body_height.saturating_sub(rail_height + search_height),
        );
        (accounts, search, models)
    };
    PickerGeometry {
        popup,
        accounts,
        search,
        models,
        usage,
        footer,
    }
}

pub(crate) fn scroll_start(cursor: usize, height: u16, len: usize) -> usize {
    cursor
        .min(len.saturating_sub(1))
        .saturating_sub(usize::from(height).saturating_sub(1))
}

fn contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.right() && y >= rect.y && y < rect.bottom()
}

pub(crate) fn hit_test(home: &HomeState, area: Rect, x: u16, y: u16) -> Option<PickerHit> {
    if !home.account_picker.active {
        return None;
    }
    let g = geometry(area);
    if !contains(g.popup, x, y) {
        return Some(PickerHit::Close);
    }
    if contains(g.accounts, x, y) {
        let start = scroll_start(
            home.account_picker.account_cursor,
            g.accounts.height,
            home.account_picker.rows.len(),
        );
        let index = start + usize::from(y - g.accounts.y);
        return (index < home.account_picker.rows.len()).then_some(PickerHit::Account(index));
    }
    if contains(g.models, x, y) {
        let len = home.filtered_account_models().len();
        let index = scroll_start(home.account_picker.model_cursor, g.models.height, len)
            + usize::from(y - g.models.y);
        return (index < len).then_some(PickerHit::Model(index));
    }
    None
}

pub(crate) fn apply_hit(home: &mut HomeState, hit: PickerHit) {
    match hit {
        PickerHit::Account(index) => {
            home.select_account_row(index);
            home.account_picker.focus = PickerFocus::Accounts;
        }
        PickerHit::Model(index) => {
            home.select_account_model(index);
        }
        PickerHit::Close => home.close_account_picker(),
    }
}

pub(crate) fn render(app: &AppState, home: &HomeState, frame: &mut Frame, area: Rect) {
    if !home.account_picker.active {
        return;
    }
    let g = geometry(area);
    let base = Style::default()
        .fg(app.palette.text)
        .bg(app.palette.panel_bg);
    frame.render_widget(Clear, g.popup);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(" provider / account / model ")
            .border_style(Style::default().fg(app.palette.accent))
            .style(base),
        g.popup,
    );
    let start = scroll_start(
        home.account_picker.account_cursor,
        g.accounts.height,
        home.account_picker.rows.len(),
    );
    for (offset, row) in home
        .account_picker
        .rows
        .iter()
        .skip(start)
        .take(g.accounts.height.into())
        .enumerate()
    {
        let active = start + offset == home.account_picker.account_cursor;
        let prefix = if active { "> " } else { "  " };
        let usage = row
            .selection
            .as_ref()
            .and_then(|selection| {
                app.provider_usage.accounts.iter().find(|account| {
                    account.provider == selection.provider
                        && account.profile_id == selection.account
                })
            })
            .map(|account| &account.usage);
        let unavailable = home.account_row_unavailable(row);
        let quota = usage
            .map(|usage| {
                if usage.unavailable == Some(crate::provider_usage::UsageUnavailable::AuthExpired) {
                    return "login required".to_string();
                }
                if usage.stale {
                    return "stale".to_string();
                }
                usage
                    .peak_percent()
                    .map(|percent| format!("{}% left", 100u8.saturating_sub(percent)))
                    .unwrap_or_else(|| "unknown".into())
            })
            .unwrap_or_default();
        render_row(
            app,
            frame,
            Rect::new(
                g.accounts.x,
                g.accounts.y + offset as u16,
                g.accounts.width,
                1,
            ),
            &format!(
                "{prefix}{} {}",
                row.label,
                if unavailable { "unavailable" } else { &quota }
            ),
            active,
            home.account_picker.focus == PickerFocus::Accounts,
        );
    }
    frame.render_widget(
        Paragraph::new(truncate_end(
            &format!("/ {}", home.account_picker.query),
            g.search.width.into(),
        ))
        .style(base.add_modifier(Modifier::BOLD)),
        g.search,
    );
    let matches = home.filtered_account_models();
    let start = scroll_start(
        home.account_picker.model_cursor,
        g.models.height,
        matches.len(),
    );
    if matches.is_empty() {
        let label = if home.profile().owns_its_flags() {
            "model flags owned by launcher"
        } else {
            "no matching model"
        };
        frame.render_widget(
            Paragraph::new(truncate_end(label, g.models.width.into())).style(base),
            g.models,
        );
    }
    for (offset, index) in matches
        .iter()
        .skip(start)
        .take(g.models.height.into())
        .enumerate()
    {
        let active = start + offset == home.account_picker.model_cursor;
        let model = &home.model_options()[*index];
        let marker = if model.id == home.model { "* " } else { "  " };
        render_row(
            app,
            frame,
            Rect::new(g.models.x, g.models.y + offset as u16, g.models.width, 1),
            &format!("{marker}{}", model.display_name),
            active,
            home.account_picker.focus == PickerFocus::Models,
        );
    }
    let mut lines = vec![home.account_label()];
    if let Err(reason) = home.account_launch_env() {
        lines.push(reason);
    } else if home
        .account_selection
        .as_ref()
        .is_some_and(|selection| selection.provider == crate::provider_usage::QuotaProvider::Claude)
    {
        lines.push("subscription authentication checked at launch".into());
    }
    lines.extend(quota_lines(
        home.selected_account_usage(&app.provider_usage),
        app.provider_usage.collected_unix,
    ));
    for (offset, line) in lines.iter().take(g.usage.height.into()).enumerate() {
        frame.render_widget(
            Paragraph::new(truncate_end(line, g.usage.width.into())).style(base),
            Rect::new(g.usage.x, g.usage.y + offset as u16, g.usage.width, 1),
        );
    }
    frame.render_widget(
        Paragraph::new(truncate_end(
            "tab rail/models · arrows select · type search · enter use · esc close",
            g.footer.width.into(),
        ))
        .style(base),
        g.footer,
    );
}

fn render_row(
    app: &AppState,
    frame: &mut Frame,
    rect: Rect,
    label: &str,
    active: bool,
    focused: bool,
) {
    let style = Style::default().fg(app.palette.text).bg(if active {
        app.palette.selection_bg
    } else {
        app.palette.panel_bg
    });
    frame.render_widget(
        Paragraph::new(truncate_end(label, rect.width.into())).style(if active && focused {
            style.add_modifier(Modifier::BOLD)
        } else {
            style
        }),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::home::HomePicker;
    use ratatui::{backend::TestBackend, Terminal};
    #[test]
    fn account_picker_normal_and_narrow_render_hit_geometry_and_scroll_match() {
        for (width, height) in [(100, 30), (40, 24), (30, 12), (18, 10), (2, 2)] {
            let mut app = AppState::test_new();
            let mut home = HomeState::default();
            home.refresh_account_rows(&Default::default());
            home.open_account_picker(HomePicker::Agent);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
            let area = Rect::new(0, 0, width, height);
            terminal
                .draw(|frame| render(&app, &home, frame, area))
                .expect("render");
            let g = geometry(area);
            assert!(g.popup.right() <= area.right() && g.popup.bottom() <= area.bottom());
            if g.accounts.height > 0 && g.accounts.width > 0 {
                assert_eq!(
                    hit_test(&home, area, g.accounts.x, g.accounts.y),
                    Some(PickerHit::Account(0))
                );
                apply_hit(&mut home, PickerHit::Account(1));
                assert_eq!(home.profile_id(), "codex");
            }
            app.home = Some(home);
        }
        assert_eq!(scroll_start(12, 3, 15), 10);
    }
}
