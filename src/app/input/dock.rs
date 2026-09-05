use ratatui::layout::Rect;

use crate::app::state::{AppState, DockSurface};

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
        true
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
}
