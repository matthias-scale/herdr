//! Home: the fleet at a glance, and one keystroke to whatever is waiting.
//!
//! The inbox next door answers "deal with these one at a time" and deliberately
//! shows a single agent, because clearing a queue is a loop. Home answers the
//! question you ask *before* that loop — is there anything worth entering it
//! for, and if so, which one — so it shows the whole queue and jumps.
//!
//! Like the inbox, the queue is derived on every read rather than cached, so an
//! agent that answers its own gate simply stops appearing. The only thing held
//! here is the cursor plus the draft dispatch settings, which cannot be
//! re-derived.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

use crate::{app::inbox::BlockedAgent, detect::Agent, ui::dropdown::DropdownFilterState};

use super::home_catalog::{HomeCatalog, HomeProviderCatalog, AUTO_EFFORT, DEFAULT_MODEL};

pub(crate) const HOME_COMPOSER_MIN_HEIGHT: u16 = 11;
pub(crate) const HOME_LENS_MIN_HEIGHT: u16 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HomeFocus {
    Reply,
    Prompt,
    Agent,
    Model,
    Effort,
    Directory,
    Workspace,
    Ref,
    Target,
}

impl HomeFocus {
    pub(crate) fn next(self, effort_visible: bool) -> Self {
        match self {
            Self::Reply => Self::Prompt,
            Self::Prompt => Self::Agent,
            Self::Agent => Self::Model,
            Self::Model if effort_visible => Self::Effort,
            Self::Model => Self::Directory,
            Self::Effort => Self::Directory,
            Self::Directory => Self::Workspace,
            Self::Workspace => Self::Ref,
            Self::Ref => Self::Target,
            Self::Target => Self::Prompt,
        }
    }

