//! Input handling — translates crossterm key/mouse events into state mutations.

use bytes::Bytes;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tracing::warn;

use crate::app::PaneClickState;
use crate::input::TerminalKey;
#[cfg(test)]
use ratatui::layout::Direction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollbarClickTarget {
    Thumb { grab_row_offset: u16 },
    Track { offset_from_bottom: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
enum WheelRouting {
    HostScroll,
    MouseReport,
    AlternateScroll,
}

const WORKSPACE_DRAG_THRESHOLD: u16 = 1;
const TAB_DRAG_THRESHOLD: u16 = 1;

fn modified_url_click_modifier() -> KeyModifiers {
    KeyModifiers::CONTROL
}

#[cfg(test)]
#[test]
fn modified_url_click_modifier_matches_terminal_mouse_reporting() {
    assert_eq!(modified_url_click_modifier(), KeyModifiers::CONTROL);
}

mod clipboard;
mod copy_mode;
mod dock;
mod lease;
mod modal;
mod mouse;
mod navigate;
mod overlays;
mod selection;
mod settings;
mod sidebar;
mod terminal;

pub(crate) use self::{
    lease::{ConsumedInputLease, ForwardedInputLease, InputLeaseKey, InputLeaseTable, RepeatPlan},
    modal::{
        handle_global_menu_key, handle_keybind_help_key, handle_navigator_key,
        insert_keybind_help_query_text, insert_navigator_search_text, insert_rename_input_text,
        open_new_workspace_dialog,
    },
    navigate::{
        terminal_direct_indexed_navigation_action, terminal_direct_non_indexed_navigation_action,
    },
    settings::open_settings_at,
};
use self::{
    modal::{
        modal_action_from_key, ModalAction, ONBOARDING_WELCOME_ACTIONS, RELEASE_NOTES_ACTIONS,
    },
    mouse::MouseAction,
    settings::SettingsAction,
};
use super::state::{AppState, Mode};
use super::App;

// ---------------------------------------------------------------------------
// Key handling
// ---------------------------------------------------------------------------

impl App {
    pub(super) async fn handle_key(
        &mut self,
        key: TerminalKey,
    ) -> Option<super::TerminalInputTarget> {
        if self.state.popup_pane.is_some() {
            return self.handle_terminal_key(key).await;
        }
        let key_event = key.as_key_event();
        if self.handle_symphony_key(key_event) {
            return None;
        }
        if self.handle_loop_run_history_key(key_event) {
            return None;
        }
        if self.state.home.is_some() {
            return self.handle_home_key(key).await;
        }
        if self.state.inbox.is_some() {
            return self.handle_inbox_key(key).await;
        }
        if modal_paste_target_active(&self.state) && is_modal_paste_shortcut(&key_event) {
            if let Some(text) = crate::platform::read_clipboard_text() {
                self.paste_into_active_text_input(&text);
            }
            return None;
        }

        match self.state.mode {
            Mode::Terminal => return self.handle_terminal_key(key).await,
            Mode::Prefix => self.handle_prefix_key(key),
            Mode::Navigate => self.handle_navigate_key(key),
            Mode::Copy => self.handle_copy_mode_key(key),
            _ => match self.state.mode {
                Mode::Onboarding => self.handle_onboarding_key(key_event),
                Mode::ReleaseNotes => self.handle_release_notes_key(key_event),
                Mode::ProductAnnouncement => self.handle_product_announcement_key(key_event),
                Mode::Prefix | Mode::Navigate | Mode::Copy => unreachable!(),
                Mode::RenameWorkspace | Mode::RenameTab | Mode::RenamePane => {
                    self.handle_rename_key_via_api(key_event)
                }
                Mode::NewLinkedWorktree => self.handle_worktree_create_key(key_event),
                Mode::OpenExistingWorktree => self.handle_worktree_open_key(key_event),
                Mode::ConfirmRemoveWorktree => self.handle_worktree_remove_key(key_event),
                Mode::Resize => self.handle_resize_key_via_api(key),
                Mode::ConfirmClose => self.handle_confirm_close_key_via_api(key_event),
                Mode::ContextMenu => {
                    self.handle_context_menu_key_via_api(key_event);
                }
                Mode::Settings => self.handle_settings_key(key_event),
                Mode::GlobalMenu => handle_global_menu_key(&mut self.state, key_event),
                Mode::KeybindHelp => handle_keybind_help_key(&mut self.state, key),
                Mode::Navigator => {
                    handle_navigator_key(&mut self.state, &self.terminal_runtimes, key_event)
                }
                Mode::WorkLinkPicker => self.handle_work_link_picker_key(key_event),
                Mode::Terminal => unreachable!(),
            },
        }
        None
    }

    /// Home owns the full key stream so navigation never leaks into a pane.
    pub(crate) async fn handle_home_key(
        &mut self,
        key: crate::input::TerminalKey,
    ) -> Option<super::TerminalInputTarget> {
        self.handle_home_key_event(key.as_key_event());
        None
    }

    /// Headless mirror. Home consumes every key, including keys without an action.
    pub(crate) fn handle_home_key_headless(&mut self, key: KeyEvent) -> bool {
        if self.state.home.is_none() {
            return false;
        }
        self.handle_home_key_event(key);
        true
    }

    fn handle_home_key_event(&mut self, event: KeyEvent) {
        let queue = self.state.blocked_agents();
        if event.modifiers.is_empty() {
            match event.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    if let Some(home) = self.state.home.as_mut() {
                        home.select_prev(&queue);
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if let Some(home) = self.state.home.as_mut() {
                        home.select_next(&queue);
                    }
                }
                KeyCode::Enter => {
                    self.state.jump_to_selected_home_agent(&queue);
                }
                KeyCode::Esc => {
                    self.state.clear_home();
                    self.state.mode = Mode::Terminal;
                }
                _ => {}
            }
        }
    }

    pub(crate) fn handle_loop_run_history_key(&mut self, key: KeyEvent) -> bool {
        if self.state.loop_run_history_detail.is_none() {
            return false;
        }
        if key.code == KeyCode::Esc && key.modifiers.is_empty() {
            self.state.clear_loop_run_history();
            self.state.mode = Mode::Terminal;
        }
        true
    }

    /// Inbox keys go to the blocked agent on screen, never to the focused pane.
    /// Only Esc and the defer key are the inbox's own; everything else is the
    /// operator answering the agent, which is the entire point of the mode.
    pub(crate) async fn handle_inbox_key(
        &mut self,
        key: crate::input::TerminalKey,
    ) -> Option<super::TerminalInputTarget> {
        let event = key.as_key_event();
        if event.code == KeyCode::Esc && event.modifiers.is_empty() {
            self.state.clear_inbox();
            self.state.mode = Mode::Terminal;
            return None;
        }
        let queue = self.state.blocked_agents();
        if event.code == KeyCode::Tab && event.modifiers.is_empty() {
            self.defer_current_inbox_agent(&queue);
            return None;
        }
        let target = super::TerminalInputTarget::new(self.inbox_target(&queue)?);
        self.forward_terminal_key_to_target(&target, key).await;
        Some(target)
    }

    /// Headless mirror. Returns whether the inbox consumed the key.
    pub(crate) fn handle_inbox_key_headless(&mut self, key: KeyEvent) -> bool {
        if self.state.inbox.is_none() {
            return false;
        }
        if key.code == KeyCode::Esc && key.modifiers.is_empty() {
            self.state.clear_inbox();
            self.state.mode = Mode::Terminal;
            return true;
        }
        let queue = self.state.blocked_agents();
        if key.code == KeyCode::Tab && key.modifiers.is_empty() {
            self.defer_current_inbox_agent(&queue);
        }
        true
    }

    fn inbox_target(
        &self,
        queue: &[crate::app::inbox::BlockedAgent],
    ) -> Option<crate::terminal::TerminalId> {
        self.state
            .inbox
            .as_ref()?
            .current(queue)
            .map(|agent| agent.terminal_id.clone())
    }

    fn defer_current_inbox_agent(&mut self, queue: &[crate::app::inbox::BlockedAgent]) {
        let Some(pane_id) = self
            .state
            .inbox
            .as_ref()
            .and_then(|inbox| inbox.current(queue))
            .map(|agent| agent.pane_id)
        else {
            return;
        };
        if let Some(inbox) = self.state.inbox.as_mut() {
            inbox.defer(pane_id, queue);
        }
    }

    /// A status-bar button does exactly what its keybinding does, so the two
    /// affordances can never drift into meaning different things.
    fn activate_status_button(&mut self, action: crate::app::state::StatusButtonAction) {
        use crate::app::state::StatusButtonAction;
        match action {
            StatusButtonAction::Home => self.state.toggle_home(),
            StatusButtonAction::Inbox => self.state.toggle_inbox(),
            StatusButtonAction::Scratchpad => self.state.show_scratchpad_tab(),
            StatusButtonAction::Dock => {
                self.state.dock_collapsed = !self.state.dock_collapsed;
            }
        }
    }

    pub(crate) fn handle_symphony_key(&mut self, key: KeyEvent) -> bool {
        let Some(detail) = self.state.symphony_detail.as_mut() else {
            return false;
        };
        match key.code {
            KeyCode::Esc if key.modifiers.is_empty() => {
                self.state.clear_symphony();
                self.state.mode = Mode::Terminal;
            }
            KeyCode::Up | KeyCode::Char('k') if key.modifiers.is_empty() => {
                detail.selected = detail.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') if key.modifiers.is_empty() => {
                detail.selected =
                    (detail.selected + 1).min(detail.snapshot.workflows.len().saturating_sub(1));
            }
            KeyCode::Enter if key.modifiers.is_empty() => self.open_selected_symphony_workflow(),
            _ => {}
        }
        true
    }

    fn open_selected_symphony_workflow(&mut self) {
        let Some(workflow) = self
            .state
            .symphony_detail
            .as_ref()
            .and_then(|detail| detail.snapshot.workflows.get(detail.selected))
            .cloned()
        else {
            return;
        };
        let Some(repo) = workflow.repo.as_deref() else {
            self.state.config_diagnostic =
                Some("Symphony workflow has no repository checkout".to_string());
            return;
        };
        let mut verification_error = None;
        let workspace_match =
            self.state
                .workspaces
                .iter()
                .enumerate()
                .find_map(|(index, workspace)| {
                    let cwd = workspace.resolved_identity_cwd_from(
                        &self.state.terminals,
                        &self.terminal_runtimes,
                    )?;
                    match crate::symphony::checkout_matches_repo(&cwd, repo) {
                        Ok(()) => Some((index, cwd)),
                        Err(error) => {
                            verification_error.get_or_insert(error);
                            None
                        }
                    }
                });
        let (workspace_id, cwd) = if let Some((index, cwd)) = workspace_match {
            (Some(self.public_workspace_id(index)), Some(cwd))
        } else {
            match crate::symphony::common_checkout(repo) {
                Ok(cwd) => (None, cwd),
                Err(error) => {
                    verification_error.get_or_insert(error);
                    (None, None)
                }
            }
        };
        let Some(cwd) = cwd else {
            self.state.config_diagnostic = Some(
                verification_error
                    .unwrap_or_else(|| format!("Symphony checkout unavailable for {repo}")),
            );
            return;
        };
        self.runtime_tab_create(
            "tui.symphony.workflow.open",
            crate::api::schema::TabCreateParams {
                workspace_id,
                cwd: Some(cwd.to_string_lossy().into_owned()),
                focus: true,
                label: workflow
                    .ticket
                    .clone()
                    .or_else(|| Some(workflow.name.clone())),
                env: crate::symphony::launch_env(&workflow),
            },
        );
        self.state.clear_symphony();
        self.state.mode = Mode::Terminal;
    }

    pub(crate) fn handle_text_commit_headless(&mut self, text: &str) {
        if text.is_empty() || self.state.symphony_detail.is_some() {
            return;
        }
        if self.state.popup_pane.is_some() {
            if let Some(runtime) = self.popup_runtime() {
                let _ = runtime.try_send_bytes(Bytes::copy_from_slice(text.as_bytes()));
            } else {
                self.close_popup_pane();
            }
            return;
        }
        if self.state.mode != Mode::Terminal {
            self.paste_into_active_text_input(text);
            return;
        }

        self.state.clear_selection();
        self.selection_autoscroll_deadline = None;
        self.state.update_dismissed = true;
        if let Some(ws_idx) = self.state.active {
            let pane_id = self
                .state
                .workspaces
                .get(ws_idx)
                .and_then(|workspace| workspace.focused_pane_id());
            let sent = self
                .state
                .focused_runtime_in_workspace(&self.terminal_runtimes, ws_idx)
                .is_some_and(|runtime| {
                    runtime
                        .try_send_bytes(Bytes::copy_from_slice(text.as_bytes()))
                        .is_ok()
                });
            if let (true, Some(pane_id)) = (sent, pane_id) {
                self.retire_blocked_hook_authority_for_pane(pane_id, std::time::Instant::now());
            }
        }
    }

    pub(super) async fn handle_text_commit(&mut self, text: String) {
        if text.is_empty() || self.state.symphony_detail.is_some() {
            return;
        }
        if self.state.popup_pane.is_some() {
            if let Some(runtime) = self.popup_runtime() {
                let _ = runtime.send_bytes(Bytes::from(text)).await;
            } else {
                self.close_popup_pane();
            }
            return;
        }
        if self.state.mode != Mode::Terminal {
            self.paste_into_active_text_input(&text);
            return;
        }

        self.state.clear_selection();
        self.selection_autoscroll_deadline = None;
        self.state.update_dismissed = true;
        if let Some(ws_idx) = self.state.active {
            let pane_id = self
                .state
                .workspaces
                .get(ws_idx)
                .and_then(|workspace| workspace.focused_pane_id());
            let sent = if let Some(runtime) = self
                .state
                .focused_runtime_in_workspace(&self.terminal_runtimes, ws_idx)
            {
                runtime.send_bytes(Bytes::from(text)).await.is_ok()
            } else {
                false
            };
            if let (true, Some(pane_id)) = (sent, pane_id) {
                self.retire_blocked_hook_authority_for_pane(pane_id, std::time::Instant::now());
            }
        }
    }

    pub(super) async fn handle_paste(&mut self, text: String) {
        if self.state.symphony_detail.is_some() {
            return;
        }
        if self.state.popup_pane.is_some() {
            if let Some(runtime) = self.popup_runtime() {
                let _ = runtime.send_paste(text).await;
            } else {
                self.close_popup_pane();
            }
            return;
        }
        if self.state.mode != Mode::Terminal {
            self.paste_into_active_text_input(&text);
            return;
        }

        if let Some(runtime) = self.dock_editor_runtime() {
            let _ = runtime.send_paste(text).await;
            return;
        }

        if let Some(ws_idx) = self.state.active {
            let pane_id = self
                .state
                .workspaces
                .get(ws_idx)
                .and_then(|workspace| workspace.focused_pane_id());
            let has_text = !text.is_empty();
            let sent = if let Some(runtime) = self
                .state
                .focused_runtime_in_workspace(&self.terminal_runtimes, ws_idx)
            {
                runtime.send_paste(text).await.is_ok()
            } else {
                false
            };
            if let (true, Some(pane_id)) = (sent && has_text, pane_id) {
                self.retire_blocked_hook_authority_for_pane(pane_id, std::time::Instant::now());
            }
        }
    }

    pub(crate) fn paste_into_active_text_input(&mut self, text: &str) -> bool {
        match self.state.mode {
            Mode::RenameWorkspace | Mode::RenameTab | Mode::RenamePane => {
                insert_rename_input_text(&mut self.state, text);
                true
            }
            Mode::NewLinkedWorktree => {
                self.insert_worktree_create_text(text);
                true
            }
            Mode::OpenExistingWorktree => {
                if !self
                    .state
                    .worktree_open
                    .as_ref()
                    .is_some_and(|open| open.search_focused)
                {
                    return false;
                }
                self.insert_worktree_open_search_text(text);
                true
            }
            Mode::Navigator => {
                if !self.state.navigator.search_focused {
                    return false;
                }
                insert_navigator_search_text(&mut self.state, &self.terminal_runtimes, text);
                true
            }
            Mode::KeybindHelp => {
                if !self.state.keybind_help.search_focused {
                    return false;
                }
                insert_keybind_help_query_text(&mut self.state, text);
                true
            }
            Mode::Copy => {
                let Some(prompt) = self
                    .state
                    .copy_mode
                    .as_mut()
                    .and_then(|copy_mode| copy_mode.search.prompt.as_mut())
                else {
                    return false;
                };
                prompt
                    .query
                    .extend(text.chars().filter(|ch| !ch.is_control()));
                true
            }
            Mode::WorkLinkPicker => false,
            _ => false,
        }
    }

    pub(crate) fn handle_onboarding_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Right | KeyCode::Char('l') => self.open_settings_from_onboarding(),
            _ => {
                if let Some(ModalAction::Continue) =
                    modal_action_from_key(&key, ONBOARDING_WELCOME_ACTIONS)
                {
                    self.open_settings_from_onboarding();
                }
            }
        }
    }

    pub(crate) fn handle_release_notes_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.scroll_release_notes(-1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_release_notes(1),
            KeyCode::PageUp => self.scroll_release_notes(-8),
            KeyCode::PageDown => self.scroll_release_notes(8),
            KeyCode::Home => {
                if let Some(notes) = &mut self.state.release_notes {
                    notes.scroll = 0;
                }
            }
            KeyCode::End => {
                let max_scroll = self.state.release_notes_max_scroll();
                if let Some(notes) = &mut self.state.release_notes {
                    notes.scroll = max_scroll;
                }
            }
            _ => {
                if let Some(ModalAction::Close) = modal_action_from_key(&key, RELEASE_NOTES_ACTIONS)
                {
                    self.dismiss_release_notes();
                }
            }
        }
    }

    pub(crate) fn handle_product_announcement_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.scroll_product_announcement(-1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_product_announcement(1),
            KeyCode::PageUp => self.scroll_product_announcement(-8),
            KeyCode::PageDown => self.scroll_product_announcement(8),
            KeyCode::Home => {
                if let Some(announcement) = &mut self.state.product_announcement {
                    announcement.scroll = 0;
                }
            }
            KeyCode::End => {
                let max_scroll = self.state.product_announcement_max_scroll();
                if let Some(announcement) = &mut self.state.product_announcement {
                    announcement.scroll = max_scroll;
                }
            }
            _ => {
                if let Some(ModalAction::Close) = modal_action_from_key(&key, RELEASE_NOTES_ACTIONS)
                {
                    self.dismiss_product_announcement();
                }
            }
        }
    }

    pub(super) fn handle_mouse(&mut self, mouse: MouseEvent) {
        self.handle_mouse_from_input_source(super::LOCAL_INPUT_SOURCE, mouse);
    }

    pub(super) fn handle_mouse_from_input_source(
        &mut self,
        source_id: super::InputSourceId,
        mouse: MouseEvent,
    ) {
        if self.state.symphony_detail.is_some() {
            return;
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.pending_url_click_sources.remove(&source_id);
            }
            MouseEventKind::Drag(MouseButton::Left)
                if self.pending_url_click_sources.contains(&source_id) =>
            {
                return;
            }
            MouseEventKind::Up(MouseButton::Left)
                if self.pending_url_click_sources.remove(&source_id) =>
            {
                return;
            }
            _ => {}
        }

        if self.state.popup_pane.is_some() {
            self.handle_popup_mouse(mouse);
            return;
        }
        if self.handle_overlay_mouse(mouse) {
            return;
        }

        if matches!(self.state.mode, Mode::Terminal | Mode::Navigate)
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        {
            if let Some(copy_value) = self
                .state
                .view
                .info_panel_link_rows
                .iter()
                .find(|row| {
                    mouse.column >= row.rect.x
                        && mouse.column < row.rect.x.saturating_add(row.rect.width)
                        && mouse.row >= row.rect.y
                        && mouse.row < row.rect.y.saturating_add(row.rect.height)
                })
                .map(|row| row.copy_value.clone())
            {
                if self
                    .event_tx
                    .try_send(crate::events::AppEvent::ClipboardWrite {
                        content: copy_value.into_bytes(),
                    })
                    .is_err()
                {
                    self.show_work_link_notice("could not copy work link");
                } else {
                    self.show_work_link_notice("copied");
                }
                return;
            }

            if let Some(action) = self
                .state
                .view
                .status_buttons
                .iter()
                .find(|button| {
                    mouse.column >= button.rect.x
                        && mouse.column < button.rect.x.saturating_add(button.rect.width)
                        && mouse.row >= button.rect.y
                        && mouse.row < button.rect.y.saturating_add(button.rect.height)
                })
                .map(|button| button.action)
            {
                self.activate_status_button(action);
                return;
            }

            // Scratchpad rows open rather than copy: the note is being read, and
            // the reason a link is in it is to be followed.
            if let Some(url) = self
                .state
                .view
                .scratchpad_link_rows
                .iter()
                .find(|row| {
                    mouse.column >= row.rect.x
                        && mouse.column < row.rect.x.saturating_add(row.rect.width)
                        && mouse.row >= row.rect.y
                        && mouse.row < row.rect.y.saturating_add(row.rect.height)
                })
                .map(|row| row.url.clone())
            {
                if let Err(error) = crate::platform::open_url(&url) {
                    tracing::warn!(%error, %url, "failed to open scratchpad link");
                    self.show_work_link_notice("could not open link");
                }
                return;
            }
        }

        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && self.state.on_sidebar_divider(mouse.column, mouse.row)
        {
            let now = std::time::Instant::now();
            let is_double_click = self
                .last_sidebar_divider_click
                .is_some_and(|last| now.duration_since(last) <= super::SIDEBAR_DOUBLE_CLICK_WINDOW);
            self.last_sidebar_divider_click = Some(now);

            if is_double_click {
                self.state.sidebar_width = self.state.default_sidebar_width;
                self.state.sidebar_width_source =
                    crate::app::state::SidebarWidthSource::ConfigDefault;
                self.state.sidebar_width_auto = false;
                self.state.mark_session_dirty();
                self.state.drag = None;
                return;
            }
        }

        if self.handle_modified_url_click(source_id, mouse) {
            return;
        }

        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && self.state.on_dock_divider(mouse.column, mouse.row)
        {
            let now = std::time::Instant::now();
            let double_click = self.last_dock_divider_click.is_some_and(|previous| {
                now.duration_since(previous) <= super::SIDEBAR_DOUBLE_CLICK_WINDOW
            });
            self.last_dock_divider_click = Some(now);
            if double_click {
                self.state.set_dock_width(crate::ui::DOCK_DEFAULT_WIDTH);
                self.state.drag = None;
            } else {
                self.state.drag = Some(crate::app::state::DragState {
                    target: crate::app::state::DragTarget::DockDivider,
                });
            }
            return;
        }

        let handled_pane_double_click = self.handle_pane_double_click(mouse);
        if !handled_pane_double_click {
            self.focus_pane_before_mouse_press(mouse);
        }

        let previous_agent_panel_sort = self.state.agent_panel_sort;
        let previous_settings_section = self.state.settings.section;
        if !handled_pane_double_click {
            if let Some(action) = self.state.handle_mouse(&mut self.terminal_runtimes, mouse) {
                match action {
                    MouseAction::NewWorkspace => {
                        self.begin_tui_workspace_create("tui.mouse.workspace.create")
                    }
                    MouseAction::Settings(action) => match action {
                        SettingsAction::SaveTheme(name) => self.save_theme(&name),
                        SettingsAction::SaveStatusIndicators(style) => {
                            self.save_status_indicators(style)
                        }
                        SettingsAction::SaveSound(enabled) => self.save_sound(enabled),
                        SettingsAction::SaveToastDelivery(delivery) => {
                            self.save_toast_delivery(delivery)
                        }
                        SettingsAction::SaveAgentBorderLabels(enabled) => {
                            self.save_agent_border_labels(enabled)
                        }
                        SettingsAction::InstallRecommendedIntegrations => {
                            self.install_recommended_integrations()
                        }
                    },
                    MouseAction::FocusWorkspace { ws_idx } => {
                        self.focus_workspace_idx_via_api(ws_idx)
                    }
                    MouseAction::FocusTab { tab_idx } => self.focus_tab_idx_via_api(tab_idx),
                    MouseAction::FocusSidebarTab { ws_idx, tab_idx } => {
                        self.focus_workspace_tab_via_api(ws_idx, tab_idx)
                    }
                    MouseAction::ToggleSidebarTabPrio { ws_idx, tab_idx } => {
                        if let Some(changed) = self.state.apply_tab_prio(
                            ws_idx,
                            tab_idx,
                            crate::workspace::TabPrioAction::Toggle,
                        ) {
                            if changed {
                                self.schedule_session_save();
                                if self.no_session {
                                    self.state.mark_session_dirty();
                                }
                            }
                        }
                    }
                    MouseAction::FocusPane { ws_idx, pane_id } => {
                        self.focus_pane_internal_via_api(ws_idx, pane_id)
                    }
                    MouseAction::FocusToastTarget => self.focus_toast_target_via_api(),
                    MouseAction::MoveWorkspace {
                        source_ws_idx,
                        insert_idx,
                    } => self.move_workspace_via_api(source_ws_idx, insert_idx),
                    MouseAction::MoveWorkspaceBlock { params } => {
                        self.move_workspace_block_via_api(params)
                    }
                    MouseAction::MoveTab {
                        ws_idx,
                        source_tab_idx,
                        insert_idx,
                    } => self.move_tab_via_api(ws_idx, source_tab_idx, insert_idx),
                    MouseAction::SetSplitRatio { path, ratio } => {
                        self.set_split_ratio_via_api(path, ratio)
                    }
                    MouseAction::RenameModal(action) => {
                        self.apply_rename_mouse_action_via_api(action)
                    }
                    MouseAction::ConfirmCloseAccept => self.confirm_close_accept_via_api(),
                    MouseAction::ContextMenu { menu, idx } => {
                        self.apply_context_menu_action_via_api(menu, idx)
                    }
                }
            }
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
                && self
                    .state
                    .selection
                    .as_ref()
                    .is_none_or(crate::selection::Selection::is_in_progress)
            {
                self.selection_highlight_clear_deadline = None;
            }
        }
        if previous_settings_section != crate::app::state::SettingsSection::Integrations
            && self.state.settings.section == crate::app::state::SettingsSection::Integrations
        {
            self.refresh_integration_recommendations();
        }
        if self.state.agent_panel_sort != previous_agent_panel_sort {
            self.save_agent_panel_sort(self.state.agent_panel_sort);
        }

        self.dispatch_pending_clipboard_write();

        // Sync autoscroll deadline with state (mouse handler may have
        // set or cleared selection_autoscroll during handle_mouse).
        if self.state.selection_autoscroll.is_none() {
            self.selection_autoscroll_deadline = None;
        } else if self.selection_autoscroll_deadline.is_none() {
            self.selection_autoscroll_deadline =
                Some(std::time::Instant::now() + super::SELECTION_AUTOSCROLL_INTERVAL);
        }
    }

    fn handle_popup_mouse(&mut self, mouse: MouseEvent) {
        let Some((_outer, inner)) =
            crate::ui::popup_pane_rects(&self.state, self.state.view.terminal_area)
        else {
            return;
        };
        if mouse.column < inner.x
            || mouse.column >= inner.x.saturating_add(inner.width)
            || mouse.row < inner.y
            || mouse.row >= inner.y.saturating_add(inner.height)
        {
            return;
        }
        let Some(rt) = self.popup_runtime() else {
            self.close_popup_pane();
            return;
        };
        let column = mouse.column.saturating_sub(inner.x);
        let row = mouse.row.saturating_sub(inner.y);
        let bytes = match mouse.kind {
            MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight => match rt.wheel_routing() {
                Some(crate::pane::WheelRouting::MouseReport) => {
                    rt.encode_mouse_wheel(mouse.kind, column, row, mouse.modifiers)
                }
                Some(crate::pane::WheelRouting::AlternateScroll) => {
                    rt.encode_alternate_scroll(mouse.kind)
                }
                Some(crate::pane::WheelRouting::HostScroll) | None => {
                    let lines_per_notch = self.state.mouse_scroll_lines;
                    match mouse.kind {
                        MouseEventKind::ScrollUp => rt.scroll_up(lines_per_notch),
                        MouseEventKind::ScrollDown => rt.scroll_down(lines_per_notch),
                        _ => {}
                    }
                    return;
                }
            },
            MouseEventKind::Down(_) | MouseEventKind::Up(_) | MouseEventKind::Drag(_) => {
                rt.encode_mouse_button(mouse.kind, column, row, mouse.modifiers)
            }
            MouseEventKind::Moved => {
                rt.encode_mouse_motion(mouse.kind, column, row, mouse.modifiers)
            }
        };
        let Some(bytes) = bytes else {
            return;
        };
        if !matches!(mouse.kind, MouseEventKind::Moved) {
            rt.scroll_reset();
        }
        if let Err(err) = rt.try_send_bytes(Bytes::from(bytes)) {
            warn!(err = %err, kind = ?mouse.kind, "failed to forward popup mouse event");
        }
    }

    fn focus_pane_before_mouse_press(&mut self, mouse: MouseEvent) {
        if !matches!(self.state.mode, Mode::Terminal | Mode::Resize)
            || !matches!(
                mouse.kind,
                MouseEventKind::Down(MouseButton::Left | MouseButton::Middle)
            )
        {
            return;
        }

        let Some(pane_id) = self
            .state
            .pane_at(mouse.column, mouse.row)
            .map(|info| info.id)
        else {
            return;
        };
        let Some(ws_idx) = self.state.active else {
            return;
        };

        self.state.dock_editor_focused = false;
        // Focus through the runtime API before an application can consume its press.
        self.focus_pane_internal_via_api(ws_idx, pane_id);
    }

    fn handle_modified_url_click(
        &mut self,
        source_id: super::InputSourceId,
        mouse: MouseEvent,
    ) -> bool {
        if self.state.mode != Mode::Terminal
            || !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            || !mouse.modifiers.contains(modified_url_click_modifier())
        {
            return false;
        }

        let Some(info) = self.state.pane_at(mouse.column, mouse.row).cloned() else {
            return false;
        };
        let viewport_row = mouse.row.saturating_sub(info.inner_rect.y);
        let col = mouse.column.saturating_sub(info.inner_rect.x);
        let Some(url) =
            self.state
                .url_at_pane_cell(&self.terminal_runtimes, info.id, viewport_row, col)
        else {
            return false;
        };

        self.last_pane_click = None;
        self.pending_url_click_sources.insert(source_id);
        match self.invoke_plugin_link_handler_for_url(&url, info.id) {
            Ok(true) => return true,
            Ok(false) => {}
            Err(err) => {
                tracing::warn!(err = %err, url = %url, "failed to invoke plugin link handler");
            }
        }
        if let Err(err) = crate::platform::open_url(&url) {
            tracing::warn!(err = %err, url = %url, "failed to open pane URL");
        }
        true
    }

    fn handle_pane_double_click(&mut self, mouse: MouseEvent) -> bool {
        // A pane press stops being a double-click candidate once it becomes
        // a drag or completes as a real text selection.
        match mouse.kind {
            MouseEventKind::Drag(MouseButton::Left) => {
                self.last_pane_click = None;
                return false;
            }
            MouseEventKind::Up(MouseButton::Left)
                if self
                    .state
                    .selection
                    .as_ref()
                    .is_some_and(|selection| selection.is_visible()) =>
            {
                self.last_pane_click = None;
                return false;
            }
            _ => {}
        }

        // Only terminal-pane left-clicks can start this gesture; other clicks
        // should keep their existing mouse behavior and clear stale candidates.
        let Some(click) = self.pane_click_candidate(mouse) else {
            return false;
        };

        // Require the second click to land near the first click in the same pane
        // and within the double-click window so adjacent interactions do not select a word.
        if !self.take_pane_double_click(click) {
            return false;
        }

        self.select_double_clicked_word(click)
    }

    fn pane_click_candidate(&mut self, mouse: MouseEvent) -> Option<PaneClickState> {
        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return None;
        }

        if !mouse.modifiers.is_empty() {
            self.last_pane_click = None;
            return None;
        }

        if self.state.mode != Mode::Terminal {
            self.last_pane_click = None;
            return None;
        }

        let Some(info) = self.state.pane_at(mouse.column, mouse.row).cloned() else {
            self.last_pane_click = None;
            return None;
        };

        Some(PaneClickState {
            pane_id: info.id,
            viewport_row: mouse.row - info.inner_rect.y,
            col: mouse.column - info.inner_rect.x,
            at: std::time::Instant::now(),
        })
    }

    fn take_pane_double_click(&mut self, click: PaneClickState) -> bool {
        if !self
            .last_pane_click
            .is_some_and(|last| last.is_double_click_for(click))
        {
            self.last_pane_click = Some(click);
            return false;
        }

        self.last_pane_click = None;
        true
    }

    fn select_double_clicked_word(&mut self, click: PaneClickState) -> bool {
        let selected = self.state.select_word_at_pane_cell(
            &self.terminal_runtimes,
            click.pane_id,
            click.viewport_row,
            click.col,
        );
        if selected {
            self.selection_highlight_clear_deadline = self
                .state
                .copy_on_select
                .then(|| std::time::Instant::now() + super::PANE_COPY_HIGHLIGHT_DURATION);
        }
        selected
    }
}

