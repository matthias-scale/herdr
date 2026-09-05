use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::Rect;

use crate::{
    app::{
        state::{AppState, DockSurface},
        App,
    },
    input::TerminalKey,
};

impl App {
    /// The open surface menu owns every key so the focused surface cannot
    /// receive input until the menu closes.
    pub(crate) fn handle_dock_surface_menu_key(&mut self, key: &TerminalKey) -> bool {
        if self.state.dock_surface_menu.is_none() {
            return false;
        }

        let event = key.as_key_event();
        let count = DockSurface::ALL.len();
        match event.code {
            KeyCode::Esc => {
                self.state.dock_surface_menu = None;
            }
            KeyCode::Down => {
                if let Some(menu) = self.state.dock_surface_menu.as_mut() {
                    menu.selected = (menu.selected + 1) % count;
                }
            }
            KeyCode::Up => {
                if let Some(menu) = self.state.dock_surface_menu.as_mut() {
                    menu.selected = (menu.selected + count - 1) % count;
                }
            }
            KeyCode::Enter => {
                let selected = self
                    .state
                    .dock_surface_menu
                    .and_then(|menu| DockSurface::ALL.get(menu.selected).copied());
                if let Some(surface) = selected {
                    self.state.activate_dock_surface(surface);
                }
            }
            KeyCode::Char(character)
                if event.modifiers.is_empty() || event.modifiers == KeyModifiers::SHIFT =>
            {
                if let Some(surface) = DockSurface::from_shortcut(character) {
                    self.state.activate_dock_surface(surface);
                }
            }
            _ => {}
        }
        true
    }
}

impl AppState {
    pub(crate) fn on_dock_toggle(&self, col: u16, row: u16) -> bool {
        rect_contains(self.view.dock_handle_rect, col, row)
    }

    pub(crate) fn on_dock_divider(&self, col: u16, row: u16) -> bool {
        !self.dock_collapsed && rect_contains(self.view.dock_divider_rect, col, row)
    }

    pub(crate) fn dock_tab_at(&self, col: u16, row: u16) -> Option<DockSurface> {
        if self.dock_collapsed {
            return None;
        }
        self.view
            .dock_tab_hit_areas
            .iter()
            .position(|area| rect_contains(*area, col, row))
            .and_then(|index| self.dock_open_surfaces.get(index).copied())
    }

    /// The close glyph of the active tab.
    pub(crate) fn on_dock_tab_close(&self, col: u16, row: u16) -> bool {
        !self.dock_collapsed && rect_contains(self.view.dock_tab_close_rect, col, row)
    }

    pub(crate) fn on_dock_plus(&self, col: u16, row: u16) -> bool {
        !self.dock_collapsed && rect_contains(self.view.dock_plus_rect, col, row)
    }

    pub(crate) fn on_dock_maximize(&self, col: u16, row: u16) -> bool {
        !self.dock_collapsed && rect_contains(self.view.dock_maximize_rect, col, row)
    }

    /// Card of the empty-dock grid under the cursor, available or not. The
    /// caller decides what an unavailable card does, so the geometry stays a
    /// pure function of the rect.
    pub(crate) fn dock_surface_card_at(&self, col: u16, row: u16) -> Option<DockSurface> {
        if self.dock_collapsed || self.dock_tab.is_some() {
            return None;
        }
        self.view
            .dock_surface_card_hit_areas
            .iter()
            .position(|area| rect_contains(*area, col, row))
            .and_then(|index| DockSurface::CARDS.get(index).copied())
    }

    /// Row of the open `+` menu under the cursor.
    pub(crate) fn dock_surface_menu_at(&self, col: u16, row: u16) -> Option<DockSurface> {
        let layout = self.view.dock_surface_menu_layout?;
        crate::ui::dropdown::hit_test(&layout, col, row)
            .and_then(|index| DockSurface::ALL.get(index).copied())
    }