    pub(crate) fn previous(self, effort_visible: bool) -> Self {
        match self {
            Self::Reply => Self::Target,
            Self::Prompt => Self::Target,
            Self::Agent => Self::Prompt,
            Self::Model => Self::Agent,
            Self::Effort => Self::Model,
            Self::Directory if effort_visible => Self::Effort,
            Self::Directory => Self::Model,
            Self::Workspace => Self::Directory,
            Self::Ref => Self::Workspace,
            Self::Target => Self::Ref,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HomePicker {
    Agent,
    Model,
    Effort,
    Directory,
    Workspace,
    Ref,
    Target,
}

impl HomePicker {
    pub(crate) fn for_focus(focus: HomeFocus) -> Option<Self> {
        match focus {
            HomeFocus::Reply => None,
            HomeFocus::Prompt => None,
            HomeFocus::Agent => Some(Self::Agent),
            HomeFocus::Model => Some(Self::Model),
            HomeFocus::Effort => Some(Self::Effort),
            HomeFocus::Directory => Some(Self::Directory),
            HomeFocus::Workspace => Some(Self::Workspace),
            HomeFocus::Ref => Some(Self::Ref),
            HomeFocus::Target => Some(Self::Target),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HomeTarget {
    NewSpace,
    Existing(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HomeDispatchPlan {
    pub(crate) agent: Agent,
    pub(crate) model: String,
    pub(crate) effort: Option<String>,
    pub(crate) directory: PathBuf,
    pub(crate) target: HomeTarget,
    pub(crate) prompt: String,
    pub(crate) argv: Vec<String>,
}

/// Placeholder option of the workspace picker until slice 1b fills it.
pub(crate) const CURRENT_CHECKOUT_LABEL: &str = "Current checkout";
/// Shown when no pane in the selected directory has reported a branch yet.
pub(crate) const UNKNOWN_REF_LABEL: &str = "current branch";

/// Last path component, or the whole path when there is none (`/`).
fn directory_basename(directory: &Path) -> String {
    directory
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| directory.display().to_string())
}

fn default_directory() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

pub(crate) fn directory_label(path: &Path) -> String {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return path.display().to_string();
    };
    path.strip_prefix(&home)
        .map(|relative| {
            if relative.as_os_str().is_empty() {
                "~".to_string()
            } else {
                format!("~/{}", relative.display())
            }
        })
        .unwrap_or_else(|_| path.display().to_string())
}

pub(crate) fn dispatchable_agents() -> &'static [Agent] {
    &[Agent::Claude, Agent::Codex]
}

/// Cursor and dispatch state for an open home view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HomeState {
    catalog: HomeCatalog,
    selected: usize,
    pub(crate) focus: Option<HomeFocus>,
    pub(crate) prompt: String,
    pub(crate) reply: String,
    pub(crate) reply_error: Option<String>,
    pub(crate) agent: Agent,
    pub(crate) model: String,
    pub(crate) effort: Option<String>,
    pub(crate) directory: PathBuf,
    pub(crate) target: HomeTarget,
    pub(crate) picker: Option<HomePicker>,
    pub(crate) picker_selected: usize,
    pub(crate) directory_filter: DropdownFilterState,
}

impl Default for HomeState {
    fn default() -> Self {
        Self {
            catalog: HomeCatalog::fallback(),
            selected: 0,
            focus: Some(HomeFocus::Prompt),
            prompt: String::new(),
            reply: String::new(),
            reply_error: None,
            agent: Agent::Claude,
            model: DEFAULT_MODEL.into(),
            effort: Some(AUTO_EFFORT.into()),
            directory: default_directory(),
            target: HomeTarget::NewSpace,
            picker: None,
            picker_selected: 0,
            directory_filter: DropdownFilterState::default(),
        }
    }
}

impl HomeState {
    pub(crate) fn with_catalog(catalog: HomeCatalog) -> Self {
        Self {
            catalog,
            ..Self::default()
        }
    }

    pub(crate) fn replace_provider_catalog(&mut self, provider: HomeProviderCatalog) {
        let selected_agent = provider.agent == self.agent;
        self.catalog.replace(provider);
        if !selected_agent {
            return;
        }
        let selected_model_exists = self
            .catalog
            .provider(self.agent)
            .is_some_and(|provider| provider.model(&self.model).is_some());
        if !selected_model_exists {
            self.set_agent(self.agent);
        } else {
            let selected_effort_exists = self
                .effort
                .as_deref()
                .is_some_and(|effort| self.effort_options().iter().any(|known| known == effort));
            if !selected_effort_exists {
                self.effort = self.effort_options().first().cloned();
            }
        }

        let picker_len = match self.picker {
            Some(HomePicker::Model) => Some(self.model_options().len()),
            Some(HomePicker::Effort) => Some(self.effort_options().len()),
            _ => None,
        };
        if let Some(picker_len) = picker_len {
            if picker_len == 0 {
                self.picker = None;
                self.picker_selected = 0;
            } else {
                self.picker_selected = self.picker_selected.min(picker_len - 1);
            }
        }
    }

    pub(crate) fn model_options(&self) -> &[super::home_catalog::HomeModelCatalogEntry] {
        self.catalog
            .provider(self.agent)
            .map(|provider| provider.models.as_slice())
            .unwrap_or(&[])
    }

    pub(crate) fn effort_options(&self) -> &[String] {
        self.catalog
            .provider(self.agent)
            .and_then(|provider| provider.model(&self.model))
            .map(|model| model.efforts.as_slice())
            .unwrap_or(&[])
    }

    /// The row the cursor is on, or nothing when the queue is empty.
    ///
    /// The cursor is clamped on read rather than on every queue change: agents
    /// block and unblock without anything telling the view, so a stored index is
    /// only ever a hint about where the operator was looking.
    pub(crate) fn current<'a>(&self, queue: &'a [BlockedAgent]) -> Option<&'a BlockedAgent> {
        queue.get(self.selected.min(queue.len().saturating_sub(1)))
    }

    /// Index of the selected row, clamped to the queue.
    pub(crate) fn selected(&self, queue: &[BlockedAgent]) -> usize {
        self.selected.min(queue.len().saturating_sub(1))
    }

    /// Put the cursor on a specific row. Clamped on read like every other
    /// cursor move, so a stale index from a click is harmless.
    pub(crate) fn select(&mut self, index: usize) {
        self.selected = index;
        self.reply_error = None;
    }

    /// Move down one row, stopping at the end.
    ///
    /// Deliberately does not wrap. The list is sorted by how long each agent has
    /// been waiting, so the ends mean something — arriving at the bottom should
    /// read as "that is all of them", not put you back on the oldest.
    pub(crate) fn select_next(&mut self, queue: &[BlockedAgent]) {
        self.reply_error = None;
        if queue.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected(queue) + 1).min(queue.len() - 1);
    }

    /// Move up one row, stopping at the top.
    pub(crate) fn select_prev(&mut self, queue: &[BlockedAgent]) {
        self.selected = self.selected(queue).saturating_sub(1);
        self.reply_error = None;
    }

    /// First visible row, given how many rows fit.
    ///
    /// Scrolling is derived from the cursor instead of stored, so a queue that
    /// shrinks under the view cannot strand it on blank space below the list.
    pub(crate) fn scroll(&self, queue: &[BlockedAgent], visible_rows: usize) -> usize {
        if visible_rows == 0 || queue.len() <= visible_rows {
            return 0;
        }
        let selected = self.selected(queue);
        let max_scroll = queue.len() - visible_rows;
        selected.saturating_sub(visible_rows - 1).min(max_scroll)
    }

    pub(crate) fn effort_visible(&self) -> bool {
        self.effort.is_some()
    }

    pub(crate) fn move_focus(&mut self, backwards: bool) {
        let current = self.focus.unwrap_or(HomeFocus::Prompt);
        self.focus = Some(if backwards {
            current.previous(self.effort_visible())
        } else {
            current.next(self.effort_visible())
        });
    }

    pub(crate) fn close_composer_or_home(&mut self) -> bool {
        if self.picker.take().is_some() {
            return false;
        }
        if self.focus.take().is_some() {
            return false;
        }
        true
    }

    pub(crate) fn set_agent(&mut self, agent: Agent) {
        self.agent = agent;
        self.model = self
            .catalog
            .provider(agent)
            .and_then(|catalog| catalog.models.first())
            .map(|model| model.id.as_str())
            .unwrap_or_default()
            .to_string();
        self.effort = self
            .catalog
            .provider(agent)
            .and_then(|catalog| catalog.model(&self.model))
            .and_then(|model| model.efforts.first())
            .cloned();
    }

    pub(crate) fn set_model(&mut self, model: impl Into<String>) {
        let model = model.into();
        let efforts = self
            .catalog
            .provider(self.agent)
            .and_then(|provider| provider.model(&model))
            .map(|model| model.efforts.clone())
            .unwrap_or_default();
        if efforts.is_empty() {
            return;
        }
        self.model = model;
        if !self
            .effort
            .as_deref()
            .is_some_and(|effort| efforts.iter().any(|known| known == effort))
        {
            self.effort = efforts.first().cloned();
        }
    }

    pub(crate) fn append_prompt(&mut self, character: char) {
        self.prompt.push(character);
    }

    pub(crate) fn append_reply(&mut self, character: char) {
        self.reply.push(character);
        self.reply_error = None;
    }

    pub(crate) fn backspace_reply(&mut self) {
        self.reply.pop();
        self.reply_error = None;
    }

    pub(crate) fn backspace_prompt(&mut self) {
        self.prompt.pop();
    }

    pub(crate) fn dispatch_plan(&self) -> Result<HomeDispatchPlan, String> {
        let prompt = self.prompt.trim();
        if prompt.is_empty() {
            return Err("enter a prompt before dispatching".into());
        }
        let Some(catalog) = self.catalog.provider(self.agent) else {
            return Err("that agent cannot be dispatched from home".into());
        };
        let Some(model) = catalog.model(&self.model) else {
            return Err("select a model supported by that agent".into());
        };
        let effort = self.effort.as_deref().unwrap_or(AUTO_EFFORT);
        if !model.efforts.iter().any(|known| known == effort) {
            return Err("select an effort supported by that model".into());
        }

        let mut argv = vec![crate::detect::interactive_agent_executable(self.agent).into()];
        match self.agent {
            Agent::Claude => {
                if self.model != DEFAULT_MODEL {
                    argv.extend(["--model".into(), self.model.clone()]);
                }
                if effort != AUTO_EFFORT {
                    argv.extend(["--effort".into(), effort.into()]);
                }
            }
            Agent::Codex => {
                if self.model != DEFAULT_MODEL {
                    argv.extend(["--model".into(), self.model.clone()]);
                }
                if effort != AUTO_EFFORT {
                    argv.extend(["-c".into(), format!("model_reasoning_effort={effort}")]);
                }
            }
            _ => return Err("that agent cannot be dispatched from home".into()),
        }
        argv.push(prompt.into());

        Ok(HomeDispatchPlan {
            agent: self.agent,
            model: self.model.clone(),
            effort: self.effort.clone(),
            directory: self.directory.clone(),
            target: self.target.clone(),
            prompt: prompt.into(),
            argv,
        })
    }
}

/// Fleet-wide counts for the header line.
///
/// `agents` counts panes running a recognised agent, not panes: a shell you left
/// open is not something the fleet is doing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct HomeCounts {
    pub blocked: usize,
    pub agents: usize,
    pub spaces: usize,
}

