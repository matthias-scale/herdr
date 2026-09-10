use bytes::Bytes;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Direction, Rect};
use tracing::warn;

use crate::{
    app::state::{
        AddActionState, AppState, ContextMenuKind, ContextMenuState, DragState, DragTarget,
        HomeHitTarget, MenuListState, Mode, PaneMenuWorkLink, PaneMenuWorkLinkAction,
        RightClickPassthroughGesture, TabPressState, ViewLayout, WorkspacePressState,
    },
    layout::{PaneId, PaneInfo, SplitBorder},
    selection::Selection,
    terminal::TerminalRuntimeRegistry,
};

#[cfg(test)]
use super::WheelRouting;
use super::{
    modal::{
        apply_global_menu_action, confirm_close_cancel, global_menu_actions, leave_modal,
        modal_action_from_buttons, open_global_menu, open_new_tab_dialog, ModalAction,
    },
    settings::SettingsAction,
    ScrollbarClickTarget, TAB_DRAG_THRESHOLD, WORKSPACE_DRAG_THRESHOLD,
};

fn dock_body_contains(app: &AppState, column: u16, row: u16) -> bool {
    let body = app.view.dock_body_rect;
    column >= body.x
        && column < body.x.saturating_add(body.width)
        && row >= body.y
        && row < body.y.saturating_add(body.height)
}

pub(super) enum MouseAction {
    SidebarObjectMenu {
        index: usize,
    },
    SettledMenu {
        index: usize,
    },
    SidebarNewMenu {
        action: crate::app::state::SidebarNewMenuAction,
    },
    NewWorkspace,
    DispatchSidebarWork(Box<crate::app::home::HomeDispatchPlan>),
    Settings(SettingsAction),
    FocusWorkspace {
        ws_idx: usize,
    },
    FocusTab {
        tab_idx: usize,
    },
    FocusSidebarTab {
        ws_idx: usize,
        tab_idx: usize,
    },
    FocusPane {
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    },
    FocusToastTarget,
    RefreshDockFiles,
    SortDockFiles,
    PreviewDockFile(std::path::PathBuf),
    OpenDockFile(std::path::PathBuf),
    RefreshEditorPreview,
    OpenEditorPreview,
    MoveWorkspace {
        source_ws_idx: usize,
        insert_idx: usize,
    },
    MoveWorkspaceBlock {
        params: crate::api::schema::WorkspaceMoveBlockParams,
    },
    MoveTab {
        ws_idx: usize,
        source_tab_idx: usize,
        insert_idx: usize,
    },
    SetSplitRatio {
        path: Vec<bool>,
        ratio: f32,
    },
    RenameModal(ModalAction),
    ConfirmCloseAccept,
    ContextMenu {
        menu: ContextMenuState,
        idx: usize,
    },
    /// Open a Symphony job: its checkout terminal plus the dock surface bound
    /// to it. Index into the current snapshot, resolved by the app because
    /// creating the tab needs the runtime.
    OpenSymphonyWorkflow {
        index: usize,
    },
    /// Open a configured fleet host in a normal local tab.
    OpenFleetHost {
        name: String,
    },
    /// Hand a link to the desktop browser.
    OpenUrl {
        url: String,
    },
}

enum MobileMouseResult {
    Ignored,
    Consumed,
    Action(MouseAction),
}

impl AppState {
    pub(crate) fn handle_pane_mouse_only(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        mouse: MouseEvent,
    ) {
        self.forwarded_pane_input = None;
        if self.mode != Mode::Terminal
            || self.symphony_detail.is_some()
            || self.inbox.is_some()
            || self.work_view.is_some()
            || self.dock_object_preview.is_some()
        {
            return;
        }
        let Some(info) = self.pane_at(mouse.column, mouse.row).cloned() else {
            return;
        };

        match mouse.kind {
            MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight => {
                self.forward_pane_reported_wheel(terminal_runtimes, &info, mouse);
            }
            MouseEventKind::Down(_) | MouseEventKind::Up(_) | MouseEventKind::Drag(_) => {
                self.forward_pane_mouse_button(terminal_runtimes, &info, mouse);
            }
            MouseEventKind::Moved => {
                self.forward_pane_mouse_motion(terminal_runtimes, &info, mouse);
            }
        }
    }

