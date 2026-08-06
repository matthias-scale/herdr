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

    pub(crate) fn set_manual_dock_width(&mut self, divider_col: u16) {
        let screen = self.screen_rect();
        let right = screen.x.saturating_add(screen.width);
        let width = right.saturating_sub(divider_col);
        let width = width.clamp(crate::ui::DOCK_MIN_WIDTH, crate::ui::DOCK_MAX_WIDTH);
        if self.dock_width != width {
            self.dock_width = width;
            self.mark_session_dirty();
        }
    }
}

fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x
        && col < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}