impl crate::app::state::AppState {
    pub(crate) fn pane_id_for_terminal(
        &self,
        terminal_id: &crate::terminal::TerminalId,
    ) -> Option<crate::layout::PaneId> {
        self.workspaces
            .iter()
            .flat_map(|workspace| workspace.tabs.iter())
            .flat_map(|tab| tab.panes.iter())
            .find_map(|(pane_id, pane)| {
                (pane.attached_terminal_id == *terminal_id).then_some(*pane_id)
            })
    }

    pub(crate) fn note_human_key(
        &mut self,
        pane_id: crate::layout::PaneId,
        key: &crate::input::TerminalKey,
    ) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        let has_terminal_modifier = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
        if has_terminal_modifier {
            return;
        }
        match key.code {
            KeyCode::Char(character) => {
                let character = key
                    .shifted_codepoint
                    .and_then(char::from_u32)
                    .unwrap_or(character);
                let draft = self.pending_human_drafts.entry(pane_id).or_default();
                for _ in 0..key.repeat_count.max(1) {
                    draft.push(character);
                }
            }
            KeyCode::Backspace => {
                if let Some(draft) = self.pending_human_drafts.get_mut(&pane_id) {
                    for _ in 0..key.repeat_count.max(1) {
                        draft.pop();
                    }
                    if draft.is_empty() {
                        self.pending_human_drafts.remove(&pane_id);
                    }
                }
            }
            KeyCode::Enter => {
                self.pending_human_drafts.remove(&pane_id);
            }
            _ => {}
        }
    }

    pub(crate) fn note_human_text(&mut self, pane_id: crate::layout::PaneId, text: &str) {
        if text.is_empty() {
            return;
        }
        if text.contains(['\r', '\n']) {
            self.pending_human_drafts.remove(&pane_id);
            return;
        }
        self.pending_human_drafts
            .entry(pane_id)
            .or_default()
            .push_str(text);
    }

    pub(crate) fn toggle_home(&mut self) {
        self.home = match self.home.take() {
            Some(_) => None,
            None => {
                // Home and the inbox both want the whole frame; opening one puts
                // the other away rather than stacking two overlays.
                self.inbox = None;
                Some(HomeState::with_catalog(self.home_catalog.clone()))
            }
        };
    }

    /// Open home as the launch screen, if the config wants it.
    ///
    /// Deliberately not done in `App::new`: that constructor is what dozens of
    /// tests build from, and a full-frame overlay that covers the panes and
    /// swallows input would change what every one of them is testing. Launching
    /// is a concern of the thing that starts a session, not of the constructor.
    pub(crate) fn open_home_on_launch(&mut self, config: &crate::config::Config) {
        if config.ui.show_home_on_start {
            self.home = Some(HomeState::with_catalog(self.home_catalog.clone()));
            self.inbox = None;
        }
    }

    pub(crate) fn clear_home(&mut self) {
        self.home = None;
    }

    pub(crate) fn home_counts(&self, queue: &[BlockedAgent]) -> HomeCounts {
        let agents = self
            .workspaces
            .iter()
            .flat_map(|ws| ws.tabs.iter())
            .flat_map(|tab| tab.panes.values())
            .filter(|pane| {
                self.terminals
                    .get(&pane.attached_terminal_id)
                    .is_some_and(|terminal| terminal.effective_agent_label().is_some())
            })
            .count();
        HomeCounts {
            blocked: queue.len(),
            agents,
            spaces: self.workspaces.len(),
        }
    }

    pub(crate) fn home_directory_options(&self) -> Vec<PathBuf> {
        let mut options = Vec::new();
        if let Some(home) = &self.home {
            options.push(home.directory.clone());
        }
        if let Ok(current) = std::env::current_dir() {
            if !options.contains(&current) {
                options.push(current);
            }
        }
        for cwd in self
            .workspaces
            .iter()
            .map(|workspace| workspace.identity_cwd.clone())
        {
            if !options.contains(&cwd) {
                options.push(cwd);
            }
        }
        options
    }

    /// Every pane work context observed for a workspace rooted at `directory`.
    ///
    /// Home names a directory, not a pane, so the repo and branch it shows are
    /// whatever the panes already working there have observed. Nothing is
    /// derived here: an unlinked directory simply yields nothing.
    fn work_contexts_for_directory<'a>(
        &'a self,
        directory: &'a Path,
    ) -> impl Iterator<Item = &'a crate::work_context::PaneWorkContext> + 'a {
        self.workspaces
            .iter()
            .filter(move |workspace| workspace.identity_cwd == directory)
            .flat_map(|workspace| workspace.tabs.iter())
            .flat_map(|tab| tab.panes.values())
            .filter_map(|pane| self.terminals.get(&pane.attached_terminal_id))
            .map(|terminal| terminal.effective_work_context())
    }

    fn home_directory(&self) -> PathBuf {
        self.home
            .as_ref()
            .map(|home| home.directory.clone())
            .unwrap_or_else(default_directory)
    }

    /// What the headline calls the place this thread will start in.
    ///
    /// The declared repo outranks the path because a worktree directory is
    /// named after the task, not the project.
    pub(crate) fn home_headline_name(&self) -> String {
        let directory = self.home_directory();
        let repo = self
            .work_contexts_for_directory(&directory)
            .find_map(|context| context.repo.as_deref())
            .map(|repo| {
                repo.rsplit('/')
                    .next()
                    .filter(|name| !name.is_empty())
                    .unwrap_or(repo)
                    .to_string()
            });
        repo.unwrap_or_else(|| directory_basename(&directory))
    }

    /// The single ref option the bottom-right picker shows. Slice 1c replaces
    /// it with the real ref list; until then it names the branch the panes in
    /// that directory already report.
    pub(crate) fn home_ref_options(&self) -> Vec<String> {
        let directory = self.home_directory();
        let branch = self
            .work_contexts_for_directory(&directory)
            .find_map(|context| context.branch.clone());
        vec![branch.unwrap_or_else(|| UNKNOWN_REF_LABEL.to_string())]
    }

    /// The single workspace option the bottom-left picker shows. Slice 1b adds
    /// the new and previous worktree options.
    pub(crate) fn home_workspace_options(&self) -> Vec<String> {
        vec![CURRENT_CHECKOUT_LABEL.to_string()]
    }

    pub(crate) fn home_target_options(&self) -> Vec<HomeTarget> {
        let mut options = vec![HomeTarget::NewSpace];
        options.extend(
            self.workspaces
                .iter()
                .map(|workspace| HomeTarget::Existing(workspace.id.clone())),
        );
        options
    }

    fn home_picker_len(&self, picker: HomePicker) -> usize {
        match picker {
            HomePicker::Agent => dispatchable_agents().len(),
            HomePicker::Model => self
                .home
                .as_ref()
                .map(|home| home.model_options().len())
                .unwrap_or(0),
            HomePicker::Effort => self
                .home
                .as_ref()
                .map(|home| home.effort_options().len())
                .unwrap_or(0),
            HomePicker::Directory => self.home_directory_match_indices().len(),
            HomePicker::Workspace => self.home_workspace_options().len(),
            HomePicker::Ref => self.home_ref_options().len(),
            HomePicker::Target => self.home_target_options().len(),
        }
    }

    fn home_directory_match_indices(&self) -> Vec<usize> {
        let labels = self
            .home_directory_options()
            .iter()
            .map(|directory| directory_label(directory))
            .collect::<Vec<_>>();
        self.home
            .as_ref()
            .map(|home| {
                home.directory_filter
                    .matches(&labels)
                    .into_iter()
                    .map(|(index, _)| index)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn home_open_picker(&mut self, picker: HomePicker) {
        if picker == HomePicker::Directory {
            if let Some(home) = self.home.as_mut() {
                home.directory_filter.set_query("");
            }
        }
        let length = self.home_picker_len(picker);
        if length == 0 {
            return;
        }
        let current = self
            .home
            .as_ref()
            .and_then(|home| match picker {
                HomePicker::Agent => dispatchable_agents()
                    .iter()
                    .position(|agent| *agent == home.agent),
                HomePicker::Model => home
                    .model_options()
                    .iter()
                    .position(|model| model.id == home.model),
                HomePicker::Effort => home.effort.as_deref().and_then(|effort| {
                    home.effort_options()
                        .iter()
                        .position(|option| *option == effort)
                }),
                HomePicker::Directory => self
                    .home_directory_options()
                    .iter()
                    .position(|directory| directory == &home.directory),
                // Both placeholders carry a single option, so the cursor has
                // nowhere else to be until 1b and 1c give them real lists.
                HomePicker::Workspace | HomePicker::Ref => Some(0),
                HomePicker::Target => self
                    .home_target_options()
                    .iter()
                    .position(|target| target == &home.target),
            })
            .unwrap_or(0);
        if let Some(home) = self.home.as_mut() {
            home.picker = Some(picker);
            if picker == HomePicker::Directory {
                home.directory_filter.selected = current.min(length - 1);
            } else {
                home.picker_selected = current.min(length - 1);
            }
        }
    }

    pub(crate) fn home_move_picker(&mut self, delta: i32) {
        let Some(picker) = self.home.as_ref().and_then(|home| home.picker) else {
            return;
        };
        let length = self.home_picker_len(picker);
        if let Some(home) = self.home.as_mut() {
            if picker == HomePicker::Directory {
                home.directory_filter.move_selection(delta, length);
            } else if length > 0 {
                let selected =
                    (home.picker_selected as i32 + delta).clamp(0, length as i32 - 1) as usize;
                home.picker_selected = selected;
            }
        }
    }

    pub(crate) fn home_push_directory_filter(&mut self, character: char) {
        if let Some(home) = self
            .home
            .as_mut()
            .filter(|home| home.picker == Some(HomePicker::Directory))
        {
            home.directory_filter.push(character);
        }
    }

    pub(crate) fn home_pop_directory_filter(&mut self) {
        if let Some(home) = self
            .home
            .as_mut()
            .filter(|home| home.picker == Some(HomePicker::Directory))
        {
            home.directory_filter.pop();
        }
    }

    pub(crate) fn home_accept_picker(&mut self) {
        let Some((picker, selected)) = self.home.as_ref().and_then(|home| {
            home.picker.map(|picker| {
                let selected = if picker == HomePicker::Directory {
                    home.directory_filter.selected
                } else {
                    home.picker_selected
                };
                (picker, selected)
            })
        }) else {
            return;
        };
        let selected = if picker == HomePicker::Directory {
            let Some(selected) = self.home_directory_match_indices().get(selected).copied() else {
                return;
            };
            selected
        } else {
            selected
        };
        match picker {
            HomePicker::Agent => {
                if let Some(agent) = dispatchable_agents().get(selected).copied() {
                    if let Some(home) = self.home.as_mut() {
                        home.set_agent(agent);
                    }
                }
            }
            HomePicker::Model => {
                let model = self
                    .home
                    .as_ref()
                    .and_then(|home| home.model_options().get(selected))
                    .map(|model| model.id.clone());
                if let Some(model) = model {
                    if let Some(home) = self.home.as_mut() {
                        home.set_model(model);
                    }
                }
            }
            HomePicker::Effort => {
                let effort = self
                    .home
                    .as_ref()
                    .and_then(|home| home.effort_options().get(selected))
                    .cloned();
                if let Some(home) = self.home.as_mut() {
                    home.effort = effort;
                }
            }
            HomePicker::Directory => {
                if let Some(directory) = self.home_directory_options().get(selected).cloned() {
                    if let Some(home) = self.home.as_mut() {
                        home.directory = directory;
                    }
                }
            }
            // Selecting the only option cannot change anything.
            HomePicker::Workspace | HomePicker::Ref => {}
            HomePicker::Target => {
                if let Some(target) = self.home_target_options().get(selected).cloned() {
                    if let Some(home) = self.home.as_mut() {
                        home.target = target;
                    }
                }
            }
        }
        if let Some(home) = self.home.as_mut() {
            home.picker = None;
        }
    }

    /// Focus the pane the cursor is on and leave home. Returns whether it moved.
    ///
    /// A row can name a pane that has since closed, so this reports failure
    /// rather than leaving home closed over nothing.
    pub(crate) fn jump_to_selected_home_agent(&mut self, queue: &[BlockedAgent]) -> bool {
        let Some(agent) = self.home.as_ref().and_then(|home| home.current(queue)) else {
            return false;
        };
        let (ws_idx, pane_id) = (agent.ws_idx, agent.pane_id);
        let pane_exists = self
            .workspaces
            .get(ws_idx)
            .is_some_and(|ws| ws.tabs.iter().any(|tab| tab.panes.contains_key(&pane_id)));
        if !pane_exists {
            return false;
        }
        self.focus_pane_in_workspace(ws_idx, pane_id);
        self.clear_home();
        true
    }
}

impl crate::app::App {
    pub(crate) fn reply_to_selected_home_agent(&mut self) {
        let queue = self.state.blocked_agents();
        let Some((ws_idx, pane_id, reply)) = self.state.home.as_ref().and_then(|home| {
            home.current(&queue)
                .map(|agent| (agent.ws_idx, agent.pane_id, home.reply.clone()))
        }) else {
            if let Some(home) = self.state.home.as_mut() {
                home.reply_error = Some("no blocked pane is selected".into());
            }
            return;
        };
        if reply.trim().is_empty() {
            if let Some(home) = self.state.home.as_mut() {
                home.reply_error = Some("type a reply first".into());
            }
            return;
        }
        if self
            .state
            .pending_human_drafts
            .get(&pane_id)
            .is_some_and(|draft| !draft.is_empty())
        {
            if let Some(home) = self.state.home.as_mut() {
                home.reply_error = Some("human draft pending · clear it in the pane".into());
            }
            return;
        }
        let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) else {
            if let Some(home) = self.state.home.as_mut() {
                home.reply_error = Some("pane is no longer available".into());
            }
            return;
        };
        match self.try_send_text_to_pane(&public_pane_id, &format!("{reply}\r")) {
            Ok(()) => {
                if let Some(home) = self.state.home.as_mut() {
                    home.reply.clear();
                    home.reply_error = None;
                    home.focus = None;
                }
            }
            Err(crate::app::api::PaneSendError::NotFound) => {
                if let Some(home) = self.state.home.as_mut() {
                    home.reply_error = Some("pane is no longer available".into());
                }
            }
            Err(crate::app::api::PaneSendError::Failed(error)) => {
                if let Some(home) = self.state.home.as_mut() {
                    home.reply_error = Some(format!("reply failed · {error}"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::PaneId;
    use crate::terminal::TerminalId;

    fn queue(n: usize) -> Vec<BlockedAgent> {
        (0..n)
            .map(|i| BlockedAgent {
                ws_idx: 0,
                pane_id: PaneId::alloc(),
                terminal_id: TerminalId::alloc(),
                workspace_label: format!("ws{i}"),
                agent_label: format!("agent{i}"),
                blocked_since: None,
                seq: None,
            })
            .collect()
    }

    fn home_with_codex_catalog() -> HomeState {
        let codex = super::super::home_catalog::parse_codex_catalog(
            br#"{"models":[
                {"slug":"gpt-5.6-sol","visibility":"list","priority":1,"supported_reasoning_levels":[{"effort":"low"},{"effort":"ultra"}]},
                {"slug":"gpt-5.6-luna","visibility":"list","priority":2,"supported_reasoning_levels":[{"effort":"low"},{"effort":"max"}]}
            ]}"#,
        )
        .expect("Codex fixture");
        HomeState::with_catalog(HomeCatalog::with_codex(codex))
    }

    #[test]
    fn an_empty_queue_selects_nothing() {
        assert!(HomeState::default().current(&[]).is_none());
    }

    #[test]
    fn the_cursor_starts_on_the_longest_wait() {
        let q = queue(3);
        let home = HomeState::default();
        assert_eq!(home.current(&q).map(|a| a.pane_id), Some(q[0].pane_id));
    }

    #[test]
    fn the_cursor_stops_at_both_ends_rather_than_wrapping() {
        let q = queue(3);
        let mut home = HomeState::default();
        home.select_prev(&q);
        assert_eq!(home.selected(&q), 0, "top must not wrap to the bottom");
        for _ in 0..5 {
            home.select_next(&q);
        }
        assert_eq!(home.selected(&q), 2, "bottom must not wrap to the top");
    }

    /// The queue shrinks whenever an agent answers its own gate, which happens
    /// without the view being told. A stale index must never index past the end.
    #[test]
    fn a_cursor_left_past_the_end_of_a_shrunken_queue_lands_on_the_last_row() {
        let mut home = HomeState::default();
        let long = queue(5);
        for _ in 0..4 {
            home.select_next(&long);
        }
        let short = queue(2);

        assert_eq!(home.selected(&short), 1);
        assert_eq!(
            home.current(&short).map(|a| a.pane_id),
            Some(short[1].pane_id)
        );
    }

    #[test]
    fn a_queue_that_fits_never_scrolls() {
        let q = queue(4);
        let mut home = HomeState::default();
        for _ in 0..4 {
            home.select_next(&q);
        }
        assert_eq!(home.scroll(&q, 4), 0);
        assert_eq!(home.scroll(&q, 9), 0);
    }

    #[test]
    fn scrolling_keeps_the_cursor_on_screen_without_running_past_the_last_row() {
        let q = queue(10);
        let mut home = HomeState::default();
        assert_eq!(home.scroll(&q, 3), 0, "the top of the list does not scroll");
        for _ in 0..5 {
            home.select_next(&q);
        }
        assert_eq!(home.scroll(&q, 3), 3, "the cursor stays on the last row");
        for _ in 0..9 {
            home.select_next(&q);
        }
        assert_eq!(
            home.scroll(&q, 3),
            7,
            "the last screen is full rather than mostly blank"
        );
    }

    #[test]
    fn a_view_with_no_room_asks_for_no_scroll() {
        assert_eq!(HomeState::default().scroll(&queue(5), 0), 0);
    }

    #[test]
    fn injected_catalog_exposes_provider_models_and_model_specific_efforts() {
        let mut home = home_with_codex_catalog();
        assert_eq!(
            home.model_options()
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["default", "fable", "opus", "sonnet", "haiku"]
        );
        home.set_agent(Agent::Codex);
        home.set_model("gpt-5.6-sol");
        assert!(home.effort_options().iter().any(|effort| effort == "ultra"));
        home.set_model("gpt-5.6-luna");
        assert!(!home.effort_options().iter().any(|effort| effort == "ultra"));
    }

    #[test]
    fn changing_model_reconciles_an_unsupported_effort_to_auto() {
        let mut home = home_with_codex_catalog();
        home.set_agent(Agent::Codex);
        home.set_model("gpt-5.6-sol");
        home.effort = Some("ultra".into());

        home.set_model("gpt-5.6-luna");

        assert_eq!(home.effort.as_deref(), Some("auto"));
    }

    #[test]
    fn catalog_refresh_reconciles_a_removed_selected_model() {
        let mut home = home_with_codex_catalog();
        home.set_agent(Agent::Codex);
        home.set_model("gpt-5.6-sol");
        let replacement = super::super::home_catalog::parse_codex_catalog(
            br#"{"models":[
                {"slug":"new-model","visibility":"list","priority":1,"supported_reasoning_levels":[{"effort":"high"}]}
            ]}"#,
        )
        .expect("replacement catalog");

        home.replace_provider_catalog(replacement);

        assert_eq!(home.model, DEFAULT_MODEL);
        assert_eq!(home.effort.as_deref(), Some(AUTO_EFFORT));
    }

    #[test]
    fn catalog_refresh_clamps_an_open_picker_to_the_new_options() {
        let mut home = home_with_codex_catalog();
        home.set_agent(Agent::Codex);
        home.picker = Some(HomePicker::Model);
        home.picker_selected = home.model_options().len() - 1;
        let replacement = super::super::home_catalog::parse_codex_catalog(
            br#"{"models":[
                {"slug":"new-model","visibility":"list","priority":1,"supported_reasoning_levels":[{"effort":"high"}]}
            ]}"#,
        )
        .expect("replacement catalog");

        home.replace_provider_catalog(replacement);

        assert_eq!(home.picker, Some(HomePicker::Model));
        assert_eq!(home.picker_selected, home.model_options().len() - 1);
    }

    #[test]
    fn default_model_and_auto_effort_defer_to_the_provider() {
        let mut home = home_with_codex_catalog();
        home.prompt = "implement the retry cap".into();

        let plan = home.dispatch_plan().expect("prompt should dispatch");

        assert_eq!(home.model, "default");
        assert_eq!(home.effort.as_deref(), Some("auto"));
        assert_eq!(plan.argv, vec!["claude", "implement the retry cap"]);
    }

    /// Characterization: pins `dispatch_plan()` before the T3 card layout moves
    /// the fields around. Layout may change; the plan for the same inputs may
    /// not.
    #[test]
    fn dispatch_plan_is_frozen_for_a_fixed_set_of_inputs() {
        let mut home = home_with_codex_catalog();
        home.prompt = "  cap the retry loop\nand log it  ".into();
        home.set_agent(Agent::Codex);
        home.set_model("gpt-5.6-sol");
        home.effort = Some("ultra".into());
        home.directory = PathBuf::from("/tmp/frozen-plan");
        home.target = HomeTarget::Existing("space-7".into());

        let plan = home.dispatch_plan().expect("frozen inputs dispatch");

        assert_eq!(
            plan,
            HomeDispatchPlan {
                agent: Agent::Codex,
                model: "gpt-5.6-sol".into(),
                effort: Some("ultra".into()),
                directory: PathBuf::from("/tmp/frozen-plan"),
                target: HomeTarget::Existing("space-7".into()),
                prompt: "cap the retry loop\nand log it".into(),
                argv: vec![
                    "codex".into(),
                    "--model".into(),
                    "gpt-5.6-sol".into(),
                    "-c".into(),
                    "model_reasoning_effort=ultra".into(),
                    "cap the retry loop\nand log it".into(),
                ],
            }
        );
    }

    #[test]
    fn launch_argv_uses_exact_provider_flags_for_explicit_choices() {
        let mut home = home_with_codex_catalog();
        home.prompt = "implement the retry cap".into();
        home.set_model("fable");
        home.effort = Some("max".into());
        assert_eq!(
            home.dispatch_plan().expect("Claude plan").argv,
            vec![
                "claude",
                "--model",
                "fable",
                "--effort",
                "max",
                "implement the retry cap",
            ]
        );

        home.set_agent(Agent::Codex);
        home.set_model("gpt-5.6-sol");
        home.effort = Some("ultra".into());
        assert_eq!(
            home.dispatch_plan().expect("Codex plan").argv,
            vec![
                "codex",
                "--model",
                "gpt-5.6-sol",
                "-c",
                "model_reasoning_effort=ultra",
                "implement the retry cap",
            ]
        );
    }
}