pub(crate) fn is_modal_paste_shortcut(key: &KeyEvent) -> bool {
    if !matches!(key.code, KeyCode::Char('v' | 'V')) {
        return false;
    }

    #[cfg(target_os = "macos")]
    {
        key.modifiers.contains(KeyModifiers::SUPER) || key.modifiers.contains(KeyModifiers::CONTROL)
    }

    #[cfg(not(target_os = "macos"))]
    {
        key.modifiers.contains(KeyModifiers::CONTROL)
    }
}

pub(crate) fn modal_paste_target_active(state: &AppState) -> bool {
    match state.mode {
        Mode::RenameWorkspace | Mode::RenameTab | Mode::RenamePane | Mode::NewLinkedWorktree => {
            true
        }
        Mode::OpenExistingWorktree => state
            .worktree_open
            .as_ref()
            .is_some_and(|open| open.search_focused),
        Mode::Navigator => state.navigator.search_focused,
        Mode::KeybindHelp => state.keybind_help.search_focused,
        Mode::Copy => state
            .copy_mode
            .as_ref()
            .is_some_and(|copy_mode| copy_mode.search.prompt.is_some()),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Mouse handling
// ---------------------------------------------------------------------------

// Note: split_pane needs runtime (event_tx for PTY spawn), so it lives on App
impl AppState {
    #[cfg(test)]
    pub(crate) fn split_pane(
        &mut self,
        terminal_runtimes: &mut crate::terminal::TerminalRuntimeRegistry,
        direction: Direction,
    ) {
        self.split_pane_with_placement(terminal_runtimes, direction, false);
    }

    #[cfg(test)]
    pub(crate) fn split_pane_with_placement(
        &mut self,
        terminal_runtimes: &mut crate::terminal::TerminalRuntimeRegistry,
        direction: Direction,
        before: bool,
    ) {
        // Actual PTY spawning happens in Workspace::split_focused
        // which needs events channel — this is called from navigate_key
        // where we don't have async context, so the workspace handles it
        let (rows, cols) = self.estimate_pane_size();
        let new_rows = (rows / 2).max(4);
        let new_cols = (cols / 2).max(10);

        let follow_cwd = self
            .active
            .and_then(|i| self.workspaces.get(i))
            .and_then(|ws| {
                let tab = ws.active_tab()?;
                let terminal_id = tab.terminal_id(tab.layout.focused())?;
                super::creation::launch_cwd_for_terminal(
                    terminal_id,
                    &self.terminals,
                    terminal_runtimes,
                )
            });
        let cwd = Some(super::creation::resolve_new_terminal_cwd(
            &self.new_terminal_cwd,
            follow_cwd,
        ));

        let previous_focus = self.current_pane_focus_target();
        if let Some(ws_idx) = self.active {
            let Some(ws) = self.workspaces.get_mut(ws_idx) else {
                return;
            };
            let split = if before {
                ws.split_focused_with_placement(
                    direction,
                    true,
                    new_rows,
                    new_cols,
                    cwd,
                    self.pane_scrollback_limit_bytes,
                    self.host_terminal_theme,
                    self.host_terminal_appearance,
                    crate::pane::PaneShellConfig::new(&self.default_shell, self.shell_mode),
                    Vec::new(),
                )
            } else {
                ws.split_focused(
                    direction,
                    new_rows,
                    new_cols,
                    cwd,
                    self.pane_scrollback_limit_bytes,
                    self.host_terminal_theme,
                    self.host_terminal_appearance,
                    crate::pane::PaneShellConfig::new(&self.default_shell, self.shell_mode),
                    Vec::new(),
                )
            };
            if let Ok(new_pane) = split {
                let new_id = new_pane.pane_id;
                terminal_runtimes.insert(new_pane.terminal.id.clone(), new_pane.runtime);
                self.remove_alias_shadowed_by_new_pane(new_id);
                self.terminals
                    .insert(new_pane.terminal.id.clone(), new_pane.terminal);
                self.record_pane_focus_change(previous_focus, ws_idx, new_id);
                self.mark_session_dirty();
                self.mode = Mode::Terminal;
            }
        }
    }
}

#[cfg(test)]
fn state_with_workspaces(names: &[&str]) -> AppState {
    let mut state = AppState::test_new();
    state.workspaces = names
        .iter()
        .map(|name| crate::workspace::Workspace::test_new(name))
        .collect();
    if !state.workspaces.is_empty() {
        state.active = Some(0);
        state.selected = 0;
        state.mode = Mode::Navigate;
    }
    state
}

#[cfg(test)]
fn app_for_mouse_test() -> App {
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        &crate::config::Config::default(),
        true,
        None,
        api_rx,
        crate::api::EventHub::default(),
    );
    app.state.mode = Mode::Terminal;
    app.state.update_available = None;
    app.state.latest_release_notes_available = false;
    app.state.view.sidebar_rect = ratatui::layout::Rect::new(0, 0, 26, 20);
    app.state.view.terminal_area = ratatui::layout::Rect::new(26, 0, 80, 20);
    app
}

#[cfg(test)]
fn mouse(
    kind: crossterm::event::MouseEventKind,
    col: u16,
    row: u16,
) -> crossterm::event::MouseEvent {
    crossterm::event::MouseEvent {
        kind,
        column: col,
        row,
        modifiers: crossterm::event::KeyModifiers::empty(),
    }
}

#[cfg(test)]
fn numbered_lines_bytes(count: usize) -> Vec<u8> {
    (0..count)
        .map(|i| format!("{i:06}\r\n"))
        .collect::<String>()
        .into_bytes()
}

#[cfg(test)]
fn capture_snapshot(state: &AppState) -> crate::persist::SessionSnapshot {
    let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
    crate::persist::capture(
        &state.workspaces,
        &state.terminals,
        &terminal_runtimes,
        state.active,
        state.selected,
        state.sidebar_width,
        state.sidebar_section_split,
        state.collapsed_space_keys.clone(),
        state.prio_panel_collapsed,
    )
}

#[cfg(test)]
fn root_layout_ratio(snapshot: &crate::persist::SessionSnapshot) -> Option<f32> {
    match &snapshot.workspaces.first()?.tabs.first()?.layout {
        crate::persist::LayoutSnapshot::Split { ratio, .. } => Some(*ratio),
        crate::persist::LayoutSnapshot::Pane(_) => None,
    }
}

#[cfg(test)]
fn unique_temp_path(name: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("herdr-{name}-{}-{nanos}", std::process::id()))
}

