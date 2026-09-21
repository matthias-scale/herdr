use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::app::state::{AppState, ViewLayout};

use super::ScrollbarClickTarget;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SettledMenuAction {
    Resume(crate::app::state::PaneFocusTarget),
    FocusLive(crate::app::state::PaneFocusTarget),
    NewThread {
        directory: std::path::PathBuf,
        workspace: crate::app::home::HomeWorkspace,
    },
    Delete(crate::app::state::PaneFocusTarget),
}

pub(crate) enum SidebarWorkGroupKeyAction {
    Ignored,
    Consumed,
    Dispatch(Box<crate::app::home::HomeDispatchPlan>),
}

fn sidebar_snooze_params(
    pane_id: String,
    preset: crate::app::state::SidebarSnoozePreset,
    tomorrow_morning: Option<u64>,
) -> Option<crate::api::schema::PaneSnoozeParams> {
    let (duration_s, snoozed_until) = match preset {
        crate::app::state::SidebarSnoozePreset::Duration(duration_s) => (Some(duration_s), None),
        crate::app::state::SidebarSnoozePreset::TomorrowMorning => (None, Some(tomorrow_morning?)),
    };
    Some(crate::api::schema::PaneSnoozeParams {
        pane_id,
        duration_s,
        snoozed_until,
    })
}

impl AppState {
    pub(crate) fn settled_target_has_resume_plan(
        &self,
        target: &crate::app::state::PaneFocusTarget,
    ) -> bool {
        self.workspaces
            .iter()
            .find(|workspace| workspace.id == target.workspace_id)
            .and_then(|workspace| workspace.pane_state(target.pane_id))
            .and_then(|pane| self.terminals.get(&pane.attached_terminal_id))
            .is_some_and(crate::app::settled::pane_has_resume_plan)
    }

    pub(crate) fn open_sidebar_new_menu(&mut self) {
        self.sidebar_group_menu_open = false;
        self.sidebar_filter_menu_open = false;
        self.sidebar_object_menu = None;
        self.sidebar_search_active = false;
        self.sidebar_new_thread = None;
        self.sidebar_new_menu = Some(Default::default());
    }

    pub(crate) fn sidebar_new_menu_item_at(&self, col: u16, row: u16) -> Option<usize> {
        let layout = crate::ui::sidebar_new_menu_layout(self, self.screen_rect())?;
        crate::ui::dropdown::hit_test(&layout, col, row)
    }

    pub(crate) fn dispatch_sidebar_new_menu_action(
        &mut self,
        action: crate::app::state::SidebarNewMenuAction,
    ) {
        self.sidebar_new_menu = None;
        match action {
            crate::app::state::SidebarNewMenuAction::NewSpace => {
                self.request_new_workspace = true;
            }
            crate::app::state::SidebarNewMenuAction::AddProject => {
                self.open_add_project_from_sidebar();
            }
            crate::app::state::SidebarNewMenuAction::NewThread => {
                self.open_sidebar_new_thread();
            }
            crate::app::state::SidebarNewMenuAction::OpenFolder => {
                self.open_folder_from_sidebar();
            }
        }
    }

    pub(crate) fn select_sidebar_new_menu_item(&mut self, index: usize) -> bool {
        let Some(action) = crate::app::state::SidebarNewMenuAction::ALL
            .get(index)
            .copied()
        else {
            return false;
        };
        self.dispatch_sidebar_new_menu_action(action);
        true
    }

    pub(crate) fn handle_sidebar_new_menu_key(&mut self, key: KeyEvent) -> bool {
        let Some(mut menu) = self.sidebar_new_menu.take() else {
            return false;
        };
        match key.code {
            KeyCode::Esc => {}
            KeyCode::Up => {
                menu.selected = menu.selected.saturating_sub(1);
                self.sidebar_new_menu = Some(menu);
            }
            KeyCode::Down => {
                menu.selected = menu
                    .selected
                    .saturating_add(1)
                    .min(crate::app::state::SidebarNewMenuAction::ALL.len() - 1);
                self.sidebar_new_menu = Some(menu);
            }
            KeyCode::Enter => {
                self.select_sidebar_new_menu_item(menu.selected);
            }
            _ => self.sidebar_new_menu = Some(menu),
        }
        true
    }

    pub(crate) fn open_sidebar_new_thread(&mut self) {
        self.sidebar_new_menu = None;
        self.sidebar_group_menu_open = false;
        self.sidebar_filter_menu_open = false;
        self.sidebar_object_menu = None;
        self.sidebar_search_active = false;
        self.sidebar_new_thread = Some(Default::default());
    }

    pub(crate) fn sidebar_new_thread_item_at(&self, col: u16, row: u16) -> Option<usize> {
        let layout = crate::ui::sidebar_new_thread_layout(self, self.screen_rect())?;
        crate::ui::dropdown::hit_test(&layout, col, row)
    }

    pub(crate) fn accept_sidebar_new_thread(&mut self, position: usize) -> bool {
        let Some((path_index, _)) = crate::ui::sidebar_new_thread_matches(self)
            .get(position)
            .cloned()
        else {
            return false;
        };
        let Some(directory) = self
            .new_thread_options()
            .get(path_index)
            .map(|option| option.path.clone())
        else {
            return false;
        };
        self.sidebar_new_thread = None;
        self.open_home_composer_in_directory(directory, self.default_home_workspace());
        true
    }

