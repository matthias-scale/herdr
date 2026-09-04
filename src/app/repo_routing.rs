//! Placement of panes into the workspace bound to the repository they work on.
//!
//! Organising workspaces by repository decays on its own: a pane is created in
//! whichever workspace the session happens to be in, so a grouping built by
//! hand collects unrelated work again within hours. This module closes that
//! loop by routing a pane once it resolves which repository it works on.
//!
//! Two properties matter more than coverage:
//!
//! * **Placement is never derived from prose, and never from cwd alone when a
//!   declaration exists.** The repository is read from the pane's *effective*
//!   work context, whose tier order puts declarations above the cwd-derived
//!   git observation. Sessions routinely run from one shared worktree while
//!   operating on a different repository, so a cwd-driven rule would misfile
//!   exactly the panes that matter.
//! * **Placement never takes focus.** Moves are issued with `focus: false`,
//!   and the pane the human is currently focused in is never moved at all. An
//!   agent tidying workspaces must not yank a cursor mid-keystroke.

use crate::api::schema::{Method, PaneMoveDestination, PaneMoveParams};
use crate::layout::PaneId;

use super::App;

/// Why a pane was or was not routed. Every non-routing outcome is named so the
/// caller can report precisely where the mechanism declined to act.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RepoRouteDecision {
    /// The pane has not resolved a repository: no declaration, and either no
    /// checkout or no usable `origin` remote.
    RepoUnresolved,
    /// The pane knows its repository but no workspace claims it.
    NoBoundWorkspace,
    /// The pane already sits in the workspace bound to its repository.
    AlreadyPlaced,
    /// The pane is the one the human is currently focused in. Moving it would
    /// disturb live work, so it is left where it is and routed later, once
    /// focus has moved on.
    HeldByFocus,
    /// The pane should move to this workspace.
    Route { target_ws_idx: usize },
}

impl App {
    /// Repository the pane works on, as resolved by work-context tier order.
    pub(crate) fn pane_effective_repo(&self, ws_idx: usize, pane_id: PaneId) -> Option<String> {
        let terminal_id = self
            .state
            .workspaces
            .get(ws_idx)?
            .tabs
            .iter()
            .find_map(|tab| tab.panes.get(&pane_id))
            .map(|pane| pane.attached_terminal_id.clone())?;
        self.state
            .terminals
            .get(&terminal_id)?
            .effective_work_context()
            .repo
            .clone()
    }

    /// Current workspace index of a pane. Routing moves panes between
    /// workspaces, so a caller holding a pre-move index must re-resolve.
    pub(crate) fn workspace_index_for_pane(&self, pane_id: PaneId) -> Option<usize> {
        self.state
            .workspaces
            .iter()
            .position(|ws| ws.tabs.iter().any(|tab| tab.panes.contains_key(&pane_id)))
    }

    /// Index of the workspace bound to `repo`, if any.
    ///
    /// Bindings are compared case-insensitively because GitHub preserves owner
    /// casing without distinguishing on it; `matthiasSchedel/ghx` and
    /// `matthiasschedel/ghx` must not become two spaces.
    pub(crate) fn workspace_bound_to_repo(&self, repo: &str) -> Option<usize> {
        self.state.workspaces.iter().position(|ws| {
            ws.repo_binding
                .as_deref()
                .is_some_and(|bound| crate::work_context::repo_slugs_match(bound, repo))
        })
    }

    /// Decide, without mutating anything, where a pane belongs.
    pub(crate) fn repo_route_decision(&self, ws_idx: usize, pane_id: PaneId) -> RepoRouteDecision {
        let Some(repo) = self.pane_effective_repo(ws_idx, pane_id) else {
            return RepoRouteDecision::RepoUnresolved;
        };
        let Some(target_ws_idx) = self.workspace_bound_to_repo(&repo) else {
            return RepoRouteDecision::NoBoundWorkspace;
        };
        if target_ws_idx == ws_idx {
            return RepoRouteDecision::AlreadyPlaced;
        }
        if self.pane_is_human_focused(ws_idx, pane_id) {
            return RepoRouteDecision::HeldByFocus;
        }
        RepoRouteDecision::Route { target_ws_idx }
    }

