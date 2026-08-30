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

use std::path::PathBuf;

use crate::{app::inbox::BlockedAgent, detect::Agent};

pub(crate) const HOME_COMPOSER_MIN_HEIGHT: u16 = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HomeFocus {
    Prompt,
    Agent,
    Model,
    Effort,
    Directory,
    Target,
}

impl HomeFocus {
    pub(crate) fn next(self, effort_visible: bool) -> Self {
        match self {
            Self::Prompt => Self::Agent,
            Self::Agent => Self::Model,
            Self::Model if effort_visible => Self::Effort,
            Self::Model => Self::Directory,
            Self::Effort => Self::Directory,
            Self::Directory => Self::Target,
            Self::Target => Self::Prompt,
        }
    }

    pub(crate) fn previous(self, effort_visible: bool) -> Self {
        match self {
            Self::Prompt => Self::Target,
            Self::Agent => Self::Prompt,
            Self::Model => Self::Agent,
            Self::Effort => Self::Model,
            Self::Directory if effort_visible => Self::Effort,
            Self::Directory => Self::Model,
            Self::Target => Self::Directory,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HomePicker {
    Agent,
    Model,
    Effort,
    Directory,
    Target,
}

impl HomePicker {
    pub(crate) fn for_focus(focus: HomeFocus) -> Option<Self> {
        match focus {
            HomeFocus::Prompt => None,
            HomeFocus::Agent => Some(Self::Agent),
            HomeFocus::Model => Some(Self::Model),
            HomeFocus::Effort => Some(Self::Effort),
            HomeFocus::Directory => Some(Self::Directory),
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

fn default_directory() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

pub(crate) fn model_options(agent: Agent) -> &'static [&'static str] {
    match agent {
        Agent::Claude => &["opus", "sonnet", "haiku"],
        Agent::Codex => &["gpt-5.4", "gpt-5.3-codex"],
        _ => &[],
    }
}

pub(crate) fn effort_options(agent: Agent) -> &'static [&'static str] {
    match agent {
        Agent::Claude => &["low", "medium", "high"],
        Agent::Codex => &["low", "medium", "high", "xhigh"],
        _ => &[],
    }
}

pub(crate) fn dispatchable_agents() -> &'static [Agent] {
    &[Agent::Claude, Agent::Codex]
}

/// Cursor and dispatch state for an open home view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HomeState {
    selected: usize,
    pub(crate) focus: Option<HomeFocus>,
    pub(crate) prompt: String,
    pub(crate) agent: Agent,
    pub(crate) model: String,
    pub(crate) effort: Option<String>,
    pub(crate) directory: PathBuf,
    pub(crate) target: HomeTarget,
    pub(crate) picker: Option<HomePicker>,
    pub(crate) picker_selected: usize,
}

impl Default for HomeState {
    fn default() -> Self {
        Self {
            selected: 0,
            focus: Some(HomeFocus::Prompt),
            prompt: String::new(),
            agent: Agent::Claude,
            model: "opus".into(),
            effort: Some("medium".into()),
            directory: default_directory(),
            target: HomeTarget::NewSpace,
            picker: None,
            picker_selected: 0,
        }
    }
}

impl HomeState {
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
    }

    /// Move down one row, stopping at the end.
    ///
    /// Deliberately does not wrap. The list is sorted by how long each agent has
    /// been waiting, so the ends mean something — arriving at the bottom should
    /// read as "that is all of them", not put you back on the oldest.
    pub(crate) fn select_next(&mut self, queue: &[BlockedAgent]) {
        if queue.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected(queue) + 1).min(queue.len() - 1);
    }

    /// Move up one row, stopping at the top.
    pub(crate) fn select_prev(&mut self, queue: &[BlockedAgent]) {
        self.selected = self.selected(queue).saturating_sub(1);
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
        self.model = model_options(agent)
            .first()
            .copied()
            .unwrap_or_default()
            .into();
        self.effort = effort_options(agent).first().copied().map(str::to_owned);
    }

    pub(crate) fn append_prompt(&mut self, character: char) {
        self.prompt.push(character);
    }

    pub(crate) fn backspace_prompt(&mut self) {
        self.prompt.pop();
    }

    pub(crate) fn dispatch_plan(&self) -> Result<HomeDispatchPlan, String> {
        let prompt = self.prompt.trim();
        if prompt.is_empty() {
            return Err("enter a prompt before dispatching".into());
        }
        if model_options(self.agent).is_empty() {
            return Err("that agent cannot be dispatched from home".into());
        }

        let mut argv = vec![crate::detect::interactive_agent_executable(self.agent).into()];
        match self.agent {
            Agent::Claude => {
                argv.extend(["--model".into(), self.model.clone()]);
                if let Some(effort) = &self.effort {
                    argv.extend(["--effort".into(), effort.clone()]);
                }
            }
            Agent::Codex => {
                argv.extend(["--model".into(), self.model.clone()]);
                if let Some(effort) = &self.effort {
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
    pub(crate) fn toggle_home(&mut self) {
        self.home = match self.home.take() {
            Some(_) => None,
            None => {
                // Home and the inbox both want the whole frame; opening one puts
                // the other away rather than stacking two overlays.
                self.inbox = None;
                Some(HomeState::default())
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
            self.home = Some(HomeState::default());
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
                .map(|home| model_options(home.agent).len())
                .unwrap_or(0),
            HomePicker::Effort => self
                .home
                .as_ref()
                .map(|home| effort_options(home.agent).len())
                .unwrap_or(0),
            HomePicker::Directory => self.home_directory_options().len(),
            HomePicker::Target => self.home_target_options().len(),
        }
    }

    pub(crate) fn home_open_picker(&mut self, picker: HomePicker) {
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
                HomePicker::Model => model_options(home.agent)
                    .iter()
                    .position(|model| *model == home.model),
                HomePicker::Effort => home.effort.as_deref().and_then(|effort| {
                    effort_options(home.agent)
                        .iter()
                        .position(|option| *option == effort)
                }),
                HomePicker::Directory => self
                    .home_directory_options()
                    .iter()
                    .position(|directory| directory == &home.directory),
                HomePicker::Target => self
                    .home_target_options()
                    .iter()
                    .position(|target| target == &home.target),
            })
            .unwrap_or(0);
        if let Some(home) = self.home.as_mut() {
            home.picker = Some(picker);
            home.picker_selected = current.min(length - 1);
        }
    }

    pub(crate) fn home_move_picker(&mut self, delta: i32) {
        let Some((picker, selected)) = self
            .home
            .as_ref()
            .and_then(|home| home.picker.map(|picker| (picker, home.picker_selected)))
        else {
            return;
        };
        let length = self.home_picker_len(picker);
        if length == 0 {
            return;
        }
        let selected = (selected as i32 + delta).clamp(0, length as i32 - 1) as usize;
        if let Some(home) = self.home.as_mut() {
            home.picker_selected = selected;
        }
    }

    pub(crate) fn home_accept_picker(&mut self) {
        let Some((picker, selected)) = self
            .home
            .as_ref()
            .and_then(|home| home.picker.map(|picker| (picker, home.picker_selected)))
        else {
            return;
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
                    .and_then(|home| model_options(home.agent).get(selected))
                    .copied()
                    .map(str::to_owned);
                if let Some(model) = model {
                    if let Some(home) = self.home.as_mut() {
                        home.model = model;
                    }
                }
            }
            HomePicker::Effort => {
                let effort = self
                    .home
                    .as_ref()
                    .and_then(|home| effort_options(home.agent).get(selected))
                    .copied()
                    .map(str::to_owned);
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
}
