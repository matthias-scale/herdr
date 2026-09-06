use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::{
    app::{
        state::{AppState, SettingsSection, THEME_NAMES},
        App, Mode,
    },
    config::{StatusIndicatorStyle, ToastDelivery},
};

#[derive(Debug, Clone, PartialEq, Eq)]
// The shared `Save` verb is semantic: these actions persist settings.
#[allow(clippy::enum_variant_names)]
pub(super) enum SettingsAction {
    SaveTheme(String),
    SaveStatusIndicators(StatusIndicatorStyle),
    SaveSound(bool),
    SaveToastDelivery(ToastDelivery),
    SaveAgentBorderLabels(bool),
    SaveConfigEdit(crate::app::settings_general::ConfigEdit),
    RestoreArchived(crate::app::state::PaneFocusTarget),
    DeleteArchived(crate::app::state::PaneFocusTarget),
    InstallRecommendedIntegrations,
}

impl App {
    pub(crate) fn handle_settings_key(&mut self, key: KeyEvent) {
        let previous_section = self.state.settings.section;
        if let Some(action) = update_settings_state(&mut self.state, key) {
            match action {
                SettingsAction::SaveTheme(name) => self.save_theme(&name),
                SettingsAction::SaveStatusIndicators(style) => self.save_status_indicators(style),
                SettingsAction::SaveSound(enabled) => self.save_sound(enabled),
                SettingsAction::SaveToastDelivery(delivery) => self.save_toast_delivery(delivery),
                SettingsAction::SaveAgentBorderLabels(enabled) => {
                    self.save_agent_border_labels(enabled)
                }
                SettingsAction::SaveConfigEdit(edit) => self.save_config_edit(edit),
                SettingsAction::RestoreArchived(target) => {
                    self.restore_archived_pane(&target);
                }
                SettingsAction::DeleteArchived(target) => {
                    self.delete_archived_pane(&target);
                }
                SettingsAction::InstallRecommendedIntegrations => {
                    self.install_recommended_integrations()
                }
            }
        }
        if previous_section != SettingsSection::Integrations
            && self.state.settings.section == SettingsSection::Integrations
        {
            self.refresh_integration_recommendations();
        }
        if matches!(
            self.state.settings.section,
            SettingsSection::Providers | SettingsSection::Integrations
        ) {
            self.start_tool_probes_if_needed();
        }
    }
}

fn normalize_theme_name(name: &str) -> String {
    name.to_lowercase().replace([' ', '_'], "-")
}

fn current_theme_index(theme_name: &str) -> usize {
    let normalized = normalize_theme_name(theme_name);
    THEME_NAMES
        .iter()
        .position(|name| normalize_theme_name(name) == normalized)
        .unwrap_or(0)
}

fn status_indicator_index(style: StatusIndicatorStyle) -> usize {
    match style {
        StatusIndicatorStyle::Dots => 0,
        StatusIndicatorStyle::Symbols => 1,
    }
}

fn status_indicator_for_index(idx: usize) -> StatusIndicatorStyle {
    if idx == 0 {
        StatusIndicatorStyle::Dots
    } else {
        StatusIndicatorStyle::Symbols
    }
}

fn toast_delivery_index(delivery: ToastDelivery) -> usize {
    match delivery {
        ToastDelivery::Off => 0,
        ToastDelivery::Herdr => 1,
        ToastDelivery::Terminal => 2,
        ToastDelivery::System => 3,
    }
}

fn toast_delivery_for_index(idx: usize) -> ToastDelivery {
    match idx {
        0 => ToastDelivery::Off,
        1 => ToastDelivery::Herdr,
        2 => ToastDelivery::Terminal,
        _ => ToastDelivery::System,
    }
}

fn preview_selected_theme(state: &mut AppState) {
    use crate::app::state::Palette;

    let name = THEME_NAMES[state.settings.list.selected];
    if let Some(mut palette) = Palette::from_name(name) {
        if let Some(custom) = &state.theme_runtime.custom {
            palette = palette.with_overrides(custom);
        }
        if let Some(accent) = &state.theme_runtime.legacy_accent {
            palette.accent = crate::config::parse_color(accent);
        }
        state.palette = palette;
        state.theme_name = name.to_string();
    }
}

