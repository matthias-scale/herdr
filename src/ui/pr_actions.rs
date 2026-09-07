use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    widgets::Paragraph,
    Frame,
};

use crate::app::state::{AppState, PrActionMenuState};
use crate::ui::dropdown::{layout_dropdown, DropdownLayout, DropdownMenuRow, DropdownSpec};
use crate::ui::text::display_width;
use crate::ui::work_list_detail::{PrAction, PrActionPlacement};

enum MenuRow<'a> {
    Separator,
    Action { index: usize, action: &'a PrAction },
}

fn menu_rows(actions: &[PrAction]) -> Vec<MenuRow<'_>> {
    let mut rows = Vec::new();
    let mut previous_group = None;
    let mut index = 0;
    for action in actions {
        let PrActionPlacement::Menu { group } = action.placement else {
            continue;
        };
        if previous_group.is_some_and(|previous| previous != group) {
            rows.push(MenuRow::Separator);
        }
        rows.push(MenuRow::Action { index, action });
        previous_group = Some(group);
        index += 1;
    }
    rows
}

pub(crate) fn menu_actions(actions: &[PrAction]) -> Vec<&PrAction> {
    actions
        .iter()
        .filter(|action| matches!(action.placement, PrActionPlacement::Menu { .. }))
        .collect()
}

pub(crate) fn move_selection(state: &mut PrActionMenuState, actions: &[PrAction], delta: i8) {
    let count = menu_actions(actions).len();
    if count == 0 {
        state.selected = 0;
        return;
    }
    state.selected = (state.selected as i64 + i64::from(delta))
        .clamp(0, count.saturating_sub(1) as i64) as usize;
}

pub(crate) fn layout(
    area: Rect,
    anchor: Rect,
    actions: &[PrAction],
    state: PrActionMenuState,
) -> Option<DropdownLayout> {
    let rows = menu_rows(actions);
    let selected_row = rows
        .iter()
        .position(|row| matches!(row, MenuRow::Action { index, .. } if *index == state.selected))
        .unwrap_or(0);
    let width = rows
        .iter()
        .map(|row| match row {
            MenuRow::Separator => 1,
            MenuRow::Action { action, .. } => {
                display_width(&action.label)
                    + action
                        .disabled_reason
                        .map(|reason| display_width(reason) + 3)
                        .unwrap_or_default()
                    + 2
            }
        })
        .max()
        .unwrap_or(1);
    layout_dropdown(
        &DropdownSpec {
            anchor,
            item_count: rows.len(),
            selected: selected_row,
            has_filter: false,
            max_rows: rows.len(),
            min_width: u16::try_from(width).unwrap_or(u16::MAX),
        },
        area,
    )
}

pub(crate) fn hit_test(
    area: Rect,
    anchor: Rect,
    actions: &[PrAction],
    state: PrActionMenuState,
    col: u16,
    row: u16,
) -> Option<usize> {
    let layout = layout(area, anchor, actions, state)?;
    let row_index = crate::ui::dropdown::hit_test(&layout, col, row)?;
    match menu_rows(actions).get(row_index)? {
        MenuRow::Action { index, .. } => Some(*index),
        MenuRow::Separator => None,
    }
}

pub(crate) fn render(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    anchor: Rect,
    actions: &[PrAction],
    state: PrActionMenuState,
) -> bool {
    let Some(layout) = layout(area, anchor, actions, state) else {
        return false;
    };
    let rows = menu_rows(actions);
    let selected_row = rows
        .iter()
        .position(|row| matches!(row, MenuRow::Action { index, .. } if *index == state.selected))
        .unwrap_or(0);
    let rows = rows
        .into_iter()
        .map(|row| match row {
            MenuRow::Separator => DropdownMenuRow::Separator,
            MenuRow::Action { action, .. } => {
                let reason = action
                    .disabled_reason
                    .map(|reason| format!(" · {reason}"))
                    .unwrap_or_default();
                DropdownMenuRow::Item {
                    label: format!("{}{reason}", action.label),
                    enabled: action.enabled(),
                }
            }
        })
        .collect::<Vec<_>>();
    crate::ui::dropdown::render_menu(&app.palette, frame, &layout, &rows, selected_row);
    true
}