    pub(crate) fn handle_sidebar_new_thread_key(&mut self, key: KeyEvent) -> bool {
        let Some(mut picker) = self.sidebar_new_thread.take() else {
            return false;
        };
        let match_count = {
            self.sidebar_new_thread = Some(picker.clone());
            let count = crate::ui::sidebar_new_thread_matches(self).len();
            self.sidebar_new_thread = None;
            count
        };
        match key.code {
            KeyCode::Esc => return true,
            KeyCode::Up => picker.filter.move_selection(-1, match_count),
            KeyCode::Down => picker.filter.move_selection(1, match_count),
            KeyCode::Backspace => picker.filter.pop(),
            KeyCode::Enter => {
                let selected = picker.filter.selected;
                self.sidebar_new_thread = Some(picker);
                self.accept_sidebar_new_thread(selected);
                return true;
            }
            KeyCode::Char(character)
                if key.modifiers.is_empty() && ('1'..='9').contains(&character) =>
            {
                let selected = character.to_digit(10).unwrap_or(1) as usize - 1;
                self.sidebar_new_thread = Some(picker);
                self.accept_sidebar_new_thread(selected);
                return true;
            }
            KeyCode::Char(character)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                picker.filter.push(character);
            }
            _ => {}
        }
        self.sidebar_new_thread = Some(picker);
        true
    }

    pub(crate) fn open_sidebar_project_menu(&mut self) {
        self.sidebar_new_menu = None;
        self.sidebar_new_thread = None;
        self.sidebar_group_menu_open = false;
        self.sidebar_filter_menu_open = false;
        self.sidebar_object_menu = None;
        self.sidebar_search_active = false;
        self.sidebar_project_menu = Some(Default::default());
    }

    pub(crate) fn sidebar_project_menu_item_at(&self, col: u16, row: u16) -> Option<usize> {
        let layout = crate::ui::sidebar_project_menu_layout(self, self.screen_rect())?;
        crate::ui::dropdown::hit_test(&layout, col, row)
    }

    /// Apply the row at `position` in the filtered list. Row 0 of the unfiltered
    /// list clears the scope; every other row names a project.
    pub(crate) fn accept_sidebar_project_menu(&mut self, position: usize) -> bool {
        let Some((index, _)) = crate::ui::sidebar_project_menu_matches(self)
            .get(position)
            .cloned()
        else {
            return false;
        };
        let project = match index.checked_sub(1) {
            Some(project) => match self.projects.get(project) {
                Some(project) => Some(project.id.clone()),
                None => return false,
            },
            None => None,
        };
        self.sidebar_project_menu = None;
        let mut filter = self.sidebar_work_filter.clone();
        filter.project = project;
        self.set_sidebar_work_filter(filter);
        true
    }

    pub(crate) fn handle_sidebar_project_menu_key(&mut self, key: KeyEvent) -> bool {
        let Some(mut menu) = self.sidebar_project_menu.take() else {
            return false;
        };
        let match_count = {
            self.sidebar_project_menu = Some(menu.clone());
            let count = crate::ui::sidebar_project_menu_matches(self).len();
            self.sidebar_project_menu = None;
            count
        };
        match key.code {
            KeyCode::Esc => return true,
            KeyCode::Up => menu.filter.move_selection(-1, match_count),
            KeyCode::Down => menu.filter.move_selection(1, match_count),
            KeyCode::Backspace => menu.filter.pop(),
            KeyCode::Enter => {
                let selected = menu.filter.selected;
                self.sidebar_project_menu = Some(menu);
                self.accept_sidebar_project_menu(selected);
                return true;
            }
            KeyCode::Char(character)
                if key.modifiers.is_empty() && ('1'..='9').contains(&character) =>
            {
                let selected = character.to_digit(10).unwrap_or(1) as usize - 1;
                self.sidebar_project_menu = Some(menu);
                self.accept_sidebar_project_menu(selected);
                return true;
            }
            KeyCode::Char(character)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                menu.filter.push(character);
            }
            _ => {}
        }
        self.sidebar_project_menu = Some(menu);
        true
    }

    pub(crate) fn handle_sidebar_search_key(&mut self, key: KeyEvent) -> bool {
        if !self.sidebar_search_active {
            return false;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Enter => self.sidebar_search_active = false,
            KeyCode::Backspace => {
                let mut filter = self.sidebar_work_filter.clone();
                filter.query.pop();
                self.set_sidebar_work_filter(filter);
                self.sidebar_search_active = true;
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let mut filter = self.sidebar_work_filter.clone();
                filter.query.clear();
                self.set_sidebar_work_filter(filter);
                self.sidebar_search_active = true;
            }
            KeyCode::Char(character)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                let mut filter = self.sidebar_work_filter.clone();
                filter.query.push(character);
                self.set_sidebar_work_filter(filter);
                self.sidebar_search_active = true;
            }
            _ => {}
        }
        true
    }

    pub(crate) fn open_sidebar_object_menu(&mut self, target: String) {
        if self.dock_pending_write.is_some() {
            return;
        }
        self.sidebar_selected_work_group = Some(target.clone());
        self.sidebar_group_menu_open = false;
        self.sidebar_filter_menu_open = false;
        self.sidebar_settled_menu_target = None;
        self.sidebar_object_menu = Some(crate::app::state::SidebarObjectMenuState {
            target,
            anchor_row: None,
            page: crate::app::state::SidebarObjectMenuPage::Actions,
            selected: 0,
        });
    }

    pub(crate) fn open_sidebar_sort_menu(
        &mut self,
        target: String,
        current: crate::app::state::SidebarSortMode,
        anchor: (u16, u16),
    ) {
        self.sidebar_group_menu_open = false;
        self.sidebar_filter_menu_open = false;
        self.sidebar_object_menu = None;
        self.sidebar_settled_menu_target = None;
        self.sidebar_sort_menu = Some(crate::app::state::SidebarSortMenuState {
            target,
            current,
            anchor,
            selected: current.index(),
        });
    }

    pub(crate) fn sidebar_sort_menu_item_at(&self, col: u16, row: u16) -> Option<usize> {
        crate::ui::sidebar::sidebar_sort_menu_item_at(self, self.screen_rect(), col, row)
    }

    pub(crate) fn handle_sidebar_sort_menu_key(&mut self, key: KeyEvent) -> bool {
        let Some(menu) = self.sidebar_sort_menu.as_mut() else {
            return false;
        };
        let count = crate::app::state::SidebarSortMode::ALL.len();
        match key.code {
            KeyCode::Esc if key.modifiers.is_empty() => self.sidebar_sort_menu = None,
            KeyCode::Up | KeyCode::Char('k') if key.modifiers.is_empty() => {
                menu.selected = menu.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') if key.modifiers.is_empty() => {
                menu.selected = menu.selected.saturating_add(1).min(count - 1);
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                let target = menu.target.clone();
                let mode = crate::app::state::SidebarSortMode::ALL
                    .get(menu.selected)
                    .copied()
                    .unwrap_or_default();
                self.set_sidebar_group_sort(target, mode);
            }
            _ => {}
        }
        true
    }

    pub(crate) fn apply_sidebar_sort_menu_selection(&mut self, index: usize) {
        let Some(menu) = self.sidebar_sort_menu.as_ref() else {
            return;
        };
        let Some(mode) = crate::app::state::SidebarSortMode::ALL.get(index).copied() else {
            self.sidebar_sort_menu = None;
            return;
        };
        let target = menu.target.clone();
        self.set_sidebar_group_sort(target, mode);
    }

    pub(crate) fn sidebar_subgroup_picker_item_at(&self, col: u16, row: u16) -> Option<usize> {
        let layout = crate::ui::sidebar::sidebar_subgroup_picker_layout(self, self.screen_rect())?;
        crate::ui::dropdown::hit_test(&layout, col, row)
    }

    pub(crate) fn handle_sidebar_subgroup_picker_key(&mut self, key: KeyEvent) -> bool {
        if self.sidebar_subgroup_picker.is_none() {
            return false;
        }
        let row_count = crate::ui::sidebar::sidebar_subgroup_picker_choices(self).len();
        let Some(mut picker) = self.sidebar_subgroup_picker.take() else {
            return false;
        };
        match key.code {
            KeyCode::Esc => return true,
            KeyCode::Up => picker.filter.move_selection(-1, row_count),
            KeyCode::Down => picker.filter.move_selection(1, row_count),
            KeyCode::Backspace => picker.filter.pop(),
            KeyCode::Enter => {
                let selected = picker.filter.selected;
                self.sidebar_subgroup_picker = Some(picker);
                self.accept_sidebar_subgroup_picker(selected);
                return true;
            }
            KeyCode::Char(character)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                picker.filter.push(character);
            }
            _ => {}
        }
        self.sidebar_subgroup_picker = Some(picker);
        true
    }

    pub(crate) fn accept_sidebar_subgroup_picker(&mut self, index: usize) {
        let choices = crate::ui::sidebar::sidebar_subgroup_picker_choices(self);
        let Some(picker) = self.sidebar_subgroup_picker.take() else {
            return;
        };
        let Some(name) = choices.get(index).map(|choice| choice.name().to_string()) else {
            return;
        };
        if let Some(tab) = self
            .workspaces
            .get_mut(picker.ws_idx)
            .and_then(|workspace| workspace.tabs.get_mut(picker.tab_idx))
        {
            tab.set_subgroup(Some(name));
            self.mark_session_dirty();
        }
    }

    /// Remove the window's subgroup outright, from the tab context menu.
    pub(crate) fn clear_tab_subgroup(&mut self, ws_idx: usize, tab_idx: usize) {
        if let Some(tab) = self
            .workspaces
            .get_mut(ws_idx)
            .and_then(|workspace| workspace.tabs.get_mut(tab_idx))
        {
            if tab.subgroup.is_some() {
                tab.set_subgroup(None);
                self.mark_session_dirty();
            }
        }
    }

    pub(crate) fn sidebar_settled_target_at(
        &self,
        row: u16,
    ) -> Option<crate::app::state::PaneFocusTarget> {
        let (ws_idx, _, pane_id) = self.sidebar_local_pane_at(row)?;
        self.pane_is_settled(ws_idx, pane_id)
            .then(|| crate::app::state::PaneFocusTarget {
                workspace_id: self.workspaces[ws_idx].id.clone(),
                pane_id,
            })
    }

    pub(crate) fn sidebar_local_pane_at(
        &self,
        row: u16,
    ) -> Option<(usize, usize, crate::layout::PaneId)> {
        crate::ui::compute_tab_card_areas(self, self.view.sidebar_rect)
            .into_iter()
            .find(|card| row >= card.rect.y && row < card.rect.bottom())
            .map(|card| (card.ws_idx, card.tab_idx, card.pane_id))
            .or_else(|| self.agent_detail_target_at(row))
            .or_else(|| {
                self.sidebar_settled_workspace_target_at(row)
                    .and_then(|(ws_idx, pane_id)| {
                        let tab_idx = self.workspaces[ws_idx]
                            .tabs
                            .iter()
                            .position(|tab| tab.panes.contains_key(&pane_id))?;
                        Some((ws_idx, tab_idx, pane_id))
                    })
            })
    }

    fn sidebar_settled_workspace_target_at(
        &self,
        row: u16,
    ) -> Option<(usize, crate::layout::PaneId)> {
        let (cards, _) = crate::ui::compute_sidebar_row_areas(self, self.view.sidebar_rect);
        let card = cards
            .iter()
            .find(|card| row >= card.rect.y && row < card.rect.bottom())?;
        let pane_id = card.settled_pane_id?;
        Some((card.ws_idx, pane_id))
    }

    pub(crate) fn sidebar_settled_menu_item_at(&self, col: u16, row: u16) -> Option<usize> {
        let layout = crate::ui::sidebar_settled_menu_layout(self, self.screen_rect())?;
        crate::ui::dropdown::hit_test(&layout, col, row)
    }

    pub(crate) fn sidebar_snooze_menu_item_at(&self, col: u16, row: u16) -> Option<usize> {
        let layout = crate::ui::sidebar_snooze_menu_layout(self, self.screen_rect())?;
        crate::ui::dropdown::hit_test(&layout, col, row)
    }

    /// Resolve a settled-menu row press into an action.
    ///
    /// The delete row asks once first while `ui.confirm_close` is on: the first
    /// press arms the row and returns nothing, the second press deletes. Every
    /// other row disarms it, so an armed delete cannot fire from a later press
    /// on a different row.
    pub(crate) fn select_settled_menu_action(&mut self, index: usize) -> Option<SettledMenuAction> {
        let delete_armed = std::mem::take(&mut self.sidebar_settled_menu_delete_armed);
        if index == 3 && self.confirm_close && !delete_armed {
            self.sidebar_settled_menu_delete_armed = true;
            return None;
        }
        let target = self.sidebar_settled_menu_target.take()?;
        self.sidebar_selected_settled = None;
        let ws_idx = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == target.workspace_id)?;
        let pane = self.workspaces[ws_idx].pane_state(target.pane_id)?;
        let terminal = self.terminals.get(&pane.attached_terminal_id)?;
        match index {
            0 if self.settled_target_has_resume_plan(&target) => {
                Some(SettledMenuAction::Resume(target))
            }
            0 => Some(SettledMenuAction::FocusLive(target)),
            1 => Some(SettledMenuAction::NewThread {
                directory: terminal.cwd.clone(),
                workspace: crate::app::home::HomeWorkspace::CurrentCheckout,
            }),
            2 => Some(SettledMenuAction::NewThread {
                directory: terminal.cwd.clone(),
                workspace: crate::app::home::HomeWorkspace::NewWorktree,
            }),
            3 => Some(SettledMenuAction::Delete(target)),
            _ => None,
        }
    }

    /// Whether this point belongs to the sidebar for keyboard purposes.
    ///
    /// The sidebar body plus whatever menu it currently has open, which the
    /// renderer floats outside the sidebar rectangle. A collapsed sidebar
    /// claims nothing: its only control expands it, and expanding is not the
    /// operator asking to drive it from the keyboard.
    pub(crate) fn sidebar_claims_pointer(&self, col: u16, row: u16) -> bool {
        if self.sidebar_collapsed {
            return false;
        }
        if self.point_in_rect(self.view.sidebar_rect, col, row) {
            return true;
        }
        self.sidebar_new_menu_item_at(col, row).is_some()
            || self.sidebar_new_thread_item_at(col, row).is_some()
            || self.sidebar_project_menu_item_at(col, row).is_some()
            || self.sidebar_group_menu_item_at(col, row).is_some()
            || self.sidebar_filter_menu_item_at(col, row).is_some()
            || self.sidebar_sort_menu_item_at(col, row).is_some()
            || self.sidebar_subgroup_picker_item_at(col, row).is_some()
            || crate::ui::sidebar_object_menu_item_at(self, self.screen_rect(), col, row).is_some()
    }

    pub(crate) fn sidebar_group_mode_anchor_rect(&self) -> Rect {
        crate::ui::sidebar_group_mode_anchor_rect(self.view.sidebar_rect)
    }

    pub(crate) fn sidebar_group_menu_item_at(&self, col: u16, row: u16) -> Option<usize> {
        let layout = crate::ui::sidebar_group_menu_layout(self, self.screen_rect())?;
        crate::ui::dropdown::hit_test(&layout, col, row)
    }

    pub(crate) fn open_sidebar_group_menu(&mut self) {
        self.sidebar_group_menu_selected = self.sidebar_group_mode.view_index();
        self.sidebar_group_menu_open = true;
    }

    pub(crate) fn sidebar_filter_anchor_rect(&self) -> Rect {
        crate::ui::sidebar_filter_anchor_rect(self, self.view.sidebar_rect)
    }

    pub(crate) fn sidebar_filter_menu_item_at(&self, col: u16, row: u16) -> Option<usize> {
        let layout = crate::ui::sidebar_filter_menu_layout(self, self.screen_rect())?;
        crate::ui::dropdown::hit_test(&layout, col, row)
    }

    pub(crate) fn open_sidebar_filter_menu(&mut self) {
        self.sidebar_filter_menu_selected = 0;
        self.sidebar_group_menu_open = false;
        self.sidebar_filter_menu_open = true;
    }

    /// Apply the option at `index` to the stored filter. A team option replaces
    /// the team and leaves the assignee alone, and vice versa, so the two
    /// narrowings compose instead of resetting each other.
    pub(crate) fn select_sidebar_filter_option(&mut self, index: usize) {
        let Some(option) = crate::ui::sidebar_filter_options(self)
            .into_iter()
            .nth(index)
        else {
            self.sidebar_filter_menu_open = false;
            return;
        };
        let mut filter = self.sidebar_work_filter.clone();
        let mut keep_open = false;
        match option {
            crate::ui::SidebarFilterOption::LinearTeam(team) => filter.team = team,
            crate::ui::SidebarFilterOption::LinearOwnership(ownership) => {
                filter.linear_ownership = ownership;
            }
            crate::ui::SidebarFilterOption::LinearAssignee(assignee) => {
                filter.assignee = assignee;
            }
            crate::ui::SidebarFilterOption::LinearStatus(status, selected) => {
                if selected {
                    filter.linear_statuses.remove(&status);
                } else {
                    filter.linear_statuses.insert(status);
                }
                keep_open = true;
            }
            crate::ui::SidebarFilterOption::GithubAssignee(assignee) => {
                filter.github.assignee = assignee;
            }
            crate::ui::SidebarFilterOption::GithubOwnership(ownership) => {
                filter.github.ownership = ownership;
            }
            crate::ui::SidebarFilterOption::GithubDrafts(shown) => {
                filter.github.show_drafts = !shown;
                keep_open = true;
            }
            crate::ui::SidebarFilterOption::GithubState(state) => {
                filter.github.state = state;
            }
            crate::ui::SidebarFilterOption::MissiveTeam(team) => {
                filter.missive.team = team;
            }
            crate::ui::SidebarFilterOption::MissiveAssignee(assignee) => {
                filter.missive.assignee = assignee;
            }
            crate::ui::SidebarFilterOption::MissiveClosed(shown) => {
                filter.missive.show_closed = !shown;
                keep_open = true;
            }
        }
        self.set_sidebar_work_filter(filter);
        if keep_open {
            self.sidebar_filter_menu_open = true;
            self.sidebar_filter_menu_selected = index;
        }
    }

    pub(crate) fn handle_sidebar_filter_menu_key(&mut self, key: KeyEvent) -> bool {
        if !self.sidebar_filter_menu_open {
            return false;
        }
        let count = crate::ui::sidebar_filter_options(self).len();
        match key.code {
            KeyCode::Esc => self.sidebar_filter_menu_open = false,
            KeyCode::Up | KeyCode::Char('k') => {
                self.sidebar_filter_menu_selected =
                    self.sidebar_filter_menu_selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.sidebar_filter_menu_selected = self
                    .sidebar_filter_menu_selected
                    .saturating_add(1)
                    .min(count.saturating_sub(1));
            }
            KeyCode::Enter => self.select_sidebar_filter_option(self.sidebar_filter_menu_selected),
            _ => {}
        }
        true
    }

    /// `Enter` opens a selected provider object without creating a pane; `n`
    /// keeps the direct thread-start shortcut. The selection only exists while
    /// one row is selected, so this never swallows input meant for a pane.
    pub(crate) fn handle_sidebar_work_group_key(
        &mut self,
        key: KeyEvent,
    ) -> SidebarWorkGroupKeyAction {
        let Some(selected) = self.sidebar_selected_work_group.clone() else {
            return SidebarWorkGroupKeyAction::Ignored;
        };
        // Aloops rows that are not findings have their own verbs (MAT-159):
        // loop headers open the run-history table (AC8), run lines and the
        // clean-run fold toggle, and a clean run opens its recorded log (AC7).
        // None of them dispatch, so `n` is ignored on purpose.
        use crate::ui::sidebar::aloops as aloop_rows;
        if let Some(name) = selected.strip_prefix(aloop_rows::ALOOP_LOOP_KEY_PREFIX) {
            if key.code == KeyCode::Enter && key.modifiers.is_empty() {
                self.sidebar_selected_work_group = None;
                self.open_aloop_loop_history(name);
                return SidebarWorkGroupKeyAction::Consumed;
            }
            if key.code == KeyCode::Char('n') && key.modifiers.is_empty() {
                self.sidebar_selected_work_group = None;
                return SidebarWorkGroupKeyAction::Consumed;
            }
        } else if selected.starts_with(aloop_rows::ALOOP_RUN_KEY_PREFIX)
            || selected.starts_with(aloop_rows::ALOOP_CLEAN_KEY_PREFIX)
        {
            if key.code == KeyCode::Enter && key.modifiers.is_empty() {
                self.toggle_sidebar_group(&selected);
                return SidebarWorkGroupKeyAction::Consumed;
            }
            if key.code == KeyCode::Char('n') && key.modifiers.is_empty() {
                self.sidebar_selected_work_group = None;
                return SidebarWorkGroupKeyAction::Consumed;
            }
        } else if let Some(rest) = selected.strip_prefix(aloop_rows::ALOOP_CLEAN_RUN_KEY_PREFIX) {
            if key.code == KeyCode::Enter && key.modifiers.is_empty() {
                self.sidebar_selected_work_group = None;
                if let Some((loop_name, at)) = rest.split_once(':') {
                    self.open_aloop_run_log(loop_name, at);
                }
                return SidebarWorkGroupKeyAction::Consumed;
            }
            if key.code == KeyCode::Char('n') && key.modifiers.is_empty() {
                self.sidebar_selected_work_group = None;
                return SidebarWorkGroupKeyAction::Consumed;
            }
        }
        match key.code {
            KeyCode::Enter if key.modifiers.is_empty() => {
                self.sidebar_selected_work_group = None;
                if selected == crate::ui::sidebar_show_more_key(self.sidebar_group_mode) {
                    self.sidebar_unassigned_expanded_views
                        .insert(self.sidebar_group_mode);
                    self.workspace_scroll = crate::ui::normalized_workspace_scroll(
                        self,
                        self.view.sidebar_rect,
                        self.workspace_scroll,
                    );
                } else if !self.open_sidebar_unassigned_object(&selected) {
                    match self.sidebar_unassigned_dispatch_plan(&selected) {
                        Ok(plan) => return SidebarWorkGroupKeyAction::Dispatch(Box::new(plan)),
                        Err(error) => self.config_diagnostic = Some(error),
                    }
                }
                SidebarWorkGroupKeyAction::Consumed
            }
            KeyCode::Char('n') if key.modifiers.is_empty() => {
                self.sidebar_selected_work_group = None;
                match self.sidebar_unassigned_dispatch_plan(&selected) {
                    Ok(plan) => SidebarWorkGroupKeyAction::Dispatch(Box::new(plan)),
                    Err(error) => {
                        self.config_diagnostic = Some(error);
                        SidebarWorkGroupKeyAction::Consumed
                    }
                }
            }
            KeyCode::Esc => {
                self.sidebar_selected_work_group = None;
                SidebarWorkGroupKeyAction::Consumed
            }
            _ => SidebarWorkGroupKeyAction::Ignored,
        }
    }

    pub(crate) fn handle_sidebar_group_menu_key(&mut self, key: KeyEvent) -> bool {
        if !self.sidebar_group_menu_open {
            return false;
        }
        match key.code {
            KeyCode::Esc => self.sidebar_group_menu_open = false,
            KeyCode::Up | KeyCode::Char('k') => {
                self.sidebar_group_menu_selected =
                    self.sidebar_group_menu_selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.sidebar_group_menu_selected =
                    self.sidebar_group_menu_selected.saturating_add(1).min(
                        crate::app::state::SidebarGroupMode::VIEWS
                            .len()
                            .saturating_sub(1),
                    );
            }
            KeyCode::Enter => {
                if let Some(mode) = crate::app::state::SidebarGroupMode::VIEWS
                    .get(self.sidebar_group_menu_selected)
                    .copied()
                {
                    self.set_sidebar_group_mode(mode);
                }
            }
            _ => {}
        }
        true
    }

    pub(super) fn workspace_list_rect(&self) -> Rect {
        let sidebar = self.view.sidebar_rect;
        if self.sidebar_collapsed || sidebar.width <= 1 || sidebar.height == 0 {
            return Rect::default();
        }
        crate::ui::workspace_list_rect_for_app(self, sidebar)
    }

    pub(super) fn workspace_list_scrollbar_target_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<ScrollbarClickTarget> {
        let area = self.workspace_list_rect();
        let metrics = crate::ui::workspace_list_scroll_metrics(self, area);
        let track = crate::ui::workspace_list_scrollbar_rect(self, area)?;
        if col < track.x
            || col >= track.x + track.width
            || row < track.y
            || row >= track.y + track.height
        {
            return None;
        }
        if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(metrics, track, row) {
            Some(ScrollbarClickTarget::Thumb { grab_row_offset })
        } else {
            Some(ScrollbarClickTarget::Track {
                offset_from_bottom: crate::ui::scrollbar_offset_from_row(metrics, track, row),
            })
        }
    }

    pub(super) fn workspace_list_offset_for_drag_row(
        &self,
        row: u16,
        grab_row_offset: u16,
    ) -> Option<usize> {
        let area = self.workspace_list_rect();
        let metrics = crate::ui::workspace_list_scroll_metrics(self, area);
        let track = crate::ui::workspace_list_scrollbar_rect(self, area)?;
        Some(crate::ui::scrollbar_offset_from_drag_row(
            metrics,
            track,
            row,
            grab_row_offset,
        ))
    }

    pub(super) fn set_workspace_list_offset_from_bottom(&mut self, offset_from_bottom: usize) {
        let area = self.workspace_list_rect();
        let metrics = crate::ui::workspace_list_scroll_metrics(self, area);
        self.workspace_scroll = metrics
            .max_offset_from_bottom
            .saturating_sub(offset_from_bottom);
        self.workspace_scroll = crate::ui::normalized_workspace_scroll(
            self,
            self.view.sidebar_rect,
            self.workspace_scroll,
        );
    }

    pub(super) fn scroll_workspace_list(&mut self, delta: i16) {
        if delta.is_negative() {
            self.workspace_scroll = self
                .workspace_scroll
                .saturating_sub(delta.unsigned_abs() as usize);
            self.workspace_scroll = crate::ui::normalized_workspace_scroll(
                self,
                self.view.sidebar_rect,
                self.workspace_scroll,
            );
            return;
        }

        let area = self.workspace_list_rect();
        let metrics = crate::ui::workspace_list_scroll_metrics(self, area);
        self.workspace_scroll = self
            .workspace_scroll
            .saturating_add(delta as usize)
            .min(metrics.max_offset_from_bottom);
        self.workspace_scroll = crate::ui::normalized_workspace_scroll(
            self,
            self.view.sidebar_rect,
            self.workspace_scroll,
        );
    }

    pub(super) fn scroll_collapsed_sidebar(&mut self, delta: i16) -> bool {
        if !self.sidebar_collapsed {
            return false;
        }
        let (ws_area, _, _) = crate::ui::collapsed_sidebar_sections(self.view.sidebar_rect);
        if ws_area == Rect::default() {
            return false;
        }
        let max_scroll = crate::ui::sidebar_rows(self)
            .len()
            .saturating_sub(ws_area.height as usize);
        if max_scroll == 0 {
            return false;
        }
        let next = if delta.is_negative() {
            self.workspace_scroll
                .saturating_sub(delta.unsigned_abs() as usize)
        } else {
            self.workspace_scroll.saturating_add(delta as usize)
        };
        self.workspace_scroll = next.min(max_scroll);
        true
    }

    pub(crate) fn sidebar_footer_rect(&self) -> Rect {
        let ws_area = self.workspace_list_rect();
        if ws_area == Rect::default() {
            return Rect::default();
        }
        let y = ws_area.y + ws_area.height;
        Rect::new(ws_area.x, y, ws_area.width, 0)
    }

    pub(crate) fn global_launcher_rect(&self) -> Rect {
        if self.view.layout == ViewLayout::Mobile {
            return self.view.mobile_menu_hit_area;
        }

        crate::ui::sidebar_header_overflow_rect(self.view.sidebar_rect)
    }

    pub(crate) fn global_menu_labels(&self) -> Vec<&'static str> {
        let mut labels = vec!["settings", "keybinds", "reload config"];
        if self.update_available.is_some() {
            labels.push("update ready");
        } else if self.latest_release_notes_available {
            labels.push("what's new");
        }
        labels.push("detach");
        labels
    }

    pub(crate) fn global_menu_rect(&self) -> Rect {
        let screen = self.screen_rect();
        let launcher = self.global_launcher_rect();
        let labels = self.global_menu_labels();
        let content_width = labels
            .iter()
            .map(|label| {
                let badge_width = if self.global_menu_item_has_badge(label) {
                    2
                } else {
                    0
                };
                label.chars().count() as u16 + badge_width
            })
            .max()
            .unwrap_or(8)
            .saturating_add(2);
        let menu_w = content_width.saturating_add(2).min(screen.width.max(1));
        let menu_h = (labels.len() as u16 + 2).min(screen.height.max(1));
        let max_x = screen.x + screen.width.saturating_sub(menu_w);
        let desired_x = launcher.x + launcher.width.saturating_sub(menu_w);
        let x = desired_x.min(max_x);
        let y = launcher
            .y
            .saturating_add(1)
            .min(screen.y + screen.height.saturating_sub(menu_h));
        Rect::new(x, y, menu_w, menu_h)
    }

    pub(super) fn on_sidebar_divider(&self, col: u16, row: u16) -> bool {
        if self.sidebar_collapsed {
            return false;
        }
        let sidebar = self.view.sidebar_rect;
        let toggle = crate::ui::expanded_sidebar_toggle_rect(sidebar);
        let on_toggle = toggle.width > 0
            && col >= toggle.x
            && col < toggle.x + toggle.width
            && row >= toggle.y
            && row < toggle.y + toggle.height;
        crate::ui::sidebar_separator_col(sidebar).is_some_and(|separator_col| col == separator_col)
            && !on_toggle
            && row >= sidebar.y
            && row < sidebar.y + sidebar.height
    }

    pub(super) fn on_sidebar_toggle(&self, col: u16, row: u16) -> bool {
        let rect = if self.sidebar_collapsed {
            crate::ui::collapsed_sidebar_toggle_rect(self.view.sidebar_rect)
        } else {
            crate::ui::expanded_sidebar_toggle_rect(self.view.sidebar_rect)
        };
        rect.width > 0
            && col >= rect.x
            && col < rect.x + rect.width
            && row >= rect.y
            && row < rect.y + rect.height
    }

    pub(super) fn set_manual_sidebar_width(&mut self, divider_col: u16) {
        let sidebar = self.view.sidebar_rect;
        let width = divider_col.saturating_sub(sidebar.x).saturating_add(1);
        self.sidebar_width = width.clamp(self.sidebar_min_width, self.sidebar_max_width);
        self.sidebar_width_source = crate::app::state::SidebarWidthSource::Manual;
        self.mark_session_dirty();
    }

    pub(super) fn workspace_at_row(&self, row: u16) -> Option<usize> {
        let footer = self.sidebar_footer_rect();
        if footer == Rect::default() {
            return None;
        }

        let (cards, _) = crate::ui::compute_sidebar_row_areas(self, self.view.sidebar_rect);

        cards.iter().find_map(|card| {
            (row >= card.rect.y && row < card.rect.y + card.rect.height).then_some(card.ws_idx)
        })
    }

    pub(super) fn sidebar_section_header_at(&self, row: u16) -> Option<&'static str> {
        crate::ui::compute_sidebar_section_header_areas(self, self.view.sidebar_rect)
            .into_iter()
            .find(|header| row >= header.rect.y && row < header.rect.y + header.rect.height)
            .map(|header| header.title)
    }

    /// Folding a group is view state, not session state, so it needs no API
    /// round trip -- but it does change the row count, so the sidebar's own
    /// scroll clamp has to run afterwards.
    pub(crate) fn toggle_sidebar_group(&mut self, title: &str) {
        let key = format!("{}:{title}", self.sidebar_group_mode.collapse_namespace());
        if title.starts_with(crate::ui::sidebar::aloops::ALOOP_CLEAN_KEY_PREFIX) {
            if !self
                .sidebar_presentation
                .expanded_remote_host_groups
                .remove(&key)
            {
                self.sidebar_presentation
                    .expanded_remote_host_groups
                    .insert(key);
            }
        } else {
            let collapsed = !self.collapsed_sidebar_groups.contains(&key);
            self.set_sidebar_group_collapsed(title, collapsed);
        }
        self.workspace_scroll = crate::ui::normalized_workspace_scroll(
            self,
            self.view.sidebar_rect,
            self.workspace_scroll,
        );
    }

    pub(crate) fn set_sidebar_group_collapsed(&mut self, title: &str, collapsed: bool) {
        let key = format!("{}:{title}", self.sidebar_group_mode.collapse_namespace());
        if collapsed {
            self.collapsed_sidebar_groups.insert(key.clone());
        } else {
            self.collapsed_sidebar_groups.remove(&key);
        }
        self.sidebar_group_collapsed_persistence_request = Some((key, collapsed));
    }

    /// Select a fleet agent, then keep its row in view.
    pub(crate) fn select_remote_agent_row(&mut self, agent_ref: crate::api::schema::AgentRef) {
        self.sidebar_selected_remote_agent = Some(agent_ref.clone());
        self.mark_sidebar_projection_changed();
        if let Some(target_row) = crate::ui::sidebar_rows(self).iter().position(|row| {
            matches!(
                row,
                crate::ui::SidebarRow::RemoteAgent { entry, .. }
                    if entry.agent_ref == agent_ref
            )
        }) {
            self.workspace_scroll = crate::ui::sidebar_row_scroll_for_target(
                self,
                self.view.sidebar_rect,
                self.workspace_scroll,
                target_row,
            );
        }
    }

    pub(super) fn collapsed_workspace_at_row(&self, row: u16) -> Option<usize> {
        if !self.sidebar_collapsed {
            return None;
        }

        let (ws_area, _, _) = crate::ui::collapsed_sidebar_sections(self.view.sidebar_rect);
        if ws_area == Rect::default() || row < ws_area.y || row >= ws_area.y + ws_area.height {
            return None;
        }

        let idx =
            (row - ws_area.y) as usize + crate::ui::collapsed_sidebar_row_scroll(self, ws_area);
        crate::ui::sidebar_rows(self)
            .get(idx)
            .and_then(|entry| match entry {
                crate::ui::SidebarRow::Workspace { ws_idx, .. } => Some(*ws_idx),
                crate::ui::SidebarRow::Agent { .. }
                | crate::ui::SidebarRow::RemoteAgent { .. }
                | crate::ui::SidebarRow::Tab { .. }
                | crate::ui::SidebarRow::SectionHeader { .. }
                | crate::ui::SidebarRow::NestedHeader { .. }
                | crate::ui::SidebarRow::SymphonyJob { .. }
                | crate::ui::SidebarRow::SymphonyEmpty
                | crate::ui::SidebarRow::Divider
                | crate::ui::SidebarRow::AloopLoop { .. }
                | crate::ui::SidebarRow::AloopRunLine { .. }
                | crate::ui::SidebarRow::AloopFinding { .. }
                | crate::ui::SidebarRow::AloopCleanRuns { .. }
                | crate::ui::SidebarRow::AloopCleanRun { .. }
                | crate::ui::SidebarRow::AloopUnreachable { .. }
                | crate::ui::SidebarRow::AloopEmpty
                | crate::ui::SidebarRow::AgentRun { .. } => None,
            })
    }

    pub(super) fn collapsed_agent_detail_target_at(
        &self,
        row: u16,
    ) -> Option<(usize, crate::layout::PaneId)> {
        if !self.sidebar_collapsed {
            return None;
        }

        let (content, _, _) = crate::ui::collapsed_sidebar_sections(self.view.sidebar_rect);
        if content == Rect::default() || row < content.y || row >= content.y + content.height {
            return None;
        }
        let row_idx =
            (row - content.y) as usize + crate::ui::collapsed_sidebar_row_scroll(self, content);
        crate::ui::sidebar_rows(self)
            .get(row_idx)
            .and_then(|entry| match entry {
                crate::ui::SidebarRow::Agent { entry, .. } => entry
                    .local_target()
                    .map(|target| (target.ws_idx, target.pane_id)),
                crate::ui::SidebarRow::Workspace { .. }
                | crate::ui::SidebarRow::RemoteAgent { .. }
                | crate::ui::SidebarRow::SectionHeader { .. }
                | crate::ui::SidebarRow::NestedHeader { .. }
                | crate::ui::SidebarRow::SymphonyJob { .. }
                | crate::ui::SidebarRow::SymphonyEmpty
                | crate::ui::SidebarRow::Divider
                | crate::ui::SidebarRow::AloopLoop { .. }
                | crate::ui::SidebarRow::AloopRunLine { .. }
                | crate::ui::SidebarRow::AloopFinding { .. }
                | crate::ui::SidebarRow::AloopCleanRuns { .. }
                | crate::ui::SidebarRow::AloopCleanRun { .. }
                | crate::ui::SidebarRow::AloopUnreachable { .. }
                | crate::ui::SidebarRow::AloopEmpty
                | crate::ui::SidebarRow::AgentRun { .. } => None,
                crate::ui::SidebarRow::Tab { entry, .. } => entry
                    .local_target()
                    .map(|target| (target.ws_idx, target.pane_id)),
            })
    }

    pub(super) fn collapsed_remote_agent_target_at(
        &self,
        row: u16,
    ) -> Option<crate::api::schema::AgentRef> {
        if !self.sidebar_collapsed {
            return None;
        }
        let (content, _, _) = crate::ui::collapsed_sidebar_sections(self.view.sidebar_rect);
        if content == Rect::default() || row < content.y || row >= content.bottom() {
            return None;
        }
        let row_idx =
            usize::from(row - content.y) + crate::ui::collapsed_sidebar_row_scroll(self, content);
        crate::ui::sidebar_rows(self)
            .get(row_idx)
            .and_then(|entry| match entry {
                crate::ui::SidebarRow::RemoteAgent { entry, .. } => Some(entry.agent_ref.clone()),
                _ => None,
            })
    }

    pub(super) fn workspace_drop_target_at_row(
        &self,
        row: u16,
    ) -> Option<crate::app::state::WorkspaceDropTarget> {
        let area = self.workspace_list_rect();
        let footer = self.sidebar_footer_rect();
        if area == Rect::default() || row < area.y || row >= footer.y {
            return None;
        }

        let (cards, _) = crate::ui::compute_sidebar_row_areas(self, self.view.sidebar_rect);
        let slots = crate::ui::workspace_drop_slots(self, &cards, area);
        if slots.last().is_some_and(|(_, slot_row)| row <= *slot_row) {
            slots
                .into_iter()
                .enumerate()
                .min_by_key(|(slot_idx, (_, slot_row))| (row.abs_diff(*slot_row), *slot_idx))
                .map(|(_, (target, _))| target)
        } else {
            None
        }
    }

    pub(super) fn workspace_move_block_params(
        &self,
        source_ws_idx: usize,
        drop_target: crate::app::state::WorkspaceDropTarget,
    ) -> Option<crate::api::schema::WorkspaceMoveBlockParams> {
        let source = self.workspaces.get(source_ws_idx)?;
        if source
            .worktree_space()
            .is_some_and(|space| space.is_linked_worktree)
        {
            return None;
        }

        let roots = crate::ui::workspace_list_entries_expanded(self)
            .into_iter()
            .filter_map(|entry| match entry {
                crate::ui::WorkspaceListEntry::Workspace {
                    ws_idx,
                    indented: false,
                } => Some(ws_idx),
                crate::ui::WorkspaceListEntry::Workspace { .. } => None,
                crate::ui::WorkspaceListEntry::NestedHeader { .. } => None,
            })
            .collect::<Vec<_>>();
        let source_pos = roots.iter().position(|ws_idx| *ws_idx == source_ws_idx)?;
        let remaining_roots = roots
            .iter()
            .copied()
            .filter(|ws_idx| *ws_idx != source_ws_idx)
            .collect::<Vec<_>>();
        let insert_pos = match drop_target {
            crate::app::state::WorkspaceDropTarget::Before(target_ws_idx) => remaining_roots
                .iter()
                .position(|ws_idx| *ws_idx == target_ws_idx)?,
            crate::app::state::WorkspaceDropTarget::End => remaining_roots.len(),
        };
        if insert_pos == source_pos {
            return None;
        }

        let workspace_ids = match source.worktree_space() {
            Some(source_space) => {
                let mut ids = vec![source.id.clone()];
                ids.extend(
                    self.workspaces
                        .iter()
                        .filter(|workspace| workspace.id != source.id)
                        .filter(|workspace| {
                            workspace
                                .worktree_space()
                                .is_some_and(|space| space.key == source_space.key)
                        })
                        .map(|workspace| workspace.id.clone()),
                );
                ids
            }
            None => vec![source.id.clone()],
        };
        let before_workspace_id = match drop_target {
            crate::app::state::WorkspaceDropTarget::Before(target_ws_idx) => {
                let target = self.workspaces.get(target_ws_idx)?;
                let anchor = match crate::ui::workspace_parent_group_state(self, target_ws_idx)
                    .and_then(|_| target.worktree_space())
                {
                    Some(target_space) => self
                        .workspaces
                        .iter()
                        .find(|workspace| {
                            workspace
                                .worktree_space()
                                .is_some_and(|space| space.key == target_space.key)
                        })
                        .unwrap_or(target),
                    None => target,
                };
                Some(anchor.id.clone())
            }
            crate::app::state::WorkspaceDropTarget::End => None,
        };

        Some(crate::api::schema::WorkspaceMoveBlockParams {
            workspace_ids,
            before_workspace_id,
        })
    }

    pub(super) fn agent_detail_target_at(
        &self,
        row: u16,
    ) -> Option<(usize, usize, crate::layout::PaneId)> {
        if self.sidebar_collapsed {
            return None;
        }

        let (_, cards) = crate::ui::compute_sidebar_row_areas(self, self.view.sidebar_rect);
        cards.iter().find_map(|card| {
            (row >= card.rect.y && row < card.rect.y + card.rect.height).then_some((
                card.ws_idx,
                card.tab_idx,
                card.pane_id,
            ))
        })
    }

    pub(super) fn tab_target_at(&self, row: u16) -> Option<(usize, usize)> {
        if self.sidebar_collapsed {
            return None;
        }
        crate::ui::compute_tab_card_areas(self, self.view.sidebar_rect)
            .into_iter()
            .find(|card| row >= card.rect.y && row < card.rect.y + card.rect.height)
            .map(|card| (card.ws_idx, card.tab_idx))
    }
}