    /// Open `surface` unless the focused pane makes it useless. A disabled card
    /// or menu row is inert rather than opening an empty surface.
    pub(crate) fn activate_dock_surface(&mut self, surface: DockSurface) -> bool {
        let (context, in_git_repo) = crate::ui::dock::chooser::focused_availability(self);
        if !crate::ui::dock::chooser::surface_available(surface, &context, in_git_repo) {
            return false;
        }
        self.open_dock_surface(surface);
        self.dock_scroll = 0;
        self.dock_editor_focused = surface == DockSurface::Editor;
        self.dock_home_focused = surface == DockSurface::Home;
        self.dock_diff_focused = surface == DockSurface::Diff;
        self.dock_files_focused = surface == DockSurface::Files;
        true
    }

    pub(crate) fn toggle_dock_diff_whitespace(&mut self) {
        self.dock_diff_ignore_whitespace = !self.dock_diff_ignore_whitespace;
        self.dock_diff_active_key = None;
        self.dock_diff_request = None;
        self.dock_scroll = 0;
    }

    pub(crate) fn toggle_selected_dock_diff_file(&mut self) -> bool {
        let Some(key) = self.dock_diff_active_key.as_ref() else {
            return false;
        };
        let Some(file) = self
            .dock_diff_cache
            .get(key)
            .and_then(|entry| entry.files.get(self.dock_diff_selected))
        else {
            return false;
        };
        if !self.dock_diff_collapsed.remove(&file.path) {
            self.dock_diff_collapsed.insert(file.path.clone());
        }
        true
    }

    pub(crate) fn dock_diff_file_at(&self, col: u16, row: u16) -> Option<usize> {
        if self.dock_collapsed || self.dock_tab != Some(DockSurface::Diff) {
            return None;
        }
        crate::ui::dock::diff::file_index_at(self, self.view.dock_body_rect, col, row)
    }

    pub(crate) fn on_dock_diff_whitespace_toggle(&self, col: u16, row: u16) -> bool {
        !self.dock_collapsed
            && self.dock_tab == Some(DockSurface::Diff)
            && crate::ui::dock::diff::whitespace_toggle_at(self.view.dock_body_rect, col, row)
    }

    pub(crate) fn toggle_dock_surface_menu(&mut self) {
        self.dock_surface_menu = match self.dock_surface_menu {
            Some(_) => None,
            None => Some(crate::app::state::DockSurfaceMenu { selected: 0 }),
        };
    }

    pub(crate) fn dock_home_tab_at(&self, col: u16, row: u16) -> Option<usize> {
        if self.dock_collapsed || self.dock_tab != Some(DockSurface::Home) {
            return None;
        }
        self.view
            .dock_home_tab_hit_areas
            .iter()
            .position(|area| rect_contains(*area, col, row))
    }

    pub(crate) fn dock_home_section_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<crate::app::state::DockHomeSection> {
        if self.dock_collapsed || self.dock_tab != Some(DockSurface::Home) {
            return None;
        }
        self.view
            .dock_home_section_hit_areas
            .iter()
            .position(|area| rect_contains(*area, col, row))
            .and_then(|index| match index {
                0 => Some(crate::app::state::DockHomeSection::Prs),
                1 => Some(crate::app::state::DockHomeSection::Tickets),
                2 => Some(crate::app::state::DockHomeSection::XPolls),
                _ => None,
            })
    }

    pub(crate) fn dock_home_detail_tab_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<crate::app::state::DockHomeDetailTab> {
        self.view
            .dock_home_detail_tab_hit_areas
            .iter()
            .position(|area| rect_contains(*area, col, row))
            .and_then(|index| {
                crate::app::state::DockHomeDetailTab::ALL
                    .get(index)
                    .copied()
            })
    }

    pub(crate) fn set_manual_dock_width(&mut self, divider_col: u16) {
        let screen = self.screen_rect();
        let right = screen.x.saturating_add(screen.width);
        let width = right.saturating_sub(divider_col);
        let width = width.clamp(crate::ui::DOCK_MIN_WIDTH, crate::ui::DOCK_MAX_WIDTH);
        self.set_dock_width(width);
    }
}

fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x
        && col < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::{DiffCacheEntry, DiffCacheKey, DiffFileSummary};
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn dock_home_tab_at_maps_each_horizontal_hit_area() {
        let mut app = AppState::test_new();
        app.dock_collapsed = false;
        app.dock_tab = Some(DockSurface::Home);
        app.view.dock_home_tab_hit_areas = vec![Rect::new(80, 2, 8, 1), Rect::new(88, 2, 9, 1)];

        assert_eq!(app.dock_home_tab_at(81, 2), Some(0));
        assert_eq!(app.dock_home_tab_at(90, 2), Some(1));
        assert_eq!(app.dock_home_tab_at(81, 3), None);
        assert_eq!(app.dock_home_tab_at(79, 2), None);
    }

    #[test]
    fn dock_home_detail_tab_at_maps_the_sub_tab_row() {
        let mut app = AppState::test_new();
        app.view.dock_home_detail_tab_hit_areas =
            vec![Rect::new(80, 4, 8, 1), Rect::new(88, 4, 9, 1)];

        assert_eq!(
            app.dock_home_detail_tab_at(81, 4),
            Some(crate::app::state::DockHomeDetailTab::Overview)
        );
        assert_eq!(
            app.dock_home_detail_tab_at(90, 4),
            Some(crate::app::state::DockHomeDetailTab::Comments)
        );
        assert_eq!(app.dock_home_detail_tab_at(81, 5), None);
    }

    #[test]
    fn opening_a_surface_appends_a_tab_and_activates_it() {
        let mut app = AppState::test_new();
        app.dock_collapsed = false;

        app.open_dock_surface(DockSurface::Files);

        assert_eq!(app.dock_tab, Some(DockSurface::Files));
        assert_eq!(
            app.dock_open_surfaces.last().copied(),
            Some(DockSurface::Files)
        );

        // Reopening an already-open surface must not move it in the strip.
        let before = app.dock_open_surfaces.clone();
        app.open_dock_surface(DockSurface::Home);
        assert_eq!(app.dock_open_surfaces, before);
        assert_eq!(app.dock_tab, Some(DockSurface::Home));
    }

    #[test]
    fn closing_the_active_surface_moves_to_its_neighbour() {
        let mut app = AppState::test_new();
        app.dock_collapsed = false;
        app.dock_open_surfaces = vec![
            DockSurface::Home,
            DockSurface::Editor,
            DockSurface::Scratchpad,
        ];
        app.dock_tab = Some(DockSurface::Editor);

        app.close_dock_surface(DockSurface::Editor);
        assert_eq!(
            app.dock_open_surfaces,
            vec![DockSurface::Home, DockSurface::Scratchpad]
        );
        assert_eq!(app.dock_tab, Some(DockSurface::Scratchpad));

        // Closing the last tab falls back to the one before it.
        app.close_dock_surface(DockSurface::Scratchpad);
        assert_eq!(app.dock_tab, Some(DockSurface::Home));
    }

    #[test]
    fn closing_every_surface_leaves_the_chooser() {
        let mut app = AppState::test_new();
        app.dock_collapsed = false;
        for surface in DockSurface::DEFAULT_OPEN {
            app.close_dock_surface(surface);
        }

        assert!(app.dock_open_surfaces.is_empty());
        assert_eq!(app.dock_tab, None);
        assert!(app.dock_chooser_focused);
        assert!(!app.dock_home_focused);
        assert!(!app.dock_editor_focused);
    }

    #[test]
    fn an_unavailable_surface_refuses_to_open() {
        let mut app = AppState::test_new();
        app.dock_collapsed = false;
        app.dock_open_surfaces.clear();
        app.dock_tab = None;

        // No focused pane: no pull request and no ticket.
        assert!(!app.activate_dock_surface(DockSurface::Pr));
        assert!(!app.activate_dock_surface(DockSurface::Linear));
        assert!(app.dock_open_surfaces.is_empty());
        assert_eq!(app.dock_tab, None);

        assert!(app.activate_dock_surface(DockSurface::Terminal));
        assert_eq!(app.dock_tab, Some(DockSurface::Terminal));
    }

    #[test]
    fn the_maximise_toggle_is_reversible() {
        let mut app = AppState::test_new();
        assert!(!app.dock_maximized);
        app.toggle_dock_maximized();
        assert!(app.dock_maximized);
        app.toggle_dock_maximized();
        assert!(!app.dock_maximized);
    }

    #[test]
    fn the_plus_menu_toggles_and_the_tab_hit_areas_track_open_surfaces() {
        let mut app = AppState::test_new();
        app.dock_collapsed = false;
        app.dock_open_surfaces = vec![DockSurface::Files, DockSurface::Diff];
        app.dock_tab = Some(DockSurface::Files);
        app.view.dock_tab_hit_areas = vec![Rect::new(80, 1, 8, 1), Rect::new(88, 1, 5, 1)];
        app.view.dock_plus_rect = Rect::new(93, 1, 2, 1);

        assert_eq!(app.dock_tab_at(81, 1), Some(DockSurface::Files));
        assert_eq!(app.dock_tab_at(89, 1), Some(DockSurface::Diff));
        assert_eq!(app.dock_tab_at(94, 1), None);
        assert!(app.on_dock_plus(93, 1));

        app.toggle_dock_surface_menu();
        assert!(app.dock_surface_menu.is_some());
        app.toggle_dock_surface_menu();
        assert!(app.dock_surface_menu.is_none());
    }

    #[test]
    fn card_hits_only_land_while_the_dock_is_a_chooser() {
        let mut app = AppState::test_new();
        app.dock_collapsed = false;
        app.dock_open_surfaces.clear();
        app.dock_tab = None;
        app.view.dock_surface_card_hit_areas =
            vec![Rect::new(80, 4, 12, 4), Rect::new(93, 4, 12, 4)];

        assert_eq!(app.dock_surface_card_at(81, 5), Some(DockSurface::Terminal));
        assert_eq!(app.dock_surface_card_at(94, 5), Some(DockSurface::Files));
        assert_eq!(app.dock_surface_card_at(81, 9), None);

        app.dock_tab = Some(DockSurface::Home);
        assert_eq!(app.dock_surface_card_at(81, 5), None);
    }

    #[test]
    fn diff_collapse_state_is_kept_per_file() {
        let mut app = AppState::test_new();
        let key = DiffCacheKey {
            root: PathBuf::from("/repo"),
            base: "main".into(),
            ignore_whitespace: false,
        };
        app.dock_diff_cache.insert(
            key.clone(),
            DiffCacheEntry {
                branch: "feature".into(),
                files: ["one.rs", "two.rs"]
                    .into_iter()
                    .map(|path| DiffFileSummary {
                        path: path.into(),
                        display_path: path.into(),
                        additions: 1,
                        deletions: 0,
                        binary: false,
                    })
                    .collect(),
                contents: HashMap::new(),
                error: None,
            },
        );
        app.dock_diff_active_key = Some(key);

        assert!(app.toggle_selected_dock_diff_file());
        assert!(app.dock_diff_collapsed.contains("one.rs"));
        assert!(!app.dock_diff_collapsed.contains("two.rs"));
        app.dock_diff_selected = 1;
        assert!(app.toggle_selected_dock_diff_file());
        assert!(app.dock_diff_collapsed.contains("one.rs"));
        assert!(app.dock_diff_collapsed.contains("two.rs"));
        app.dock_diff_selected = 0;
        assert!(app.toggle_selected_dock_diff_file());
        assert!(!app.dock_diff_collapsed.contains("one.rs"));
        assert!(app.dock_diff_collapsed.contains("two.rs"));
    }
}