    pub(super) fn handle_mouse(
        &mut self,
        terminal_runtimes: &mut TerminalRuntimeRegistry,
        source_id: crate::app::InputSourceId,
        mouse: MouseEvent,
    ) -> Option<MouseAction> {
        self.forwarded_pane_input = None;
        // Same rule as the keyboard: a due break reminder owns the screen.
        if self.pomodoro.prompt.is_some() {
            return None;
        }
        if self.handle_notepad_mouse(&mouse) {
            return None;
        }
        if rect_contains(self.view.pomodoro_hit_area, mouse.column, mouse.row) {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    self.toggle_pomodoro(std::time::Instant::now());
                    return None;
                }
                // Right-click ends the phase early, which is the deliberate
                // "I am done with this block" action.
                MouseEventKind::Down(MouseButton::Right) => {
                    self.skip_pomodoro_phase(std::time::Instant::now());
                    return None;
                }
                _ => {}
            }
        }
        if rect_contains(self.view.hyperspace_pause_hit_area, mouse.column, mouse.row)
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        {
            self.hyperspace.toggle_paused(std::time::Instant::now());
            return None;
        }
        if self.mode == Mode::Onboarding {
            self.handle_onboarding_mouse(mouse);
            return None;
        }
        if self.add_project_active() {
            if matches!(
                mouse.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            ) {
                let delta = if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                    -3
                } else {
                    3
                };
                self.add_project_scroll(delta);
                return None;
            }
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                let layout = self.view.add_project_layout.clone();
                if rect_contains(layout.close, mouse.column, mouse.row) {
                    self.close_add_project();
                } else if rect_contains(layout.hidden_toggle, mouse.column, mouse.row) {
                    self.add_project_toggle_hidden();
                } else if let Some(path) = layout.breadcrumbs.iter().find_map(|(path, rect)| {
                    rect_contains(*rect, mouse.column, mouse.row).then(|| path.clone())
                }) {
                    self.add_project_jump_to_breadcrumb(path);
                } else if let Some(tab) = layout.tabs.iter().find_map(|(tab, rect)| {
                    rect_contains(*rect, mouse.column, mouse.row).then_some(*tab)
                }) {
                    self.add_project_select_tab(tab);
                } else if let Some(index) = layout.list.as_ref().and_then(|dropdown| {
                    crate::ui::dropdown::hit_test(dropdown, mouse.column, mouse.row)
                }) {
                    let tab = self
                        .home
                        .as_ref()
                        .and_then(|home| home.add_project.as_ref())
                        .map(|project| project.tab);
                    match tab {
                        Some(crate::app::home::AddProjectTab::LocalFolder) => {
                            if let Some(entry_index) = index.checked_sub(1) {
                                self.add_project_select_row(entry_index);
                            }
                        }
                        Some(crate::app::home::AddProjectTab::GitHub) => {
                            self.add_project_select_row(index);
                            self.accept_add_project();
                        }
                        _ => {}
                    }
                }
            }
            return None;
        }
        if self.mode == Mode::AddAction {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                if rect_contains(self.view.add_action_close_hit_area, mouse.column, mouse.row)
                    || rect_contains(
                        self.view.add_action_cancel_hit_area,
                        mouse.column,
                        mouse.row,
                    )
                {
                    self.add_action = None;
                    self.mode = Mode::Terminal;
                } else if rect_contains(self.view.add_action_save_hit_area, mouse.column, mouse.row)
                {
                    self.request_save_add_action = true;
                } else if let Some(field) =
                    self.view
                        .add_action_field_hit_areas
                        .iter()
                        .find_map(|(field, rect)| {
                            rect_contains(*rect, mouse.column, mouse.row).then_some(*field)
                        })
                {
                    if let Some(action) = self.add_action.as_mut() {
                        action.field = field;
                        match field {
                            crate::app::state::AddActionField::RunOnWorktreeCreate => {
                                action.run_on_worktree_create = !action.run_on_worktree_create;
                            }
                            crate::app::state::AddActionField::OpenInBottomPane => {
                                action.open_in_bottom_pane = !action.open_in_bottom_pane;
                            }
                            _ => {}
                        }
                    }
                }
            }
            return None;
        }

        // Home covers the panes, so every event inside its frame is consumed:
        // otherwise a click that misses a row reaches the pane hidden behind it
        // and silently moves focus. The status bar sits outside this rect, so
        // its buttons — including the home button — keep working.
        if self.home.is_some()
            && self.point_in_rect(self.view.terminal_area, mouse.column, mouse.row)
        {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                if let Some(target) = self.home_hit_at(mouse.column, mouse.row) {
                    match target {
                        HomeHitTarget::QueueRow(index) => {
                            let queue = self.blocked_agents();
                            if let Some(home) = self.home.as_mut() {
                                home.select(index);
                            }
                            self.jump_to_selected_home_agent(&queue);
                        }
                        HomeHitTarget::PickerOption(index) => {
                            if self.home_browse_active() {
                                // A row under the path input is a child to
                                // descend into, not a directory to dispatch in.
                                self.home_browse_select(index);
                                return None;
                            }
                            if let Some(home) = self.home.as_mut() {
                                if home.picker == Some(crate::app::home::HomePicker::Directory) {
                                    home.directory_filter.selected = index;
                                } else if home.picker == Some(crate::app::home::HomePicker::Ref) {
                                    home.ref_filter.selected = index;
                                } else {
                                    home.picker_selected = index;
                                }
                            }
                            self.home_accept_picker();
                        }
                        HomeHitTarget::NewTask | HomeHitTarget::Prompt => {
                            self.home_focus_prompt();
                        }
                        HomeHitTarget::Reply => {
                            self.home_focus_reply();
                        }
                        HomeHitTarget::Detach => {
                            let queue = self.blocked_agents();
                            self.jump_to_selected_home_agent(&queue);
                        }
                        target => {
                            let picker = match target {
                                HomeHitTarget::Agent => crate::app::home::HomePicker::Agent,
                                HomeHitTarget::Model => crate::app::home::HomePicker::Model,
                                HomeHitTarget::Effort => crate::app::home::HomePicker::Effort,
                                HomeHitTarget::Access => crate::app::home::HomePicker::Access,
                                HomeHitTarget::Context => crate::app::home::HomePicker::Context,
                                HomeHitTarget::Project => crate::app::home::HomePicker::Project,
                                HomeHitTarget::Repo => crate::app::home::HomePicker::Repo,
                                HomeHitTarget::Machine => crate::app::home::HomePicker::Machine,
                                HomeHitTarget::Directory => crate::app::home::HomePicker::Directory,
                                HomeHitTarget::Workspace => crate::app::home::HomePicker::Workspace,
                                HomeHitTarget::Ref => crate::app::home::HomePicker::Ref,
                                HomeHitTarget::Target => crate::app::home::HomePicker::Target,
                                HomeHitTarget::QueueRow(_)
                                | HomeHitTarget::NewTask
                                | HomeHitTarget::Reply
                                | HomeHitTarget::Detach
                                | HomeHitTarget::Prompt
                                | HomeHitTarget::PickerOption(_) => return None,
                            };
                            self.home_focus_picker(picker);
                        }
                    }
                } else {
                    self.home_dismiss_picker();
                }
            }
            return None;
        }

        if self.mode == Mode::Terminal
            && self.clickable_toast_at(mouse.column, mouse.row)
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        {
            return Some(MouseAction::FocusToastTarget);
        }

        if self.mode == Mode::Terminal
            && self.clickable_toast_at(mouse.column, mouse.row)
            && matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left))
        {
            return None;
        }

        if self.mode == Mode::Settings {
            return self.handle_settings_mouse(mouse).map(MouseAction::Settings);
        }

        // A press decides which surface owns the keyboard, and the sidebar's
        // bare-key shortcuts are gated on owning it. Deciding here rather than
        // per hit-test means a press that lands anywhere else revokes it, and a
        // press that focuses a pane revokes it again when the resulting action
        // runs `release_dock_focus_to_pane`.
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            self.sidebar_focused = self.sidebar_claims_pointer(mouse.column, mouse.row);
        }
        let group_menu_enabled = self.view.layout != ViewLayout::Mobile
            && !self.sidebar_collapsed
            && matches!(self.mode, Mode::Terminal | Mode::Navigate | Mode::Resize);
        let new_thread_anchor = crate::ui::sidebar_header_new_thread_rect(self.view.sidebar_rect);
        let new_menu_anchor = crate::ui::sidebar_header_new_menu_rect(self.view.sidebar_rect);
        let search_anchor = crate::ui::sidebar_header_search_rect(self.view.sidebar_rect);
        let new_thread_hit =
            group_menu_enabled && self.point_in_rect(new_thread_anchor, mouse.column, mouse.row);
        let new_menu_hit =
            group_menu_enabled && self.point_in_rect(new_menu_anchor, mouse.column, mouse.row);
        let search_hit =
            group_menu_enabled && self.point_in_rect(search_anchor, mouse.column, mouse.row);
        let group_anchor = self.sidebar_group_mode_anchor_rect();
        let filter_anchor = self.sidebar_filter_anchor_rect();
        let filter_anchor_hit =
            group_menu_enabled && self.point_in_rect(filter_anchor, mouse.column, mouse.row);
        // The filter chip sits inside the mode anchor, so it claims the click
        // first; the rest of the header still opens the mode dropdown.
        let group_anchor_hit = group_menu_enabled
            && !filter_anchor_hit
            && self.point_in_rect(group_anchor, mouse.column, mouse.row);
        if matches!(mouse.kind, MouseEventKind::Moved) && self.sidebar_new_menu.is_some() {
            if let Some(index) = self.sidebar_new_menu_item_at(mouse.column, mouse.row) {
                if let Some(menu) = self.sidebar_new_menu.as_mut() {
                    menu.selected = index;
                }
            }
            return None;
        }
        if self.sidebar_new_menu.is_some()
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        {
            if new_menu_hit {
                self.sidebar_new_menu = None;
            } else if let Some(index) = self.sidebar_new_menu_item_at(mouse.column, mouse.row) {
                let action = crate::app::state::SidebarNewMenuAction::ALL
                    .get(index)
                    .copied();
                self.sidebar_new_menu = None;
                return action.map(|action| MouseAction::SidebarNewMenu { action });
            } else {
                self.sidebar_new_menu = None;
            }
            return None;
        }
        if matches!(mouse.kind, MouseEventKind::Moved) && self.sidebar_new_thread.is_some() {
            if let Some(index) = self.sidebar_new_thread_item_at(mouse.column, mouse.row) {
                if let Some(picker) = self.sidebar_new_thread.as_mut() {
                    picker.filter.selected = index;
                }
            }
            return None;
        }
        if self.sidebar_new_thread.is_some()
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        {
            if new_thread_hit {
                self.sidebar_new_thread = None;
            } else if let Some(index) = self.sidebar_new_thread_item_at(mouse.column, mouse.row) {
                self.accept_sidebar_new_thread(index);
            } else {
                self.sidebar_new_thread = None;
            }
            return None;
        }
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) && new_thread_hit {
            self.open_sidebar_new_thread();
            return None;
        }
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) && new_menu_hit {
            self.open_sidebar_new_menu();
            return None;
        }
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) && search_hit {
            self.sidebar_group_menu_open = false;
            self.sidebar_filter_menu_open = false;
            self.sidebar_object_menu = None;
            self.sidebar_search_active = true;
            return None;
        }
        if matches!(mouse.kind, MouseEventKind::Moved) && self.sidebar_object_menu.is_some() {
            if let Some(index) = crate::ui::sidebar_object_menu_item_at(
                self,
                self.screen_rect(),
                mouse.column,
                mouse.row,
            ) {
                if let Some(menu) = self.sidebar_object_menu.as_mut() {
                    menu.selected = index;
                }
            }
            return None;
        }
        if self.sidebar_object_menu.is_some() {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                if let Some(index) = crate::ui::sidebar_object_menu_item_at(
                    self,
                    self.screen_rect(),
                    mouse.column,
                    mouse.row,
                ) {
                    return Some(MouseAction::SidebarObjectMenu { index });
                }
                if self.sidebar_object_menu.as_ref().is_some_and(|menu| {
                    menu.page == crate::app::state::SidebarObjectMenuPage::Confirmation
                }) {
                    self.dock_pending_write = None;
                }
                self.sidebar_object_menu = None;
            }
            return None;
        }
        if group_menu_enabled && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            if let Some(target) = crate::ui::sidebar_object_action_at(self, mouse.column, mouse.row)
            {
                self.open_sidebar_object_menu(target);
                if let Some(menu) = self.sidebar_object_menu.as_mut() {
                    menu.anchor_row = Some(mouse.row);
                }
                return None;
            }
        }
        if matches!(mouse.kind, MouseEventKind::Moved) && self.sidebar_settled_menu_target.is_some()
        {
            if let Some(index) = self.sidebar_settled_menu_item_at(mouse.column, mouse.row) {
                if index != self.sidebar_settled_menu_selected {
                    self.sidebar_settled_menu_delete_armed = false;
                }
                self.sidebar_settled_menu_selected = index;
            }
            return None;
        }
        if self.sidebar_settled_menu_target.is_some() {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                if let Some(index) = self.sidebar_settled_menu_item_at(mouse.column, mouse.row) {
                    return Some(MouseAction::SettledMenu { index });
                }
                self.sidebar_settled_menu_target = None;
                self.sidebar_settled_menu_delete_armed = false;
            }
            return None;
        }
        if matches!(mouse.kind, MouseEventKind::Moved) && self.sidebar_filter_menu_open {
            if let Some(index) = self.sidebar_filter_menu_item_at(mouse.column, mouse.row) {
                self.sidebar_filter_menu_selected = index;
            }
            return None;
        }
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) && filter_anchor_hit {
            if self.sidebar_filter_menu_open {
                self.sidebar_filter_menu_open = false;
            } else {
                self.open_sidebar_filter_menu();
            }
            return None;
        }
        if self.sidebar_filter_menu_open {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                match self.sidebar_filter_menu_item_at(mouse.column, mouse.row) {
                    Some(index) => self.select_sidebar_filter_option(index),
                    None => self.sidebar_filter_menu_open = false,
                }
            }
            return None;
        }
        if matches!(mouse.kind, MouseEventKind::Moved) && self.sidebar_group_menu_open {
            if let Some(index) = self.sidebar_group_menu_item_at(mouse.column, mouse.row) {
                self.sidebar_group_menu_selected = index;
            }
            return None;
        }
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) && group_anchor_hit {
            if self.sidebar_group_menu_open {
                self.sidebar_group_menu_open = false;
            } else {
                self.open_sidebar_group_menu();
            }
            return None;
        }
        if self.sidebar_group_menu_open {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                if let Some(index) = self.sidebar_group_menu_item_at(mouse.column, mouse.row) {
                    if let Some(mode) = crate::app::state::SidebarGroupMode::VIEWS
                        .get(index)
                        .copied()
                    {
                        self.set_sidebar_group_mode(mode);
                    }
                } else {
                    self.sidebar_group_menu_open = false;
                }
            }
            return None;
        }

        let launcher_enabled = self.view.layout != ViewLayout::Mobile
            && !self.sidebar_collapsed
            && matches!(
                self.mode,
                Mode::Terminal
                    | Mode::Navigate
                    | Mode::Resize
                    | Mode::GlobalMenu
                    | Mode::KeybindHelp
            );
        let launcher = self.global_launcher_rect();
        let launcher_hit = launcher_enabled
            && mouse.column >= launcher.x
            && mouse.column < launcher.x + launcher.width
            && mouse.row >= launcher.y
            && mouse.row < launcher.y + launcher.height;

        if matches!(mouse.kind, MouseEventKind::Moved) && self.mode == Mode::GlobalMenu {
            let actions = global_menu_actions(self);
            let hovered = self
                .global_menu_item_at(mouse.column, mouse.row)
                .and_then(|action| actions.iter().position(|item| *item == action));
            self.global_menu.hover(hovered);
            return None;
        }

        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) && launcher_hit {
            if self.mode == Mode::GlobalMenu {
                leave_modal(self);
            } else {
                open_global_menu(self);
            }
            return None;
        }

        if self.mode == Mode::GlobalMenu {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                if let Some(action) = self.global_menu_item_at(mouse.column, mouse.row) {
                    apply_global_menu_action(self, action);
                } else {
                    leave_modal(self);
                }
            }
            return None;
        }

        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && rect_contains(
                self.view.repo_editor_button_hit_area,
                mouse.column,
                mouse.row,
            )
            && matches!(self.mode, Mode::Terminal | Mode::Navigate)
        {
            if self.repo_editor_available() {
                self.request_open_repo_editor = true;
                self.mode = Mode::Terminal;
            }
            return None;
        }

        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && rect_contains(
                self.view.add_action_button_hit_area,
                mouse.column,
                mouse.row,
            )
            && matches!(self.mode, Mode::Terminal | Mode::Navigate)
        {
            self.add_action = Some(AddActionState::default());
            self.mode = Mode::AddAction;
            return None;
        }

        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && matches!(self.mode, Mode::Terminal | Mode::Navigate)
        {
            if let Some(index) = self
                .view
                .user_action_hit_areas
                .iter()
                .find_map(|(index, rect)| {
                    rect_contains(*rect, mouse.column, mouse.row).then_some(*index)
                })
            {
                self.request_user_action = Some(index);
                self.mode = Mode::Terminal;
                return None;
            }
        }

        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && self.on_git_menu_button(mouse.column, mouse.row)
        {
            if self.mode == Mode::GitMenu {
                self.mode = Mode::Terminal;
            } else if matches!(self.mode, Mode::Terminal | Mode::Navigate) {
                self.git_menu = MenuListState::new(0);
                self.mode = Mode::GitMenu;
            }
            return None;
        }

        if self.mode == Mode::GitMenu {
            let in_git_repo = crate::ui::dock::chooser::focused_in_git_repo(self);
            match mouse.kind {
                MouseEventKind::Moved => {
                    let hovered = self
                        .git_menu_row_at(mouse.column, mouse.row)
                        .filter(|index| {
                            in_git_repo && *index < crate::app::state::GitAction::ALL.len()
                        });
                    self.git_menu.hover(hovered);
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if in_git_repo {
                        if let Some(index) = self.git_menu_row_at(mouse.column, mouse.row) {
                            if let Some(action) =
                                crate::app::state::GitAction::ALL.get(index).copied()
                            {
                                self.request_git_action = Some(action);
                                self.mode = Mode::Terminal;
                            }
                        } else {
                            self.mode = Mode::Terminal;
                        }
                    } else if self.git_menu_row_at(mouse.column, mouse.row).is_none() {
                        self.mode = Mode::Terminal;
                    }
                }
                _ => {}
            }
            return None;
        }

        if self.mode == Mode::KeybindHelp {
            return None;
        }

        if self.view.layout == ViewLayout::Mobile {
            match self.handle_mobile_mouse(mouse) {
                MobileMouseResult::Ignored => {}
                MobileMouseResult::Consumed => return None,
                MobileMouseResult::Action(action) => return Some(action),
            }
        }

        let sidebar = self.view.sidebar_rect;
        let in_sidebar = mouse.column >= sidebar.x
            && mouse.column < sidebar.x + sidebar.width
            && mouse.row >= sidebar.y
            && mouse.row < sidebar.y + sidebar.height;
        let dock = self.view.dock_rect;
        let in_dock = mouse.column >= dock.x
            && mouse.column < dock.x.saturating_add(dock.width)
            && mouse.row >= dock.y
            && mouse.row < dock.y.saturating_add(dock.height);

        if self.handle_right_click_passthrough(
            terminal_runtimes,
            source_id,
            mouse,
            in_sidebar || in_dock,
        ) {
            return None;
        }

        if self.mode == Mode::OpenExistingWorktree {
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    if let Some(open) = &mut self.worktree_open {
                        open.select_previous_filtered();
                    }
                    return None;
                }
                MouseEventKind::ScrollDown => {
                    if let Some(open) = &mut self.worktree_open {
                        open.select_next_filtered();
                    }
                    return None;
                }
                _ => {}
            }
        }

        if matches!(
            self.mode,
            Mode::NewLinkedWorktree | Mode::OpenExistingWorktree | Mode::ConfirmRemoveWorktree
        ) && !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        {
            return None;
        }

        if self.dock_editor_preview.is_some()
            && self.point_in_rect(self.view.terminal_area, mouse.column, mouse.row)
        {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                if rect_contains(
                    self.view.editor_preview_refresh_rect,
                    mouse.column,
                    mouse.row,
                ) {
                    return Some(MouseAction::RefreshEditorPreview);
                }
                if rect_contains(self.view.editor_preview_open_rect, mouse.column, mouse.row) {
                    return Some(MouseAction::OpenEditorPreview);
                }
            }
            return None;
        }

        if self.dock_object_preview.is_some()
            && self.point_in_rect(self.view.terminal_area, mouse.column, mouse.row)
        {
            // The preview draws the same detail the dock tab does, so its
            // section headers fold on click too. Resolved from the previewed
            // surface, because the dock tab underneath it may be anything.
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                if let Some((object_key, section)) = self
                    .dock_object_preview
                    .as_ref()
                    .map(|object| object.surface)
                    .and_then(|surface| {
                        self.dock_detail_section_at(surface, mouse.column, mouse.row)
                    })
                {
                    self.dock_object_views
                        .entry(object_key)
                        .or_default()
                        .toggle_section(section);
                    return None;
                }
            }
            let delta = match mouse.kind {
                MouseEventKind::ScrollUp => Some(-3_i16),
                MouseEventKind::ScrollDown => Some(3_i16),
                _ => None,
            };
            if let Some(delta) = delta {
                let key = match self
                    .dock_object_preview
                    .as_ref()
                    .map(|object| object.surface)
                {
                    Some(crate::app::DockSurface::Pr) => crate::ui::dock::pr::focused_pr_key(self),
                    Some(crate::app::DockSurface::Linear) => {
                        crate::ui::dock::linear::focused_ticket_key(self)
                    }
                    _ => None,
                };
                if let Some(key) = key {
                    let view = self.dock_object_views.entry(key).or_default();
                    view.scroll = view.scroll.saturating_add_signed(delta);
                } else {
                    self.dock_scroll = self.dock_scroll.saturating_add_signed(delta);
                }
            }
            return None;
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.selection = None;
                self.selection_autoscroll = None;
                self.clear_chrome_press(source_id);

                if self.mode == Mode::ConfirmClose {
                    let popup = self.confirm_close_rect();
                    let inner = Rect::new(
                        popup.x + 1,
                        popup.y + 1,
                        popup.width.saturating_sub(2),
                        popup.height.saturating_sub(2),
                    );
                    let (confirm, cancel) = crate::ui::confirm_close_button_rects(inner);
                    match modal_action_from_buttons(
                        mouse.column,
                        mouse.row,
                        &[
                            (confirm, ModalAction::Confirm),
                            (cancel, ModalAction::Cancel),
                        ],
                    ) {
                        Some(ModalAction::Confirm) => {
                            return Some(MouseAction::ConfirmCloseAccept);
                        }
                        Some(ModalAction::Cancel) | None => confirm_close_cancel(self),
                        _ => {}
                    }
                    return None;
                }

                if self.mode == Mode::NewLinkedWorktree {
                    if let Some(inner) =
                        crate::ui::new_linked_worktree_inner_rect(self.screen_rect())
                    {
                        let (create, cancel) = crate::ui::new_linked_worktree_button_rects(inner);
                        match modal_action_from_buttons(
                            mouse.column,
                            mouse.row,
                            &[
                                (create, ModalAction::Confirm),
                                (cancel, ModalAction::Cancel),
                            ],
                        ) {
                            Some(ModalAction::Confirm) => {
                                self.request_submit_worktree_create = true;
                            }
                            Some(ModalAction::Cancel)
                                if !self
                                    .worktree_create
                                    .as_ref()
                                    .is_some_and(|create| create.creating) =>
                            {
                                self.worktree_create = None;
                                self.name_input.clear();
                                self.name_input_replace_on_type = false;
                                leave_modal(self);
                            }
                            _ => {}
                        }
                    }
                    return None;
                }

                if self.mode == Mode::OpenExistingWorktree {
                    if let Some(open) = self.worktree_open.as_ref() {
                        if let Some(inner) = crate::ui::open_existing_worktree_inner_rect(
                            self.screen_rect(),
                            open.entries.len(),
                        ) {
                            let filtered = open.filtered_indices();
                            let max_rows =
                                crate::ui::open_existing_worktree_max_visible_rows(inner);
                            let start =
                                crate::ui::open_existing_worktree_visible_start(open, max_rows);
                            if mouse.row == inner.y.saturating_add(1)
                                && mouse.column >= inner.x
                                && mouse.column < inner.x.saturating_add(inner.width)
                            {
                                if let Some(open) = &mut self.worktree_open {
                                    open.search_focused = true;
                                }
                                return None;
                            }
                            let row_idx = if rect_contains(inner, mouse.column, mouse.row) {
                                mouse
                                    .row
                                    .checked_sub(inner.y.saturating_add(3))
                                    .map(usize::from)
                                    .map(|row| row / 2)
                                    .filter(|row| *row < max_rows)
                                    .and_then(|row| filtered.get(start + row).copied())
                            } else {
                                None
                            };
                            if let Some(entry_idx) = row_idx {
                                if let Some(open) = &mut self.worktree_open {
                                    open.selected = entry_idx;
                                }
                                self.request_submit_worktree_open = true;
                                return None;
                            }

                            let (open_button, cancel) =
                                crate::ui::open_existing_worktree_button_rects(inner);
                            match modal_action_from_buttons(
                                mouse.column,
                                mouse.row,
                                &[
                                    (open_button, ModalAction::Confirm),
                                    (cancel, ModalAction::Cancel),
                                ],
                            ) {
                                Some(ModalAction::Confirm) => {
                                    self.request_submit_worktree_open = true;
                                }
                                Some(ModalAction::Cancel) => {
                                    self.worktree_open = None;
                                    leave_modal(self);
                                }
                                _ => {}
                            }
                        }
                    }
                    return None;
                }

                if self.mode == Mode::ConfirmRemoveWorktree {
                    if let Some(popup) = crate::ui::remove_worktree_popup_rect(self.screen_rect()) {
                        let inner = Rect::new(
                            popup.x + 1,
                            popup.y + 1,
                            popup.width.saturating_sub(2),
                            popup.height.saturating_sub(2),
                        );
                        let force_confirmation = self
                            .worktree_remove
                            .as_ref()
                            .is_some_and(|remove| remove.force_confirmation);
                        let (remove, cancel) =
                            crate::ui::remove_worktree_button_rects(inner, force_confirmation);
                        match modal_action_from_buttons(
                            mouse.column,
                            mouse.row,
                            &[
                                (remove, ModalAction::Confirm),
                                (cancel, ModalAction::Cancel),
                            ],
                        ) {
                            Some(ModalAction::Confirm) => {
                                self.request_submit_worktree_remove = true;
                            }
                            Some(ModalAction::Cancel)
                                if !self
                                    .worktree_remove
                                    .as_ref()
                                    .is_some_and(|remove| remove.removing) =>
                            {
                                self.worktree_remove = None;
                                leave_modal(self);
                            }
                            _ => {}
                        }
                    }
                    return None;
                }

                if matches!(
                    self.mode,
                    Mode::RenameWorkspace | Mode::RenameTab | Mode::RenamePane
                ) {
                    let action = self
                        .rename_modal_inner()
                        .map(crate::ui::rename_button_rects)
                        .and_then(|(save, clear, cancel)| {
                            modal_action_from_buttons(
                                mouse.column,
                                mouse.row,
                                &[
                                    (save, ModalAction::Save),
                                    (clear, ModalAction::Clear),
                                    (cancel, ModalAction::Cancel),
                                ],
                            )
                        })
                        .unwrap_or(ModalAction::Cancel);
                    return Some(MouseAction::RenameModal(action));
                }

                if self.mode == Mode::ContextMenu {
                    let item_idx = self.context_menu_item_at(mouse.column, mouse.row);
                    if let Some(menu) = self.context_menu.take() {
                        if let Some(idx) = item_idx {
                            return Some(MouseAction::ContextMenu { menu, idx });
                        } else {
                            leave_modal(self);
                        }
                    }
                    return None;
                }

                if self.on_dock_divider(mouse.column, mouse.row) {
                    self.drag = Some(DragState {
                        target: DragTarget::DockDivider,
                    });
                    self.set_manual_dock_width(mouse.column);
                    return None;
                }
                if self.on_dock_toggle(mouse.column, mouse.row) {
                    self.dock_collapsed = !self.dock_collapsed;
                    self.dock_home_focused = !self.dock_collapsed
                        && self.dock_tab == Some(crate::app::DockSurface::Home);
                    self.dock_diff_focused = !self.dock_collapsed
                        && self.dock_tab == Some(crate::app::DockSurface::Diff);
                    self.dock_files_focused = !self.dock_collapsed
                        && self.dock_tab == Some(crate::app::DockSurface::Files);
                    self.dock_agents_focused = !self.dock_collapsed
                        && self.dock_tab == Some(crate::app::DockSurface::Agents);
                    self.dock_hosts_focused = !self.dock_collapsed
                        && self.dock_tab == Some(crate::app::DockSurface::Hosts);
                    self.dock_pr_focused =
                        !self.dock_collapsed && self.dock_tab == Some(crate::app::DockSurface::Pr);
                    self.dock_linear_focused = !self.dock_collapsed
                        && self.dock_tab == Some(crate::app::DockSurface::Linear);
                    self.mark_session_dirty();
                    return None;
                }
                // The open chooser owns every click while it is up: a row
                // selects, anything else dismisses it before the click can
                // reach the strip underneath.
                if self.dock_surface_menu.is_some() {
                    if let Some(entry) = self.dock_surface_menu_entry_at(mouse.column, mouse.row) {
                        if self.activate_dock_chooser_entry(entry) {
                            self.dock_surface_menu = None;
                        }
                        return None;
                    }
                    self.dock_surface_menu = None;
                    if self.on_dock_plus(mouse.column, mouse.row) {
                        return None;
                    }
                }
                if self.on_dock_maximize(mouse.column, mouse.row) {
                    self.toggle_dock_maximized();
                    self.mark_session_dirty();
                    return None;
                }
                if self.on_dock_tab_close(mouse.column, mouse.row) {
                    if let Some(surface) = self.dock_tab {
                        self.close_dock_surface(surface);
                        self.dock_editor_focused =
                            self.dock_tab == Some(crate::app::DockSurface::Editor);
                        self.dock_home_focused =
                            self.dock_tab == Some(crate::app::DockSurface::Home);
                        self.dock_diff_focused =
                            self.dock_tab == Some(crate::app::DockSurface::Diff);
                        self.dock_files_focused =
                            self.dock_tab == Some(crate::app::DockSurface::Files);
                        self.dock_agents_focused =
                            self.dock_tab == Some(crate::app::DockSurface::Agents);
                        self.dock_hosts_focused =
                            self.dock_tab == Some(crate::app::DockSurface::Hosts);
                        self.dock_pr_focused = self.dock_tab == Some(crate::app::DockSurface::Pr);
                        self.dock_linear_focused =
                            self.dock_tab == Some(crate::app::DockSurface::Linear);
                    }
                    return None;
                }
                if self.on_dock_plus(mouse.column, mouse.row) {
                    self.toggle_dock_surface_menu();
                    return None;
                }
                if let Some(entry) = self.dock_chooser_entry_at(mouse.column, mouse.row, true) {
                    self.activate_dock_chooser_entry(entry);
                    return None;
                }
                if let Some(index) = self.dock_tab_index_at(mouse.column, mouse.row) {
                    let tab = self.dock_open_surfaces.get(index).copied()?;
                    self.select_dock_tab_index(index);
                    self.dock_editor_focused = tab == crate::app::DockSurface::Editor;
                    self.dock_home_focused = tab == crate::app::DockSurface::Home;
                    self.dock_diff_focused = tab == crate::app::DockSurface::Diff;
                    self.dock_files_focused = tab == crate::app::DockSurface::Files;
                    self.dock_agents_focused = tab == crate::app::DockSurface::Agents;
                    self.dock_hosts_focused = tab == crate::app::DockSurface::Hosts;
                    self.dock_pr_focused = tab == crate::app::DockSurface::Pr;
                    self.dock_linear_focused = tab == crate::app::DockSurface::Linear;
                    if self.dock_agents_focused {
                        self.reconcile_dock_agents_selection();
                    }
                    if self.dock_hosts_focused {
                        self.reconcile_dock_hosts_selection();
                    }
                    return None;
                }
                if self.on_dock_diff_whitespace_toggle(mouse.column, mouse.row) {
                    self.toggle_dock_diff_whitespace();
                    self.dock_diff_focused = true;
                    return None;
                }
                if rect_contains(self.view.dock_files_refresh_rect, mouse.column, mouse.row) {
                    self.dock_files_focused = true;
                    return Some(MouseAction::RefreshDockFiles);
                }
                if rect_contains(self.view.dock_files_sort_rect, mouse.column, mouse.row) {
                    self.dock_files_focused = true;
                    return Some(MouseAction::SortDockFiles);
                }
                if let Some(index) = self.dock_diff_file_at(mouse.column, mouse.row) {
                    self.dock_diff_selected = index;
                    self.dock_diff_focused = true;
                    self.toggle_selected_dock_diff_file();
                    return None;
                }
                if let Some(action) =
                    self.click_dock_file_row_at(mouse.column, mouse.row, std::time::Instant::now())
                {
                    return match action {
                        crate::app::files::FileClickAction::DirectoryToggled => None,
                        crate::app::files::FileClickAction::Preview(path) => {
                            Some(MouseAction::PreviewDockFile(path))
                        }
                        crate::app::files::FileClickAction::Open(path) => {
                            Some(MouseAction::OpenDockFile(path))
                        }
                    };
                }
                if self.click_dock_agent_row(mouse.column, mouse.row) {
                    return None;
                }
                if let Some(section) = self.dock_home_section_at(mouse.column, mouse.row) {
                    self.set_dock_home_section(section);
                    self.dock_home_focused = true;
                    // Clicking a section is as deliberate as moving the cursor:
                    // it overrides the focused pane having nothing bound, so the
                    // section opens on a real row instead of an inert list.
                    self.dock_home_focus_unbound = false;
                    return None;
                }
                if let Some(index) = self.dock_home_tab_at(mouse.column, mouse.row) {
                    let rendered_key = self.view.dock_home_tab_keys.get(index).cloned()?;
                    let projection = self.dock_home_projection();
                    let key = match self.dock_home_section {
                        crate::app::state::DockHomeSection::Prs => projection
                            .rows
                            .iter()
                            .find(|row| row.key == rendered_key)
                            .map(|row| row.key.clone()),
                        crate::app::state::DockHomeSection::Tickets => projection
                            .ticket_rows
                            .iter()
                            .find(|row| row.key == rendered_key)
                            .map(|row| row.key.clone()),
                        crate::app::state::DockHomeSection::XPolls => projection
                            .poll_rows
                            .iter()
                            .find(|row| row.key == rendered_key)
                            .map(|row| row.key.clone()),
                    };
                    if let Some(key) = key {
                        // While the focused pane is unbound nothing is selected
                        // on screen, so a click has to select rather than treat
                        // the stale stored key as the current selection.
                        let already_selected = !self.dock_home_focus_unbound
                            && self.dock_home_active_selection().as_ref() == Some(&key);
                        self.dock_home_focus_unbound = false;
                        match self.dock_home_section {
                            crate::app::state::DockHomeSection::Prs => {
                                self.dock_home_selection = Some(key)
                            }
                            crate::app::state::DockHomeSection::Tickets => {
                                self.dock_home_ticket_selection = Some(key)
                            }
                            crate::app::state::DockHomeSection::XPolls => {
                                self.dock_home_poll_selection = Some(key)
                            }
                        }
                        self.dock_home_focused = true;
                        if already_selected {
                            self.jump_to_dock_home_selection();
                        } else {
                            self.dock_scroll = 0;
                        }
                    }
                    return None;
                }
                if let Some(detail_tab) = self.dock_home_detail_tab_at(mouse.column, mouse.row) {
                    self.set_dock_home_detail_tab(detail_tab);
                    return None;
                }
                if in_dock && self.dock_tab == Some(crate::app::DockSurface::Symphony) {
                    if let Some(url) = self.dock_symphony_dashboard_click(mouse.column, mouse.row) {
                        return Some(MouseAction::OpenUrl { url });
                    }
                }
                if in_dock && self.dock_tab == Some(crate::app::DockSurface::Hosts) {
                    if let Some(name) = self.click_dock_host_row(mouse.column, mouse.row) {
                        return Some(MouseAction::OpenFleetHost { name });
                    }
                }
                // A section header folds on click. This runs before the plain
                // focus fallback below, which would otherwise swallow it.
                if in_dock {
                    if let Some((object_key, section)) = self.dock_tab.and_then(|surface| {
                        self.dock_detail_section_at(surface, mouse.column, mouse.row)
                    }) {
                        self.dock_pr_focused = self.dock_tab == Some(crate::app::DockSurface::Pr);
                        self.dock_linear_focused =
                            self.dock_tab == Some(crate::app::DockSurface::Linear);
                        self.dock_object_views
                            .entry(object_key)
                            .or_default()
                            .toggle_section(section);
                        return None;
                    }
                }
                if in_dock {
                    self.dock_editor_focused =
                        self.dock_tab == Some(crate::app::DockSurface::Editor);
                    self.dock_diff_focused = self.dock_tab == Some(crate::app::DockSurface::Diff);
                    self.dock_files_focused = self.dock_tab == Some(crate::app::DockSurface::Files);
                    self.dock_agents_focused =
                        self.dock_tab == Some(crate::app::DockSurface::Agents);
                    self.dock_hosts_focused = self.dock_tab == Some(crate::app::DockSurface::Hosts);
                    self.dock_pr_focused = self.dock_tab == Some(crate::app::DockSurface::Pr);
                    self.dock_linear_focused =
                        self.dock_tab == Some(crate::app::DockSurface::Linear);
                    // Clicking an empty dock hands it the keyboard so the card
                    // shortcuts work without a tab to focus first.
                    self.dock_chooser_focused = self.dock_tab.is_none();
                    return None;
                }

                if self.on_sidebar_divider(mouse.column, mouse.row) {
                    self.drag = Some(DragState {
                        target: DragTarget::SidebarDivider,
                    });
                    self.set_manual_sidebar_width(mouse.column);
                    return None;
                }

                if !in_sidebar && !in_dock {
                    if let Some(border) = self.find_border_at(mouse.column, mouse.row) {
                        let grab_offset = match border.direction {
                            Direction::Horizontal => border.pos.saturating_sub(mouse.column),
                            Direction::Vertical => border.pos.saturating_sub(mouse.row),
                        };
                        self.drag = Some(DragState {
                            target: DragTarget::PaneSplit {
                                path: border.path.clone(),
                                direction: border.direction,
                                area: border.area,
                                grab_offset,
                            },
                        });
                        return None;
                    }

                    if let Some((pane_id, target)) =
                        self.scrollbar_target_at(terminal_runtimes, mouse.column, mouse.row)
                    {
                        self.focus_pane(pane_id);
                        match target {
                            ScrollbarClickTarget::Thumb { grab_row_offset } => {
                                self.drag = Some(DragState {
                                    target: DragTarget::PaneScrollbar {
                                        pane_id,
                                        grab_row_offset,
                                    },
                                });
                            }
                            ScrollbarClickTarget::Track { offset_from_bottom } => {
                                self.set_pane_scroll_offset(
                                    terminal_runtimes,
                                    pane_id,
                                    offset_from_bottom,
                                );
                            }
                        }
                        if self.mode != Mode::Terminal {
                            self.mode = Mode::Terminal;
                        }
                        return None;
                    }
                }

                if self.mode_bar_covers_tab_row(mouse.column, mouse.row) {
                    return None;
                }

                if let Some(direction) = self.pane_toggle_at(mouse.column, mouse.row) {
                    self.request_pane_toggle = Some(direction);
                    self.mode = Mode::Terminal;
                    return None;
                }
                if self.on_tab_scroll_left_button(mouse.column, mouse.row) {
                    self.scroll_tabs_left();
                    return None;
                }
                if self.on_tab_scroll_right_button(mouse.column, mouse.row) {
                    self.scroll_tabs_right();
                    return None;
                }
                if let (Some(ws_idx), Some(tab_idx)) =
                    (self.active, self.tab_pin_glyph_at(mouse.column, mouse.row))
                {
                    self.request_pin_toggle = Some((ws_idx, tab_idx));
                    return None;
                }
                if let (Some(ws_idx), Some(tab_idx)) =
                    (self.active, self.tab_at(mouse.column, mouse.row))
                {
                    self.tab_presses.insert(
                        source_id,
                        TabPressState {
                            ws_idx,
                            tab_idx,
                            start_col: mouse.column,
                            start_row: mouse.row,
                        },
                    );
                    return None;
                }
                if self.on_new_tab_button(mouse.column, mouse.row) {
                    if self.prompt_new_tab_name {
                        open_new_tab_dialog(self);
                    } else {
                        self.request_new_tab = true;
                        self.mode = Mode::Terminal;
                    }
                    return None;
                }

                if in_sidebar {
                    self.sidebar_selected_settled = None;
                    if self.on_sidebar_toggle(mouse.column, mouse.row) {
                        self.sidebar_collapsed = !self.sidebar_collapsed;
                        return None;
                    }

                    if self.sidebar_collapsed {
                        if let Some(idx) = self.collapsed_workspace_at_row(mouse.row) {
                            self.mode = Mode::Terminal;
                            return Some(MouseAction::FocusWorkspace { ws_idx: idx });
                        }

                        if let Some((ws_idx, tab_idx)) =
                            self.collapsed_agent_detail_target_at(mouse.row)
                        {
                            self.mode = Mode::Terminal;
                            return Some(MouseAction::FocusSidebarTab { ws_idx, tab_idx });
                        }
                        return None;
                    }

                    if let Some(target) =
                        self.workspace_list_scrollbar_target_at(mouse.column, mouse.row)
                    {
                        match target {
                            ScrollbarClickTarget::Thumb { grab_row_offset } => {
                                self.drag = Some(DragState {
                                    target: DragTarget::WorkspaceListScrollbar { grab_row_offset },
                                });
                            }
                            ScrollbarClickTarget::Track { offset_from_bottom } => {
                                self.set_workspace_list_offset_from_bottom(offset_from_bottom);
                            }
                        }
                        return None;
                    }

                    let (cards, _) =
                        crate::ui::compute_sidebar_row_areas(self, self.view.sidebar_rect);
                    let agent_counts = crate::ui::agent_counts_by_workspace(
                        &crate::ui::sidebar_thread_entries(self),
                    );
                    if let Some(card) = cards.iter().find(|card| {
                        let chevron = crate::ui::workspace_agent_chevron_rect(
                            self,
                            card,
                            agent_counts.contains_key(&card.ws_idx),
                        );
                        mouse.row == chevron.y && mouse.column == chevron.x && chevron.width > 0
                    }) {
                        self.toggle_workspace_agent_disclosure(card.ws_idx);
                        return None;
                    }
                    // Headers are tested before spaces: a header row owns its
                    // whole width, so anywhere on it folds the group.
                    if let Some(title) = self.sidebar_section_header_at(mouse.row) {
                        self.toggle_sidebar_group(title);
                        return None;
                    }
                    if let Some(key) = crate::ui::sidebar_nested_header_at(self, mouse.row) {
                        self.sidebar_selected_work_group =
                            crate::ui::sidebar_object_at(self, mouse.row);
                        self.toggle_sidebar_group(&key);
                        return None;
                    }
                    if let Some(key) =
                        crate::ui::sidebar_unassigned_spawn_at(self, mouse.column, mouse.row)
                    {
                        self.sidebar_selected_work_group = None;
                        match self.sidebar_unassigned_dispatch_plan(&key) {
                            Ok(plan) => {
                                return Some(MouseAction::DispatchSidebarWork(Box::new(plan)));
                            }
                            Err(error) => self.config_diagnostic = Some(error),
                        }
                        return None;
                    }
                    if crate::ui::sidebar_show_more_at(self, mouse.row) {
                        self.sidebar_unassigned_expanded_views
                            .insert(self.sidebar_group_mode);
                        self.sidebar_selected_work_group = None;
                        self.workspace_scroll = crate::ui::normalized_workspace_scroll(
                            self,
                            self.view.sidebar_rect,
                            self.workspace_scroll,
                        );
                        return None;
                    }
                    if let Some(key) = crate::ui::sidebar_dim_header_at(self, mouse.row) {
                        if key.starts_with("repo:") {
                            self.sidebar_selected_work_group = Some(key);
                        } else {
                            self.sidebar_selected_work_group = None;
                            if !self.open_sidebar_unassigned_object(&key) {
                                self.config_diagnostic =
                                    Some("unassigned object is no longer available".to_string());
                            }
                        }
                        return None;
                    }
                    if let Some(index) = crate::ui::sidebar_symphony_job_at(self, mouse.row) {
                        return Some(MouseAction::OpenSymphonyWorkflow { index });
                    }
                    if let Some(idx) = self.workspace_at_row(mouse.row) {
                        self.workspace_presses.insert(
                            source_id,
                            WorkspacePressState {
                                ws_idx: idx,
                                start_col: mouse.column,
                                start_row: mouse.row,
                            },
                        );
                        return None;
                    }

                    if let Some(target) = self.sidebar_settled_target_at(mouse.row) {
                        self.sidebar_selected_work_group = None;
                        self.sidebar_selected_settled = Some(target);
                        self.mode = Mode::Navigate;
                        return None;
                    }

                    if let Some((ws_idx, tab_idx)) = self.tab_target_at(mouse.row) {
                        self.selected = ws_idx;
                        self.mode = Mode::Terminal;
                        return Some(MouseAction::FocusSidebarTab { ws_idx, tab_idx });
                    }

                    if let Some((ws_idx, _tab_idx, pane_id)) =
                        self.agent_detail_target_at(mouse.row)
                    {
                        self.mode = Mode::Terminal;
                        return Some(MouseAction::FocusPane { ws_idx, pane_id });
                    }
                } else if let Some(info) = self.pane_at(mouse.column, mouse.row).cloned() {
                    if self.mode != Mode::Terminal {
                        self.mode = Mode::Terminal;
                    }
                    self.note_pane_activity_at(info.id, std::time::Instant::now());
                    // Clicking pane content aims the keyboard at the shell, and
                    // it reaches here even when that pane already held focus, so
                    // the surface flags are dropped here rather than only on a
                    // focus change.
                    self.release_surface_focus_to_pane();

                    if self.forward_pane_mouse_button(terminal_runtimes, &info, mouse) {
                        self.selection = None;
                        self.selection_autoscroll = None;
                        return self.mouse_pane_focus_action(info.id);
                    }

                    let (row, col) = (
                        mouse.row - info.inner_rect.y,
                        mouse.column - info.inner_rect.x,
                    );
                    self.selection = Some(Selection::anchor(
                        info.id,
                        row,
                        col,
                        self.pane_scroll_metrics(terminal_runtimes, info.id),
                    ));
                    return self.mouse_pane_focus_action(info.id);
                } else if let Some(info) = self.view.pane_infos.iter().find(|p| {
                    mouse.column >= p.rect.x
                        && mouse.column < p.rect.x + p.rect.width
                        && mouse.row >= p.rect.y
                        && mouse.row < p.rect.y + p.rect.height
                }) {
                    let id = info.id;
                    if self.mode != Mode::Terminal {
                        self.mode = Mode::Terminal;
                    }
                    self.release_surface_focus_to_pane();
                    return self.mouse_pane_focus_action(id);
                }
            }

            MouseEventKind::Drag(MouseButton::Left) => {
                if self.selection.is_some() {
                    self.update_selection_drag(terminal_runtimes, mouse.column, mouse.row);
                    return None;
                }

                if (self.drag.is_none() || self.chrome_drag_owned_by_other(source_id))
                    && !self.chrome_press_pending(source_id)
                {
                    if let Some(info) = self.pane_mouse_target(mouse.column, mouse.row).cloned() {
                        if self.forward_pane_mouse_button(terminal_runtimes, &info, mouse) {
                            self.selection = None;
                            self.selection_autoscroll = None;
                            return None;
                        }
                    }
                }

                let workspace_drop_target = self.workspace_drop_target_at_row(mouse.row);
                let tab_drop_index = self.tab_drop_index_at(mouse.column, mouse.row);
                if self.drag.is_none() {
                    if let Some(press) = self.workspace_presses.get(&source_id) {
                        let delta_col = mouse.column.abs_diff(press.start_col);
                        let delta_row = mouse.row.abs_diff(press.start_row);
                        let can_reorder = self.workspaces.get(press.ws_idx).is_some_and(|ws| {
                            ws.worktree_space()
                                .is_none_or(|space| !space.is_linked_worktree)
                        });
                        if workspace_drop_target.is_some()
                            && can_reorder
                            && delta_col.max(delta_row) >= WORKSPACE_DRAG_THRESHOLD
                        {
                            self.drag = Some(DragState {
                                target: DragTarget::WorkspaceReorder {
                                    source_id,
                                    source_ws_idx: press.ws_idx,
                                    drop_target: workspace_drop_target,
                                },
                            });
                        }
                    } else if let Some(press) = self.tab_presses.get(&source_id) {
                        let delta_col = mouse.column.abs_diff(press.start_col);
                        let delta_row = mouse.row.abs_diff(press.start_row);
                        // Require a real drop target before opening a reorder,
                        // so a report from off the tab bar cannot start a drag
                        // that has nowhere to land.
                        if tab_drop_index.is_some()
                            && delta_col.max(delta_row) >= TAB_DRAG_THRESHOLD
                        {
                            self.drag = Some(DragState {
                                target: DragTarget::TabReorder {
                                    source_id,
                                    ws_idx: press.ws_idx,
                                    source_tab_idx: press.tab_idx,
                                    insert_idx: tab_drop_index,
                                },
                            });
                        }
                    }
                }

                if let Some(DragState {
                    target:
                        DragTarget::WorkspaceReorder {
                            source_id: drag_source_id,
                            drop_target,
                            ..
                        },
                }) = &mut self.drag
                {
                    if *drag_source_id == source_id {
                        *drop_target = workspace_drop_target;
                    }
                } else if let Some(DragState {
                    target:
                        DragTarget::TabReorder {
                            source_id: drag_source_id,
                            ws_idx,
                            insert_idx,
                            ..
                        },
                }) = &mut self.drag
                {
                    if *drag_source_id == source_id && self.active == Some(*ws_idx) {
                        *insert_idx = tab_drop_index;
                    }
                } else if let Some(drag) = &self.drag {
                    match &drag.target {
                        DragTarget::WorkspaceReorder { .. } | DragTarget::TabReorder { .. } => {}
                        DragTarget::WorkspaceListScrollbar { grab_row_offset } => {
                            if let Some(offset_from_bottom) =
                                self.workspace_list_offset_for_drag_row(mouse.row, *grab_row_offset)
                            {
                                self.set_workspace_list_offset_from_bottom(offset_from_bottom);
                            }
                        }
                        DragTarget::PaneSplit {
                            path,
                            direction,
                            area,
                            grab_offset,
                        } => {
                            let ratio = match direction {
                                Direction::Horizontal => {
                                    (mouse
                                        .column
                                        .saturating_add(*grab_offset)
                                        .saturating_sub(area.x))
                                        as f32
                                        / area.width.max(1) as f32
                                }
                                Direction::Vertical => {
                                    (mouse
                                        .row
                                        .saturating_add(*grab_offset)
                                        .saturating_sub(area.y))
                                        as f32
                                        / area.height.max(1) as f32
                                }
                            };
                            let ratio = ratio.clamp(0.1, 0.9);
                            let path = path.clone();
                            return Some(MouseAction::SetSplitRatio { path, ratio });
                        }
                        DragTarget::PaneScrollbar {
                            pane_id,
                            grab_row_offset,
                        } => {
                            if let Some(offset_from_bottom) = self.scrollbar_offset_for_pane_row(
                                terminal_runtimes,
                                *pane_id,
                                mouse.row,
                                *grab_row_offset,
                            ) {
                                self.set_pane_scroll_offset(
                                    terminal_runtimes,
                                    *pane_id,
                                    offset_from_bottom,
                                );
                            }
                        }
                        DragTarget::SidebarDivider => {
                            self.set_manual_sidebar_width(mouse.column);
                        }
                        DragTarget::DockDivider => {
                            self.set_manual_dock_width(mouse.column);
                        }
                        DragTarget::ReleaseNotesScrollbar { .. }
                        | DragTarget::ProductAnnouncementScrollbar { .. }
                        | DragTarget::KeybindHelpScrollbar { .. } => {}
                    }
                }
            }

            MouseEventKind::Up(MouseButton::Left) => {
                // Mouse-up either finishes a drag selection or releases after a
                // double-click word selection; the latter is already finalized.
                if let Some(selection) = self.selection.as_ref() {
                    let was_click = selection.was_just_click();
                    let was_finalized = selection.is_finalized();

                    self.clear_chrome_press(source_id);
                    self.drag = None;
                    self.selection_autoscroll = None;
                    if was_click {
                        self.selection = None;
                    } else if was_finalized {
                        // Double-click already finalized this word selection.
                    } else if self.copy_on_select {
                        self.copy_selection(terminal_runtimes);
                    } else if let Some(selection) = self.selection.as_mut() {
                        selection.finish();
                    }
                    return None;
                }

                let foreign_chrome_drag = self.chrome_drag_owned_by_other(source_id);
                if (self.drag.is_none() || foreign_chrome_drag)
                    && !self.chrome_press_pending(source_id)
                {
                    if let Some(info) = self.pane_mouse_target(mouse.column, mouse.row).cloned() {
                        if self.forward_pane_mouse_button(terminal_runtimes, &info, mouse) {
                            self.selection = None;
                            self.selection_autoscroll = None;
                            return None;
                        }
                    }
                }

                let workspace_press = self.workspace_presses.remove(&source_id);
                let tab_press = self.tab_presses.remove(&source_id);
                if foreign_chrome_drag {
                    return self.chrome_press_action(workspace_press, tab_press);
                }

                match self.drag.take() {
                    Some(DragState {
                        target:
                            DragTarget::WorkspaceReorder {
                                source_ws_idx,
                                drop_target: Some(drop_target),
                                ..
                            },
                    }) => {
                        if let Some(params) =
                            self.workspace_move_block_params(source_ws_idx, drop_target)
                        {
                            if self
                                .workspaces
                                .get(source_ws_idx)
                                .is_some_and(|workspace| workspace.worktree_space().is_some())
                            {
                                return Some(MouseAction::MoveWorkspaceBlock { params });
                            }
                            let insert_idx = params
                                .before_workspace_id
                                .as_ref()
                                .and_then(|id| {
                                    self.workspaces
                                        .iter()
                                        .position(|workspace| workspace.id == *id)
                                })
                                .unwrap_or(self.workspaces.len());
                            return Some(MouseAction::MoveWorkspace {
                                source_ws_idx,
                                insert_idx,
                            });
                        }
                    }
                    Some(DragState {
                        target:
                            DragTarget::TabReorder {
                                ws_idx,
                                source_tab_idx,
                                insert_idx: Some(insert_idx),
                                ..
                            },
                    }) => {
                        if self.active == Some(ws_idx) {
                            self.mode = Mode::Terminal;
                            return Some(MouseAction::MoveTab {
                                ws_idx,
                                source_tab_idx,
                                insert_idx,
                            });
                        }
                    }
                    Some(_) => {}
                    None => return self.chrome_press_action(workspace_press, tab_press),
                }
            }

            MouseEventKind::Up(MouseButton::Middle) | MouseEventKind::Drag(MouseButton::Middle)
                if !in_sidebar && !in_dock =>
            {
                if let Some(info) = self.pane_mouse_target(mouse.column, mouse.row).cloned() {
                    let _ = self.forward_pane_mouse_button(terminal_runtimes, &info, mouse);
                }
            }

            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if self.mode_bar_covers_tab_row(mouse.column, mouse.row) => {}

            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if self.on_tab_bar(mouse.column, mouse.row) =>
            {
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        if let Some(ws) = self.active.and_then(|i| self.workspaces.get(i)) {
                            if !ws.tabs.is_empty() {
                                let prev = if ws.active_tab == 0 {
                                    ws.tabs.len() - 1
                                } else {
                                    ws.active_tab - 1
                                };
                                return Some(MouseAction::FocusTab { tab_idx: prev });
                            }
                        }
                    }
                    MouseEventKind::ScrollDown => {
                        if let Some(ws) = self.active.and_then(|i| self.workspaces.get(i)) {
                            if !ws.tabs.is_empty() {
                                let next = (ws.active_tab + 1) % ws.tabs.len();
                                return Some(MouseAction::FocusTab { tab_idx: next });
                            }
                        }
                    }
                    _ => {}
                }
            }

            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if !in_sidebar
                    && !in_dock
                    && self.scroll_selection_with_wheel(terminal_runtimes, mouse) => {}

            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if !in_sidebar && !in_dock => {
                self.selection = None;
                self.selection_autoscroll = None;
                self.handle_terminal_wheel(terminal_runtimes, mouse);
            }

            MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight
                if self.mode == Mode::Terminal && !in_sidebar && !in_dock =>
            {
                if let Some(info) = self.pane_at(mouse.column, mouse.row).cloned() {
                    self.forward_pane_reported_wheel(terminal_runtimes, &info, mouse);
                }
            }

            MouseEventKind::ScrollUp if in_sidebar => {
                if !self.scroll_collapsed_sidebar(-1) {
                    if crate::ui::should_show_scrollbar(crate::ui::workspace_list_scroll_metrics(
                        self,
                        self.workspace_list_rect(),
                    )) {
                        self.scroll_workspace_list(-1);
                    } else if self.sidebar_shows_spaces_tree() {
                        self.move_selected_workspace_by_visible_delta(-1);
                    }
                }
            }
            MouseEventKind::ScrollDown if in_sidebar => {
                if !self.scroll_collapsed_sidebar(1) {
                    if crate::ui::should_show_scrollbar(crate::ui::workspace_list_scroll_metrics(
                        self,
                        self.workspace_list_rect(),
                    )) {
                        self.scroll_workspace_list(1);
                    } else if self.sidebar_shows_spaces_tree() {
                        self.move_selected_workspace_by_visible_delta(1);
                    }
                }
            }
            MouseEventKind::ScrollUp
                if in_dock && dock_body_contains(self, mouse.column, mouse.row) =>
            {
                if self.dock_tab == Some(crate::app::DockSurface::Agents) {
                    self.scroll_dock_agents(-3);
                } else if let Some(key) = match self.dock_tab {
                    Some(crate::app::DockSurface::Pr) => crate::ui::dock::pr::focused_pr_key(self),
                    Some(crate::app::DockSurface::Linear) => {
                        crate::ui::dock::linear::focused_ticket_key(self)
                    }
                    _ => None,
                } {
                    let view = self.dock_object_views.entry(key).or_default();
                    view.scroll = view.scroll.saturating_sub(3);
                } else {
                    self.dock_scroll = self.dock_scroll.saturating_sub(3);
                }
            }
            MouseEventKind::ScrollDown
                if in_dock && dock_body_contains(self, mouse.column, mouse.row) =>
            {
                if self.dock_tab == Some(crate::app::DockSurface::Agents) {
                    self.scroll_dock_agents(3);
                } else if let Some(key) = match self.dock_tab {
                    Some(crate::app::DockSurface::Pr) => crate::ui::dock::pr::focused_pr_key(self),
                    Some(crate::app::DockSurface::Linear) => {
                        crate::ui::dock::linear::focused_ticket_key(self)
                    }
                    _ => None,
                } {
                    let view = self.dock_object_views.entry(key).or_default();
                    view.scroll = view.scroll.saturating_add(3);
                } else {
                    self.dock_scroll = self.dock_scroll.saturating_add(3);
                }
            }

            MouseEventKind::Moved if self.mode == Mode::ContextMenu => {
                let hovered = self.context_menu_item_at(mouse.column, mouse.row);
                if let Some(menu) = &mut self.context_menu {
                    menu.list.hover(hovered);
                }
            }

            MouseEventKind::Moved if self.mode == Mode::Terminal && !in_sidebar && !in_dock => {
                if let Some(info) = self.pane_at(mouse.column, mouse.row).cloned() {
                    let _ = self.forward_pane_mouse_motion(terminal_runtimes, &info, mouse);
                }
            }

            MouseEventKind::Down(MouseButton::Right) if in_sidebar && !self.sidebar_collapsed => {
                self.clear_chrome_press(source_id);
                if self
                    .workspace_list_scrollbar_target_at(mouse.column, mouse.row)
                    .is_some()
                {
                    return None;
                }
                if let Some(idx) = self.workspace_at_row(mouse.row) {
                    self.selected = idx;
                    let kind = self
                        .workspaces
                        .get(idx)
                        .and_then(|ws| {
                            let group_state = crate::ui::workspace_parent_group_state(self, idx);
                            let git_space = ws.git_space().cloned().or_else(|| {
                                ws.resolved_identity_cwd_from(&self.terminals, terminal_runtimes)
                                    .as_deref()
                                    .and_then(crate::workspace::git_space_metadata)
                            });
                            let is_linked_worktree = ws.worktree_space().map_or_else(
                                || {
                                    git_space
                                        .as_ref()
                                        .is_some_and(|space| space.is_linked_worktree)
                                },
                                |space| space.is_linked_worktree,
                            );
                            let show_git_menu = ws.worktree_space().is_some()
                                || git_space
                                    .as_ref()
                                    .is_some_and(|space| !space.is_linked_worktree);
                            show_git_menu.then_some(ContextMenuKind::GitWorkspace {
                                ws_idx: idx,
                                is_linked_worktree,
                                has_worktree_children: group_state.is_some(),
                                collapsed: group_state
                                    .as_ref()
                                    .is_some_and(|(_, collapsed)| *collapsed),
                            })
                        })
                        .unwrap_or(ContextMenuKind::Workspace { ws_idx: idx });
                    self.context_menu = Some(ContextMenuState {
                        kind,
                        x: mouse.column,
                        y: mouse.row,
                        list: MenuListState::new(0),
                    });
                    self.mode = Mode::ContextMenu;
                }
            }

            MouseEventKind::Down(MouseButton::Right)
                if !self.mode_bar_covers_tab_row(mouse.column, mouse.row)
                    && self.tab_at(mouse.column, mouse.row).is_some() =>
            {
                if let (Some(ws_idx), Some(tab_idx)) =
                    (self.active, self.tab_at(mouse.column, mouse.row))
                {
                    self.context_menu = Some(ContextMenuState {
                        kind: ContextMenuKind::Tab { ws_idx, tab_idx },
                        x: mouse.column,
                        y: mouse.row,
                        list: MenuListState::new(0),
                    });
                    self.mode = Mode::ContextMenu;
                }
            }

            MouseEventKind::Down(MouseButton::Right) if !in_sidebar && !in_dock => {
                if let Some(info) = self.pane_mouse_target(mouse.column, mouse.row).cloned() {
                    let ws_idx = self.active?;
                    let tab_idx = self
                        .workspaces
                        .get(ws_idx)
                        .map(|ws| ws.active_tab_index())?;
                    let previous_focused_pane_id = self
                        .workspaces
                        .get(ws_idx)
                        .and_then(|ws| ws.focused_pane_id());
                    let source_pane_id =
                        previous_focused_pane_id.filter(|pane_id| *pane_id != info.id);
                    let pane_state = self
                        .workspaces
                        .get(ws_idx)
                        .and_then(|ws| ws.pane_state(info.id));
                    let has_manual_label = pane_state
                        .and_then(|pane| self.terminals.get(&pane.attached_terminal_id))
                        .and_then(|terminal| terminal.manual_label.as_ref())
                        .is_some();
                    let right_click_passthrough =
                        pane_state.is_some_and(|pane| pane.right_click_passthrough);
                    let linkable_work_link = self.linkable_work_link_at(
                        terminal_runtimes,
                        &info,
                        mouse.column,
                        mouse.row,
                        ws_idx,
                        tab_idx,
                    );
                    self.context_menu = Some(ContextMenuState {
                        kind: ContextMenuKind::Pane {
                            ws_idx,
                            tab_idx,
                            pane_id: info.id,
                            source_pane_id,
                            has_manual_label,
                            right_click_passthrough,
                            linkable_work_link,
                        },
                        x: mouse.column,
                        y: mouse.row,
                        list: MenuListState::new(0),
                    });
                    self.mode = Mode::ContextMenu;
                }
            }

            _ => {}
        }

        None
    }

    fn handle_mobile_mouse(&mut self, mouse: MouseEvent) -> MobileMouseResult {
        if self.mode == Mode::Navigate {
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    self.scroll_mobile_switcher_at(mouse.column, mouse.row, -1);
                    return MobileMouseResult::Consumed;
                }
                MouseEventKind::ScrollDown => {
                    self.scroll_mobile_switcher_at(mouse.column, mouse.row, 1);
                    return MobileMouseResult::Consumed;
                }
                MouseEventKind::Down(MouseButton::Left) => {}
                _ => return MobileMouseResult::Consumed,
            }
        } else if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return MobileMouseResult::Ignored;
        }

        if self.mode != Mode::Navigate {
            if !matches!(self.mode, Mode::Terminal | Mode::Resize) {
                return MobileMouseResult::Ignored;
            }
            if rect_contains(self.view.mobile_menu_hit_area, mouse.column, mouse.row) {
                self.begin_workspace_picker_presentation();
                self.mode = Mode::Navigate;
                return MobileMouseResult::Consumed;
            }
            return MobileMouseResult::Ignored;
        }

        let areas = crate::ui::mobile_switcher_areas(self);
        if rect_contains(areas.close, mouse.column, mouse.row) {
            self.close_workspace_picker();
            return MobileMouseResult::Consumed;
        }

        match crate::ui::mobile_switcher_target_at(self, mouse.column, mouse.row) {
            Some(crate::ui::MobileSwitcherTarget::Section(title)) => {
                self.toggle_sidebar_group(title);
            }
            Some(crate::ui::MobileSwitcherTarget::NewWorkspace) => {
                return MobileMouseResult::Action(MouseAction::NewWorkspace);
            }
            Some(crate::ui::MobileSwitcherTarget::Workspace(ws_idx)) => {
                self.close_workspace_picker();
                return MobileMouseResult::Action(MouseAction::FocusWorkspace { ws_idx });
            }
            Some(crate::ui::MobileSwitcherTarget::WorkspaceDisclosure(ws_idx)) => {
                self.toggle_workspace_agent_disclosure(ws_idx);
            }
            Some(crate::ui::MobileSwitcherTarget::NewTab) => {
                if self.prompt_new_tab_name {
                    open_new_tab_dialog(self);
                } else {
                    self.request_new_tab = true;
                    self.close_workspace_picker();
                }
            }
            Some(crate::ui::MobileSwitcherTarget::Tab(tab_idx)) => {
                self.close_workspace_picker();
                return MobileMouseResult::Action(MouseAction::FocusTab { tab_idx });
            }
            Some(crate::ui::MobileSwitcherTarget::SidebarTab { ws_idx, tab_idx }) => {
                self.close_workspace_picker();
                return MobileMouseResult::Action(MouseAction::FocusSidebarTab { ws_idx, tab_idx });
            }
            Some(crate::ui::MobileSwitcherTarget::Agent {
                ws_idx,
                tab_idx: _,
                pane_id,
            }) => {
                self.close_workspace_picker();
                return MobileMouseResult::Action(MouseAction::FocusPane { ws_idx, pane_id });
            }
            Some(crate::ui::MobileSwitcherTarget::Menu(action_idx)) => {
                let actions = global_menu_actions(self);
                if let Some(action) = actions.get(action_idx).copied() {
                    apply_global_menu_action(self, action);
                }
            }
            None => {}
        }

        MobileMouseResult::Consumed
    }

    fn close_workspace_picker(&mut self) {
        self.end_workspace_picker_presentation();
        self.mode = Mode::Terminal;
    }

    fn scroll_mobile_switcher_at(&mut self, _col: u16, _row: u16, delta: i16) {
        let max_scroll = crate::ui::mobile_switcher_max_scroll(self);
        apply_scroll(
            &mut self.mobile_switcher_scroll,
            delta.saturating_mul(2),
            max_scroll,
        );
    }

    pub(super) fn screen_rect(&self) -> Rect {
        if self.view.layout == ViewLayout::Mobile {
            self.view.mobile_header_rect.union(self.view.terminal_area)
        } else {
            self.view
                .status_bar_rect
                .union(self.view.sidebar_rect)
                .union(self.view.terminal_area)
                .union(self.view.dock_rect)
        }
    }

    pub(crate) fn context_menu_rect(&self) -> Option<Rect> {
        let menu = self.context_menu.as_ref()?;
        let screen = self.screen_rect();
        let max_item_w = menu
            .items()
            .iter()
            .map(|item| item.len() as u16)
            .max()
            .unwrap_or(0);
        let menu_w = (max_item_w + 4).max(14).min(screen.width.max(1));
        let menu_h = (menu.items().len() as u16 + 2).min(screen.height.max(1));
        let x = menu.x.min(screen.x + screen.width.saturating_sub(menu_w));
        let y = menu.y.min(screen.y + screen.height.saturating_sub(menu_h));
        Some(Rect::new(x, y, menu_w, menu_h))
    }

    pub(crate) fn confirm_close_rect(&self) -> Rect {
        crate::ui::confirm_close_popup_rect(self.view.terminal_area).unwrap_or_default()
    }

    fn context_menu_item_at(&self, col: u16, row: u16) -> Option<usize> {
        let menu_rect = self.context_menu_rect()?;
        let inner_x = menu_rect.x + 1;
        let inner_y = menu_rect.y + 1;
        let inner_w = menu_rect.width.saturating_sub(2);
        let inner_h = menu_rect.height.saturating_sub(2);
        let item_count = self
            .context_menu
            .as_ref()
            .map(|menu| menu.items().len() as u16)
            .unwrap_or(0);
        if col >= inner_x
            && col < inner_x + inner_w
            && row >= inner_y
            && row < inner_y + inner_h.min(item_count)
        {
            Some((row - inner_y) as usize)
        } else {
            None
        }
    }

    pub(super) fn tab_at(&self, col: u16, row: u16) -> Option<usize> {
        self.view
            .tab_hit_areas
            .iter()
            .enumerate()
            .find_map(|(idx, area)| {
                (area.width > 0
                    && row >= area.y
                    && row < area.y + area.height
                    && col >= area.x
                    && col < area.x + area.width)
                    .then_some(idx)
            })
    }

    fn mode_bar_covers_tab_row(&self, col: u16, row: u16) -> bool {
        self.tab_bar_position == crate::config::TabBarPositionConfig::Bottom
            && matches!(
                self.mode,
                Mode::Navigate | Mode::Prefix | Mode::Copy | Mode::Resize
            )
            && self.on_tab_bar(col, row)
    }

    /// The pin glyph is the tab cell's leading pad column. Keeping it to that
    /// one column means a click anywhere else on the tab still does what it
    /// always did: focus the tab.
    pub(super) fn tab_pin_glyph_at(&self, col: u16, row: u16) -> Option<usize> {
        let idx = self.tab_at(col, row)?;
        let rect = self.view.tab_hit_areas.get(idx)?;
        (rect.width > 1 && col == rect.x).then_some(idx)
    }

    /// The section whose header sits under this screen row in a detail drawn by
    /// `surface`, accounting for the scroll offset the render applies. The
    /// surface is passed in because the same renderers back the dock tab and the
    /// collapsed-dock preview, which resolve it differently.
    fn dock_detail_section_at(
        &self,
        surface: crate::app::DockSurface,
        column: u16,
        row: u16,
    ) -> Option<(
        crate::app::state::WorkItemKey,
        crate::app::state::DetailSection,
    )> {
        // Rebuilding a detail layout is proportional to the detail's size, so
        // the surface check comes first: a click on any other surface costs a
        // single comparison.
        if !matches!(
            surface,
            crate::app::DockSurface::Pr | crate::app::DockSurface::Linear
        ) {
            return None;
        }
        let area = super::dock_detail_area(self);
        if !self.point_in_rect(area, column, row) {
            return None;
        }
        let (object_key, layout) = match surface {
            crate::app::DockSurface::Pr => (
                crate::ui::dock::pr::focused_pr_key(self)?,
                crate::ui::dock::pr::focused_pr_layout(self, area)?,
            ),
            crate::app::DockSurface::Linear => (
                crate::ui::dock::linear::focused_ticket_key(self)?,
                crate::ui::dock::linear::focused_ticket_layout(self, area)?,
            ),
            _ => return None,
        };
        let view = self
            .dock_object_views
            .get(&object_key)
            .cloned()
            .unwrap_or_default();
        let section = super::section_at_row(&layout, &view, area, row)?;
        Some((object_key, section))
    }

    pub(super) fn point_in_rect(&self, rect: Rect, col: u16, row: u16) -> bool {
        rect.width > 0
            && rect.height > 0
            && col >= rect.x
            && col < rect.right()
            && row >= rect.y
            && row < rect.bottom()
    }

    /// Queue index of the home row under the pointer.
    ///
    /// Empty hit areas mean home is closed, so this needs no separate check.
    #[cfg(test)]
    pub(super) fn home_row_at(&self, col: u16, row: u16) -> Option<usize> {
        self.home_hit_at(col, row).and_then(|target| match target {
            HomeHitTarget::QueueRow(index) => Some(index),
            _ => None,
        })
    }

    pub(super) fn home_hit_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<crate::app::state::HomeHitTarget> {
        let hit = self.view.home_hit_areas.iter().find_map(|hit| {
            (hit.rect.width > 0
                && hit.rect.height > 0
                && row >= hit.rect.y
                && row < hit.rect.y.saturating_add(hit.rect.height)
                && col >= hit.rect.x
                && col < hit.rect.x.saturating_add(hit.rect.width))
            .then_some(hit.target)
        });
        hit.or_else(|| {
            self.view
                .home_row_hit_areas
                .iter()
                .rev()
                .find_map(|(index, area)| {
                    (area.width > 0
                        && area.height > 0
                        && row >= area.y
                        && row < area.y.saturating_add(area.height)
                        && col >= area.x
                        && col < area.x.saturating_add(area.width))
                    .then_some(HomeHitTarget::QueueRow(*index))
                })
        })
    }

    pub(super) fn on_tab_bar(&self, col: u16, row: u16) -> bool {
        let area = self.view.tab_bar_rect;
        area.width > 0
            && row >= area.y
            && row < area.y + area.height
            && col >= area.x
            && col < area.x + area.width
    }

    pub(super) fn on_git_menu_button(&self, col: u16, row: u16) -> bool {
        let area = self.view.git_menu_button_hit_area;
        area.width > 0
            && row >= area.y
            && row < area.bottom()
            && col >= area.x
            && col < area.right()
    }

    pub(super) fn git_menu_row_at(&self, col: u16, row: u16) -> Option<usize> {
        self.view
            .git_menu_row_hit_areas
            .iter()
            .position(|area| {
                area.width > 0
                    && row >= area.y
                    && row < area.bottom()
                    && col >= area.x
                    && col < area.right()
            })
            .map(|offset| self.view.git_menu_first_visible + offset)
    }

    pub(super) fn on_tab_scroll_left_button(&self, col: u16, row: u16) -> bool {
        let area = self.view.tab_scroll_left_hit_area;
        area.width > 0
            && row >= area.y
            && row < area.y + area.height
            && col >= area.x
            && col < area.x + area.width
    }

    pub(super) fn on_tab_scroll_right_button(&self, col: u16, row: u16) -> bool {
        let area = self.view.tab_scroll_right_hit_area;
        area.width > 0
            && row >= area.y
            && row < area.y + area.height
            && col >= area.x
            && col < area.x + area.width
    }

    pub(super) fn tab_drop_index_at(&self, col: u16, row: u16) -> Option<usize> {
        if !self.on_tab_bar(col, row) {
            return None;
        }

        let visible_tabs: Vec<_> = self
            .view
            .tab_hit_areas
            .iter()
            .enumerate()
            .filter(|(_, rect)| rect.width > 0)
            .collect();
        let (first_idx, first_rect) = *visible_tabs.first()?;
        let (last_idx, last_rect) = *visible_tabs.last()?;

        if self.on_tab_scroll_left_button(col, row) {
            return Some(0);
        }
        if self.on_tab_scroll_right_button(col, row) {
            return self
                .active
                .and_then(|idx| self.workspaces.get(idx))
                .map(|ws| ws.tabs.len());
        }

        let left_edge = if first_idx == 0 {
            first_rect.x
        } else {
            self.view.tab_scroll_left_hit_area.x + self.view.tab_scroll_left_hit_area.width
        };
        let right_edge = if self
            .active
            .and_then(|idx| self.workspaces.get(idx))
            .is_some_and(|ws| last_idx + 1 >= ws.tabs.len())
        {
            last_rect.x + last_rect.width
        } else {
            self.view.tab_scroll_right_hit_area.x.saturating_sub(1)
        };

        if col <= left_edge {
            return Some(first_idx);
        }
        if col >= right_edge {
            return Some(last_idx + 1);
        }

        for (idx, rect) in visible_tabs {
            let midpoint = rect.x + rect.width / 2;
            if col < midpoint {
                return Some(idx);
            }
            if col < rect.x + rect.width {
                return Some(idx + 1);
            }
        }

        Some(last_idx + 1)
    }

    pub(super) fn pane_toggle_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<crate::app::state::PaneToggleDirection> {
        [
            (
                self.view.pane_toggle_below_hit_area,
                crate::app::state::PaneToggleDirection::Below,
            ),
            (
                self.view.pane_toggle_right_hit_area,
                crate::app::state::PaneToggleDirection::Right,
            ),
        ]
        .into_iter()
        .find_map(|(area, direction)| {
            (area.width > 0
                && row >= area.y
                && row < area.y + area.height
                && col >= area.x
                && col < area.x + area.width)
                .then_some(direction)
        })
    }

    pub(super) fn on_new_tab_button(&self, col: u16, row: u16) -> bool {
        let area = self.view.new_tab_hit_area;
        area.width > 0
            && row >= area.y
            && row < area.y + area.height
            && col >= area.x
            && col < area.x + area.width
    }

    pub(super) fn find_border_at(&self, col: u16, row: u16) -> Option<&SplitBorder> {
        self.view.split_borders.iter().find(|b| match b.direction {
            Direction::Horizontal if self.pane_borders && !self.pane_gaps => {
                col == b.pos && row >= b.area.y && row < b.area.y + b.area.height
            }
            Direction::Horizontal if self.pane_borders && self.pane_gaps => {
                row >= b.area.y
                    && row < b.area.y + b.area.height
                    && col >= b.pos.saturating_sub(1)
                    && col <= b.pos
            }
            Direction::Horizontal if !self.pane_borders && self.pane_gaps => {
                row >= b.area.y
                    && row < b.area.y + b.area.height
                    && b.pos.checked_sub(1).is_some_and(|gap_col| {
                        col == gap_col && self.pane_frame_at(col, row).is_none()
                    })
            }
            Direction::Vertical if self.pane_borders && !self.pane_gaps => {
                row == b.pos && col >= b.area.x && col < b.area.x + b.area.width
            }
            Direction::Vertical if self.pane_borders && self.pane_gaps => {
                col >= b.area.x
                    && col < b.area.x + b.area.width
                    && row >= b.pos.saturating_sub(1)
                    && row <= b.pos
            }
            Direction::Vertical if !self.pane_borders && self.pane_gaps => {
                col >= b.area.x
                    && col < b.area.x + b.area.width
                    && b.pos.checked_sub(1).is_some_and(|gap_row| {
                        row == gap_row && self.pane_frame_at(col, row).is_none()
                    })
            }
            _ => false,
        })
    }

    pub(super) fn pane_at(&self, col: u16, row: u16) -> Option<&PaneInfo> {
        self.view.pane_infos.iter().find(|p| {
            col >= p.inner_rect.x
                && col < p.inner_rect.x + p.inner_rect.width
                && row >= p.inner_rect.y
                && row < p.inner_rect.y + p.inner_rect.height
        })
    }

    /// Work link under a pane click, resolved to the action worth offering.
    ///
    /// A link every pane declares offers the way back out. One the window
    /// carries partially, or not at all, offers the binding that completes it.
    /// A link only the hook or git tier observed offers nothing, because a
    /// declaration cannot remove an observation and the entry would lie.
    pub(super) fn linkable_work_link_at(
        &self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        info: &PaneInfo,
        col: u16,
        row: u16,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<PaneMenuWorkLinkAction> {
        if col < info.inner_rect.x || row < info.inner_rect.y {
            return None;
        }
        let url = self.url_at_pane_cell(
            terminal_runtimes,
            info.id,
            row - info.inner_rect.y,
            col - info.inner_rect.x,
        )?;
        let link = crate::work_context::extract_pr_urls(&url)
            .into_iter()
            .next()
            .map(PaneMenuWorkLink::PullRequest)
            .or_else(|| {
                crate::work_context::extract_ticket_ids(&url)
                    .into_iter()
                    .next()
                    .map(PaneMenuWorkLink::Ticket)
            })?;
        let panes = self.window_pane_ids(ws_idx, tab_idx);
        let mut declared_anywhere = false;
        let mut declared_everywhere = true;
        let mut bound_everywhere = true;
        for pane_id in panes {
            let terminal = self
                .workspaces
                .get(ws_idx)
                .and_then(|ws| ws.pane_state(pane_id))
                .and_then(|pane| self.terminals.get(&pane.attached_terminal_id));
            let Some(terminal) = terminal else {
                bound_everywhere = false;
                declared_everywhere = false;
                continue;
            };
            let declared = link.is_bound_in(terminal.manual_work_context());
            declared_anywhere |= declared;
            declared_everywhere &= declared;
            bound_everywhere &= link.is_bound_in(terminal.effective_work_context());
        }
        if declared_everywhere {
            Some(PaneMenuWorkLinkAction::unlink(link))
        } else if !bound_everywhere || declared_anywhere {
            Some(PaneMenuWorkLinkAction::link(link))
        } else {
            None
        }
    }

    /// Panes of one window, in layout order, so a window-wide binding applies
    /// deterministically.
    pub(crate) fn window_pane_ids(&self, ws_idx: usize, tab_idx: usize) -> Vec<PaneId> {
        self.workspaces
            .get(ws_idx)
            .and_then(|ws| ws.tabs.get(tab_idx))
            .map(|tab| tab.layout.pane_ids())
            .unwrap_or_default()
    }

    pub(super) fn pane_mouse_target(&self, col: u16, row: u16) -> Option<&PaneInfo> {
        self.pane_at(col, row)
            .or_else(|| self.pane_frame_at(col, row))
    }

    fn chrome_press_pending(&self, source_id: crate::app::InputSourceId) -> bool {
        self.tab_presses.contains_key(&source_id) || self.workspace_presses.contains_key(&source_id)
    }

    fn chrome_drag_owned_by_other(&self, source_id: crate::app::InputSourceId) -> bool {
        self.drag.as_ref().is_some_and(|drag| {
            matches!(
                drag.target,
                DragTarget::WorkspaceReorder {
                    source_id: drag_source_id,
                    ..
                } | DragTarget::TabReorder {
                    source_id: drag_source_id,
                    ..
                } if drag_source_id != source_id
            )
        })
    }

    fn chrome_press_action(
        &mut self,
        workspace_press: Option<WorkspacePressState>,
        tab_press: Option<TabPressState>,
    ) -> Option<MouseAction> {
        if let Some(press) = workspace_press {
            self.mode = Mode::Terminal;
            return Some(MouseAction::FocusWorkspace {
                ws_idx: press.ws_idx,
            });
        }
        if let Some(press) = tab_press {
            if self.active == Some(press.ws_idx) {
                self.mode = Mode::Terminal;
                return Some(MouseAction::FocusTab {
                    tab_idx: press.tab_idx,
                });
            }
        }
        None
    }

    pub(crate) fn clear_chrome_gesture(&mut self, source_id: crate::app::InputSourceId) {
        if self.drag.as_ref().is_some_and(|drag| {
            matches!(
                drag.target,
                DragTarget::WorkspaceReorder {
                    source_id: drag_source_id,
                    ..
                } | DragTarget::TabReorder {
                    source_id: drag_source_id,
                    ..
                } if drag_source_id == source_id
            )
        }) {
            self.drag = None;
        }
        self.clear_chrome_press(source_id);
    }

    fn clear_chrome_press(&mut self, source_id: crate::app::InputSourceId) {
        self.tab_presses.remove(&source_id);
        self.workspace_presses.remove(&source_id);
    }

    fn mouse_pane_focus_action(&self, pane_id: crate::layout::PaneId) -> Option<MouseAction> {
        let ws_idx = self.active?;
        (self
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.focused_pane_id())
            != Some(pane_id))
        .then_some(MouseAction::FocusPane { ws_idx, pane_id })
    }

    pub(crate) fn pane_info_by_id(&self, pane_id: crate::layout::PaneId) -> Option<&PaneInfo> {
        self.view.pane_infos.iter().find(|info| info.id == pane_id)
    }

    pub(super) fn pane_frame_at(&self, col: u16, row: u16) -> Option<&PaneInfo> {
        self.view.pane_infos.iter().find(|p| {
            col >= p.rect.x
                && col < p.rect.x + p.rect.width
                && row >= p.rect.y
                && row < p.rect.y + p.rect.height
        })
    }

    pub(super) fn focus_pane(&mut self, pane_id: crate::layout::PaneId) {
        let _ = pane_id;
    }

    fn clickable_toast_at(&self, col: u16, row: u16) -> bool {
        self.toast
            .as_ref()
            .is_some_and(|toast| toast.target.is_some())
            && rect_contains(self.view.toast_hit_area, col, row)
    }

    #[cfg(test)]
    pub(crate) fn focus_toast_target(&mut self) {
        let Some(target) = self.toast.as_ref().and_then(|toast| toast.target.clone()) else {
            return;
        };
        let Some(ws_idx) = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == target.workspace_id)
        else {
            return;
        };
        let Some(_tab_idx) = self.workspaces[ws_idx].find_tab_index_for_pane(target.pane_id) else {
            return;
        };

        self.focus_pane_in_workspace(ws_idx, target.pane_id);
        self.toast = None;
        self.settle_terminal_mode_after_focus();
    }

    pub(crate) fn scroll_pane_up(
        &self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        pane_id: crate::layout::PaneId,
        lines: usize,
    ) {
        if let Some(ws_idx) = self.active {
            if let Some(rt) = self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, pane_id)
            {
                rt.scroll_up(lines);
            }
        }
    }

    pub(crate) fn scroll_pane_down(
        &self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        pane_id: crate::layout::PaneId,
        lines: usize,
    ) {
        if let Some(ws_idx) = self.active {
            if let Some(rt) = self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, pane_id)
            {
                rt.scroll_down(lines);
            }
        }
    }

    pub(crate) fn pane_scroll_metrics(
        &self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        pane_id: crate::layout::PaneId,
    ) -> Option<crate::pane::ScrollMetrics> {
        self.active
            .and_then(|i| self.runtime_for_pane_in_workspace(terminal_runtimes, i, pane_id))
            .and_then(crate::terminal::TerminalRuntime::scroll_metrics)
    }

    fn handle_right_click_passthrough(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        source_id: crate::app::InputSourceId,
        mouse: MouseEvent,
        in_sidebar: bool,
    ) -> bool {
        if let Some(gesture) = self.right_click_passthrough.clone() {
            match mouse.kind {
                MouseEventKind::Drag(MouseButton::Right)
                | MouseEventKind::Up(MouseButton::Right) => {
                    let forwarded_mouse =
                        self.strip_right_click_passthrough_modifiers(mouse, gesture.modifiers);
                    let _ = self.forward_pane_mouse_button(
                        terminal_runtimes,
                        &gesture.pane_info,
                        forwarded_mouse,
                    );
                    if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Right)) {
                        self.right_click_passthrough = None;
                    }
                    return true;
                }
                _ => {
                    self.right_click_passthrough = None;
                }
            }
        }

        if self.mode != Mode::Terminal
            || in_sidebar
            || !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Right))
        {
            return false;
        }

        let Some(info) = self.pane_at(mouse.column, mouse.row).cloned() else {
            return false;
        };
        let configured_modifiers = self
            .right_click_passthrough_modifiers
            .filter(|modifiers| mouse.modifiers == *modifiers);
        let pane_passthrough = mouse.modifiers.is_empty()
            && self.active.is_some_and(|ws_idx| {
                self.workspaces
                    .get(ws_idx)
                    .and_then(|workspace| workspace.pane_state(info.id))
                    .is_some_and(|pane| pane.right_click_passthrough)
            });
        let Some(modifiers) = configured_modifiers
            .or_else(|| pane_passthrough.then(crossterm::event::KeyModifiers::empty))
        else {
            return false;
        };

        self.focus_pane(info.id);
        let forwarded_mouse = self.strip_right_click_passthrough_modifiers(mouse, modifiers);
        if !self.forward_pane_mouse_button(terminal_runtimes, &info, forwarded_mouse) {
            return false;
        }

        self.selection = None;
        self.selection_autoscroll = None;
        self.clear_chrome_press(source_id);
        self.drag = None;
        self.context_menu = None;
        self.right_click_passthrough = Some(RightClickPassthroughGesture {
            pane_info: info,
            modifiers,
        });
        true
    }

    fn strip_right_click_passthrough_modifiers(
        &self,
        mouse: MouseEvent,
        modifiers: crossterm::event::KeyModifiers,
    ) -> MouseEvent {
        MouseEvent {
            modifiers: mouse.modifiers.difference(modifiers),
            ..mouse
        }
    }

    pub(super) fn handle_terminal_wheel(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        mouse: MouseEvent,
    ) {
        let lines_per_notch = self.mouse_scroll_lines;

        if let Some(info) = self.pane_at(mouse.column, mouse.row).cloned() {
            self.focus_pane(info.id);
            if self.forward_pane_wheel(terminal_runtimes, &info, mouse) {
                return;
            }
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    self.scroll_pane_up(terminal_runtimes, info.id, lines_per_notch)
                }
                MouseEventKind::ScrollDown => {
                    self.scroll_pane_down(terminal_runtimes, info.id, lines_per_notch)
                }
                _ => {}
            }
            return;
        }

        if let Some(info) = self.pane_frame_at(mouse.column, mouse.row).cloned() {
            self.focus_pane(info.id);
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    self.scroll_pane_up(terminal_runtimes, info.id, lines_per_notch)
                }
                MouseEventKind::ScrollDown => {
                    self.scroll_pane_down(terminal_runtimes, info.id, lines_per_notch)
                }
                _ => {}
            }
            return;
        }

        if let Some(ws_idx) = self.active {
            if let Some(rt) = self.focused_runtime_in_workspace(terminal_runtimes, ws_idx) {
                match mouse.kind {
                    MouseEventKind::ScrollUp => rt.scroll_up(lines_per_notch),
                    MouseEventKind::ScrollDown => rt.scroll_down(lines_per_notch),
                    _ => {}
                }
            }
        }
    }

    fn pane_mouse_position(
        &self,
        runtime: &crate::terminal::TerminalRuntime,
        inner: Rect,
        mouse: MouseEvent,
    ) -> Option<crate::input::mouse::Position> {
        let column = mouse.column.saturating_sub(inner.x);
        let row = mouse.row.saturating_sub(inner.y);
        let cell = crate::input::mouse::Position::Cell { column, row };
        let Some(host) = self.host_mouse_pixels else {
            return Some(cell);
        };
        let wants_pixels = runtime.sgr_pixel_mouse_enabled();
        if !wants_pixels {
            return Some(cell);
        }
        let Some((width_px, height_px)) = runtime.pixel_size() else {
            return Some(cell);
        };
        Some(
            host.pane_position(inner, width_px, height_px)
                .unwrap_or(cell),
        )
    }

    pub(super) fn forward_pane_mouse_button(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        info: &PaneInfo,
        mouse: MouseEvent,
    ) -> bool {
        let Some(ws_idx) = self.active else {
            return false;
        };
        let Some(rt) = self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
        else {
            return false;
        };
        let Some(position) = self.pane_mouse_position(rt, info.inner_rect, mouse) else {
            return false;
        };
        let Some(bytes) = rt.encode_mouse_button(mouse.kind, position, mouse.modifiers) else {
            return false;
        };
        rt.scroll_reset();
        if let Err(err) = rt.try_send_bytes(Bytes::from(bytes)) {
            warn!(pane = info.id.raw(), err = %err, kind = ?mouse.kind, "failed to forward mouse button event");
        } else {
            self.forwarded_pane_input = Some(info.id);
        }
        true
    }

    pub(super) fn forward_pane_mouse_motion(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        info: &PaneInfo,
        mouse: MouseEvent,
    ) -> bool {
        let Some(ws_idx) = self.active else {
            return false;
        };
        let Some(rt) = self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
        else {
            return false;
        };
        let Some(position) = self.pane_mouse_position(rt, info.inner_rect, mouse) else {
            return false;
        };
        let Some(bytes) = rt.encode_mouse_motion(mouse.kind, position, mouse.modifiers) else {
            return false;
        };
        if let Err(err) = rt.try_send_bytes(Bytes::from(bytes)) {
            warn!(pane = info.id.raw(), err = %err, kind = ?mouse.kind, "failed to forward mouse motion event");
        } else {
            self.forwarded_pane_input = Some(info.id);
        }
        true
    }

    fn forward_pane_reported_wheel(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        info: &PaneInfo,
        mouse: MouseEvent,
    ) -> bool {
        let Some(ws_idx) = self.active else {
            return false;
        };
        let Some(rt) = self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
        else {
            return false;
        };
        if rt.wheel_routing() != Some(crate::pane::WheelRouting::MouseReport) {
            return false;
        }
        rt.scroll_reset();
        let Some(position) = self.pane_mouse_position(rt, info.inner_rect, mouse) else {
            return false;
        };
        let Some(bytes) = rt.encode_mouse_wheel(mouse.kind, position, mouse.modifiers) else {
            warn!(pane = info.id.raw(), kind = ?mouse.kind, "failed to encode mouse wheel event");
            return true;
        };
        if let Err(err) = rt.try_send_bytes(Bytes::from(bytes)) {
            warn!(pane = info.id.raw(), err = %err, "failed to forward mouse wheel event");
        } else {
            self.forwarded_pane_input = Some(info.id);
        }
        true
    }

    pub(super) fn forward_pane_wheel(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        info: &PaneInfo,
        mouse: MouseEvent,
    ) -> bool {
        let Some(ws_idx) = self.active else {
            return false;
        };
        let Some(rt) = self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
        else {
            return false;
        };
        match rt.wheel_routing() {
            Some(crate::pane::WheelRouting::HostScroll) | None => false,
            Some(crate::pane::WheelRouting::MouseReport) => {
                rt.scroll_reset();
                let column = mouse.column.saturating_sub(info.inner_rect.x);
                let row = mouse.row.saturating_sub(info.inner_rect.y);
                let Some(bytes) = rt.encode_mouse_wheel(
                    mouse.kind,
                    crate::input::mouse::Position::Cell { column, row },
                    mouse.modifiers,
                ) else {
                    warn!(pane = info.id.raw(), kind = ?mouse.kind, "failed to encode mouse wheel event");
                    return true;
                };
                if let Err(err) = rt.try_send_bytes(Bytes::from(bytes)) {
                    warn!(pane = info.id.raw(), err = %err, "failed to forward mouse wheel event");
                } else {
                    self.forwarded_pane_input = Some(info.id);
                }
                true
            }
            Some(crate::pane::WheelRouting::AlternateScroll) => {
                rt.scroll_reset();
                let Some(bytes) = rt.encode_alternate_scroll(mouse.kind) else {
                    return true;
                };
                if let Err(err) = rt.try_send_bytes(Bytes::from(bytes)) {
                    warn!(pane = info.id.raw(), err = %err, "failed to forward alternate-scroll key");
                } else {
                    self.forwarded_pane_input = Some(info.id);
                }
                true
            }
        }
    }

    pub(crate) fn take_forwarded_pane_input(&mut self) -> Option<PaneId> {
        self.forwarded_pane_input.take()
    }

    pub(super) fn set_pane_scroll_offset(
        &self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        pane_id: crate::layout::PaneId,
        offset_from_bottom: usize,
    ) {
        for ws_idx in 0..self.workspaces.len() {
            let Some(rt) = self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, pane_id)
            else {
                continue;
            };
            rt.set_scroll_offset_from_bottom(offset_from_bottom);
            return;
        }
    }

    pub(super) fn scrollbar_target_at(
        &self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        col: u16,
        row: u16,
    ) -> Option<(crate::layout::PaneId, ScrollbarClickTarget)> {
        let ws_idx = self.active?;
        let info = self.view.pane_infos.iter().find(|info| {
            crate::ui::pane_scrollbar_rect(info).is_some_and(|track| {
                col >= track.x
                    && col < track.x + track.width
                    && row >= track.y
                    && row < track.y + track.height
            })
        })?;
        let rt = self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)?;
        let metrics = rt.scroll_metrics()?;
        if metrics.max_offset_from_bottom == 0 {
            return None;
        }
        let track = crate::ui::pane_scrollbar_rect(info)?;
        if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(metrics, track, row) {
            Some((info.id, ScrollbarClickTarget::Thumb { grab_row_offset }))
        } else {
            Some((
                info.id,
                ScrollbarClickTarget::Track {
                    offset_from_bottom: crate::ui::scrollbar_offset_from_row(metrics, track, row),
                },
            ))
        }
    }

    pub(super) fn scrollbar_offset_for_pane_row(
        &self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        pane_id: crate::layout::PaneId,
        row: u16,
        grab_row_offset: u16,
    ) -> Option<usize> {
        let ws_idx = self.active?;
        let info = self
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == pane_id)?;
        let track = crate::ui::pane_scrollbar_rect(info)?;
        let rt = self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, pane_id)?;
        let metrics = rt.scroll_metrics()?;
        if metrics.max_offset_from_bottom == 0 {
            return None;
        }
        Some(crate::ui::scrollbar_offset_from_drag_row(
            metrics,
            track,
            row,
            grab_row_offset,
        ))
    }
}

