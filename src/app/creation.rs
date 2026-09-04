use std::path::PathBuf;

use crate::api::schema::{EventData, EventEnvelope, EventKind};
#[cfg(test)]
use tracing::error;

use super::{
    api_helpers::{pane_agent_status, pane_agent_status_with_stale, tab_attention_priority},
    App, Mode,
};
use crate::{config::NewTerminalCwdConfig, workspace::Workspace};

pub(crate) fn resolve_new_terminal_cwd(
    policy: &NewTerminalCwdConfig,
    follow_cwd: Option<PathBuf>,
) -> PathBuf {
    match policy {
        NewTerminalCwdConfig::Follow => follow_cwd
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/")),
        NewTerminalCwdConfig::Home => std::env::var_os("HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/")),
        NewTerminalCwdConfig::Current => {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
        }
        NewTerminalCwdConfig::Path(path) => crate::worktree::expand_tilde_path(path),
    }
}

pub(super) fn launch_cwd_for_terminal(
    terminal_id: &crate::terminal::TerminalId,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
) -> Option<PathBuf> {
    terminal_runtimes
        .get(terminal_id)
        .and_then(|runtime| runtime.follow_cwd())
        .or_else(|| {
            terminals
                .get(terminal_id)
                .map(|terminal| terminal.cwd.clone())
        })
}

impl App {
    pub(super) fn prepare_spawn_work_context(
        &self,
        context: Option<crate::work_context::PaneWorkContext>,
    ) -> Result<Option<crate::work_context::PaneWorkContext>, String> {
        let Some(mut context) = context else {
            return Ok(None);
        };
        context = context.normalized_spawn_binding()?;
        let Some(pr_url) = context.primary_pr() else {
            return Ok(Some(context));
        };
        if context.active_owner
            && self.state.workspaces.iter().any(|workspace| {
                workspace.tabs.iter().any(|tab| {
                    tab.panes.values().any(|pane| {
                        self.state
                            .terminals
                            .get(&pane.attached_terminal_id)
                            .map(crate::terminal::TerminalState::effective_work_context)
                            .is_some_and(|existing| existing.is_active_owner_of(pr_url))
                    })
                })
            })
        {
            context.active_owner = false;
        }
        Ok(Some(context))
    }

    pub(super) fn bind_spawn_work_context(
        terminal: &mut crate::terminal::TerminalState,
        context: Option<crate::work_context::PaneWorkContext>,
    ) {
        if let Some(context) = context {
            terminal.replace_prevalidated_manual_work_context(context);
        }
    }