#[cfg(test)]
#[cfg(unix)]
fn wait_for_file(path: &std::path::Path) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if let Ok(content) = std::fs::read_to_string(path) {
            if !content.is_empty() {
                return content;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("timed out waiting for {}", path.display());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal_app_with_blocked_hook() -> (
        App,
        crate::terminal::TerminalId,
        tokio::sync::mpsc::Receiver<Bytes>,
    ) {
        let mut app = test_app();
        let mut workspace = crate::workspace::Workspace::test_new("test");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let (runtime, rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        workspace.insert_test_runtime(pane_id, runtime);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
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
        (app, terminal_id, rx)
    }

    fn assert_blocked_hook_retired(app: &App, terminal_id: &crate::terminal::TerminalId) {
        assert_eq!(
            app.state.terminals[terminal_id].state,
            crate::detect::AgentState::Idle
        );
        assert!(!app.state.terminals[terminal_id].full_lifecycle_hook_authority_active());
    }

    fn test_app() -> App {
        App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        )
    }

    fn app_with_blocked_home_rows(row_count: usize) -> (App, Vec<crate::layout::PaneId>) {
        assert!(row_count > 0);
        let mut app = test_app();
        let mut workspace = crate::workspace::Workspace::test_new("home");
        let mut pane_ids = vec![workspace.tabs[0].root_pane];
        for _ in 1..row_count {
            pane_ids.push(workspace.test_split(Direction::Horizontal));
        }
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        for pane_id in &pane_ids {
            let terminal_id = app.state.workspaces[0]
                .terminal_id(*pane_id)
                .expect("test pane terminal")
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .expect("test terminal state")
                .state = crate::detect::AgentState::Blocked;
        }
        app.state.toggle_home();
        (app, pane_ids)
    }

    #[tokio::test]
    async fn clicking_a_home_row_selects_it_and_jumps_to_that_pane() {
        let (mut app, pane_ids) = app_with_blocked_home_rows(3);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 120, 40));

        let hits = app.state.view.home_row_hit_areas.clone();
        assert_eq!(hits.len(), 3, "every blocked row should be clickable");

        // Click the second row rather than the first, so a jump proves the
        // click chose the row instead of the cursor happening to be there.
        let (index, rect) = hits[1];
        let queue = app.state.blocked_agents();
        let target = queue[index].pane_id;
        assert_ne!(target, pane_ids[0]);

        app.handle_raw_input_event(crate::raw_input::RawInputEvent::Mouse(
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: rect.x + 1,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            },
        ))
        .await;

        assert!(app.state.home.is_none(), "a jump leaves home");
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(target));
    }

    #[tokio::test]
    async fn clicking_off_the_home_rows_neither_jumps_nor_closes_home() {
        let (mut app, _pane_ids) = app_with_blocked_home_rows(2);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 120, 40));
        let before = app.state.workspaces[0].focused_pane_id();

        // The hint row at the bottom of the home frame is not a row.
        let terminal_area = app.state.view.terminal_area;
        app.handle_raw_input_event(crate::raw_input::RawInputEvent::Mouse(
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: terminal_area.x + 1,
                row: terminal_area.bottom() - 1,
                modifiers: KeyModifiers::NONE,
            },
        ))
        .await;

        assert!(app.state.home.is_some());
        // Home covers the panes, so the click must not reach the one behind it.
        assert_eq!(app.state.workspaces[0].focused_pane_id(), before);
    }

    #[test]
    fn home_row_hit_areas_are_dropped_as_soon_as_home_closes() {
        let (mut app, _pane_ids) = app_with_blocked_home_rows(2);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 120, 40));
        assert!(!app.state.view.home_row_hit_areas.is_empty());

        app.state.clear_home();
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 120, 40));

        assert!(app.state.view.home_row_hit_areas.is_empty());
        assert_eq!(app.state.home_row_at(30, 3), None);
    }

    #[test]
    fn opening_home_and_inbox_closes_the_other_overlay() {
        let mut app = test_app();

        app.state.toggle_inbox();
        app.state.toggle_home();
        assert!(app.state.home.is_some());
        assert!(app.state.inbox.is_none());

        app.state.toggle_inbox();
        assert!(app.state.home.is_none());
        assert!(app.state.inbox.is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pressing_enter_after_the_selected_home_pane_closes_leaves_home_open() {
        let (mut app, pane_ids) = app_with_blocked_home_rows(1);
        app.state.workspaces[0].tabs[0].panes.remove(&pane_ids[0]);

        app.handle_key(TerminalKey::new(KeyCode::Enter, KeyModifiers::empty()))
            .await;

        assert!(app.state.home.is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pressing_enter_on_an_existing_home_pane_closes_home() {
        let (mut app, _pane_ids) = app_with_blocked_home_rows(1);

        app.handle_key(TerminalKey::new(KeyCode::Enter, KeyModifiers::empty()))
            .await;

        assert!(app.state.home.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pressing_escape_closes_the_home_overlay() {
        let (mut app, _pane_ids) = app_with_blocked_home_rows(1);

        app.handle_key(TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()))
            .await;

        assert!(app.state.home.is_none());
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn home_cursor_moves_with_vi_and_arrow_keys_without_wrapping() {
        let (mut app, _pane_ids) = app_with_blocked_home_rows(3);
        let selected = |app: &App| {
            app.state
                .home
                .as_ref()
                .expect("home overlay")
                .selected(&app.state.blocked_agents())
        };

        assert_eq!(selected(&app), 0);
        app.handle_key(TerminalKey::new(KeyCode::Char('j'), KeyModifiers::empty()))
            .await;
        assert_eq!(selected(&app), 1);
        app.handle_key(TerminalKey::new(KeyCode::Down, KeyModifiers::empty()))
            .await;
        assert_eq!(selected(&app), 2);
        app.handle_key(TerminalKey::new(KeyCode::Down, KeyModifiers::empty()))
            .await;
        assert_eq!(selected(&app), 2);
        app.handle_key(TerminalKey::new(KeyCode::Char('k'), KeyModifiers::empty()))
            .await;
        assert_eq!(selected(&app), 1);
        app.handle_key(TerminalKey::new(KeyCode::Up, KeyModifiers::empty()))
            .await;
        assert_eq!(selected(&app), 0);
        app.handle_key(TerminalKey::new(KeyCode::Up, KeyModifiers::empty()))
            .await;
        assert_eq!(selected(&app), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unrecognized_home_keys_are_swallowed_instead_of_forwarded_to_the_pane() {
        let (mut app, _terminal_id, mut rx) = terminal_app_with_blocked_hook();
        app.state.toggle_home();

        let target = app
            .handle_key(TerminalKey::new(KeyCode::Char('x'), KeyModifiers::empty()))
            .await;

        assert!(target.is_none());
        assert!(rx.try_recv().is_err());
        assert!(app.state.home.is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn headless_home_keys_are_swallowed_before_the_terminal_input_path() {
        let (mut app, _terminal_id, mut rx) = terminal_app_with_blocked_hook();
        app.state.toggle_home();

        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Key(TerminalKey::new(
                KeyCode::Char('x'),
                KeyModifiers::empty(),
            ))],
            false,
        );

        assert!(rx.try_recv().is_err());
        assert!(app.state.home.is_some());
    }

    #[tokio::test]
    async fn paste_routes_to_rename_modal_input() {
        let mut app = test_app();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::RenameTab;
        app.state.name_input = "2".into();
        app.state.name_input_replace_on_type = true;

        app.handle_paste("feature/logs".into()).await;

        assert_eq!(app.state.name_input, "feature/logs");
        assert!(!app.state.name_input_replace_on_type);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn paste_retires_blocked_hook_authority_after_forwarding() {
        let (mut app, terminal_id, mut rx) = terminal_app_with_blocked_hook();

        app.handle_paste("continue".into()).await;

        assert!(rx.try_recv().is_ok());
        assert_blocked_hook_retired(&app, &terminal_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn text_commit_paths_retire_blocked_hook_authority_after_forwarding() {
        let (mut app, terminal_id, mut rx) = terminal_app_with_blocked_hook();
        app.handle_text_commit("continue".into()).await;
        assert!(rx.try_recv().is_ok());
        assert_blocked_hook_retired(&app, &terminal_id);

        let (mut app, terminal_id, mut rx) = terminal_app_with_blocked_hook();
        app.handle_text_commit_headless("continue");
        assert!(rx.try_recv().is_ok());
        assert_blocked_hook_retired(&app, &terminal_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn symphony_overlay_blocks_text_paste_and_headless_keys() {
        let (mut app, _terminal_id, mut rx) = terminal_app_with_blocked_hook();
        app.state.toggle_symphony();

        app.handle_text_commit("hidden command\n".into()).await;
        app.handle_text_commit_headless("hidden command\n");
        app.handle_paste("hidden command\n".into()).await;
        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Paste(
                "hidden command\n".into(),
            )],
            false,
        );
        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Key(TerminalKey::new(
                KeyCode::Char('x'),
                KeyModifiers::empty(),
            ))],
            false,
        );

        assert!(rx.try_recv().is_err());
        assert!(app.state.symphony_detail.is_some());
    }

    #[tokio::test]
    async fn paste_routes_to_keybind_help_query_only_when_searching() {
        let mut app = test_app();
        app.state.mode = Mode::KeybindHelp;
        app.handle_paste("ignored".into()).await;
        assert!(app.state.keybind_help.query.is_empty());

        app.state.keybind_help.search_focused = true;
        app.state.keybind_help.scroll = 3;
        app.handle_paste("work\nspace".into()).await;

        assert_eq!(app.state.keybind_help.query, "workspace");
        assert_eq!(app.state.keybind_help.scroll, 0);
    }

    #[tokio::test]
    async fn paste_routes_to_new_linked_worktree_input() {
        let mut app = test_app();
        app.state.mode = Mode::NewLinkedWorktree;
        app.state.name_input = "generated-branch".into();
        app.state.name_input_replace_on_type = true;
        app.state.worktree_create = Some(crate::app::state::WorktreeCreateState {
            source_workspace_id: "source".into(),
            source_checkout_path: "/repo/herdr".into(),
            source_existing_membership: None,
            source_repo_root: "/repo/herdr".into(),
            repo_key: "repo-key".into(),
            repo_name: "herdr".into(),
            branch: "generated-branch".into(),
            checkout_path: "/repo/herdr-generated-branch".into(),
            error: None,
            creating: false,
        });

        app.handle_paste("feature/linear-302".into()).await;

        assert_eq!(app.state.name_input, "feature/linear-302");
        assert_eq!(
            app.state
                .worktree_create
                .as_ref()
                .map(|create| create.branch.as_str()),
            Some("feature/linear-302")
        );
    }

    #[test]
    fn modal_paste_shortcut_matches_platform_primary_v() {
        #[cfg(target_os = "macos")]
        let modifiers = KeyModifiers::SUPER;
        #[cfg(not(target_os = "macos"))]
        let modifiers = KeyModifiers::CONTROL;

        assert!(is_modal_paste_shortcut(&KeyEvent::new(
            KeyCode::Char('v'),
            modifiers
        )));
        assert!(is_modal_paste_shortcut(&KeyEvent::new(
            KeyCode::Char('V'),
            modifiers | KeyModifiers::SHIFT
        )));
        assert!(!is_modal_paste_shortcut(&KeyEvent::new(
            KeyCode::Char('v'),
            KeyModifiers::ALT
        )));
    }

    #[test]
    fn modal_paste_target_is_active_only_for_text_inputs() {
        let mut state = AppState::test_new();

        state.mode = Mode::RenameTab;
        assert!(modal_paste_target_active(&state));

        state.mode = Mode::Navigator;
        state.navigator.search_focused = false;
        assert!(!modal_paste_target_active(&state));
        state.navigator.search_focused = true;
        assert!(modal_paste_target_active(&state));

        state.mode = Mode::KeybindHelp;
        state.keybind_help.search_focused = false;
        assert!(!modal_paste_target_active(&state));
        state.keybind_help.search_focused = true;
        assert!(modal_paste_target_active(&state));

        state.mode = Mode::ConfirmClose;
        assert!(!modal_paste_target_active(&state));
    }
}