impl super::super::App {
    pub(crate) fn open_sidebar_snooze_menu(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        column: u16,
        row: u16,
    ) {
        if !self.state.pane_is_snoozed(ws_idx, pane_id)
            && !self.state.pane_can_snooze(ws_idx, pane_id)
        {
            return;
        }
        self.state.sidebar_snooze = Some(crate::app::state::SidebarSnoozeUiState {
            target: crate::app::state::PaneFocusTarget {
                workspace_id: self.state.workspaces[ws_idx].id.clone(),
                pane_id,
            },
            anchor: (column, row),
            selected: crate::app::state::sidebar_snooze_menu_items(
                self.state.pane_is_snoozed(ws_idx, pane_id),
            )[0]
            .1,
            time_draft: None,
            error: None,
        });
    }

    pub(crate) fn open_snooze_time_input(&mut self, ws_idx: usize, pane_id: crate::layout::PaneId) {
        if !self.state.pane_is_snoozed(ws_idx, pane_id)
            && !self.state.pane_can_snooze(ws_idx, pane_id)
        {
            return;
        }
        let Some(workspace) = self.state.workspaces.get(ws_idx) else {
            return;
        };
        let Some(pane) = workspace.pane_state(pane_id) else {
            return;
        };
        let prefill = pane
            .snoozed_until()
            .and_then(crate::platform::local_datetime_at)
            .map(|deadline| format!("{:02}:{:02}", deadline.hour(), deadline.minute()))
            .unwrap_or_default();
        let anchor = self
            .state
            .sidebar_snooze
            .as_ref()
            .filter(|snooze| {
                snooze.target.workspace_id == workspace.id && snooze.target.pane_id == pane_id
            })
            .map_or(
                (
                    self.state.view.sidebar_rect.x,
                    self.state.view.sidebar_rect.y,
                ),
                |snooze| snooze.anchor,
            );
        self.state.sidebar_snooze = Some(crate::app::state::SidebarSnoozeUiState {
            target: crate::app::state::PaneFocusTarget {
                workspace_id: workspace.id.clone(),
                pane_id,
            },
            anchor,
            selected: crate::app::state::SidebarSnoozeMenuAction::SetTime,
            time_draft: Some(prefill),
            error: None,
        });
    }