    /// Whether this pane is the one the human is currently focused in.
    ///
    /// Session focus, not tab-layout focus: every tab has a focused pane, but
    /// only the active workspace's is the one under the cursor.
    pub(crate) fn pane_is_human_focused(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        if self.state.active != Some(ws_idx) {
            return false;
        }
        self.state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.focused_pane_id())
            == Some(pane_id)
    }

    /// Route a pane to its bound workspace if it has one. Returns the decision
    /// that was acted on.
    pub(crate) fn route_pane_to_bound_workspace(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
    ) -> RepoRouteDecision {
        let decision = self.repo_route_decision(ws_idx, pane_id);
        let RepoRouteDecision::Route { target_ws_idx } = decision else {
            return decision;
        };
        let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) else {
            return RepoRouteDecision::RepoUnresolved;
        };
        let target_workspace_id = self.public_workspace_id(target_ws_idx);

        // Reuse the ordinary move path so layout, events, persistence and the
        // focus rules established for socket-initiated moves all apply. `focus`
        // is false: this move is bookkeeping, not navigation.
        let _ = self.dispatch_api_request(
            "repo-routing",
            Method::PaneMove(PaneMoveParams {
                pane_id: public_pane_id,
                destination: PaneMoveDestination::NewTab {
                    workspace_id: Some(target_workspace_id),
                    label: None,
                },
                focus: false,
            }),
        );
        decision
    }

    /// Route every pane whose repository resolves to a bound workspace.
    ///
    /// Used after a binding changes, so an existing pile of panes is sorted
    /// once rather than only new work being placed correctly.
    pub(crate) fn reconcile_repo_routing(&mut self) -> Vec<RepoRouteDecision> {
        let mut decisions = Vec::new();
        // Collected up front because routing mutates workspace membership, and
        // re-resolved by workspace id on each step for the same reason.
        let candidates: Vec<(String, PaneId)> = self
            .state
            .workspaces
            .iter()
            .flat_map(|ws| {
                let workspace_id = ws.id.clone();
                ws.tabs
                    .iter()
                    .flat_map(|tab| tab.panes.keys().copied())
                    .map(move |pane_id| (workspace_id.clone(), pane_id))
                    .collect::<Vec<_>>()
            })
            .collect();

        for (workspace_id, pane_id) in candidates {
            let Some(ws_idx) = self
                .state
                .workspaces
                .iter()
                .position(|ws| ws.id == workspace_id)
            else {
                continue;
            };
            if !self.state.workspaces[ws_idx]
                .tabs
                .iter()
                .any(|tab| tab.panes.contains_key(&pane_id))
            {
                continue;
            }
            decisions.push(self.route_pane_to_bound_workspace(ws_idx, pane_id));
        }
        decisions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::work_context::{PaneWorkContext, PaneWorkContextPatch};
    use crate::workspace::Workspace;

    /// Two workspaces, one bound to `owner/bound`, plus an unbound grab-bag
    /// workspace that new panes land in — the shape that decays in practice.
    fn app_with_bound_workspace() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![
            Workspace::test_new("grab-bag"),
            Workspace::test_new("bound"),
        ];
        app.state.ensure_test_terminals();
        app.state.workspaces[1].repo_binding = Some("owner/bound".into());
        // The grab-bag keeps a second, unrelated pane. A real one holds many,
        // and it means a routed pane does not empty and close the workspace,
        // which would renumber every index the assertions rely on.
        app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        seed_terminals(&mut app);
        app
    }

    fn seed_terminals(app: &mut App) {
        for ws in &app.state.workspaces {
            for tab in &ws.tabs {
                for pane in tab.panes.values() {
                    app.state
                        .terminals
                        .entry(pane.attached_terminal_id.clone())
                        .or_insert_with(|| {
                            crate::terminal::TerminalState::new(
                                pane.attached_terminal_id.clone(),
                                std::path::PathBuf::from("/herdr-test"),
                            )
                        });
                }
            }
        }
    }

    fn terminal_id(app: &App, ws_idx: usize, pane_id: PaneId) -> crate::terminal::TerminalId {
        app.state.workspaces[ws_idx]
            .tabs
            .iter()
            .find_map(|tab| tab.panes.get(&pane_id))
            .map(|pane| pane.attached_terminal_id.clone())
            .expect("pane must exist")
    }

    /// Record what the background poller saw at the pane's cwd.
    fn observe_git_repo(app: &mut App, ws_idx: usize, pane_id: PaneId, repo: &str) {
        let id = terminal_id(app, ws_idx, pane_id);
        app.state
            .terminals
            .get_mut(&id)
            .expect("terminal")
            .replace_git_work_context(PaneWorkContext {
                repo: Some(repo.into()),
                ..PaneWorkContext::default()
            })
            .expect("git observation must normalize");
    }

    /// Record what the agent declared about the work it is actually doing.
    fn declare_repo(app: &mut App, ws_idx: usize, pane_id: PaneId, repo: &str) {
        let id = terminal_id(app, ws_idx, pane_id);
        app.state
            .terminals
            .get_mut(&id)
            .expect("terminal")
            .apply_manual_work_context_patch(PaneWorkContextPatch {
                repo: Some(repo.into()),
                ..PaneWorkContextPatch::default()
            })
            .expect("declaration must normalize");
    }

    #[test]
    fn observed_repo_routes_a_pane_into_its_bound_workspace() {
        let mut app = app_with_bound_workspace();
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        observe_git_repo(&mut app, 0, pane, "owner/bound");

        let decision = app.route_pane_to_bound_workspace(0, pane);

        assert_eq!(decision, RepoRouteDecision::Route { target_ws_idx: 1 });
        assert_eq!(
            app.workspace_index_for_pane(pane),
            Some(1),
            "the pane must end up in the workspace bound to its repository"
        );
    }

    /// The headline property. The pane's checkout points at one repository
    /// while the agent works on another — the shared-worktree case. Routing
    /// must follow the declaration, not the checkout.
    #[test]
    fn a_misleading_cwd_does_not_misfile_a_pane_that_declared_its_repository() {
        let mut app = app_with_bound_workspace();
        app.state.workspaces.push(Workspace::test_new("decoy"));
        app.state.ensure_test_terminals();
        seed_terminals(&mut app);
        app.state.workspaces[2].repo_binding = Some("owner/shared-worktree".into());
        let pane = app.state.workspaces[0].tabs[0].root_pane;

        // What the cwd says: the shared worktree everything is launched from.
        observe_git_repo(&mut app, 0, pane, "owner/shared-worktree");
        // What the session is actually doing.
        declare_repo(&mut app, 0, pane, "owner/bound");

        assert_eq!(
            app.pane_effective_repo(0, pane).as_deref(),
            Some("owner/bound"),
            "a declaration must outrank the cwd-derived observation"
        );

        let decision = app.route_pane_to_bound_workspace(0, pane);

        assert_eq!(decision, RepoRouteDecision::Route { target_ws_idx: 1 });
        assert_eq!(
            app.workspace_index_for_pane(pane),
            Some(1),
            "the pane must follow its declared repository, not its checkout"
        );
        assert!(
            app.state.workspaces[2]
                .tabs
                .iter()
                .all(|tab| !tab.panes.contains_key(&pane)),
            "the pane must never land in the workspace its misleading cwd points at"
        );
    }

    /// A later cwd observation must not drag a declared pane back out.
    #[test]
    fn a_later_cwd_observation_cannot_override_an_existing_declaration() {
        let mut app = app_with_bound_workspace();
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        declare_repo(&mut app, 0, pane, "owner/bound");
        observe_git_repo(&mut app, 0, pane, "owner/shared-worktree");

        assert_eq!(
            app.pane_effective_repo(0, pane).as_deref(),
            Some("owner/bound")
        );
    }

    #[test]
    fn routing_never_moves_session_focus() {
        let mut app = app_with_bound_workspace();
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        // The human sits on a sibling pane in the same workspace, so the
        // routed pane itself is not the focused one.
        let sibling = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        seed_terminals(&mut app);
        app.state.workspaces[0].tabs[0].layout.focus_pane(sibling);
        app.state.active = Some(0);
        observe_git_repo(&mut app, 0, pane, "owner/bound");

        app.route_pane_to_bound_workspace(0, pane);

        assert_eq!(
            app.state.active,
            Some(0),
            "an automatic move must leave the human where they were"
        );
        assert_eq!(
            app.state.workspaces[0].tabs[0].layout.focused(),
            sibling,
            "the human's pane must stay focused in its own workspace"
        );
    }

    #[test]
    fn the_pane_the_human_is_focused_in_is_never_moved() {
        let mut app = app_with_bound_workspace();
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0].tabs[0].layout.focus_pane(pane);
        app.state.active = Some(0);
        observe_git_repo(&mut app, 0, pane, "owner/bound");

        let decision = app.route_pane_to_bound_workspace(0, pane);

        assert_eq!(decision, RepoRouteDecision::HeldByFocus);
        assert_eq!(
            app.workspace_index_for_pane(pane),
            Some(0),
            "the pane under the cursor must stay put"
        );
    }

    /// Focus is a hold, not a veto: once the human moves on, the pane routes.
    #[test]
    fn a_pane_held_by_focus_routes_once_focus_moves_away() {
        let mut app = app_with_bound_workspace();
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0].tabs[0].layout.focus_pane(pane);
        app.state.active = Some(0);
        observe_git_repo(&mut app, 0, pane, "owner/bound");
        assert_eq!(
            app.route_pane_to_bound_workspace(0, pane),
            RepoRouteDecision::HeldByFocus
        );

        app.state.active = Some(1);

        assert_eq!(
            app.route_pane_to_bound_workspace(0, pane),
            RepoRouteDecision::Route { target_ws_idx: 1 }
        );
        assert_eq!(app.workspace_index_for_pane(pane), Some(1));
    }

    #[test]
    fn a_pane_without_a_resolved_repository_is_left_alone() {
        let mut app = app_with_bound_workspace();
        let pane = app.state.workspaces[0].tabs[0].root_pane;

        assert_eq!(
            app.route_pane_to_bound_workspace(0, pane),
            RepoRouteDecision::RepoUnresolved
        );
        assert_eq!(app.workspace_index_for_pane(pane), Some(0));
    }

    #[test]
    fn a_repository_no_workspace_claims_does_not_move_a_pane() {
        let mut app = app_with_bound_workspace();
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        observe_git_repo(&mut app, 0, pane, "owner/unclaimed");

        assert_eq!(
            app.route_pane_to_bound_workspace(0, pane),
            RepoRouteDecision::NoBoundWorkspace
        );
        assert_eq!(app.workspace_index_for_pane(pane), Some(0));
    }

    #[test]
    fn a_pane_already_in_its_bound_workspace_is_not_moved_again() {
        let mut app = app_with_bound_workspace();
        let pane = app.state.workspaces[1].tabs[0].root_pane;
        observe_git_repo(&mut app, 1, pane, "owner/bound");

        assert_eq!(
            app.route_pane_to_bound_workspace(1, pane),
            RepoRouteDecision::AlreadyPlaced
        );
    }

    #[test]
    fn binding_lookup_ignores_owner_casing() {
        let mut app = app_with_bound_workspace();
        app.state.workspaces[1].repo_binding = Some("matthiasSchedel/ghx".into());

        assert_eq!(app.workspace_bound_to_repo("matthiasschedel/ghx"), Some(1));
        assert_eq!(app.workspace_bound_to_repo("MatthiasSchedel/GHX"), Some(1));
    }

    #[test]
    fn reconcile_sorts_an_accumulated_workspace_without_taking_focus() {
        let mut app = app_with_bound_workspace();
        let first = app.state.workspaces[0].tabs[0].root_pane;
        let second = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        seed_terminals(&mut app);
        app.state.workspaces[0].tabs[0].layout.focus_pane(second);
        app.state.active = Some(0);
        observe_git_repo(&mut app, 0, first, "owner/bound");
        declare_repo(&mut app, 0, second, "owner/bound");

        app.reconcile_repo_routing();

        assert_eq!(
            app.workspace_index_for_pane(first),
            Some(1),
            "an unfocused pane is sorted"
        );
        assert_eq!(
            app.workspace_index_for_pane(second),
            Some(0),
            "the focused pane is held back rather than yanked away"
        );
        assert_eq!(app.state.active, Some(0));
    }
}
