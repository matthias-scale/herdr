use std::{
    fs, io,
    io::Write,
    process::Stdio,
    time::{SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Direction;

use crate::{
    app::{
        state::{AppState, Mode},
        App,
    },
    input::TerminalKey,
    layout::NavDirection,
    terminal::TerminalRuntimeRegistry,
};

#[cfg(test)]
pub(crate) fn terminal_direct_navigation_action(
    state: &AppState,
    key: TerminalKey,
) -> Option<NavigateAction> {
    action_for_key(state, key, BindingDispatch::Direct)
}

pub(crate) fn terminal_direct_non_indexed_navigation_action(
    state: &AppState,
    key: &TerminalKey,
) -> Option<NavigateAction> {
    non_indexed_action_for_key(state, key, BindingDispatch::Direct)
}

pub(crate) fn terminal_direct_indexed_navigation_action(
    state: &AppState,
    key: &TerminalKey,
) -> Option<NavigateAction> {
    indexed_navigation_action(state, key, BindingDispatch::Direct)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActionContext {
    Direct,
    Prefix,
    Navigate,
}

impl App {
    fn cancel_copy_mode_if_active(&mut self) {
        if self.state.copy_mode.is_some() {
            self.state.cancel_copy_mode(&self.terminal_runtimes);
        }
    }

    pub(crate) fn handle_prefix_key(&mut self, raw_key: TerminalKey) {
        let key = raw_key.as_key_event();
        self.state.update_dismissed = true;

        if matches!(key.code, KeyCode::Modifier(_)) {
            return;
        }

        if self.state.is_prefix_key(&raw_key) {
            if self.state.copy_mode_pane_is_focused() {
                self.state.cancel_copy_mode(&self.terminal_runtimes);
            }
            if !self.pass_through_key_to_focused_pane(raw_key) {
                leave_command_mode(&mut self.state);
            }
            return;
        }

        if key.code == KeyCode::Esc {
            leave_command_mode(&mut self.state);
            return;
        }

        if key.code == KeyCode::Char('h') && key.modifiers == KeyModifiers::CONTROL {
            self.state.toggle_loop_run_history();
            leave_command_mode(&mut self.state);
            return;
        }

        if let Some(action) =
            non_indexed_action_for_key(&self.state, &raw_key, BindingDispatch::Prefix)
        {
            self.execute_prefix_key_action(action);
            return;
        }

        if let Some(binding) = command_for_key(&self.state, &raw_key, BindingDispatch::Prefix) {
            self.cancel_copy_mode_if_active();
            self.launch_custom_command(binding, ActionContext::Prefix);
            return;
        }

        if let Some(action) =
            indexed_navigation_action(&self.state, &raw_key, BindingDispatch::Prefix)
        {
            self.execute_prefix_key_action(action);
            return;
        }

        leave_command_mode(&mut self.state);
    }

    fn execute_prefix_key_action(&mut self, action: NavigateAction) {
        if action == NavigateAction::EditScrollback {
            let previous_mode = self.state.mode;
            self.cancel_copy_mode_if_active();
            self.launch_focused_scrollback_editor();
            finish_action_context(&mut self.state, ActionContext::Prefix, previous_mode);
        } else if action == NavigateAction::CopyMode {
            self.cancel_copy_mode_if_active();
            self.execute_tui_navigate_action(action, ActionContext::Prefix);
        } else if copy_mode_survives_prefix_action(action) {
            self.execute_tui_navigate_action(action, ActionContext::Prefix);
            if self.state.copy_mode.is_some() {
                self.state.sync_copy_mode_with_focus();
            }
        } else {
            self.cancel_copy_mode_if_active();
            self.execute_tui_navigate_action(action, ActionContext::Prefix);
        }
        self.selection_autoscroll_deadline = None;
    }

    pub(crate) fn handle_navigate_key(&mut self, raw_key: TerminalKey) {
        let key = raw_key.as_key_event();
        self.state.update_dismissed = true;

        if key.code == KeyCode::Esc || self.state.is_prefix_key(&raw_key) {
            leave_navigate_mode(&mut self.state);
            return;
        }

        if self
            .state
            .keybinds
            .navigate
            .workspace_up
            .matches_direct_key(&raw_key)
        {
            self.state.move_selected_workspace_by_visible_delta(-1);
            return;
        }
        if self
            .state
            .keybinds
            .navigate
            .workspace_down
            .matches_direct_key(&raw_key)
        {
            self.state.move_selected_workspace_by_visible_delta(1);
            return;
        }

        if let Some(action) = navigate_reserved_action_for_key(&self.state, &raw_key) {
            self.execute_tui_navigate_action(action, ActionContext::Navigate);
            return;
        }

        if let Some(action) = navigate_mode_non_indexed_action_for_key(&self.state, &raw_key) {
            if action == NavigateAction::EditScrollback {
                self.launch_focused_scrollback_editor();
            } else {
                self.execute_tui_navigate_action(action, ActionContext::Navigate);
            }
            self.selection_autoscroll_deadline = None;
            return;
        }

        if let Some(binding) = command_for_key(&self.state, &raw_key, BindingDispatch::Prefix) {
            self.launch_custom_command(binding, ActionContext::Navigate);
            return;
        }

        if let Some(action) = navigate_mode_indexed_action_for_key(&self.state, &raw_key) {
            self.execute_tui_navigate_action(action, ActionContext::Navigate);
            self.selection_autoscroll_deadline = None;
        }
    }

    pub(super) fn execute_tui_navigate_action(
        &mut self,
        action: NavigateAction,
        context: ActionContext,
    ) {
        let previous_mode = self.state.mode;
        match action {
            NavigateAction::NewWorkspace => {
                self.begin_tui_workspace_create("tui.key.workspace.create");
            }
            NavigateAction::NewWorktree => {
                if let Some(ws_idx) = workspace_action_target(&self.state, context).filter(|idx| {
                    workspace_can_start_worktree_action(&self.state, &self.terminal_runtimes, *idx)
                }) {
                    self.state.request_new_linked_worktree = Some(ws_idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::OpenWorktree => {
                if let Some(ws_idx) = workspace_action_target(&self.state, context).filter(|idx| {
                    workspace_can_start_worktree_action(&self.state, &self.terminal_runtimes, *idx)
                }) {
                    self.state.request_open_existing_worktree = Some(ws_idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::RemoveWorktree => {
                if let Some(ws_idx) = workspace_action_target(&self.state, context) {
                    self.state.request_remove_linked_worktree = Some(ws_idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::RenameWorkspace => {
                if let Some(ws_idx) = workspace_action_target(&self.state, context) {
                    super::modal::open_rename_workspace(
                        &mut self.state,
                        &self.terminal_runtimes,
                        ws_idx,
                    );
                }
            }
            NavigateAction::CloseWorkspace => {
                if let Some(ws_idx) = workspace_action_target(&self.state, context) {
                    self.state.selected = ws_idx;
                    if self.state.confirm_close {
                        super::modal::open_confirm_close(&mut self.state);
                    } else {
                        self.close_workspace_idx_via_api(ws_idx);
                        leave_navigate_mode(&mut self.state);
                    }
                }
            }
            NavigateAction::SwitchWorkspace(idx) => {
                if let Some(ws_idx) = self.state.workspace_at_visible_position(idx) {
                    self.focus_workspace_idx_via_api(ws_idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::SwitchTab(idx) => {
                if self
                    .state
                    .active
                    .and_then(|ws_idx| self.state.workspaces.get(ws_idx))
                    .is_some_and(|ws| idx < ws.tabs.len())
                {
                    self.focus_tab_idx_via_api(idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::FocusAgent(idx) => {
                if let Some((ws_idx, pane_id)) = self.agent_entry_target(idx) {
                    self.focus_pane_internal_via_api(ws_idx, pane_id);
                    leave_navigate_mode(&mut self.state);
                    self.state.ensure_agent_row_visible(ws_idx, pane_id);
                }
            }
            NavigateAction::WorkspacePicker => {
                self.state.begin_workspace_picker_presentation();
                self.state.mode = Mode::Navigate;
            }
            NavigateAction::PreviousWorkspace => {
                if let Some(ws_idx) = self.relative_visible_workspace(-1) {
                    self.focus_workspace_idx_via_api(ws_idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::NextWorkspace => {
                if let Some(ws_idx) = self.relative_visible_workspace(1) {
                    self.focus_workspace_idx_via_api(ws_idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::PreviousAgent => {
                if let Some((_idx, ws_idx, pane_id)) = self.relative_agent_entry(false) {
                    self.focus_pane_internal_via_api(ws_idx, pane_id);
                    leave_navigate_mode(&mut self.state);
                    self.state.ensure_agent_row_visible(ws_idx, pane_id);
                }
            }
            NavigateAction::NextAgent => {
                if let Some((_idx, ws_idx, pane_id)) = self.relative_agent_entry(true) {
                    self.focus_pane_internal_via_api(ws_idx, pane_id);
                    leave_navigate_mode(&mut self.state);
                    self.state.ensure_agent_row_visible(ws_idx, pane_id);
                }
            }
            NavigateAction::NewTab => {
                if self.state.active.is_some() {
                    if self.state.prompt_new_tab_name {
                        super::modal::open_new_tab_dialog(&mut self.state);
                    } else {
                        self.runtime_tab_create(
                            "tui.key.tab.create",
                            crate::api::schema::TabCreateParams {
                                workspace_id: None,
                                cwd: None,
                                focus: true,
                                label: None,
                                env: Default::default(),
                            },
                        );
                        leave_navigate_mode(&mut self.state);
                    }
                }
            }
            NavigateAction::RenameTab => {
                super::modal::open_rename_active_tab(&mut self.state, false)
            }
            NavigateAction::ToggleTabPrio => {
                if toggle_tab_prio(&mut self.state, context) {
                    self.schedule_session_save();
                    if self.no_session {
                        self.state.mark_session_dirty();
                    }
                    if context == ActionContext::Navigate {
                        leave_navigate_mode(&mut self.state);
                    }
                }
            }
            NavigateAction::TogglePrioPanel => {
                self.state.toggle_prio_panel();
                self.schedule_session_save();
                if self.no_session {
                    self.state.mark_session_dirty();
                }
                if context == ActionContext::Navigate {
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::ToggleBlockedFilter => {
                self.state.blocked_filter = !self.state.blocked_filter;
                self.state.workspace_scroll = crate::ui::normalized_workspace_scroll(
                    &self.state,
                    self.state.view.sidebar_rect,
                    self.state.workspace_scroll,
                );
                if context == ActionContext::Navigate {
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::PreviousTab => {
                if let Some(tab_idx) = self.relative_tab(-1) {
                    self.focus_tab_idx_via_api(tab_idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::NextTab => {
                if let Some(tab_idx) = self.relative_tab(1) {
                    self.focus_tab_idx_via_api(tab_idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::PreviousWindow => {
                self.focus_relative_window(false);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::NextWindow => {
                self.focus_relative_window(true);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::NextBlockedWindow => {
                self.focus_next_blocked_window();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::CloseTab => {
                if !self.close_active_tab_via_api_requires_confirmation() {
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::RenamePane => {
                if let Some(pane_id) = self
                    .state
                    .active
                    .and_then(|ws_idx| self.state.workspaces.get(ws_idx))
                    .and_then(|ws| ws.focused_pane_id())
                {
                    super::modal::open_rename_pane(&mut self.state, pane_id);
                }
            }
            NavigateAction::FocusPaneLeft => {
                self.focus_pane_direction_in_context(NavDirection::Left, context)
            }
            NavigateAction::FocusPaneDown => {
                self.focus_pane_direction_in_context(NavDirection::Down, context)
            }
            NavigateAction::FocusPaneUp => {
                self.focus_pane_direction_in_context(NavDirection::Up, context)
            }
            NavigateAction::FocusPaneRight => {
                self.focus_pane_direction_in_context(NavDirection::Right, context)
            }
            NavigateAction::SwapPaneLeft => {
                self.swap_pane_direction_via_api(NavDirection::Left);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::SwapPaneDown => {
                self.swap_pane_direction_via_api(NavDirection::Down);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::SwapPaneUp => {
                self.swap_pane_direction_via_api(NavDirection::Up);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::SwapPaneRight => {
                self.swap_pane_direction_via_api(NavDirection::Right);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::SplitVertical => {
                self.split_focused_pane_via_api(crate::api::schema::SplitDirection::Right);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::SplitHorizontal => {
                self.split_focused_pane_via_api(crate::api::schema::SplitDirection::Down);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::SplitLeft => {
                self.split_focused_pane_via_api(crate::api::schema::SplitDirection::Left);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::SplitUp => {
                self.split_focused_pane_via_api(crate::api::schema::SplitDirection::Up);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::ClosePane => {
                if !self.close_focused_pane_via_api_requires_confirmation() {
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::EditScrollback => {}
            NavigateAction::CopyMode => self.state.enter_copy_mode(&self.terminal_runtimes),
            NavigateAction::Zoom => {
                self.zoom_focused_pane_via_api();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::TogglePinTab => {
                self.toggle_pin_active_tab_via_api();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::EnterResizeMode => self.state.mode = Mode::Resize,
            NavigateAction::ToggleSidebar => {
                self.state.sidebar_collapsed = !self.state.sidebar_collapsed;
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::ToggleStatusDetail => {
                self.state.status_bar_expanded = !self.state.status_bar_expanded;
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::ToggleDock => {
                self.state.dock_collapsed = !self.state.dock_collapsed;
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::PreviousDockTab => {
                self.state.dock_tab = self.state.dock_tab.previous();
                if self.state.dock_tab == crate::app::DockTab::Editor {
                    self.state.dock_editor_focused = true;
                }
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::NextDockTab => {
                self.state.dock_tab = self.state.dock_tab.next();
                if self.state.dock_tab == crate::app::DockTab::Editor {
                    self.state.dock_editor_focused = true;
                }
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::EditScratchpad => {
                self.open_scratchpad_in_editor();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::ShowScratchpad => {
                self.state.show_scratchpad_tab();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::CyclePaneNext => {
                self.cycle_pane_via_api(false);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::CyclePanePrevious => {
                self.cycle_pane_via_api(true);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::LastPane => {
                self.last_pane_via_api();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::Help => super::modal::open_keybind_help(&mut self.state),
            NavigateAction::Settings => super::settings::open_settings(&mut self.state),
            NavigateAction::ReloadConfig => {
                self.runtime_server_reload_config("tui.server.reload_config");
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::OpenNotificationTarget => {
                self.focus_toast_target_via_api();
                if self.state.mode == Mode::Navigate {
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::OpenWorkUrl | NavigateAction::OpenWorkLink => {
                self.open_or_copy_work_link(crate::app::state::WorkLinkPickerAction::Open, context)
            }
            NavigateAction::CopyWorkUrl | NavigateAction::CopyWorkLink => {
                self.open_or_copy_work_link(crate::app::state::WorkLinkPickerAction::Copy, context)
            }
            NavigateAction::CopyWorkTicket => self.copy_focused_work_ticket(),
            NavigateAction::CopyWorkPr => self.copy_focused_work_pr(),
            NavigateAction::CopyWorkPreview => self.copy_focused_work_preview(),
            NavigateAction::ToggleInfoPanel => {
                self.state.info_panel_expanded = !self.state.info_panel_expanded;
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::OpenSymphony => {
                self.state.toggle_symphony();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::OpenInbox => {
                self.state.toggle_inbox();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::OpenHome => {
                self.state.toggle_home();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::Detach => {
                super::modal::request_detach(&mut self.state);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::OpenNavigator => {
                self.state.open_navigator_from(&self.terminal_runtimes)
            }
        }

        finish_action_context(&mut self.state, context, previous_mode);
    }

    pub(crate) fn focus_workspace_idx_via_api(&mut self, ws_idx: usize) {
        let workspace_id = self.public_workspace_id(ws_idx);
        self.runtime_workspace_focus("tui.workspace.focus", workspace_id);
    }

    pub(crate) fn show_work_link_notice(&mut self, message: &str) {
        self.state.copy_feedback = Some(crate::app::state::CopyFeedback {
            message: message.to_string(),
        });
        self.copy_feedback_deadline =
            Some(std::time::Instant::now() + super::super::COPY_FEEDBACK_DURATION);
    }

    fn open_or_copy_work_link(
        &mut self,
        action: crate::app::state::WorkLinkPickerAction,
        context: ActionContext,
    ) {
        let candidates = focused_work_context(&self.state)
            .map(crate::work_context::work_link_candidates)
            .unwrap_or_default();
        match candidates.as_slice() {
            [] => {
                self.show_work_link_notice("focused pane has no work link");
                leave_navigate_mode(&mut self.state);
            }
            [candidate] => {
                self.perform_work_link_action(action, &candidate.url);
                leave_navigate_mode(&mut self.state);
            }
            _ => {
                self.state.work_link_picker = Some(crate::app::state::WorkLinkPickerState {
                    candidates: candidates.into_iter().take(9).collect(),
                    action,
                    return_mode: if context == ActionContext::Navigate {
                        Mode::Navigate
                    } else {
                        Mode::Terminal
                    },
                });
                self.state.mode = Mode::WorkLinkPicker;
            }
        }
    }

    fn perform_work_link_action(
        &mut self,
        action: crate::app::state::WorkLinkPickerAction,
        url: &str,
    ) {
        match action {
            crate::app::state::WorkLinkPickerAction::Open => {
                if let Err(error) = crate::platform::open_url(url) {
                    tracing::warn!(%error, %url, "failed to open focused pane work link");
                    self.show_work_link_notice("could not open work link");
                }
            }
            crate::app::state::WorkLinkPickerAction::Copy => {
                if self
                    .event_tx
                    .try_send(crate::events::AppEvent::ClipboardWrite {
                        content: url.as_bytes().to_vec(),
                    })
                    .is_err()
                {
                    tracing::warn!("failed to queue focused pane work link clipboard event");
                    self.show_work_link_notice("could not copy work link");
                }
            }
        }
    }

    pub(crate) fn handle_work_link_picker_key(&mut self, key: KeyEvent) {
        let Some(picker) = self.state.work_link_picker.clone() else {
            self.state.mode = Mode::Terminal;
            return;
        };
        if key.code == KeyCode::Esc {
            self.state.work_link_picker = None;
            self.state.mode = picker.return_mode;
            return;
        }
        let Some(index) = (match key.code {
            KeyCode::Char(digit @ '1'..='9') if key.modifiers.is_empty() => {
                Some(usize::from(digit as u8 - b'1'))
            }
            _ => None,
        }) else {
            return;
        };
        let Some(candidate) = picker.candidates.get(index) else {
            return;
        };
        let url = candidate.url.clone();
        let still_live = focused_work_context(&self.state).is_some_and(|context| {
            crate::work_context::work_link_candidates(context)
                .iter()
                .any(|live| live.url == url)
        });
        self.state.work_link_picker = None;
        if !still_live {
            self.show_work_link_notice("work link is stale");
            self.state.mode = picker.return_mode;
            return;
        }
        self.perform_work_link_action(picker.action, &url);
        leave_navigate_mode(&mut self.state);
    }

    fn copy_focused_work_ticket(&mut self) {
        self.copy_focused_work_value(
            focused_work_context(&self.state)
                .and_then(|context| context.primary_ticket().map(str::to_string)),
            "focused pane has no work ticket",
            "could not copy work ticket",
        );
    }

    fn copy_focused_work_pr(&mut self) {
        self.copy_focused_work_value(
            focused_work_context(&self.state)
                .and_then(|context| context.primary_pr().map(str::to_string)),
            "focused pane has no pull request",
            "could not copy pull request",
        );
    }

    fn copy_focused_work_preview(&mut self) {
        self.copy_focused_work_value(
            focused_work_context(&self.state)
                .and_then(|context| context.preview_urls.first().cloned()),
            "focused pane has no preview URL",
            "could not copy preview URL",
        );
    }

    fn copy_focused_work_value(
        &mut self,
        value: Option<String>,
        missing_notice: &str,
        failure_notice: &str,
    ) {
        let Some(value) = value else {
            self.show_work_link_notice(missing_notice);
            leave_navigate_mode(&mut self.state);
            return;
        };
        if self
            .event_tx
            .try_send(crate::events::AppEvent::ClipboardWrite {
                content: value.into_bytes(),
            })
            .is_err()
        {
            tracing::warn!("failed to queue focused pane work-context clipboard event");
            self.show_work_link_notice(failure_notice);
        }
        leave_navigate_mode(&mut self.state);
    }

    pub(crate) fn close_workspace_idx_via_api(&mut self, ws_idx: usize) {
        let workspace_id = self.public_workspace_id(ws_idx);
        self.runtime_workspace_close("tui.workspace.close", workspace_id);
    }

    pub(crate) fn move_workspace_via_api(&mut self, source_ws_idx: usize, insert_idx: usize) {
        let workspace_id = self.public_workspace_id(source_ws_idx);
        self.runtime_workspace_move(
            "tui.workspace.move",
            crate::api::schema::WorkspaceMoveParams {
                workspace_id,
                insert_index: insert_idx,
            },
        );
    }

    pub(crate) fn move_workspace_block_via_api(
        &mut self,
        params: crate::api::schema::WorkspaceMoveBlockParams,
    ) {
        self.runtime_workspace_move_block("tui.workspace.move_block", params);
    }

    pub(crate) fn focus_tab_idx_via_api(&mut self, tab_idx: usize) {
        let Some(ws_idx) = self.state.active else {
            return;
        };
        let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) else {
            return;
        };
        self.runtime_tab_focus("tui.tab.focus", tab_id);
    }

    pub(crate) fn focus_workspace_tab_via_api(&mut self, ws_idx: usize, tab_idx: usize) {
        let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) else {
            return;
        };
        self.runtime_tab_focus("tui.sidebar.tab.focus", tab_id);
    }

    /// Windows are Herdr tabs. Canonical workspace/vector/tab order is used so
    /// agent lifecycle or cwd changes cannot affect global navigation.
    fn focus_relative_window(&mut self, forward: bool) {
        let windows = self
            .state
            .workspaces
            .iter()
            .enumerate()
            .flat_map(|(ws_idx, ws)| (0..ws.tabs.len()).map(move |tab_idx| (ws_idx, tab_idx)))
            .collect::<Vec<_>>();
        let Some(active_ws) = self.state.active else {
            return;
        };
        let active_tab = self.state.workspaces[active_ws].active_tab_index();
        let Some(current) = windows
            .iter()
            .position(|window| *window == (active_ws, active_tab))
        else {
            return;
        };
        let Some(next) = crate::workspace::relative_window_index(windows.len(), current, forward)
        else {
            return;
        };
        let (ws_idx, tab_idx) = windows[next];
        let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) else {
            return;
        };
        self.runtime_tab_focus("tui.window.focus_relative", tab_id);
    }

    fn focus_next_blocked_window(&mut self) {
        let Some((ws_idx, tab_idx)) = next_blocked_window_target(&self.state) else {
            return;
        };
        let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) else {
            return;
        };
        self.runtime_tab_focus("tui.window.focus_next_blocked", tab_id);
    }

    pub(crate) fn close_active_tab_via_api_requires_confirmation(&mut self) -> bool {
        let Some(ws_idx) = self.state.active else {
            return false;
        };
        if self
            .state
            .workspaces
            .get(ws_idx)
            .is_some_and(|ws| ws.tabs.len() <= 1)
        {
            if self.state.confirm_implicit_worktree_group_close(ws_idx) {
                return true;
            }
            self.close_workspace_idx_via_api(ws_idx);
            return false;
        }
        let tab_idx = self.state.workspaces[ws_idx].active_tab_index();
        let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) else {
            return false;
        };
        self.runtime_tab_close("tui.tab.close", tab_id);
        false
    }

    pub(crate) fn move_tab_via_api(
        &mut self,
        ws_idx: usize,
        source_tab_idx: usize,
        insert_idx: usize,
    ) {
        let Some(tab_id) = self.public_tab_id(ws_idx, source_tab_idx) else {
            return;
        };
        self.runtime_tab_move(
            "tui.tab.move",
            crate::api::schema::TabMoveParams {
                tab_id,
                insert_index: insert_idx,
            },
        );
    }

    pub(crate) fn focus_pane_internal_via_api(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) {
        let Some(pane_id) = self.public_pane_id(ws_idx, pane_id) else {
            return;
        };
        self.runtime_pane_focus("tui.pane.focus", pane_id);
    }

    pub(crate) fn focus_pane_direction_via_api(&mut self, direction: NavDirection) {
        if let Some((ws_idx, target)) = self.directional_pane_target_from_view(direction) {
            self.focus_pane_internal_via_api(ws_idx, target);
            return;
        }
        self.runtime_pane_focus_direction(
            "tui.pane.focus_direction",
            crate::api::schema::PaneFocusDirectionParams {
                pane_id: None,
                direction: api_pane_direction(direction),
            },
        );
    }

    fn focus_pane_direction_in_context(&mut self, direction: NavDirection, context: ActionContext) {
        let preserve_navigate_mode =
            context == ActionContext::Navigate && self.state.mode == Mode::Navigate;
        self.focus_pane_direction_via_api(direction);
        if preserve_navigate_mode {
            self.state.mode = Mode::Navigate;
        }
    }

    pub(crate) fn swap_pane_direction_via_api(&mut self, direction: NavDirection) {
        if let Some((ws_idx, source, target)) = self.directional_pane_swap_from_view(direction) {
            let source_pane_id = self.public_pane_id(ws_idx, source);
            let target_pane_id = self.public_pane_id(ws_idx, target);
            if let (Some(source_pane_id), Some(target_pane_id)) = (source_pane_id, target_pane_id) {
                self.runtime_pane_swap(
                    "tui.pane.swap_exact",
                    crate::api::schema::PaneSwapParams {
                        pane_id: None,
                        direction: None,
                        source_pane_id: Some(source_pane_id),
                        target_pane_id: Some(target_pane_id),
                    },
                );
                return;
            }
        }
        self.runtime_pane_swap(
            "tui.pane.swap",
            crate::api::schema::PaneSwapParams {
                pane_id: None,
                direction: Some(api_pane_direction(direction)),
                source_pane_id: None,
                target_pane_id: None,
            },
        );
    }

    pub(crate) fn split_focused_pane_via_api(
        &mut self,
        direction: crate::api::schema::SplitDirection,
    ) {
        self.runtime_pane_split(
            "tui.pane.split",
            crate::api::schema::PaneSplitParams {
                workspace_id: None,
                target_pane_id: None,
                direction,
                ratio: None,
                cwd: None,
                focus: true,
                env: Default::default(),
            },
        );
    }

    pub(crate) fn close_focused_pane_via_api_requires_confirmation(&mut self) -> bool {
        let Some((ws_idx, pane_id)) = self.focused_pane_target() else {
            return false;
        };
        let Some(pane_id) = self.public_pane_id(ws_idx, pane_id) else {
            return false;
        };
        self.runtime_pane_close("tui.pane.close", pane_id);
        self.state.mode == Mode::ConfirmClose
    }

    pub(crate) fn zoom_focused_pane_via_api(&mut self) {
        self.runtime_pane_zoom(
            "tui.pane.zoom",
            crate::api::schema::PaneZoomParams {
                pane_id: None,
                mode: crate::api::schema::PaneZoomMode::Toggle,
            },
        );
    }

    /// Pin or unpin whichever tab is in front. Takes an explicit index so the
    /// same path serves both the keybinding (active tab) and a click on some
    /// other tab's pin glyph.
    pub(crate) fn toggle_pin_tab_via_api(&mut self, ws_idx: usize, tab_idx: usize) {
        let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) else {
            return;
        };
        self.runtime_tab_pin(
            "tui.tab.pin",
            crate::api::schema::TabPinParams {
                tab_id,
                mode: crate::api::schema::TabPinMode::Toggle,
            },
        );
    }

    pub(crate) fn toggle_pin_active_tab_via_api(&mut self) {
        let Some(ws_idx) = self.state.active else {
            return;
        };
        let tab_idx = self.state.workspaces[ws_idx].active_tab;
        self.toggle_pin_tab_via_api(ws_idx, tab_idx);
    }

    pub(crate) fn set_split_ratio_via_api(&mut self, path: Vec<bool>, ratio: f32) {
        self.runtime_layout_set_split_ratio(
            "tui.layout.set_split_ratio",
            crate::api::schema::LayoutSetSplitRatioParams {
                tab_id: None,
                pane_id: None,
                path,
                ratio,
            },
        );
    }

    pub(crate) fn cycle_pane_via_api(&mut self, reverse: bool) {
        let Some((ws_idx, pane_id)) = self.focused_pane_target() else {
            return;
        };
        let Some(tab) = self.state.workspaces[ws_idx].active_tab() else {
            return;
        };
        let ids = tab.layout.pane_ids();
        let Some(pos) = ids.iter().position(|id| *id == pane_id) else {
            return;
        };
        let target = if reverse {
            ids[(pos + ids.len() - 1) % ids.len()]
        } else {
            ids[(pos + 1) % ids.len()]
        };
        self.focus_pane_internal_via_api(ws_idx, target);
    }

    pub(crate) fn last_pane_via_api(&mut self) {
        let Some(target) = self.state.previous_pane_focus.clone() else {
            return;
        };
        let Some((ws_idx, _tab_idx)) = self.state.pane_focus_target_indices(&target) else {
            self.state.previous_pane_focus = None;
            return;
        };
        if self.state.current_pane_focus_target().as_ref() == Some(&target) {
            self.state.previous_pane_focus = None;
            return;
        }
        self.focus_pane_internal_via_api(ws_idx, target.pane_id);
    }

    pub(crate) fn focus_toast_target_via_api(&mut self) {
        let Some(target) = self
            .state
            .toast
            .as_ref()
            .and_then(|toast| toast.target.clone())
        else {
            return;
        };
        let Some(ws_idx) = self
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == target.workspace_id)
        else {
            return;
        };
        self.focus_pane_internal_via_api(ws_idx, target.pane_id);
        self.state.toast = None;
        self.state.mode = Mode::Terminal;
    }

    fn focused_pane_target(&self) -> Option<(usize, crate::layout::PaneId)> {
        let ws_idx = self.state.active?;
        let pane_id = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
        Some((ws_idx, pane_id))
    }

    fn directional_pane_target_from_view(
        &self,
        direction: NavDirection,
    ) -> Option<(usize, crate::layout::PaneId)> {
        let ws_idx = self.state.active?;
        let focused = self
            .state
            .view
            .pane_infos
            .iter()
            .find(|pane| pane.is_focused)?;
        let target =
            crate::layout::find_in_direction(focused, direction, &self.state.view.pane_infos)?;
        Some((ws_idx, target))
    }

    fn directional_pane_swap_from_view(
        &self,
        direction: NavDirection,
    ) -> Option<(usize, crate::layout::PaneId, crate::layout::PaneId)> {
        let ws_idx = self.state.active?;
        let focused = self
            .state
            .view
            .pane_infos
            .iter()
            .find(|pane| pane.is_focused)?;
        let target =
            crate::layout::find_in_direction(focused, direction, &self.state.view.pane_infos)?;
        Some((ws_idx, focused.id, target))
    }

    fn relative_visible_workspace(&self, delta: isize) -> Option<usize> {
        let order = self.state.visible_workspace_order();
        if order.is_empty() {
            return None;
        }
        let current = self.state.active.unwrap_or(self.state.selected);
        let current_pos = order.iter().position(|idx| *idx == current).unwrap_or(0);
        let next = (current_pos as isize + delta).rem_euclid(order.len() as isize) as usize;
        order.get(next).copied()
    }

    fn relative_tab(&self, delta: isize) -> Option<usize> {
        let ws = self
            .state
            .active
            .and_then(|ws_idx| self.state.workspaces.get(ws_idx))?;
        if ws.tabs.is_empty() {
            return None;
        }
        Some((ws.active_tab as isize + delta).rem_euclid(ws.tabs.len() as isize) as usize)
    }

    fn agent_entry_target(&self, idx: usize) -> Option<(usize, crate::layout::PaneId)> {
        let entries = crate::ui::agent_panel_entries(&self.state);
        let target = entries.get(idx)?;
        Some((target.ws_idx, target.pane_id))
    }

    fn relative_agent_entry(&self, forward: bool) -> Option<(usize, usize, crate::layout::PaneId)> {
        crate::ui::relative_agent_navigation_entry(&self.state, forward)
            .map(|(idx, target)| (idx, target.ws_idx, target.pane_id))
    }

    fn pass_through_key_to_focused_pane(&mut self, key: TerminalKey) -> bool {
        let Some(ws_idx) = self.state.active else {
            return false;
        };
        let Some(pane_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.focused_pane_id())
        else {
            return false;
        };
        let Some(rt) = self
            .state
            .focused_runtime_in_workspace(&self.terminal_runtimes, ws_idx)
        else {
            return false;
        };

        let bytes = rt.encode_terminal_key(key.clone());
        if bytes.is_empty() || rt.try_send_bytes(Bytes::from(bytes)).is_err() {
            return false;
        }

        self.retire_blocked_hook_authority_for_pane(pane_id, std::time::Instant::now());
        self.state.mode = Mode::Terminal;
        true
    }

    pub(super) fn launch_custom_command(
        &mut self,
        binding: crate::config::CustomCommandKeybind,
        context: ActionContext,
    ) {
        let previous_mode = self.state.mode;
        let previous_toast = self.state.toast.clone();
        let result = match binding.action {
            crate::config::CustomCommandAction::Shell => self.spawn_custom_command(&binding),
            crate::config::CustomCommandAction::Pane => {
                self.spawn_pane_command(&binding.command, Vec::new())
            }
            crate::config::CustomCommandAction::Popup => self.spawn_custom_popup_command(&binding),
            crate::config::CustomCommandAction::PluginAction => self
                .invoke_plugin_action_from_keybind(binding.command.clone())
                .map_err(std::io::Error::other),
        };
        match result {
            Ok(()) => finish_custom_command_context(&mut self.state, context, previous_mode),
            Err(err) => {
                self.state.toast = Some(crate::app::state::ToastNotification {
                    kind: crate::app::state::ToastKind::NeedsAttention,
                    title: "custom command failed".to_string(),
                    context: err.to_string(),
                    position: None,
                    target: None,
                });
                self.sync_toast_deadline(previous_toast);
                finish_custom_command_context(&mut self.state, context, previous_mode);
            }
        }
    }

    fn spawn_custom_popup_command(
        &mut self,
        binding: &crate::config::CustomCommandKeybind,
    ) -> io::Result<()> {
        self.spawn_popup_shell_command(
            &binding.command,
            None,
            self.custom_command_env().0,
            crate::app::popup::PopupGeometry {
                width: binding.width,
                height: binding.height,
            },
        )
    }

    fn custom_command_env(&self) -> (Vec<(String, String)>, Option<std::path::PathBuf>) {
        let mut env = vec![(
            crate::api::SOCKET_PATH_ENV_VAR.to_string(),
            crate::api::socket_path().display().to_string(),
        )];
        if let Ok(current_exe) = std::env::current_exe() {
            env.push((
                "HERDR_BIN_PATH".to_string(),
                current_exe.display().to_string(),
            ));
        }

        let mut cwd = None;
        if let Some(ws_idx) = self.state.active {
            env.push((
                "HERDR_ACTIVE_WORKSPACE_ID".to_string(),
                self.public_workspace_id(ws_idx),
            ));
            if let Some(workspace) = self.state.workspaces.get(ws_idx) {
                let tab_idx = workspace.active_tab_index();
                if let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) {
                    env.push(("HERDR_ACTIVE_TAB_ID".to_string(), tab_id));
                }
                if let Some(pane_id) = workspace.focused_pane_id() {
                    if let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) {
                        env.push(("HERDR_ACTIVE_PANE_ID".to_string(), public_pane_id));
                    }
                    if let Some(pane_cwd) = workspace.active_tab().and_then(|tab| {
                        tab.cwd_for_pane(pane_id, &self.state.terminals, &self.terminal_runtimes)
                    }) {
                        env.push((
                            "HERDR_ACTIVE_PANE_CWD".to_string(),
                            pane_cwd.display().to_string(),
                        ));
                        if pane_cwd.is_dir() {
                            cwd = Some(pane_cwd);
                        }
                    }
                }
            }
        }
        (env, cwd)
    }

    fn spawn_custom_command(
        &mut self,
        binding: &crate::config::CustomCommandKeybind,
    ) -> std::io::Result<()> {
        let mut command = crate::platform::detached_custom_command_process(&binding.command);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let (env, cwd) = self.custom_command_env();
        command.envs(env);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let child = command.spawn()?;
        self.detached_custom_command_children.push(child);
        Ok(())
    }

    pub(super) fn launch_focused_scrollback_editor(&mut self) {
        let previous_toast = self.state.toast.clone();
        match self.open_focused_scrollback_in_editor() {
            Ok(()) => self.sync_toast_deadline(previous_toast),
            Err(err) => {
                self.state.toast = Some(crate::app::state::ToastNotification {
                    kind: crate::app::state::ToastKind::NeedsAttention,
                    title: "edit scrollback failed".to_string(),
                    context: err.to_string(),
                    position: None,
                    target: None,
                });
                self.sync_toast_deadline(previous_toast);
            }
        }
    }

    fn open_focused_scrollback_in_editor(&mut self) -> std::io::Result<()> {
        let ws_idx = self
            .state
            .active
            .ok_or_else(|| std::io::Error::other("no active workspace"))?;
        let ws = self
            .state
            .workspaces
            .get(ws_idx)
            .ok_or_else(|| std::io::Error::other("active workspace disappeared"))?;
        let pane_id = ws
            .focused_pane_id()
            .ok_or_else(|| std::io::Error::other("no focused pane"))?;
        let scrollback = self
            .state
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
            .ok_or_else(|| std::io::Error::other("focused pane has no scrollback runtime"))?
            .recent_text(usize::MAX);

        let path = write_scrollback_temp_file(&scrollback)?;

        let argv = match crate::platform::scrollback_editor_argv(&path) {
            Ok(argv) => argv,
            Err(err) => {
                let _ = fs::remove_file(&path);
                return Err(err);
            }
        };
        let (env, _) = self.custom_command_env();
        let new_pane = match self.spawn_overlay_argv_command(&argv, None, env, vec![path.clone()]) {
            Ok((_, new_pane)) => new_pane,
            Err(err) => {
                let _ = fs::remove_file(&path);
                return Err(err);
            }
        };
        let terminal_id = new_pane.terminal.id.clone();
        self.terminal_runtimes
            .insert(terminal_id.clone(), new_pane.runtime);
        self.state
            .remove_alias_shadowed_by_new_pane(new_pane.pane_id);
        self.state.terminals.insert(terminal_id, new_pane.terminal);

        if let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) {
            self.state.toast = Some(crate::app::state::ToastNotification {
                kind: crate::app::state::ToastKind::Finished,
                title: "opened scrollback".to_string(),
                context: format!("focused pane {public_pane_id}"),
                position: None,
                target: None,
            });
        }
        Ok(())
    }

    fn spawn_pane_command(
        &mut self,
        command: &str,
        temp_files: Vec<std::path::PathBuf>,
    ) -> std::io::Result<()> {
        let Some(ws_idx) = self.state.active else {
            return Err(std::io::Error::other("no active workspace"));
        };
        let previous_focus_target = self.state.current_pane_focus_target();
        let (rows, cols) = self.state.estimate_pane_size();
        let new_rows = rows.max(4);
        let new_cols = cols.max(10);
        let (env, _) = self.custom_command_env();

        let ws = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .ok_or_else(|| std::io::Error::other("active workspace disappeared"))?;
        let tab_idx = ws.active_tab_index();
        let previous_focus = ws
            .focused_pane_id()
            .ok_or_else(|| std::io::Error::other("no focused pane"))?;
        let previous_zoomed = ws.active_tab().map(|tab| tab.zoomed).unwrap_or(false);
        let cwd = ws.active_tab().and_then(|tab| {
            tab.cwd_for_pane(
                previous_focus,
                &self.state.terminals,
                &self.terminal_runtimes,
            )
        });
        let new_pane = ws.split_focused_command(
            Direction::Horizontal,
            new_rows,
            new_cols,
            cwd,
            command,
            env,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            self.state.host_terminal_appearance,
        )?;
        let new_pane_id = new_pane.pane_id;
        self.terminal_runtimes
            .insert(new_pane.terminal.id.clone(), new_pane.runtime);
        self.state
            .terminals
            .insert(new_pane.terminal.id.clone(), new_pane.terminal);
        let new_focus_target = crate::app::state::PaneFocusTarget {
            workspace_id: ws.id.clone(),
            pane_id: new_pane_id,
        };
        if previous_focus_target.as_ref() != Some(&new_focus_target) {
            self.state.previous_pane_focus = previous_focus_target;
        }
        ws.active_tab_mut()
            .expect("workspace must have an active tab")
            .layout
            .focus_pane(new_pane_id);
        ws.active_tab_mut()
            .expect("workspace must have an active tab")
            .zoomed = true;
        self.overlay_panes.insert(
            new_pane_id,
            super::super::OverlayPaneState {
                ws_idx,
                tab_idx,
                previous_focus,
                previous_zoomed,
                temp_files,
            },
        );
        self.state.remove_alias_shadowed_by_new_pane(new_pane_id);
        self.state.mode = Mode::Terminal;
        Ok(())
    }

    pub(crate) fn spawn_overlay_argv_command(
        &mut self,
        argv: &[String],
        cwd: Option<std::path::PathBuf>,
        extra_env: Vec<(String, String)>,
        temp_files: Vec<std::path::PathBuf>,
    ) -> std::io::Result<(usize, crate::workspace::NewPane)> {
        let Some(ws_idx) = self.state.active else {
            return Err(std::io::Error::other("no active workspace"));
        };
        let previous_focus_target = self.state.current_pane_focus_target();
        let (rows, cols) = self.state.estimate_pane_size();
        let new_rows = rows.max(4);
        let new_cols = cols.max(10);

        let ws = self
            .state
            .workspaces
            .get(ws_idx)
            .ok_or_else(|| std::io::Error::other("active workspace disappeared"))?;
        let previous_focus = ws
            .focused_pane_id()
            .ok_or_else(|| std::io::Error::other("no focused pane"))?;
        let cwd = cwd.or_else(|| {
            ws.active_tab().and_then(|tab| {
                tab.cwd_for_pane(
                    previous_focus,
                    &self.state.terminals,
                    &self.terminal_runtimes,
                )
            })
        });

        let (tab_idx, new_pane, workspace_id) = {
            let ws = self
                .state
                .workspaces
                .get_mut(ws_idx)
                .ok_or_else(|| std::io::Error::other("active workspace disappeared"))?;
            let previous_zoomed = ws.active_tab().map(|tab| tab.zoomed).unwrap_or(false);
            let result = ws.split_pane_argv_command(
                previous_focus,
                Direction::Horizontal,
                new_rows,
                new_cols,
                cwd,
                argv,
                extra_env,
                self.state.pane_scrollback_limit_bytes,
                self.state.host_terminal_theme,
                self.state.host_terminal_appearance,
                true,
            );
            let (tab_idx, new_pane) = match result {
                Some(Ok(result)) => result,
                Some(Err(err)) => return Err(err),
                None => return Err(std::io::Error::other("focused pane disappeared")),
            };
            ws.tabs
                .get_mut(tab_idx)
                .ok_or_else(|| std::io::Error::other("plugin overlay tab disappeared"))?
                .zoomed = true;
            self.overlay_panes.insert(
                new_pane.pane_id,
                super::super::OverlayPaneState {
                    ws_idx,
                    tab_idx,
                    previous_focus,
                    previous_zoomed,
                    temp_files,
                },
            );
            (tab_idx, new_pane, ws.id.clone())
        };

        let new_focus_target = crate::app::state::PaneFocusTarget {
            workspace_id,
            pane_id: new_pane.pane_id,
        };
        if previous_focus_target.as_ref() != Some(&new_focus_target) {
            self.state.previous_pane_focus = previous_focus_target;
        }
        self.state.switch_workspace_tab(ws_idx, tab_idx);
        self.state.mode = Mode::Terminal;
        Ok((ws_idx, new_pane))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BindingDispatch {
    Direct,
    Prefix,
}

pub(crate) fn command_for_key(
    state: &AppState,
    key: &TerminalKey,
    dispatch: BindingDispatch,
) -> Option<crate::config::CustomCommandKeybind> {
    state
        .keybinds
        .custom_commands
        .iter()
        .find(|binding| match dispatch {
            BindingDispatch::Direct => binding.bindings.matches_direct_key(key),
            BindingDispatch::Prefix => binding.bindings.matches_prefix_key(key),
        })
        .cloned()
}

fn unmodified_digit_for_key(key: &TerminalKey) -> Option<char> {
    ('1'..='9').find(|digit| {
        crate::config::terminal_key_matches_combo(
            key,
            (
                KeyCode::Char(*digit),
                crossterm::event::KeyModifiers::empty(),
            ),
        )
    })
}

#[cfg(test)]
pub(super) fn handle_navigate_reserved_key(state: &mut AppState, key: TerminalKey) -> bool {
    if let Some(c) = unmodified_digit_for_key(&key) {
        let idx = (c as usize) - ('1' as usize);
        if let Some(ws_idx) = state.workspace_at_visible_position(idx) {
            state.switch_workspace(ws_idx);
            leave_navigate_mode(state);
        }
        return true;
    }

    let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
    if modifiers.is_empty() {
        match code {
            KeyCode::Enter => {
                if !state.workspaces.is_empty() {
                    state.switch_workspace(state.selected);
                    leave_navigate_mode(state);
                }
                return true;
            }
            KeyCode::Tab => {
                state.cycle_pane(false);
                return true;
            }
            KeyCode::BackTab => {
                state.cycle_pane(true);
                return true;
            }
            KeyCode::Left => {
                state.navigate_pane(NavDirection::Left);
                return true;
            }
            KeyCode::Right => {
                state.navigate_pane(NavDirection::Right);
                return true;
            }
            _ => {}
        }
    }

    if state
        .keybinds
        .navigate
        .workspace_up
        .matches_direct_key(&key)
    {
        state.move_selected_workspace_by_visible_delta(-1);
        return true;
    }
    if state
        .keybinds
        .navigate
        .workspace_down
        .matches_direct_key(&key)
    {
        state.move_selected_workspace_by_visible_delta(1);
        return true;
    }
    if state.keybinds.navigate.pane_left.matches_direct_key(&key) {
        state.navigate_pane(NavDirection::Left);
        return true;
    }
    if state.keybinds.navigate.pane_down.matches_direct_key(&key) {
        state.navigate_pane(NavDirection::Down);
        return true;
    }
    if state.keybinds.navigate.pane_up.matches_direct_key(&key) {
        state.navigate_pane(NavDirection::Up);
        return true;
    }
    if state.keybinds.navigate.pane_right.matches_direct_key(&key) {
        state.navigate_pane(NavDirection::Right);
        return true;
    }

    false
}

fn navigate_reserved_action_for_key(state: &AppState, key: &TerminalKey) -> Option<NavigateAction> {
    if let Some(c) = unmodified_digit_for_key(key) {
        return Some(NavigateAction::SwitchWorkspace(
            (c as usize) - ('1' as usize),
        ));
    }

    let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
    if modifiers.is_empty() {
        match code {
            KeyCode::Enter => {
                return (!state.workspaces.is_empty()).then_some(NavigateAction::SwitchWorkspace(
                    state
                        .visible_workspace_order()
                        .iter()
                        .position(|idx| *idx == state.selected)
                        .unwrap_or(state.selected),
                ));
            }
            KeyCode::Tab => return Some(NavigateAction::CyclePaneNext),
            KeyCode::BackTab => return Some(NavigateAction::CyclePanePrevious),
            KeyCode::Left => return Some(NavigateAction::FocusPaneLeft),
            KeyCode::Right => return Some(NavigateAction::FocusPaneRight),
            _ => {}
        }
    }

    if state.keybinds.navigate.workspace_up.matches_direct_key(key)
        || state
            .keybinds
            .navigate
            .workspace_down
            .matches_direct_key(key)
    {
        return None;
    }
    if state.keybinds.navigate.pane_left.matches_direct_key(key) {
        return Some(NavigateAction::FocusPaneLeft);
    }
    if state.keybinds.navigate.pane_down.matches_direct_key(key) {
        return Some(NavigateAction::FocusPaneDown);
    }
    if state.keybinds.navigate.pane_up.matches_direct_key(key) {
        return Some(NavigateAction::FocusPaneUp);
    }
    if state.keybinds.navigate.pane_right.matches_direct_key(key) {
        return Some(NavigateAction::FocusPaneRight);
    }

    None
}

pub(super) fn api_pane_direction(direction: NavDirection) -> crate::api::schema::PaneDirection {
    match direction {
        NavDirection::Left => crate::api::schema::PaneDirection::Left,
        NavDirection::Right => crate::api::schema::PaneDirection::Right,
        NavDirection::Up => crate::api::schema::PaneDirection::Up,
        NavDirection::Down => crate::api::schema::PaneDirection::Down,
    }
}

#[cfg(test)]
pub(crate) fn handle_navigate_key(state: &mut AppState, key: KeyEvent) {
    let mut terminal_runtimes = TerminalRuntimeRegistry::new();
    state.update_dismissed = true;
    let terminal_key = TerminalKey::from(key);

    if state.is_prefix_key(&terminal_key) || key.code == KeyCode::Esc {
        leave_navigate_mode(state);
        return;
    }

    if handle_navigate_reserved_key(state, terminal_key.clone()) {
        return;
    }

    if let Some(action) = navigate_mode_action_for_key(state, terminal_key) {
        execute_navigate_action_in_context(
            state,
            &mut terminal_runtimes,
            action,
            ActionContext::Navigate,
        );
    }
}

fn next_blocked_window_target(state: &AppState) -> Option<(usize, usize)> {
    let windows = state
        .workspaces
        .iter()
        .enumerate()
        .flat_map(|(ws_idx, ws)| (0..ws.tabs.len()).map(move |tab_idx| (ws_idx, tab_idx)))
        .collect::<Vec<_>>();
    if windows.is_empty() {
        return None;
    }
    let active_ws = state.active?;
    let active_tab = state.workspaces.get(active_ws)?.active_tab_index();
    let current = windows
        .iter()
        .position(|window| *window == (active_ws, active_tab))?;

    (1..=windows.len()).find_map(|offset| {
        let target = windows[(current + offset) % windows.len()];
        let tab = &state.workspaces[target.0].tabs[target.1];
        (tab.aggregate_state(&state.terminals).0 == crate::detect::AgentState::Blocked)
            .then_some(target)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NavigateAction {
    NewWorkspace,
    NewWorktree,
    OpenWorktree,
    RemoveWorktree,
    RenameWorkspace,
    CloseWorkspace,
    SwitchWorkspace(usize),
    SwitchTab(usize),
    FocusAgent(usize),
    WorkspacePicker,
    PreviousWorkspace,
    NextWorkspace,
    PreviousAgent,
    NextAgent,
    NewTab,
    RenameTab,
    ToggleTabPrio,
    TogglePrioPanel,
    ToggleBlockedFilter,
    PreviousTab,
    NextTab,
    PreviousWindow,
    NextWindow,
    NextBlockedWindow,
    CloseTab,
    RenamePane,
    FocusPaneLeft,
    FocusPaneDown,
    FocusPaneUp,
    FocusPaneRight,
    SwapPaneLeft,
    SwapPaneDown,
    SwapPaneUp,
    SwapPaneRight,
    SplitVertical,
    SplitHorizontal,
    SplitLeft,
    SplitUp,
    ClosePane,
    EditScrollback,
    CopyMode,
    Zoom,
    TogglePinTab,
    EnterResizeMode,
    ToggleSidebar,
    ToggleStatusDetail,
    ToggleDock,
    PreviousDockTab,
    NextDockTab,
    OpenInbox,
    OpenHome,
    EditScratchpad,
    ShowScratchpad,
    CyclePaneNext,
    CyclePanePrevious,
    LastPane,
    Help,
    Settings,
    ReloadConfig,
    OpenNotificationTarget,
    OpenWorkUrl,
    CopyWorkUrl,
    OpenWorkLink,
    CopyWorkLink,
    CopyWorkTicket,
    CopyWorkPr,
    CopyWorkPreview,
    ToggleInfoPanel,
    OpenSymphony,
    Detach,
    OpenNavigator,
}

fn copy_mode_survives_prefix_action(action: NavigateAction) -> bool {
    matches!(
        action,
        NavigateAction::SwitchWorkspace(_)
            | NavigateAction::SwitchTab(_)
            | NavigateAction::FocusAgent(_)
            | NavigateAction::PreviousWorkspace
            | NavigateAction::NextWorkspace
            | NavigateAction::PreviousAgent
            | NavigateAction::NextAgent
            | NavigateAction::PreviousTab
            | NavigateAction::NextTab
            | NavigateAction::ToggleTabPrio
            | NavigateAction::PreviousWindow
            | NavigateAction::NextWindow
            | NavigateAction::NextBlockedWindow
            | NavigateAction::FocusPaneLeft
            | NavigateAction::FocusPaneDown
            | NavigateAction::FocusPaneUp
            | NavigateAction::FocusPaneRight
            | NavigateAction::CyclePaneNext
            | NavigateAction::CyclePanePrevious
            | NavigateAction::LastPane
            | NavigateAction::OpenNotificationTarget
    )
}

fn indexed_navigation_action(
    state: &AppState,
    key: &TerminalKey,
    dispatch: BindingDispatch,
) -> Option<NavigateAction> {
    let kb = &state.keybinds;
    let actual_modifiers = crate::config::normalize_key_combo((key.code, key.modifiers)).1;

    for exact_modifiers in [true, false] {
        let trigger_matches = |binding: &crate::config::IndexedKeybind| {
            let dispatch_matches = match dispatch {
                BindingDispatch::Direct => binding.trigger.is_direct(),
                BindingDispatch::Prefix => binding.trigger.is_prefix(),
            };
            let expected_modifiers = crate::config::normalize_key_combo(binding.trigger.combo()).1;
            dispatch_matches && (actual_modifiers == expected_modifiers) == exact_modifiers
        };

        for binding in &kb.switch_tab {
            if trigger_matches(binding) {
                if let Some(idx) = binding.matched_index(key) {
                    return Some(NavigateAction::SwitchTab(idx));
                }
            }
        }
        for binding in &kb.switch_workspace {
            if trigger_matches(binding) {
                if let Some(idx) = binding.matched_index(key) {
                    return Some(NavigateAction::SwitchWorkspace(idx));
                }
            }
        }
        for binding in &kb.focus_agent {
            if trigger_matches(binding) {
                if let Some(idx) = binding.matched_index(key) {
                    return Some(NavigateAction::FocusAgent(idx));
                }
            }
        }
    }

    None
}

fn action_matches(
    bindings: &crate::config::ActionKeybinds,
    key: &TerminalKey,
    dispatch: BindingDispatch,
) -> bool {
    match dispatch {
        BindingDispatch::Direct => bindings.matches_direct_key(key),
        BindingDispatch::Prefix => bindings.matches_prefix_key(key),
    }
}

#[cfg(test)]
fn action_for_key(
    state: &AppState,
    key: TerminalKey,
    dispatch: BindingDispatch,
) -> Option<NavigateAction> {
    non_indexed_action_for_key(state, &key, dispatch)
        .or_else(|| indexed_navigation_action(state, &key, dispatch))
}

fn non_indexed_action_for_key(
    state: &AppState,
    key: &TerminalKey,
    dispatch: BindingDispatch,
) -> Option<NavigateAction> {
    let kb = &state.keybinds;
    for (bindings, action) in [
        (&kb.help, NavigateAction::Help),
        (&kb.settings, NavigateAction::Settings),
        (&kb.workspace_picker, NavigateAction::WorkspacePicker),
        (&kb.new_workspace, NavigateAction::NewWorkspace),
        (&kb.new_worktree, NavigateAction::NewWorktree),
        (&kb.open_worktree, NavigateAction::OpenWorktree),
        (&kb.remove_worktree, NavigateAction::RemoveWorktree),
        (&kb.rename_workspace, NavigateAction::RenameWorkspace),
        (&kb.close_workspace, NavigateAction::CloseWorkspace),
        (&kb.previous_workspace, NavigateAction::PreviousWorkspace),
        (&kb.next_workspace, NavigateAction::NextWorkspace),
        (&kb.previous_agent, NavigateAction::PreviousAgent),
        (&kb.next_agent, NavigateAction::NextAgent),
        (&kb.new_tab, NavigateAction::NewTab),
        (&kb.rename_tab, NavigateAction::RenameTab),
        (&kb.toggle_tab_prio, NavigateAction::ToggleTabPrio),
        (&kb.toggle_prio_panel, NavigateAction::TogglePrioPanel),
        (
            &kb.toggle_blocked_filter,
            NavigateAction::ToggleBlockedFilter,
        ),
        (&kb.previous_tab, NavigateAction::PreviousTab),
        (&kb.next_tab, NavigateAction::NextTab),
        (&kb.previous_window, NavigateAction::PreviousWindow),
        (&kb.next_window, NavigateAction::NextWindow),
        (&kb.next_blocked_window, NavigateAction::NextBlockedWindow),
        (&kb.close_tab, NavigateAction::CloseTab),
        (&kb.rename_pane, NavigateAction::RenamePane),
        (&kb.edit_scrollback, NavigateAction::EditScrollback),
        (&kb.copy_mode, NavigateAction::CopyMode),
        (&kb.focus_pane_left, NavigateAction::FocusPaneLeft),
        (&kb.focus_pane_down, NavigateAction::FocusPaneDown),
        (&kb.focus_pane_up, NavigateAction::FocusPaneUp),
        (&kb.focus_pane_right, NavigateAction::FocusPaneRight),
        (&kb.swap_pane_left, NavigateAction::SwapPaneLeft),
        (&kb.swap_pane_down, NavigateAction::SwapPaneDown),
        (&kb.swap_pane_up, NavigateAction::SwapPaneUp),
        (&kb.swap_pane_right, NavigateAction::SwapPaneRight),
        (&kb.last_pane, NavigateAction::LastPane),
        (&kb.cycle_pane_next, NavigateAction::CyclePaneNext),
        (&kb.cycle_pane_previous, NavigateAction::CyclePanePrevious),
        (&kb.split_vertical, NavigateAction::SplitVertical),
        (&kb.split_horizontal, NavigateAction::SplitHorizontal),
        (&kb.split_left, NavigateAction::SplitLeft),
        (&kb.split_up, NavigateAction::SplitUp),
        (&kb.close_pane, NavigateAction::ClosePane),
        (&kb.zoom, NavigateAction::Zoom),
        (&kb.toggle_pin_tab, NavigateAction::TogglePinTab),
        (&kb.resize_mode, NavigateAction::EnterResizeMode),
        (&kb.toggle_sidebar, NavigateAction::ToggleSidebar),
        (&kb.toggle_status_detail, NavigateAction::ToggleStatusDetail),
        (&kb.toggle_dock, NavigateAction::ToggleDock),
        (&kb.previous_dock_tab, NavigateAction::PreviousDockTab),
        (&kb.next_dock_tab, NavigateAction::NextDockTab),
        (&kb.edit_scratchpad, NavigateAction::EditScratchpad),
        (&kb.show_scratchpad, NavigateAction::ShowScratchpad),
        (&kb.toggle_info_panel, NavigateAction::ToggleInfoPanel),
        (&kb.symphony, NavigateAction::OpenSymphony),
        (&kb.inbox, NavigateAction::OpenInbox),
        (&kb.home, NavigateAction::OpenHome),
        (&kb.reload_config, NavigateAction::ReloadConfig),
        (
            &kb.open_notification_target,
            NavigateAction::OpenNotificationTarget,
        ),
        (&kb.open_work_url, NavigateAction::OpenWorkUrl),
        (&kb.copy_work_url, NavigateAction::CopyWorkUrl),
        (&kb.open_work_link, NavigateAction::OpenWorkLink),
        (&kb.copy_work_link, NavigateAction::CopyWorkLink),
        (&kb.copy_work_ticket, NavigateAction::CopyWorkTicket),
        (&kb.copy_work_pr, NavigateAction::CopyWorkPr),
        (&kb.copy_work_preview, NavigateAction::CopyWorkPreview),
        (&kb.detach, NavigateAction::Detach),
        (&kb.goto, NavigateAction::OpenNavigator),
    ] {
        if action_matches(bindings, key, dispatch) {
            return Some(action);
        }
    }
    None
}

#[cfg(test)]
fn navigate_mode_action_for_key(state: &AppState, key: TerminalKey) -> Option<NavigateAction> {
    let action = action_for_key(state, key, BindingDispatch::Prefix)?;
    if matches!(
        action,
        NavigateAction::FocusPaneLeft
            | NavigateAction::FocusPaneDown
            | NavigateAction::FocusPaneUp
            | NavigateAction::FocusPaneRight
    ) {
        return None;
    }
    Some(action)
}

fn navigate_mode_non_indexed_action_for_key(
    state: &AppState,
    key: &TerminalKey,
) -> Option<NavigateAction> {
    let action = non_indexed_action_for_key(state, key, BindingDispatch::Prefix)?;
    if matches!(
        action,
        NavigateAction::FocusPaneLeft
            | NavigateAction::FocusPaneDown
            | NavigateAction::FocusPaneUp
            | NavigateAction::FocusPaneRight
    ) {
        return None;
    }
    Some(action)
}

fn navigate_mode_indexed_action_for_key(
    state: &AppState,
    key: &TerminalKey,
) -> Option<NavigateAction> {
    indexed_navigation_action(state, key, BindingDispatch::Prefix)
}

#[cfg(test)]
pub(super) fn execute_navigate_action(state: &mut AppState, action: NavigateAction) {
    let mut terminal_runtimes = TerminalRuntimeRegistry::new();
    execute_navigate_action_in_context(
        state,
        &mut terminal_runtimes,
        action,
        ActionContext::Navigate,
    );
}

#[cfg(test)]
pub(super) fn execute_navigate_action_in_context(
    state: &mut AppState,
    terminal_runtimes: &mut TerminalRuntimeRegistry,
    action: NavigateAction,
    context: ActionContext,
) {
    let previous_mode = state.mode;
    match action {
        NavigateAction::NewWorkspace => {
            state.request_new_workspace = true;
            leave_navigate_mode(state);
        }
        NavigateAction::NewWorktree => {
            if let Some(ws_idx) = workspace_action_target(state, context)
                .filter(|idx| workspace_can_start_worktree_action(state, terminal_runtimes, *idx))
            {
                state.request_new_linked_worktree = Some(ws_idx);
                leave_navigate_mode(state);
            }
        }
        NavigateAction::OpenWorktree => {
            if let Some(ws_idx) = workspace_action_target(state, context)
                .filter(|idx| workspace_can_start_worktree_action(state, terminal_runtimes, *idx))
            {
                state.request_open_existing_worktree = Some(ws_idx);
                leave_navigate_mode(state);
            }
        }
        NavigateAction::RemoveWorktree => {
            if let Some(ws_idx) = workspace_action_target(state, context) {
                state.request_remove_linked_worktree = Some(ws_idx);
                leave_navigate_mode(state);
            }
        }
        NavigateAction::RenameWorkspace => {
            if let Some(ws_idx) = workspace_action_target(state, context) {
                super::modal::open_rename_workspace(state, terminal_runtimes, ws_idx);
            }
        }
        NavigateAction::CloseWorkspace => {
            if let Some(ws_idx) = workspace_action_target(state, context) {
                state.selected = ws_idx;
                if state.confirm_close {
                    super::modal::open_confirm_close(state);
                } else {
                    state.close_selected_workspace();
                    leave_navigate_mode(state);
                }
            }
        }
        NavigateAction::SwitchWorkspace(idx) => {
            if let Some(ws_idx) = state.workspace_at_visible_position(idx) {
                state.switch_workspace(ws_idx);
                leave_navigate_mode(state);
            }
        }
        NavigateAction::SwitchTab(idx) => {
            let tab_exists = state
                .active
                .and_then(|ws_idx| state.workspaces.get(ws_idx))
                .is_some_and(|ws| idx < ws.tabs.len());
            if tab_exists {
                state.switch_tab(idx);
                leave_navigate_mode(state);
            }
        }
        NavigateAction::FocusAgent(idx) => {
            if state.focus_agent_entry(idx) {
                leave_navigate_mode(state);
            }
        }
        NavigateAction::WorkspacePicker => {
            state.mobile_switcher_scroll = 0;
            state.mode = Mode::Navigate;
        }
        NavigateAction::PreviousWorkspace => {
            state.previous_workspace();
            leave_navigate_mode(state);
        }
        NavigateAction::NextWorkspace => {
            state.next_workspace();
            leave_navigate_mode(state);
        }
        NavigateAction::PreviousAgent => {
            state.previous_agent();
            leave_navigate_mode(state);
        }
        NavigateAction::NextAgent => {
            state.next_agent();
            leave_navigate_mode(state);
        }
        NavigateAction::NewTab => {
            if state.active.is_some() {
                if state.prompt_new_tab_name {
                    super::modal::open_new_tab_dialog(state);
                } else {
                    state.request_new_tab = true;
                    leave_navigate_mode(state);
                }
            }
        }
        NavigateAction::RenameTab => super::modal::open_rename_active_tab(state, false),
        NavigateAction::ToggleTabPrio => {
            if toggle_tab_prio(state, context) {
                state.mark_session_dirty();
                if context == ActionContext::Navigate {
                    leave_navigate_mode(state);
                }
            }
        }
        NavigateAction::TogglePrioPanel => {
            state.toggle_prio_panel();
            state.mark_session_dirty();
            if context == ActionContext::Navigate {
                leave_navigate_mode(state);
            }
        }
        NavigateAction::ToggleBlockedFilter => {
            state.blocked_filter = !state.blocked_filter;
            state.workspace_scroll = crate::ui::normalized_workspace_scroll(
                state,
                state.view.sidebar_rect,
                state.workspace_scroll,
            );
            if context == ActionContext::Navigate {
                leave_navigate_mode(state);
            }
        }
        NavigateAction::PreviousTab => {
            state.previous_tab();
            leave_navigate_mode(state);
        }
        NavigateAction::NextTab => {
            state.next_tab();
            leave_navigate_mode(state);
        }
        NavigateAction::PreviousWindow | NavigateAction::NextWindow => {
            let windows = state
                .workspaces
                .iter()
                .enumerate()
                .flat_map(|(ws_idx, ws)| (0..ws.tabs.len()).map(move |tab_idx| (ws_idx, tab_idx)))
                .collect::<Vec<_>>();
            if let Some(active_ws) = state.active {
                let active_tab = state.workspaces[active_ws].active_tab_index();
                if let Some(current) = windows
                    .iter()
                    .position(|window| *window == (active_ws, active_tab))
                {
                    let forward = matches!(action, NavigateAction::NextWindow);
                    if let Some(next) =
                        crate::workspace::relative_window_index(windows.len(), current, forward)
                    {
                        let (ws_idx, tab_idx) = windows[next];
                        state.switch_workspace(ws_idx);
                        state.switch_tab(tab_idx);
                    }
                }
            }
            leave_navigate_mode(state);
        }
        NavigateAction::NextBlockedWindow => {
            if let Some((ws_idx, tab_idx)) = next_blocked_window_target(state) {
                state.switch_workspace(ws_idx);
                state.switch_tab(tab_idx);
            }
            leave_navigate_mode(state);
        }
        NavigateAction::CloseTab => {
            if !state.close_tab() {
                leave_navigate_mode(state);
            }
        }
        NavigateAction::RenamePane => {
            if let Some(pane_id) = state
                .active
                .and_then(|ws_idx| state.workspaces.get(ws_idx))
                .and_then(|ws| ws.focused_pane_id())
            {
                super::modal::open_rename_pane(state, pane_id);
            }
        }
        NavigateAction::FocusPaneLeft => state.navigate_pane(NavDirection::Left),
        NavigateAction::FocusPaneDown => state.navigate_pane(NavDirection::Down),
        NavigateAction::FocusPaneUp => state.navigate_pane(NavDirection::Up),
        NavigateAction::FocusPaneRight => state.navigate_pane(NavDirection::Right),
        NavigateAction::SwapPaneLeft => {
            state.swap_pane(NavDirection::Left);
            leave_navigate_mode(state);
        }
        NavigateAction::SwapPaneDown => {
            state.swap_pane(NavDirection::Down);
            leave_navigate_mode(state);
        }
        NavigateAction::SwapPaneUp => {
            state.swap_pane(NavDirection::Up);
            leave_navigate_mode(state);
        }
        NavigateAction::SwapPaneRight => {
            state.swap_pane(NavDirection::Right);
            leave_navigate_mode(state);
        }
        NavigateAction::SplitVertical => {
            state.split_pane(terminal_runtimes, Direction::Horizontal);
            leave_navigate_mode(state);
        }
        NavigateAction::SplitHorizontal => {
            state.split_pane(terminal_runtimes, Direction::Vertical);
            leave_navigate_mode(state);
        }
        NavigateAction::SplitLeft => {
            state.split_pane_with_placement(terminal_runtimes, Direction::Horizontal, true);
            leave_navigate_mode(state);
        }
        NavigateAction::SplitUp => {
            state.split_pane_with_placement(terminal_runtimes, Direction::Vertical, true);
            leave_navigate_mode(state);
        }
        NavigateAction::ClosePane => {
            if !state.close_pane() {
                leave_navigate_mode(state);
            }
        }
        NavigateAction::EditScrollback => {}
        NavigateAction::CopyMode => state.enter_copy_mode(terminal_runtimes),
        NavigateAction::Zoom => {
            state.toggle_zoom();
            leave_navigate_mode(state);
        }
        // Headless/test dispatch has no API client, so it flips the flag it
        // would otherwise have asked the server to flip.
        NavigateAction::TogglePinTab => {
            state.toggle_pin_active_tab();
            leave_navigate_mode(state);
        }
        NavigateAction::EnterResizeMode => state.mode = Mode::Resize,
        NavigateAction::ToggleSidebar => {
            state.sidebar_collapsed = !state.sidebar_collapsed;
            leave_navigate_mode(state);
        }
        NavigateAction::ToggleStatusDetail => {
            state.status_bar_expanded = !state.status_bar_expanded;
            leave_navigate_mode(state);
        }
        NavigateAction::ToggleDock => {
            state.dock_collapsed = !state.dock_collapsed;
            leave_navigate_mode(state);
        }
        NavigateAction::PreviousDockTab => {
            state.dock_tab = state.dock_tab.previous();
            if state.dock_tab == crate::app::DockTab::Editor {
                state.dock_editor_focused = true;
            }
            leave_navigate_mode(state);
        }
        NavigateAction::NextDockTab => {
            state.dock_tab = state.dock_tab.next();
            if state.dock_tab == crate::app::DockTab::Editor {
                state.dock_editor_focused = true;
            }
            leave_navigate_mode(state);
        }
        // Spawning the editor needs an `App`; the state-only mirror cannot do it.
        NavigateAction::EditScratchpad => leave_navigate_mode(state),
        NavigateAction::ShowScratchpad => {
            state.show_scratchpad_tab();
            leave_navigate_mode(state);
        }
        NavigateAction::CyclePaneNext => {
            state.cycle_pane(false);
            leave_navigate_mode(state);
        }
        NavigateAction::CyclePanePrevious => {
            state.cycle_pane(true);
            leave_navigate_mode(state);
        }
        NavigateAction::LastPane => {
            state.last_pane();
            leave_navigate_mode(state);
        }
        NavigateAction::Help => super::modal::open_keybind_help(state),
        NavigateAction::Settings => super::settings::open_settings(state),
        NavigateAction::ReloadConfig => {
            state.request_reload_config = true;
            leave_navigate_mode(state);
        }
        NavigateAction::OpenNotificationTarget => {
            state.focus_toast_target();
            if state.mode == Mode::Navigate {
                leave_navigate_mode(state);
            }
        }
        NavigateAction::OpenWorkUrl
        | NavigateAction::CopyWorkUrl
        | NavigateAction::OpenWorkLink
        | NavigateAction::CopyWorkLink
        | NavigateAction::CopyWorkTicket
        | NavigateAction::CopyWorkPr
        | NavigateAction::CopyWorkPreview => {
            leave_navigate_mode(state);
        }
        NavigateAction::ToggleInfoPanel => {
            state.info_panel_expanded = !state.info_panel_expanded;
            leave_navigate_mode(state);
        }
        NavigateAction::OpenSymphony => {
            state.toggle_symphony();
            leave_navigate_mode(state);
        }
        NavigateAction::OpenInbox => {
            state.toggle_inbox();
            leave_navigate_mode(state);
        }
        NavigateAction::OpenHome => {
            state.toggle_home();
            leave_navigate_mode(state);
        }
        NavigateAction::Detach => {
            super::modal::request_detach(state);
            leave_navigate_mode(state);
        }
        NavigateAction::OpenNavigator => state.open_navigator_from(terminal_runtimes),
    }

    finish_action_context(state, context, previous_mode);
}

fn workspace_action_target(state: &AppState, context: ActionContext) -> Option<usize> {
    let idx = match context {
        ActionContext::Direct | ActionContext::Prefix => state.active.unwrap_or(state.selected),
        ActionContext::Navigate => state.selected,
    };
    (idx < state.workspaces.len()).then_some(idx)
}

fn toggle_tab_prio(state: &mut AppState, context: ActionContext) -> bool {
    let Some(ws_idx) = workspace_action_target(state, context) else {
        return false;
    };
    let Some(tab_idx) = state
        .workspaces
        .get(ws_idx)
        .map(crate::workspace::Workspace::active_tab_index)
    else {
        return false;
    };
    state
        .apply_tab_prio(ws_idx, tab_idx, crate::workspace::TabPrioAction::Toggle)
        .is_some()
}

#[cfg(test)]
fn focused_work_url(state: &AppState) -> Option<String> {
    focused_work_context(state)?.primary_action_url()
}

fn focused_work_context(state: &AppState) -> Option<&crate::work_context::PaneWorkContext> {
    let workspace = state
        .active
        .and_then(|ws_idx| state.workspaces.get(ws_idx))?;
    let pane_id = workspace.focused_pane_id()?;
    let terminal_id = workspace.terminal_id(pane_id)?;
    Some(state.terminals.get(terminal_id)?.effective_work_context())
}

fn workspace_can_start_worktree_action(
    state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    ws_idx: usize,
) -> bool {
    let Some(ws) = state.workspaces.get(ws_idx) else {
        return false;
    };
    if ws
        .worktree_space()
        .is_some_and(|space| space.is_linked_worktree)
    {
        return false;
    }
    let git_space = ws.git_space().cloned().or_else(|| {
        ws.resolved_identity_cwd_from(&state.terminals, terminal_runtimes)
            .as_deref()
            .and_then(crate::workspace::git_space_metadata)
    });
    !git_space.is_some_and(|space| space.is_linked_worktree)
}

fn leave_navigate_mode(state: &mut AppState) {
    state.end_workspace_picker_presentation();
    if state.active.is_some() {
        state.mode = Mode::Terminal;
    }
}

fn finish_action_context(state: &mut AppState, context: ActionContext, previous_mode: Mode) {
    if matches!(context, ActionContext::Direct | ActionContext::Prefix)
        && state.mode == previous_mode
    {
        leave_command_mode(state);
    }
}

fn finish_custom_command_context(
    state: &mut AppState,
    context: ActionContext,
    previous_mode: Mode,
) {
    if context == ActionContext::Navigate {
        leave_navigate_mode(state);
    } else {
        finish_action_context(state, context, previous_mode);
    }
}

fn leave_command_mode(state: &mut AppState) {
    if state.copy_mode_pane_is_focused() {
        state.mode = Mode::Copy;
    } else if state.active.is_some() {
        state.mode = Mode::Terminal;
    } else {
        state.mode = Mode::Navigate;
    };
}

fn write_scrollback_temp_file(content: &str) -> io::Result<std::path::PathBuf> {
    let mut last_collision = None;
    for attempt in 0..16 {
        let path = unique_scrollback_path(attempt);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        match options.open(&path) {
            Ok(mut file) => {
                file.write_all(content.as_bytes())?;
                return Ok(path);
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                last_collision = Some(err);
            }
            Err(err) => return Err(err),
        }
    }

    Err(last_collision.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "failed to create unique scrollback temp file",
        )
    }))
}

fn unique_scrollback_path(attempt: u32) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "herdr-scrollback-{}-{nanos}-{attempt}.txt",
        std::process::id()
    ))
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::time::Duration;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, ModifierKeyCode};
    use ratatui::layout::Direction;

    #[cfg(unix)]
    use super::super::wait_for_file;
    use super::super::{state_with_workspaces, unique_temp_path};
    use super::*;
    use crate::{
        app::App, config::Config, input::TerminalKey, terminal::TerminalState, workspace::Workspace,
    };

    fn mark_worktree_space_member(state: &mut AppState, ws_idx: usize, key: &str) {
        state.workspaces[ws_idx].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: key.into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: format!("/repo/worktree-{ws_idx}").into(),
            is_linked_worktree: ws_idx != 0,
        });
    }

    fn app_with_test_workspaces(names: &[&str]) -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = names.iter().map(|name| Workspace::test_new(name)).collect();
        app.state.ensure_test_terminals();
        app.state.active = (!app.state.workspaces.is_empty()).then_some(0);
        app.state.selected = 0;
        app
    }

    fn app_with_global_window_fixture() -> App {
        let mut app = app_with_test_workspaces(&["first", "second"]);
        app.state.workspaces[0].test_add_tab(Some("first-agentless"));
        app.state.workspaces[1].test_add_tab(Some("second-agentless"));
        app.state.ensure_test_terminals();
        app
    }

    fn active_window(state: &AppState) -> (usize, usize) {
        let workspace = state.active.expect("active workspace");
        (workspace, state.workspaces[workspace].active_tab_index())
    }

    fn assert_tui_window_cycle(app: &mut App, action: NavigateAction, expected: &[(usize, usize)]) {
        for expected_window in expected {
            app.execute_tui_navigate_action(action, ActionContext::Prefix);
            assert_eq!(active_window(&app.state), *expected_window);
        }
    }

    fn assert_headless_window_cycle(
        state: &mut AppState,
        action: NavigateAction,
        expected: &[(usize, usize)],
    ) {
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        for expected_window in expected {
            execute_navigate_action_in_context(
                state,
                &mut terminal_runtimes,
                action,
                ActionContext::Prefix,
            );
            assert_eq!(active_window(state), *expected_window);
        }
    }

    #[test]
    fn global_window_cycle_includes_every_tab() {
        let forward = [(0, 1), (1, 0), (1, 1), (0, 0)];
        let backward = [(1, 1), (1, 0), (0, 1), (0, 0)];

        let mut app = app_with_global_window_fixture();
        assert_tui_window_cycle(&mut app, NavigateAction::NextWindow, &forward);
        assert_tui_window_cycle(&mut app, NavigateAction::PreviousWindow, &backward);

        let mut state = app_with_global_window_fixture().state;
        assert_headless_window_cycle(&mut state, NavigateAction::NextWindow, &forward);
        assert_headless_window_cycle(&mut state, NavigateAction::PreviousWindow, &backward);
    }

    #[test]
    fn global_window_cycle_ignores_presentation_state() {
        let mut app = app_with_global_window_fixture();
        mark_worktree_space_member(&mut app.state, 0, "repo-key");
        mark_worktree_space_member(&mut app.state, 1, "repo-key");
        app.state.sidebar_collapsed = true;
        app.state.collapsed_space_keys.insert("repo-key".into());
        app.state.collapsed_sidebar_groups.insert("agents".into());
        app.state.workspaces[0].identity_cwd = "/changed/first".into();
        app.state.workspaces[1].identity_cwd = "/changed/second".into();
        for terminal in app.state.terminals.values_mut() {
            terminal.cwd = "/changed/terminal".into();
            terminal.set_detected_state(
                Some(crate::detect::Agent::Claude),
                crate::detect::AgentState::Blocked,
            );
        }

        assert_tui_window_cycle(
            &mut app,
            NavigateAction::NextWindow,
            &[(0, 1), (1, 0), (1, 1), (0, 0)],
        );

        let mut state = app.state;
        assert_headless_window_cycle(
            &mut state,
            NavigateAction::PreviousWindow,
            &[(1, 1), (1, 0), (0, 1), (0, 0)],
        );
    }

    fn set_tab_agent_state(
        state: &mut AppState,
        workspace: usize,
        tab: usize,
        agent_state: crate::detect::AgentState,
    ) {
        let root_pane = state.workspaces[workspace].tabs[tab].root_pane;
        set_pane_agent_state(state, workspace, tab, root_pane, agent_state);
    }

    fn set_pane_agent_state(
        state: &mut AppState,
        workspace: usize,
        tab: usize,
        pane: crate::layout::PaneId,
        agent_state: crate::detect::AgentState,
    ) {
        let terminal_id = state.workspaces[workspace].tabs[tab].panes[&pane]
            .attached_terminal_id
            .clone();
        state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .set_detected_state(Some(crate::detect::Agent::Claude), agent_state);
    }

    fn app_with_blocked_window_fixture() -> App {
        let mut app = app_with_global_window_fixture();
        let blocked_child = app.state.workspaces[1].test_split(Direction::Horizontal);
        app.state.ensure_test_terminals();
        set_tab_agent_state(&mut app.state, 0, 1, crate::detect::AgentState::Blocked);
        set_tab_agent_state(&mut app.state, 1, 0, crate::detect::AgentState::Working);
        set_pane_agent_state(
            &mut app.state,
            1,
            0,
            blocked_child,
            crate::detect::AgentState::Blocked,
        );
        app
    }

    #[test]
    fn next_blocked_window_cycles_canonical_order() {
        let mut app = app_with_blocked_window_fixture();

        assert_tui_window_cycle(
            &mut app,
            NavigateAction::NextBlockedWindow,
            &[(0, 1), (1, 0), (0, 1)],
        );

        let mut state = app_with_blocked_window_fixture().state;
        assert_headless_window_cycle(
            &mut state,
            NavigateAction::NextBlockedWindow,
            &[(0, 1), (1, 0), (0, 1)],
        );
    }

    #[test]
    fn next_blocked_window_default_binding_dispatches_action() {
        let state = state_with_workspaces(&["test"]);

        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('b'), KeyModifiers::empty()),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::NextBlockedWindow)
        );
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('b'), KeyModifiers::SHIFT),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::ToggleSidebar)
        );
    }

    #[test]
    fn next_blocked_window_handles_no_match_and_nonblocked_current() {
        let mut app = app_with_global_window_fixture();
        app.state.switch_tab(1);
        app.execute_tui_navigate_action(NavigateAction::NextBlockedWindow, ActionContext::Prefix);
        assert_eq!(active_window(&app.state), (0, 1));

        set_tab_agent_state(&mut app.state, 0, 0, crate::detect::AgentState::Blocked);
        set_tab_agent_state(&mut app.state, 1, 1, crate::detect::AgentState::Blocked);
        assert_tui_window_cycle(
            &mut app,
            NavigateAction::NextBlockedWindow,
            &[(1, 1), (0, 0)],
        );

        let mut state = app_with_global_window_fixture().state;
        state.switch_tab(1);
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::NextBlockedWindow,
            ActionContext::Prefix,
        );
        assert_eq!(active_window(&state), (0, 1));

        set_tab_agent_state(&mut state, 0, 0, crate::detect::AgentState::Blocked);
        set_tab_agent_state(&mut state, 1, 1, crate::detect::AgentState::Blocked);
        assert_headless_window_cycle(
            &mut state,
            NavigateAction::NextBlockedWindow,
            &[(1, 1), (0, 0)],
        );
    }

    fn temporary_checkout(name: &str, origin: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "herdr-symphony-{name}-{}-{unique}",
            std::process::id()
        ));
        let checkout = root.join(name);
        std::fs::create_dir_all(&checkout).expect("create test checkout");
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&checkout)
            .status()
            .expect("run git init");
        assert!(status.success());
        let status = std::process::Command::new("git")
            .args(["remote", "add", "origin", origin])
            .current_dir(&checkout)
            .status()
            .expect("add git origin");
        assert!(status.success());
        checkout
    }

    fn point_workspace_at(app: &mut App, workspace: usize, cwd: &std::path::Path) {
        let root_pane = app.state.workspaces[workspace].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[workspace].tabs[0]
            .terminal_id(root_pane)
            .expect("root terminal")
            .clone();
        app.state.workspaces[workspace].identity_cwd = cwd.to_path_buf();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .cwd = cwd.to_path_buf();
    }

    fn set_symphony_workflow(app: &mut App, repo: &str) {
        app.state.symphony_detail = Some(crate::app::state::SymphonyDetail {
            snapshot: crate::symphony::Snapshot {
                workflows: vec![crate::symphony::Workflow {
                    workflow_id: "symphony-MAT-138".to_string(),
                    run_id: "run".to_string(),
                    name: "Temporal blocker dashboard".to_string(),
                    phase: "runFlowStep".to_string(),
                    wait: Some("plan-sign-off".to_string()),
                    started_at: None,
                    ticket: Some("MAT-138".to_string()),
                    repo: Some(repo.to_string()),
                    pr: Some("https://github.com/matthias-scale/herdr/pull/1".to_string()),
                    receipts: Some("/receipts".to_string()),
                }],
                unavailable: None,
            },
            selected: 0,
            observed_at: std::time::SystemTime::now(),
        });
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prefix_pass_through_retires_blocked_hook_authority_after_forwarding() {
        let mut app = app_with_test_workspaces(&["test"]);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(
            Some(crate::detect::Agent::Codex),
            crate::detect::AgentState::Idle,
        );
        terminal.set_hook_authority(
            "herdr:codex-closing-block".into(),
            "codex".into(),
            crate::detect::AgentState::Blocked,
            None,
            Some(1),
        );
        app.state.mode = Mode::Prefix;

        app.handle_prefix_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ));

        assert!(rx.try_recv().is_ok());
        assert_eq!(app.state.mode, Mode::Terminal);
        assert_eq!(
            app.state.terminals[&terminal_id].state,
            crate::detect::AgentState::Idle
        );
        assert!(!app.state.terminals[&terminal_id].full_lifecycle_hook_authority_active());
    }

    #[test]
    fn prefix_ctrl_h_opens_and_escape_closes_run_history_detail() {
        let mut app = app_with_test_workspaces(&["test"]);
        app.state.mode = Mode::Prefix;

        app.handle_prefix_key(TerminalKey::new(KeyCode::Char('h'), KeyModifiers::CONTROL));

        assert!(app.state.loop_run_history_detail.is_some());

        assert!(
            app.handle_loop_run_history_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty(),))
        );

        assert!(app.state.loop_run_history_detail.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn symphony_enter_opens_interactive_tab_in_matching_checkout() {
        let mut app = app_with_test_workspaces(&["test"]);
        app.state.default_shell = crate::app::api::test_support::exiting_test_command().into();
        let cwd = temporary_checkout(
            "mat-138-symphony-service",
            "git@github.com:owner-a/mat-138-symphony-service.git",
        );
        point_workspace_at(&mut app, 0, &cwd);
        set_symphony_workflow(&mut app, "owner-a/mat-138-symphony-service");

        assert!(app.handle_symphony_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty(),)));

        assert_eq!(app.state.workspaces[0].tabs.len(), 2);
        assert!(app.state.symphony_detail.is_none());
        let created = &app.state.workspaces[0].tabs[1];
        let terminal_id = created.terminal_id(created.root_pane).unwrap();
        assert_eq!(app.state.terminals[terminal_id].cwd, cwd);
        crate::app::api::test_support::shutdown_test_runtimes(&mut app);
        std::fs::remove_dir_all(cwd.parent().expect("checkout parent"))
            .expect("remove test checkout");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn symphony_enter_accepts_matching_origin_with_custom_checkout_basename() {
        let mut app = app_with_test_workspaces(&["test"]);
        app.state.default_shell = crate::app::api::test_support::exiting_test_command().into();
        let cwd = temporary_checkout("custom-worktree-path", "git@github.com:owner-a/service.git");
        point_workspace_at(&mut app, 0, &cwd);
        set_symphony_workflow(&mut app, "owner-a/service");

        assert!(app.handle_symphony_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty(),)));

        assert_eq!(app.state.workspaces[0].tabs.len(), 2);
        assert!(app.state.symphony_detail.is_none());
        let created = &app.state.workspaces[0].tabs[1];
        let terminal_id = created.terminal_id(created.root_pane).unwrap();
        assert_eq!(app.state.terminals[terminal_id].cwd, cwd);
        crate::app::api::test_support::shutdown_test_runtimes(&mut app);
        std::fs::remove_dir_all(cwd.parent().expect("checkout parent"))
            .expect("remove test checkout");
    }

    #[test]
    fn symphony_enter_rejects_same_basename_with_different_owner() {
        let mut app = app_with_test_workspaces(&["test"]);
        let cwd = temporary_checkout(
            "mat-138-symphony-collision",
            "git@github.com:owner-b/mat-138-symphony-collision.git",
        );
        point_workspace_at(&mut app, 0, &cwd);
        set_symphony_workflow(&mut app, "owner-a/mat-138-symphony-collision");

        assert!(app.handle_symphony_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty(),)));

        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert!(app.state.symphony_detail.is_some());
        assert_eq!(
            app.state.config_diagnostic.as_deref(),
            Some("Symphony checkout origin mismatch for owner-a/mat-138-symphony-collision")
        );
        std::fs::remove_dir_all(cwd.parent().expect("checkout parent"))
            .expect("remove test checkout");
    }

    #[test]
    fn symphony_enter_rejects_hostile_repository_name() {
        let mut app = app_with_test_workspaces(&["test"]);
        set_symphony_workflow(&mut app, "..");

        assert!(app.handle_symphony_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty(),)));

        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert!(app.state.symphony_detail.is_some());
        assert_eq!(
            app.state.config_diagnostic.as_deref(),
            Some("Invalid Symphony repository name: ..")
        );
    }

    #[test]
    fn toggle_tab_prio_flips_flag_and_marks_session_dirty() {
        let mut app = app_with_test_workspaces(&["one"]);
        app.no_session = false;
        app.state.session_dirty = false;
        app.state.session_dirty_revision = 0;

        app.execute_tui_navigate_action(NavigateAction::ToggleTabPrio, ActionContext::Direct);
        assert!(app.state.workspaces[0].tabs[0].prio);
        assert!(app.state.session_dirty);
        assert_eq!(app.state.session_dirty_revision, 1);

        app.execute_tui_navigate_action(NavigateAction::ToggleTabPrio, ActionContext::Direct);
        assert!(!app.state.workspaces[0].tabs[0].prio);
        assert_eq!(app.state.session_dirty_revision, 2);
    }

    fn add_multiple_work_links(app: &mut App) {
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .unwrap();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                ticket_ids: Some(vec!["MAT-1".into()]),
                pr_urls: Some(vec!["https://github.com/o/r/pull/2".into()]),
                ..Default::default()
            })
            .unwrap();
    }

    #[test]
    fn dock_key_actions_cycle_tabs_and_toggle_the_dock() {
        let mut state = app_with_test_workspaces(&["one"]).state;
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        state.mode = Mode::Prefix;

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::NextDockTab,
            ActionContext::Prefix,
        );
        assert_eq!(state.dock_tab, crate::app::DockTab::Shortcuts);

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::PreviousDockTab,
            ActionContext::Prefix,
        );
        assert_eq!(state.dock_tab, crate::app::DockTab::Editor);

        state.dock_collapsed = true;
        state.session_dirty = false;
        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::ToggleDock,
            ActionContext::Prefix,
        );
        assert!(!state.dock_collapsed);
        assert!(
            !state.session_dirty,
            "client-local dock presentation must not dirty shared session state"
        );
    }

    #[test]
    fn ac4_default_work_link_keybindings_map_to_distinct_prefix_actions() {
        let state = app_with_test_workspaces(&["one"]).state;
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('e'), KeyModifiers::SHIFT),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::ToggleDock)
        );
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('['), KeyModifiers::SHIFT),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::PreviousDockTab)
        );
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char(']'), KeyModifiers::SHIFT),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::NextDockTab)
        );
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('u'), KeyModifiers::empty()),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::OpenWorkUrl)
        );
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('U'), KeyModifiers::SHIFT),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::CopyWorkUrl)
        );
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::CopyWorkTicket)
        );
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('u'), KeyModifiers::ALT),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::CopyWorkPr)
        );
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(
                    KeyCode::Char('u'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                ),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::CopyWorkPreview)
        );
        assert_eq!(
            action_for_key(
                &state,
                TerminalKey::new(KeyCode::Char('i'), KeyModifiers::empty()),
                BindingDispatch::Prefix,
            ),
            Some(NavigateAction::ToggleInfoPanel)
        );
    }

    #[test]
    fn ac4_work_link_resolver_and_clipboard_use_only_active_focused_pane() {
        let mut app = app_with_test_workspaces(&["active", "selected"]);
        app.state.selected = 1;
        let focused = app.state.workspaces[0].test_split(Direction::Horizontal);
        app.state.ensure_test_terminals();
        app.state.workspaces[0].tabs[0].layout.focus_pane(focused);

        let root = app.state.workspaces[0].tabs[0].root_pane;
        let selected = app.state.workspaces[1].tabs[0].root_pane;
        for (ws_idx, pane, ticket, pr) in [
            (
                0,
                root,
                None,
                Some("https://github.com/ogulcancelik/herdr/pull/1"),
            ),
            (
                0,
                focused,
                Some("SCA-42"),
                Some("https://github.com/ogulcancelik/herdr/pull/4"),
            ),
            (1, selected, Some("SCA-999"), None),
        ] {
            let terminal_id = app.state.workspaces[ws_idx]
                .terminal_id(pane)
                .cloned()
                .unwrap();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                    ticket_ids: ticket.map(|ticket| vec![ticket.into()]),
                    pr_urls: pr.map(|pr| vec![pr.into()]),
                    ..Default::default()
                })
                .unwrap();
        }

        assert_eq!(
            focused_work_url(&app.state).as_deref(),
            Some("https://linear.app/scalable/issue/SCA-42")
        );
        app.execute_tui_navigate_action(NavigateAction::CopyWorkUrl, ActionContext::Prefix);
        assert_eq!(app.state.mode, Mode::WorkLinkPicker);
        app.handle_work_link_picker_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::empty()));
        match app.event_rx.try_recv().expect("clipboard event") {
            crate::events::AppEvent::ClipboardWrite { content } => {
                assert_eq!(content, b"https://linear.app/scalable/issue/SCA-42")
            }
            event => panic!("unexpected event: {event:?}"),
        }
    }

    #[test]
    fn ac4_missing_work_context_is_a_nonfatal_notice_without_clipboard_event() {
        let mut app = app_with_test_workspaces(&["one"]);
        app.execute_tui_navigate_action(NavigateAction::CopyWorkUrl, ActionContext::Prefix);

        assert!(app.event_rx.try_recv().is_err());
        assert_eq!(
            app.state
                .copy_feedback
                .as_ref()
                .map(|feedback| feedback.message.as_str()),
            Some("focused pane has no work link")
        );
    }

    #[test]
    fn ac25_granular_work_copy_actions_use_effective_focused_context() {
        let mut app = app_with_test_workspaces(&["one"]);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .unwrap();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                ticket_ids: Some(vec!["mat-231".into()]),
                pr_urls: Some(vec!["https://github.com/o/r/pull/231".into()]),
                ..Default::default()
            })
            .unwrap();
        terminal
            .replace_hook_work_context(crate::work_context::PaneWorkContext {
                preview_urls: vec!["https://preview-231.vercel.app".into()],
                ..Default::default()
            })
            .unwrap();

        for (action, expected) in [
            (NavigateAction::CopyWorkTicket, b"MAT-231".as_slice()),
            (
                NavigateAction::CopyWorkPr,
                b"https://github.com/o/r/pull/231".as_slice(),
            ),
            (
                NavigateAction::CopyWorkPreview,
                b"https://preview-231.vercel.app".as_slice(),
            ),
        ] {
            app.execute_tui_navigate_action(action, ActionContext::Prefix);
            match app.event_rx.try_recv().expect("clipboard event") {
                crate::events::AppEvent::ClipboardWrite { content } => {
                    assert_eq!(content, expected)
                }
                event => panic!("unexpected event: {event:?}"),
            }
        }
    }

    #[test]
    fn ac25_copy_work_preview_hook_tier_precedes_restored_manual_and_git_tiers() {
        let mut app = app_with_test_workspaces(&["one"]);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .unwrap();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .restore_work_context_with_tiers(
                crate::work_context::PaneWorkContext::default(),
                Some(crate::work_context::PaneWorkContextTiers {
                    manual: crate::work_context::PaneWorkContext {
                        preview_urls: vec!["https://manual.vercel.app".into()],
                        ..Default::default()
                    },
                    hook_turn: crate::work_context::PaneWorkContext {
                        preview_urls: vec!["https://hook.vercel.app".into()],
                        ..Default::default()
                    },
                    git_observation: crate::work_context::PaneWorkContext {
                        preview_urls: vec!["https://git.vercel.app".into()],
                        ..Default::default()
                    },
                    restored_fallback: crate::work_context::PaneWorkContext {
                        preview_urls: vec!["https://fallback.vercel.app".into()],
                        ..Default::default()
                    },
                }),
            )
            .unwrap();

        app.execute_tui_navigate_action(NavigateAction::CopyWorkPreview, ActionContext::Prefix);
        match app.event_rx.try_recv().expect("clipboard event") {
            crate::events::AppEvent::ClipboardWrite { content } => {
                assert_eq!(content, b"https://hook.vercel.app")
            }
            event => panic!("unexpected event: {event:?}"),
        }
    }

    #[test]
    fn ac25_granular_work_copy_actions_are_safe_noops_when_missing() {
        for (action, notice) in [
            (
                NavigateAction::CopyWorkTicket,
                "focused pane has no work ticket",
            ),
            (
                NavigateAction::CopyWorkPr,
                "focused pane has no pull request",
            ),
            (
                NavigateAction::CopyWorkPreview,
                "focused pane has no preview URL",
            ),
        ] {
            let mut app = app_with_test_workspaces(&["one"]);
            app.execute_tui_navigate_action(action, ActionContext::Prefix);
            assert!(app.event_rx.try_recv().is_err());
            assert_eq!(
                app.state
                    .copy_feedback
                    .as_ref()
                    .map(|feedback| feedback.message.as_str()),
                Some(notice)
            );
        }
    }

    #[test]
    fn ac26_link_picker_inherits_copy_action_and_selects_snapshot_entry() {
        let mut app = app_with_test_workspaces(&["one"]);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .unwrap();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                ticket_ids: Some(vec!["MAT-1".into()]),
                pr_urls: Some(vec!["https://github.com/o/r/pull/2".into()]),
                ..Default::default()
            })
            .unwrap();

        app.execute_tui_navigate_action(NavigateAction::CopyWorkLink, ActionContext::Prefix);
        assert_eq!(app.state.mode, Mode::WorkLinkPicker);
        assert_eq!(
            app.state.work_link_picker.as_ref().unwrap().action,
            crate::app::state::WorkLinkPickerAction::Copy
        );

        app.handle_work_link_picker_key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::empty()));
        match app.event_rx.try_recv().expect("picker clipboard event") {
            crate::events::AppEvent::ClipboardWrite { content } => {
                assert_eq!(content, b"https://github.com/o/r/pull/2")
            }
            event => panic!("unexpected event: {event:?}"),
        }
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[test]
    fn ac26_legacy_prefix_binding_dispatches_to_picker_action() {
        let mut app = app_with_test_workspaces(&["one"]);
        add_multiple_work_links(&mut app);
        app.state.mode = Mode::Prefix;
        app.handle_prefix_key(TerminalKey::new(KeyCode::Char('U'), KeyModifiers::SHIFT));

        assert_eq!(app.state.mode, Mode::WorkLinkPicker);
        assert_eq!(
            app.state.work_link_picker.as_ref().unwrap().action,
            crate::app::state::WorkLinkPickerAction::Copy
        );
    }

    #[test]
    fn ac26_navigate_mode_work_link_aliases_open_picker_and_escape_restores_mode() {
        let mut app = app_with_test_workspaces(&["one"]);
        add_multiple_work_links(&mut app);
        app.state.mode = Mode::Navigate;

        app.handle_navigate_key(TerminalKey::new(KeyCode::Char('u'), KeyModifiers::empty()));
        assert_eq!(app.state.mode, Mode::WorkLinkPicker);
        assert_eq!(
            app.state.work_link_picker.as_ref().unwrap().action,
            crate::app::state::WorkLinkPickerAction::Open
        );

        app.handle_work_link_picker_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert_eq!(app.state.mode, Mode::Navigate);
        assert!(app.state.work_link_picker.is_none());

        app.handle_navigate_key(TerminalKey::new(KeyCode::Char('U'), KeyModifiers::SHIFT));
        assert_eq!(app.state.mode, Mode::WorkLinkPicker);
        assert_eq!(
            app.state.work_link_picker.as_ref().unwrap().action,
            crate::app::state::WorkLinkPickerAction::Copy
        );
    }

    #[test]
    fn ac26_direct_legacy_alias_binding_dispatches_to_picker() {
        let mut app = app_with_test_workspaces(&["one"]);
        add_multiple_work_links(&mut app);
        app.state.keybinds.open_work_url = crate::config::ActionKeybinds::direct("u");

        assert!(app
            .handle_terminal_key_headless(TerminalKey::new(
                KeyCode::Char('u'),
                KeyModifiers::empty()
            ))
            .is_none());
        assert_eq!(app.state.mode, Mode::WorkLinkPicker);
        assert_eq!(
            app.state.work_link_picker.as_ref().unwrap().action,
            crate::app::state::WorkLinkPickerAction::Open
        );
    }

    #[test]
    fn ac26_open_link_picker_preserves_open_action() {
        let mut app = app_with_test_workspaces(&["one"]);
        add_multiple_work_links(&mut app);

        app.execute_tui_navigate_action(NavigateAction::OpenWorkLink, ActionContext::Prefix);

        assert_eq!(app.state.mode, Mode::WorkLinkPicker);
        assert_eq!(
            app.state.work_link_picker.as_ref().unwrap().action,
            crate::app::state::WorkLinkPickerAction::Open
        );
    }

    #[test]
    fn ac26_link_picker_revalidates_stale_snapshot_before_acting() {
        let mut app = app_with_test_workspaces(&["one"]);
        add_multiple_work_links(&mut app);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .unwrap();
        app.execute_tui_navigate_action(NavigateAction::CopyWorkLink, ActionContext::Prefix);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                ticket_ids: Some(Vec::new()),
                pr_urls: Some(Vec::new()),
                ..Default::default()
            })
            .unwrap();

        app.handle_work_link_picker_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::empty()));
        assert!(app.event_rx.try_recv().is_err());
        assert_eq!(app.state.mode, Mode::Terminal);
        assert_eq!(
            app.state
                .copy_feedback
                .as_ref()
                .map(|feedback| feedback.message.as_str()),
            Some("work link is stale")
        );
    }

    #[test]
    fn ac26_link_picker_revalidates_reordered_candidates_by_url() {
        let mut app = app_with_test_workspaces(&["one"]);
        add_multiple_work_links(&mut app);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .unwrap();

        app.execute_tui_navigate_action(NavigateAction::CopyWorkLink, ActionContext::Prefix);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                ticket_ids: Some(vec!["MAT-2".into(), "MAT-1".into()]),
                ..Default::default()
            })
            .unwrap();

        app.handle_work_link_picker_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::empty()));
        match app.event_rx.try_recv().expect("picker clipboard event") {
            crate::events::AppEvent::ClipboardWrite { content } => {
                assert_eq!(content, b"https://linear.app/scalable/issue/MAT-1")
            }
            event => panic!("unexpected event: {event:?}"),
        }
    }

    #[test]
    fn ac26_single_link_is_single_shot_and_empty_context_is_a_noop() {
        let mut app = app_with_test_workspaces(&["one"]);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .unwrap();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                ticket_ids: Some(vec!["MAT-1".into()]),
                ..Default::default()
            })
            .unwrap();
        app.execute_tui_navigate_action(NavigateAction::CopyWorkLink, ActionContext::Prefix);
        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(app.state.work_link_picker.is_none());
        assert!(matches!(
            app.event_rx
                .try_recv()
                .expect("single-shot clipboard event"),
            crate::events::AppEvent::ClipboardWrite { .. }
        ));

        let mut empty = app_with_test_workspaces(&["one"]);
        empty.execute_tui_navigate_action(NavigateAction::CopyWorkLink, ActionContext::Prefix);
        assert!(empty.event_rx.try_recv().is_err());
        assert_eq!(empty.state.mode, Mode::Terminal);
        assert_eq!(
            empty
                .state
                .copy_feedback
                .as_ref()
                .map(|feedback| feedback.message.as_str()),
            Some("focused pane has no work link")
        );
    }

    #[test]
    fn next_agent_starts_at_first_visible_entry_when_focused_agent_is_filtered_out() {
        let mut app = app_with_test_workspaces(&["hidden", "first", "second"]);
        for ws_idx in 0..app.state.workspaces.len() {
            let pane_id = app.state.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.state.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(crate::detect::Agent::Claude);
            terminal.state = if ws_idx == 0 {
                crate::detect::AgentState::Idle
            } else {
                crate::detect::AgentState::Working
            };
        }
        app.state.agent_view_override = Some(crate::api::schema::AgentViewSetParams {
            source: "example.views".to_string(),
            label: None,
            filter: Some(crate::api::schema::AgentViewFilter::Eq {
                field: crate::api::schema::AgentViewField::Builtin(
                    crate::api::schema::AgentViewBuiltinField::Status,
                ),
                value: crate::api::schema::AgentViewValue::String("working".to_string()),
            }),
            sort: Vec::new(),
        });

        app.execute_tui_navigate_action(NavigateAction::NextAgent, ActionContext::Prefix);

        assert_eq!(app.state.active, Some(1));
    }

    #[test]
    fn review_findings_agent_navigation_reveals_against_final_picker_projection() {
        let mut app = app_with_test_workspaces(&["one", "two", "three", "four", "five"]);
        for ws_idx in 0..app.state.workspaces.len() {
            let pane_id = app.state.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.state.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(crate::detect::Agent::Claude);
            terminal.state = if ws_idx == 1 {
                crate::detect::AgentState::Idle
            } else {
                crate::detect::AgentState::Blocked
            };
        }
        app.state.agent_panel_sort = crate::app::state::AgentPanelSort::Priority;
        app.state.view.sidebar_rect = ratatui::layout::Rect::new(0, 0, 30, 6);
        app.state.begin_workspace_picker_presentation();

        app.execute_tui_navigate_action(NavigateAction::NextAgent, ActionContext::Prefix);

        assert!(app.state.sidebar_shows_spaces_tree());
        let target_row = crate::ui::sidebar_rows(&app.state)
            .iter()
            .position(|row| {
                matches!(
                    row,
                    crate::ui::SidebarRow::Tab { entry, .. }
                        if entry.ws_idx == 1 && entry.tab_idx == 0
                )
            })
            .unwrap();
        let normalized = crate::ui::normalized_workspace_scroll(
            &app.state,
            app.state.view.sidebar_rect,
            app.state.workspace_scroll,
        );
        assert_eq!(
            app.state.workspace_scroll,
            crate::ui::sidebar_row_scroll_for_target(
                &app.state,
                app.state.view.sidebar_rect,
                normalized,
                target_row,
            )
        );
    }

    #[test]
    fn default_goto_key_opens_navigator() {
        let mut state = state_with_workspaces(&["test"]);

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert_eq!(state.mode, Mode::Navigator);
    }

    #[test]
    fn custom_rename_key_enters_rename_mode() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.rename_workspace = crate::config::ActionKeybinds::prefix("g");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert_eq!(state.mode, Mode::RenameWorkspace);
        assert_eq!(state.name_input, "test");
    }

    #[test]
    fn rename_workspace_prefills_live_terminal_cwd_label() {
        let mut state = state_with_workspaces(&["stale"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.workspaces[0].panes[&root]
            .attached_terminal_id
            .clone();
        state.workspaces[0].custom_name = None;
        state.workspaces[0].identity_cwd = "/__herdr_original__".into();
        state.terminals.insert(
            terminal_id.clone(),
            TerminalState::new(terminal_id, "/__herdr_projects__".into()),
        );
        state.keybinds.rename_workspace = crate::config::ActionKeybinds::prefix("g");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert_eq!(state.mode, Mode::RenameWorkspace);
        assert_eq!(state.name_input, "__herdr_projects__");
        assert_eq!(state.workspaces[0].display_name(), "__herdr_original__");
    }

    #[test]
    fn prefix_rename_workspace_targets_active_workspace_not_stale_selection() {
        let mut state = state_with_workspaces(&["main", "issue"]);
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        state.active = Some(1);
        state.selected = 0;
        state.mode = Mode::Prefix;

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::RenameWorkspace,
            ActionContext::Prefix,
        );

        assert_eq!(state.mode, Mode::RenameWorkspace);
        assert_eq!(state.selected, 1);
        assert_eq!(state.name_input, "issue");
    }

    #[test]
    fn prefix_close_workspace_targets_active_linked_worktree_without_removing_checkout() {
        let mut state = state_with_workspaces(&["main", "issue"]);
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        state.active = Some(1);
        state.selected = 0;
        state.mode = Mode::Prefix;
        state.confirm_close = false;
        state.workspaces[1].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-issue".into(),
            is_linked_worktree: true,
        });

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::CloseWorkspace,
            ActionContext::Prefix,
        );

        assert_eq!(state.request_remove_linked_worktree, None);
        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].display_name(), "main");
        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn custom_new_workspace_key_requests_and_exits_navigate() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.new_workspace = crate::config::ActionKeybinds::prefix("g");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert!(state.request_new_workspace);
        assert_eq!(state.mode, Mode::Terminal);
    }

    #[tokio::test]
    async fn new_workspace_key_opens_prefilled_prompt_and_preserves_captured_cwd() {
        let cwd = unique_temp_path("workspace-name-suggestion");
        std::fs::create_dir_all(&cwd).unwrap();
        let suggested_name = crate::workspace::derive_label_from_cwd(&cwd);
        let mut app = app_with_test_workspaces(&["test"]);
        app.state.new_terminal_cwd =
            crate::config::NewTerminalCwdConfig::Path(cwd.display().to_string());
        app.state.prompt_new_workspace_name = true;
        app.state.mode = Mode::Navigate;
        app.state.keybinds.new_workspace = crate::config::ActionKeybinds::prefix("g");

        app.handle_navigate_key(TerminalKey::new(KeyCode::Char('g'), KeyModifiers::empty()));

        assert_eq!(app.state.mode, Mode::RenameWorkspace);
        assert_eq!(app.state.name_input, suggested_name);
        assert!(app.state.name_input_replace_on_type);
        assert_eq!(app.state.pending_workspace_create_cwd.as_ref(), Some(&cwd));
        assert_eq!(app.state.workspaces.len(), 1);

        app.state.new_terminal_cwd =
            crate::config::NewTerminalCwdConfig::Path("/tmp/changed-after-prompt".into());
        app.handle_rename_key_via_api(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert_eq!(app.state.workspaces.len(), 2);
        assert_eq!(app.state.workspaces[1].identity_cwd, cwd);
        assert!(app.state.workspaces[1].custom_name.is_none());
        assert!(app.state.pending_workspace_create_cwd.is_none());
        assert_eq!(app.state.mode, Mode::Terminal);
        crate::app::api::test_support::shutdown_test_runtimes(&mut app);
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn api_rename_enter_keeps_auto_name_when_live_label_changes() {
        let mut app = app_with_test_workspaces(&["test"]);
        let tab = &app.state.workspaces[0].tabs[0];
        let terminal_id = tab
            .terminal_id(tab.layout.focused())
            .cloned()
            .expect("focused terminal");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .agent_name = Some("claude".into());

        super::super::modal::open_rename_active_tab(&mut app.state, false);
        assert_eq!(app.state.name_input, "claude");

        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .agent_name = Some("codex".into());

        app.handle_rename_key_via_api(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(
            app.state.workspaces[0].tabs[0].custom_name.is_none(),
            "an unedited Enter must not pin the stale prefill as a user name"
        );
    }

    #[tokio::test]
    async fn new_workspace_prompt_saves_custom_name_atomically() {
        let cwd = unique_temp_path("workspace-custom-name");
        std::fs::create_dir_all(&cwd).unwrap();
        let mut app = app_with_test_workspaces(&["test"]);
        app.state.new_terminal_cwd =
            crate::config::NewTerminalCwdConfig::Path(cwd.display().to_string());
        app.state.prompt_new_workspace_name = true;
        app.state.mode = Mode::Navigate;

        app.execute_tui_navigate_action(NavigateAction::NewWorkspace, ActionContext::Navigate);
        app.state.name_input = "  logs  ".into();
        app.handle_rename_key_via_api(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert_eq!(app.state.workspaces.len(), 2);
        assert_eq!(app.state.workspaces[1].custom_name.as_deref(), Some("logs"));
        assert_eq!(app.state.workspaces[1].identity_cwd, cwd);
        crate::app::api::test_support::shutdown_test_runtimes(&mut app);
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn cancelling_new_workspace_prompt_creates_nothing() {
        let mut app = app_with_test_workspaces(&["test"]);
        app.state.prompt_new_workspace_name = true;
        app.state.mode = Mode::Navigate;

        app.execute_tui_navigate_action(NavigateAction::NewWorkspace, ActionContext::Navigate);
        app.handle_rename_key_via_api(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));

        assert_eq!(app.state.workspaces.len(), 1);
        assert!(app.state.pending_workspace_create_cwd.is_none());
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[test]
    fn custom_new_worktree_key_requests_selected_workspace() {
        let mut state = state_with_workspaces(&["main", "scratch"]);
        state.workspaces[1].identity_cwd = unique_temp_path("navigate-new-worktree-selected");
        state.mode = Mode::Navigate;
        state.selected = 1;
        state.active = Some(0);
        state.keybinds.new_worktree = crate::config::ActionKeybinds::prefix("g");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert_eq!(state.request_new_linked_worktree, Some(1));
        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn worktree_actions_do_not_start_from_linked_child_workspace() {
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        let mut state = state_with_workspaces(&["main", "issue"]);
        mark_worktree_space_member(&mut state, 0, "repo-key");
        mark_worktree_space_member(&mut state, 1, "repo-key");
        state.mode = Mode::Navigate;
        state.selected = 1;
        state.active = Some(0);

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::NewWorktree,
            ActionContext::Navigate,
        );
        assert_eq!(state.request_new_linked_worktree, None);

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::OpenWorktree,
            ActionContext::Navigate,
        );
        assert_eq!(state.request_open_existing_worktree, None);
    }

    #[test]
    fn direct_new_worktree_action_targets_active_workspace() {
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        let mut state = state_with_workspaces(&["main", "scratch"]);
        state.workspaces[0].identity_cwd = unique_temp_path("navigate-new-worktree-active");
        state.mode = Mode::Terminal;
        state.selected = 1;
        state.active = Some(0);

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::NewWorktree,
            ActionContext::Direct,
        );

        assert_eq!(state.request_new_linked_worktree, Some(0));
    }

    #[test]
    fn navigate_down_follows_grouped_sidebar_visual_order() {
        let mut state = state_with_workspaces(&["main", "normal", "issue"]);
        mark_worktree_space_member(&mut state, 0, "repo-key");
        mark_worktree_space_member(&mut state, 2, "repo-key");
        state.mode = Mode::Navigate;
        state.active = Some(0);
        state.selected = 0;

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );

        assert_eq!(state.selected, 2);
    }

    #[test]
    fn navigate_number_keys_follow_grouped_sidebar_visual_order() {
        let mut state = state_with_workspaces(&["main", "normal", "issue"]);
        mark_worktree_space_member(&mut state, 0, "repo-key");
        mark_worktree_space_member(&mut state, 2, "repo-key");
        state.mode = Mode::Navigate;
        state.active = Some(0);
        state.selected = 0;

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('2'), KeyModifiers::empty()),
        );

        assert_eq!(state.active, Some(2));
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn indexed_switch_workspace_keybind_follows_grouped_sidebar_visual_order() {
        let mut state = state_with_workspaces(&["main", "normal", "issue"]);
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        mark_worktree_space_member(&mut state, 0, "repo-key");
        mark_worktree_space_member(&mut state, 2, "repo-key");
        state.mode = Mode::Prefix;
        state.active = Some(0);
        state.selected = 0;

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::SwitchWorkspace(1),
            ActionContext::Prefix,
        );

        assert_eq!(state.active, Some(2));
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn custom_sidebar_toggle_key_toggles_and_exits_navigate() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.toggle_sidebar = crate::config::ActionKeybinds::prefix("g");
        assert!(!state.sidebar_collapsed);

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert!(state.sidebar_collapsed);
        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn custom_resize_key_enters_resize_mode() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.resize_mode = crate::config::ActionKeybinds::prefix("g");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert_eq!(state.mode, Mode::Resize);
    }

    #[test]
    fn custom_reload_config_key_requests_reload_and_exits_navigate() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.reload_config = crate::config::ActionKeybinds::prefix("g");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert!(state.request_reload_config);
        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn custom_open_notification_key_focuses_current_toast_target() {
        let mut state = state_with_workspaces(&["one", "two"]);
        state.active = Some(0);
        state.selected = 0;
        state.mode = Mode::Navigate;
        state.keybinds.open_notification_target = crate::config::ActionKeybinds::prefix("g");
        let target_workspace_id = state.workspaces[1].id.clone();
        let target_pane = state.workspaces[1].tabs[0].root_pane;
        state.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::NeedsAttention,
            title: "pi needs attention".into(),
            context: "two".into(),
            position: None,
            target: Some(crate::app::state::ToastTarget {
                workspace_id: target_workspace_id,
                pane_id: target_pane,
            }),
        });

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert_eq!(state.active, Some(1));
        assert_eq!(state.selected, 1);
        assert_eq!(state.workspaces[1].focused_pane_id(), Some(target_pane));
        assert!(state.toast.is_none());
        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn movement_action_stays_in_navigate_mode() {
        let mut state = state_with_workspaces(&["a", "b"]);
        state.selected = 0;

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );

        assert_eq!(state.selected, 1);
        assert_eq!(state.mode, Mode::Navigate);
    }

    #[test]
    fn navigate_workspace_keys_are_configurable() {
        let mut state = state_with_workspaces(&["a", "b"]);
        let config: Config = toml::from_str(
            r#"
[keys]
navigate_workspace_down = "j"
navigate_pane_down = "ctrl+j"
"#,
        )
        .unwrap();
        state.keybinds = config.keybinds();
        state.selected = 0;

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::empty()),
        );

        assert_eq!(state.selected, 1);
        assert_eq!(state.mode, Mode::Navigate);
    }

    #[test]
    fn navigate_pane_keys_are_configurable() {
        let mut state = state_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let below = state.workspaces[0].test_split(Direction::Vertical);
        state.workspaces[0].layout.focus_pane(root);
        state.view.pane_infos = state.workspaces[0]
            .active_tab()
            .unwrap()
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 80, 24));
        let config: Config = toml::from_str(
            r#"
[keys]
navigate_workspace_down = "j"
navigate_pane_down = "ctrl+j"
"#,
        )
        .unwrap();
        state.keybinds = config.keybinds();

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
        );

        assert_eq!(state.workspaces[0].focused_pane_id(), Some(below));
        assert_eq!(state.mode, Mode::Navigate);
    }

    #[test]
    fn focus_pane_prefix_rhs_does_not_create_navigate_mode_pane_shortcut() {
        let mut state = state_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let below = state.workspaces[0].test_split(Direction::Vertical);
        state.workspaces[0].layout.focus_pane(root);
        state.view.pane_infos = state.workspaces[0]
            .active_tab()
            .unwrap()
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 80, 24));
        let config: Config = toml::from_str(
            r#"
[keys]
focus_pane_down = "prefix+f"
"#,
        )
        .unwrap();
        state.keybinds = config.keybinds();

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::empty()),
        );
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(root));

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::empty()),
        );
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(below));
        assert_eq!(state.mode, Mode::Navigate);
    }

    #[test]
    fn customized_navigate_pane_key_disables_matching_prefix_rhs_fallback() {
        let mut state = state_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let below = state.workspaces[0].test_split(Direction::Vertical);
        state.workspaces[0].layout.focus_pane(root);
        state.view.pane_infos = state.workspaces[0]
            .active_tab()
            .unwrap()
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 80, 24));
        let config: Config = toml::from_str(
            r#"
[keys]
navigate_pane_down = "ctrl+j"
"#,
        )
        .unwrap();
        state.keybinds = config.keybinds();

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::empty()),
        );
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(root));

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
        );
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(below));
        assert_eq!(state.mode, Mode::Navigate);
    }

    #[test]
    fn left_and_right_arrows_remain_permanent_navigate_pane_aliases() {
        let mut state = state_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let right = state.workspaces[0].test_split(Direction::Horizontal);
        state.workspaces[0].layout.focus_pane(right);
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 80, 24));
        let config: Config = toml::from_str(
            r#"
[keys]
navigate_pane_left = "ctrl+h"
navigate_pane_right = "ctrl+l"
"#,
        )
        .unwrap();
        state.keybinds = config.keybinds();

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Left, KeyModifiers::empty()),
        );
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(root));
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 80, 24));

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Right, KeyModifiers::empty()),
        );
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(right));
        assert_eq!(state.mode, Mode::Navigate);
    }

    #[test]
    fn mobile_workspace_keyboard_navigation_keeps_selected_row_visible() {
        let mut state = state_with_workspaces(&["a", "b", "c", "d"]);
        state.active = Some(0);
        state.selected = 0;
        state.mode = Mode::Navigate;
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 44, 8));
        assert_eq!(state.mobile_switcher_scroll, 0);

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );

        assert_eq!(state.selected, 1);
        assert_eq!(state.mobile_switcher_scroll, 0);
    }

    #[test]
    fn terminal_direct_agent_shortcut_maps_to_navigation_action() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.next_agent = crate::config::ActionKeybinds::direct("alt+a");

        let action = terminal_direct_navigation_action(
            &state,
            TerminalKey::new(KeyCode::Char('a'), KeyModifiers::ALT),
        );

        assert_eq!(action, Some(NavigateAction::NextAgent));
    }

    #[test]
    fn terminal_direct_focus_pane_shortcut_maps_to_navigation_action() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.focus_pane_left = crate::config::ActionKeybinds::direct("alt+left");

        let action = terminal_direct_navigation_action(
            &state,
            TerminalKey::new(KeyCode::Left, KeyModifiers::ALT),
        );

        assert_eq!(action, Some(NavigateAction::FocusPaneLeft));
    }

    #[test]
    fn terminal_direct_swap_pane_shortcut_maps_to_navigation_action() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.swap_pane_right = crate::config::ActionKeybinds::direct("alt+shift+l");

        let action = terminal_direct_navigation_action(
            &state,
            TerminalKey::new(KeyCode::Char('l'), KeyModifiers::ALT | KeyModifiers::SHIFT),
        );

        assert_eq!(action, Some(NavigateAction::SwapPaneRight));
    }

    #[test]
    fn terminal_direct_last_pane_shortcut_maps_to_navigation_action() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.last_pane = crate::config::ActionKeybinds::direct("alt+l");

        let action = terminal_direct_navigation_action(
            &state,
            TerminalKey::new(KeyCode::Char('l'), KeyModifiers::ALT),
        );

        assert_eq!(action, Some(NavigateAction::LastPane));
    }

    #[test]
    fn prefix_tab_override_can_map_to_last_pane() {
        let config: Config = toml::from_str(
            r#"
[keys]
last_pane = "prefix+tab"
"#,
        )
        .unwrap();
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds = config.keybinds();

        let pane_action = action_for_key(
            &state,
            TerminalKey::new(KeyCode::Tab, KeyModifiers::empty()),
            BindingDispatch::Prefix,
        );

        assert_eq!(pane_action, Some(NavigateAction::LastPane));
    }

    #[test]
    fn terminal_direct_indexed_tab_shortcut_maps_to_navigation_action() {
        let mut state = state_with_workspaces(&["test"]);
        let config: Config = toml::from_str("[keys]\nswitch_tab = \"ctrl+3\"\n").unwrap();
        state.keybinds.switch_tab = config.keybinds().switch_tab;

        let action = terminal_direct_navigation_action(
            &state,
            TerminalKey::new(KeyCode::Char('3'), KeyModifiers::CONTROL),
        );

        assert_eq!(action, Some(NavigateAction::SwitchTab(2)));
    }

    #[test]
    fn prefix_shift_indexed_workspace_shortcut_maps_legacy_us_symbol_key() {
        let mut state = state_with_workspaces(&["one", "two"]);
        let config: Config =
            toml::from_str("[keys]\nswitch_workspace = \"prefix+shift+1..9\"\n").unwrap();
        state.keybinds.switch_workspace = config.keybinds().switch_workspace;

        let action = action_for_key(
            &state,
            TerminalKey::new(KeyCode::Char('@'), KeyModifiers::empty()),
            BindingDispatch::Prefix,
        );

        assert_eq!(action, Some(NavigateAction::SwitchWorkspace(1)));
    }

    #[test]
    fn prefix_shift_indexed_workspace_shortcut_maps_non_us_number_rows() {
        let mut state = state_with_workspaces(&["one", "two"]);
        let config: Config =
            toml::from_str("[keys]\nswitch_workspace = \"prefix+shift+1..9\"\n").unwrap();
        state.keybinds.switch_workspace = config.keybinds().switch_workspace;

        for key in [
            TerminalKey::new(KeyCode::Char('2'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('"' as u32),
            TerminalKey::new(KeyCode::Char('é'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('2' as u32),
        ] {
            assert_eq!(
                action_for_key(&state, key, BindingDispatch::Prefix),
                Some(NavigateAction::SwitchWorkspace(1))
            );
        }
    }

    #[test]
    fn prefix_shift_indexed_workspace_shortcut_survives_modifier_press() {
        let mut app = app_with_test_workspaces(&["one", "two"]);
        let config: Config =
            toml::from_str("[keys]\nswitch_workspace = \"prefix+shift+1..9\"\n").unwrap();
        app.state.keybinds.switch_workspace = config.keybinds().switch_workspace;
        app.state.mode = Mode::Prefix;

        app.handle_prefix_key(TerminalKey::new(
            KeyCode::Modifier(ModifierKeyCode::LeftShift),
            KeyModifiers::SHIFT,
        ));

        assert_eq!(app.state.mode, Mode::Prefix);

        app.handle_prefix_key(
            TerminalKey::new(KeyCode::Char('2'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('"' as u32),
        );

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[test]
    fn prefix_unshifted_indexed_shortcut_maps_shifted_french_number_row() {
        let mut state = state_with_workspaces(&["one"]);
        let config: Config = toml::from_str("[keys]\nswitch_tab = \"prefix+1..9\"\n").unwrap();
        state.keybinds.switch_tab = config.keybinds().switch_tab;

        let action = action_for_key(
            &state,
            TerminalKey::new(KeyCode::Char('é'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('2' as u32),
            BindingDispatch::Prefix,
        );

        assert_eq!(action, Some(NavigateAction::SwitchTab(1)));
    }

    #[test]
    fn literal_symbol_binding_takes_precedence_over_shifted_indexed_alias() {
        let mut state = state_with_workspaces(&["one", "two"]);
        let config: Config = toml::from_str(
            r#"
[keys]
help = "prefix+!"
switch_workspace = "prefix+shift+1..9"
"#,
        )
        .unwrap();
        state.keybinds = config.keybinds();

        let action = action_for_key(
            &state,
            TerminalKey::new(KeyCode::Char('!'), KeyModifiers::empty()),
            BindingDispatch::Prefix,
        );

        assert_eq!(action, Some(NavigateAction::Help));
    }

    #[test]
    fn literal_symbol_custom_command_is_visible_before_shifted_indexed_alias() {
        let mut state = state_with_workspaces(&["one", "two"]);
        let config: Config = toml::from_str(
            r#"
[keys]
switch_workspace = "prefix+shift+1..9"

[[keys.command]]
key = "prefix+!"
command = "echo literal"
"#,
        )
        .unwrap();
        state.keybinds = config.keybinds();

        let key = TerminalKey::new(KeyCode::Char('!'), KeyModifiers::empty());
        assert!(command_for_key(&state, &key, BindingDispatch::Prefix).is_some());
        assert_eq!(
            indexed_navigation_action(&state, &key, BindingDispatch::Prefix),
            Some(NavigateAction::SwitchWorkspace(0))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn literal_symbol_custom_command_runs_before_shifted_indexed_alias() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.state.active = Some(1);
        app.state.selected = 1;
        app.state.mode = Mode::Terminal;

        let output_path = unique_temp_path("literal-symbol-custom-command");
        let config: Config = toml::from_str(&format!(
            r#"
[keys]
switch_workspace = "prefix+shift+1..9"

[[keys.command]]
key = "prefix+!"
command = "printf literal > '{}'"
"#,
            output_path.display()
        ))
        .unwrap();
        app.state.keybinds = config.keybinds();

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(TerminalKey::new(KeyCode::Char('!'), KeyModifiers::empty()))
            .await;

        assert_eq!(wait_for_file(&output_path), "literal");
        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.mode, Mode::Terminal);
        let _ = std::fs::remove_file(output_path);
    }

    #[tokio::test]
    async fn navigate_mode_runs_prefix_action_rhs_without_pressing_prefix_again() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Navigate;

        app.handle_navigate_key(TerminalKey::new(KeyCode::Char('n'), KeyModifiers::SHIFT));

        assert_eq!(app.state.workspaces.len(), 2);
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[tokio::test]
    async fn navigate_mode_matches_legacy_uppercase_shifted_letter() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Navigate;

        app.handle_navigate_key(TerminalKey::new(KeyCode::Char('N'), KeyModifiers::empty()));

        assert_eq!(app.state.workspaces.len(), 2);
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[tokio::test]
    async fn legacy_uppercase_prefers_shifted_workspace_binding_over_unshifted() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Navigate;

        app.handle_navigate_key(TerminalKey::new(KeyCode::Char('W'), KeyModifiers::empty()));

        assert_eq!(app.state.mode, Mode::RenameWorkspace);
    }

    #[tokio::test]
    async fn legacy_uppercase_prefers_shifted_reload_binding_over_unshifted() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Navigate;

        app.handle_navigate_key(TerminalKey::new(KeyCode::Char('R'), KeyModifiers::empty()));

        assert!(!app.state.request_reload_config);
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[tokio::test]
    async fn legacy_uppercase_prefers_shifted_pane_binding_over_unshifted() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Navigate;

        app.handle_navigate_key(TerminalKey::new(KeyCode::Char('P'), KeyModifiers::empty()));

        assert_eq!(app.state.mode, Mode::RenamePane);
    }

    #[test]
    fn app_navigate_mode_workspace_down_moves_selection() {
        let mut app = app_with_test_workspaces(&["one", "two"]);
        app.state.mode = Mode::Navigate;

        app.handle_navigate_key(TerminalKey::new(KeyCode::Down, KeyModifiers::empty()));

        assert_eq!(app.state.selected, 1);
        assert_eq!(app.state.mode, Mode::Navigate);
    }

    #[test]
    fn app_navigate_mode_maps_french_number_row_to_workspace() {
        let mut app = app_with_test_workspaces(&["one", "two"]);
        app.state.mode = Mode::Navigate;

        app.handle_navigate_key(
            TerminalKey::new(KeyCode::Char('é'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('2' as u32),
        );

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[test]
    fn app_navigate_mode_workspace_keys_are_configurable() {
        let mut app = app_with_test_workspaces(&["one", "two"]);
        let config: Config = toml::from_str(
            r#"
[keys]
navigate_workspace_down = "j"
navigate_pane_down = "ctrl+j"
"#,
        )
        .unwrap();
        app.state.keybinds = config.keybinds();
        app.state.mode = Mode::Navigate;

        app.handle_navigate_key(TerminalKey::new(KeyCode::Char('j'), KeyModifiers::empty()));

        assert_eq!(app.state.selected, 1);
        assert_eq!(app.state.mode, Mode::Navigate);
    }

    #[tokio::test]
    async fn prefix_focus_pane_is_one_shot_and_returns_to_terminal() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(Direction::Horizontal);
        app.state.workspaces[0].layout.focus_pane(right);
        app.state.view.pane_infos = app.state.workspaces[0]
            .active_tab()
            .unwrap()
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 80, 24));

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(TerminalKey::new(KeyCode::Char('h'), KeyModifiers::empty()))
            .await;

        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(root));
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[tokio::test]
    async fn navigate_focus_pane_keeps_navigate_mode_active() {
        let mut app = app_with_test_workspaces(&["test"]);
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let below = app.state.workspaces[0].test_split(Direction::Vertical);
        app.state.workspaces[0].layout.focus_pane(below);
        app.state.view.pane_infos = app.state.workspaces[0]
            .active_tab()
            .unwrap()
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 80, 24));
        app.state.mode = Mode::Navigate;

        app.handle_key(TerminalKey::new(KeyCode::Char('k'), KeyModifiers::empty()))
            .await;

        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(root));
        assert_eq!(app.state.mode, Mode::Navigate);
    }

    #[tokio::test]
    async fn no_op_prefix_action_exits_prefix_mode() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(TerminalKey::new(KeyCode::Char('o'), KeyModifiers::empty()))
            .await;

        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[tokio::test]
    async fn unmatched_prefix_rhs_exits_prefix_mode() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(TerminalKey::new(KeyCode::F(12), KeyModifiers::empty()))
            .await;

        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[tokio::test]
    async fn prefix_help_matches_enhanced_shifted_question_mark() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(
            TerminalKey::new(KeyCode::Char('/'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('?' as u32),
        )
        .await;

        assert_eq!(app.state.mode, Mode::KeybindHelp);
    }

    #[test]
    fn navigate_mode_help_is_binding_driven() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.help = crate::config::ActionKeybinds::prefix("f");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT),
        );
        assert_eq!(state.mode, Mode::Navigate);

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::empty()),
        );
        assert_eq!(state.mode, Mode::KeybindHelp);
    }

    #[test]
    fn modified_navigate_local_key_can_be_bound_as_prefix_rhs() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.toggle_sidebar = crate::config::ActionKeybinds::prefix("shift+u");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('U'), KeyModifiers::SHIFT),
        );

        assert!(state.sidebar_collapsed);
    }

    #[test]
    fn empty_state_new_tab_is_no_op() {
        let mut state = crate::app::state::AppState::test_new();
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        state.mode = Mode::Prefix;

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::NewTab,
            ActionContext::Prefix,
        );

        assert_eq!(state.mode, Mode::Navigate);
        assert!(!state.creating_new_tab);
        assert!(!state.request_new_tab);
        assert!(state.workspaces.is_empty());
    }

    #[test]
    fn closing_linked_worktree_closes_workspace_without_removing_checkout() {
        let mut state = state_with_workspaces(&["main", "issue"]);
        state.selected = 1;
        state.active = Some(1);
        state.mode = Mode::Navigate;
        state.confirm_close = false;
        state.workspaces[1].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-issue".into(),
            is_linked_worktree: true,
        });

        execute_navigate_action(&mut state, NavigateAction::CloseWorkspace);

        assert_eq!(state.request_remove_linked_worktree, None);
        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].display_name(), "main");
        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn prefix_close_pane_last_parent_group_pane_opens_confirmation() {
        let mut state = state_with_workspaces(&["main", "issue"]);
        mark_worktree_space_member(&mut state, 0, "repo-key");
        mark_worktree_space_member(&mut state, 1, "repo-key");
        state.selected = 1;
        state.active = Some(0);
        state.mode = Mode::Navigate;

        execute_navigate_action(&mut state, NavigateAction::ClosePane);

        assert_eq!(state.selected, 0);
        assert_eq!(state.mode, Mode::ConfirmClose);
        assert_eq!(state.workspaces.len(), 2);
    }

    #[test]
    fn tui_close_tab_last_parent_group_workspace_opens_confirmation_via_api() {
        let mut app = app_with_test_workspaces(&["main", "issue"]);
        mark_worktree_space_member(&mut app.state, 0, "repo-key");
        mark_worktree_space_member(&mut app.state, 1, "repo-key");
        app.state.active = Some(0);
        app.state.selected = 1;
        app.state.mode = Mode::Navigate;

        app.execute_tui_navigate_action(NavigateAction::CloseTab, ActionContext::Navigate);

        assert_eq!(app.state.selected, 0);
        assert_eq!(app.state.mode, Mode::ConfirmClose);
        assert_eq!(app.state.workspaces.len(), 2);
    }

    #[test]
    fn tui_close_pane_last_parent_group_pane_opens_confirmation_via_api() {
        let mut app = app_with_test_workspaces(&["main", "issue"]);
        mark_worktree_space_member(&mut app.state, 0, "repo-key");
        mark_worktree_space_member(&mut app.state, 1, "repo-key");
        app.state.active = Some(0);
        app.state.selected = 1;
        app.state.mode = Mode::Navigate;

        app.execute_tui_navigate_action(NavigateAction::ClosePane, ActionContext::Navigate);

        assert_eq!(app.state.selected, 0);
        assert_eq!(app.state.mode, Mode::ConfirmClose);
        assert_eq!(app.state.workspaces.len(), 2);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn custom_command_runs_from_prefix_key_in_navigate_mode() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        let output_path = unique_temp_path("custom-command-keybind");
        let release_path = unique_temp_path("custom-command-release");
        let command = format!(
            "printf '%s\\n%s\\n%s\\n%s\\n' \"$$\" \"$HERDR_ACTIVE_WORKSPACE_ID\" \"$HERDR_ACTIVE_TAB_ID\" \"$HERDR_ACTIVE_PANE_ID\" > '{}'; i=0; while [ ! -e '{}' ] && [ \"$i\" -lt 250 ]; do sleep 0.02; i=$((i + 1)); done",
            output_path.display(),
            release_path.display(),
        );
        app.state.keybinds.custom_commands = vec![crate::config::CustomCommandKeybind {
            bindings: crate::config::ActionKeybinds::prefix("m"),
            label: "prefix+m".into(),
            command,
            action: crate::config::CustomCommandAction::Shell,
            description: None,
            width: None,
            height: None,
        }];

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        assert_eq!(app.state.mode, Mode::Prefix);

        let launch_started = std::time::Instant::now();
        app.handle_key(TerminalKey::new(KeyCode::Char('m'), KeyModifiers::empty()))
            .await;
        assert!(launch_started.elapsed() < Duration::from_secs(2));

        let content = wait_for_file(&output_path);
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 4);
        let pid = lines[0]
            .parse::<u32>()
            .expect("command should report its pid");
        assert!(crate::platform::process_exists(pid));
        assert_eq!(lines[1], app.state.workspaces[0].id);
        assert_eq!(lines[2], format!("{}:t1", app.state.workspaces[0].id));
        assert_eq!(lines[3], format!("{}:p1", app.state.workspaces[0].id));
        assert_eq!(app.state.mode, Mode::Terminal);

        std::fs::write(&release_path, b"release").expect("release command");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while crate::platform::process_exists(pid) && tokio::time::Instant::now() < deadline {
            app.reap_finished_custom_commands();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        app.reap_finished_custom_commands();
        let reaped_by_runtime = !crate::platform::process_exists(pid);
        if !reaped_by_runtime {
            if let Some(child) = app
                .detached_custom_command_children
                .iter_mut()
                .find(|child| child.id() == pid)
            {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        assert!(
            reaped_by_runtime,
            "detached command child {pid} was not reaped"
        );

        let _ = std::fs::remove_file(output_path);
        let _ = std::fs::remove_file(release_path);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pane_overlay_command_opens_and_closes_after_exit() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let (workspace, terminal, runtime) = Workspace::new(
            std::env::current_dir().unwrap_or_else(|_| "/".into()),
            24,
            80,
            app.state.pane_scrollback_limit_bytes,
            app.state.host_terminal_theme,
            app.state.host_terminal_appearance,
            crate::pane::PaneShellConfig::new(&app.state.default_shell, app.state.shell_mode),
            app.event_tx.clone(),
            app.render_notify.clone(),
            app.render_dirty.clone(),
        )
        .expect("workspace should spawn");
        let root_pane = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.terminal_runtimes.insert(terminal.id.clone(), runtime);
        app.state.terminals.insert(terminal.id.clone(), terminal);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        let output_path = unique_temp_path("custom-pane-command");
        let command = format!("printf done > '{}'", output_path.display());
        app.state.keybinds.custom_commands = vec![crate::config::CustomCommandKeybind {
            bindings: crate::config::ActionKeybinds::prefix("m"),
            label: "prefix+m".into(),
            command,
            action: crate::config::CustomCommandAction::Pane,
            description: None,
            width: None,
            height: None,
        }];

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(TerminalKey::new(KeyCode::Char('m'), KeyModifiers::empty()))
            .await;

        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 2);
        assert_eq!(app.terminal_runtimes.len(), 2);
        assert!(app.state.workspaces[0].tabs[0].zoomed);
        let overlay_pane = app.state.workspaces[0].focused_pane_id().unwrap();
        assert_ne!(overlay_pane, root_pane);

        app.state.last_pane();

        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(root_pane));

        app.state.last_pane();

        assert_eq!(
            app.state.workspaces[0].focused_pane_id(),
            Some(overlay_pane)
        );

        let _ = wait_for_file(&output_path);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if app.drain_internal_events()
                && app.state.workspaces[0].tabs[0].layout.pane_count() == 1
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 1);
        assert!(!app.state.workspaces[0].tabs[0].zoomed);
        assert_eq!(app.state.mode, Mode::Terminal);
        let _ = std::fs::remove_file(output_path);

        let runtimes: Vec<_> = app.terminal_runtimes.drain().collect();
        for (_terminal_id, runtime) in runtimes {
            runtime.shutdown();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edit_scrollback_key_opens_focused_runtime_scrollback_in_editor_pane() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = Workspace::test_new("test");
        let root_pane = workspace.tabs[0].root_pane;
        workspace.tabs[0].runtimes.insert(
            root_pane,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                20,
                5,
                4096,
                b"alpha\nbeta\n",
            ),
        );
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        let output_path = unique_temp_path("edit-scrollback");
        let previous_editor = std::env::var_os("EDITOR");
        std::env::set_var(
            "EDITOR",
            format!("sh -c 'cp \"$1\" {}' sh", output_path.display()),
        );
        app.state.keybinds.edit_scrollback = crate::config::ActionKeybinds::prefix("g");

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(TerminalKey::new(KeyCode::Char('g'), KeyModifiers::empty()))
            .await;

        match previous_editor {
            Some(value) => std::env::set_var("EDITOR", value),
            None => std::env::remove_var("EDITOR"),
        }

        let content = wait_for_file(&output_path);
        assert!(content.contains("alpha"));
        assert!(content.contains("beta"));
        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(
            app.state.terminals.values().any(|terminal| terminal
                .launch_argv
                .as_ref()
                .is_some_and(|argv| argv.first().is_some_and(|program| program == "/bin/sh"))),
            "scrollback editor should launch through argv overlay path"
        );

        let _ = std::fs::remove_file(output_path);
    }

    #[test]
    fn zoom_action_exits_navigate_mode() {
        let mut state = state_with_workspaces(&["test"]);
        state.workspaces[0].test_split(Direction::Horizontal);
        state.keybinds.zoom = crate::config::ActionKeybinds::prefix("g");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert!(state.workspaces[0].zoomed);
        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn focus_pane_action_keeps_zoomed_when_changing_focus() {
        let mut state = state_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let right = state.workspaces[0].test_split(Direction::Horizontal);
        state.workspaces[0].layout.focus_pane(root);
        state.workspaces[0].zoomed = true;
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 100, 20));

        execute_navigate_action(&mut state, NavigateAction::FocusPaneRight);

        assert!(state.workspaces[0].zoomed);
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(right));
    }

    #[test]
    fn question_mark_opens_keybind_help_from_navigate() {
        let mut state = state_with_workspaces(&["test"]);

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT),
        );

        assert_eq!(state.mode, Mode::KeybindHelp);
    }

    #[test]
    fn new_tab_action_opens_dialog_without_creating_tab() {
        let mut state = state_with_workspaces(&["test"]);

        execute_navigate_action(&mut state, NavigateAction::NewTab);

        assert_eq!(state.mode, Mode::RenameTab);
        assert!(state.creating_new_tab);
        assert_eq!(state.name_input, "2");
        assert!(state.name_input_replace_on_type);
        assert!(!state.request_new_tab);
        assert_eq!(state.workspaces[0].tabs.len(), 1);
    }

    #[test]
    fn new_tab_action_can_skip_rename_dialog() {
        let mut state = state_with_workspaces(&["test"]);
        state.prompt_new_tab_name = false;

        execute_navigate_action(&mut state, NavigateAction::NewTab);

        assert_eq!(state.mode, Mode::Terminal);
        assert!(!state.creating_new_tab);
        assert!(state.request_new_tab);
        assert!(state.requested_new_tab_name.is_none());
    }

    #[test]
    fn navigate_q_detaches_in_persistence_mode() {
        let mut state = crate::app::state::AppState::test_new();
        state.detach_exits = false;

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::empty()),
        );

        assert!(state.detach_requested);
        assert!(!state.should_quit);
    }
}