fn cancel_settings(state: &mut AppState) {
    if let Some(palette) = state.settings.original_palette.take() {
        state.palette = palette;
    }
    if let Some(theme_name) = state.settings.original_theme.take() {
        state.theme_name = theme_name;
    }
    super::modal::leave_modal(state);
}

fn integrations_need_install(state: &AppState) -> bool {
    state
        .integration_recommendations
        .iter()
        .any(crate::integration::IntegrationRecommendation::needs_install)
}

fn apply_settings(state: &mut AppState) -> Option<SettingsAction> {
    match state.settings.section {
        SettingsSection::Theme => {
            let theme_name = state.theme_name.clone();
            state.settings.original_palette = None;
            state.settings.original_theme = None;
            super::modal::leave_modal(state);
            Some(SettingsAction::SaveTheme(theme_name))
        }
        SettingsSection::Integrations if integrations_need_install(state) => {
            Some(SettingsAction::InstallRecommendedIntegrations)
        }
        SettingsSection::Integrations => None,
        _ => {
            super::modal::leave_modal(state);
            None
        }
    }
}

/// How many selectable rows the section's content column has. Sections that
/// only display facts have none, so `up`/`down` are inert there rather than
/// moving an invisible cursor.
pub(crate) fn settings_section_item_count(state: &AppState, section: SettingsSection) -> usize {
    match section {
        SettingsSection::General => crate::app::settings_general::GeneralRow::ALL.len(),
        SettingsSection::Theme => THEME_NAMES.len(),
        SettingsSection::Indicators | SettingsSection::Sound | SettingsSection::PaneLabels => 2,
        SettingsSection::Toast => 4,
        SettingsSection::Archive => state.archive_entries().len(),
        SettingsSection::Keybindings => crate::ui::settings_keybinding_rows(state).len(),
        SettingsSection::Providers
        | SettingsSection::Integrations
        | SettingsSection::SourceControl
        | SettingsSection::About => 0,
    }
}

/// Where the cursor lands when a section is opened: on the current value, so
/// the screen tells the operator what is set without them having to read it.
fn default_selected_index(state: &AppState, section: SettingsSection) -> usize {
    match section {
        SettingsSection::Theme => current_theme_index(&state.theme_name),
        SettingsSection::Indicators => status_indicator_index(state.status_indicators),
        SettingsSection::Sound => usize::from(!state.sound_enabled()),
        SettingsSection::Toast => toast_delivery_index(state.toast_delivery()),
        SettingsSection::PaneLabels => usize::from(!state.agent_border_labels_enabled()),
        _ => 0,
    }
}

fn select_section(state: &mut AppState, section: SettingsSection) {
    state.settings.section = section;
    state.settings.list.selected = default_selected_index(state, section);
    state.settings.archive_delete_armed = false;
}

fn move_selection(state: &mut AppState, delta: isize) {
    let count = settings_section_item_count(state, state.settings.section);
    if count == 0 {
        return;
    }
    let previous = state.settings.list.selected;
    if delta < 0 {
        state.settings.list.move_prev();
    } else {
        state.settings.list.move_next(count);
    }
    if state.settings.list.selected != previous {
        state.settings.archive_delete_armed = false;
        if state.settings.section == SettingsSection::Theme {
            preview_selected_theme(state);
        }
    }
}

/// `enter` / `space` on the selected row.
fn activate_selection(state: &mut AppState) -> Option<SettingsAction> {
    let idx = state.settings.list.selected;
    match state.settings.section {
        SettingsSection::General => {
            let row = *crate::app::settings_general::GeneralRow::ALL.get(idx)?;
            crate::app::settings_general::cycle_general_row(state, row)
                .map(SettingsAction::SaveConfigEdit)
        }
        SettingsSection::Indicators => Some(SettingsAction::SaveStatusIndicators(
            status_indicator_for_index(idx),
        )),
        SettingsSection::Sound => Some(SettingsAction::SaveSound(idx == 0)),
        SettingsSection::Toast => Some(SettingsAction::SaveToastDelivery(
            toast_delivery_for_index(idx),
        )),
        SettingsSection::PaneLabels => Some(SettingsAction::SaveAgentBorderLabels(idx == 0)),
        SettingsSection::Archive => {
            let entry = state.archive_entries().into_iter().nth(idx)?;
            state.settings.archive_delete_armed = false;
            Some(SettingsAction::RestoreArchived(entry.target))
        }
        SettingsSection::Integrations if integrations_need_install(state) => {
            Some(SettingsAction::InstallRecommendedIntegrations)
        }
        _ => None,
    }
}

