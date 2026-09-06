use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    widgets::{Clear, Paragraph, Wrap},
    Frame,
};

use super::widgets::{
    action_button_row_rects, centered_popup_rect, panel_contrast_fg, render_action_button,
    render_modal_header, render_modal_shell, ActionButtonSpec,
};
use crate::app::state::{AddActionField, AddActionState};
use crate::app::AppState;

const POPUP_WIDTH: u16 = 62;
const POPUP_HEIGHT: u16 = 18;

#[derive(Debug, Clone, Default)]
pub(crate) struct AddActionLayout {
    pub(crate) close: Rect,
    pub(crate) fields: Vec<(AddActionField, Rect)>,
    pub(crate) cancel: Rect,
    pub(crate) save: Rect,
}

pub(crate) fn add_action_layout(area: Rect) -> AddActionLayout {
    let Some(popup) = centered_popup_rect(area, POPUP_WIDTH, POPUP_HEIGHT) else {
        return AddActionLayout::default();
    };
    let inner = Rect::new(
        popup.x.saturating_add(1),
        popup.y.saturating_add(1),
        popup.width.saturating_sub(2),
        popup.height.saturating_sub(2),
    );
    if inner.width < 20 || inner.height < 14 {
        return AddActionLayout::default();
    }
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(inner);
    let buttons = action_button_row_rects(
        rows[14],
        &[
            ActionButtonSpec {
                hint: Some("Esc"),
                label: "Cancel",
            },
            ActionButtonSpec {
                hint: Some("Enter"),
                label: "Save action",
            },
        ],
        2,
        0,
    );
    AddActionLayout {
        close: Rect::new(inner.x + inner.width.saturating_sub(1), inner.y, 1, 1),
        fields: vec![
            (AddActionField::Name, rows[2]),
            (AddActionField::Key, rows[5]),
            (AddActionField::Command, rows[8]),
            (AddActionField::RunOnWorktreeCreate, rows[10]),
            (AddActionField::OpenInBottomPane, rows[11]),
        ],
        cancel: buttons.first().copied().unwrap_or_default(),
        save: buttons.get(1).copied().unwrap_or_default(),
    }
}

pub(super) fn render_add_action_overlay(app: &AppState, frame: &mut Frame) {
    let Some(action) = app.add_action.as_ref() else {
        return;
    };
    super::dim_background(frame, frame.area());
    let Some(inner) =
        render_modal_shell(frame, frame.area(), POPUP_WIDTH, POPUP_HEIGHT, &app.palette)
    else {
        return;
    };
    let layout = add_action_layout(frame.area());
    render_modal_header(
        frame,
        Rect::new(inner.x, inner.y, inner.width, 1),
        "Add action",
        &app.palette,
    );
    frame.render_widget(
        Paragraph::new("×").style(Style::default().fg(app.palette.overlay1)),
        layout.close,
    );

    render_text_field(
        app,
        frame,
        action,
        AddActionField::Name,
        "Name",
        &action.name,
    );
    render_text_field(
        app,
        frame,
        action,
        AddActionField::Key,
        "Keybinding",
        if action.key.is_empty() {
            "press a chord"
        } else {
            &action.key
        },
    );
    render_text_field(
        app,
        frame,
        action,
        AddActionField::Command,
        "Command",
        &action.command,
    );
    render_toggle(
        app,
        frame,
        action,
        AddActionField::RunOnWorktreeCreate,
        "Run on worktree create",
        action.run_on_worktree_create,
    );
    render_toggle(
        app,
        frame,
        action,
        AddActionField::OpenInBottomPane,
        "Open in bottom pane",
        action.open_in_bottom_pane,
    );

    if let Some(error) = &action.error {
        let error_area = Rect::new(inner.x, inner.y + 12, inner.width, 2);
        frame.render_widget(
            Paragraph::new(error.as_str())
                .style(Style::default().fg(app.palette.red))
                .wrap(Wrap { trim: true }),
            error_area,
        );
    }

    let inactive = Style::default()
        .fg(app.palette.text)
        .bg(app.palette.surface0);
    let active = Style::default()
        .fg(panel_contrast_fg(&app.palette))
        .bg(app.palette.accent)
        .add_modifier(Modifier::BOLD);
    render_action_button(frame, layout.cancel, Some("Esc"), "Cancel", inactive);
    render_action_button(frame, layout.save, Some("Enter"), "Save action", active);
}

fn render_text_field(
    app: &AppState,
    frame: &mut Frame,
    action: &AddActionState,
    field: AddActionField,
    label: &str,
    value: &str,
) {
    let layout = add_action_layout(frame.area());
    let Some((_, rect)) = layout
        .fields
        .iter()
        .find(|(candidate, _)| *candidate == field)
    else {
        return;
    };
    let label_rect = Rect::new(rect.x, rect.y.saturating_sub(1), rect.width, 1);
    frame.render_widget(
        Paragraph::new(label).style(Style::default().fg(app.palette.overlay0)),
        label_rect,
    );
    let style = if action.field == field {
        Style::default()
            .fg(app.palette.text)
            .bg(app.palette.surface1)
    } else {
        Style::default()
            .fg(app.palette.subtext0)
            .bg(app.palette.surface0)
    };
    let cursor = if action.field == field && field != AddActionField::Key {
        "█"
    } else {
        ""
    };
    frame.render_widget(Clear, *rect);
    frame.render_widget(
        Paragraph::new(format!(" {value}{cursor}")).style(style),
        *rect,
    );
}

fn render_toggle(
    app: &AppState,
    frame: &mut Frame,
    action: &AddActionState,
    field: AddActionField,
    label: &str,
    value: bool,
) {
    let layout = add_action_layout(frame.area());
    let Some((_, rect)) = layout
        .fields
        .iter()
        .find(|(candidate, _)| *candidate == field)
    else {
        return;
    };
    let marker = if value { "[x]" } else { "[ ]" };
    let style = if action.field == field {
        Style::default()
            .fg(app.palette.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(app.palette.subtext0)
    };
    frame.render_widget(
        Paragraph::new(format!(" {marker} {label}")).style(style),
        *rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_action_modal_layout_exposes_close_fields_and_actions_at_80x24() {
        let layout = add_action_layout(Rect::new(0, 0, 80, 24));
        assert_eq!(layout.fields.len(), AddActionField::ALL.len());
        assert!(layout.close.width > 0);
        assert!(layout.cancel.width > 0);
        assert!(layout.save.width > 0);
        assert!(layout.fields.iter().all(|(_, rect)| rect.bottom() <= 24));
    }
}
