use ratatui::layout::Rect;

use crate::app::state::{AppState, DockTab};

impl AppState {
    pub(crate) fn on_dock_toggle(&self, col: u16, row: u16) -> bool {
        rect_contains(self.view.dock_handle_rect, col, row)
    }

    pub(crate) fn on_dock_divider(&self, col: u16, row: u16) -> bool {
        !self.dock_collapsed && rect_contains(self.view.dock_divider_rect, col, row)
    }

    pub(crate) fn dock_tab_at(&self, col: u16, row: u16) -> Option<DockTab> {
        if self.dock_collapsed {
            return None;
        }
        self.view
            .dock_tab_hit_areas
            .iter()
            .position(|area| rect_contains(*area, col, row))
            .and_then(|index| DockTab::ALL.get(index).copied())
    }

    pub(crate) fn dock_home_tab_at(&self, col: u16, row: u16) -> Option<usize> {
        if self.dock_collapsed || self.dock_tab != DockTab::Home {
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
        if self.dock_collapsed || self.dock_tab != DockTab::Home {
            return None;
        }
        self.view
            .dock_home_section_hit_areas
            .iter()
            .position(|area| rect_contains(*area, col, row))
            .and_then(|index| match index {
                0 => Some(crate::app::state::DockHomeSection::Prs),
                1 => Some(crate::app::state::DockHomeSection::Tickets),
                _ => None,
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
        app.dock_tab = DockTab::Home;
        app.view.dock_home_tab_hit_areas = vec![Rect::new(80, 2, 8, 1), Rect::new(88, 2, 9, 1)];

        assert_eq!(app.dock_home_tab_at(81, 2), Some(0));
        assert_eq!(app.dock_home_tab_at(90, 2), Some(1));
        assert_eq!(app.dock_home_tab_at(81, 3), None);
        assert_eq!(app.dock_home_tab_at(79, 2), None);
    }
}