/// `d` on an archive row. Confirms once first while `ui.confirm_close` is on,
/// the same rule the sidebar's settled menu follows.
fn delete_archive_selection(state: &mut AppState) -> Option<SettingsAction> {
    if state.settings.section != SettingsSection::Archive {
        return None;
    }
    let entry = state
        .archive_entries()
        .into_iter()
        .nth(state.settings.list.selected)?;
    if state.confirm_close && !std::mem::take(&mut state.settings.archive_delete_armed) {
        state.settings.archive_delete_armed = true;
        return None;
    }
    state.settings.archive_delete_armed = false;
    Some(SettingsAction::DeleteArchived(entry.target))
}

pub(super) fn update_settings_state(state: &mut AppState, key: KeyEvent) -> Option<SettingsAction> {
    match key.code {
        KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
            select_section(state, state.settings.section.next());
            return None;
        }
        KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
            select_section(state, state.settings.section.prev());
            return None;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            move_selection(state, -1);
            return None;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            move_selection(state, 1);
            return None;
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            if let Some(action) = activate_selection(state) {
                return Some(action);
            }
            if state.settings.section == SettingsSection::Theme {
                return apply_settings(state);
            }
            return None;
        }
        KeyCode::Char('d') | KeyCode::Delete => {
            if let Some(action) = delete_archive_selection(state) {
                return Some(action);
            }
            if key.code == KeyCode::Char('d') {
                return None;
            }
        }
        _ => {}
    }

    match super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS) {
        Some(super::modal::ModalAction::Apply) => apply_settings(state),
        Some(super::modal::ModalAction::Close) => {
            cancel_settings(state);
            None
        }
        _ => None,
    }
}

pub(crate) fn open_settings(state: &mut AppState) {
    open_settings_at(state, SettingsSection::General);
}

pub(crate) fn open_settings_at(state: &mut AppState, section: SettingsSection) {
    state.integration_install_messages.clear();
    state.settings.original_palette = Some(state.palette.clone());
    state.settings.original_theme = Some(state.theme_name.clone());
    select_section(state, section);
    state.mode = Mode::Settings;
}

impl AppState {
    fn settings_popup_rect(&self) -> Rect {
        crate::ui::centered_popup_rect(
            self.screen_rect(),
            crate::ui::SETTINGS_POPUP_WIDTH,
            crate::ui::settings_popup_height(self),
        )
        .unwrap_or_default()
    }

    fn settings_inner_rect(&self) -> Rect {
        let popup = self.settings_popup_rect();
        Rect::new(
            popup.x + 1,
            popup.y + 1,
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        )
    }

    /// The section under a click in the left nav column.
    fn settings_tab_at(&self, col: u16, row: u16) -> Option<SettingsSection> {
        let nav = crate::ui::settings_areas(self.settings_inner_rect()).nav;
        if col < nav.x || col >= nav.x + nav.width || row < nav.y || row >= nav.y + nav.height {
            return None;
        }
        let index = crate::ui::settings_nav_scroll(self, nav) + (row - nav.y) as usize;
        SettingsSection::ALL.get(index).copied()
    }

    pub(crate) fn settings_content_rect(&self) -> Rect {
        crate::ui::settings_areas(self.settings_inner_rect()).content
    }