pub(crate) fn render_confirmation(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(confirmation) = app.pr_action_confirmation.as_ref() else {
        return;
    };
    super::dim_background(frame, area);
    let Some(inner) = super::widgets::render_modal_shell(frame, area, 58, 7, &app.palette) else {
        return;
    };
    super::widgets::render_modal_header(
        frame,
        Rect::new(inner.x, inner.y, inner.width, 1),
        confirmation.title(),
        &app.palette,
    );
    frame.render_widget(
        Paragraph::new(confirmation.body()).style(Style::default().fg(app.palette.subtext0)),
        Rect::new(inner.x, inner.y.saturating_add(2), inner.width, 1),
    );
    let buttons = format!("[Cancel] [{}]", confirmation.confirm_label());
    let width = u16::try_from(display_width(&buttons)).unwrap_or(inner.width);
    frame.render_widget(
        Paragraph::new(buttons).style(
            Style::default()
                .fg(app.palette.text)
                .add_modifier(Modifier::BOLD),
        ),
        Rect::new(
            inner.right().saturating_sub(width),
            inner.bottom().saturating_sub(1),
            width.min(inner.width),
            1,
        ),
    );
}

pub(crate) fn confirmation_button_rects(app: &AppState, area: Rect) -> Option<(Rect, Rect)> {
    let confirmation = app.pr_action_confirmation.as_ref()?;
    let popup = super::widgets::centered_popup_rect(area, 58, 7)?;
    let inner = Rect::new(
        popup.x.saturating_add(1),
        popup.y.saturating_add(1),
        popup.width.saturating_sub(2),
        popup.height.saturating_sub(2),
    );
    let confirm = format!("[{}]", confirmation.confirm_label());
    let confirm_width = u16::try_from(display_width(&confirm)).unwrap_or(inner.width);
    let cancel_width: u16 = 8;
    let total_width = cancel_width.saturating_add(1).saturating_add(confirm_width);
    let start = inner.right().saturating_sub(total_width);
    let row = inner.bottom().saturating_sub(1);
    Some((
        Rect::new(start, row, cancel_width, 1),
        Rect::new(
            start.saturating_add(cancel_width).saturating_add(1),
            row,
            confirm_width,
            1,
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn confirmation_text(method: crate::config::MergeMethodConfig) -> String {
        let mut app = AppState::test_new();
        app.pr_action_confirmation = Some(crate::app::state::PrActionConfirmation {
            key: crate::app::state::WorkItemKey {
                repo: "owner/repo".into(),
                pr_number: Some(42),
                pr_url: Some("https://github.com/owner/repo/pull/42".into()),
                ticket_id: None,
            },
            action: crate::ui::work_list_detail::PrActionKind::Merge(method),
        });
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render_confirmation(&app, frame, frame.area()))
            .expect("render confirmation");
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(80)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn merge_confirmation_renders_exact_copy_for_each_method() {
        for method in crate::config::MergeMethodConfig::ALL {
            let rendered = confirmation_text(method);
            assert!(rendered.contains("Merge pull request?"), "{rendered}");
            assert!(
                rendered.contains(&format!("This merges #42 using {}.", method.label())),
                "{rendered}"
            );
            assert!(rendered.contains("[Cancel] [Merge]"), "{rendered}");
        }
    }

    #[test]
    fn grouped_action_menu_opens_downward_from_its_anchor() {
        let actions = vec![
            PrAction {
                kind: crate::ui::work_list_detail::PrActionKind::Refresh,
                label: "Refresh".into(),
                placement: PrActionPlacement::Menu { group: 0 },
                disabled_reason: None,
            },
            PrAction {
                kind: crate::ui::work_list_detail::PrActionKind::Close,
                label: "Close pull request".into(),
                placement: PrActionPlacement::Menu { group: 1 },
                disabled_reason: Some("pull request is closed"),
            },
        ];
        let anchor = Rect::new(20, 3, 1, 1);
        let menu = layout(
            Rect::new(0, 0, 80, 24),
            anchor,
            &actions,
            PrActionMenuState::default(),
        )
        .expect("menu fits below anchor");
        assert_eq!(menu.rect.y, anchor.bottom());
        assert_eq!(menu.visible_rows, 3);
    }
}