    pub(crate) fn apply_sidebar_snooze_menu_action(
        &mut self,
        selected: crate::app::state::SidebarSnoozeMenuAction,
    ) {
        let Some(snooze) = self.state.sidebar_snooze.as_ref() else {
            return;
        };
        if snooze.time_draft.is_some() {
            return;
        }
        let target = snooze.target.clone();
        let Some(ws_idx) = self
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == target.workspace_id)
        else {
            return;
        };
        let snoozed = self.state.pane_is_snoozed(ws_idx, target.pane_id);
        let actions = crate::app::state::sidebar_snooze_menu_items(snoozed);
        let Some((_, action)) = actions.iter().find(|(_, action)| *action == selected) else {
            self.state.sidebar_snooze = None;
            return;
        };
        let Some(public_pane_id) = self.public_pane_id(ws_idx, target.pane_id) else {
            return;
        };
        match action {
            crate::app::state::SidebarSnoozeMenuAction::Preset(preset) => {
                let tomorrow_morning = matches!(
                    preset,
                    crate::app::state::SidebarSnoozePreset::TomorrowMorning
                )
                .then(crate::platform::tomorrow_morning_unix)
                .flatten();
                let Some(params) = sidebar_snooze_params(public_pane_id, *preset, tomorrow_morning)
                else {
                    return;
                };
                self.state.sidebar_snooze = None;
                self.runtime_pane_snooze("tui.sidebar.snooze", params);
            }
            crate::app::state::SidebarSnoozeMenuAction::SetTime => {
                self.open_snooze_time_input(ws_idx, target.pane_id);
            }
            crate::app::state::SidebarSnoozeMenuAction::Unsnooze => {
                self.state.sidebar_snooze = None;
                self.runtime_pane_unsnooze("tui.sidebar.unsnooze", public_pane_id);
            }
        }
    }

    pub(crate) fn handle_sidebar_snooze_menu_key(&mut self, key: KeyEvent) -> bool {
        let Some(snooze) = self.state.sidebar_snooze.as_ref() else {
            return false;
        };
        if snooze.time_draft.is_some() {
            return false;
        }
        let Some(ws_idx) = self
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == snooze.target.workspace_id)
        else {
            self.state.sidebar_snooze = None;
            return true;
        };
        let item_count = crate::app::state::sidebar_snooze_menu_items(
            self.state.pane_is_snoozed(ws_idx, snooze.target.pane_id),
        );
        match key.code {
            KeyCode::Esc => self.state.sidebar_snooze = None,
            KeyCode::Up | KeyCode::Char('k') => {
                let selected = item_count
                    .iter()
                    .position(|(_, action)| *action == snooze.selected)
                    .unwrap_or(0)
                    .saturating_sub(1);
                if let (Some(snooze), Some((_, action))) =
                    (self.state.sidebar_snooze.as_mut(), item_count.get(selected))
                {
                    snooze.selected = *action;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let selected = item_count
                    .iter()
                    .position(|(_, action)| *action == snooze.selected)
                    .unwrap_or(0)
                    .saturating_add(1)
                    .min(item_count.len().saturating_sub(1));
                if let (Some(snooze), Some((_, action))) =
                    (self.state.sidebar_snooze.as_mut(), item_count.get(selected))
                {
                    snooze.selected = *action;
                }
            }
            KeyCode::Enter => {
                let action = self
                    .state
                    .sidebar_snooze
                    .as_ref()
                    .map(|snooze| snooze.selected);
                if let Some(action) = action {
                    self.apply_sidebar_snooze_menu_action(action);
                }
            }
            _ => {}
        }
        true
    }

    pub(crate) fn handle_sidebar_session_action_key(&mut self, key: KeyEvent) -> bool {
        if !self.state.sidebar_focused
            || !matches!(
                self.state.effective_interaction_mode(),
                crate::app::Mode::Terminal | crate::app::Mode::Navigate
            )
            || !key.modifiers.is_empty()
            || self.state.sidebar_settled_menu_target.is_some()
            || self.state.sidebar_selected_settled.is_some()
            || self.state.sidebar_selected_remote_agent.is_some()
            || self.state.sidebar_object_menu.is_some()
            || self.state.sidebar_sort_menu.is_some()
            || self.state.sidebar_group_menu_open
            || self.state.sidebar_filter_menu_open
            || self.state.sidebar_subgroup_picker.is_some()
            || self.state.sidebar_new_menu.is_some()
            || self.state.sidebar_new_thread.is_some()
            || self.state.sidebar_project_menu.is_some()
            || self.state.sidebar_search_active
            || self.state.sidebar_selected_work_group.is_some()
        {
            return false;
        }
        let Some(ws_idx) = self.state.active else {
            return false;
        };
        let Some(pane_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(crate::workspace::Workspace::focused_pane_id)
        else {
            return false;
        };
        match key.code {
            KeyCode::Char('z') => {
                let anchor =
                    crate::ui::compute_tab_card_areas(&self.state, self.state.view.sidebar_rect)
                        .into_iter()
                        .find(|card| card.ws_idx == ws_idx && card.pane_id == pane_id)
                        .map(|card| (card.rect.right().saturating_sub(4), card.rect.y))
                        .unwrap_or((
                            self.state.view.sidebar_rect.x,
                            self.state.view.sidebar_rect.y,
                        ));
                self.open_sidebar_snooze_menu(ws_idx, pane_id, anchor.0, anchor.1);
                self.state.sidebar_snooze.is_some()
            }
            KeyCode::Char('s') => {
                self.settle_sidebar_pane(ws_idx, pane_id);
                true
            }
            _ => false,
        }
    }

    pub(crate) fn handle_sidebar_object_menu_key(&mut self, key: KeyEvent) -> bool {
        if self.state.sidebar_object_menu.is_none() {
            if key.code != KeyCode::Char('m') || !key.modifiers.is_empty() {
                return false;
            }
            let Some(target) = self.state.sidebar_selected_work_group.clone() else {
                return false;
            };
            self.state.open_sidebar_object_menu(target);
            if crate::ui::sidebar_object_menu_items(&self.state).is_empty() {
                self.state.sidebar_object_menu = None;
                return false;
            }
            return true;
        }

        let page = self
            .state
            .sidebar_object_menu
            .as_ref()
            .map(|menu| menu.page)
            .unwrap_or_default();
        if page == crate::app::state::SidebarObjectMenuPage::Confirmation {
            match key.code {
                KeyCode::Char('y' | 'Y') if key.modifiers.is_empty() => {
                    self.handle_pending_dock_write_key(key);
                    self.state.sidebar_object_menu = None;
                }
                KeyCode::Esc | KeyCode::Char('n' | 'N') if key.modifiers.is_empty() => {
                    self.handle_pending_dock_write_key(key);
                    self.state.sidebar_object_menu = None;
                }
                _ => {}
            }
            return true;
        }

        let count = crate::ui::sidebar_object_menu_items(&self.state).len();
        match key.code {
            KeyCode::Esc if key.modifiers.is_empty() => self.state.sidebar_object_menu = None,
            KeyCode::Up | KeyCode::Char('k') if key.modifiers.is_empty() => {
                if let Some(menu) = self.state.sidebar_object_menu.as_mut() {
                    menu.selected = menu.selected.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') if key.modifiers.is_empty() => {
                if let Some(menu) = self.state.sidebar_object_menu.as_mut() {
                    menu.selected = menu.selected.saturating_add(1).min(count.saturating_sub(1));
                }
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                let index = self
                    .state
                    .sidebar_object_menu
                    .as_ref()
                    .map_or(0, |menu| menu.selected);
                self.apply_sidebar_object_menu_action(index);
            }
            _ => {}
        }
        true
    }

    pub(crate) fn apply_sidebar_object_menu_action(&mut self, index: usize) {
        let Some(item) = crate::ui::sidebar_object_menu_items(&self.state)
            .get(index)
            .copied()
        else {
            return;
        };
        if let crate::ui::SidebarObjectMenuItem::Ticket(action) = item {
            let enabled = crate::ui::sidebar_ticket_action_entries(&self.state)
                .into_iter()
                .find(|entry| entry.action == action)
                .is_some_and(|entry| entry.enabled());
            if enabled {
                self.apply_sidebar_ticket_action(action);
            }
            return;
        }
        match item {
            crate::ui::SidebarObjectMenuItem::PullRequest(action) => {
                let action_enabled = crate::ui::sidebar_pull_request_actions(&self.state)
                    .get(index)
                    .is_some_and(|action| action.enabled());
                if !action_enabled {
                    return;
                }
                let Some(key) = crate::ui::sidebar_pull_request_key(&self.state) else {
                    self.state.config_diagnostic =
                        Some("pull request is no longer available".to_string());
                    self.state.sidebar_object_menu = None;
                    return;
                };
                self.state.sidebar_object_menu = None;
                self.activate_pr_action(key, action);
            }
            crate::ui::SidebarObjectMenuItem::StartThread => {
                let target = self
                    .state
                    .sidebar_object_menu
                    .as_ref()
                    .map(|menu| menu.target.clone());
                self.state.sidebar_object_menu = None;
                let Some(target) = target else { return };
                match self.state.sidebar_unassigned_dispatch_plan(&target) {
                    Ok(plan) => self.dispatch_sidebar_work_group_plan(plan),
                    Err(error) => self.state.config_diagnostic = Some(error),
                }
            }
            crate::ui::SidebarObjectMenuItem::CopyMissiveUrl => {
                let url = crate::ui::sidebar_missive_copy_url(&self.state);
                self.state.sidebar_object_menu = None;
                if let Some(url) = url {
                    self.state.request_clipboard_write = Some(url.into_bytes());
                } else {
                    self.state.config_diagnostic =
                        Some("conversation is no longer available".to_string());
                }
            }
            crate::ui::SidebarObjectMenuItem::Ticket(_) => {}
        }
    }

    fn sidebar_ticket_parts(
        &self,
    ) -> Option<(
        crate::app::state::WorkItemKey,
        crate::work_index::WorkTicket,
        String,
    )> {
        let identifier = crate::ui::sidebar_ticket_target(&self.state)?;
        let source = self
            .state
            .work_index_snapshot
            .as_ref()?
            .items
            .iter()
            .find(|item| {
                item.ticket_details
                    .iter()
                    .any(|ticket| ticket.identifier.eq_ignore_ascii_case(&identifier))
            })?;
        let ticket = source
            .ticket_details
            .iter()
            .find(|ticket| ticket.identifier.eq_ignore_ascii_case(&identifier))?
            .clone();
        Some((
            crate::app::state::WorkItemKey {
                repo: String::new(),
                pr_number: None,
                pr_url: None,
                ticket_id: Some(identifier),
            },
            ticket,
            source.repo.clone(),
        ))
    }

    fn stage_sidebar_ticket_write(&mut self, write: crate::work_index::WorkItemWrite) {
        self.state.dock_pending_write = Some(write);
        self.state.dock_write_notice = None;
        if let Some(menu) = self.state.sidebar_object_menu.as_mut() {
            menu.page = crate::app::state::SidebarObjectMenuPage::Confirmation;
            menu.selected = 0;
        }
    }

    fn apply_sidebar_ticket_action(&mut self, action: crate::ui::ticket_actions::TicketAction) {
        use crate::ui::ticket_actions::TicketAction;
        let Some((key, ticket, repo)) = self.sidebar_ticket_parts() else {
            self.state.config_diagnostic = Some("ticket is no longer available".into());
            self.state.sidebar_object_menu = None;
            return;
        };
        let context = crate::ui::ticket_actions::TicketActionContext::from_ticket(
            &ticket,
            self.state.work_item_detail_cache.get(&key),
            self.state.work_index_session.linear.viewer.as_deref(),
            self.state.work_index_session.linear_viewer_identity(),
            crate::ui::dock::pr::focused_pr_key(&self.state).is_some(),
        );
        match action {
            TicketAction::TransitionMenu | TicketAction::PriorityMenu => {
                if let Some(menu) = self.state.sidebar_object_menu.as_mut() {
                    menu.page = if action == TicketAction::TransitionMenu {
                        crate::app::state::SidebarObjectMenuPage::TicketTransitions
                    } else {
                        crate::app::state::SidebarObjectMenuPage::TicketPriorities
                    };
                    menu.selected = 0;
                }
            }
            TicketAction::Refresh => {
                self.next_work_index_refresh = std::time::Instant::now();
                self.state.sidebar_object_menu = None;
            }
            TicketAction::AskQuestion => {
                self.state.sidebar_object_menu = None;
                self.open_ticket_home(
                    ticket,
                    repo,
                    format!("About {} ({}): ", context.identifier, context.title),
                    crate::app::state::PrCheckoutChoice::CurrentCheckout,
                );
            }
            TicketAction::Explain => {
                self.state.sidebar_object_menu = None;
                self.open_ticket_home(
                    ticket,
                    repo,
                    format!(
                        "Summarise {} ({}). Include the ticket, linked pull requests, and the acceptance criteria block.",
                        context.identifier, context.title
                    ),
                    crate::app::state::PrCheckoutChoice::CurrentCheckout,
                );
            }
            TicketAction::WorkInThread => {
                let prompt = Self::ticket_work_prompt(&ticket);
                self.state.sidebar_object_menu = None;
                self.open_ticket_home(
                    ticket,
                    repo,
                    prompt,
                    crate::app::state::PrCheckoutChoice::CurrentCheckout,
                );
            }
            TicketAction::Transition(choice) => self.stage_sidebar_ticket_write(
                crate::work_index::WorkItemWrite::TransitionTicket {
                    identifier: ticket.identifier,
                    state: choice.label().to_string(),
                },
            ),
            TicketAction::AssignToMe => {
                let Some(viewer) = context.viewer_id else {
                    return;
                };
                self.stage_sidebar_ticket_write(crate::work_index::WorkItemWrite::AssignTicket {
                    identifier: ticket.identifier,
                    assignee: viewer,
                });
            }
            TicketAction::SetPriority(priority) => self.stage_sidebar_ticket_write(
                crate::work_index::WorkItemWrite::SetTicketPriority {
                    identifier: ticket.identifier,
                    priority,
                },
            ),
            TicketAction::LinkPr => {
                let Some(pr) = crate::ui::dock::pr::focused_pr_key(&self.state) else {
                    return;
                };
                let (Some(number), Some(url)) = (pr.pr_number, pr.pr_url) else {
                    return;
                };
                self.stage_sidebar_ticket_write(
                    crate::work_index::WorkItemWrite::LinkTicketPullRequest {
                        identifier: ticket.identifier,
                        title: format!("{}#{number}", pr.repo),
                        url,
                    },
                );
            }
            TicketAction::Comment => {
                let mut view = crate::app::state::WorkViewState::new(
                    true,
                    self.state.work_index_snapshot.clone(),
                );
                view.projection = crate::app::state::WorkProjection::Tickets;
                view.selected = Some(key);
                view.ticket_comment_draft = Some(String::new());
                self.state.sidebar_object_menu = None;
                self.state.work_view = Some(view);
            }
            TicketAction::OpenInLinear => {
                self.state.sidebar_object_menu = None;
                if let Some(url) = context.url {
                    if let Err(error) = crate::platform::open_url(&url) {
                        self.state.config_diagnostic =
                            Some(format!("could not open ticket: {error}"));
                    }
                }
            }
            TicketAction::CopyLink => {
                self.state.sidebar_object_menu = None;
                if let Some(url) = context.url {
                    self.state.request_clipboard_write = Some(url.into_bytes());
                }
            }
            TicketAction::CopyIdentifier => {
                self.state.sidebar_object_menu = None;
                self.state.request_clipboard_write = Some(ticket.identifier.into_bytes());
            }
            TicketAction::Cancel => self.stage_sidebar_ticket_write(
                crate::work_index::WorkItemWrite::TransitionTicket {
                    identifier: ticket.identifier,
                    state: "Canceled".into(),
                },
            ),
        }
    }

    pub(crate) fn handle_sidebar_settled_key(&mut self, key: KeyEvent) -> bool {
        if self.state.sidebar_settled_menu_target.is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.state.sidebar_settled_menu_target = None;
                    self.state.sidebar_settled_menu_delete_armed = false;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.state.sidebar_settled_menu_delete_armed = false;
                    self.state.sidebar_settled_menu_selected =
                        self.state.sidebar_settled_menu_selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.state.sidebar_settled_menu_delete_armed = false;
                    self.state.sidebar_settled_menu_selected = self
                        .state
                        .sidebar_settled_menu_selected
                        .saturating_add(1)
                        .min(crate::ui::SETTLED_MENU_LABELS.len() - 1);
                }
                KeyCode::Enter => {
                    let index = self.state.sidebar_settled_menu_selected;
                    self.apply_sidebar_settled_menu_action(index);
                }
                _ => {}
            }
            return true;
        }
        let Some(target) = self.state.sidebar_selected_settled.clone() else {
            return false;
        };
        match key.code {
            KeyCode::Enter => {
                if !self.state.settled_target_has_resume_plan(&target) {
                    self.focus_settled_pane(target);
                    return true;
                }
                self.state.sidebar_settled_menu_target = Some(target);
                self.state.sidebar_settled_menu_selected = 0;
                self.state.sidebar_settled_menu_delete_armed = false;
                true
            }
            KeyCode::Esc => {
                self.state.sidebar_selected_settled = None;
                true
            }
            _ => {
                self.state.sidebar_selected_settled = None;
                false
            }
        }
    }

    pub(crate) fn apply_sidebar_settled_menu_action(&mut self, index: usize) {
        let Some(action) = self.state.select_settled_menu_action(index) else {
            return;
        };
        match action {
            SettledMenuAction::Resume(target) => self.resume_settled_pane(target),
            SettledMenuAction::FocusLive(target) => self.focus_settled_pane(target),
            SettledMenuAction::NewThread {
                directory,
                workspace,
            } => self
                .state
                .open_home_composer_in_directory(directory, workspace),
            SettledMenuAction::Delete(target) => {
                let Some(ws_idx) = self
                    .state
                    .workspaces
                    .iter()
                    .position(|workspace| workspace.id == target.workspace_id)
                else {
                    return;
                };
                if let Some(pane_id) = self.public_pane_id(ws_idx, target.pane_id) {
                    self.runtime_pane_close("tui.sidebar.settled.delete", pane_id);
                }
            }
        }
        self.flush_pane_settlement_events();
    }

    pub(crate) fn focus_settled_pane(&mut self, target: crate::app::state::PaneFocusTarget) {
        let Some(ws_idx) = self
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == target.workspace_id)
        else {
            return;
        };
        self.focus_pane_internal_via_api(ws_idx, target.pane_id);
    }

    pub(crate) fn resume_settled_pane(&mut self, target: crate::app::state::PaneFocusTarget) {
        self.state
            .note_pane_activity_at(target.pane_id, std::time::Instant::now());
        self.focus_settled_pane(target);
        self.flush_pane_settlement_events();
    }

    pub(crate) fn settle_sidebar_pane(&mut self, ws_idx: usize, pane_id: crate::layout::PaneId) {
        let Some(workspace) = self.state.workspaces.get(ws_idx) else {
            return;
        };
        if workspace.pane_state(pane_id).is_none() || self.state.pane_is_settled(ws_idx, pane_id) {
            return;
        }
        let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) else {
            return;
        };
        self.runtime_pane_settle("tui.sidebar.settle", public_pane_id);
    }

    pub(crate) fn resume_settled_pane_before_input(&mut self, pane_id: crate::layout::PaneId) {
        let Some(ws_idx) = self
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.pane_state(pane_id).is_some())
        else {
            return;
        };
        if !self.state.pane_is_settled(ws_idx, pane_id) {
            return;
        }
        self.state
            .note_pane_activity_at(pane_id, std::time::Instant::now());
        self.flush_pane_settlement_events();
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
    use ratatui::layout::{Direction, Rect};

    use super::super::{app_for_mouse_test, capture_snapshot, mouse, unique_temp_path};
    use crate::{
        app::state::{AgentPanelSort, DragTarget, Mode, SidebarGroupMode},
        config::SidebarCollapsedModeConfig,
        detect::{Agent, AgentState},
        workspace::Workspace,
    };

    fn sidebar_order_app(settled: bool) -> crate::app::App {
        let mut app = app_for_mouse_test();
        app.state.workspaces = ["alpha", "beta"]
            .into_iter()
            .map(|name| {
                let mut workspace = Workspace::test_new(name);
                workspace.test_add_tab(Some("two"));
                workspace.test_add_tab(Some("three"));
                workspace
            })
            .collect();
        app.state.ensure_test_terminals();
        for ws_idx in 0..app.state.workspaces.len() {
            for tab_idx in 0..app.state.workspaces[ws_idx].tabs.len() {
                let pane_id = app.state.workspaces[ws_idx].tabs[tab_idx].root_pane;
                if settled {
                    app.state.workspaces[ws_idx].tabs[tab_idx]
                        .panes
                        .get_mut(&pane_id)
                        .expect("root pane")
                        .settled_at = Some(1_725_000_000);
                }
                let terminal_id = app.state.workspaces[ws_idx].tabs[tab_idx].panes[&pane_id]
                    .attached_terminal_id
                    .clone();
                app.state
                    .terminals
                    .get_mut(&terminal_id)
                    .expect("fixture terminal")
                    .replace_prevalidated_manual_work_context(
                        crate::work_context::PaneWorkContext {
                            repo: Some(format!("acme/{}", ["alpha", "beta"][ws_idx])),
                            branch: Some(format!("branch-{ws_idx}-{tab_idx}")),
                            pr_urls: vec![format!(
                                "https://github.com/acme/project/pull/{}",
                                ws_idx * 10 + tab_idx + 1
                            )],
                            ticket_ids: vec![format!("SCA-{}", 100 + ws_idx * 10 + tab_idx)],
                            missive_urls: vec![format!(
                                "https://mail.missiveapp.com/#inbox/conversations/{ws_idx}{tab_idx}"
                            )],
                            work_title: Some(format!("work-{ws_idx}-{tab_idx}")),
                            ..Default::default()
                        },
                    );
                if settled {
                    app.state
                        .terminals
                        .get_mut(&terminal_id)
                        .expect("fixture terminal")
                        .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                            source: "herdr:codex".into(),
                            agent: "codex".into(),
                            session_ref: crate::agent_resume::AgentSessionRef::id(format!(
                                "settled-{ws_idx}-{tab_idx}"
                            ))
                            .expect("valid session id"),
                        });
                }
            }
        }
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.reconcile_sidebar_presentation();
        app.state.collapsed_sidebar_groups.clear();
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        app
    }

    #[test]
    fn section_toggle_queues_explicit_expand_and_collapse_for_persistence() {
        let mut app = crate::app::state::AppState::test_new();
        assert!(app.collapsed_sidebar_groups.contains("repo:Runs"));

        app.toggle_sidebar_group(crate::ui::sidebar::RUNS_SECTION_TITLE);
        assert!(!app.collapsed_sidebar_groups.contains("repo:Runs"));
        assert_eq!(
            app.take_sidebar_group_collapsed_persistence_request(),
            Some(("repo:Runs".to_string(), false))
        );

        app.toggle_sidebar_group(crate::ui::sidebar::RUNS_SECTION_TITLE);
        assert!(app.collapsed_sidebar_groups.contains("repo:Runs"));
        assert_eq!(
            app.take_sidebar_group_collapsed_persistence_request(),
            Some(("repo:Runs".to_string(), true))
        );
    }

    fn sidebar_order_signature(app: &crate::app::state::AppState) -> Vec<String> {
        crate::ui::sidebar_rows(app)
            .into_iter()
            .map(|row| match row {
                crate::ui::SidebarRow::Workspace {
                    ws_idx, indented, ..
                } => {
                    format!("workspace:{ws_idx}:{indented}")
                }
                crate::ui::SidebarRow::Tab { entry, .. } => {
                    let target = entry.local_target().unwrap();
                    format!("tab:{}:{}", target.ws_idx, target.tab_idx)
                }
                crate::ui::SidebarRow::Agent { entry, .. } => {
                    let target = entry.local_target().unwrap();
                    format!(
                        "pane:{}:{}:{}",
                        target.ws_idx,
                        target.tab_idx,
                        target.pane_id.raw()
                    )
                }
                crate::ui::SidebarRow::RemoteAgent { entry, .. } => {
                    format!("remote:{}", entry.agent_ref)
                }
                crate::ui::SidebarRow::SectionHeader { title, .. } => {
                    format!("section:{title}")
                }
                crate::ui::SidebarRow::Divider => "divider".to_string(),
                crate::ui::SidebarRow::NestedHeader { key, .. } => format!("group:{key}"),
                crate::ui::SidebarRow::SymphonyJob { name, .. } => format!("symphony:{name}"),
                crate::ui::SidebarRow::SymphonyEmpty => "symphony:empty".to_string(),
                crate::ui::SidebarRow::AgentRun { host, summary } => summary.map_or_else(
                    || format!("run:{host}:empty"),
                    |summary| format!("run:{host}:{}", summary.run_id),
                ),
                crate::ui::SidebarRow::AloopLoop { name, .. } => format!("aloop:{name}"),
                crate::ui::SidebarRow::AloopRunLine { key, .. }
                | crate::ui::SidebarRow::AloopFinding { key, .. }
                | crate::ui::SidebarRow::AloopCleanRuns { key, .. }
                | crate::ui::SidebarRow::AloopCleanRun { key, .. } => format!("aloop:row:{key}"),
                crate::ui::SidebarRow::AloopUnreachable { .. } => "aloop:unreachable".to_string(),
                crate::ui::SidebarRow::AloopEmpty => "aloop:empty".to_string(),
            })
            .collect()
    }

    fn assert_sidebar_click_preserves_order(mode: SidebarGroupMode, settled: bool) {
        let mut app = sidebar_order_app(settled);
        app.state.set_sidebar_group_mode(mode);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let before = sidebar_order_signature(&app.state);
        let before_storage = app
            .state
            .workspaces
            .iter()
            .map(|workspace| {
                workspace
                    .tabs
                    .iter()
                    .map(|tab| tab.root_pane)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let target = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect)
            .into_iter()
            .nth(2)
            .expect("third sidebar tab row");
        let target_rect = target.rect;
        let target_ws_idx = target.ws_idx;
        let target_tab_idx = target.tab_idx;
        let target_pane_id = target.pane_id;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            target_rect.x + 1,
            target_rect.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            target_rect.x + 1,
            target_rect.y,
        ));

        if settled {
            assert_eq!(app.state.active, Some(target_ws_idx));
            assert_eq!(
                app.state.workspaces[target_ws_idx].focused_pane_id(),
                Some(target_pane_id)
            );
            assert!(app.state.pane_is_settled(target_ws_idx, target_pane_id));
            assert!(app.state.sidebar_selected_settled.is_none());
        } else {
            assert_eq!(app.state.active, Some(target_ws_idx));
            assert_eq!(
                app.state.workspaces[target_ws_idx].active_tab,
                target_tab_idx
            );
        }
        if !settled {
            assert_eq!(
                sidebar_order_signature(&app.state),
                before,
                "{mode:?}, settled={settled}"
            );
        }
        assert_eq!(
            app.state
                .workspaces
                .iter()
                .map(|workspace| {
                    workspace
                        .tabs
                        .iter()
                        .map(|tab| tab.root_pane)
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>(),
            before_storage,
            "{mode:?}, settled={settled}"
        );
    }

    #[test]
    fn sidebar_click_preserves_order_in_every_group_mode_and_settled() {
        for mode in SidebarGroupMode::ALL {
            assert_sidebar_click_preserves_order(mode, false);
            assert_sidebar_click_preserves_order(mode, true);
        }
    }

    #[test]
    fn clicking_resumable_settled_row_focuses_without_unsettling_its_pane() {
        for mode in SidebarGroupMode::ALL {
            let mut app = sidebar_order_app(true);
            app.state.set_sidebar_group_mode(mode);
            crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
            let target = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect)
                .into_iter()
                .nth(2)
                .expect("third settled sidebar tab row");

            app.handle_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                target.rect.x + 1,
                target.rect.y,
            ));
            app.handle_mouse(mouse(
                MouseEventKind::Up(MouseButton::Left),
                target.rect.x + 1,
                target.rect.y,
            ));

            assert_eq!(app.state.active, Some(target.ws_idx), "{mode:?}");
            assert_eq!(
                app.state.workspaces[target.ws_idx].focused_pane_id(),
                Some(target.pane_id),
                "{mode:?}"
            );
            assert!(
                app.state.pane_is_settled(target.ws_idx, target.pane_id),
                "{mode:?}"
            );
            assert!(app.state.sidebar_selected_settled.is_none(), "{mode:?}");
        }
    }

    #[test]
    fn clicking_sidebar_settle_icon_settles_the_exact_pane() {
        let mut app = sidebar_order_app(false);
        // Nested rows need room for both selected-row controls. The 26-column
        // default intentionally preserves the title instead of drawing them.
        app.state.sidebar_width = 40;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let target = app
            .state
            .view
            .sidebar_hover_targets
            .iter()
            .find(|target| target.label == "Settle")
            .cloned()
            .expect("settle icon target");
        let crate::app::state::SidebarHoverAction::Settle { ws_idx, pane_id } =
            target.action.expect("settle action")
        else {
            panic!("settle target carried a different action");
        };

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            target.rect.x,
            target.rect.y,
        ));

        assert!(app.state.pane_is_settled(ws_idx, pane_id));
    }

    #[test]
    fn clicking_sidebar_snooze_opens_durations_and_dispatches_the_api() {
        let mut app = sidebar_order_app(false);
        app.state.sidebar_width = 40;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let target = app
            .state
            .view
            .sidebar_hover_targets
            .iter()
            .find(|target| target.label == "Set time")
            .cloned()
            .expect("snooze control target");
        let crate::app::state::SidebarHoverAction::Snooze { ws_idx, pane_id } =
            target.action.expect("snooze action")
        else {
            panic!("snooze target carried a different action");
        };

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            target.rect.x,
            target.rect.y,
        ));
        assert!(app.state.sidebar_snooze.is_some());
        let before = crate::app::settled::unix_seconds(std::time::SystemTime::now());
        app.handle_sidebar_snooze_menu_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        let deadline = app.state.workspaces[ws_idx]
            .pane_state(pane_id)
            .and_then(crate::pane::PaneState::snoozed_until)
            .expect("pane snoozed through runtime API");
        assert!((before + 15 * 60..=before + 15 * 60 + 1).contains(&deadline));
        assert!(crate::ui::sidebar_rows(&app.state)
            .iter()
            .any(|row| matches!(
                row,
                crate::ui::SidebarRow::SectionHeader {
                    title: crate::ui::sidebar::SNOOZED_SECTION_TITLE,
                    ..
                }
            )));
    }

    #[test]
    fn snoozed_section_timer_dropdown_unsnoozes_the_exact_pane() {
        let mut app = sidebar_order_app(false);
        app.state.sidebar_width = 40;
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let deadline = crate::app::settled::unix_seconds(std::time::SystemTime::now()) + 900;
        assert!(app.state.snooze_pane_at(0, pane_id, deadline));
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let target = app
            .state
            .view
            .sidebar_hover_targets
            .iter()
            .find(|target| {
                target.label.starts_with("Unsnoozes")
                    && matches!(
                        target.action.as_ref(),
                        Some(crate::app::state::SidebarHoverAction::Snooze {
                            pane_id: target_pane,
                            ..
                        }) if *target_pane == pane_id
                    )
            })
            .cloned()
            .expect("timer control in Snoozed section");

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            target.rect.x,
            target.rect.y,
        ));
        assert!(app.state.sidebar_snooze.as_ref().is_some_and(|snooze| {
            snooze.time_draft.is_none() && app.state.pane_is_snoozed(0, snooze.target.pane_id)
        }));
        app.handle_sidebar_snooze_menu_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert!(!app.state.pane_is_snoozed(0, pane_id));
    }

    #[test]
    fn snooze_presets_map_to_bounded_api_requests() {
        let expected = [
            ("Snooze for 15 minutes", Some(15 * 60), None),
            ("Snooze for 1 hour", Some(60 * 60), None),
            ("Snooze for 4 hours", Some(4 * 60 * 60), None),
            ("Snooze until tomorrow at 09:00", None, Some(1_725_033_600)),
        ];

        for ((label, action), (expected_label, duration_s, snoozed_until)) in
            crate::app::state::SNOOZE_MENU_ITEMS
                .into_iter()
                .take(4)
                .zip(expected)
        {
            let crate::app::state::SidebarSnoozeMenuAction::Preset(preset) = action else {
                panic!("duration row must dispatch a preset");
            };
            let params = super::sidebar_snooze_params(
                "workspace:pane".to_string(),
                preset,
                Some(1_725_033_600),
            )
            .expect("bounded snooze request");
            assert_eq!(label, expected_label);
            assert_eq!(params.pane_id, "workspace:pane");
            assert_eq!(params.duration_s, duration_s);
            assert_eq!(params.snoozed_until, snoozed_until);
        }
    }

    #[tokio::test]
    async fn sidebar_keyboard_reaches_snooze_and_settle_controls_through_input_routing() {
        let mut snooze = sidebar_order_app(false);
        crate::ui::compute_view(&mut snooze.state, Rect::new(0, 0, 120, 40));
        snooze.state.sidebar_focused = true;
        let _ = snooze
            .handle_key_inner(crate::input::TerminalKey::new(
                KeyCode::Char('z'),
                KeyModifiers::empty(),
            ))
            .await;
        assert!(snooze.state.sidebar_snooze.is_some());
        let _ = snooze
            .handle_key_inner(crate::input::TerminalKey::new(
                KeyCode::Enter,
                KeyModifiers::empty(),
            ))
            .await;
        let snoozed_pane = snooze.state.workspaces[0]
            .focused_pane_id()
            .expect("focused pane");
        assert!(snooze.state.pane_is_snoozed(0, snoozed_pane));

        let mut settle = sidebar_order_app(false);
        settle.state.sidebar_focused = true;
        let _ = settle
            .handle_key_inner(crate::input::TerminalKey::new(
                KeyCode::Char('s'),
                KeyModifiers::empty(),
            ))
            .await;
        let settled_pane = settle.state.workspaces[0]
            .focused_pane_id()
            .expect("focused pane");
        assert!(settle.state.pane_is_settled(0, settled_pane));
    }

    #[tokio::test]
    async fn sidebar_focus_and_snooze_editor_are_isolated_between_clients() {
        let mut app = sidebar_order_app(false);
        let editor_pane = app.state.workspaces[0].tabs[0].root_pane;
        let client_pane = app.state.workspaces[0].tabs[1].root_pane;
        let mut client_a = crate::app::state::SidebarPresentationState::default();
        let mut client_b = crate::app::state::SidebarPresentationState::default();

        app.state.swap_sidebar_presentation(&mut client_a);
        app.state.focus_client_on_sidebar();
        app.open_snooze_time_input(0, editor_pane);
        if let Some(snooze) = app.state.sidebar_snooze.as_mut() {
            snooze.time_draft = Some("14:30".into());
            snooze.error = Some("client A only".into());
        }
        app.state.swap_sidebar_presentation(&mut client_a);

        app.state.swap_sidebar_presentation(&mut client_b);
        assert!(!app.state.sidebar_focused);
        assert!(app.state.sidebar_snooze.is_none());
        app.state.sidebar_focused = true;
        app.state.workspaces[0].active_tab = 1;
        for key in ['z', '\n', 'z', '\n', 's'] {
            let code = if key == '\n' {
                KeyCode::Enter
            } else {
                KeyCode::Char(key)
            };
            let _ = app
                .handle_key_inner(crate::input::TerminalKey::new(code, KeyModifiers::empty()))
                .await;
        }
        assert!(app.state.pane_is_settled(0, client_pane));
        assert!(!app.state.pane_is_settled(0, editor_pane));
        assert!(!app.state.pane_is_snoozed(0, editor_pane));
        app.state.swap_sidebar_presentation(&mut client_b);

        app.state.swap_sidebar_presentation(&mut client_a);
        assert!(app.state.sidebar_focused);
        let editor = app.state.sidebar_snooze.as_ref().expect("client A editor");
        assert_eq!(editor.target.pane_id, editor_pane);
        assert_eq!(editor.time_draft.as_deref(), Some("14:30"));
        assert_eq!(editor.error.as_deref(), Some("client A only"));
    }

    #[test]
    fn remote_sidebar_selection_blocks_local_snooze_and_settle_shortcuts() {
        let mut app = sidebar_order_app(false);
        let local_pane = app.state.workspaces[0].tabs[0].root_pane;
        app.state.sidebar_focused = true;
        app.state.sidebar_selected_remote_agent = Some(
            crate::api::schema::AgentRef::new("offline", "remote-pane")
                .expect("valid remote reference"),
        );

        for key in ['z', 's'] {
            assert!(!app.handle_sidebar_session_action_key(KeyEvent::new(
                KeyCode::Char(key),
                KeyModifiers::empty(),
            )));
        }
        assert!(app.state.sidebar_snooze.is_none());
        assert!(!app.state.pane_is_settled(0, local_pane));
    }

    #[test]
    fn failed_remote_selection_does_not_block_another_clients_session_shortcuts() {
        let mut app = sidebar_order_app(false);
        let local_pane = app.state.workspaces[0].tabs[0].root_pane;
        let mut client_a = crate::app::state::SidebarPresentationState::default();
        let mut client_b = crate::app::state::SidebarPresentationState::default();

        app.state.swap_sidebar_presentation(&mut client_a);
        app.state.sidebar_focused = true;
        app.state.sidebar_selected_remote_agent = Some(
            crate::api::schema::AgentRef::new("offline", "failed-attach")
                .expect("valid remote reference"),
        );
        app.state.swap_sidebar_presentation(&mut client_a);

        app.state.swap_sidebar_presentation(&mut client_b);
        app.state.sidebar_focused = true;
        assert!(app.handle_sidebar_session_action_key(KeyEvent::new(
            KeyCode::Char('z'),
            KeyModifiers::empty(),
        )));
        assert!(app.state.sidebar_snooze.is_some());
        app.state.sidebar_snooze = None;
        assert!(app.handle_sidebar_session_action_key(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::empty(),
        )));
        assert!(app.state.pane_is_settled(0, local_pane));
        app.state.swap_sidebar_presentation(&mut client_b);

        app.state.swap_sidebar_presentation(&mut client_a);
        assert!(app.state.sidebar_selected_remote_agent.is_some());
    }

    #[test]
    fn sidebar_session_shortcuts_do_not_fire_through_an_open_menu() {
        let mut app = sidebar_order_app(false);
        app.state.sidebar_focused = true;
        app.state.sidebar_group_menu_open = true;
        let pane_id = app.state.workspaces[0]
            .focused_pane_id()
            .expect("focused pane");

        assert!(!app.handle_sidebar_session_action_key(KeyEvent::new(
            KeyCode::Char('z'),
            KeyModifiers::empty(),
        )));
        assert!(!app.handle_sidebar_session_action_key(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::empty(),
        )));
        assert!(app.state.sidebar_snooze.is_none());
        assert!(!app.state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn clicking_settled_workspace_header_focuses_without_unsettling_its_pane() {
        for mode in [
            SidebarGroupMode::Repo,
            SidebarGroupMode::RepoWorktree,
            SidebarGroupMode::Spaces,
        ] {
            let mut app = sidebar_order_app(true);
            let target_ws_idx = 1;
            let hidden_sibling =
                app.state.workspaces[target_ws_idx].test_split(Direction::Horizontal);
            app.state.workspaces[target_ws_idx].tabs[0]
                .panes
                .get_mut(&hidden_sibling)
                .expect("hidden sibling")
                .settled_at = Some(1_725_000_001);
            app.state.ensure_test_terminals();
            app.state.set_sidebar_group_mode(mode);
            crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
            let target =
                crate::ui::compute_workspace_card_areas(&app.state, app.state.view.sidebar_rect)
                    .into_iter()
                    .find(|card| card.ws_idx == target_ws_idx && card.settled_pane_id.is_some())
                    .expect("settled workspace header");
            let target_pane_id = target.settled_pane_id.expect("represented settled pane");

            app.handle_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                target.rect.x + 8,
                target.rect.y,
            ));
            app.handle_mouse(mouse(
                MouseEventKind::Up(MouseButton::Left),
                target.rect.x + 8,
                target.rect.y,
            ));

            assert_eq!(app.state.active, Some(target_ws_idx), "{mode:?}");
            assert_eq!(
                app.state.workspaces[target_ws_idx].focused_pane_id(),
                Some(target_pane_id),
                "{mode:?}"
            );
            assert!(
                app.state.pane_is_settled(target_ws_idx, target_pane_id),
                "{mode:?}"
            );
            assert!(
                app.state.pane_is_settled(target_ws_idx, hidden_sibling),
                "{mode:?}: group header resumed an unrelated hidden sibling"
            );
            assert!(app.state.sidebar_selected_settled.is_none(), "{mode:?}");
        }
    }

    #[test]
    fn settled_workspace_header_targets_its_settled_pane_in_a_mixed_workspace() {
        let mut app = sidebar_order_app(false);
        app.state.set_sidebar_group_mode(SidebarGroupMode::Repo);
        let target_ws_idx = 0;
        let target_tab_idx = 2;
        let target_pane_id = app.state.workspaces[target_ws_idx].tabs[target_tab_idx].root_pane;
        app.state.workspaces[target_ws_idx].tabs[target_tab_idx]
            .panes
            .get_mut(&target_pane_id)
            .expect("target pane")
            .settled_at = Some(1_725_000_000);
        let terminal_id = app.state.workspaces[target_ws_idx].tabs[target_tab_idx].panes
            [&target_pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("target terminal")
            .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("mixed-settled")
                    .expect("valid session id"),
            });
        app.state.reconcile_sidebar_presentation();
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        assert_ne!(
            app.state.workspaces[target_ws_idx].focused_pane_id(),
            Some(target_pane_id)
        );
        let cards =
            crate::ui::compute_workspace_card_areas(&app.state, app.state.view.sidebar_rect);
        assert!(cards
            .iter()
            .any(|card| { card.ws_idx == target_ws_idx && card.settled_pane_id.is_none() }));
        let target = cards
            .into_iter()
            .find(|card| {
                card.ws_idx == target_ws_idx && card.settled_pane_id == Some(target_pane_id)
            })
            .expect("mixed workspace settled header");

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            target.rect.x + 8,
            target.rect.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            target.rect.x + 8,
            target.rect.y,
        ));

        assert_eq!(app.state.active, Some(target_ws_idx));
        assert_eq!(
            app.state.workspaces[target_ws_idx].active_tab_index(),
            target_tab_idx
        );
        assert_eq!(
            app.state.workspaces[target_ws_idx].focused_pane_id(),
            Some(target_pane_id)
        );
        assert!(app.state.pane_is_settled(target_ws_idx, target_pane_id));
        assert!(app.state.sidebar_selected_settled.is_none());
    }

    #[test]
    fn dragging_a_settled_row_does_not_resume_it() {
        let mut app = sidebar_order_app(true);
        app.state.set_sidebar_group_mode(SidebarGroupMode::Repo);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let cards =
            crate::ui::compute_workspace_card_areas(&app.state, app.state.view.sidebar_rect);
        let source = cards
            .iter()
            .find(|card| card.ws_idx == 0 && card.settled_pane_id.is_some())
            .expect("settled source workspace row");
        let destination = cards
            .iter()
            .find(|card| card.ws_idx == 1 && card.settled_pane_id.is_some())
            .expect("settled destination workspace row");
        let workspace_id = app.state.workspaces[source.ws_idx].id.clone();
        let pane_id = source.settled_pane_id.expect("settled source pane");

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            source.rect.x + 8,
            source.rect.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            destination.rect.x + 8,
            destination.rect.y,
        ));
        assert!(matches!(
            app.state.drag.as_ref().map(|drag| &drag.target),
            Some(DragTarget::WorkspaceReorder { .. })
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            destination.rect.x + 8,
            destination.rect.y,
        ));

        let ws_idx = app
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == workspace_id)
            .expect("moved workspace");
        assert!(app.state.pane_is_settled(ws_idx, pane_id));
        assert!(app.state.sidebar_selected_settled.is_none());
    }

    /// One workspace with three tabs in a single flat repo group, named so the
    /// canonical order and the alphabetical order disagree.
    fn sidebar_sort_mouse_app() -> crate::app::App {
        let mut app = app_for_mouse_test();
        let mut workspace = Workspace::test_new("ws");
        workspace.test_add_tab(Some("zeta"));
        workspace.test_add_tab(Some("alpha"));
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.workspaces[0].tabs[0].custom_name = Some("mike".to_string());
        for tab_idx in 0..3 {
            let pane_id = app.state.workspaces[0].tabs[tab_idx].root_pane;
            let terminal_id = app.state.workspaces[0].tabs[tab_idx].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let terminal = app
                .state
                .terminals
                .get_mut(&terminal_id)
                .expect("fixture terminal");
            terminal.replace_prevalidated_manual_work_context(
                crate::work_context::PaneWorkContext {
                    repo: Some("acme/one".into()),
                    ..Default::default()
                },
            );
            terminal.detected_agent = Some(Agent::Pi);
            terminal.set_raw_agent_state_for_test(AgentState::Working);
            terminal.last_agent_state_change_seq = Some(tab_idx as u64 + 1);
        }
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.reconcile_sidebar_presentation();
        app
    }

    #[test]
    fn clicking_the_sort_glyph_opens_the_dropdown_and_picking_applies_the_sort() {
        let mut app = sidebar_order_app(false);
        app.state.set_sidebar_group_mode(SidebarGroupMode::Repo);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let sidebar_area = Rect::new(
            app.state.view.sidebar_rect.x,
            app.state.view.sidebar_rect.y,
            app.state.view.sidebar_rect.width.saturating_add(1),
            app.state.view.sidebar_rect.height,
        );
        let card = crate::ui::compute_workspace_card_areas(&app.state, sidebar_area)
            .first()
            .expect("repo group card")
            .rect;
        let (glyph_col, glyph_row) = (card.right() - 1, card.y);
        assert_eq!(
            crate::ui::sidebar::sidebar_group_sort_at(&app.state, glyph_col, glyph_row),
            Some(("repo:acme/alpha".to_string(), glyph_col)),
            "the repo group header's last cell is its sort control"
        );

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            glyph_col,
            glyph_row,
        ));
        let menu = app
            .state
            .sidebar_sort_menu
            .as_ref()
            .expect("clicking the glyph opens the sort dropdown");
        assert_eq!(menu.target, "repo:acme/alpha");
        assert_eq!(menu.current, crate::app::state::SidebarSortMode::Default);
        assert_eq!(menu.selected, 0, "the cursor starts on the current choice");

        let layout =
            crate::ui::sidebar::sidebar_sort_menu_layout(&app.state, app.state.screen_rect())
                .expect("sort dropdown layout");
        let name_row = layout.list_rect.y + 1;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            layout.list_rect.x + 1,
            name_row,
        ));
        assert_eq!(
            app.state.sidebar_group_sort("repo:acme/alpha"),
            crate::app::state::SidebarSortMode::Name,
            "picking a row applies the sort to exactly that group"
        );
        assert!(app.state.sidebar_sort_menu.is_none(), "the menu closes");
        assert_eq!(
            app.state.take_sidebar_group_sort_persistence_request(),
            Some((
                "repo:acme/alpha".to_string(),
                crate::app::state::SidebarSortMode::Name
            )),
            "the choice is queued for client-local persistence"
        );
    }

    #[test]
    fn clicking_a_sorted_row_focuses_that_rows_pane() {
        let mut app = sidebar_sort_mouse_app();
        app.state.set_sidebar_group_sort(
            "repo:acme/one".to_string(),
            crate::app::state::SidebarSortMode::Name,
        );
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let cards = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect);
        let first = cards.first().expect("first sorted tab row");
        assert_eq!(first.tab_idx, 2, "alpha sorts above mike and zeta");
        let (target_ws, target_tab, target_pane) = (first.ws_idx, first.tab_idx, first.pane_id);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            first.rect.x + 1,
            first.rect.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            first.rect.x + 1,
            first.rect.y,
        ));

        assert_eq!(app.state.active, Some(target_ws));
        assert_eq!(app.state.workspaces[target_ws].active_tab, target_tab);
        assert_eq!(
            app.state.workspaces[target_ws].tabs[target_tab]
                .layout
                .focused(),
            target_pane,
            "the click lands on the row drawn there after sorting, not the row that used to be there"
        );
    }

    #[test]
    fn subgroup_picker_creates_a_name_then_offers_it_to_the_next_window() {
        let mut app = sidebar_sort_mouse_app();
        app.state.sidebar_subgroup_picker = Some(crate::app::state::SidebarSubgroupPickerState {
            ws_idx: 0,
            tab_idx: 0,
            anchor: (5, 5),
            filter: crate::ui::dropdown::DropdownFilterState::default(),
        });
        for character in "api".chars() {
            assert!(app.state.handle_sidebar_subgroup_picker_key(KeyEvent::new(
                KeyCode::Char(character),
                KeyModifiers::empty(),
            )));
        }
        let choices = crate::ui::sidebar::sidebar_subgroup_picker_choices(&app.state);
        assert_eq!(
            choices,
            vec![crate::ui::sidebar::SidebarSubgroupChoice::Create(
                "api".to_string()
            )],
            "an unknown name is offered as a create row"
        );
        assert!(app.state.handle_sidebar_subgroup_picker_key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::empty(),
        )));
        assert_eq!(
            app.state.workspaces[0].tabs[0].subgroup(),
            Some("api"),
            "Enter assigns the typed name"
        );
        assert!(app.state.sidebar_subgroup_picker.is_none());

        // The next window's picker offers the name the group already has.
        app.state.sidebar_subgroup_picker = Some(crate::app::state::SidebarSubgroupPickerState {
            ws_idx: 0,
            tab_idx: 1,
            anchor: (5, 5),
            filter: crate::ui::dropdown::DropdownFilterState::default(),
        });
        let choices = crate::ui::sidebar::sidebar_subgroup_picker_choices(&app.state);
        assert_eq!(
            choices,
            vec![crate::ui::sidebar::SidebarSubgroupChoice::Existing(
                "api".to_string()
            )],
            "an existing subgroup name is a pick, not a retype"
        );
        assert!(app.state.handle_sidebar_subgroup_picker_key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::empty(),
        )));
        assert_eq!(app.state.workspaces[0].tabs[1].subgroup(), Some("api"));

        app.state.clear_tab_subgroup(0, 1);
        assert_eq!(app.state.workspaces[0].tabs[1].subgroup(), None);
    }

    fn settled_target(app: &mut crate::app::App) -> crate::app::state::PaneFocusTarget {
        if app.state.workspaces.is_empty() {
            app.state.workspaces.push(Workspace::test_new("settled"));
            app.state.active = Some(0);
            app.state.selected = 0;
            app.state.ensure_test_terminals();
        }
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane")
            .settled_at = Some(1_725_000_000);
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal")
            .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("settled-menu")
                    .expect("valid session id"),
            });
        crate::app::state::PaneFocusTarget {
            workspace_id: app.state.workspaces[0].id.clone(),
            pane_id,
        }
    }

    #[tokio::test]
    async fn a_pane_keeps_its_own_keys_while_a_sidebar_row_stays_selected() {
        let mut app = app_for_mouse_test();
        app.state = crate::ui::sidebar_work_item_fixture();
        app.state.set_server_mode(crate::app::Mode::Terminal);
        app.state.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        app.state.sidebar_selected_work_group = Some("linear:SCA-3102".into());
        app.state.sidebar_focused = false;

        // `m` opens the sidebar object menu, but only for the sidebar. Typed
        // into a pane it is just a letter.
        let consumed = app
            .handle_key_inner(crate::input::TerminalKey::new(
                KeyCode::Char('m'),
                KeyModifiers::empty(),
            ))
            .await;
        assert!(
            app.state.sidebar_object_menu.is_none(),
            "a selected sidebar row must not claim keys the pane owns"
        );
        drop(consumed);

        app.state.focus_client_on_sidebar();
        assert_eq!(
            app.state.input_owner(),
            crate::app::state::InputOwner::Sidebar
        );
        assert_eq!(
            app.state.sidebar_selected_work_group.as_deref(),
            Some("linear:SCA-3102")
        );
        let _ = app
            .handle_key_inner(crate::input::TerminalKey::new(
                KeyCode::Char('m'),
                KeyModifiers::empty(),
            ))
            .await;
        assert_eq!(
            app.state
                .sidebar_object_menu
                .as_ref()
                .map(|menu| menu.target.as_str()),
            Some("linear:SCA-3102"),
            "the same key still works once the sidebar owns the keyboard"
        );
    }

    #[test]
    fn focusing_a_pane_releases_the_sidebar_keyboard() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_focused = true;
        app.state.release_dock_focus_to_pane();
        assert!(!app.state.sidebar_focused);
    }

    #[test]
    fn sidebar_object_menu_keyboard_opens_and_escape_closes() {
        let mut app = app_for_mouse_test();
        app.state = crate::ui::sidebar_work_item_fixture();
        app.state.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        app.state.sidebar_selected_work_group = Some("linear:SCA-3102".into());

        assert!(app.handle_sidebar_object_menu_key(KeyEvent::new(
            KeyCode::Char('m'),
            KeyModifiers::empty(),
        )));
        assert_eq!(
            app.state
                .sidebar_object_menu
                .as_ref()
                .map(|menu| menu.target.as_str()),
            Some("linear:SCA-3102")
        );
        assert!(
            app.handle_sidebar_object_menu_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty(),))
        );
        assert!(app.state.sidebar_object_menu.is_none());
    }

    #[test]
    fn sidebar_ellipsis_click_anchors_the_object_menu_to_that_row() {
        let mut app = app_for_mouse_test();
        app.state = crate::ui::sidebar_work_item_fixture();
        app.state.sidebar_group_mode = SidebarGroupMode::LinearTeam;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 80, 24));
        let (col, row) = (app.state.view.sidebar_rect.y..app.state.view.sidebar_rect.bottom())
            .find_map(|row| {
                (app.state.view.sidebar_rect.x..app.state.view.sidebar_rect.right())
                    .find(|col| {
                        crate::ui::sidebar_object_action_at(&app.state, *col, row).is_some()
                    })
                    .map(|col| (col, row))
            })
            .expect("Linear object action row");

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));

        assert_eq!(
            app.state
                .sidebar_object_menu
                .as_ref()
                .and_then(|menu| menu.anchor_row),
            Some(row)
        );
        let layout =
            crate::ui::sidebar_object_menu_layout_for_test(&app.state, Rect::new(0, 0, 80, 24))
                .expect("sidebar action menu");
        assert_eq!(layout.rect.y, row + 1);
    }

    #[test]
    fn f27_sidebar_row_click_opens_linked_home_without_starting_a_pane() {
        let mut app = app_for_mouse_test();
        app.state = crate::ui::sidebar_work_item_fixture();
        app.state.sidebar_group_mode = SidebarGroupMode::RepoPr;
        app.state.collapsed_sidebar_groups.clear();
        app.state.sidebar_work_filter.github.assignee = None;
        app.state.dock_collapsed = false;
        app.state
            .work_index_snapshot
            .as_mut()
            .and_then(|snapshot| {
                snapshot
                    .items
                    .iter_mut()
                    .find(|item| item.pr_number == Some(159))
            })
            .expect("pull request fixture")
            .source
            .github = true;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let target = "github:https://github.com/scalable-so/herdr/pull/159";
        let row = (app.state.view.sidebar_rect.y..app.state.view.sidebar_rect.bottom())
            .find(|row| {
                crate::ui::sidebar_dim_header_at(&app.state, *row).as_deref() == Some(target)
            })
            .expect("No agent yet PR row");
        let pane_count = app.state.workspaces[0].tabs[0].panes.len();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            app.state.view.sidebar_rect.x.saturating_add(2),
            row,
        ));

        assert_eq!(app.state.workspaces[0].tabs[0].panes.len(), pane_count);
        assert!(app.state.dock_object_preview.is_none());
        assert_eq!(
            app.state
                .home
                .as_ref()
                .and_then(|home| home.pr.as_ref())
                .map(|pr| (pr.repo.as_str(), pr.number)),
            Some(("scalable-so/herdr", 159))
        );
    }

    #[test]
    fn sidebar_pr_writes_use_the_shared_modal_confirmation_gate() {
        let mut app = app_for_mouse_test();
        app.state = crate::ui::sidebar_work_item_fixture();
        app.work_index_gh_program_override = Some(std::path::PathBuf::from("/usr/bin/false"));
        app.state.open_sidebar_object_menu(
            "github:https://github.com/scalable-so/herdr/pull/159".into(),
        );

        app.apply_sidebar_object_menu_action(11);
        assert!(matches!(
            app.state.pr_action_confirmation,
            Some(crate::app::state::PrActionConfirmation {
                ref key,
                action: crate::ui::work_list_detail::PrActionKind::Close,
            }) if key.repo == "scalable-so/herdr" && key.pr_number == Some(159)
        ));
        assert!(app.state.sidebar_object_menu.is_none());
        assert!(app.state.dock_pending_write.is_none());
        assert!(app.state.dock_write_notice.is_none());

        assert!(app.handle_pr_action_confirmation_key(KeyEvent::new(
            KeyCode::Char('y'),
            KeyModifiers::empty(),
        )));
        assert!(app.state.dock_pending_write.is_none());
        assert!(app.state.sidebar_object_menu.is_none());
        assert!(app
            .state
            .dock_write_notice
            .as_deref()
            .is_some_and(|notice| notice.contains("failed")));
    }

    #[test]
    fn sidebar_ticket_transition_stages_without_running() {
        let mut app = app_for_mouse_test();
        app.state = crate::ui::sidebar_work_item_fixture();
        app.work_index_linearis_program_override = Some(std::path::PathBuf::from("/usr/bin/false"));
        app.state.open_sidebar_object_menu("linear:SCA-3102".into());

        app.apply_sidebar_object_menu_action(4);
        assert_eq!(
            app.state.sidebar_object_menu.as_ref().map(|menu| menu.page),
            Some(crate::app::state::SidebarObjectMenuPage::TicketTransitions)
        );
        app.apply_sidebar_object_menu_action(3);
        assert_eq!(
            app.state.dock_pending_write,
            Some(crate::work_index::WorkItemWrite::TransitionTicket {
                identifier: "SCA-3102".into(),
                state: "Done".into(),
            })
        );
        assert!(app.state.dock_write_notice.is_none());
        assert!(
            app.handle_sidebar_object_menu_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty(),))
        );
        assert!(app.state.dock_pending_write.is_none());
    }

    #[test]
    fn sidebar_missive_action_only_requests_a_clipboard_copy() {
        let mut app = app_for_mouse_test();
        app.state = crate::ui::sidebar_work_item_fixture();
        let web_url = "https://mail.missiveapp.com/#inbox/conversations/sample";
        let app_url = "missive://mail.missiveapp.com/#inbox/conversations/sample";
        app.state
            .work_index_snapshot
            .as_mut()
            .expect("work snapshot")
            .conversations
            .push(crate::work_index::MissiveConversation {
                id: "sample".into(),
                subject: "Billing question".into(),
                app_url: app_url.into(),
                web_url: web_url.into(),
                team: None,
                assignees: Vec::new(),
                last_activity_at: None,
                closed: false,
                labels: Vec::new(),
                pane_bound: false,
                messages: Vec::new(),
                notes: Vec::new(),
                drafts: Vec::new(),
                posts: Vec::new(),
            });
        app.state
            .open_sidebar_object_menu(format!("missive:{web_url}"));

        app.apply_sidebar_object_menu_action(0);

        assert_eq!(
            app.state.request_clipboard_write,
            Some(app_url.as_bytes().to_vec())
        );
        assert!(app.state.sidebar_object_menu.is_none());
        assert!(app.state.dock_pending_write.is_none());
    }

    #[test]
    fn settled_menu_items_yield_resume_delete_and_home_dispatch_plans() {
        let mut app = app_for_mouse_test();
        let target = settled_target(&mut app);

        app.state.sidebar_settled_menu_target = Some(target.clone());
        assert_eq!(
            app.state.select_settled_menu_action(0),
            Some(super::SettledMenuAction::Resume(target.clone()))
        );
        // Delete asks once while ui.confirm_close is on.
        app.state.sidebar_settled_menu_target = Some(target.clone());
        assert_eq!(app.state.select_settled_menu_action(3), None);
        assert!(app.state.sidebar_settled_menu_delete_armed);
        assert_eq!(
            app.state.select_settled_menu_action(3),
            Some(super::SettledMenuAction::Delete(target.clone()))
        );
        assert!(!app.state.sidebar_settled_menu_delete_armed);

        for (index, expected_workspace) in [
            (1, crate::app::home::HomeWorkspace::CurrentCheckout),
            (2, crate::app::home::HomeWorkspace::NewWorktree),
        ] {
            app.state.sidebar_settled_menu_target = Some(target.clone());
            let Some(super::SettledMenuAction::NewThread {
                directory,
                workspace,
            }) = app.state.select_settled_menu_action(index)
            else {
                panic!("menu item {index} should start a new thread");
            };
            assert_eq!(workspace, expected_workspace);
            app.state
                .open_home_composer_in_directory(directory.clone(), workspace);
            let home = app.state.home.as_mut().expect("home composer");
            home.prompt = "continue this work".into();
            let plan = home.dispatch_plan().expect("dispatch plan");
            assert_eq!(plan.directory, directory);
            assert_eq!(plan.workspace, expected_workspace);
        }
    }

    #[test]
    fn settled_menu_delete_is_immediate_when_confirmation_is_off() {
        let mut app = app_for_mouse_test();
        let target = settled_target(&mut app);
        app.state.confirm_close = false;
        app.state.sidebar_settled_menu_target = Some(target.clone());

        assert_eq!(
            app.state.select_settled_menu_action(3),
            Some(super::SettledMenuAction::Delete(target))
        );
    }

    #[test]
    fn settled_menu_delete_disarms_when_another_row_is_pressed() {
        let mut app = app_for_mouse_test();
        let target = settled_target(&mut app);
        app.state.sidebar_settled_menu_target = Some(target.clone());

        assert_eq!(app.state.select_settled_menu_action(3), None);
        assert_eq!(
            app.state.select_settled_menu_action(0),
            Some(super::SettledMenuAction::Resume(target.clone()))
        );
        assert!(!app.state.sidebar_settled_menu_delete_armed);

        app.state.sidebar_settled_menu_target = Some(target);
        assert_eq!(app.state.select_settled_menu_action(3), None);
    }

    #[test]
    fn settled_menu_resume_clears_settled_at() {
        let mut app = app_for_mouse_test();
        let target = settled_target(&mut app);
        app.state.set_server_mode(Mode::Settings);
        app.state.sidebar_settled_menu_target = Some(target.clone());

        app.apply_sidebar_settled_menu_action(0);

        assert!(!app.state.pane_is_settled(0, target.pane_id));
        assert_eq!(app.state.active, Some(0));
        assert_eq!(
            app.state.workspaces[0].focused_pane_id(),
            Some(target.pane_id)
        );
        assert_eq!(app.state.server_mode(), Mode::Settings);
    }

    #[test]
    fn clicking_unresumable_settled_row_focuses_its_live_pane_without_unsettling() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            Workspace::test_new("settled-live"),
            Workspace::test_new("currently-focused"),
        ];
        app.state.ensure_test_terminals();
        app.state.active = Some(1);
        app.state.selected = 1;
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        assert!(app.state.settle_pane_at(0, pane_id, 1_725_000_000));
        app.state.collapsed_sidebar_groups.clear();
        let area = Rect::new(0, 0, 120, 40);
        crate::ui::compute_view(&mut app.state, area);
        let row = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect)
            .into_iter()
            .find(|card| card.ws_idx == 0 && card.pane_id == pane_id)
            .expect("settled row")
            .rect;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            row.x + 1,
            row.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            row.x + 1,
            row.y,
        ));

        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(pane_id));
        assert!(app.state.pane_is_settled(0, pane_id));
        assert!(app.state.sidebar_settled_menu_target.is_none());
    }

    #[test]
    fn keyboard_enter_still_opens_the_settled_menu() {
        let mut app = app_for_mouse_test();
        let target = settled_target(&mut app);
        app.state.sidebar_selected_settled = Some(target.clone());

        assert!(
            app.handle_sidebar_settled_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty(),))
        );

        assert_eq!(app.state.sidebar_settled_menu_target, Some(target.clone()));
        let ws_idx = app
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == target.workspace_id)
            .expect("settled target workspace");
        assert!(app.state.pane_is_settled(ws_idx, target.pane_id));
    }

    #[test]
    fn settled_menu_opens_below_its_sidebar_row() {
        let mut app = app_for_mouse_test();
        let target = settled_target(&mut app);
        app.state.collapsed_sidebar_groups.clear();
        let area = Rect::new(0, 0, 120, 40);
        crate::ui::compute_view(&mut app.state, area);
        app.state.sidebar_settled_menu_target = Some(target.clone());
        let anchor = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect)
            .into_iter()
            .find(|card| card.pane_id == target.pane_id)
            .expect("settled row")
            .rect;
        let layout =
            crate::ui::sidebar_settled_menu_layout(&app.state, area).expect("settled dropdown");
        assert_eq!(layout.rect.y, anchor.bottom());
    }

    #[test]
    fn clicking_launcher_opens_global_menu() {
        let mut app = app_for_mouse_test();
        let rect = app.state.global_launcher_rect();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            rect.x + rect.width.saturating_sub(1),
            rect.y,
        ));

        assert_eq!(app.state.server_mode(), Mode::GlobalMenu);
    }

    #[test]
    fn clicking_sidebar_mode_header_opens_and_selects_dropdown() {
        let mut app = app_for_mouse_test();
        let anchor = app.state.sidebar_group_mode_anchor_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            anchor.x,
            anchor.y,
        ));
        assert!(app.state.sidebar_group_menu_open);

        let menu = crate::ui::sidebar_group_menu_layout(&app.state, Rect::new(0, 0, 106, 20))
            .expect("mode menu");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.list_rect.x,
            menu.list_rect.y + 2,
        ));
        assert_eq!(
            app.state.sidebar_group_mode,
            crate::app::state::SidebarGroupMode::LinearTeam
        );
        assert!(!app.state.sidebar_group_menu_open);
    }

    #[test]
    fn clicking_the_filter_chip_opens_a_downward_dropdown() {
        let mut app = app_for_mouse_test();
        app.state
            .set_sidebar_group_mode(crate::app::state::SidebarGroupMode::LinearTeam);
        let area = Rect::new(0, 0, 106, 40);
        crate::ui::compute_view(&mut app.state, area);
        let anchor = app.state.sidebar_filter_anchor_rect();
        assert!(anchor.width > 0, "the filter chip needs a hit area");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            anchor.x,
            anchor.y,
        ));
        assert!(app.state.sidebar_filter_menu_open);
        assert!(!app.state.sidebar_group_menu_open);

        let layout =
            crate::ui::sidebar_filter_menu_layout(&app.state, area).expect("filter dropdown");
        assert_eq!(layout.rect.y, anchor.bottom(), "dropdowns open downward");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            layout.list_rect.x,
            layout.list_rect.y,
        ));
        assert!(!app.state.sidebar_filter_menu_open);
    }

    #[test]
    fn hovering_global_menu_updates_highlight() {
        let mut app = app_for_mouse_test();
        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(MouseEventKind::Moved, menu.x + 2, menu.y + 2));

        assert_eq!(app.state.global_menu.highlighted, 1);
    }

    #[test]
    fn clicking_keybinds_menu_item_opens_help() {
        let mut app = app_for_mouse_test();
        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 2,
        ));

        assert_eq!(app.state.server_mode(), Mode::KeybindHelp);
    }

    #[test]
    fn clicking_settings_menu_item_opens_settings() {
        let mut app = app_for_mouse_test();
        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 1,
        ));

        assert_eq!(app.state.server_mode(), Mode::Settings);
    }

    #[test]
    fn clicking_reload_config_menu_item_requests_reload() {
        let mut app = app_for_mouse_test();
        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 3,
        ));

        assert!(app.state.request_reload_config);
        assert_eq!(app.state.server_mode(), Mode::Navigate);
    }

    #[test]
    fn update_pending_menu_surfaces_update_ready_entry() {
        let mut app = app_for_mouse_test();
        app.state.update_available = Some("0.3.2".into());
        app.state.latest_release_notes_available = true;

        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        assert_eq!(
            app.state.global_menu_labels(),
            vec![
                "settings",
                "keybinds",
                "reload config",
                "update ready",
                "detach"
            ]
        );
        assert!(!app.state.should_quit);
    }

    #[test]
    fn persistence_mode_menu_surfaces_detach_action() {
        let mut app = app_for_mouse_test();
        app.state.detach_exits = false;

        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        assert_eq!(
            app.state.global_menu_labels(),
            vec!["settings", "keybinds", "reload config", "detach"]
        );

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 4,
        ));

        assert!(app.state.detach_requested);
        assert!(!app.state.should_quit);
        assert_ne!(app.state.server_mode(), Mode::GlobalMenu);
    }

    #[test]
    fn whats_new_remains_in_menu_for_latest_installed_release_notes() {
        let mut app = app_for_mouse_test();
        app.state.latest_release_notes_available = true;

        assert_eq!(
            app.state.global_menu_labels(),
            vec![
                "settings",
                "keybinds",
                "reload config",
                "what's new",
                "detach"
            ]
        );
    }

    #[test]
    fn ac4_clicking_tab_row_preserves_that_tabs_focused_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("main".into());
        let first_pane = ws.tabs[0].root_pane;
        let first_tab = ws.test_add_tab(Some("logs"));
        ws.active_tab = first_tab;
        let second_pane = ws.test_split(Direction::Horizontal);
        assert_eq!(ws.tabs[first_tab].layout.focused(), second_pane);
        ws.active_tab = 0;
        app.state.workspaces = vec![ws];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        let second_terminal_id = app.state.workspaces[0].tabs[first_tab].panes[&second_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.reconcile_sidebar_presentation();
        let target = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect)
            .iter()
            .find(|card| card.tab_idx == first_tab)
            .unwrap()
            .rect;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            target.x + 1,
            target.y,
        ));

        assert_eq!(app.state.workspaces[0].active_tab, 1);
        assert_eq!(
            app.state.workspaces[0].tabs[1].layout.focused(),
            second_pane
        );
        assert_eq!(app.state.server_mode(), Mode::Terminal);
        let snapshot = capture_snapshot(&app.state);
        assert_eq!(snapshot.workspaces[0].active_tab, first_tab);
        assert_eq!(
            snapshot.workspaces[0].tabs[first_tab].focused,
            Some(second_pane.raw())
        );
    }

    #[test]
    fn legacy_agent_row_layout_does_not_change_single_line_tab_targets() {
        let mut app = app_for_mouse_test();
        let first = Workspace::test_new("one");
        let second = Workspace::test_new("two");
        app.state.workspaces = vec![first, second];
        app.state.ensure_test_terminals();
        app.state.sidebar_agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];
        app.state.sidebar_agents.rows_by_agent.insert(
            "claude".into(),
            vec![
                vec![crate::config::AgentSidebarToken::Agent],
                vec![crate::config::AgentSidebarToken::Workspace],
            ],
        );
        app.state.sidebar_agents.row_gap = 1;
        app.state.agent_panel_sort = AgentPanelSort::Priority;
        app.state.reconcile_sidebar_presentation();

        let cards = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect);
        assert_eq!(cards.len(), 2);
        assert_eq!(app.state.tab_target_at(cards[0].rect.y), Some((0, 0)));
        assert_eq!(app.state.tab_target_at(cards[1].rect.y), Some((1, 0)));
        assert!(
            crate::ui::compute_agent_card_areas(&app.state, app.state.view.sidebar_rect).is_empty()
        );
    }

    #[test]
    fn sidebar_hit_testing_recomputes_client_local_scrolled_row_geometry() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            Workspace::test_new("one"),
            Workspace::test_new("two"),
            Workspace::test_new("three"),
        ];
        app.state.sidebar_spaces.rows = vec![
            vec![crate::config::SpaceSidebarToken::Workspace],
            vec![crate::config::SpaceSidebarToken::StateText],
        ];
        app.state.sidebar_spaces.row_gap = 1;
        app.state.view.sidebar_rect = Rect::new(0, 0, 26, 8);
        app.state.ensure_test_terminals();
        app.state.reconcile_sidebar_presentation();
        app.state.workspace_scroll =
            crate::ui::sidebar_row_index_for_workspace(&app.state, 1).expect("second Space row");

        let (cards, _) =
            crate::ui::compute_sidebar_row_areas(&app.state, app.state.view.sidebar_rect);
        assert_eq!(cards[0].ws_idx, 1);
        let target_row = cards[0].rect.y;
        app.state.view.workspace_card_areas = vec![crate::app::state::WorkspaceCardArea {
            ws_idx: 0,
            rect: cards[0].rect,
            indented: false,
            repo_header: false,
            settled_pane_id: None,
        }];

        assert_eq!(app.state.workspace_at_row(target_row), Some(1));
        assert_eq!(
            app.state.workspace_drop_target_at_row(target_row),
            Some(crate::app::state::WorkspaceDropTarget::Before(1))
        );
    }

    #[test]
    fn tab_hit_testing_ignores_legacy_agent_geometry_cached_for_another_client() {
        let mut app = app_for_mouse_test();
        let first = Workspace::test_new("one");
        let first_pane = first.tabs[0].root_pane;
        let second = Workspace::test_new("two");
        let second_pane = second.tabs[0].root_pane;
        app.state.workspaces = vec![first, second];
        app.state.ensure_test_terminals();
        for (ws_idx, pane_id) in [(0, first_pane), (1, second_pane)] {
            let terminal_id = app.state.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .detected_agent = Some(Agent::Claude);
        }
        app.state.agent_panel_sort = AgentPanelSort::Priority;
        app.state.sidebar_presentation.expanded_workspace_ids = app
            .state
            .workspaces
            .iter()
            .map(|workspace| workspace.id.clone())
            .collect();
        let cards = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect);
        let first_row = cards[0].rect;
        app.state.view.agent_card_areas = vec![crate::app::state::AgentCardArea {
            ws_idx: 1,
            tab_idx: 0,
            pane_id: second_pane,
            rect: first_row,
            row_idx: 0,
        }];

        assert_eq!(app.state.tab_target_at(first_row.y), Some((0, 0)));
    }

    #[test]
    fn tab_hit_testing_uses_current_hierarchy_after_legacy_filter_change() {
        let mut app = app_for_mouse_test();
        let first = Workspace::test_new("one");
        let first_pane = first.tabs[0].root_pane;
        let second = Workspace::test_new("two");
        let second_pane = second.tabs[0].root_pane;
        app.state.workspaces = vec![first, second];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        for (ws_idx, pane_id) in [(0, first_pane), (1, second_pane)] {
            let terminal_id = app.state.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .detected_agent = Some(Agent::Claude);
        }
        app.state.agent_view_override = Some(crate::api::schema::AgentViewSetParams {
            source: "example.views".to_string(),
            label: None,
            filter: Some(crate::api::schema::AgentViewFilter::Eq {
                field: crate::api::schema::AgentViewField::Builtin(
                    crate::api::schema::AgentViewBuiltinField::WorkspaceId,
                ),
                value: crate::api::schema::AgentViewValue::Context {
                    context: crate::api::schema::AgentViewContext::CurrentWorkspaceId,
                },
            }),
            sort: Vec::new(),
        });
        app.state.sidebar_presentation.expanded_workspace_ids = app
            .state
            .workspaces
            .iter()
            .map(|workspace| workspace.id.clone())
            .collect();
        app.state.workspace_scroll = 10;
        let cards = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect);
        let card = cards
            .first()
            .expect("expanded hierarchy should expose a visible tab card");

        assert_eq!(
            app.state.tab_target_at(card.rect.y),
            Some((card.ws_idx, card.tab_idx))
        );
    }

    #[test]
    fn clicking_all_workspaces_tab_row_switches_to_correct_workspace() {
        let mut app = app_for_mouse_test();
        let first = Workspace::test_new("one");
        let first_pane = first.tabs[0].root_pane;

        let second = Workspace::test_new("two");
        let second_pane = second.tabs[0].root_pane;

        app.state.workspaces = vec![first, second];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        let second_terminal_id = app.state.workspaces[1].tabs[0].panes[&second_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.reconcile_sidebar_presentation();
        assert!(app.state.workspace_agents_expanded(1));
        let target = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect)
            .iter()
            .find(|card| card.ws_idx == 1 && card.tab_idx == 0)
            .unwrap()
            .rect;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            target.x + 1,
            target.y,
        ));

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.selected, 1);
        assert_eq!(app.state.workspaces[1].active_tab, 0);
        assert_eq!(
            app.state.workspaces[1].tabs[0].layout.focused(),
            second_pane
        );
    }

    #[test]
    fn scrolling_flat_agent_projection_with_wheel_updates_shared_scroll() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let first_pane = ws.tabs[0].root_pane;

        let mut tabs = Vec::new();
        for (tab_name, agent) in [
            ("logs", Agent::Claude),
            ("review", Agent::Codex),
            ("ops", Agent::Gemini),
            ("deploy", Agent::Claude),
        ] {
            let tab_idx = ws.test_add_tab(Some(tab_name));
            let pane_id = ws.tabs[tab_idx].root_pane;
            tabs.push((tab_idx, pane_id, agent));
        }

        app.state.workspaces = vec![ws];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        for (tab_idx, pane_id, agent) in tabs {
            let terminal_id = app.state.workspaces[0].tabs[tab_idx].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .detected_agent = Some(agent);
        }
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.agent_panel_sort = AgentPanelSort::Priority;
        app.state.view.sidebar_rect = Rect::new(0, 0, 26, 5);
        app.state.view.terminal_area = Rect::new(26, 0, 80, 5);

        let detail_area = app.state.workspace_list_rect();
        assert!(crate::ui::should_show_scrollbar(
            crate::ui::workspace_list_scroll_metrics(&app.state, detail_area)
        ));

        app.handle_mouse(mouse(
            MouseEventKind::ScrollDown,
            detail_area.x + 1,
            detail_area.y + 4,
        ));

        assert_eq!(app.state.workspace_scroll, 1);
        assert_eq!(app.state.selected, 0);
    }

    #[test]
    fn clicking_scrolled_tab_row_switches_to_correct_tab_and_preserves_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let first_pane = ws.tabs[0].root_pane;
        let second_tab = ws.test_add_tab(Some("logs"));
        let second_pane = ws.tabs[second_tab].root_pane;
        let mut extra_tabs = Vec::new();
        for (tab_name, agent) in [("review", Agent::Codex), ("ops", Agent::Gemini)] {
            let tab_idx = ws.test_add_tab(Some(tab_name));
            let pane_id = ws.tabs[tab_idx].root_pane;
            extra_tabs.push((tab_idx, pane_id, agent));
        }

        app.state.workspaces = vec![ws];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        let second_terminal_id = app.state.workspaces[0].tabs[second_tab].panes[&second_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        for (tab_idx, pane_id, agent) in extra_tabs {
            let terminal_id = app.state.workspaces[0].tabs[tab_idx].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .detected_agent = Some(agent);
        }
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.sidebar_agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];
        app.state.sidebar_agents.rows_by_agent.insert(
            "claude".into(),
            vec![
                vec![crate::config::AgentSidebarToken::Agent],
                vec![crate::config::AgentSidebarToken::Workspace],
            ],
        );
        app.state.reconcile_sidebar_presentation();
        app.state.workspace_scroll = 1;
        let target = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect)
            .iter()
            .find(|card| card.tab_idx == second_tab)
            .unwrap()
            .rect;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            target.x + 1,
            target.y,
        ));

        assert_eq!(app.state.workspaces[0].active_tab, second_tab);
        assert_eq!(
            app.state.workspaces[0].tabs[second_tab].layout.focused(),
            second_pane
        );
        assert_eq!(app.state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn clicking_collapsed_agent_row_switches_to_correct_tab_and_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let first_pane = ws.tabs[0].root_pane;
        let second_tab = ws.test_add_tab(Some("logs"));
        let second_pane = ws.tabs[second_tab].root_pane;
        app.state.workspaces = vec![ws];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        let second_terminal_id = app.state.workspaces[0].tabs[second_tab].panes[&second_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.sidebar_collapsed = true;
        app.state.view.sidebar_rect = Rect::new(0, 0, 4, 20);
        app.state.view.terminal_area = Rect::new(4, 0, 80, 20);
        app.state.reconcile_sidebar_presentation();

        let row = crate::ui::sidebar_rows(&app.state)
            .iter()
            .position(|entry| {
                matches!(
                    entry,
                    crate::ui::SidebarRow::Tab { entry, .. }
                        if entry.local_target().is_some_and(|target| target.tab_idx == second_tab)
                )
            })
            .unwrap() as u16;
        let (content, _, _) = crate::ui::collapsed_sidebar_sections(app.state.view.sidebar_rect);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            content.x,
            content.y + row,
        ));

        assert_eq!(app.state.workspaces[0].active_tab, 1);
        assert_eq!(
            app.state.workspaces[0].tabs[1].layout.focused(),
            second_pane
        );
        assert_eq!(app.state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn clicking_collapsed_snoozed_row_focuses_its_exact_pane() {
        let mut app = app_for_mouse_test();
        let mut workspace = Workspace::test_new("split");
        let active_pane = workspace.tabs[0].root_pane;
        let snoozed_pane = workspace.test_split(Direction::Horizontal);
        workspace.tabs[0].layout.focus_pane(active_pane);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        for pane_id in [active_pane, snoozed_pane] {
            let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .expect("terminal")
                .detected_agent = Some(Agent::Pi);
        }
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.sidebar_collapsed = true;
        app.state.view.sidebar_rect = Rect::new(0, 0, 4, 20);
        app.state.view.terminal_area = Rect::new(4, 0, 80, 20);
        let deadline = crate::app::settled::unix_seconds(std::time::SystemTime::now()) + 900;
        assert!(app.state.snooze_pane_at(0, snoozed_pane, deadline));
        app.state.collapsed_sidebar_groups.clear();
        app.state.refresh_local_agent_panel_identities();
        app.state.reconcile_sidebar_presentation();

        let row = crate::ui::sidebar_rows(&app.state)
            .iter()
            .position(|entry| {
                matches!(
                    entry,
                    crate::ui::SidebarRow::Agent { entry, .. }
                        | crate::ui::SidebarRow::Tab { entry, .. }
                        if entry.local_target().is_some_and(|target| target.pane_id == snoozed_pane)
                )
            })
            .expect("snoozed pane row") as u16;
        let (content, _, _) = crate::ui::collapsed_sidebar_sections(app.state.view.sidebar_rect);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            content.x,
            content.y + row,
        ));

        assert_eq!(
            app.state.workspaces[0].tabs[0].layout.focused(),
            snoozed_pane
        );
    }

    #[test]
    fn clicking_collapsed_priority_projection_keeps_workspace_order() {
        let mut app = app_for_mouse_test();
        let first = Workspace::test_new("one");
        let first_pane = first.tabs[0].root_pane;
        let second = Workspace::test_new("two");
        let second_pane = second.tabs[0].root_pane;

        app.state.workspaces = vec![first, second];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        app.state.sidebar_collapsed = true;
        app.state.agent_panel_sort = AgentPanelSort::Priority;
        app.state.view.sidebar_rect = Rect::new(0, 0, 4, 20);
        app.state.view.terminal_area = Rect::new(4, 0, 80, 20);

        let set_state = |app: &mut crate::app::App, ws_idx: usize, pane_id, state| {
            let terminal_id = app.state.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(Agent::Claude);
            terminal.set_raw_agent_state_for_test(state);
        };
        set_state(&mut app, 0, first_pane, AgentState::Working);
        set_state(&mut app, 1, second_pane, AgentState::Blocked);

        let (detail_area, _, _) =
            crate::ui::collapsed_sidebar_sections(app.state.view.sidebar_rect);
        let row = crate::ui::sidebar_rows(&app.state)
            .iter()
            .position(|row| matches!(row, crate::ui::SidebarRow::Workspace { ws_idx: 1, .. }))
            .unwrap() as u16;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            detail_area.x,
            detail_area.y + row,
        ));

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.selected, 1);
        assert_eq!(
            app.state.workspaces[1].tabs[0].layout.focused(),
            second_pane
        );
    }

    #[test]
    fn clicking_collapsed_sidebar_toggle_expands_sidebar() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_collapsed = true;
        app.state.view.sidebar_rect = Rect::new(0, 0, 4, 20);
        app.state.view.terminal_area = Rect::new(4, 0, 80, 20);

        let toggle = crate::ui::collapsed_sidebar_toggle_rect(app.state.view.sidebar_rect);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            toggle.x,
            toggle.y,
        ));

        assert!(!app.state.sidebar_collapsed);
    }

    #[test]
    fn hidden_collapsed_sidebar_has_no_mouse_expand_hotspot() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_collapsed = true;
        app.state.sidebar_collapsed_mode = SidebarCollapsedModeConfig::Hidden;
        app.state.view.sidebar_rect = Rect::new(0, 0, 0, 20);
        app.state.view.terminal_area = Rect::new(0, 0, 80, 20);

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 19));

        assert!(app.state.sidebar_collapsed);
    }

    #[test]
    fn clicking_expanded_sidebar_toggle_collapses_sidebar() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_collapsed = false;
        app.state.view.sidebar_rect = Rect::new(0, 0, 26, 20);
        app.state.view.terminal_area = Rect::new(26, 0, 80, 20);

        let toggle = crate::ui::expanded_sidebar_toggle_rect(app.state.view.sidebar_rect);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            toggle.x,
            toggle.y,
        ));

        assert!(app.state.sidebar_collapsed);
        assert!(app.state.drag.is_none());
    }

    #[test]
    fn clicking_workspace_switches_on_mouse_up() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("a"), Workspace::test_new("b")];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let target_row = app.state.view.workspace_card_areas[1].rect.y;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            2,
            target_row,
        ));
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.workspace_presses.len(), 1);

        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 2, target_row));
        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.selected, 1);
        assert!(app.state.workspace_presses.is_empty());
        let snapshot = capture_snapshot(&app.state);
        assert_eq!(snapshot.active, Some(1));
        assert_eq!(snapshot.selected, 1);
    }

    #[test]
    fn clicking_the_spaces_header_folds_and_unfolds_the_tree() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("main"), Workspace::test_new("issue")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(Agent::Claude);
        terminal.set_raw_agent_state_for_test(crate::detect::AgentState::Blocked);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 40));

        let header = crate::ui::compute_sidebar_section_header_areas(
            &app.state,
            app.state.view.sidebar_rect,
        )
        .into_iter()
        .find(|header| header.title == crate::ui::SPACES_SECTION_TITLE)
        .expect("the Spaces header is always present");
        let rows_open = crate::ui::sidebar_rows(&app.state).len();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            header.rect.x + 1,
            header.rect.y,
        ));
        assert!(app.state.collapsed_sidebar_groups.contains("repo:Spaces"));
        assert!(crate::ui::sidebar_rows(&app.state).len() < rows_open);

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 40));
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            header.rect.x + 1,
            header.rect.y,
        ));
        assert!(!app.state.collapsed_sidebar_groups.contains("repo:Spaces"));
        assert_eq!(crate::ui::sidebar_rows(&app.state).len(), rows_open);
    }

    #[test]
    fn clicking_a_blocked_worklist_row_focuses_its_own_pane() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("main"), Workspace::test_new("blocked")];
        app.state.ensure_test_terminals();
        let working_pane = app.state.workspaces[0].tabs[0].root_pane;
        let blocked_pane = app.state.workspaces[1].tabs[0].root_pane;
        for (ws_idx, pane_id, state) in [
            (0, working_pane, AgentState::Working),
            (1, blocked_pane, AgentState::Blocked),
        ] {
            let terminal_id = app.state.workspaces[ws_idx]
                .terminal_id(pane_id)
                .unwrap()
                .clone();
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(Agent::Claude);
            terminal.set_raw_agent_state_for_test(state);
        }
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        let screen = Rect::new(0, 0, 106, 40);
        crate::ui::compute_view(&mut app.state, screen);
        let area = app.state.view.sidebar_rect;
        let card = crate::ui::compute_tab_card_areas(&app.state, area)
            .into_iter()
            .find(|card| card.pane_id == blocked_pane)
            .expect("blocked worklist row should be clickable");

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            card.rect.x + 1,
            card.rect.y,
        ));

        assert_eq!(app.state.active, Some(1));
        assert_eq!(
            app.state.workspaces[1].tabs[0].layout.focused(),
            blocked_pane
        );
        assert_eq!(app.state.server_mode(), Mode::Terminal);
    }

    #[test]
    fn clicking_worktree_parent_row_focuses_workspace_without_toggling() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("main"), Workspace::test_new("issue")];
        for (idx, checkout_path) in ["/repo/herdr", "/repo/herdr-issue"].into_iter().enumerate() {
            app.state.workspaces[idx].worktree_space =
                Some(crate::workspace::WorktreeSpaceMembership {
                    key: "repo-key".into(),
                    label: "herdr".into(),
                    repo_root: "/repo/herdr".into(),
                    checkout_path: checkout_path.into(),
                    is_linked_worktree: idx > 0,
                });
        }
        app.state.active = None;
        app.state.set_server_mode(Mode::Terminal);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let parent = app.state.view.workspace_card_areas[0].rect;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            parent.x + 2,
            parent.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            parent.x + 2,
            parent.y,
        ));

        assert_eq!(app.state.active, Some(0));
        assert!(!app.state.collapsed_space_keys.contains("repo-key"));
    }

    #[test]
    fn clicking_space_chevron_toggles_direct_window_rows() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("main"), Workspace::test_new("issue")];
        for (idx, checkout_path) in ["/repo/herdr", "/repo/herdr-issue"].into_iter().enumerate() {
            app.state.workspaces[idx].worktree_space =
                Some(crate::workspace::WorktreeSpaceMembership {
                    key: "repo-key".into(),
                    label: "herdr".into(),
                    repo_root: "/repo/herdr".into(),
                    checkout_path: checkout_path.into(),
                    is_linked_worktree: idx > 0,
                });
        }
        app.state.active = None;
        app.state.set_server_mode(Mode::Terminal);
        app.state.ensure_test_terminals();
        app.state.reconcile_sidebar_presentation();
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let parent = app.state.view.workspace_card_areas[0];
        let chevron = crate::ui::workspace_agent_chevron_rect(&app.state, &parent, true);
        assert!(app.state.workspace_agents_expanded(0));

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            chevron.x,
            chevron.y,
        ));

        assert_eq!(app.state.active, None);
        assert!(app.state.workspace_presses.is_empty());
        assert!(!app.state.workspace_agents_expanded(0));
        assert!(!app.state.collapsed_space_keys.contains("repo-key"));

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            chevron.x,
            chevron.y,
        ));

        assert!(app.state.workspace_agents_expanded(0));
    }

    #[test]
    fn wheel_workspace_selection_follows_grouped_visual_order_without_scrollbar() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            Workspace::test_new("main"),
            Workspace::test_new("normal"),
            Workspace::test_new("issue"),
        ];
        for (idx, checkout_path) in [(0, "/repo/herdr"), (2, "/repo/herdr-issue")] {
            app.state.workspaces[idx].worktree_space =
                Some(crate::workspace::WorktreeSpaceMembership {
                    key: "repo-key".into(),
                    label: "herdr".into(),
                    repo_root: "/repo/herdr".into(),
                    checkout_path: checkout_path.into(),
                    is_linked_worktree: idx != 0,
                });
        }
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Navigate);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 30));
        let list = app.state.workspace_list_rect();
        assert!(!crate::ui::should_show_scrollbar(
            crate::ui::workspace_list_scroll_metrics(&app.state, list)
        ));

        app.handle_mouse(mouse(MouseEventKind::ScrollDown, list.x + 1, list.y + 1));

        assert_eq!(app.state.selected, 2);
    }

    #[test]
    fn dragging_workspace_reorders_without_changing_identity() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            Workspace::test_new("a"),
            Workspace::test_new("b"),
            Workspace::test_new("c"),
        ];
        app.state.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.state.sidebar_spaces.row_gap = 0;
        let active_id = app.state.workspaces[1].id.clone();
        let selected_id = app.state.workspaces[2].id.clone();
        app.state.active = Some(1);
        app.state.selected = 2;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let packed_boundary_row = app.state.view.workspace_card_areas[1].rect.y;
        assert_eq!(
            app.state.workspace_drop_target_at_row(packed_boundary_row),
            Some(crate::app::state::WorkspaceDropTarget::Before(2))
        );

        let source_row = app.state.view.workspace_card_areas[1].rect.y;
        let target_row = crate::ui::workspace_drop_indicator_row(
            &app.state,
            &app.state.view.workspace_card_areas,
            app.state.workspace_list_rect(),
            crate::app::state::WorkspaceDropTarget::Before(0),
        )
        .unwrap();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            2,
            source_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            2,
            target_row,
        ));
        assert!(matches!(
            app.state.drag.as_ref().map(|drag| &drag.target),
            Some(DragTarget::WorkspaceReorder {
                source_ws_idx: 1,
                drop_target: Some(crate::app::state::WorkspaceDropTarget::Before(0)),
                ..
            })
        ));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 2, target_row));

        let names: Vec<_> = app
            .state
            .workspaces
            .iter()
            .map(|ws| ws.display_name())
            .collect();
        assert_eq!(names, vec!["b", "a", "c"]);
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.selected, 2);
        assert_eq!(app.state.workspaces[0].id, active_id);
        assert_eq!(app.state.workspaces[2].id, selected_id);
        let events = app.event_hub.events_after(0);
        assert!(events.iter().any(|(_, event)| matches!(
            event.data,
            crate::api::schema::EventData::WorkspaceMoved { .. }
        )));
        assert!(!events.iter().any(|(_, event)| matches!(
            event.data,
            crate::api::schema::EventData::WorkspaceReordered { .. }
        )));
        let snapshot = capture_snapshot(&app.state);
        let captured_names: Vec<_> = snapshot
            .workspaces
            .iter()
            .map(|ws| ws.custom_name.clone().unwrap())
            .collect();
        assert_eq!(captured_names, vec!["b", "a", "c"]);
    }

    #[test]
    fn clicking_tab_scroll_button_reveals_hidden_tabs_without_renaming() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        ws.test_add_tab(Some("logs"));
        ws.test_add_tab(Some("review"));
        ws.test_add_tab(Some("ops"));
        ws.test_add_tab(Some("notes"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 66, 20));

        let right = app.state.view.tab_scroll_right_hit_area;
        assert!(right.width > 0);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            right.x + 1,
            right.y,
        ));

        assert_eq!(app.state.tab_scroll, 1);
        assert!(!app.state.tab_scroll_follow_active);
        assert_eq!(app.state.workspaces[0].active_tab, 0);
        assert_eq!(app.state.view.tab_hit_areas[0].width, 0);
        assert!(app.state.workspaces[0].tabs[0].custom_name.is_none());
        assert_eq!(
            app.state.workspaces[0].tabs[1].custom_name.as_deref(),
            Some("logs")
        );
    }

    #[test]
    fn clicking_last_visible_tab_at_right_edge_does_not_overscroll() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        for name in [
            "one", "two", "three", "four", "five", "six", "seven", "eight",
        ] {
            ws.test_add_tab(Some(name));
        }
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.tab_scroll = usize::MAX;
        app.state.tab_scroll_follow_active = false;
        // 91 columns leaves the same tab strip as 65 did before the tab row
        // gained the action, git, and pane-toggle buttons at its right end.
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 91, 20));

        let last_idx = app.state.workspaces[0].tabs.len() - 1;
        let target = app.state.view.tab_hit_areas[last_idx];
        let clamped_scroll = app.state.tab_scroll;
        assert!(target.width > 0, "last tab should already be visible");
        let target_col = target.x + target.width - 1;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            target_col,
            target.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            target_col,
            target.y,
        ));

        assert_eq!(app.state.workspaces[0].active_tab, last_idx);
        assert_eq!(app.state.tab_scroll, clamped_scroll);
        assert!(app.state.view.tab_hit_areas[last_idx].width > 0);
    }

    #[test]
    fn dragging_tab_reorders_auto_and_custom_names_without_materializing_numbers() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        ws.test_add_tab(Some("foo"));
        ws.test_add_tab(None);
        let moved_root = ws.tabs[0].root_pane;
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let source = app.state.view.tab_hit_areas[0];
        let last = app.state.view.tab_hit_areas[2];
        let drop_col = last.x + last.width;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            source.x + 1,
            source.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            drop_col,
            source.y,
        ));
        assert!(matches!(
            app.state.drag.as_ref().map(|drag| &drag.target),
            Some(DragTarget::TabReorder {
                ws_idx: 0,
                source_tab_idx: 0,
                insert_idx: Some(3),
                ..
            })
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            drop_col,
            source.y,
        ));

        let labels: Vec<_> = app.state.workspaces[0]
            .tabs
            .iter()
            .enumerate()
            .map(|(tab_idx, _)| {
                app.state.workspaces[0]
                    .tab_display_name_from(&app.state.terminals, tab_idx)
                    .unwrap_or_else(|| (tab_idx + 1).to_string())
            })
            .collect();
        assert_eq!(labels, vec!["foo", "2", "3"]);
        assert_eq!(
            app.state.workspaces[0].tabs[0].custom_name.as_deref(),
            Some("foo")
        );
        assert!(app.state.workspaces[0].tabs[1].custom_name.is_none());
        assert!(app.state.workspaces[0].tabs[2].custom_name.is_none());
        assert_eq!(app.state.workspaces[0].tabs[0].number, 2);
        assert_eq!(app.state.workspaces[0].tabs[1].number, 3);
        assert_eq!(app.state.workspaces[0].tabs[2].number, 1);
        assert_eq!(app.state.workspaces[0].tabs[2].root_pane, moved_root);
        assert_eq!(app.state.workspaces[0].active_tab, 2);
    }

    fn temp_git_repo(branch: &str) -> std::path::PathBuf {
        let repo = unique_temp_path("sidebar-drop-slot-repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(
            repo.join(".git/HEAD"),
            format!("ref: refs/heads/{branch}\n"),
        )
        .unwrap();
        repo
    }

    fn workspace_with_space(name: &str, key: &str) -> Workspace {
        let mut ws = Workspace::test_new(name);
        ws.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: key.into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: format!("/repo/{name}").into(),
            is_linked_worktree: name != "main",
        });
        ws
    }

    #[test]
    fn top_drop_slot_is_distinct_from_gap_below_first_workspace() {
        let mut app = app_for_mouse_test();
        let first_repo = temp_git_repo("main");
        let second_repo = temp_git_repo("main");

        let mut first = Workspace::test_new("a");
        let first_root = first.tabs[0].root_pane;
        first.identity_cwd = first_repo.clone();
        first.refresh_git_ahead_behind();

        let mut second = Workspace::test_new("b");
        let second_root = second.tabs[0].root_pane;
        second.identity_cwd = second_repo.clone();
        second.refresh_git_ahead_behind();

        app.state.workspaces = vec![first, second];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_root]
            .attached_terminal_id
            .clone();
        app.state.terminals.get_mut(&first_terminal_id).unwrap().cwd = first_repo.clone();
        let second_terminal_id = app.state.workspaces[1].tabs[0].panes[&second_root]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .cwd = second_repo.clone();
        app.state.sidebar_spaces.row_gap = 1;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        // Status bar occupies row 0; sidebar list starts at y=1.
        assert_eq!(app.state.workspace_drop_target_at_row(0), None);
        let slots = crate::ui::workspace_drop_slots(
            &app.state,
            &app.state.view.workspace_card_areas,
            app.state.workspace_list_rect(),
        );
        let first_slot = slots
            .iter()
            .find(|(target, _)| *target == crate::app::state::WorkspaceDropTarget::Before(0))
            .unwrap()
            .1;
        let second_slot = slots
            .iter()
            .find(|(target, _)| *target == crate::app::state::WorkspaceDropTarget::Before(1))
            .unwrap()
            .1;
        assert!(second_slot > first_slot);
        assert_eq!(
            app.state.workspace_drop_target_at_row(first_slot),
            Some(crate::app::state::WorkspaceDropTarget::Before(0))
        );
        assert_eq!(
            app.state.workspace_drop_target_at_row(second_slot),
            Some(crate::app::state::WorkspaceDropTarget::Before(1))
        );

        let _ = fs::remove_dir_all(first_repo);
        let _ = fs::remove_dir_all(second_repo);
    }

    #[test]
    fn bottom_drop_slot_stays_below_last_workspace_not_footer() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            Workspace::test_new("a"),
            Workspace::test_new("b"),
            Workspace::test_new("c"),
        ];
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 24));

        let cards = &app.state.view.workspace_card_areas;
        let bottom_slot = crate::ui::workspace_drop_indicator_row(
            &app.state,
            cards,
            app.state.workspace_list_rect(),
            crate::app::state::WorkspaceDropTarget::End,
        )
        .unwrap();

        let last = cards.last().unwrap().rect;
        assert_eq!(bottom_slot, last.y + last.height);
        assert!(bottom_slot < app.state.sidebar_footer_rect().y.saturating_sub(1));
    }

    #[test]
    fn grouped_sidebar_drop_slots_do_not_land_inside_compact_group() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            workspace_with_space("main", "repo-key"),
            Workspace::test_new("normal"),
            workspace_with_space("issue", "repo-key"),
        ];
        app.state.active = Some(1);
        app.state.selected = 1;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 40));

        let cards = &app.state.view.workspace_card_areas;
        let order = cards.iter().map(|card| card.ws_idx).collect::<Vec<_>>();
        assert_eq!(order, vec![0, 1]);
        let normal = cards.iter().find(|card| card.ws_idx == 1).unwrap();

        assert_eq!(
            app.state.workspace_drop_target_at_row(cards[0].rect.y),
            Some(crate::app::state::WorkspaceDropTarget::Before(0))
        );
        assert_eq!(
            crate::ui::workspace_drop_indicator_row(
                &app.state,
                cards,
                app.state.workspace_list_rect(),
                crate::app::state::WorkspaceDropTarget::End,
            ),
            Some(normal.rect.y + normal.rect.height)
        );
    }

    #[test]
    fn plain_drag_anchors_to_the_selected_parentless_linked_workspace() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            workspace_with_space("one", "repo-key"),
            workspace_with_space("two", "repo-key"),
            Workspace::test_new("normal"),
        ];
        let target_id = app.state.workspaces[1].id.clone();

        let params = app
            .state
            .workspace_move_block_params(2, crate::app::state::WorkspaceDropTarget::Before(1))
            .unwrap();

        assert_eq!(params.workspace_ids, [app.state.workspaces[2].id.clone()]);
        assert_eq!(
            params.before_workspace_id.as_deref(),
            Some(target_id.as_str())
        );
    }

    #[test]
    fn dragging_worktree_parent_reorders_the_complete_group() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            workspace_with_space("main", "repo-key"),
            Workspace::test_new("normal"),
            workspace_with_space("issue", "repo-key"),
        ];
        app.state.active = Some(2);
        app.state.selected = 1;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 40));

        let parent = app
            .state
            .view
            .workspace_card_areas
            .iter()
            .find(|card| card.ws_idx == 0)
            .unwrap()
            .rect;
        let target_row = crate::ui::workspace_drop_indicator_row(
            &app.state,
            &app.state.view.workspace_card_areas,
            app.state.workspace_list_rect(),
            crate::app::state::WorkspaceDropTarget::End,
        )
        .unwrap();
        let active_id = app.state.workspaces[2].id.clone();
        let selected_id = app.state.workspaces[1].id.clone();

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, parent.y));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            2,
            target_row,
        ));
        assert!(matches!(
            app.state.drag.as_ref().map(|drag| &drag.target),
            Some(DragTarget::WorkspaceReorder {
                source_ws_idx: 0,
                drop_target: Some(crate::app::state::WorkspaceDropTarget::End),
                ..
            })
        ));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 2, target_row));

        assert_eq!(
            app.state
                .workspaces
                .iter()
                .map(|workspace| workspace.display_name())
                .collect::<Vec<_>>(),
            ["normal", "main", "issue"]
        );
        assert_eq!(
            app.state.workspaces[app.state.active.unwrap()].id,
            active_id
        );
        assert_eq!(app.state.workspaces[app.state.selected].id, selected_id);
    }

    #[test]
    fn dragging_collapsed_worktree_parent_still_moves_hidden_children() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            workspace_with_space("issue", "repo-key"),
            Workspace::test_new("normal"),
            workspace_with_space("main", "repo-key"),
            workspace_with_space("review", "repo-key"),
        ];
        app.state.active = Some(0);
        app.state.selected = 1;
        app.state.collapsed_space_keys.insert("repo-key".into());
        let active_id = app.state.workspaces[0].id.clone();
        let selected_id = app.state.workspaces[1].id.clone();
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 40));
        assert_eq!(app.state.view.workspace_card_areas.len(), 2);

        let parent = app.state.view.workspace_card_areas[0].rect;
        let target_row = crate::ui::workspace_drop_indicator_row(
            &app.state,
            &app.state.view.workspace_card_areas,
            app.state.workspace_list_rect(),
            crate::app::state::WorkspaceDropTarget::End,
        )
        .unwrap();
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, parent.y));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            2,
            target_row,
        ));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 2, target_row));

        assert_eq!(
            app.state
                .workspaces
                .iter()
                .map(|workspace| workspace.display_name())
                .collect::<Vec<_>>(),
            ["normal", "main", "issue", "review"]
        );
        assert_eq!(
            app.state.workspaces[app.state.active.unwrap()].id,
            active_id
        );
        assert_eq!(app.state.workspaces[app.state.selected].id, selected_id);
    }

    #[test]
    fn linked_worktree_has_no_intermediate_draggable_space_row() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            workspace_with_space("main", "repo-key"),
            Workspace::test_new("normal"),
            workspace_with_space("issue", "repo-key"),
        ];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 40));

        let source = app
            .state
            .view
            .workspace_card_areas
            .iter()
            .find(|card| card.ws_idx == 2)
            .copied();
        assert!(source.is_none());

        let names = app
            .state
            .workspaces
            .iter()
            .map(|ws| ws.display_name())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["main", "normal", "issue"]);
    }

    #[test]
    fn dragging_sidebar_divider_sets_manual_width() {
        let mut app = app_for_mouse_test();

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 30, 5));

        assert_eq!(app.state.sidebar_width, 31);
        let snapshot = capture_snapshot(&app.state);
        assert_eq!(snapshot.sidebar_width, Some(31));
    }

    #[test]
    fn drag_hit_column_matches_rendered_sidebar_separator() {
        let app = app_for_mouse_test();
        let sidebar = app.state.view.sidebar_rect;
        let separator_col = crate::ui::sidebar_separator_col(sidebar).unwrap();
        let row = sidebar.y + 2;

        assert!(app.state.on_sidebar_divider(separator_col, row));
        assert!(!app
            .state
            .on_sidebar_divider(separator_col.saturating_sub(1), row));
    }

    #[test]
    fn dragging_sidebar_bottom_divider_still_sets_manual_width() {
        let mut app = app_for_mouse_test();
        let divider_col = app.state.view.sidebar_rect.x + app.state.view.sidebar_rect.width - 1;
        let bottom_row = app.state.view.sidebar_rect.y + app.state.view.sidebar_rect.height - 1;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            divider_col,
            bottom_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            divider_col + 5,
            bottom_row,
        ));

        assert_eq!(app.state.sidebar_width, 31);
    }

    #[test]
    fn dragging_past_max_clamps_to_configured_max() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_max_width = 30;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 50, 5));

        assert_eq!(app.state.sidebar_width, 30);
    }

    #[test]
    fn dragging_below_min_clamps_to_configured_min() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_min_width = 22;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 5, 5));

        assert_eq!(app.state.sidebar_width, 22);
    }

    #[test]
    fn removed_sidebar_section_divider_is_not_interactive() {
        let mut app = app_for_mouse_test();
        let original = app.state.sidebar_section_split;
        let old_divider_row =
            app.state.view.sidebar_rect.y + app.state.view.sidebar_rect.height / 2;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            app.state.view.sidebar_rect.x + 1,
            old_divider_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            app.state.view.sidebar_rect.x + 1,
            old_divider_row + 4,
        ));

        assert_eq!(app.state.sidebar_section_split, original);
        assert!(app.state.drag.is_none());
    }

    #[test]
    fn double_clicking_sidebar_divider_resets_default_width() {
        let mut app = app_for_mouse_test();
        app.state.default_sidebar_width = 26;
        app.state.sidebar_width = 30;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));

        assert_eq!(app.state.sidebar_width, 26);
        assert!(app.state.drag.is_none());
        let snapshot = capture_snapshot(&app.state);
        assert_eq!(snapshot.sidebar_width, Some(26));
    }

    fn aloop_key_fixture() -> crate::app::state::AppState {
        let mut app = crate::app::state::AppState::test_new();
        app.fleet_snapshot = crate::fleet::Snapshot {
            polled: true,
            aloop: Some(crate::aloop::ProducerSnapshot::read(
                "ub2".to_string(),
                crate::aloop::HostData {
                    findings: vec![std::sync::Arc::new(crate::aloop::Finding {
                        loop_name: "nightly".to_string(),
                        source: "sentry".to_string(),
                        stable_id: "abc-123".to_string(),
                        title: "worker crashed".to_string(),
                        url: None,
                        evidence: "stacktrace line".to_string(),
                        prompt: "fix the crash".to_string(),
                        created_at: "2026-09-18T09:50:00Z".to_string(),
                        created_at_unix_s: crate::fleet::parse_utc_timestamp(
                            "2026-09-18T09:50:00Z",
                        )
                        .expect("timestamp"),
                        status: crate::aloop::FindingStatus::Pending,
                    })],
                    loops: vec![crate::aloop::LoopRuns {
                        loop_name: "nightly".to_string(),
                        runs: vec![
                            std::sync::Arc::new(crate::aloop::RunRecord {
                                at: "2026-09-18T09:59:00Z".to_string(),
                                at_unix_s: crate::fleet::parse_utc_timestamp(
                                    "2026-09-18T09:59:00Z",
                                )
                                .expect("timestamp"),
                                duration_ms: 1_200,
                                exit: 0,
                                findings: 0,
                                stable_ids: Vec::new(),
                                log_excerpt: "clean log tail".to_string(),
                            }),
                            std::sync::Arc::new(crate::aloop::RunRecord {
                                at: "2026-09-18T09:58:00Z".to_string(),
                                at_unix_s: crate::fleet::parse_utc_timestamp(
                                    "2026-09-18T09:58:00Z",
                                )
                                .expect("timestamp"),
                                duration_ms: 2_400,
                                exit: 0,
                                findings: 2,
                                stable_ids: vec!["abc-123".to_string()],
                                log_excerpt: "hit log tail".to_string(),
                            }),
                        ],
                        skipped_lines: 0,
                    }],
                    ..Default::default()
                },
            )),
            ..Default::default()
        };
        app
    }

    #[test]
    fn ac5_enter_on_an_aloop_finding_opens_the_composer_without_a_spawn() {
        let mut app = aloop_key_fixture();
        app.machines = crate::app::machines::resolve(&crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "ub2".to_string(),
                target: "ub2".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        });
        app.sidebar_selected_work_group = Some("aloop:finding:nightly:abc-123".into());

        let action =
            app.handle_sidebar_work_group_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert!(matches!(
            action,
            crate::app::SidebarWorkGroupKeyAction::Consumed
        ));
        let home = app.home.as_ref().expect("composer opened");
        assert_eq!(home.prompt, "fix the crash\n\nEvidence:\nstacktrace line");
        // AC4: the composer targets the finding's producer host.
        assert_eq!(
            home.machine().map(|machine| machine.name.as_str()),
            Some("ub2")
        );
        // AC5: opening the composer never creates a pane.
        assert!(app.workspaces.is_empty());
        assert_eq!(app.sidebar_selected_work_group, None);
    }

    #[test]
    fn ac4_n_on_an_aloop_finding_dispatches_to_the_producer_host() {
        let mut app = aloop_key_fixture();
        app.machines = crate::app::machines::resolve(&crate::config::FleetConfig {
            hosts: vec![crate::config::FleetHostConfig {
                name: "ub2".to_string(),
                target: "ub2".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        });
        app.sidebar_selected_work_group = Some("aloop:finding:nightly:abc-123".into());

        let crate::app::SidebarWorkGroupKeyAction::Dispatch(plan) = app
            .handle_sidebar_work_group_key(KeyEvent::new(
                KeyCode::Char('n'),
                KeyModifiers::empty(),
            ))
        else {
            panic!("n should dispatch the selected finding");
        };

        assert_eq!(plan.prompt, "fix the crash\n\nEvidence:\nstacktrace line");
        let remote = plan.remote.expect("producer host machine");
        assert_eq!(remote.name, "ub2");
    }

    #[test]
    fn ac8_enter_on_an_aloop_loop_header_opens_its_run_history() {
        let mut app = aloop_key_fixture();
        app.sidebar_selected_work_group = Some("aloop:loop:nightly".into());

        let action =
            app.handle_sidebar_work_group_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert!(matches!(
            action,
            crate::app::SidebarWorkGroupKeyAction::Consumed
        ));
        let detail = app
            .loop_run_history_detail
            .as_ref()
            .expect("run history opened");
        assert_eq!(detail.loop_id, "nightly");
        let rendered = crate::ui::loop_runs::project_loop_run_history(
            &detail.history,
            &detail.loop_id,
            std::time::UNIX_EPOCH,
        );
        assert_eq!(rendered.rows.len(), 2);
        assert_eq!(rendered.rows[0].run_id, "2026-09-18T09:59:00Z");
        assert_eq!(rendered.rows[1].run_id, "2026-09-18T09:58:00Z");
        assert_eq!(rendered.rows[1].duration, "2s");
        assert_eq!(app.sidebar_selected_work_group, None);
        // A loop header never dispatches.
        assert!(matches!(
            {
                app.sidebar_selected_work_group = Some("aloop:loop:nightly".into());
                app.handle_sidebar_work_group_key(KeyEvent::new(
                    KeyCode::Char('n'),
                    KeyModifiers::empty(),
                ))
            },
            crate::app::SidebarWorkGroupKeyAction::Consumed
        ));
        assert!(app.workspaces.is_empty());
    }

    #[test]
    fn ac7_enter_on_a_clean_run_opens_its_run_log() {
        let mut app = aloop_key_fixture();
        app.sidebar_selected_work_group =
            Some("aloop:cleanrun:nightly:2026-09-18T09:59:00Z".into());

        let action =
            app.handle_sidebar_work_group_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert!(matches!(
            action,
            crate::app::SidebarWorkGroupKeyAction::Consumed
        ));
        let detail = app.aloop_run_detail.expect("run log opened");
        assert_eq!(detail.loop_name, "nightly");
        assert_eq!(detail.host, "ub2");
        assert_eq!(detail.run.log_excerpt, "clean log tail");
    }

    #[test]
    fn ac4_dispatch_fails_loudly_when_the_producer_host_is_not_a_machine() {
        let mut app = aloop_key_fixture();
        app.sidebar_selected_work_group = Some("aloop:finding:nightly:abc-123".into());

        let action = app.handle_sidebar_work_group_key(KeyEvent::new(
            KeyCode::Char('n'),
            KeyModifiers::empty(),
        ));

        assert!(matches!(
            action,
            crate::app::SidebarWorkGroupKeyAction::Consumed
        ));
        assert_eq!(
            app.config_diagnostic.as_deref(),
            Some("aloop producer host `ub2` is not a configured machine")
        );
        assert!(app.workspaces.is_empty());
    }

    #[test]
    fn ac7_enter_on_a_run_line_toggles_its_fold_without_dispatching() {
        let mut app = aloop_key_fixture();
        let key = "aloop:run:nightly:2026-09-18T09:59:00Z".to_string();
        app.sidebar_selected_work_group = Some(key.clone());

        let action =
            app.handle_sidebar_work_group_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert!(matches!(
            action,
            crate::app::SidebarWorkGroupKeyAction::Consumed
        ));
        assert!(app
            .collapsed_sidebar_groups
            .iter()
            .any(|entry| entry.ends_with(&key)));
        // The fold keeps the row selected so Enter toggles it back open.
        assert_eq!(
            app.sidebar_selected_work_group.as_deref(),
            Some(key.as_str())
        );
    }
}