    fn settings_list_index_at(&self, col: u16, row: u16) -> Option<usize> {
        let area = self.settings_content_rect();
        if row < area.y || row >= area.y + area.height || col < area.x || col >= area.x + area.width
        {
            return None;
        }

        match self.settings.section {
            SettingsSection::General => {
                let list_y = area.y + 2;
                let offset = row.checked_sub(list_y)?;
                crate::ui::general_row_offsets()
                    .into_iter()
                    .position(|(start, height)| offset >= start && offset < start + height)
            }
            SettingsSection::Theme => {
                let max_visible = area.height as usize;
                let scroll = if self.settings.list.selected >= max_visible {
                    self.settings.list.selected - max_visible + 1
                } else {
                    0
                };
                let idx = scroll + (row - area.y) as usize;
                (idx < THEME_NAMES.len()).then_some(idx)
            }
            SettingsSection::Indicators | SettingsSection::Sound | SettingsSection::PaneLabels => {
                let list_y = area.y + 3;
                if row >= list_y && row < list_y + 2 {
                    Some((row - list_y) as usize)
                } else {
                    None
                }
            }
            SettingsSection::Toast => {
                let list_y = area.y + 3;
                if row >= list_y && row < list_y + 8 {
                    Some(((row - list_y) / 2) as usize)
                } else {
                    None
                }
            }
            SettingsSection::Archive | SettingsSection::Keybindings => {
                let list_y = area.y + 2;
                let visible = area.height.saturating_sub(2) as usize;
                let offset = row.checked_sub(list_y)? as usize;
                let count = settings_section_item_count(self, self.settings.section);
                let scroll = self
                    .settings
                    .list
                    .selected
                    .saturating_sub(visible.saturating_sub(1));
                let idx = scroll + offset;
                (idx < count).then_some(idx)
            }
            SettingsSection::Providers
            | SettingsSection::Integrations
            | SettingsSection::SourceControl
            | SettingsSection::About => None,
        }
    }