#[cfg(test)]
pub(super) fn wheel_routing(input_state: crate::pane::InputState) -> WheelRouting {
    if input_state.mouse_protocol_mode.reporting_enabled() {
        WheelRouting::MouseReport
    } else if input_state.alternate_screen && input_state.mouse_alternate_scroll {
        WheelRouting::AlternateScroll
    } else {
        WheelRouting::HostScroll
    }
}

fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && col >= rect.x
        && col < rect.x + rect.width
        && row >= rect.y
        && row < rect.y + rect.height
}

fn apply_scroll(scroll: &mut usize, delta: i16, max_scroll: usize) {
    if delta.is_negative() {
        *scroll = scroll.saturating_sub(delta.unsigned_abs() as usize);
    } else {
        *scroll = scroll.saturating_add(delta as usize).min(max_scroll);
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
    use ratatui::{
        backend::TestBackend,
        layout::{Direction, Rect},
        Terminal,
    };

    use super::super::{
        app_for_mouse_test, capture_snapshot, mouse, numbered_lines_bytes, root_layout_ratio,
    };
    use super::*;
    use crate::app::input::modal::handle_context_menu_key;
    use crate::{
        app::state::{
            ContextMenuKind, ContextMenuState, InfoPanelLinkRow, MenuListState, Mode, ViewLayout,
        },
        app::App,
        detect::{Agent, AgentState},
        input::TerminalKey,
        workspace::Workspace,
    };

    /// One workspace laid out for real, so a click lands on pane content.
    fn app_with_clickable_pane() -> (App, crate::layout::PaneId, Rect) {
        let mut app = app_for_mouse_test();
        let ws = Workspace::test_new("one");
        let pane_id = ws.tabs[0].root_pane;
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let rect = app
            .state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == pane_id)
            .expect("pane laid out")
            .inner_rect;
        (app, pane_id, rect)
    }

    #[test]
    fn clicking_a_status_row_work_link_opens_it_in_the_dock() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        let terminal_id = app.state.workspaces[0]
            .focused_pane_id()
            .and_then(|pane_id| app.state.workspaces[0].terminal_id(pane_id))
            .expect("focused terminal")
            .clone();
        let terminal = app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("focused terminal state");
        terminal.set_terminal_title(Some("Fix billing".into()));
        terminal.replace_prevalidated_manual_work_context(crate::work_context::PaneWorkContext {
            repo: Some("herdrdev/herdr".into()),
            ticket_ids: vec!["SCA-3165".into()],
            ..Default::default()
        });
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 24));

        assert!(
            app.state.dock_open_surfaces.is_empty(),
            "the link alone opens nothing"
        );
        let link = app
            .state
            .view
            .status_work_links
            .first()
            .cloned()
            .expect("the status row names the ticket");
        assert_eq!(link.label, "SCA-3165");

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            link.rect.x,
            link.rect.y,
        ));

        assert!(!app.state.dock_collapsed, "the click opens the dock");
        assert_eq!(
            app.state.dock_tab,
            Some(crate::app::DockSurface::Linear),
            "on the ticket that was clicked"
        );
        assert_eq!(app.state.dock_tab_label(0), "SCA-3165");
    }

    #[test]
    fn clicking_the_sidebar_animation_button_toggles_its_pause() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.active = Some(0);
        app.state.hyperspace.enabled = true;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 24));

        let button = app.state.view.hyperspace_pause_hit_area;
        assert!(button.width > 0, "the panel offers a pause button");
        assert_eq!(
            button.x, app.state.view.sidebar_rect.x,
            "the button sits in the sidebar's left column"
        );
        assert!(!app.state.hyperspace.paused());

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            button.x,
            button.y,
        ));
        assert!(app.state.hyperspace.paused(), "one click stops the field");

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            button.x,
            button.y,
        ));
        assert!(
            !app.state.hyperspace.paused(),
            "a second click starts it again"
        );
    }

    #[test]
    fn a_click_next_to_the_animation_button_leaves_it_alone() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.active = Some(0);
        app.state.hyperspace.enabled = true;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 24));

        let button = app.state.view.hyperspace_pause_hit_area;
        assert!(button.width > 0);
        // The footer icon row is one row below the panel, and it owns its own
        // clicks; a near miss must not toggle the animation.
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            button.right(),
            button.y,
        ));
        assert!(!app.state.hyperspace.paused());
    }

    #[test]
    fn clicking_pane_content_releases_dock_and_sidebar_focus() {
        let (mut app, _pane_id, rect) = app_with_clickable_pane();
        app.state.dock_home_focused = true;
        app.state.dock_pr_focused = true;
        app.state.dock_chooser_focused = true;
        app.state.sidebar_selected_work_group = Some("linear:SCA-3102".into());
        app.state.sidebar_selected_settled = Some(crate::app::state::PaneFocusTarget {
            workspace_id: app.state.workspaces[0].id.clone(),
            pane_id: _pane_id,
        });

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            rect.x + 1,
            rect.y + 1,
        ));

        assert!(!app.state.dock_home_focused, "dock home kept focus");
        assert!(!app.state.dock_pr_focused, "dock pr kept focus");
        assert!(!app.state.dock_chooser_focused, "dock chooser kept focus");
        assert_eq!(app.state.sidebar_selected_work_group, None);
        assert!(app.state.sidebar_object_menu.is_none());
        assert!(app.state.sidebar_selected_settled.is_none());
        assert!(
            !app.handle_sidebar_settled_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty())),
            "settled row consumed Enter after the pane took focus"
        );

        // The letters the dock would otherwise answer now belong to the shell.
        app.state.dock_collapsed = false;
        app.state.dock_tab = Some(crate::app::DockSurface::Home);
        for character in ['a', 'm', 'x', 'r'] {
            let key = TerminalKey::new(KeyCode::Char(character), KeyModifiers::empty());
            assert!(
                !app.handle_dock_home_key(&key),
                "dock home consumed '{character}' after the pane took focus"
            );
        }
    }

    #[test]
    fn refocusing_the_already_focused_pane_releases_dock_focus() {
        let (mut app, pane_id, _rect) = app_with_clickable_pane();
        app.state.workspaces[0].tabs[0].layout.focus_pane(pane_id);
        app.state.dock_home_focused = true;
        app.state.dock_chooser_focused = true;

        // Already focused, so this returns false; the release must still happen.
        assert!(!app.state.focus_pane_in_workspace(0, pane_id));

        assert!(!app.state.dock_home_focused);
        assert!(!app.state.dock_chooser_focused);
    }

    #[test]
    fn switching_workspace_releases_dock_focus() {
        let (mut app, _pane_id, _rect) = app_with_clickable_pane();
        app.state.workspaces.push(Workspace::test_new("two"));
        app.state.dock_home_focused = true;
        app.state.dock_pr_focused = true;

        app.state.switch_workspace(1);

        assert!(!app.state.dock_home_focused);
        assert!(!app.state.dock_pr_focused);
    }

    #[test]
    fn focusing_another_pane_releases_dock_focus() {
        let (mut app, pane_id, _rect) = app_with_clickable_pane();
        app.state.workspaces[0].tabs[0].layout.focus_pane(pane_id);
        let second = Workspace::test_new("two");
        let second_pane = second.tabs[0].root_pane;
        app.state.workspaces.push(second);
        app.state.dock_pr_focused = true;
        app.state.dock_linear_focused = true;
        app.state.dock_agents_focused = true;
        app.state.dock_files_focused = true;
        app.state.dock_diff_focused = true;

        assert!(app.state.focus_pane_in_workspace(1, second_pane));

        assert!(!app.state.dock_pr_focused);
        assert!(!app.state.dock_linear_focused);
        assert!(!app.state.dock_agents_focused);
        assert!(!app.state.dock_files_focused);
        assert!(!app.state.dock_diff_focused);
    }

    #[test]
    fn tab_click_survives_stray_drag_report_off_the_tab_bar() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        ws.test_add_tab(None);
        ws.active_tab = 1;
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        let area = Rect::new(0, 0, 106, 20);
        crate::ui::compute_view(&mut app.state, area);

        let first_tab = app.state.view.tab_hit_areas[0];
        let press_col = first_tab.x + 1;
        let stray_row = area.height - 1;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            press_col,
            first_tab.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            press_col,
            stray_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            press_col,
            stray_row,
        ));

        assert_eq!(app.state.workspaces[0].active_tab, 0);
    }

    #[test]
    fn workspace_click_survives_stray_drag_report_off_the_workspace_list() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("first"), Workspace::test_new("second")];
        app.state.active = Some(1);
        app.state.selected = 1;
        let area = Rect::new(0, 0, 106, 20);
        crate::ui::compute_view(&mut app.state, area);

        let first_workspace = app.state.view.workspace_card_areas[0];
        let press_col = first_workspace.rect.x + 1;
        let press_row = first_workspace.rect.y;
        let stray_row = area.height - 1;
        assert!(app.state.workspace_drop_target_at_row(stray_row).is_none());

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            press_col,
            press_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            press_col,
            stray_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            press_col,
            stray_row,
        ));

        assert_eq!(app.state.active, Some(0));
    }

    #[test]
    fn concurrent_input_sources_keep_their_tab_clicks() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        ws.test_add_tab(None);
        ws.test_add_tab(None);
        ws.active_tab = 2;
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let first_tab = app.state.view.tab_hit_areas[0];
        let second_tab = app.state.view.tab_hit_areas[1];
        app.handle_mouse_from_input_source(
            41,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                first_tab.x + 1,
                first_tab.y,
            ),
        );
        app.handle_mouse_from_input_source(
            42,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                second_tab.x + 1,
                second_tab.y,
            ),
        );

        app.handle_mouse_from_input_source(
            41,
            mouse(
                MouseEventKind::Up(MouseButton::Left),
                first_tab.x + 1,
                first_tab.y,
            ),
        );
        assert_eq!(app.state.workspaces[0].active_tab, 0);

        app.handle_mouse_from_input_source(
            42,
            mouse(
                MouseEventKind::Up(MouseButton::Left),
                second_tab.x + 1,
                second_tab.y,
            ),
        );
        assert_eq!(app.state.workspaces[0].active_tab, 1);
    }

    #[test]
    fn concurrent_input_sources_keep_their_workspace_clicks() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            Workspace::test_new("first"),
            Workspace::test_new("second"),
            Workspace::test_new("third"),
        ];
        app.state.active = Some(2);
        app.state.selected = 2;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let first = app.state.view.workspace_card_areas[0].rect;
        let second = app.state.view.workspace_card_areas[1].rect;
        app.handle_mouse_from_input_source(
            41,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                first.x + 1,
                first.y,
            ),
        );
        app.handle_mouse_from_input_source(
            42,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                second.x + 1,
                second.y,
            ),
        );

        app.handle_mouse_from_input_source(
            41,
            mouse(MouseEventKind::Up(MouseButton::Left), first.x + 1, first.y),
        );
        assert_eq!(app.state.active, Some(0));

        app.handle_mouse_from_input_source(
            42,
            mouse(
                MouseEventKind::Up(MouseButton::Left),
                second.x + 1,
                second.y,
            ),
        );
        assert_eq!(app.state.active, Some(1));
    }

    #[test]
    fn tab_click_completes_while_other_source_reorders() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        ws.test_add_tab(None);
        ws.test_add_tab(None);
        ws.active_tab = 2;
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let first_tab = app.state.view.tab_hit_areas[0];
        let second_tab = app.state.view.tab_hit_areas[1];
        let last_tab = app.state.view.tab_hit_areas[2];
        let drop_col = last_tab.x + last_tab.width;
        app.handle_mouse_from_input_source(
            41,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                first_tab.x + 1,
                first_tab.y,
            ),
        );
        app.handle_mouse_from_input_source(
            41,
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                drop_col,
                first_tab.y,
            ),
        );
        app.handle_mouse_from_input_source(
            42,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                second_tab.x + 1,
                second_tab.y,
            ),
        );

        app.handle_mouse_from_input_source(
            42,
            mouse(
                MouseEventKind::Up(MouseButton::Left),
                second_tab.x + 1,
                second_tab.y,
            ),
        );
        assert_eq!(app.state.workspaces[0].active_tab, 1);
        assert!(matches!(
            app.state.drag.as_ref().map(|drag| &drag.target),
            Some(DragTarget::TabReorder { source_id: 41, .. })
        ));

        app.handle_mouse_from_input_source(
            41,
            mouse(MouseEventKind::Up(MouseButton::Left), drop_col, first_tab.y),
        );
        assert!(app.state.drag.is_none());
    }

    #[test]
    fn releasing_input_source_clears_only_its_pending_tab_click() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        ws.test_add_tab(None);
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let first_tab = app.state.view.tab_hit_areas[0];
        let second_tab = app.state.view.tab_hit_areas[1];
        app.handle_mouse_from_input_source(
            41,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                first_tab.x + 1,
                first_tab.y,
            ),
        );
        app.handle_mouse_from_input_source(
            42,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                second_tab.x + 1,
                second_tab.y,
            ),
        );

        app.clear_input_source(41);

        assert!(!app.state.tab_presses.contains_key(&41));
        assert!(app.state.tab_presses.contains_key(&42));
    }

    #[tokio::test]
    async fn other_input_source_pane_gesture_is_not_swallowed_by_chrome_press() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        ws.test_add_tab(None);
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        let area = Rect::new(0, 0, 106, 20);
        crate::ui::compute_view(&mut app.state, area);

        let info = app.state.view.pane_infos[0].clone();
        let pane_id = info.id;
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                0,
                b"\x1b[?1000h\x1b[?1006h",
                4,
            );
        app.state.insert_test_runtime(pane_id, runtime);
        crate::ui::compute_view(&mut app.state, area);

        let first_tab = app.state.view.tab_hit_areas[0];
        let pane_col = info.inner_rect.x;
        let pane_row = info.inner_rect.y;
        app.handle_mouse_from_input_source(
            41,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                first_tab.x + 1,
                first_tab.y,
            ),
        );
        app.handle_mouse_from_input_source(
            42,
            mouse(MouseEventKind::Down(MouseButton::Left), pane_col, pane_row),
        );
        app.handle_mouse_from_input_source(
            42,
            mouse(MouseEventKind::Up(MouseButton::Left), pane_col, pane_row),
        );

        assert_eq!(
            input_rx.try_recv().expect("other source mouse down"),
            Bytes::from_static(b"\x1b[<0;1;1M")
        );
        assert_eq!(
            input_rx.try_recv().expect("other source mouse up"),
            Bytes::from_static(b"\x1b[<0;1;1m")
        );
    }

    #[test]
    fn other_input_source_cannot_release_tab_reorder() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        ws.test_add_tab(Some("second"));
        ws.test_add_tab(Some("third"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let source = app.state.view.tab_hit_areas[0];
        let target = app.state.view.tab_hit_areas[2];
        let drop_col = target.x + target.width;
        app.handle_mouse_from_input_source(
            41,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                source.x + 1,
                source.y,
            ),
        );
        app.handle_mouse_from_input_source(
            41,
            mouse(MouseEventKind::Drag(MouseButton::Left), drop_col, source.y),
        );
        app.handle_mouse_from_input_source(
            42,
            mouse(MouseEventKind::Up(MouseButton::Left), drop_col, source.y),
        );

        assert!(app.state.drag.is_some());
        assert_eq!(app.state.workspaces[0].tabs[0].custom_name, None);

        app.handle_mouse_from_input_source(
            41,
            mouse(MouseEventKind::Up(MouseButton::Left), drop_col, source.y),
        );

        assert!(app.state.drag.is_none());
        assert_eq!(app.state.workspaces[0].tabs[2].custom_name.as_deref(), None);
    }

    #[tokio::test]
    async fn tab_click_survives_stray_drag_report_into_a_mouse_reporting_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        ws.test_add_tab(None);
        ws.active_tab = 1;
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        let area = Rect::new(0, 0, 106, 20);
        crate::ui::compute_view(&mut app.state, area);

        let info = app
            .state
            .view
            .pane_infos
            .first()
            .cloned()
            .expect("visible pane");
        app.state.insert_test_runtime(
            info.id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                info.inner_rect.width.max(1),
                info.inner_rect.height.max(1),
                b"\x1b[?1002h",
            ),
        );
        crate::ui::compute_view(&mut app.state, area);

        let first_tab = app.state.view.tab_hit_areas[0];
        let press_col = first_tab.x + 1;
        let stray_row = info.inner_rect.bottom().saturating_sub(1);
        assert!(
            app.state.pane_mouse_target(press_col, stray_row).is_some(),
            "stray coordinates must land on the pane for this to test anything"
        );

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            press_col,
            first_tab.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            press_col,
            stray_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            press_col,
            stray_row,
        ));

        assert_eq!(app.state.workspaces[0].active_tab, 0);
    }

    fn mark_worktree_space_member(workspace: &mut Workspace, ws_idx: usize, key: &str) {
        workspace.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: key.into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: format!("/repo/worktree-{ws_idx}").into(),
            is_linked_worktree: ws_idx != 0,
        });
    }

    fn seeded_wide_sidebar_app() -> App {
        let mut app = app_for_mouse_test();
        app.state = crate::ui::sidebar_work_item_fixture();
        app.state.mode = Mode::Terminal;
        app.state.sidebar_collapsed = false;
        app.state.sidebar_min_width = 18;
        app.state.sidebar_max_width = 60;
        app.state.sidebar_width = 50;
        app.state.active = Some(0);
        app.state.selected = 0;
        app
    }

    fn render_wide_app(app: &mut App) {
        const WIDTH: u16 = 269;
        const HEIGHT: u16 = 84;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, WIDTH, HEIGHT));
        let mut terminal =
            Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("wide test terminal");
        terminal
            .draw(|frame| crate::ui::render(&app.state, frame))
            .expect("wide app render");
    }

    #[tokio::test]
    async fn sidebar_search_mouse_character_flow_renders_seeded_wide_view() {
        let mut app = seeded_wide_sidebar_app();
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 269, 84));
        assert_eq!(app.state.view.sidebar_rect.width, 50);
        let sidebar = app.state.view.sidebar_rect;
        let search = crate::ui::sidebar_header_search_rect(sidebar);
        let new_menu = crate::ui::sidebar_header_new_menu_rect(sidebar);
        let header_areas = [
            crate::ui::expanded_sidebar_toggle_rect(sidebar),
            search,
            crate::ui::sidebar_header_new_thread_rect(sidebar),
            new_menu,
            crate::ui::sidebar_header_overflow_rect(sidebar),
            crate::ui::sidebar_group_mode_anchor_rect(sidebar),
        ];
        assert!(header_areas
            .iter()
            .all(|area| area.width > 0 && area.height == 1));
        assert!(header_areas
            .iter()
            .all(|area| area.x >= sidebar.x && area.right() <= sidebar.right()));

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            new_menu.x,
            new_menu.y,
        ));
        let menu = crate::ui::sidebar_new_menu_layout(&app.state, Rect::new(0, 0, 269, 84))
            .expect("new menu");
        assert_eq!(
            menu.visible_rows,
            crate::app::state::SidebarNewMenuAction::ALL.len()
        );
        assert_eq!(menu.list_rect.y, new_menu.bottom());
        for index in 0..menu.visible_rows {
            app.handle_mouse(mouse(
                MouseEventKind::Moved,
                menu.list_rect.x,
                menu.list_rect.y + u16::try_from(index).expect("menu row index"),
            ));
            assert_eq!(
                app.state.sidebar_new_menu.map(|state| state.selected),
                Some(index)
            );
        }
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            new_menu.x,
            new_menu.y,
        ));
        assert!(app.state.sidebar_new_menu.is_none());

        for kind in [
            MouseEventKind::Moved,
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            app.handle_mouse(mouse(kind, search.x, search.y));
        }
        assert!(app.state.sidebar_search_active);

        app.handle_key(TerminalKey::new(
            crossterm::event::KeyCode::Char('a'),
            crossterm::event::KeyModifiers::empty(),
        ))
        .await;
        assert_eq!(app.state.sidebar_work_filter.query, "a");
        render_wide_app(&mut app);
    }

    #[test]
    fn sidebar_linear_footer_mouse_flow_renders_seeded_wide_view() {
        use crate::app::state::{ControlId, SidebarFooterItem, WorkProjection};

        let mut app = seeded_wide_sidebar_app();
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 269, 84));
        assert_eq!(app.state.view.sidebar_rect.width, 50);
        let areas = [
            app.state.view.sidebar_footer_settings_hit_area,
            app.state.view.sidebar_footer_work_hit_area,
            app.state.view.sidebar_footer_usage_hit_area,
            app.state.view.sidebar_footer_ticket_hit_area,
            app.state.view.sidebar_footer_missive_hit_area,
            app.state.view.sidebar_footer_refresh_hit_area,
        ];
        assert!(areas.iter().all(|area| area.width == 2 && area.height == 1));
        assert!(areas.windows(2).all(|pair| pair[0].right() == pair[1].x));
        for (area, item) in areas.iter().zip([
            SidebarFooterItem::Settings,
            SidebarFooterItem::PullRequests,
            SidebarFooterItem::Usage,
            SidebarFooterItem::Linear,
            SidebarFooterItem::Missive,
            SidebarFooterItem::Refresh,
        ]) {
            app.handle_mouse(mouse(MouseEventKind::Moved, area.x, area.y));
            assert_eq!(
                app.state.hovered_control,
                Some(ControlId::SidebarFooter(item))
            );
        }
        let linear = areas[3];

        app.handle_mouse(mouse(MouseEventKind::Moved, linear.x, linear.y));
        assert_eq!(
            app.state.hovered_control,
            Some(ControlId::SidebarFooter(SidebarFooterItem::Linear))
        );
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            linear.x,
            linear.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            linear.x,
            linear.y,
        ));
        assert_eq!(app.state.hovered_control, None);
        assert!(app
            .state
            .work_view
            .as_ref()
            .is_some_and(|view| view.projection == WorkProjection::Tickets));
        render_wide_app(&mut app);
    }

    #[test]
    fn clicking_a_compact_sidebar_tab_row_focuses_the_tab() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.toggle_workspace_agent_disclosure(1);
        let sidebar = Rect::new(0, 0, 40, 16);
        app.state.view.sidebar_rect = sidebar;
        let target = crate::ui::compute_tab_card_areas(&app.state, sidebar)
            .into_iter()
            .find(|card| card.ws_idx == 1)
            .expect("second workspace tab row");

        let action = app.state.handle_mouse(
            &mut app.terminal_runtimes,
            crate::app::LOCAL_INPUT_SOURCE,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                target.rect.x + 2,
                target.rect.y,
            ),
        );

        assert!(matches!(
            action,
            Some(MouseAction::FocusSidebarTab {
                ws_idx: 1,
                tab_idx: 0
            })
        ));
    }

    #[test]
    fn clicking_a_symphony_row_opens_that_workflow() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        let workflow = |name: &str| crate::symphony::Workflow {
            workflow_id: format!("wf-{name}"),
            run_id: format!("run-{name}"),
            name: name.to_string(),
            phase: "runFlowStep".to_string(),
            wait: None,
            started_at: None,
            ticket: None,
            repo: None,
            pr: None,
            receipts: None,
        };
        app.state.symphony_snapshot = crate::symphony::Snapshot {
            workflows: vec![workflow("first"), workflow("second")],
            unavailable: None,
            polled: true,
        };
        let sidebar = Rect::new(0, 0, 40, 16);
        app.state.view.sidebar_rect = sidebar;
        let row = (sidebar.y..sidebar.bottom())
            .find(|row| crate::ui::sidebar_symphony_job_at(&app.state, *row) == Some(1))
            .expect("second symphony row");

        let action = app.state.handle_mouse(
            &mut app.terminal_runtimes,
            crate::app::LOCAL_INPUT_SOURCE,
            mouse(MouseEventKind::Down(MouseButton::Left), sidebar.x + 4, row),
        );

        assert!(matches!(
            action,
            Some(MouseAction::OpenSymphonyWorkflow { index: 1 })
        ));
    }

    #[test]
    fn clicking_the_symphony_dashboard_link_opens_the_job_url() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.symphony_snapshot = crate::symphony::Snapshot {
            workflows: vec![crate::symphony::Workflow {
                workflow_id: "symphony-MAT-138".to_string(),
                run_id: "019a".to_string(),
                name: "blocker dashboard".to_string(),
                phase: "runFlowStep".to_string(),
                wait: None,
                started_at: None,
                ticket: None,
                repo: None,
                pr: None,
                receipts: None,
            }],
            unavailable: None,
            polled: true,
        };
        let workflow = app.state.symphony_snapshot.workflows[0].clone();
        app.state.bind_symphony_dock(&workflow);
        assert_eq!(
            app.state.dock_tab,
            Some(crate::app::DockSurface::Symphony),
            "opening a job brings its surface up"
        );
        app.state.mode = Mode::Terminal;
        app.state.view.dock_rect = Rect::new(60, 0, 40, 20);
        app.state.view.dock_body_rect = Rect::new(60, 2, 40, 18);
        let link =
            crate::ui::dock_symphony_dashboard_link_rect(&app.state, app.state.view.dock_body_rect)
                .expect("dashboard link");

        let action = app.state.handle_mouse(
            &mut app.terminal_runtimes,
            crate::app::LOCAL_INPUT_SOURCE,
            mouse(MouseEventKind::Down(MouseButton::Left), link.x, link.y),
        );

        assert!(
            matches!(action, Some(MouseAction::OpenUrl { ref url })
                if url == "http://localhost:8233/namespaces/default/workflows/symphony-MAT-138/019a/history"),
            "the link must open the job's dashboard url"
        );

        // A click one row below the link is an ordinary dock click.
        let elsewhere = app.state.handle_mouse(
            &mut app.terminal_runtimes,
            crate::app::LOCAL_INPUT_SOURCE,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                link.x,
                link.y.saturating_add(1),
            ),
        );
        assert!(!matches!(elsewhere, Some(MouseAction::OpenUrl { .. })));
    }

    fn add_test_work_link(app: &mut crate::app::App, ws_idx: usize) {
        let pane_id = app.state.workspaces[ws_idx].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[ws_idx].tabs[0]
            .terminal_id(pane_id)
            .expect("test pane terminal")
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal")
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                ticket_ids: Some(vec!["MAT-1".into()]),
                ..Default::default()
            })
            .expect("valid test work context");
    }

    #[test]
    fn sidebar_tab_row_has_no_priority_gutter() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.reconcile_sidebar_presentation();
        let cards = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect);
        let target = cards[1].clone();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            target.rect.x + 1,
            target.rect.y,
        ));

        assert!(!app.state.workspaces[1].tabs[0].prio);
        assert_eq!(app.state.active, Some(1));
    }

    #[test]
    fn sidebar_tab_click_outside_gutters_still_focuses_the_tab() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.reconcile_sidebar_presentation();
        let cards = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect);
        let title_column = cards[1].rect.x.saturating_add(8);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            title_column,
            cards[1].rect.y,
        ));

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.workspaces[1].active_tab_index(), 0);
        assert!(!app.state.workspaces[1].tabs[0].prio);
    }

    #[test]
    fn a_linked_pane_gets_no_sidebar_info_gutter() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.state.ensure_test_terminals();
        add_test_work_link(&mut app, 1);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.reconcile_sidebar_presentation();
        let cards = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect);
        let target = cards
            .iter()
            .find(|card| card.ws_idx == 1)
            .expect("linked workspace tab row");
        // The overview shows no work-link marker, so the compact row focuses the tab directly.
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            target.rect.x + 5,
            target.rect.y,
        ));

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.workspaces[1].active_tab_index(), 0);
        assert!(
            !app.state.info_panel_expanded,
            "a linked row must not open the info panel from the overview"
        );
    }

    #[test]
    fn sidebar_tab_row_focuses_from_any_column() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.state.ensure_test_terminals();
        add_test_work_link(&mut app, 1);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.reconcile_sidebar_presentation();
        let cards = crate::ui::compute_tab_card_areas(&app.state, app.state.view.sidebar_rect);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            cards[1].rect.x + 1,
            cards[1].rect.y,
        ));
        assert_eq!(app.state.active, Some(1));
        assert!(!app.state.workspaces[1].tabs[0].prio);
    }

    #[test]
    fn clicking_the_dock_handle_toggles_it_without_touching_pane_focus() {
        let mut app = app_for_mouse_test();
        app.state.mode = Mode::Terminal;
        app.state.dock_collapsed = true;
        app.state.dock_tab = Some(crate::app::DockSurface::Home);
        app.state.view.dock_rect = Rect::new(79, 0, 1, 20);
        app.state.view.dock_handle_rect = app.state.view.dock_rect;
        let focused_before = app
            .state
            .active
            .and_then(|idx| app.state.workspaces.get(idx))
            .and_then(Workspace::focused_pane_id);

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 79, 5));

        assert!(!app.state.dock_collapsed);
        let focused_after = app
            .state
            .active
            .and_then(|idx| app.state.workspaces.get(idx))
            .and_then(Workspace::focused_pane_id);
        assert_eq!(focused_after, focused_before);
        assert!(app.state.dock_home_focused);
    }

    #[test]
    fn clicking_a_dock_tab_selects_only_that_dock_tab() {
        let mut app = app_for_mouse_test();
        app.state.mode = Mode::Terminal;
        app.state.dock_collapsed = false;
        app.state.dock_open_surfaces = vec![
            crate::app::DockSurface::Home,
            crate::app::DockSurface::Editor,
            crate::app::DockSurface::Shortcuts,
        ];
        app.state.dock_tab = Some(crate::app::DockSurface::Home);
        app.state.view.dock_rect = Rect::new(80, 0, 20, 20);
        app.state.view.dock_handle_rect = Rect::new(99, 0, 1, 20);
        app.state.view.dock_tab_hit_areas = vec![
            Rect::new(81, 0, 6, 1),
            Rect::new(87, 0, 6, 1),
            Rect::new(93, 0, 6, 1),
        ];

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 94, 0));

        assert_eq!(app.state.dock_tab, Some(crate::app::DockSurface::Shortcuts));
        assert!(!app.state.dock_home_focused);

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 82, 0));

        assert_eq!(app.state.dock_tab, Some(crate::app::DockSurface::Home));
        assert!(app.state.dock_home_focused);
    }

    #[test]
    fn files_header_clicks_route_sort_and_refresh_actions() {
        let mut app = app_for_mouse_test();
        app.state.mode = Mode::Terminal;
        app.state.dock_collapsed = false;
        app.state.dock_tab = Some(crate::app::DockSurface::Files);
        app.state.view.dock_rect = Rect::new(80, 0, 20, 20);
        app.state.view.dock_files_refresh_rect = Rect::new(81, 3, 3, 1);
        app.state.view.dock_files_sort_rect = Rect::new(90, 3, 9, 1);
        let mut workspace = Workspace::test_new("files-header");
        workspace.identity_cwd = std::env::current_dir().expect("current directory");
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let cached_root = std::path::PathBuf::from("/cached-tree");
        app.state.dock_files_root = Some(cached_root.clone());
        app.state.dock_file_cache.insert(
            cached_root.clone(),
            crate::files::FileTreeSnapshot {
                root: cached_root.clone(),
                files: Vec::new(),
                fingerprint: 1,
                source: crate::files::FileTreeSource::Git,
                error: None,
            },
        );

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 92, 3));
        assert_eq!(app.state.dock_files_sort, crate::files::FileSort::Type);

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 82, 3));
        assert!(app.files_refresh_in_flight.is_some());
        assert!(!app.state.dock_file_cache.contains_key(&cached_root));
        assert!(app.state.dock_files_root.is_none());
    }

    #[test]
    fn clicking_an_empty_panel_card_opens_it_as_the_first_tab() {
        let mut app = app_for_mouse_test();
        app.state.mode = Mode::Terminal;
        app.state.dock_collapsed = false;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let home_index = crate::app::DockSurface::CARDS
            .iter()
            .position(|surface| *surface == crate::app::DockSurface::Home)
            .expect("Home card");
        let card = app.state.view.dock_surface_card_hit_areas[home_index];

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            card.x + 1,
            card.y + 1,
        ));

        assert_eq!(
            app.state.dock_open_surfaces,
            vec![crate::app::DockSurface::Home]
        );
        assert_eq!(app.state.dock_tab, Some(crate::app::DockSurface::Home));
    }

    #[test]
    fn a_work_link_in_the_context_tab_copies_the_same_value_the_panel_would() {
        let mut app = app_for_mouse_test();
        app.state.mode = Mode::Terminal;
        app.state.workspaces = vec![Workspace::test_new("links")];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].focused_pane_id().expect("pane");
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("terminal");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                ticket_ids: Some(vec!["MAT-128".into()]),
                ..Default::default()
            })
            .expect("work context");
        app.state.dock_collapsed = false;
        app.state.dock_tab = Some(crate::app::DockSurface::Context);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 30));
        let link = app
            .state
            .view
            .info_panel_link_rows
            .first()
            .expect("dock context link row")
            .clone();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            link.rect.x + 1,
            link.rect.y,
        ));

        match app.event_rx.try_recv().expect("clipboard event") {
            crate::events::AppEvent::ClipboardWrite { content } => {
                assert_eq!(content, b"MAT-128")
            }
            event => panic!("unexpected event: {event:?}"),
        }
    }

    #[test]
    fn a_tab_click_on_real_geometry_selects_the_tab_instead_of_folding_the_dock() {
        // The hand-placed rects above cannot catch a geometry mistake, and one shipped:
        // the handle covered the whole open dock, so every click inside it toggled.
        let mut app = app_for_mouse_test();
        app.state.mode = Mode::Terminal;
        app.state.dock_collapsed = false;
        app.state.dock_open_surfaces = vec![
            crate::app::DockSurface::Home,
            crate::app::DockSurface::Editor,
            crate::app::DockSurface::Shortcuts,
        ];
        app.state.dock_tab = Some(crate::app::DockSurface::Home);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 30));
        let shortcuts_index = app
            .state
            .dock_open_surfaces
            .iter()
            .position(|tab| *tab == crate::app::DockSurface::Shortcuts)
            .expect("Shortcuts tab");
        let shortcuts = app.state.view.dock_tab_hit_areas[shortcuts_index];

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            shortcuts.x + 1,
            shortcuts.y,
        ));

        assert!(!app.state.dock_collapsed);
        assert_eq!(app.state.dock_tab, Some(crate::app::DockSurface::Shortcuts));
    }

    #[test]
    fn dock_home_tab_click_selects_then_second_click_jumps() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            Workspace::test_new("current"),
            Workspace::test_new("review"),
        ];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[1].focused_pane_id().expect("pane");
        let terminal_id = app.state.workspaces[1]
            .terminal_id(pane_id)
            .cloned()
            .expect("terminal");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                pr_urls: Some(vec!["https://github.com/herdrdev/herdr/pull/125".into()]),
                work_title: Some("review dock home".into()),
                role: Some(crate::work_context::PaneWorkRole::Review),
                active_owner: Some(true),
                ..Default::default()
            })
            .expect("work context");
        app.state.dock_collapsed = false;
        app.state.dock_tab = Some(crate::app::DockSurface::Home);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 30));
        let tab = app.state.view.dock_home_tab_hit_areas[0];
        let expected_key = app.state.dock_home_projection().rows[0].key.clone();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            tab.x + 1,
            tab.y,
        ));

        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.dock_home_selection.as_ref(), Some(&expected_key));
        assert!(app.state.dock_home_focused);

        // Esc removes keyboard focus but leaves the visibly selected tab.
        app.state.dock_home_focused = false;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            tab.x + 1,
            tab.y,
        ));

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.workspaces[1].focused_pane_id(), Some(pane_id));
        assert!(!app.state.dock_home_focused);
    }

    #[test]
    fn dock_home_tab_click_selects_while_the_focused_pane_is_unbound() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            Workspace::test_new("current"),
            Workspace::test_new("review"),
        ];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[1].focused_pane_id().expect("pane");
        let terminal_id = app.state.workspaces[1]
            .terminal_id(pane_id)
            .cloned()
            .expect("terminal");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                pr_urls: Some(vec!["https://github.com/herdrdev/herdr/pull/125".into()]),
                work_title: Some("review dock home".into()),
                role: Some(crate::work_context::PaneWorkRole::Review),
                active_owner: Some(true),
                ..Default::default()
            })
            .expect("work context");
        app.state.dock_collapsed = false;
        app.state.dock_tab = Some(crate::app::DockSurface::Home);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 30));
        let tab = app.state.view.dock_home_tab_hit_areas[0];
        let expected_key = app.state.dock_home_projection().rows[0].key.clone();
        // The focused pane carries no work item, so nothing is selected on
        // screen even though a key is still stored from an earlier pane.
        app.state.dock_home_selection = Some(expected_key.clone());
        app.state.dock_home_focus_unbound = true;
        assert_eq!(
            app.state
                .dock_home_selected_index(&app.state.dock_home_projection()),
            None
        );

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            tab.x + 1,
            tab.y,
        ));

        // The click selects rather than jumping: the row is now visible.
        assert!(!app.state.dock_home_focus_unbound);
        assert_eq!(
            app.state
                .dock_home_selected_index(&app.state.dock_home_projection()),
            Some(0)
        );
        assert_eq!(app.state.dock_home_selection.as_ref(), Some(&expected_key));
        assert_eq!(app.state.active, Some(0));
        assert!(app.state.dock_home_focused);
    }

    #[test]
    fn stale_dock_home_tab_does_not_jump_to_a_replacement_projection_tab() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            Workspace::test_new("current"),
            Workspace::test_new("first review"),
            Workspace::test_new("second review"),
        ];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        let mut review_terminal_ids = Vec::new();
        for (workspace_index, number) in [(1, 125), (2, 126)] {
            let pane_id = app.state.workspaces[workspace_index]
                .focused_pane_id()
                .expect("pane");
            let terminal_id = app.state.workspaces[workspace_index]
                .terminal_id(pane_id)
                .cloned()
                .expect("terminal");
            app.state
                .terminals
                .get_mut(&terminal_id)
                .expect("terminal state")
                .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                    pr_urls: Some(vec![format!(
                        "https://github.com/herdrdev/herdr/pull/{number}"
                    )]),
                    work_title: Some(format!("review {number}")),
                    role: Some(crate::work_context::PaneWorkRole::Review),
                    active_owner: Some(true),
                    ..Default::default()
                })
                .expect("work context");
            review_terminal_ids.push(terminal_id);
        }
        app.state.dock_collapsed = false;
        app.state.dock_tab = Some(crate::app::DockSurface::Home);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 30));
        let stale_first_tab = app.state.view.dock_home_tab_hit_areas[0];
        let second_key = app.state.dock_home_projection().rows[1].key.clone();
        app.state.dock_home_selection = Some(second_key.clone());
        app.state.dock_home_focused = true;

        app.state
            .terminals
            .get_mut(&review_terminal_ids[0])
            .expect("first terminal")
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                pr_urls: Some(Vec::new()),
                ..Default::default()
            })
            .expect("clear first work context");
        let current_projection = app.state.dock_home_projection();
        assert_eq!(current_projection.rows.len(), 1);
        assert_eq!(current_projection.rows[0].key, second_key);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            stale_first_tab.x + 1,
            stale_first_tab.y,
        ));

        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.dock_home_selection, Some(second_key));
        assert!(app.state.dock_home_focused);
    }

    #[test]
    fn dragging_the_dock_divider_resizes_and_persists_the_width() {
        let mut app = app_for_mouse_test();
        app.state.mode = Mode::Terminal;
        app.state.dock_collapsed = false;
        app.state.dock_width = 30;
        app.state.view.terminal_area = Rect::new(0, 0, 50, 20);
        app.state.view.dock_rect = Rect::new(50, 0, 30, 20);
        app.state.view.dock_divider_rect = Rect::new(50, 0, 1, 20);
        app.state.view.dock_handle_rect = Rect::new(79, 0, 1, 20);

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 50, 5));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 55, 5));

        assert_eq!(app.state.dock_width, 25);
        assert_eq!(
            app.state.take_dock_width_persistence_request(),
            Some(25),
            "dock resize must emit the client-local persistence update"
        );
        assert!(!app.state.session_dirty);
    }

    #[test]
    fn ac26_info_panel_link_click_copies_without_opening() {
        let mut app = app_for_mouse_test();
        app.state.mode = Mode::Terminal;
        app.state.view.info_panel_link_rows = vec![InfoPanelLinkRow {
            rect: Rect::new(60, 5, 30, 1),
            copy_value: "MAT-124".into(),
        }];

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 62, 5));

        match app.event_rx.try_recv().expect("link click clipboard event") {
            crate::events::AppEvent::ClipboardWrite { content } => {
                assert_eq!(content, b"MAT-124")
            }
            event => panic!("unexpected event: {event:?}"),
        }
        assert_eq!(
            app.state
                .copy_feedback
                .as_ref()
                .map(|feedback| feedback.message.as_str()),
            Some("copied")
        );
    }

    #[test]
    fn ac26_narrow_hidden_info_panel_does_not_copy_on_click() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        app.state.info_panel_expanded = true;
        app.state.view.info_panel_link_rows = vec![InfoPanelLinkRow {
            rect: Rect::new(30, 3, 30, 1),
            copy_value: "stale".into(),
        }];

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 65, 20));

        assert_eq!(app.state.view.info_panel_rect, Rect::default());
        assert!(app.state.view.info_panel_link_rows.is_empty());
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 40, 3));
        assert!(app.event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn terminal_wheel_uses_configured_mouse_scroll_lines() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        ws.tabs[0].runtimes.insert(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                16 * 1024,
                &numbered_lines_bytes(64),
            ),
        );

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;
        app.state.mouse_scroll_lines = 7;

        app.handle_mouse(mouse(
            MouseEventKind::ScrollUp,
            info.inner_rect.x + 1,
            info.inner_rect.y + 1,
        ));

        let metrics = app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .and_then(crate::terminal::TerminalRuntime::scroll_metrics)
            .expect("scroll metrics after wheel");
        assert_eq!(metrics.offset_from_bottom, 7);
    }

    #[tokio::test]
    async fn mouse_dispatcher_forwards_horizontal_wheel_to_mouse_reporting_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                0,
                b"\x1b[?1000h\x1b[?1006h",
                4,
            );
        ws.insert_test_runtime(pane_id, runtime);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;
        assert!(
            app.state.mouse_capture,
            "reproduction must use the default Herdr mouse dispatcher"
        );

        let outer_column = info.inner_rect.x + 2;
        let outer_row = info.inner_rect.y + 3;
        for (button, expected_kind, ingress) in [
            (66, MouseEventKind::ScrollLeft, "monolithic"),
            (67, MouseEventKind::ScrollRight, "headless"),
        ] {
            let input = format!("\x1b[<{button};{};{}M", outer_column + 1, outer_row + 1);
            let mut events = crate::raw_input::parse_raw_input_bytes_sync(input.as_bytes());
            let event = events
                .pop()
                .expect("horizontal SGR wheel input should parse");
            let crate::raw_input::RawInputEvent::Mouse(mouse) = &event else {
                panic!("expected parsed mouse event");
            };
            assert!(events.is_empty(), "expected one parsed mouse event");
            assert_eq!(mouse.kind, expected_kind);

            if ingress == "monolithic" {
                assert!(app.handle_raw_input_event(event).await);
            } else {
                app.route_client_events(vec![event], false);
            }

            assert_eq!(
                input_rx
                    .try_recv()
                    .expect("horizontal wheel should reach pane"),
                Bytes::from(format!("\x1b[<{button};3;4M"))
            );
        }
        assert!(input_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn symphony_overlay_blocks_monolithic_and_headless_mouse_passthrough() {
        let mut app = app_for_mouse_test();
        let mut workspace = Workspace::test_new("test");
        let pane_id = workspace.tabs[0].root_pane;
        let pane_infos = workspace.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                0,
                b"\x1b[?1000h\x1b[?1006h",
                4,
            );
        workspace.insert_test_runtime(pane_id, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;
        app.state.toggle_symphony();
        let event = mouse(
            MouseEventKind::Down(MouseButton::Left),
            info.inner_rect.x + 2,
            info.inner_rect.y + 3,
        );

        app.handle_mouse(event);
        app.state.mouse_capture = false;
        app.route_client_events(vec![crate::raw_input::RawInputEvent::Mouse(event)], false);

        assert!(input_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn horizontal_wheel_stays_inert_for_non_mouse_reporting_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                0,
                b"",
                1,
            );
        ws.insert_test_runtime(pane_id, runtime);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;

        let input = format!(
            "\x1b[<66;{};{}M",
            info.inner_rect.x + 3,
            info.inner_rect.y + 4
        );
        let event = crate::raw_input::parse_raw_input_bytes_sync(input.as_bytes())
            .pop()
            .expect("horizontal SGR wheel input should parse");

        assert!(app.handle_raw_input_event(event).await);

        assert!(input_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn pane_right_click_passthrough_is_isolated() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let passthrough_pane = ws.tabs[0].root_pane;
        let default_pane = ws.test_split(Direction::Horizontal);
        ws.pane_state_mut(passthrough_pane)
            .unwrap()
            .right_click_passthrough = true;
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let passthrough_info = app.state.pane_info_by_id(passthrough_pane).unwrap().clone();
        let default_info = app.state.pane_info_by_id(default_pane).unwrap().clone();
        let (passthrough_runtime, mut passthrough_input) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                passthrough_info.inner_rect.width,
                passthrough_info.inner_rect.height,
                0,
                b"\x1b[?1002h\x1b[?1006h",
                4,
            );
        let (default_runtime, mut default_input) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                default_info.inner_rect.width,
                default_info.inner_rect.height,
                0,
                b"\x1b[?1002h\x1b[?1006h",
                4,
            );
        app.state
            .insert_test_runtime(passthrough_pane, passthrough_runtime);
        app.state.insert_test_runtime(default_pane, default_runtime);

        let col = passthrough_info.inner_rect.x + 2;
        let row = passthrough_info.inner_rect.y + 3;
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Right), col, row));

        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(app.state.context_menu.is_none());
        assert_eq!(
            passthrough_input.try_recv().unwrap(),
            Bytes::from_static(b"\x1b[<2;3;4M")
        );

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            default_info.inner_rect.x + 2,
            default_info.inner_rect.y + 3,
        ));

        assert!(default_input.try_recv().is_err());
        assert!(matches!(
            app.state.context_menu.as_ref().map(|menu| &menu.kind),
            Some(ContextMenuKind::Pane { pane_id, .. }) if *pane_id == default_pane
        ));
    }

    fn app_with_pane_screen(
        bytes: &[u8],
        splits: usize,
    ) -> (App, Vec<PaneId>, crate::layout::PaneInfo) {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        for _ in 0..splits {
            ws.test_split(Direction::Horizontal);
        }
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let pane_ids = app.state.window_pane_ids(0, 0);
        for pane_id in pane_ids.iter().copied() {
            let info = app.state.pane_info_by_id(pane_id).unwrap().clone();
            app.state.insert_test_runtime(
                pane_id,
                crate::terminal::TerminalRuntime::test_with_screen_bytes(
                    info.inner_rect.width,
                    info.inner_rect.height,
                    bytes,
                ),
            );
            let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.state.terminals.insert(
                terminal_id.clone(),
                crate::terminal::TerminalState::new(terminal_id, "/tmp".into()),
            );
        }
        let info = app.state.pane_info_by_id(pane_ids[0]).unwrap().clone();
        (app, pane_ids, info)
    }

    fn pane_work_context(app: &App, pane_id: PaneId) -> crate::work_context::PaneWorkContext {
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state.terminals[&terminal_id]
            .effective_work_context()
            .clone()
    }

    const PR_URL: &str = "https://github.com/herdrdev/herdr/pull/398";

    fn bind_manually(
        app: &mut App,
        panes: &[PaneId],
        build: impl Fn(&mut crate::work_context::PaneWorkContextPatch),
    ) {
        for pane_id in panes {
            let terminal_id = app.state.workspaces[0].tabs[0].panes[pane_id]
                .attached_terminal_id
                .clone();
            let mut patch = crate::work_context::PaneWorkContextPatch::default();
            build(&mut patch);
            app.state
                .terminals
                .get_mut(&terminal_id)
                .expect("terminal")
                .apply_manual_work_context_patch(patch)
                .expect("manual work-context patch");
        }
    }

    fn right_click_link(app: &mut App, info: &crate::layout::PaneInfo, line: &str, needle: &str) {
        let col = line.find(needle).expect("link host") as u16;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            info.inner_rect.x + col,
            info.inner_rect.y,
        ));
    }

    fn click_menu_item(app: &mut App, item: &str) {
        let item_idx = app
            .state
            .context_menu
            .as_ref()
            .expect("pane context menu")
            .items()
            .iter()
            .position(|candidate| *candidate == item)
            .expect("menu item");
        let menu_rect = app.state.context_menu_rect().expect("menu rect");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu_rect.x + 1,
            menu_rect.y + 1 + item_idx as u16,
        ));
    }

    #[tokio::test]
    async fn right_click_on_pr_url_offers_linking_it_to_the_window() {
        let line = "opened https://github.com/herdrdev/herdr/pull/398 for review";
        let (mut app, _panes, info) = app_with_pane_screen(line.as_bytes(), 0);

        right_click_link(&mut app, &info, line, "github");

        let menu = app.state.context_menu.as_ref().expect("pane context menu");
        assert!(matches!(
            &menu.kind,
            ContextMenuKind::Pane {
                linkable_work_link: Some(PaneMenuWorkLinkAction {
                    link: PaneMenuWorkLink::PullRequest(url),
                    unlink: false,
                }),
                ..
            } if url == "https://github.com/herdrdev/herdr/pull/398"
        ));
        assert!(menu
            .items()
            .contains(&crate::app::state::LINK_PR_TO_WINDOW_ITEM));
    }

    #[tokio::test]
    async fn right_click_on_a_ticket_url_offers_linking_the_ticket() {
        let line = "tracking https://linear.app/scalable/issue/SCA-412/sidebar for review";
        let (mut app, _panes, info) = app_with_pane_screen(line.as_bytes(), 0);

        right_click_link(&mut app, &info, line, "linear");

        let menu = app.state.context_menu.as_ref().expect("pane context menu");
        assert!(matches!(
            &menu.kind,
            ContextMenuKind::Pane {
                linkable_work_link: Some(PaneMenuWorkLinkAction {
                    link: PaneMenuWorkLink::Ticket(id),
                    unlink: false,
                }),
                ..
            } if id == "SCA-412"
        ));
        assert!(menu
            .items()
            .contains(&crate::app::state::LINK_TICKET_TO_WINDOW_ITEM));
    }

    #[tokio::test]
    async fn right_click_away_from_a_work_link_offers_no_link_item() {
        let line = "opened https://github.com/herdrdev/herdr/issues/398 for review";
        let (mut app, _panes, info) = app_with_pane_screen(line.as_bytes(), 0);

        right_click_link(&mut app, &info, line, "github");

        let menu = app.state.context_menu.as_ref().expect("pane context menu");
        assert!(matches!(
            &menu.kind,
            ContextMenuKind::Pane {
                linkable_work_link: None,
                ..
            }
        ));
        assert!(!menu
            .items()
            .contains(&crate::app::state::LINK_PR_TO_WINDOW_ITEM));
    }

    #[tokio::test]
    async fn right_click_on_a_pr_the_window_already_carries_offers_no_link_item() {
        let line = "opened https://github.com/herdrdev/herdr/pull/398 for review";
        let (mut app, panes, info) = app_with_pane_screen(line.as_bytes(), 1);
        for pane_id in &panes {
            let terminal_id = app.state.workspaces[0].tabs[0].panes[pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .expect("terminal")
                .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                    pr_urls: Some(vec!["https://github.com/herdrdev/herdr/pull/398".into()]),
                    ..Default::default()
                })
                .expect("link pull request");
        }

        right_click_link(&mut app, &info, line, "github");

        let menu = app.state.context_menu.as_ref().expect("pane context menu");
        assert!(!menu
            .items()
            .contains(&crate::app::state::LINK_PR_TO_WINDOW_ITEM));
    }

    #[tokio::test]
    async fn a_pr_bound_to_one_pane_only_is_still_offered_for_the_window() {
        let line = "opened https://github.com/herdrdev/herdr/pull/398 for review";
        let (mut app, panes, info) = app_with_pane_screen(line.as_bytes(), 1);
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&panes[0]]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal")
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                pr_urls: Some(vec!["https://github.com/herdrdev/herdr/pull/398".into()]),
                ..Default::default()
            })
            .expect("link pull request");

        right_click_link(&mut app, &info, line, "github");

        assert!(app
            .state
            .context_menu
            .as_ref()
            .expect("pane context menu")
            .items()
            .contains(&crate::app::state::LINK_PR_TO_WINDOW_ITEM));
    }

    #[tokio::test]
    async fn clicking_link_pr_binds_the_pull_request_to_every_pane_of_the_window() {
        let line = "opened https://github.com/herdrdev/herdr/pull/398 for review";
        let (mut app, panes, info) = app_with_pane_screen(line.as_bytes(), 1);
        assert_eq!(panes.len(), 2);

        right_click_link(&mut app, &info, line, "github");
        click_menu_item(&mut app, crate::app::state::LINK_PR_TO_WINDOW_ITEM);

        assert_eq!(app.state.mode, Mode::Terminal);
        for pane_id in panes {
            assert_eq!(
                pane_work_context(&app, pane_id).pr_urls,
                vec!["https://github.com/herdrdev/herdr/pull/398".to_string()]
            );
        }
    }

    #[tokio::test]
    async fn right_click_on_a_manually_linked_pr_offers_unlinking_it() {
        let line = "opened https://github.com/herdrdev/herdr/pull/398 for review";
        let (mut app, panes, info) = app_with_pane_screen(line.as_bytes(), 1);
        bind_manually(&mut app, &panes, |patch| {
            patch.pr_urls = Some(vec![PR_URL.into()])
        });

        right_click_link(&mut app, &info, line, "github");

        let menu = app.state.context_menu.as_ref().expect("pane context menu");
        assert!(menu
            .items()
            .contains(&crate::app::state::UNLINK_PR_FROM_WINDOW_ITEM));
        assert!(!menu
            .items()
            .contains(&crate::app::state::LINK_PR_TO_WINDOW_ITEM));
    }

    #[tokio::test]
    async fn clicking_unlink_drops_the_pull_request_from_every_pane_of_the_window() {
        let line = "opened https://github.com/herdrdev/herdr/pull/398 for review";
        let (mut app, panes, info) = app_with_pane_screen(line.as_bytes(), 1);
        bind_manually(&mut app, &panes, |patch| {
            patch.pr_urls = Some(vec![PR_URL.into()])
        });

        right_click_link(&mut app, &info, line, "github");
        click_menu_item(&mut app, crate::app::state::UNLINK_PR_FROM_WINDOW_ITEM);

        for pane_id in &panes {
            assert!(pane_work_context(&app, *pane_id).pr_urls.is_empty());
        }
        let toast = app.state.toast.as_ref().expect("unlink toast");
        assert_eq!(toast.title, "unlinked #398");
    }

    #[tokio::test]
    async fn clicking_unlink_drops_the_ticket_from_every_pane_of_the_window() {
        let line = "tracking https://linear.app/scalable/issue/SCA-412/sidebar for review";
        let (mut app, panes, info) = app_with_pane_screen(line.as_bytes(), 1);
        bind_manually(&mut app, &panes, |patch| {
            patch.ticket_ids = Some(vec!["SCA-412".into()])
        });

        right_click_link(&mut app, &info, line, "linear");
        click_menu_item(&mut app, crate::app::state::UNLINK_TICKET_FROM_WINDOW_ITEM);

        for pane_id in &panes {
            assert!(pane_work_context(&app, *pane_id).ticket_ids.is_empty());
        }
        assert_eq!(
            app.state.toast.as_ref().expect("unlink toast").title,
            "unlinked SCA-412"
        );
    }

    #[tokio::test]
    async fn an_observed_pull_request_offers_neither_link_nor_unlink() {
        let line = "opened https://github.com/herdrdev/herdr/pull/398 for review";
        let (mut app, panes, info) = app_with_pane_screen(line.as_bytes(), 1);
        for pane_id in &panes {
            let terminal_id = app.state.workspaces[0].tabs[0].panes[pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .expect("terminal")
                .replace_git_work_context(crate::work_context::PaneWorkContext {
                    pr_urls: vec![PR_URL.into()],
                    ..Default::default()
                })
                .expect("observe pull request");
        }

        right_click_link(&mut app, &info, line, "github");

        let menu = app.state.context_menu.as_ref().expect("pane context menu");
        assert!(!menu
            .items()
            .contains(&crate::app::state::LINK_PR_TO_WINDOW_ITEM));
        assert!(!menu
            .items()
            .contains(&crate::app::state::UNLINK_PR_FROM_WINDOW_ITEM));
    }

    #[tokio::test]
    async fn linking_a_pull_request_shows_a_toast() {
        let line = "opened https://github.com/herdrdev/herdr/pull/398 for review";
        let (mut app, panes, info) = app_with_pane_screen(line.as_bytes(), 1);

        right_click_link(&mut app, &info, line, "github");
        click_menu_item(&mut app, crate::app::state::LINK_PR_TO_WINDOW_ITEM);

        let toast = app.state.toast.as_ref().expect("link toast");
        assert_eq!(toast.kind, crate::app::state::ToastKind::WorkLinked);
        assert_eq!(toast.title, "linked #398");
        assert!(
            toast.context.ends_with(&format!("· {} panes", panes.len())),
            "unexpected toast context: {}",
            toast.context
        );
        assert!(toast.target.is_none());
    }

    #[tokio::test]
    async fn linking_a_ticket_shows_a_toast_naming_the_ticket() {
        let line = "tracking https://linear.app/scalable/issue/SCA-412/sidebar for review";
        let (mut app, _panes, info) = app_with_pane_screen(line.as_bytes(), 0);

        right_click_link(&mut app, &info, line, "linear");
        click_menu_item(&mut app, crate::app::state::LINK_TICKET_TO_WINDOW_ITEM);

        let toast = app.state.toast.as_ref().expect("link toast");
        assert_eq!(toast.title, "linked SCA-412");
        assert!(
            toast.context.ends_with("· 1 pane"),
            "unexpected toast context: {}",
            toast.context
        );
    }

    #[tokio::test]
    async fn clicking_link_ticket_binds_the_ticket_to_every_pane_of_the_window() {
        let line = "tracking https://linear.app/scalable/issue/SCA-412/sidebar for review";
        let (mut app, panes, info) = app_with_pane_screen(line.as_bytes(), 1);

        right_click_link(&mut app, &info, line, "linear");
        click_menu_item(&mut app, crate::app::state::LINK_TICKET_TO_WINDOW_ITEM);

        for pane_id in panes {
            assert_eq!(
                pane_work_context(&app, pane_id).ticket_ids,
                vec!["SCA-412".to_string()]
            );
        }
    }

    #[tokio::test]
    async fn pane_right_click_passthrough_falls_back_when_mouse_reporting_is_off() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        ws.pane_state_mut(pane_id).unwrap().right_click_passthrough = true;
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let info = app.state.pane_info_by_id(pane_id).unwrap().clone();
        app.state.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                b"",
            ),
        );

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            info.inner_rect.x + 2,
            info.inner_rect.y + 3,
        ));

        assert_eq!(app.state.mode, Mode::ContextMenu);
        assert!(app.state.context_menu.is_some());
    }

    #[tokio::test]
    async fn configured_right_click_passthrough_forwards_gesture_outside_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                0,
                b"\x1b[?1002h\x1b[?1006h",
                4,
            );
        ws.insert_test_runtime(pane_id, runtime);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;
        app.state.right_click_passthrough_modifiers = Some(KeyModifiers::CONTROL);

        let col = info.inner_rect.x + 2;
        let row = info.inner_rect.y + 3;
        app.handle_mouse(MouseEvent {
            modifiers: KeyModifiers::CONTROL,
            ..mouse(MouseEventKind::Down(MouseButton::Right), col, row)
        });
        app.handle_mouse(MouseEvent {
            modifiers: KeyModifiers::CONTROL,
            ..mouse(MouseEventKind::Drag(MouseButton::Right), 0, 0)
        });
        app.handle_mouse(MouseEvent {
            modifiers: KeyModifiers::CONTROL,
            ..mouse(MouseEventKind::Up(MouseButton::Right), 0, 0)
        });

        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(app.state.context_menu.is_none());
        assert!(app.state.right_click_passthrough.is_none());
        assert_eq!(
            input_rx.try_recv().expect("forwarded right mouse down"),
            Bytes::from_static(b"\x1b[<2;3;4M")
        );
        assert_eq!(
            input_rx.try_recv().expect("forwarded right mouse drag"),
            Bytes::from_static(b"\x1b[<34;1;1M")
        );
        assert_eq!(
            input_rx.try_recv().expect("forwarded right mouse up"),
            Bytes::from_static(b"\x1b[<2;1;1m")
        );
        assert!(input_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn captured_left_press_focuses_target_before_forwarding() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let source = ws.tabs[0].root_pane;
        let target = ws.test_split(Direction::Horizontal);
        ws.tabs[0].layout.focus_pane(source);
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let info = app
            .state
            .pane_info_by_id(target)
            .expect("target pane info")
            .clone();
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                0,
                b"\x1b[?1002h\x1b[?1006h",
                4,
            );
        app.state.insert_test_runtime(target, runtime);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            info.inner_rect.x + 1,
            info.inner_rect.y + 1,
        ));

        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(target));
        assert_eq!(
            input_rx.try_recv().expect("forwarded captured left press"),
            Bytes::from_static(b"\x1b[<0;2;2M")
        );
    }

    #[tokio::test]
    async fn pane_mouse_only_forwards_moved_events_for_any_motion_apps() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                0,
                b"\x1b[?1003h\x1b[?1006h",
                4,
            );
        ws.insert_test_runtime(pane_id, runtime);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;

        app.state.handle_pane_mouse_only(
            &app.terminal_runtimes,
            mouse(
                MouseEventKind::Moved,
                info.inner_rect.x + 2,
                info.inner_rect.y + 3,
            ),
        );

        assert_eq!(
            input_rx.try_recv().expect("forwarded mouse motion"),
            Bytes::from_static(b"\x1b[<35;3;4M")
        );
        assert!(input_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn pane_mouse_motion_uses_computed_inner_rect_offsets() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80,
                18,
                0,
                b"\x1b[?1003h\x1b[?1006h",
                4,
            );
        ws.insert_test_runtime(pane_id, runtime);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let info = app.state.view.pane_infos[0].clone();
        assert!(info.inner_rect.x > 0, "sidebar offset should be present");
        assert!(info.inner_rect.y > 0, "tab bar offset should be present");

        app.state.handle_pane_mouse_only(
            &app.terminal_runtimes,
            mouse(
                MouseEventKind::Moved,
                info.inner_rect.x + 2,
                info.inner_rect.y + 3,
            ),
        );

        assert_eq!(
            input_rx.try_recv().expect("forwarded mouse motion"),
            Bytes::from_static(b"\x1b[<35;3;4M")
        );
        assert!(input_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn ordinary_cell_mouse_downgrades_pixel_mode_to_cell_coordinates() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80,
                18,
                0,
                b"\x1b[?1003h\x1b[?1006h\x1b[?1016h",
                4,
            );
        ws.insert_test_runtime(pane_id, runtime);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.host_cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 10,
            height_px: 20,
        };
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let info = app.state.view.pane_infos[0].clone();
        app.state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .unwrap()
            .resize(info.inner_rect.height, info.inner_rect.width, 10, 20);
        assert!(info.inner_rect.x > 0, "sidebar offset should be present");
        assert!(info.inner_rect.y > 0, "tab bar offset should be present");

        app.handle_mouse(mouse(
            MouseEventKind::Moved,
            info.inner_rect.x + 2,
            info.inner_rect.y + 3,
        ));

        assert_eq!(
            input_rx.try_recv().expect("forwarded mouse motion"),
            Bytes::from_static(b"\x1b[<35;3;4M")
        );
        assert!(input_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn dedicated_client_pixel_mouse_preserves_subcell_position() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80,
                18,
                0,
                b"\x1b[?1003h\x1b[?1006h\x1b[?1016h",
                4,
            );
        ws.insert_test_runtime(pane_id, runtime);
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.mouse_capture = false;
        app.state.host_cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 10,
            height_px: 20,
        };
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let inner = app.state.view.pane_infos[0].inner_rect;
        let geometry = crate::input::mouse::HostGeometry::new(106, 20, 1_060, 400).unwrap();
        let x = u32::from(inner.x + 2) * 10 + 8;
        let y = u32::from(inner.y + 3) * 20 + 9;
        let report = format!("\x1b[<35;{x};{y}M");
        app.state.host_mouse_pixels = Some(crate::input::mouse::HostPixels { x, y, geometry });
        let runtime = app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .unwrap();
        assert_eq!(runtime.pixel_size(), None);
        assert_eq!(
            app.state.pane_mouse_position(
                runtime,
                inner,
                mouse(MouseEventKind::Moved, inner.x + 2, inner.y + 3),
            ),
            Some(crate::input::mouse::Position::Cell { column: 2, row: 3 })
        );
        runtime.resize(inner.height, inner.width, 10, 20);
        let runtime = app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .unwrap();
        assert_eq!(
            runtime.pixel_size(),
            Some((u32::from(inner.width) * 10, u32::from(inner.height) * 20))
        );
        assert_eq!(
            app.state.pane_mouse_position(
                runtime,
                inner,
                mouse(MouseEventKind::Moved, inner.x + 2, inner.y + 3),
            ),
            Some(crate::input::mouse::Position::Pixels { x: 28, y: 69 })
        );
        app.state.host_mouse_pixels = None;

        assert!(app.route_client_pixel_mouse(7, report.as_bytes(), geometry));
        assert_eq!(
            input_rx.try_recv().expect("forwarded exact mouse motion"),
            Bytes::from_static(b"\x1b[<35;28;69M")
        );
        assert!(input_rx.try_recv().is_err());
        assert!(app.state.host_mouse_pixels.is_none());
    }

    #[tokio::test]
    async fn mouse_dispatcher_does_not_forward_motion_behind_herdr_modes() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80,
                18,
                0,
                b"\x1b[?1003h\x1b[?1006h",
                4,
            );
        ws.insert_test_runtime(pane_id, runtime);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Navigate;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let info = app.state.view.pane_infos[0].clone();

        app.handle_mouse(mouse(
            MouseEventKind::Moved,
            info.inner_rect.x + 2,
            info.inner_rect.y + 3,
        ));

        assert!(input_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn unset_right_click_passthrough_keeps_modified_right_click_as_herdr_menu() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                0,
                b"\x1b[?1002h\x1b[?1006h",
                4,
            );
        ws.insert_test_runtime(pane_id, runtime);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;
        app.state.right_click_passthrough_modifiers = None;

        app.handle_mouse(MouseEvent {
            modifiers: KeyModifiers::CONTROL,
            ..mouse(
                MouseEventKind::Down(MouseButton::Right),
                info.inner_rect.x + 2,
                info.inner_rect.y + 3,
            )
        });

        assert_eq!(app.state.mode, Mode::ContextMenu);
        assert!(app.state.context_menu.is_some());
        assert!(app.state.right_click_passthrough.is_none());
        assert!(input_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn pane_right_click_keeps_focus_and_swap_menu_swaps_with_focused_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let source = ws.tabs[0].root_pane;
        let target = ws.test_split(Direction::Horizontal);
        ws.tabs[0].layout.focus_pane(source);
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 100, 20));
        let target_info = app
            .state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == target)
            .expect("target pane info")
            .clone();
        let source_rect_before = app
            .state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == source)
            .expect("source pane info")
            .rect;
        let target_rect_before = target_info.rect;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            target_info.inner_rect.x,
            target_info.inner_rect.y,
        ));

        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(source));
        let menu = app.state.context_menu.as_mut().expect("pane context menu");
        assert!(matches!(
            menu.kind,
            ContextMenuKind::Pane {
                pane_id,
                source_pane_id: Some(source_pane_id),
                ..
            } if pane_id == target && source_pane_id == source
        ));
        let swap_idx = menu
            .items()
            .iter()
            .position(|item| *item == "Swap with focused pane")
            .expect("swap item");
        menu.list.highlighted = swap_idx;

        handle_context_menu_key(
            &mut app.state,
            &mut app.terminal_runtimes,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 100, 20));

        assert_eq!(app.state.mode, Mode::Terminal);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(source));
        assert_eq!(
            app.state
                .view
                .pane_infos
                .iter()
                .find(|info| info.id == source)
                .unwrap()
                .rect,
            target_rect_before
        );
        assert_eq!(
            app.state
                .view
                .pane_infos
                .iter()
                .find(|info| info.id == target)
                .unwrap()
                .rect,
            source_rect_before
        );
    }

    #[tokio::test]
    async fn normal_right_click_keeps_focus_and_exposes_swap_for_reporting_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let source = ws.tabs[0].root_pane;
        let target = ws.test_split(Direction::Horizontal);
        ws.tabs[0].layout.focus_pane(source);
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 100, 20));
        let target_info = app
            .state
            .pane_info_by_id(target)
            .expect("target pane info")
            .clone();
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                target_info.inner_rect.width,
                target_info.inner_rect.height,
                0,
                b"\x1b[?1002h\x1b[?1006h",
                4,
            );
        app.state.insert_test_runtime(target, runtime);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            target_info.inner_rect.x,
            target_info.inner_rect.y,
        ));

        assert!(input_rx.try_recv().is_err());
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(source));
        let menu = app.state.context_menu.as_mut().expect("pane context menu");
        assert!(matches!(
            menu.kind,
            ContextMenuKind::Pane {
                pane_id,
                source_pane_id: Some(source_pane_id),
                ..
            } if pane_id == target && source_pane_id == source
        ));
        assert!(menu.items().contains(&"Swap with focused pane"));
    }

    #[tokio::test]
    async fn right_click_passthrough_requires_exact_modifier_match() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                0,
                b"\x1b[?1002h\x1b[?1006h",
                4,
            );
        ws.insert_test_runtime(pane_id, runtime);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;

        app.state.right_click_passthrough_modifiers = Some(KeyModifiers::CONTROL);

        let col = info.inner_rect.x + 2;
        let row = info.inner_rect.y + 3;
        app.handle_mouse(MouseEvent {
            modifiers: KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ..mouse(MouseEventKind::Down(MouseButton::Right), col, row)
        });

        assert_eq!(app.state.mode, Mode::ContextMenu);
        assert!(app.state.context_menu.is_some());
        assert!(app.state.right_click_passthrough.is_none());
        assert!(input_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn right_click_passthrough_does_not_forward_pane_frame_clicks() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let other_pane = ws.test_split(Direction::Vertical);
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.right_click_passthrough_modifiers = Some(KeyModifiers::CONTROL);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let info = app
            .state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == pane_id)
            .expect("pane info")
            .clone();
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                0,
                b"\x1b[?1002h\x1b[?1006h",
                4,
            );
        app.state.insert_test_runtime(pane_id, runtime);
        app.state.insert_test_runtime(
            other_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(10, 5, b""),
        );

        assert!(app.state.pane_at(info.rect.x, info.rect.y).is_none());
        assert!(app
            .state
            .pane_mouse_target(info.rect.x, info.rect.y)
            .is_some());
        app.handle_mouse(MouseEvent {
            modifiers: KeyModifiers::CONTROL,
            ..mouse(
                MouseEventKind::Down(MouseButton::Right),
                info.rect.x,
                info.rect.y,
            )
        });

        assert_eq!(app.state.mode, Mode::ContextMenu);
        assert!(app.state.context_menu.is_some());
        assert!(app.state.right_click_passthrough.is_none());
        assert!(input_rx.try_recv().is_err());
    }

    fn sample_worktree_open_state() -> crate::app::state::WorktreeOpenState {
        crate::app::state::WorktreeOpenState {
            source_workspace_id: "source".into(),
            source_existing_membership: None,
            source_checkout_path: "/repo/herdr".into(),
            source_repo_root: "/repo/herdr".into(),
            repo_key: "repo-key".into(),
            repo_name: "herdr".into(),
            entries: vec![
                crate::app::state::WorktreeOpenEntry {
                    path: "/repo/herdr".into(),
                    branch: Some("main".into()),
                    is_linked_worktree: false,
                    already_open_ws_idx: Some(0),
                },
                crate::app::state::WorktreeOpenEntry {
                    path: "/repo/herdr-issue".into(),
                    branch: Some("worktree/issue".into()),
                    is_linked_worktree: true,
                    already_open_ws_idx: None,
                },
            ],
            selected: 0,
            query: String::new(),
            search_focused: false,
            error: None,
        }
    }

    #[test]
    fn hovering_context_menu_updates_highlight() {
        let mut app = app_for_mouse_test();
        app.state.context_menu = Some(ContextMenuState {
            kind: ContextMenuKind::Workspace { ws_idx: 0 },
            x: 2,
            y: 2,
            list: MenuListState::new(0),
        });
        app.state.mode = Mode::ContextMenu;

        let menu = app.state.context_menu_rect().unwrap();
        app.handle_mouse(mouse(MouseEventKind::Moved, menu.x + 2, menu.y + 2));

        assert_eq!(app.state.context_menu.unwrap().list.highlighted, 1);
    }

    #[test]
    fn clicking_agent_toast_focuses_target_pane() {
        let mut app = app_for_mouse_test();
        let active = Workspace::test_new("active");
        let mut background = Workspace::test_new("background");
        let first_pane = background.tabs[0].root_pane;
        let target_pane = background.test_split(Direction::Horizontal);
        background.tabs[0].layout.focus_pane(first_pane);

        app.state.workspaces = vec![active, background];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        app.state.toast_config.delay_seconds = 0;
        let target_terminal_id = app.state.workspaces[1]
            .panes
            .get(&target_pane)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&target_terminal_id)
            .unwrap()
            .state = AgentState::Working;

        app.state
            .handle_app_event(crate::events::AppEvent::StateChanged {
                pane_id: target_pane,
                agent: Some(Agent::Pi),
                state: AgentState::Idle,
                visible_blocker: false,
                visible_working: false,
                usage_limited: false,
                process_exited: false,
                observed_at: std::time::Instant::now(),
            });
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let hit = app.state.view.toast_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            hit.x + 1,
            hit.y + 1,
        ));

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.workspaces[1].focused_pane_id(), Some(target_pane));
        assert!(app.state.toast.is_none());
        assert_eq!(app.state.mode, Mode::Terminal);

        app.state.last_pane();

        assert_eq!(app.state.active, Some(0));
        assert_eq!(
            app.state.workspaces[0].focused_pane_id(),
            Some(app.state.workspaces[0].tabs[0].root_pane)
        );
    }

    #[test]
    fn toast_click_does_not_steal_mouse_from_settings_overlay() {
        let mut app = app_for_mouse_test();
        let active = Workspace::test_new("active");
        let background = Workspace::test_new("background");
        let target_pane = background.tabs[0].root_pane;
        let workspace_id = background.id.clone();

        app.state.workspaces = vec![active, background];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::Finished,
            title: "pi finished".into(),
            context: "background · 2".into(),
            position: None,
            target: Some(crate::app::state::ToastTarget {
                workspace_id,
                pane_id: target_pane,
            }),
        });
        app.state.mode = Mode::Settings;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let hit = app.state.view.toast_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            hit.x + 1,
            hit.y + 1,
        ));

        assert_eq!(app.state.active, Some(0));
        assert!(app.state.toast.is_some());
    }

    #[test]
    fn clicking_confirm_close_accepts_workspace_close() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("a"), Workspace::test_new("b")];
        app.state.active = Some(0);
        app.state.selected = 1;
        app.state.begin_workspace_close_confirmation(1);

        let popup = app.state.confirm_close_rect();
        let inner = Rect::new(
            popup.x + 1,
            popup.y + 1,
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        );
        let (confirm, _) = crate::ui::confirm_close_button_rects(inner);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            confirm.x,
            confirm.y,
        ));

        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[test]
    fn clicking_rename_save_submits_workspace_rename_through_api_path() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("old")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::RenameWorkspace;
        app.state.name_input = "new".into();

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 24));
        let inner = app.state.rename_modal_inner().unwrap();
        let (save, _, _) = crate::ui::rename_button_rects(inner);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            save.x,
            save.y,
        ));

        assert_eq!(app.state.workspaces[0].custom_name.as_deref(), Some("new"));
        assert!(app.event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(event.event, crate::api::schema::EventKind::WorkspaceRenamed)
        }));
    }

    #[test]
    fn clicking_open_worktree_row_selects_and_requests_open() {
        let mut app = app_for_mouse_test();
        app.state.mode = Mode::OpenExistingWorktree;
        app.state.worktree_open = Some(sample_worktree_open_state());
        let inner =
            crate::ui::open_existing_worktree_inner_rect(app.state.screen_rect(), 2).unwrap();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            inner.x + 1,
            inner.y + 5,
        ));

        assert_eq!(app.state.worktree_open.as_ref().unwrap().selected, 1);
        assert!(app.state.request_submit_worktree_open);
    }

    #[test]
    fn clicking_open_worktree_buttons_requests_open_or_cancels() {
        let mut app = app_for_mouse_test();
        app.state.mode = Mode::OpenExistingWorktree;
        app.state.worktree_open = Some(sample_worktree_open_state());
        let inner =
            crate::ui::open_existing_worktree_inner_rect(app.state.screen_rect(), 2).unwrap();
        let (open, _) = crate::ui::open_existing_worktree_button_rects(inner);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            open.x,
            open.y,
        ));

        assert!(app.state.worktree_open.is_some());
        assert!(app.state.request_submit_worktree_open);

        let mut app = app_for_mouse_test();
        app.state.mode = Mode::OpenExistingWorktree;
        app.state.worktree_open = Some(sample_worktree_open_state());
        let inner =
            crate::ui::open_existing_worktree_inner_rect(app.state.screen_rect(), 2).unwrap();
        let (_, cancel) = crate::ui::open_existing_worktree_button_rects(inner);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            cancel.x,
            cancel.y,
        ));

        assert!(app.state.worktree_open.is_none());
        assert_eq!(app.state.mode, Mode::Navigate);
    }

    #[test]
    fn scrolling_open_worktree_picker_moves_selection() {
        let mut app = app_for_mouse_test();
        app.state.mode = Mode::OpenExistingWorktree;
        app.state.worktree_open = Some(sample_worktree_open_state());

        app.handle_mouse(mouse(MouseEventKind::ScrollDown, 1, 1));
        assert_eq!(app.state.worktree_open.as_ref().unwrap().selected, 1);

        app.handle_mouse(mouse(MouseEventKind::ScrollUp, 1, 1));
        assert_eq!(app.state.worktree_open.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn clicking_remove_worktree_buttons_requests_remove_or_cancels() {
        let mut app = app_for_mouse_test();
        app.state.mode = Mode::ConfirmRemoveWorktree;
        app.state.worktree_remove = Some(crate::app::state::WorktreeRemoveState {
            workspace_id: "issue".into(),
            repo_root: "/repo/herdr".into(),
            path: "/repo/herdr-issue".into(),
            error: None,
            removing: false,
            force_confirmation: false,
        });
        let popup = crate::ui::remove_worktree_popup_rect(app.state.screen_rect()).unwrap();
        let inner = Rect::new(
            popup.x + 1,
            popup.y + 1,
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        );
        let (remove, _) = crate::ui::remove_worktree_button_rects(inner, false);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            remove.x,
            remove.y,
        ));

        assert!(app.state.worktree_remove.is_some());
        assert!(app.state.request_submit_worktree_remove);

        let mut app = app_for_mouse_test();
        app.state.mode = Mode::ConfirmRemoveWorktree;
        app.state.worktree_remove = Some(crate::app::state::WorktreeRemoveState {
            workspace_id: "issue".into(),
            repo_root: "/repo/herdr".into(),
            path: "/repo/herdr-issue".into(),
            error: None,
            removing: false,
            force_confirmation: false,
        });
        let popup = crate::ui::remove_worktree_popup_rect(app.state.screen_rect()).unwrap();
        let inner = Rect::new(
            popup.x + 1,
            popup.y + 1,
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        );
        let (_, cancel) = crate::ui::remove_worktree_button_rects(inner, false);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            cancel.x,
            cancel.y,
        ));

        assert!(app.state.worktree_remove.is_none());
        assert_eq!(app.state.mode, Mode::Navigate);
    }

    #[test]
    fn clicking_confirm_close_accepts_after_workspace_context_menu_close() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("a"), Workspace::test_new("b")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        app.state.context_menu = Some(ContextMenuState {
            kind: ContextMenuKind::Workspace { ws_idx: 1 },
            x: 2,
            y: 2,
            list: MenuListState::new(1),
        });
        app.state.mode = Mode::ContextMenu;
        handle_context_menu_key(
            &mut app.state,
            &mut app.terminal_runtimes,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        assert_eq!(app.state.mode, Mode::ConfirmClose);
        assert_eq!(app.state.selected, 1);

        let popup = app.state.confirm_close_rect();
        let inner = Rect::new(
            popup.x + 1,
            popup.y + 1,
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        );
        let (confirm, _) = crate::ui::confirm_close_button_rects(inner);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            confirm.x + 1,
            confirm.y,
        ));

        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(app.state.workspaces[0].display_name(), "a");
    }

    #[test]
    fn clicking_context_menu_close_routes_through_api_path() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("a"), Workspace::test_new("b")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.confirm_close = false;
        app.state.context_menu = Some(ContextMenuState {
            kind: ContextMenuKind::Workspace { ws_idx: 1 },
            x: 2,
            y: 2,
            list: MenuListState::new(1),
        });
        app.state.mode = Mode::ContextMenu;

        let menu = app.state.context_menu_rect().unwrap();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 2,
        ));

        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(app.state.workspaces[0].display_name(), "a");
        assert!(app.event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(event.event, crate::api::schema::EventKind::WorkspaceClosed)
        }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn keyboard_context_menu_split_keeps_new_runtime() {
        let mut app = app_for_mouse_test();
        app.state.default_shell = "/usr/bin/true".into();
        let (workspace, terminal, runtime) = Workspace::new(
            std::env::current_dir().unwrap_or_else(|_| "/".into()),
            24,
            80,
            app.state.pane_scrollback_limit_bytes,
            app.state.pane_terminal_theme(),
            Some(app.state.pane_terminal_appearance()),
            crate::pane::PaneShellConfig::new(&app.state.default_shell, app.state.shell_mode),
            app.event_tx.clone(),
            app.render_notify.clone(),
            app.render_dirty.clone(),
        )
        .expect("workspace should spawn");
        app.state.workspaces = vec![workspace];
        app.terminal_runtimes.insert(terminal.id.clone(), runtime);
        app.state.terminals.insert(terminal.id.clone(), terminal);
        app.state.active = Some(0);
        app.state.selected = 0;
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let runtime_count = app.terminal_runtimes.len();
        app.state.context_menu = Some(ContextMenuState {
            kind: ContextMenuKind::Pane {
                ws_idx: 0,
                tab_idx: 0,
                pane_id,
                source_pane_id: None,
                has_manual_label: false,
                right_click_passthrough: false,
                linkable_work_link: None,
            },
            x: 2,
            y: 2,
            list: MenuListState::new(1),
        });
        app.state.mode = Mode::ContextMenu;

        handle_context_menu_key(
            &mut app.state,
            &mut app.terminal_runtimes,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        assert_eq!(app.state.mode, Mode::Terminal);
        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 2);
        assert_eq!(app.terminal_runtimes.len(), runtime_count + 1);

        let runtimes: Vec<_> = app.terminal_runtimes.drain().collect();
        for (_terminal_id, runtime) in runtimes {
            runtime.shutdown();
        }
    }

    #[test]
    fn dragging_pane_split_updates_captured_layout_ratio() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.workspaces[0].test_split(Direction::Horizontal);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let border = app.state.view.split_borders[0].clone();
        let before = capture_snapshot(&app.state);
        let drag_row = border.area.y.saturating_add(1);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            border.pos,
            drag_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            border.pos.saturating_add(6),
            drag_row,
        ));

        let after = capture_snapshot(&app.state);
        assert_ne!(root_layout_ratio(&before), root_layout_ratio(&after));
    }

    #[test]
    fn pane_split_hitbox_does_not_overlap_right_pane_content() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.pane_gaps = false;
        app.state.workspaces[0].test_split(Direction::Horizontal);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let border = app.state.view.split_borders[0].clone();
        let row = border.area.y.saturating_add(1);

        assert!(app
            .state
            .find_border_at(border.pos.saturating_sub(1), row)
            .is_none());
        assert!(app.state.find_border_at(border.pos, row).is_some());
        assert!(app
            .state
            .find_border_at(border.pos.saturating_add(1), row)
            .is_none());
    }

    #[test]
    fn pane_split_hitbox_does_not_overlap_bottom_pane_content() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.pane_gaps = false;
        app.state.workspaces[0].test_split(Direction::Vertical);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let border = app.state.view.split_borders[0].clone();
        let col = border.area.x.saturating_add(1);

        assert!(app
            .state
            .find_border_at(col, border.pos.saturating_sub(1))
            .is_none());
        assert!(app.state.find_border_at(col, border.pos).is_some());
        assert!(app
            .state
            .find_border_at(col, border.pos.saturating_add(1))
            .is_none());
    }

    #[test]
    fn borderless_no_gap_split_has_no_mouse_hitbox_over_content() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.pane_borders = false;
        app.state.workspaces[0].test_split(Direction::Horizontal);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let border = app.state.view.split_borders[0].clone();
        let row = border.area.y.saturating_add(1);

        assert!(app.state.find_border_at(border.pos, row).is_none());
    }

    #[test]
    fn bordered_pane_gaps_keep_both_split_borders_draggable() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.pane_gaps = true;
        app.state.workspaces[0].test_split(Direction::Horizontal);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let border = app.state.view.split_borders[0].clone();
        let row = border.area.y.saturating_add(1);

        assert!(app
            .state
            .find_border_at(border.pos.saturating_sub(1), row)
            .is_some());
        assert!(app.state.find_border_at(border.pos, row).is_some());
        assert!(app
            .state
            .find_border_at(border.pos.saturating_add(1), row)
            .is_none());
    }

    #[test]
    fn borderless_pane_gap_is_not_a_pane_but_remains_split_draggable() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.pane_borders = false;
        app.state.pane_gaps = true;
        app.state.workspaces[0].test_split(Direction::Horizontal);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let border = app.state.view.split_borders[0].clone();
        let row = border.area.y.saturating_add(1);
        let gap_col = border.pos.saturating_sub(1);

        assert!(app.state.pane_at(gap_col, row).is_none());
        assert!(app.state.find_border_at(gap_col, row).is_some());
        assert!(app.state.find_border_at(border.pos, row).is_none());
    }

    #[test]
    fn borderless_gap_hitbox_is_empty_when_first_split_side_has_one_cell() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.pane_borders = false;
        app.state.pane_gaps = true;
        app.state.workspaces[0].test_split(Direction::Horizontal);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 2, 4));
        let border = app.state.view.split_borders[0].clone();
        let row = border.area.y.saturating_add(1);
        let candidate_gap_col = border.pos.saturating_sub(1);

        assert!(app.state.pane_frame_at(candidate_gap_col, row).is_some());
        assert!(app.state.find_border_at(candidate_gap_col, row).is_none());
    }

    #[test]
    fn borderless_gap_hitbox_is_empty_when_first_split_side_has_zero_width() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.pane_borders = false;
        app.state.pane_gaps = true;
        app.state.workspaces[0].test_split(Direction::Horizontal);
        app.state.workspaces[0].tabs[0]
            .layout
            .set_ratio_at(&[], 0.1);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 1, 4));
        let border = app.state.view.split_borders[0].clone();
        let row = border.area.y.saturating_add(1);

        assert_eq!(border.pos, 0);
        assert!(app.state.find_border_at(0, row).is_none());
    }

    #[test]
    fn selecting_from_right_pane_first_content_column_starts_selection() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let second_pane = ws.test_split(Direction::Horizontal);
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let second_info = app
            .state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == second_pane)
            .expect("second pane info")
            .clone();
        let col = second_info.inner_rect.x;
        let row = second_info.inner_rect.y;

        assert!(app.state.find_border_at(col, row).is_none());
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));

        assert!(app.state.drag.is_none());
        assert_eq!(
            app.state
                .selection
                .as_ref()
                .map(|selection| selection.pane_id),
            Some(second_pane)
        );
    }

    #[test]
    fn selecting_from_bottom_pane_first_content_row_starts_selection() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let second_pane = ws.test_split(Direction::Vertical);
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let second_info = app
            .state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == second_pane)
            .expect("second pane info")
            .clone();
        let col = second_info.inner_rect.x;
        let row = second_info.inner_rect.y;

        assert!(app.state.find_border_at(col, row).is_none());
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));

        assert!(app.state.drag.is_none());
        assert_eq!(
            app.state
                .selection
                .as_ref()
                .map(|selection| selection.pane_id),
            Some(second_pane)
        );
    }

    #[tokio::test]
    async fn dragging_vertical_pane_split_still_resizes_when_pane_mouse_reporting_is_enabled() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let first_pane = ws.tabs[0].root_pane;
        let second_pane = ws.test_split(Direction::Vertical);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let pane_infos = app.state.view.pane_infos.clone();
        let first_info = pane_infos
            .iter()
            .find(|info| info.id == first_pane)
            .expect("first pane info")
            .clone();
        let second_info = pane_infos
            .iter()
            .find(|info| info.id == second_pane)
            .expect("second pane info")
            .clone();

        app.state.insert_test_runtime(
            first_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                first_info.inner_rect.width.max(1),
                first_info.inner_rect.height.max(1),
                b"\x1b[?1002h",
            ),
        );
        app.state.insert_test_runtime(
            second_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                second_info.inner_rect.width.max(1),
                second_info.inner_rect.height.max(1),
                b"\x1b[?1002h",
            ),
        );

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let border = app
            .state
            .view
            .split_borders
            .iter()
            .find(|border| border.direction == Direction::Vertical)
            .expect("vertical split border")
            .clone();
        let before = capture_snapshot(&app.state);
        let drag_col = border.area.x.saturating_add(1);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            drag_col,
            border.pos,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            drag_col,
            border.pos.saturating_add(4),
        ));

        let after = capture_snapshot(&app.state);
        assert_ne!(root_layout_ratio(&before), root_layout_ratio(&after));
    }

    #[tokio::test]
    async fn dragging_horizontal_pane_split_still_resizes_when_pane_mouse_reporting_is_enabled() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let first_pane = ws.tabs[0].root_pane;
        let second_pane = ws.test_split(Direction::Horizontal);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let pane_infos = app.state.view.pane_infos.clone();
        let first_info = pane_infos
            .iter()
            .find(|info| info.id == first_pane)
            .expect("first pane info")
            .clone();
        let second_info = pane_infos
            .iter()
            .find(|info| info.id == second_pane)
            .expect("second pane info")
            .clone();

        app.state.insert_test_runtime(
            first_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                first_info.inner_rect.width.max(1),
                first_info.inner_rect.height.max(1),
                b"\x1b[?1002h",
            ),
        );
        app.state.insert_test_runtime(
            second_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                second_info.inner_rect.width.max(1),
                second_info.inner_rect.height.max(1),
                b"\x1b[?1002h",
            ),
        );

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let border = app
            .state
            .view
            .split_borders
            .iter()
            .find(|border| border.direction == Direction::Horizontal)
            .expect("horizontal split border")
            .clone();
        let before = capture_snapshot(&app.state);
        let drag_row = border.area.y.saturating_add(1);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            border.pos,
            drag_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            border.pos.saturating_add(6),
            drag_row,
        ));

        let after = capture_snapshot(&app.state);
        assert_ne!(root_layout_ratio(&before), root_layout_ratio(&after));
    }

    #[test]
    fn wheel_routing_prefers_mouse_reporting() {
        let input_state = crate::pane::InputState {
            alternate_screen: true,
            application_cursor: false,
            bracketed_paste: false,
            focus_reporting: false,
            mouse_protocol_mode: crate::input::MouseProtocolMode::ButtonMotion,
            mouse_protocol_encoding: crate::input::MouseProtocolEncoding::Sgr,
            mouse_alternate_scroll: true,
            modify_other_keys: false,
            color_scheme_reporting: false,
        };

        assert_eq!(wheel_routing(input_state), WheelRouting::MouseReport);
    }

    #[test]
    fn wheel_over_tab_bar_switches_tabs() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("one");
        ws.test_add_tab(Some("two"));
        ws.test_add_tab(Some("three"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let tab_bar = app.state.view.tab_bar_rect;

        app.handle_mouse(mouse(MouseEventKind::ScrollDown, tab_bar.x + 1, tab_bar.y));
        assert_eq!(app.state.workspaces[0].active_tab, 1);

        app.handle_mouse(mouse(MouseEventKind::ScrollUp, tab_bar.x + 1, tab_bar.y));
        assert_eq!(app.state.workspaces[0].active_tab, 0);

        app.handle_mouse(mouse(MouseEventKind::ScrollUp, tab_bar.x + 1, tab_bar.y));
        assert_eq!(app.state.workspaces[0].active_tab, 2);

        app.handle_mouse(mouse(
            MouseEventKind::ScrollDown,
            tab_bar.x + tab_bar.width.saturating_sub(1),
            tab_bar.y,
        ));
        assert_eq!(app.state.workspaces[0].active_tab, 0);
    }

    #[test]
    fn a_hidden_tab_bar_leaves_its_old_row_to_the_terminal() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("one");
        ws.test_add_tab(Some("two"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.tab_bar_position = crate::config::TabBarPositionConfig::Hidden;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        assert!(app.state.view.tab_hit_areas.is_empty());
        let terminal_area = app.state.view.terminal_area;
        // The row the tab bar used to own belongs to the terminal now, so no
        // column in it may still hit-test as a tab click.
        for col in terminal_area.x..terminal_area.right() {
            assert!(
                !app.state.on_tab_bar(col, terminal_area.y),
                "col {col} still reads as tab bar"
            );
            assert_eq!(app.state.tab_at(col, terminal_area.y), None);
        }
        assert_eq!(app.state.workspaces[0].active_tab, 0);
    }

    #[test]
    fn bottom_mode_bar_consumes_hidden_tab_mouse_actions() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("one");
        ws.test_add_tab(Some("two"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Prefix;
        app.state.tab_bar_position = crate::config::TabBarPositionConfig::Bottom;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let second_tab = app.state.view.tab_hit_areas[1];
        let new_tab = app.state.view.new_tab_hit_area;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            second_tab.x,
            second_tab.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            second_tab.x,
            second_tab.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::ScrollDown,
            second_tab.x,
            second_tab.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            second_tab.x,
            second_tab.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            new_tab.x,
            new_tab.y,
        ));

        app.state.drag = Some(DragState {
            target: DragTarget::SidebarDivider,
        });
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            second_tab.x,
            second_tab.y,
        ));

        assert_eq!(app.state.workspaces[0].active_tab, 0);
        assert_eq!(app.state.workspaces[0].tabs.len(), 2);
        assert!(app.state.context_menu.is_none());
        assert!(app.state.tab_presses.is_empty());
        assert!(app.state.drag.is_none());
    }

    #[test]
    fn right_click_inactive_tab_opens_menu_without_switching_tabs() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("one");
        ws.test_add_tab(Some("two"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let second_tab = app.state.view.tab_hit_areas[1];

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            second_tab.x + 1,
            second_tab.y,
        ));

        assert_eq!(app.state.workspaces[0].active_tab, 0);
        let menu = app.state.context_menu.as_ref().expect("tab context menu");
        assert_eq!(
            menu.kind,
            ContextMenuKind::Tab {
                ws_idx: 0,
                tab_idx: 1
            }
        );
        assert_eq!(app.state.mode, Mode::ContextMenu);
    }

    #[test]
    fn clicking_tab_context_menu_close_leaves_context_menu_mode() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("one");
        ws.test_add_tab(Some("two"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let second_tab = app.state.view.tab_hit_areas[1];

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            second_tab.x + 1,
            second_tab.y,
        ));

        let menu = app
            .state
            .context_menu_rect()
            .expect("tab context menu rect");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 3,
        ));

        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert_eq!(app.state.workspaces[0].display_name(), "one");
        assert!(app.state.context_menu.is_none());
        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(app
            .event_hub
            .events_after(0)
            .iter()
            .any(|(_, event)| { matches!(event.event, crate::api::schema::EventKind::TabClosed) }));
    }

    #[test]
    fn clicking_pane_context_menu_close_leaves_context_menu_mode() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("one");
        let first_pane = ws.tabs[0].root_pane;
        let second_pane = ws.test_split(Direction::Horizontal);
        ws.tabs[0].layout.focus_pane(second_pane);
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let first_info = app
            .state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == first_pane)
            .expect("first pane info")
            .clone();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            first_info.inner_rect.x + 1,
            first_info.inner_rect.y + 1,
        ));

        let menu_state = app.state.context_menu.as_ref().expect("pane context menu");
        let close_idx = menu_state
            .items()
            .iter()
            .position(|item| *item == "Close pane")
            .expect("close pane menu item");
        let menu = app
            .state
            .context_menu_rect()
            .expect("pane context menu rect");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 1 + close_idx as u16,
        ));

        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 1);
        assert!(app.state.context_menu.is_none());
        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(app.event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(event.event, crate::api::schema::EventKind::PaneClosed)
        }));
    }

    #[test]
    fn clicking_pane_context_menu_close_last_parent_group_pane_keeps_confirmation_mode() {
        let mut app = app_for_mouse_test();
        let mut parent = Workspace::test_new("main");
        let pane_id = parent.tabs[0].root_pane;
        mark_worktree_space_member(&mut parent, 0, "repo-key");
        let mut child = Workspace::test_new("issue");
        mark_worktree_space_member(&mut child, 1, "repo-key");
        app.state.workspaces = vec![parent, child];
        app.state.active = Some(0);
        app.state.selected = 1;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let pane_info = app
            .state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == pane_id)
            .expect("pane info")
            .clone();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            pane_info.inner_rect.x + 1,
            pane_info.inner_rect.y + 1,
        ));

        let menu_state = app.state.context_menu.as_ref().expect("pane context menu");
        let close_idx = menu_state
            .items()
            .iter()
            .position(|item| *item == "Close pane")
            .expect("close pane menu item");
        let menu = app
            .state
            .context_menu_rect()
            .expect("pane context menu rect");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 1 + close_idx as u16,
        ));

        assert_eq!(app.state.selected, 0);
        assert_eq!(app.state.mode, Mode::ConfirmClose);
        assert_eq!(app.state.workspaces.len(), 2);
        assert!(app.state.context_menu.is_none());
    }

    #[test]
    fn wheel_over_overflowing_tab_bar_switches_tabs() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("one");
        ws.tabs[0].set_custom_name("very-long-one".into());
        ws.test_add_tab(Some("very-long-two"));
        ws.test_add_tab(Some("very-long-three"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 65, 20));
        assert!(app.state.view.tab_scroll_right_hit_area.width > 0);
        let tab_bar = app.state.view.tab_bar_rect;

        app.handle_mouse(mouse(
            MouseEventKind::ScrollDown,
            tab_bar.x + tab_bar.width.saturating_sub(2),
            tab_bar.y,
        ));
        assert_eq!(app.state.workspaces[0].active_tab, 1);

        app.handle_mouse(mouse(
            MouseEventKind::ScrollDown,
            tab_bar.x + tab_bar.width.saturating_sub(2),
            tab_bar.y,
        ));
        assert_eq!(app.state.workspaces[0].active_tab, 2);
    }

    #[test]
    fn wheel_outside_tab_bar_does_not_switch_tabs() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("one");
        ws.test_add_tab(Some("two"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let terminal = app.state.view.terminal_area;

        app.handle_mouse(mouse(
            MouseEventKind::ScrollDown,
            terminal.x + 1,
            terminal.y + 1,
        ));

        assert_eq!(app.state.workspaces[0].active_tab, 0);
    }

    #[test]
    fn mobile_switch_button_opens_switcher_and_workspace_row_switches_workspace() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 44, 20));
        assert_eq!(app.state.view.layout, ViewLayout::Mobile);

        let switch = app.state.view.mobile_menu_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            switch.x + 1,
            switch.y + 1,
        ));

        assert_eq!(app.state.mode, Mode::Navigate);

        let viewport = crate::ui::mobile_switcher_areas(&app.state).viewport;
        let workspace_row = (viewport.y..viewport.y + viewport.height)
            .find(|row| {
                matches!(
                    crate::ui::mobile_switcher_target_at(&app.state, viewport.x + 4, *row),
                    Some(crate::ui::MobileSwitcherTarget::Workspace(1))
                )
            })
            .expect("second workspace row");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            viewport.x + 4,
            workspace_row,
        ));

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[test]
    fn mobile_compact_tab_click_focuses_through_dispatch() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 44, 20));
        let switch = app.state.view.mobile_menu_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            switch.x + 1,
            switch.y + 1,
        ));
        assert_ne!(
            app.state.mode,
            Mode::Terminal,
            "the switcher should be open for this test"
        );

        // Locate the row through the same projection the renderer uses, then drive the real
        // dispatch path through its compact grid.
        let viewport = crate::ui::mobile_switcher_areas(&app.state).viewport;
        let tab_row = viewport.y
            + crate::ui::mobile_switcher_workspace_doc_range(&app.state, 0)
                .expect("workspace row")
                .start as u16
            + 1;
        let row_col = (viewport.x..viewport.x + viewport.width)
            .find(|col| {
                matches!(
                    crate::ui::mobile_switcher_target_at(&app.state, *col, tab_row),
                    Some(crate::ui::MobileSwitcherTarget::SidebarTab { .. })
                )
            })
            .expect("a compact tab row");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            row_col,
            tab_row,
        ));
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[test]
    fn ac4_mobile_sidebar_tab_click_preserves_tabs_focused_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("one");
        let target_tab = ws.test_add_tab(Some("multi-pane"));
        ws.active_tab = target_tab;
        let focused_pane = ws.test_split(Direction::Horizontal);
        assert_eq!(ws.tabs[target_tab].layout.focused(), focused_pane);
        ws.active_tab = 0;
        app.state.workspaces = vec![ws];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 44, 20));
        let switch = app.state.view.mobile_menu_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            switch.x + 1,
            switch.y + 1,
        ));
        let viewport = crate::ui::mobile_switcher_areas(&app.state).viewport;
        let tab_row = (viewport.y..viewport.y + viewport.height)
            .find(|row| {
                matches!(
                    crate::ui::mobile_switcher_target_at(&app.state, viewport.x + 2, *row),
                    Some(crate::ui::MobileSwitcherTarget::SidebarTab {
                        tab_idx,
                        ..
                    }) if tab_idx == target_tab
                )
            })
            .expect("multi-pane tab row");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            viewport.x + 2,
            tab_row,
        ));

        assert_eq!(app.state.workspaces[0].active_tab, target_tab);
        assert_eq!(
            app.state.workspaces[0].tabs[target_tab].layout.focused(),
            focused_pane
        );
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[test]
    fn mobile_workspace_panel_scroll_reaches_extra_workspaces() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = (0..12)
            .map(|idx| Workspace::test_new(&format!("ws-{idx}")))
            .collect();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 44, 20));
        let switch = app.state.view.mobile_menu_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            switch.x + 1,
            switch.y + 1,
        ));
        assert_eq!(app.state.mode, Mode::Navigate);

        let viewport = crate::ui::mobile_switcher_areas(&app.state).viewport;
        app.handle_mouse(mouse(
            MouseEventKind::ScrollDown,
            viewport.x + 2,
            viewport.y,
        ));
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 44, 20));
        assert_eq!(app.state.mobile_switcher_scroll, 2);

        let workspace_row = (viewport.y..viewport.y + viewport.height)
            .find(|row| {
                matches!(
                    crate::ui::mobile_switcher_target_at(&app.state, viewport.x + 4, *row),
                    Some(crate::ui::MobileSwitcherTarget::Workspace(1))
                )
            })
            .expect("second workspace row");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            viewport.x + 4,
            workspace_row,
        ));

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[test]
    fn mobile_global_scroll_reaches_tabs_and_switches_tab() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("one");
        ws.test_add_tab(Some("two"));
        ws.test_add_tab(Some("three"));
        ws.test_add_tab(Some("four"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 44, 12));
        let switch = app.state.view.mobile_menu_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            switch.x + 1,
            switch.y + 1,
        ));

        let viewport = crate::ui::mobile_switcher_areas(&app.state).viewport;

        app.handle_mouse(mouse(
            MouseEventKind::ScrollDown,
            viewport.x + 2,
            viewport.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::ScrollDown,
            viewport.x + 2,
            viewport.y,
        ));
        assert_eq!(app.state.mobile_switcher_scroll, 4);
        let tab_row = (viewport.y..viewport.y + viewport.height)
            .find(|row| {
                matches!(
                    crate::ui::mobile_switcher_target_at(&app.state, viewport.x + 2, *row),
                    Some(crate::ui::MobileSwitcherTarget::Tab(2))
                )
            })
            .expect("third tab row");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            viewport.x + 2,
            tab_row,
        ));
        assert_eq!(app.state.workspaces[0].active_tab, 2);
    }

    #[test]
    fn mobile_switcher_new_workspace_opens_prompt_when_enabled() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.prompt_new_workspace_name = true;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 44, 20));
        let switch = app.state.view.mobile_menu_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            switch.x + 1,
            switch.y + 1,
        ));
        let viewport = crate::ui::mobile_switcher_areas(&app.state).viewport;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            viewport.x + 2,
            viewport.y + 1,
        ));

        assert_eq!(app.state.mode, Mode::RenameWorkspace);
        assert!(app.state.pending_workspace_create_cwd.is_some());
        assert!(app.state.name_input_replace_on_type);
        assert_eq!(app.state.workspaces.len(), 1);
    }

    #[test]
    fn desktop_new_workspace_opens_prompt_when_enabled() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.prompt_new_workspace_name = true;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let new_workspace = crate::ui::sidebar_header_new_menu_rect(app.state.view.sidebar_rect);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            new_workspace.x + 1,
            new_workspace.y,
        ));
        let menu = crate::ui::sidebar_new_menu_layout(&app.state, Rect::new(0, 0, 120, 40))
            .expect("new menu");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.list_rect.x,
            menu.list_rect.y,
        ));

        assert_eq!(app.state.mode, Mode::RenameWorkspace);
        assert!(app.state.pending_workspace_create_cwd.is_some());
        assert!(app.state.name_input_replace_on_type);
        assert_eq!(app.state.workspaces.len(), 1);
    }

    #[test]
    fn sidebar_footer_work_entry_opens_pull_requests_at_supported_widths() {
        for width in [80, 120] {
            let mut app = app_for_mouse_test();
            app.state.workspaces = vec![Workspace::test_new("one")];
            app.state.ensure_test_terminals();
            app.state.active = Some(0);
            app.state.selected = 0;

            crate::ui::compute_view(&mut app.state, Rect::new(0, 0, width, 24));
            let hit = app.state.view.sidebar_footer_work_hit_area;
            assert_eq!(hit.height, 1, "footer must render at {width} columns");
            app.handle_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                hit.x + 1,
                hit.y,
            ));

            assert!(
                app.state.work_view.is_some(),
                "footer click at {width} columns"
            );
        }
    }

    #[test]
    fn sidebar_footer_order_hit_areas_settings_and_hover_are_complete() {
        use crate::app::state::SidebarFooterItem;

        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 80, 24));

        let areas = [
            app.state.view.sidebar_footer_settings_hit_area,
            app.state.view.sidebar_footer_work_hit_area,
            app.state.view.sidebar_footer_usage_hit_area,
            app.state.view.sidebar_footer_ticket_hit_area,
            app.state.view.sidebar_footer_missive_hit_area,
            app.state.view.sidebar_footer_refresh_hit_area,
        ];
        assert!(areas.iter().all(|area| area.width == 2 && area.height == 1));
        assert!(areas.windows(2).all(|pair| pair[0].right() == pair[1].x));

        let linear = areas[3];
        app.handle_mouse(mouse(MouseEventKind::Moved, linear.x, linear.y));
        assert_eq!(
            app.state.hovered_control,
            Some(crate::app::state::ControlId::SidebarFooter(
                SidebarFooterItem::Linear
            ))
        );

        let settings = areas[0];
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            settings.x,
            settings.y,
        ));
        assert_eq!(app.state.hovered_control, None);
        assert!(!app.state.hover_tooltip_visible);
        assert_eq!(app.state.mode, Mode::Settings);
    }

    #[test]
    fn sidebar_footer_ticket_entry_opens_tickets_at_supported_widths() {
        for width in [80, 120] {
            let mut app = app_for_mouse_test();
            app.state.workspaces = vec![Workspace::test_new("one")];
            app.state.ensure_test_terminals();
            app.state.active = Some(0);
            app.state.selected = 0;

            crate::ui::compute_view(&mut app.state, Rect::new(0, 0, width, 24));
            let usage = app.state.view.sidebar_footer_usage_hit_area;
            let hit = app.state.view.sidebar_footer_ticket_hit_area;
            assert_eq!(
                hit.height, 1,
                "ticket footer must render at {width} columns"
            );
            assert_eq!(hit.x, usage.right(), "ticket entry follows Usage");
            app.handle_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                hit.x + 1,
                hit.y,
            ));

            assert!(app.state.work_view.as_ref().is_some_and(|view| {
                view.projection == crate::app::state::WorkProjection::Tickets
            }));
        }
    }

    #[test]
    fn sidebar_footer_missive_entry_follows_usage_and_opens_at_supported_widths() {
        for width in [80, 120] {
            let mut app = app_for_mouse_test();
            app.state.workspaces = vec![Workspace::test_new("one")];
            app.state.ensure_test_terminals();
            app.state.active = Some(0);
            app.state.selected = 0;

            crate::ui::compute_view(&mut app.state, Rect::new(0, 0, width, 24));
            let tickets = app.state.view.sidebar_footer_ticket_hit_area;
            let hit = app.state.view.sidebar_footer_missive_hit_area;
            assert_eq!(hit.height, 1, "Missive footer renders at {width} columns");
            assert_eq!(hit.x, tickets.right(), "Missive entry follows Linear");
            app.handle_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                hit.x + 1,
                hit.y,
            ));

            assert!(app.state.work_view.as_ref().is_some_and(|view| {
                view.projection == crate::app::state::WorkProjection::Missive
            }));
        }
    }

    #[test]
    fn sidebar_footer_refresh_follows_missive_and_is_single_flight() {
        for width in [80, 120] {
            let mut app = app_for_mouse_test();
            app.state.workspaces = vec![Workspace::test_new("one")];
            app.state.ensure_test_terminals();
            app.state.active = Some(0);
            app.state.selected = 0;

            crate::ui::compute_view(&mut app.state, Rect::new(0, 0, width, 24));
            let missive = app.state.view.sidebar_footer_missive_hit_area;
            let hit = app.state.view.sidebar_footer_refresh_hit_area;
            assert_eq!(hit.height, 1, "refresh footer renders at {width} columns");
            assert_eq!(hit.x, missive.right(), "refresh follows Missive");
            app.handle_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                hit.x + 1,
                hit.y,
            ));
            assert!(app.state.sidebar_refreshing);
            assert!(app.state.sidebar_refresh_requested);

            app.handle_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                hit.x + 1,
                hit.y,
            ));
            assert!(
                app.state.sidebar_refresh_requested,
                "second click is ignored"
            );
        }
    }

    #[test]
    fn sidebar_header_plus_menu_dispatches_add_project() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let hit = crate::ui::sidebar_header_new_menu_rect(app.state.view.sidebar_rect);

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), hit.x, hit.y));
        let menu = crate::ui::sidebar_new_menu_layout(&app.state, Rect::new(0, 0, 120, 40))
            .expect("new menu");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.list_rect.x,
            menu.list_rect.y + 1,
        ));

        assert!(app.state.add_project_active());
    }

    #[test]
    fn folder_browser_mouse_enters_rows_jumps_breadcrumbs_toggles_hidden_and_scrolls() {
        let root =
            std::env::temp_dir().join(format!("herdr-folder-browser-mouse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("alpha")).expect("visible directory");
        std::fs::create_dir_all(root.join(".secret")).expect("hidden directory");
        std::fs::create_dir_all(root.join("zulu")).expect("second visible directory");
        // macOS: temp_dir() is /var/… while the browser canonicalizes to /private/var/….
        let root = crate::worktree::canonical_or_original(&root);

        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.add_project_start_dir = root.display().to_string();
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        // F25 folded the header `Add project` button into the `+` menu (second entry).
        let hit = crate::ui::sidebar_header_new_menu_rect(app.state.view.sidebar_rect);
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), hit.x, hit.y));
        let menu = crate::ui::sidebar_new_menu_layout(&app.state, Rect::new(0, 0, 120, 40))
            .expect("new menu");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.list_rect.x,
            menu.list_rect.y + 1,
        ));
        assert!(app.state.add_project_active());
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));

        let layout = app.state.view.add_project_layout.clone();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            layout.hidden_toggle.x,
            layout.hidden_toggle.y,
        ));
        assert!(app
            .state
            .home
            .as_ref()
            .and_then(|home| home.add_project.as_ref())
            .is_some_and(|project| project.browse.show_hidden));

        app.handle_mouse(mouse(
            MouseEventKind::ScrollDown,
            layout.input.x,
            layout.input.y,
        ));
        assert!(app
            .state
            .home
            .as_ref()
            .and_then(|home| home.add_project.as_ref())
            .is_some_and(|project| project.browse.selected > 0));

        app.state.add_project_toggle_hidden();
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let list = app
            .state
            .view
            .add_project_layout
            .list
            .as_ref()
            .expect("folder rows")
            .list_rect;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            list.x,
            list.y + 1,
        ));
        assert!(app
            .state
            .home
            .as_ref()
            .and_then(|home| home.add_project.as_ref())
            .is_some_and(|project| project.browse.directory == root.join("alpha")));

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let root_crumb = app
            .state
            .view
            .add_project_layout
            .breadcrumbs
            .iter()
            .find(|(path, _)| path == &root)
            .map(|(_, rect)| *rect)
            .expect("fixture root breadcrumb");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            root_crumb.x,
            root_crumb.y,
        ));
        assert!(app
            .state
            .home
            .as_ref()
            .and_then(|home| home.add_project.as_ref())
            .is_some_and(|project| project.browse.directory == root));
    }

    #[test]
    fn sidebar_footer_usage_entry_and_every_view_toggle_are_clickable() {
        use crate::app::state::{UsageBreakdown, UsageHitTarget, UsageMetric, UsageRange};

        for width in [80, 120] {
            let mut app = app_for_mouse_test();
            app.state.workspaces = vec![Workspace::test_new("one")];
            app.state.ensure_test_terminals();
            app.state.active = Some(0);
            app.state.selected = 0;

            crate::ui::compute_view(&mut app.state, Rect::new(0, 0, width, 24));
            let work = app.state.view.sidebar_footer_work_hit_area;
            let footer = app.state.view.sidebar_footer_usage_hit_area;
            assert!(footer.width > 0);
            assert_eq!(footer.x, work.right(), "usage entry follows PRs");
            app.handle_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                footer.x,
                footer.y,
            ));
            assert!(app.state.usage_view.is_some());
            crate::ui::compute_view(&mut app.state, Rect::new(0, 0, width, 24));

            for target in [
                UsageHitTarget::Tokens,
                UsageHitTarget::Hours24,
                UsageHitTarget::Days7,
                UsageHitTarget::Days30,
                UsageHitTarget::Days90,
                UsageHitTarget::Day,
                UsageHitTarget::Model,
                UsageHitTarget::Cost,
                UsageHitTarget::Rescan,
            ] {
                let rect = app
                    .state
                    .view
                    .usage_hit_areas
                    .iter()
                    .find(|hit| hit.target == target)
                    .map(|hit| hit.rect)
                    .unwrap_or_else(|| panic!("usage hit area for {target:?}"));
                app.handle_mouse(mouse(
                    MouseEventKind::Down(MouseButton::Left),
                    rect.x,
                    rect.y,
                ));
            }
            let view = app.state.usage_view.as_ref().expect("usage view");
            assert_eq!(view.metric, UsageMetric::Cost);
            assert_eq!(view.range, UsageRange::Days90);
            assert_eq!(view.breakdown, UsageBreakdown::Model);
            assert!(view.scanning);
        }
    }

    #[tokio::test]
    async fn desktop_new_workspace_creates_immediately_by_default() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let new_workspace = crate::ui::sidebar_header_new_menu_rect(app.state.view.sidebar_rect);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            new_workspace.x + 1,
            new_workspace.y,
        ));
        let menu = crate::ui::sidebar_new_menu_layout(&app.state, Rect::new(0, 0, 120, 40))
            .expect("new menu");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.list_rect.x,
            menu.list_rect.y,
        ));

        assert_eq!(app.state.workspaces.len(), 2);
        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(app.state.pending_workspace_create_cwd.is_none());
        crate::app::api::test_support::shutdown_test_runtimes(&mut app);
    }

    #[test]
    fn mobile_switcher_new_tab_opens_dialog_when_enabled() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("one");
        ws.test_add_tab(Some("logs"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 44, 20));
        let switch = app.state.view.mobile_menu_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            switch.x + 1,
            switch.y + 1,
        ));
        let viewport = crate::ui::mobile_switcher_areas(&app.state).viewport;
        let new_tab_row = (viewport.y..viewport.y + viewport.height)
            .find(|row| {
                matches!(
                    crate::ui::mobile_switcher_target_at(&app.state, viewport.x + 2, *row),
                    Some(crate::ui::MobileSwitcherTarget::NewTab)
                )
            })
            .expect("new tab row");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            viewport.x + 2,
            new_tab_row,
        ));

        assert_eq!(app.state.mode, Mode::RenameTab);
        assert!(app.state.creating_new_tab);
    }

    #[test]
    fn mobile_switcher_new_tab_skips_dialog_when_prompt_disabled() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("one");
        ws.test_add_tab(Some("logs"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.prompt_new_tab_name = false;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 44, 20));
        let switch = app.state.view.mobile_menu_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            switch.x + 1,
            switch.y + 1,
        ));
        let viewport = crate::ui::mobile_switcher_areas(&app.state).viewport;

        let new_tab_row = (viewport.y..viewport.y + viewport.height)
            .find(|row| {
                matches!(
                    crate::ui::mobile_switcher_target_at(&app.state, viewport.x + 2, *row),
                    Some(crate::ui::MobileSwitcherTarget::NewTab)
                )
            })
            .expect("new tab row");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            viewport.x + 2,
            new_tab_row,
        ));
        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(!app.state.creating_new_tab);
        assert!(app.state.request_new_tab);
        assert!(app.state.requested_new_tab_name.is_none());
    }

    #[test]
    fn desktop_new_tab_button_skips_dialog_when_prompt_disabled() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.prompt_new_tab_name = false;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let new_tab_area = app.state.view.new_tab_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            new_tab_area.x + 1,
            new_tab_area.y,
        ));

        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(!app.state.creating_new_tab);
        assert!(app.state.request_new_tab);
        assert!(app.state.requested_new_tab_name.is_none());
    }

    #[test]
    fn mobile_switcher_swallows_non_left_mouse_events() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 44, 20));
        let switch = app.state.view.mobile_menu_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            switch.x + 1,
            switch.y + 1,
        ));
        assert_eq!(app.state.mode, Mode::Navigate);

        let viewport = crate::ui::mobile_switcher_areas(&app.state).viewport;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            viewport.x + 2,
            viewport.y + 2,
        ));

        assert_eq!(app.state.mode, Mode::Navigate);
        assert!(app.state.context_menu.is_none());
    }

    #[test]
    fn mobile_switch_button_does_not_bypass_rename_modal() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::RenameTab;
        app.state.creating_new_tab = true;
        app.state.name_input = "new tab".into();

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 44, 20));
        let switch = app.state.view.mobile_menu_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            switch.x + 1,
            switch.y + 1,
        ));

        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(!app.state.creating_new_tab);
        assert!(!app.state.request_new_tab);
    }

    #[test]
    fn mobile_switcher_close_returns_to_terminal() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 44, 20));
        let switch = app.state.view.mobile_menu_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            switch.x + 1,
            switch.y + 1,
        ));
        assert_eq!(app.state.mode, Mode::Navigate);

        let close = crate::ui::mobile_switcher_areas(&app.state).close;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            close.x + 1,
            close.y,
        ));

        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[test]
    fn wheel_routing_uses_alternate_scroll_in_fullscreen_without_mouse_reporting() {
        let input_state = crate::pane::InputState {
            alternate_screen: true,
            application_cursor: false,
            bracketed_paste: false,
            focus_reporting: false,
            mouse_protocol_mode: crate::input::MouseProtocolMode::None,
            mouse_protocol_encoding: crate::input::MouseProtocolEncoding::Default,
            mouse_alternate_scroll: true,
            modify_other_keys: false,
            color_scheme_reporting: false,
        };

        assert_eq!(wheel_routing(input_state), WheelRouting::AlternateScroll);
    }

    #[test]
    fn wheel_routing_falls_back_to_host_scrollback() {
        let input_state = crate::pane::InputState {
            alternate_screen: false,
            application_cursor: false,
            bracketed_paste: false,
            focus_reporting: false,
            mouse_protocol_mode: crate::input::MouseProtocolMode::None,
            mouse_protocol_encoding: crate::input::MouseProtocolEncoding::Default,
            mouse_alternate_scroll: true,
            modify_other_keys: false,
            color_scheme_reporting: false,
        };

        assert_eq!(wheel_routing(input_state), WheelRouting::HostScroll);
    }

    #[test]
    fn tab_row_pane_toggle_requests_a_split_when_no_sibling_exists() {
        for direction in [
            crate::app::state::PaneToggleDirection::Below,
            crate::app::state::PaneToggleDirection::Right,
        ] {
            let mut app = app_for_mouse_test();
            app.state.show_pane_toggle_buttons = true;
            app.state.workspaces = vec![Workspace::test_new("one")];
            app.state.active = Some(0);
            app.state.selected = 0;
            app.state.mode = Mode::Terminal;
            app.state.ensure_test_terminals();

            crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
            let area = match direction {
                crate::app::state::PaneToggleDirection::Below => {
                    app.state.view.pane_toggle_below_hit_area
                }
                crate::app::state::PaneToggleDirection::Right => {
                    app.state.view.pane_toggle_right_hit_area
                }
            };
            assert!(area.width > 0, "{direction:?} button should be visible");

            app.handle_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                area.x + 1,
                area.y,
            ));

            assert_eq!(app.state.request_pane_toggle, Some(direction));
            // No sibling on that side, so the toggle resolves to a split.
            assert_eq!(app.state.pane_toggle_sibling(direction), None);
        }
    }

    #[test]
    fn repo_editor_button_queues_only_when_an_editor_is_available() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.ensure_test_terminals();
        app.state.repo_editor_argv = None;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let button = app.state.view.repo_editor_button_hit_area;
        assert_eq!(button.width, crate::ui::REPO_EDITOR_BUTTON_WIDTH);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            button.x + 1,
            button.y,
        ));
        assert!(!app.state.request_open_repo_editor);

        app.state.repo_editor_argv = Some(vec!["nvim".into()]);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            button.x + 1,
            button.y,
        ));
        assert!(app.state.request_open_repo_editor);
    }

    #[test]
    fn git_menu_button_and_selectable_rows_queue_actions_but_status_does_not() {
        let mut app = app_for_mouse_test();
        app.state.show_pull_button = true;
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.ensure_test_terminals();
        let focused_pane = app.state.workspaces[0]
            .focused_pane_id()
            .expect("focused pane");
        let terminal_id = app.state.workspaces[0]
            .terminal_id(focused_pane)
            .expect("terminal")
            .clone();
        let cwd = app
            .state
            .terminals
            .get(&terminal_id)
            .expect("terminal state")
            .cwd
            .clone();
        app.state
            .git_root_for_cwd
            .insert(cwd.clone(), Some(cwd.clone()));

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let button = app.state.view.git_menu_button_hit_area;
        assert_eq!(button.width, 10);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            button.x + 1,
            button.y,
        ));
        assert_eq!(app.state.mode, Mode::GitMenu);

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let commit = app.state.view.git_menu_row_hit_areas[1];
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            commit.x + 1,
            commit.y,
        ));
        assert_eq!(
            app.state.request_git_action,
            Some(crate::app::state::GitAction::Commit)
        );
        assert_eq!(app.state.mode, Mode::Terminal);

        app.state.request_git_action = None;
        app.state.status_git_cwd = app.state.status_focused_cwd.clone();
        app.state.status_git_ahead_behind = Some((0, 1));
        app.state.mode = Mode::GitMenu;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let status = app.state.view.git_menu_row_hit_areas[4];
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            status.x + 1,
            status.y,
        ));
        assert_eq!(app.state.request_git_action, None);
        assert_eq!(app.state.mode, Mode::GitMenu);

        app.state.git_root_for_cwd.insert(cwd, None);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        assert_eq!(app.state.view.git_menu_row_hit_areas.len(), 1);
        let info = app.state.view.git_menu_row_hit_areas[0];
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            info.x + 1,
            info.y,
        ));
        assert_eq!(app.state.request_git_action, None);
        assert_eq!(app.state.mode, Mode::GitMenu);
    }

    #[test]
    fn tab_row_pane_toggle_closes_the_pane_already_in_that_direction() {
        for (direction, split) in [
            (
                crate::app::state::PaneToggleDirection::Below,
                ratatui::layout::Direction::Vertical,
            ),
            (
                crate::app::state::PaneToggleDirection::Right,
                ratatui::layout::Direction::Horizontal,
            ),
        ] {
            let mut app = app_for_mouse_test();
            app.state.show_pane_toggle_buttons = true;
            let mut ws = Workspace::test_new("one");
            let sibling = ws.test_split(split);
            let root = ws.tabs[0].root_pane;
            ws.tabs[0].layout.focus_pane(root);
            app.state.workspaces = vec![ws];
            app.state.active = Some(0);
            app.state.selected = 0;
            app.state.mode = Mode::Terminal;
            app.state.ensure_test_terminals();

            crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
            let area = match direction {
                crate::app::state::PaneToggleDirection::Below => {
                    app.state.view.pane_toggle_below_hit_area
                }
                crate::app::state::PaneToggleDirection::Right => {
                    app.state.view.pane_toggle_right_hit_area
                }
            };
            assert_eq!(app.state.pane_toggle_sibling(direction), Some(sibling));

            app.handle_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                area.x + 1,
                area.y,
            ));
            assert_eq!(app.state.request_pane_toggle, Some(direction));
            assert!(app.apply_pane_toggle_request());

            let panes = app.state.workspaces[0].tabs[0].layout.pane_ids();
            assert_eq!(panes, vec![root], "{direction:?} should close {sibling:?}");
            assert!(app.state.request_pane_toggle.is_none());
        }
    }
}