    pub(super) fn bind_workspace_root_work_context(
        &mut self,
        ws_idx: usize,
        context: Option<crate::work_context::PaneWorkContext>,
    ) {
        let terminal_id = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.tabs.first())
            .and_then(|tab| tab.terminal_id(tab.root_pane))
            .cloned();
        if let Some(terminal) =
            terminal_id.and_then(|terminal_id| self.state.terminals.get_mut(&terminal_id))
        {
            Self::bind_spawn_work_context(terminal, context);
        }
    }

    pub(super) fn orphan_pane_work_owner(
        &mut self,
        pane_id: crate::layout::PaneId,
    ) -> Option<usize> {
        let (ws_idx, pane) = self.find_pane(pane_id)?;
        let terminal_id = pane.attached_terminal_id.clone();
        self.state
            .terminals
            .get_mut(&terminal_id)?
            .clear_active_work_owner()
            .then_some(ws_idx)
    }

    pub(super) fn seed_cwd_from_workspace(&self, ws_idx: usize) -> Option<PathBuf> {
        self.state
            .workspaces
            .get(ws_idx)?
            .resolved_identity_cwd_from(&self.state.terminals, &self.terminal_runtimes)
    }

    pub(super) fn launch_cwd_for_pane_in_workspace(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<PathBuf> {
        let workspace = self.state.workspaces.get(ws_idx)?;
        let tab = workspace
            .tabs
            .get(workspace.find_tab_index_for_pane(pane_id)?)?;
        launch_cwd_for_terminal(
            tab.terminal_id(pane_id)?,
            &self.state.terminals,
            &self.terminal_runtimes,
        )
    }

    pub(super) fn focused_pane_cwd_in_workspace(&self, ws_idx: usize) -> Option<PathBuf> {
        let pane_id = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
        self.launch_cwd_for_pane_in_workspace(ws_idx, pane_id)
    }

    pub(super) fn resolve_new_terminal_cwd(&self, follow_cwd: Option<PathBuf>) -> PathBuf {
        resolve_new_terminal_cwd(&self.state.new_terminal_cwd, follow_cwd)
    }

    pub(super) fn workspace_creation_source(&self) -> Option<usize> {
        if self.state.mode == Mode::Navigate
            && self.state.workspaces.get(self.state.selected).is_some()
        {
            return Some(self.state.selected);
        }

        self.state.active.or_else(|| {
            self.state
                .workspaces
                .get(self.state.selected)
                .map(|_| self.state.selected)
        })
    }

    pub(super) fn begin_tui_workspace_create(&mut self, request_id: &'static str) {
        if self.state.prompt_new_workspace_name {
            let follow_cwd = self.workspace_creation_source().and_then(|ws_idx| {
                self.focused_pane_cwd_in_workspace(ws_idx)
                    .or_else(|| self.seed_cwd_from_workspace(ws_idx))
            });
            let cwd = self.resolve_new_terminal_cwd(follow_cwd);
            super::input::open_new_workspace_dialog(&mut self.state, cwd);
            return;
        }

        self.runtime_workspace_create(
            request_id,
            crate::api::schema::WorkspaceCreateParams {
                cwd: None,
                focus: true,
                label: None,
                env: Default::default(),
                work_context: None,
            },
        );
        self.state.mode = if self.state.active.is_some() {
            Mode::Terminal
        } else {
            Mode::Navigate
        };
    }

    /// Create a workspace with a real PTY (needs event_tx).
    #[cfg(test)]
    pub(crate) fn create_workspace(&mut self) {
        let follow_cwd = self.workspace_creation_source().and_then(|ws_idx| {
            self.focused_pane_cwd_in_workspace(ws_idx)
                .or_else(|| self.seed_cwd_from_workspace(ws_idx))
        });
        let initial_cwd = self.resolve_new_terminal_cwd(follow_cwd);
        if let Err(e) = self.create_workspace_with_events(initial_cwd, true) {
            error!(err = %e, "failed to create workspace");
            self.state.mode = Mode::Navigate;
        }
    }

    #[cfg(test)]
    pub(crate) fn create_tab(&mut self) {
        let custom_name = self.state.requested_new_tab_name.take();
        let active_before = self.state.active;
        let follow_cwd = self.state.active.and_then(|ws_idx| {
            self.focused_pane_cwd_in_workspace(ws_idx)
                .or_else(|| self.seed_cwd_from_workspace(ws_idx))
        });
        let initial_cwd = self.resolve_new_terminal_cwd(follow_cwd);
        match self.create_tab_with_options(initial_cwd, true) {
            Ok(created_idx) => {
                let created_workspace = active_before.is_none();
                let ws_idx = if created_workspace {
                    Some(created_idx)
                } else {
                    self.state.active
                };
                let tab_idx = if created_workspace { 0 } else { created_idx };
                if let Some(name) = custom_name {
                    if let Some(ws) =
                        ws_idx.and_then(|ws_idx| self.state.workspaces.get_mut(ws_idx))
                    {
                        if let Some(tab) = ws.tabs.get_mut(tab_idx) {
                            tab.set_custom_name(name);
                        }
                        self.schedule_session_save();
                    }
                }
                if let Some(ws_idx) = ws_idx {
                    if created_workspace {
                        self.emit_workspace_open_events(ws_idx);
                    } else {
                        self.emit_tab_created_events(ws_idx, tab_idx);
                    }
                }
            }
            Err(e) => {
                error!(err = %e, "failed to create tab");
            }
        }
    }

    #[cfg(test)]
    pub(super) fn create_tab_with_options(
        &mut self,
        initial_cwd: PathBuf,
        focus: bool,
    ) -> std::io::Result<usize> {
        let Some(ws_idx) = self.state.active else {
            return self.create_workspace_with_options(initial_cwd, focus);
        };
        let (rows, cols) = self.state.estimate_pane_size();
        let ws = &mut self.state.workspaces[ws_idx];
        let (idx, terminal, runtime) = ws.create_tab(
            rows,
            cols,
            initial_cwd,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            self.state.host_terminal_appearance,
            crate::pane::PaneShellConfig::new(&self.state.default_shell, self.state.shell_mode),
            Vec::new(),
        )?;
        let root_pane = ws.tabs[idx].root_pane;
        self.terminal_runtimes.insert(terminal.id.clone(), runtime);
        self.state.terminals.insert(terminal.id.clone(), terminal);
        self.state.remove_alias_shadowed_by_new_pane(root_pane);
        if focus {
            self.state.switch_workspace_tab(ws_idx, idx);
            self.state.mode = Mode::Terminal;
        }
        let workspace_id = self.state.workspaces[ws_idx].id.clone();
        let tab_id = self
            .public_tab_id(ws_idx, idx)
            .unwrap_or_else(|| crate::workspace::public_tab_id_for_number(&workspace_id, idx + 1));
        let root_pane = self.state.workspaces[ws_idx].tabs[idx].root_pane.raw();
        crate::logging::tab_created(&workspace_id, &tab_id, root_pane);
        self.schedule_session_save();
        Ok(idx)
    }

    pub(crate) fn create_workspace_with_options(
        &mut self,
        initial_cwd: PathBuf,
        focus: bool,
    ) -> std::io::Result<usize> {
        self.create_workspace_with_launch_env(initial_cwd, focus, Vec::new())
    }

    pub(crate) fn dispatch_home_composer(
        &mut self,
        plan: crate::app::home::HomeDispatchPlan,
    ) -> std::io::Result<()> {
        let (rows, cols) = self.state.estimate_pane_size();
        let scrollback_limit_bytes = self.state.pane_scrollback_limit_bytes;
        let host_terminal_theme = self.state.host_terminal_theme;
        let host_terminal_appearance = self.state.host_terminal_appearance;

        match plan.target {
            crate::app::home::HomeTarget::NewSpace => {
                let (workspace, terminal, runtime) = Workspace::new_argv_command_with_extra_env(
                    plan.directory,
                    rows,
                    cols,
                    &plan.argv,
                    scrollback_limit_bytes,
                    host_terminal_theme,
                    host_terminal_appearance,
                    self.event_tx.clone(),
                    self.render_notify.clone(),
                    self.render_dirty.clone(),
                    Vec::new(),
                )?;
                self.terminal_runtimes.insert(terminal.id.clone(), runtime);
                self.state.terminals.insert(terminal.id.clone(), terminal);
                self.state.workspaces.push(workspace);
                let ws_idx = self.state.workspaces.len() - 1;
                self.state.remove_alias_shadowed_by_new_pane(
                    self.state.workspaces[ws_idx].tabs[0].root_pane,
                );
                self.state.switch_workspace(ws_idx);
                self.state.mode = Mode::Terminal;
                self.emit_workspace_open_events(ws_idx);
            }
            crate::app::home::HomeTarget::Existing(workspace_id) => {
                let Some(ws_idx) = self
                    .state
                    .workspaces
                    .iter()
                    .position(|workspace| workspace.id == workspace_id)
                else {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "selected space no longer exists",
                    ));
                };
                let (tab_idx, terminal, runtime, root_pane) = {
                    let workspace = &mut self.state.workspaces[ws_idx];
                    let (tab_idx, terminal, runtime) = workspace.create_tab_argv_command(
                        rows,
                        cols,
                        plan.directory,
                        &plan.argv,
                        Vec::new(),
                        scrollback_limit_bytes,
                        host_terminal_theme,
                        host_terminal_appearance,
                    )?;
                    let root_pane = workspace.tabs[tab_idx].root_pane;
                    (tab_idx, terminal, runtime, root_pane)
                };
                self.terminal_runtimes.insert(terminal.id.clone(), runtime);
                self.state.terminals.insert(terminal.id.clone(), terminal);
                self.state.remove_alias_shadowed_by_new_pane(root_pane);
                self.state.switch_workspace_tab(ws_idx, tab_idx);
                self.state.mode = Mode::Terminal;
                self.emit_tab_created_events(ws_idx, tab_idx);
            }
        }
        self.schedule_session_save();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn create_workspace_with_events(
        &mut self,
        initial_cwd: PathBuf,
        focus: bool,
    ) -> std::io::Result<()> {
        let ws_idx = self.create_workspace_with_options(initial_cwd, focus)?;
        self.emit_workspace_open_events(ws_idx);
        Ok(())
    }

    pub(crate) fn create_workspace_with_launch_env(
        &mut self,
        initial_cwd: PathBuf,
        focus: bool,
        extra_env: Vec<(String, String)>,
    ) -> std::io::Result<usize> {
        self.create_workspace_with_launch_env_and_work_context(initial_cwd, focus, extra_env, None)
    }

    pub(crate) fn create_workspace_with_launch_env_and_work_context(
        &mut self,
        initial_cwd: PathBuf,
        focus: bool,
        extra_env: Vec<(String, String)>,
        work_context: Option<crate::work_context::PaneWorkContext>,
    ) -> std::io::Result<usize> {
        let (rows, cols) = self.state.estimate_pane_size();
        let (ws, mut terminal, runtime) = Workspace::new_with_extra_env(
            initial_cwd,
            rows,
            cols,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            self.state.host_terminal_appearance,
            crate::pane::PaneShellConfig::new(&self.state.default_shell, self.state.shell_mode),
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
            extra_env,
        )?;
        Self::bind_spawn_work_context(&mut terminal, work_context);
        self.terminal_runtimes.insert(terminal.id.clone(), runtime);
        self.state.terminals.insert(terminal.id.clone(), terminal);
        self.state.workspaces.push(ws);
        let idx = self.state.workspaces.len() - 1;
        self.state
            .remove_alias_shadowed_by_new_pane(self.state.workspaces[idx].tabs[0].root_pane);
        let workspace_id = self.state.workspaces[idx].id.clone();
        let root_pane = self.state.workspaces[idx].tabs[0].root_pane.raw();
        crate::logging::workspace_created(&workspace_id, root_pane);
        if focus || self.state.active.is_none() {
            self.state.switch_workspace(idx);
            self.state.mode = Mode::Terminal;
        }
        self.schedule_session_save();
        Ok(idx)
    }

    pub(super) fn collect_panes_for_workspace(
        &self,
        workspace_id: Option<&str>,
    ) -> Result<Vec<crate::api::schema::PaneInfo>, (String, String)> {
        if let Some(workspace_id) = workspace_id {
            let Some(ws_idx) = self.parse_workspace_id(workspace_id) else {
                return Err((
                    "workspace_not_found".into(),
                    format!("workspace {workspace_id} not found"),
                ));
            };
            let Some(ws) = self.state.workspaces.get(ws_idx) else {
                return Err((
                    "workspace_not_found".into(),
                    format!("workspace {workspace_id} not found"),
                ));
            };
            Ok(ws
                .tabs
                .iter()
                .flat_map(|tab| tab.layout.pane_ids().into_iter())
                .filter_map(|pane_id| self.pane_info(ws_idx, pane_id))
                .collect())
        } else {
            Ok(self
                .state
                .workspaces
                .iter()
                .enumerate()
                .flat_map(|(ws_idx, ws)| {
                    ws.tabs
                        .iter()
                        .flat_map(|tab| tab.layout.pane_ids().into_iter())
                        .filter_map(move |pane_id| self.pane_info(ws_idx, pane_id))
                })
                .collect())
        }
    }

    pub(super) fn tab_info(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::api::schema::TabInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(tab_idx)?;
        let (agg_state, seen) = tab
            .panes
            .values()
            .filter_map(|pane| {
                self.state
                    .terminals
                    .get(&pane.attached_terminal_id)
                    .map(|terminal| (terminal.state, pane.seen))
            })
            .max_by_key(|(state, seen)| tab_attention_priority(*state, *seen))
            .unwrap_or((crate::detect::AgentState::Unknown, true));
        Some(crate::api::schema::TabInfo {
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            workspace_id: self.public_workspace_id(ws_idx),
            number: tab.number,
            label: ws
                .tab_display_projection(&self.state.terminals, tab_idx)
                .map(|projection| projection.full_label())
                .unwrap_or_else(|| (tab_idx + 1).to_string()),
            prio: tab.prio,
            focused: self.state.active == Some(ws_idx) && ws.active_tab == tab_idx,
            pane_count: tab.panes.len(),
            agent_status: aggregate_tab_agent_status(tab, &self.state.terminals, agg_state, seen),
        })
    }

    pub(crate) fn emit_workspace_open_events(&mut self, ws_idx: usize) {
        let workspace_info = self.workspace_info(ws_idx);
        let Some(tab) = self.tab_info(ws_idx, 0) else {
            return;
        };
        let Some(root_pane) = self.root_pane_info(ws_idx, 0) else {
            return;
        };
        self.emit_event(EventEnvelope {
            event: EventKind::WorkspaceCreated,
            data: EventData::WorkspaceCreated {
                workspace: workspace_info,
            },
        });
        self.emit_tab_and_pane_created_events(tab, root_pane);
        self.emit_layout_updated_event(ws_idx, 0);
    }

    pub(crate) fn emit_tab_created_events(&mut self, ws_idx: usize, tab_idx: usize) {
        let Some(tab) = self.tab_info(ws_idx, tab_idx) else {
            return;
        };
        let Some(root_pane) = self.root_pane_info(ws_idx, tab_idx) else {
            return;
        };
        self.emit_tab_and_pane_created_events(tab, root_pane);
        self.emit_layout_updated_event(ws_idx, tab_idx);
    }

    fn emit_tab_and_pane_created_events(
        &mut self,
        tab: crate::api::schema::TabInfo,
        root_pane: crate::api::schema::PaneInfo,
    ) {
        self.emit_event(EventEnvelope {
            event: EventKind::TabCreated,
            data: EventData::TabCreated { tab },
        });
        self.emit_event(EventEnvelope {
            event: EventKind::PaneCreated,
            data: EventData::PaneCreated { pane: root_pane },
        });
    }

    pub(super) fn workspace_created_result(
        &self,
        ws_idx: usize,
    ) -> Option<crate::api::schema::ResponseResult> {
        Some(crate::api::schema::ResponseResult::WorkspaceCreated {
            workspace: self.workspace_info(ws_idx),
            tab: self.tab_info(ws_idx, 0)?,
            root_pane: self.root_pane_info(ws_idx, 0)?,
        })
    }

    pub(super) fn tab_created_result(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::api::schema::ResponseResult> {
        Some(crate::api::schema::ResponseResult::TabCreated {
            tab: self.tab_info(ws_idx, tab_idx)?,
            root_pane: self.root_pane_info(ws_idx, tab_idx)?,
        })
    }

    pub(super) fn root_pane_info(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::api::schema::PaneInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(tab_idx)?;
        self.pane_info(ws_idx, tab.root_pane)
    }

    pub(super) fn pane_info(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<crate::api::schema::PaneInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane = ws.pane_state(pane_id)?;
        let terminal = self.state.terminals.get(&pane.attached_terminal_id)?;
        let tab_idx = ws.find_tab_index_for_pane(pane_id)?;
        let scroll = self
            .state
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
            .and_then(|runtime| runtime.scroll_metrics())
            .map(|metrics| crate::api::schema::PaneScrollInfo {
                offset_from_bottom: metrics.offset_from_bottom as u64,
                max_offset_from_bottom: metrics.max_offset_from_bottom as u64,
                viewport_rows: metrics.viewport_rows as u64,
            });
        let focused = self.state.active == Some(ws_idx)
            && ws.active_tab == tab_idx
            && ws
                .focused_pane_id()
                .is_some_and(|focused| focused == pane_id);
        let presentation = terminal.effective_presentation();
        Some(crate::api::schema::PaneInfo {
            pane_id: self.public_pane_id(ws_idx, pane_id)?,
            terminal_id: terminal.id.to_string(),
            workspace_id: self.public_workspace_id(ws_idx),
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            focused,
            work_context: terminal.effective_work_context().clone(),
            cwd: ws.tabs[tab_idx]
                .cwd_for_pane(pane_id, &self.state.terminals, &self.terminal_runtimes)
                .map(|cwd| cwd.display().to_string()),
            foreground_cwd: ws.tabs[tab_idx]
                .foreground_cwd_for_pane(pane_id, &self.terminal_runtimes)
                .map(|cwd| cwd.display().to_string()),
            label: terminal.manual_label.clone(),
            agent: terminal.effective_agent_label().map(str::to_string),
            title: presentation.title,
            terminal_title: terminal.terminal_title.clone(),
            terminal_title_stripped: terminal.terminal_title_stripped(),
            display_agent: presentation.display_agent,
            agent_status: pane_agent_status_with_stale(
                terminal.state,
                pane.seen,
                terminal.supervisor_stale,
            ),
            wait: terminal
                .hook_authority
                .as_ref()
                .and_then(|report| report.wait.clone()),
            eta_s: terminal
                .hook_authority
                .as_ref()
                .and_then(|report| report.eta_s),
            reported_at: terminal
                .hook_authority
                .as_ref()
                .and_then(|report| report.reported_at_wire.clone()),
            state_labels: presentation.state_labels,
            tokens: terminal.metadata_tokens.values(),
            gates: terminal.closing_gates.clone(),
            items: terminal.closing_items.clone(),
            decisions: terminal.closing_decisions.clone(),
            agent_session: terminal_agent_session_info(terminal),
            scroll,
            revision: terminal.revision,
        })
    }

    pub(super) fn lookup_runtime(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<(&crate::terminal::TerminalRuntime, String)> {
        let runtime =
            self.state
                .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)?;
        Some((runtime, self.public_workspace_id(ws_idx)))
    }

    pub(super) fn lookup_runtime_sender(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<&crate::terminal::TerminalRuntime> {
        self.state
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
    }

    pub(super) fn workspace_info(&self, index: usize) -> crate::api::schema::WorkspaceInfo {
        let ws = &self.state.workspaces[index];
        let (agg_state, seen) = ws.aggregate_state(&self.state.terminals);
        let stale = ws.tabs.iter().any(|tab| {
            tab.panes.values().any(|pane| {
                self.state
                    .terminals
                    .get(&pane.attached_terminal_id)
                    .is_some_and(|terminal| terminal.supervisor_stale)
            })
        });
        crate::api::schema::WorkspaceInfo {
            workspace_id: self.public_workspace_id(index),
            number: index + 1,
            label: ws.display_name_from(&self.state.terminals, &self.terminal_runtimes),
            focused: self.state.active == Some(index),
            pane_count: ws.public_pane_numbers.len(),
            tab_count: ws.tabs.len(),
            active_tab_id: self.public_tab_id(index, ws.active_tab).unwrap_or_else(|| {
                crate::workspace::public_tab_id_for_number(&ws.id, ws.active_tab + 1)
            }),
            agent_status: if stale {
                crate::api::schema::AgentStatus::Stale
            } else {
                pane_agent_status(agg_state, seen)
            },
            tokens: ws.metadata_tokens.values(),
            worktree: ws
                .worktree_space()
                .map(|space| crate::api::schema::WorkspaceWorktreeInfo {
                    repo_key: space.key.clone(),
                    repo_name: space.label.clone(),
                    repo_root: space.repo_root.display().to_string(),
                    checkout_path: space.checkout_path.display().to_string(),
                    is_linked_worktree: space.is_linked_worktree,
                }),
            repo_binding: ws.repo_binding.clone(),
        }
    }
}

fn terminal_agent_session_info(
    terminal: &crate::terminal::TerminalState,
) -> Option<crate::api::schema::AgentSessionInfo> {
    if let Some(authority) = terminal.hook_authority.as_ref() {
        if let Some(session_ref) = authority.session_ref.as_ref() {
            return Some(crate::api::schema::AgentSessionInfo {
                source: authority.source.clone(),
                agent: authority.agent_label.clone(),
                kind: session_ref.kind,
                value: session_ref.value.clone(),
            });
        }
    }

    terminal
        .persisted_agent_session
        .as_ref()
        .map(|session| crate::api::schema::AgentSessionInfo {
            source: session.source.clone(),
            agent: session.agent.clone(),
            kind: session.session_ref.kind,
            value: session.session_ref.value.clone(),
        })
}

fn aggregate_tab_agent_status(
    tab: &crate::workspace::Tab,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    state: crate::detect::AgentState,
    seen: bool,
) -> crate::api::schema::AgentStatus {
    let stale = tab.panes.values().any(|pane| {
        terminals
            .get(&pane.attached_terminal_id)
            .is_some_and(|terminal| terminal.supervisor_stale)
    });
    pane_agent_status_with_stale(state, seen, stale)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title_test_app() -> (App, crate::terminal::TerminalId) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        (app, terminal_id)
    }

    fn configure_title_test_terminal(
        app: &mut App,
        terminal_id: &crate::terminal::TerminalId,
        cwd: std::path::PathBuf,
        title: &str,
        work_title: Option<&str>,
    ) {
        let terminal = app.state.terminals.get_mut(terminal_id).unwrap();
        terminal.cwd = cwd;
        terminal.detected_agent = Some(crate::detect::Agent::Codex);
        terminal.set_terminal_title(Some(title.into()));
        if let Some(work_title) = work_title {
            terminal
                .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                    work_title: Some(work_title.to_string()),
                    ..Default::default()
                })
                .unwrap();
        }
    }

    #[test]
    fn tab_info_label_uses_live_detected_agent_title() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.ensure_test_terminals();

        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(crate::detect::Agent::Claude);
        terminal.set_terminal_title(Some("⠋ Add Subabe management token to Doppler".into()));

        assert_eq!(
            app.tab_info(0, 0).unwrap().label,
            "Add Subabe management token to Doppler"
        );
    }

    #[test]
    fn tab_info_label_rejects_cwd_title_in_favor_of_work_title() {
        let (mut app, terminal_id) = title_test_app();
        configure_title_test_terminal(
            &mut app,
            &terminal_id,
            "/Users/example/Repos/herdr-test".into(),
            "herdr-test",
            Some("Write Poem"),
        );

        assert_eq!(app.tab_info(0, 0).unwrap().label, "Write Poem");
    }

    #[test]
    fn tab_info_label_rejects_composed_agent_and_cwd_titles_for_all_separators() {
        let home = std::path::PathBuf::from(
            std::env::var_os("HOME").expect("test environment should define HOME"),
        );
        let cwd = home.join(".herdr-test");

        for separator in ['—', '–', '·', '|'] {
            let (mut app, terminal_id) = title_test_app();
            let title = format!("codex {separator} ~/.herdr-test");
            configure_title_test_terminal(
                &mut app,
                &terminal_id,
                cwd.clone(),
                &title,
                Some("Write Poem"),
            );

            let label = app.tab_info(0, 0).unwrap().label;
            assert_eq!(label, "Write Poem", "title: {title:?}");
            assert!(!label.contains("codex · codex"), "title: {title:?}");
        }
    }

    #[test]
    fn tab_info_label_strips_leading_agent_from_composed_title() {
        let home = std::path::PathBuf::from(
            std::env::var_os("HOME").expect("test environment should define HOME"),
        );
        let (mut app, terminal_id) = title_test_app();
        configure_title_test_terminal(
            &mut app,
            &terminal_id,
            home.join(".herdr-test"),
            "codex — Write Poem",
            Some("Fallback title"),
        );

        assert_eq!(app.tab_info(0, 0).unwrap().label, "Write Poem");
    }

    #[test]
    fn tab_info_label_keeps_nonleading_agent_word_in_composed_title() {
        let (mut app, terminal_id) = title_test_app();
        configure_title_test_terminal(
            &mut app,
            &terminal_id,
            "/Users/example/Repos/herdr-test".into(),
            "Deploy — codex helper",
            Some("Fallback title"),
        );

        assert_eq!(app.tab_info(0, 0).unwrap().label, "Deploy — codex helper");
    }

    #[test]
    fn tab_info_label_rejects_composed_agent_and_cwd_title_case_insensitively() {
        let home = std::path::PathBuf::from(
            std::env::var_os("HOME").expect("test environment should define HOME"),
        );
        let (mut app, terminal_id) = title_test_app();
        configure_title_test_terminal(
            &mut app,
            &terminal_id,
            home.join(".herdr-test"),
            "CODEX — ~/.HERDR-TEST",
            Some("Write Poem"),
        );

        assert_eq!(app.tab_info(0, 0).unwrap().label, "Write Poem");
    }

    #[test]
    fn tab_info_label_rejects_empty_terminal_titles() {
        let home = std::path::PathBuf::from(
            std::env::var_os("HOME").expect("test environment should define HOME"),
        );

        for title in ["", "   "] {
            let (mut app, terminal_id) = title_test_app();
            configure_title_test_terminal(
                &mut app,
                &terminal_id,
                home.join(".herdr-test"),
                title,
                Some("Write Poem"),
            );

            assert_eq!(app.tab_info(0, 0).unwrap().label, "Write Poem");
        }
    }

    #[test]
    fn tab_info_label_keeps_distinct_terminal_title_ahead_of_work_title() {
        let (mut app, terminal_id) = title_test_app();
        configure_title_test_terminal(
            &mut app,
            &terminal_id,
            "/Users/example/Repos/herdr-test".into(),
            "Review the patch",
            Some("Write Poem"),
        );

        assert_eq!(app.tab_info(0, 0).unwrap().label, "Review the patch");
    }

    #[test]
    fn tab_info_label_rejects_full_and_tilde_cwd_titles() {
        let home = std::path::PathBuf::from(
            std::env::var_os("HOME").expect("test environment should define HOME"),
        );
        let cwd = home.join("Repos/herdr-test");
        let relative = cwd.strip_prefix(&home).unwrap().display().to_string();
        let expected = "Write Poem";

        let (mut full_app, full_terminal_id) = title_test_app();
        configure_title_test_terminal(
            &mut full_app,
            &full_terminal_id,
            cwd.clone(),
            &cwd.display().to_string(),
            Some("Write Poem"),
        );
        assert_eq!(full_app.tab_info(0, 0).unwrap().label, expected);

        let (mut tilde_app, tilde_terminal_id) = title_test_app();
        configure_title_test_terminal(
            &mut tilde_app,
            &tilde_terminal_id,
            cwd,
            &format!("~/{relative}"),
            Some("Write Poem"),
        );
        assert_eq!(tilde_app.tab_info(0, 0).unwrap().label, expected);
    }

    #[test]
    fn tab_info_label_rejects_cwd_path_fragment_title() {
        let (mut app, terminal_id) = title_test_app();
        configure_title_test_terminal(
            &mut app,
            &terminal_id,
            "/Users/example/Repos/herdr-test".into(),
            "Repos/herdr-test",
            Some("Write Poem"),
        );

        assert_eq!(app.tab_info(0, 0).unwrap().label, "Write Poem");
    }

    #[test]
    fn tab_info_label_keeps_manual_label_and_degrades_without_other_title() {
        let (mut manual_app, manual_terminal_id) = title_test_app();
        configure_title_test_terminal(
            &mut manual_app,
            &manual_terminal_id,
            "/Users/example/Repos/herdr-test".into(),
            "herdr-test",
            Some("Write Poem"),
        );
        manual_app
            .state
            .terminals
            .get_mut(&manual_terminal_id)
            .unwrap()
            .set_manual_label("Pinned".into());
        assert_eq!(manual_app.tab_info(0, 0).unwrap().label, "Pinned");

        let (mut no_title_app, no_title_terminal_id) = title_test_app();
        configure_title_test_terminal(
            &mut no_title_app,
            &no_title_terminal_id,
            "/Users/example/Repos/herdr-test".into(),
            "herdr-test",
            None,
        );
        assert_eq!(no_title_app.tab_info(0, 0).unwrap().label, "codex");
    }

    #[test]
    fn tab_info_label_uses_tab_number_for_plain_shell() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.ensure_test_terminals();

        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_terminal_title(Some("/Users/example/herdr".into()));

        assert_eq!(app.tab_info(0, 0).unwrap().label, "1");
    }

    #[test]
    fn second_spawn_for_same_pr_is_history_without_displacing_owner() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("owner"), Workspace::test_new("review")];
        app.state.ensure_test_terminals();
        let terminal_ids = app
            .state
            .workspaces
            .iter()
            .map(|workspace| {
                let pane_id = workspace.tabs[0].root_pane;
                workspace.terminal_id(pane_id).cloned().unwrap()
            })
            .collect::<Vec<_>>();
        let requested = |pr_url: &str, role| crate::work_context::PaneWorkContext {
            pr_urls: vec![pr_url.into()],
            role: Some(role),
            active_owner: true,
            ..Default::default()
        };

        let first = app
            .prepare_spawn_work_context(Some(requested(
                "https://github.com/O/R/pull/42",
                crate::work_context::PaneWorkRole::Ship,
            )))
            .unwrap()
            .unwrap();
        App::bind_spawn_work_context(
            app.state.terminals.get_mut(&terminal_ids[0]).unwrap(),
            Some(first),
        );
        let second = app
            .prepare_spawn_work_context(Some(requested(
                "https://github.com/o/r/pull/42",
                crate::work_context::PaneWorkRole::Review,
            )))
            .unwrap()
            .unwrap();
        assert!(!second.active_owner);
        App::bind_spawn_work_context(
            app.state.terminals.get_mut(&terminal_ids[1]).unwrap(),
            Some(second),
        );

        let contexts = terminal_ids
            .iter()
            .map(|id| app.state.terminals[id].effective_work_context())
            .collect::<Vec<_>>();
        assert_eq!(
            contexts
                .iter()
                .filter(|context| context.active_owner)
                .count(),
            1
        );
        assert_eq!(
            contexts[0].role,
            Some(crate::work_context::PaneWorkRole::Ship)
        );
        assert!(contexts[0].active_owner);
        assert_eq!(
            contexts[1].role,
            Some(crate::work_context::PaneWorkRole::Review)
        );
        assert!(!contexts[1].active_owner);
    }
}