    pub(super) fn handle_settings_mouse(&mut self, mouse: MouseEvent) -> Option<SettingsAction> {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(section) = self.settings_tab_at(mouse.column, mouse.row) {
                    select_section(self, section);
                    return None;
                }
                if let Some(idx) = self.settings_list_index_at(mouse.column, mouse.row) {
                    self.settings.list.select(idx);
                    if self.settings.section == SettingsSection::Theme {
                        preview_selected_theme(self);
                        return None;
                    }
                    return activate_selection(self);
                }

                let inner = self.settings_inner_rect();
                let show_primary = crate::ui::settings_show_primary_action(self);
                let (apply, close) =
                    crate::ui::settings_button_rects(inner, self.settings.section, show_primary);
                let mut buttons = vec![(close, super::modal::ModalAction::Close)];
                if let Some(apply) = apply {
                    buttons.insert(0, (apply, super::modal::ModalAction::Apply));
                }
                match super::modal::modal_action_from_buttons(mouse.column, mouse.row, &buttons) {
                    Some(super::modal::ModalAction::Apply) => apply_settings(self),
                    Some(super::modal::ModalAction::Close) => {
                        cancel_settings(self);
                        None
                    }
                    _ => {
                        cancel_settings(self);
                        None
                    }
                }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::layout::Rect;

    use super::super::{app_for_mouse_test, mouse, state_with_workspaces};
    use super::*;

    #[test]
    fn desktop_clamped_settings_hit_geometry_includes_status_row() {
        let mut state = state_with_workspaces(&["test"]);
        state.view.status_bar_rect = Rect::new(0, 0, 106, 1);
        state.view.sidebar_rect = Rect::new(0, 1, 26, 19);
        state.view.terminal_area = Rect::new(26, 2, 80, 18);
        open_settings(&mut state);

        assert_eq!(state.screen_rect(), Rect::new(0, 0, 106, 20));
        assert_eq!(state.settings_popup_rect(), Rect::new(5, 1, 96, 18));
        let inner = state.settings_inner_rect();
        let (_, close) = crate::ui::settings_button_rects(
            inner,
            state.settings.section,
            crate::ui::settings_show_primary_action(&state),
        );
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: close.x,
            row: close.y,
            modifiers: KeyModifiers::empty(),
        };

        assert_eq!(state.handle_settings_mouse(click), None);
        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn settings_cancel_restores_previewed_theme_from_other_sections() {
        let mut state = state_with_workspaces(&["test"]);
        let original_palette = state.palette.clone();
        let original_theme = state.theme_name.clone();

        open_settings_at(&mut state, SettingsSection::Theme);
        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );
        assert_ne!(state.theme_name, original_theme);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        );
        assert_eq!(
            state.settings.section,
            crate::app::state::SettingsSection::Indicators
        );

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
        );

        assert_eq!(state.mode, Mode::Terminal);
        assert_eq!(state.theme_name, original_theme);
        assert_eq!(state.palette.accent, original_palette.accent);
        assert_eq!(state.palette.panel_bg, original_palette.panel_bg);
    }

    #[test]
    fn settings_indicator_choice_returns_save_action() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::Indicators);
        state.settings.list.selected = 1;

        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        assert_eq!(
            action,
            Some(SettingsAction::SaveStatusIndicators(
                StatusIndicatorStyle::Symbols
            ))
        );
        assert_eq!(state.status_indicators, StatusIndicatorStyle::Dots);
        assert_eq!(state.mode, Mode::Settings);
    }

    #[test]
    fn settings_sound_toggle_returns_save_action() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings(&mut state);
        state.settings.section = crate::app::state::SettingsSection::Sound;
        state.settings.list.selected = 0;

        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        assert_eq!(action, Some(SettingsAction::SaveSound(true)));
        assert!(!state.sound.enabled);
        assert_eq!(state.mode, Mode::Settings);
    }

    #[test]
    fn settings_tab_cycle_wraps_around_the_section_list() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::About);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.section, SettingsSection::General);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.section, SettingsSection::About);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.section, SettingsSection::Archive);
    }

    #[test]
    fn integrations_enter_does_nothing_when_nothing_needs_install() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::Integrations);

        let enter_action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        assert_eq!(enter_action, None);

        let space_action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::empty()),
        );
        assert_eq!(space_action, None);
    }

    #[test]
    fn settings_hover_does_not_change_selection() {
        let mut app = app_for_mouse_test();
        open_settings(&mut app.state);
        app.state.settings.list.select(0);

        let area = app.state.settings_content_rect();
        app.handle_mouse(mouse(MouseEventKind::Moved, area.x + 2, area.y + 2));

        assert_eq!(app.state.settings.list.selected, 0);
    }

    #[test]
    fn integration_update_badge_only_tracks_outdated_recommendations() {
        let mut state = state_with_workspaces(&["test"]);
        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::NotInstalled,
            true,
        )];
        assert!(!state.integration_updates_available());

        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::NotInstalled,
            false,
        )];
        assert!(!state.integration_updates_available());

        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::Current,
            true,
        )];
        assert!(!state.integration_updates_available());

        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::Outdated,
            true,
        )];
        assert!(state.integration_updates_available());
    }

    #[test]
    fn every_nav_row_maps_back_to_its_section() {
        let mut state = state_with_workspaces(&["test"]);
        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::Outdated,
            true,
        )];
        open_settings(&mut state);

        let nav = crate::ui::settings_areas(state.settings_inner_rect()).nav;
        let scroll = crate::ui::settings_nav_scroll(&state, nav);
        for (offset, section) in SettingsSection::ALL
            .iter()
            .enumerate()
            .skip(scroll)
            .take(nav.height as usize)
        {
            let row = nav.y + (offset - scroll) as u16;
            assert_eq!(state.settings_tab_at(nav.x, row), Some(*section));
            assert_eq!(
                state.settings_tab_at(nav.x + nav.width - 1, row),
                Some(*section)
            );
        }
        assert_eq!(state.settings_tab_at(nav.x + nav.width, nav.y), None);
    }

    #[test]
    fn general_enter_returns_the_config_edit_for_the_selected_row() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::General);
        assert_eq!(state.settings.section, SettingsSection::General);

        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        assert_eq!(
            action,
            Some(SettingsAction::SaveConfigEdit(
                crate::app::settings_general::ConfigEdit::Bool {
                    section: "ui",
                    key: "combine_repos_across_hosts",
                    value: true,
                }
            ))
        );

        // The read-only path row writes nothing.
        state.settings.list.select(
            crate::app::settings_general::GeneralRow::ALL
                .iter()
                .position(|row| {
                    *row == crate::app::settings_general::GeneralRow::AddProjectStartDir
                })
                .expect("row"),
        );
        assert_eq!(
            update_settings_state(
                &mut state,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
            ),
            None
        );
    }

    #[test]
    fn clicking_a_general_row_selects_it_and_returns_its_edit() {
        let mut app = app_for_mouse_test();
        open_settings_at(&mut app.state, SettingsSection::General);

        let area = app.state.settings_content_rect();
        // Every row that fits maps back to itself, including the hinted first
        // row whose second line belongs to the same row.
        for (index, (offset, height)) in crate::ui::general_row_offsets()
            .into_iter()
            .enumerate()
        {
            for line in 0..height {
                let row = area.y + 2 + offset + line;
                if row >= area.y + area.height {
                    continue;
                }
                assert_eq!(
                    app.state.settings_list_index_at(area.x + 1, row),
                    Some(index),
                    "row {index} line {line}"
                );
            }
        }

        let hide_whitespace = crate::app::settings_general::GeneralRow::ALL
            .iter()
            .position(|row| *row == crate::app::settings_general::GeneralRow::HideWhitespace)
            .expect("row");
        let (offset, _) = crate::ui::general_row_offsets()[hide_whitespace];
        let action = app
            .state
            .handle_settings_mouse(mouse_down(area.x + 1, area.y + 2 + offset));

        assert_eq!(app.state.settings.list.selected, hide_whitespace);
        assert_eq!(
            action,
            Some(SettingsAction::SaveConfigEdit(
                crate::app::settings_general::ConfigEdit::Bool {
                    section: "ui",
                    key: "hide_whitespace_in_diff",
                    value: true,
                }
            ))
        );
    }

    #[test]
    fn archive_delete_asks_once_while_confirm_close_is_on() {
        let mut state = state_with_workspaces(&["test"]);
        let ws_idx = state.active.expect("active workspace");
        let pane_id = state.workspaces[ws_idx].tabs[0].root_pane;
        assert!(state.settle_pane_at(ws_idx, pane_id, 1_700_000_000));
        open_settings_at(&mut state, SettingsSection::Archive);
        assert_eq!(state.archive_entries().len(), 1);

        assert!(state.confirm_close);
        let armed = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::empty()),
        );
        assert_eq!(armed, None);
        assert!(state.settings.archive_delete_armed);

        let deleted = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::empty()),
        );
        assert!(matches!(deleted, Some(SettingsAction::DeleteArchived(_))));
        assert!(!state.settings.archive_delete_armed);
    }

    #[test]
    fn archive_delete_is_immediate_when_confirmation_is_off() {
        let mut state = state_with_workspaces(&["test"]);
        state.confirm_close = false;
        let ws_idx = state.active.expect("active workspace");
        let pane_id = state.workspaces[ws_idx].tabs[0].root_pane;
        assert!(state.settle_pane_at(ws_idx, pane_id, 1_700_000_000));
        open_settings_at(&mut state, SettingsSection::Archive);

        let deleted = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::empty()),
        );
        assert!(matches!(deleted, Some(SettingsAction::DeleteArchived(_))));
    }

    #[test]
    fn archive_enter_restores_the_selected_thread() {
        let mut state = state_with_workspaces(&["test"]);
        let ws_idx = state.active.expect("active workspace");
        let pane_id = state.workspaces[ws_idx].tabs[0].root_pane;
        assert!(state.settle_pane_at(ws_idx, pane_id, 1_700_000_000));
        open_settings_at(&mut state, SettingsSection::Archive);

        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        match action {
            Some(SettingsAction::RestoreArchived(target)) => {
                assert_eq!(target.pane_id, pane_id);
            }
            other => panic!("expected a restore action, got {other:?}"),
        }
    }

    #[test]
    fn keybindings_lists_actions_with_their_keys() {
        let state = state_with_workspaces(&["test"]);
        let rows = crate::ui::settings_keybinding_rows(&state);

        assert!(rows.iter().any(|row| row.heading && row.label == "global"));
        assert!(rows
            .iter()
            .any(|row| !row.heading && row.label == "settings" && !row.key.is_empty()));
    }

    fn mouse_down(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }
    }

    fn integration_recommendation(
        state: crate::integration::IntegrationStatusKind,
        available: bool,
    ) -> crate::integration::IntegrationRecommendation {
        crate::integration::IntegrationRecommendation {
            target: crate::api::schema::IntegrationTarget::Claude,
            label: "claude",
            command: "claude",
            available,
            path: std::path::PathBuf::from("/tmp/herdr-test-integration"),
            state,
        }
    }
}
