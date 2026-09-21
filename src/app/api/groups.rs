use crate::api::schema::{
    AuthorityMutation, AuthorityMutationParams, GroupCreateParams, GroupDeleteParams,
    GroupRenameParams, PaneGroupSetParams, Request, ResponseResult,
};
use crate::app::App;
use crate::groups::{
    GroupAuthoritySnapshot, GroupState, MutationError, OwnedPaneMembership, PaneGroupMembership,
    RuntimeError,
};

use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_group_host_snapshot(&self, id: String) -> String {
        let authority = match self.group_runtime.authority() {
            Ok(authority) => authority,
            Err(error) => return encode_runtime_error(id, error),
        };
        let mut memberships = Vec::new();
        for (workspace_index, workspace) in self.state.workspaces.iter().enumerate() {
            for tab in &workspace.tabs {
                for (pane_id, pane) in &tab.panes {
                    if self
                        .terminal_runtimes
                        .get(&pane.attached_terminal_id)
                        .is_some_and(crate::terminal::TerminalRuntime::is_remote_proxy)
                    {
                        continue;
                    }
                    if let Some(public_id) = self.public_pane_id(workspace_index, *pane_id) {
                        memberships.push(OwnedPaneMembership {
                            pane_id: public_id,
                            membership: pane.group_membership.clone(),
                        });
                    }
                }
            }
        }
        memberships.sort_by(|left, right| left.pane_id.cmp(&right.pane_id));
        encode_success(
            id,
            ResponseResult::GroupHostSnapshot {
                snapshot: GroupAuthoritySnapshot {
                    authority_id: authority.authority_id().clone(),
                    revision: authority.revision(),
                    groups: authority.records(),
                    memberships,
                },
            },
        )
    }

    pub(super) fn handle_group_create(&mut self, id: String, params: GroupCreateParams) -> String {
        match self
            .group_runtime
            .create(&params.name, params.expected_revision)
        {
            Ok((record, revision)) => {
                encode_success(id, ResponseResult::GroupMutation { record, revision })
            }
            Err(error) => encode_runtime_error(id, error),
        }
    }

    pub(super) fn handle_group_rename(&mut self, id: String, params: GroupRenameParams) -> String {
        if !self.local_group_owner(&params.group_id.owner) {
            let authority = params.group_id.owner.clone();
            return self.route_authority_mutation(id, authority, AuthorityMutation::Rename(params));
        }
        self.apply_group_rename(id, params)
    }

    fn apply_group_rename(&mut self, id: String, params: GroupRenameParams) -> String {
        match self
            .group_runtime
            .rename(&params.group_id, &params.name, params.expected_revision)
        {
            Ok((record, revision)) => {
                encode_success(id, ResponseResult::GroupMutation { record, revision })
            }
            Err(error) => encode_runtime_error(id, error),
        }
    }

    pub(super) fn handle_group_delete(&mut self, id: String, params: GroupDeleteParams) -> String {
        if !self.local_group_owner(&params.group_id.owner) {
            let authority = params.group_id.owner.clone();
            return self.route_authority_mutation(id, authority, AuthorityMutation::Delete(params));
        }
        self.apply_group_delete(id, params)
    }

    fn apply_group_delete(&mut self, id: String, params: GroupDeleteParams) -> String {
        match self
            .group_runtime
            .delete(&params.group_id, params.expected_revision)
        {
            Ok((record, revision)) => {
                encode_success(id, ResponseResult::GroupMutation { record, revision })
            }
            Err(error) => encode_runtime_error(id, error),
        }
    }

    pub(super) fn handle_pane_group_set(
        &mut self,
        id: String,
        params: PaneGroupSetParams,
    ) -> String {
        let local_pane =
            self.parse_pane_id(&params.pane_id)
                .and_then(|(workspace_index, pane_id)| {
                    self.state.workspaces[workspace_index]
                        .pane_state(pane_id)
                        .filter(|pane| {
                            !self
                                .terminal_runtimes
                                .get(&pane.attached_terminal_id)
                                .is_some_and(crate::terminal::TerminalRuntime::is_remote_proxy)
                        })
                        .map(|_| (workspace_index, pane_id))
                });
        if local_pane.is_none() {
            let owners = self
                .state
                .fleet_snapshot
                .group_catalogs
                .iter()
                .filter(|catalog| {
                    catalog.snapshot.as_ref().is_some_and(|snapshot| {
                        snapshot
                            .memberships
                            .iter()
                            .any(|membership| membership.pane_id == params.pane_id)
                    })
                })
                .filter_map(|catalog| catalog.authority_id().cloned())
                .collect::<Vec<_>>();
            if owners.len() != 1 {
                return encode_error(id, "pane_not_found", "pane owner is not uniquely known");
            }
            let pane_owner = owners[0].clone();
            if let Err(message) = self.state.fleet_snapshot.fresh_group_catalog(&pane_owner) {
                return encode_error(id, "authority_not_fresh", message);
            }
            if let Some(group_id) = params.group_id.as_ref() {
                if let Err(message) = self.validate_group_target(group_id) {
                    return encode_error(id, "authority_not_fresh", message);
                }
            }
            return self.route_authority_mutation(
                id,
                pane_owner,
                AuthorityMutation::PaneGroupSet(params),
            );
        }

        if let Some(group_id) = params.group_id.as_ref() {
            if let Err(message) = self.validate_group_target(group_id) {
                return encode_error(id, "authority_not_fresh", message);
            }
        }
        self.apply_pane_group_set(id, params)
    }

    fn apply_pane_group_set(&mut self, id: String, params: PaneGroupSetParams) -> String {
        if self.no_session {
            return encode_error(
                id,
                "group_persistence_disabled",
                "pane membership requires session persistence",
            );
        }
        let Some((workspace_index, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(pane) = self.state.workspaces[workspace_index].pane_state(pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        if self
            .terminal_runtimes
            .get(&pane.attached_terminal_id)
            .is_some_and(crate::terminal::TerminalRuntime::is_remote_proxy)
        {
            return encode_error(id, "pane_not_owned", "pane is not owned by this server");
        }
        if pane.group_membership.revision != params.expected_revision {
            return revision_conflict(id, params.expected_revision, pane.group_membership.revision);
        }
        let Some(revision) = pane.group_membership.revision.checked_add(1) else {
            return encode_error(id, "revision_exhausted", "membership revision is exhausted");
        };
        let membership = PaneGroupMembership {
            group_id: params.group_id,
            revision,
        };

        let mut snapshot = crate::persist::capture(
            &self.state.workspaces,
            &self.state.terminals,
            &self.terminal_runtimes,
            self.state.active,
            self.state.selected,
            self.state.sidebar_width,
            self.state.sidebar_section_split,
            self.state.collapsed_space_keys.clone(),
            self.state.prio_panel_collapsed,
        );
        let Some(saved_pane) = snapshot
            .workspaces
            .iter_mut()
            .flat_map(|workspace| workspace.tabs.iter_mut())
            .find_map(|tab| tab.panes.get_mut(&pane_id.raw()))
        else {
            return encode_error(
                id,
                "pane_not_persistable",
                "pane is not in the session snapshot",
            );
        };
        saved_pane.group_membership = membership.clone();
        let history = self.persist_pane_history.then(|| {
            crate::persist::capture_history(
                &self.state.workspaces,
                &self.state.terminals,
                &self.terminal_runtimes,
            )
        });
        if let Err(error) = self.persist_session_candidate(snapshot, history) {
            return encode_error(id, "persistence_failed", error.to_string());
        }
        let Some(pane) = self.state.workspaces[workspace_index].pane_state_mut(pane_id) else {
            return encode_error(id, "pane_not_found", "pane disappeared after persistence");
        };
        pane.group_membership = membership.clone();
        encode_success(
            id,
            ResponseResult::PaneGroupSet {
                pane_id: params.pane_id,
                membership,
            },
        )
    }

    pub(super) fn handle_group_authority_mutate(
        &mut self,
        id: String,
        params: AuthorityMutationParams,
    ) -> String {
        if !params.forwarded {
            return encode_error(
                id,
                "invalid_authority_route",
                "authority mutation is receiver-only",
            );
        }
        let actual = match self.group_runtime.authority() {
            Ok(authority) => authority.authority_id().clone(),
            Err(error) => return encode_runtime_error(id, error),
        };
        if actual != params.expected_authority {
            return encode_error(
                id,
                "authority_mismatch",
                format!(
                    "expected authority {}, receiver owns {}",
                    params.expected_authority, actual
                ),
            );
        }
        match params.mutation {
            AuthorityMutation::Rename(params) => self.apply_group_rename(id, params),
            AuthorityMutation::Delete(params) => self.apply_group_delete(id, params),
            AuthorityMutation::PaneGroupSet(params) => {
                if let Some(group_id) = params.group_id.as_ref() {
                    if let Err(message) = self.validate_group_target(group_id) {
                        return encode_error(id, "authority_not_fresh", message);
                    }
                }
                self.apply_pane_group_set(id, params)
            }
        }
    }

    fn local_group_owner(&self, authority: &crate::groups::AuthorityId) -> bool {
        self.group_runtime
            .authority()
            .is_ok_and(|local| local.authority_id() == authority)
    }

    fn validate_group_target(&self, group_id: &crate::groups::GroupId) -> Result<(), String> {
        if self.local_group_owner(&group_id.owner) {
            let authority = self.group_runtime.authority().map_err(|error| {
                format!(
                    "local authority {} is unavailable: {error:?}",
                    group_id.owner
                )
            })?;
            let record = authority
                .record(group_id)
                .map_err(|_| format!("group {group_id:?} is unavailable"))?;
            return if matches!(record.state, GroupState::Deleted) {
                Err(format!(
                    "group authority {} reports a deleted group",
                    group_id.owner
                ))
            } else {
                Ok(())
            };
        }

        let catalog = self
            .state
            .fleet_snapshot
            .fresh_group_catalog(&group_id.owner)?;
        let record = catalog
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.groups.iter().find(|record| record.id == *group_id))
            .ok_or_else(|| {
                format!(
                    "authority {} does not report the target group",
                    group_id.owner
                )
            })?;
        if matches!(record.state, GroupState::Deleted) {
            return Err(format!(
                "authority {} reports a deleted group",
                group_id.owner
            ));
        }
        Ok(())
    }

    fn route_authority_mutation(
        &self,
        id: String,
        authority: crate::groups::AuthorityId,
        mutation: AuthorityMutation,
    ) -> String {
        let catalog = match self.state.fleet_snapshot.fresh_group_catalog(&authority) {
            Ok(catalog) => catalog.clone(),
            Err(message) => return encode_error(id, "authority_not_fresh", message),
        };
        let request = Request {
            id: id.clone(),
            method: crate::api::schema::Method::GroupAuthorityMutate(AuthorityMutationParams {
                expected_authority: authority,
                forwarded: true,
                mutation,
            }),
        };
        match crate::fleet::route_api_request(&catalog, &request, std::time::Duration::from_secs(5))
        {
            Ok(response) => response,
            Err(error) => encode_error(id, "authority_unreachable", error),
        }
    }
}

fn encode_runtime_error(id: String, error: RuntimeError) -> String {
    match error {
        RuntimeError::Unavailable(message) => {
            encode_error(id, "group_authority_unavailable", message)
        }
        RuntimeError::Persistence(message) => encode_error(id, "persistence_failed", message),
        RuntimeError::Mutation(MutationError::Deleted) => {
            encode_error(id, "group_deleted", "group is deleted")
        }
        RuntimeError::Mutation(MutationError::ForeignOwner) => {
            encode_error(id, "group_not_owned", "group is owned by another server")
        }
        RuntimeError::Mutation(MutationError::InvalidName) => {
            encode_error(id, "invalid_group_name", "group name must not be empty")
        }
        RuntimeError::Mutation(MutationError::NotFound) => {
            encode_error(id, "group_not_found", "group not found")
        }
        RuntimeError::Mutation(MutationError::RevisionConflict { expected, actual }) => {
            revision_conflict(id, expected, actual)
        }
        RuntimeError::Mutation(MutationError::RevisionExhausted) => {
            encode_error(id, "revision_exhausted", "group revision is exhausted")
        }
    }
}

fn revision_conflict(id: String, expected: u64, actual: u64) -> String {
    encode_error(
        id,
        "revision_conflict",
        format!("expected revision {expected}, current revision is {actual}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{
        GroupDeleteParams, GroupRenameParams, Method, PaneMoveDestination, PaneMoveParams,
        SplitDirection, SuccessResponse,
    };
    use crate::groups::{GroupId, GroupRecord};
    use crate::workspace::Workspace;
    use std::path::PathBuf;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let unique = format!(
                "herdr-group-api-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            );
            Self(std::env::temp_dir().join(unique))
        }

        fn session_paths(&self) -> (PathBuf, PathBuf) {
            (
                self.0.join("session.json"),
                self.0.join("session-history.json"),
            )
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn app_with_groups(name: &str) -> (App, TestDir, String) {
        let dir = TestDir::new(name);
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.no_session = false;
        app.group_runtime = crate::groups::Runtime::load(&dir.0.join("groups"));
        app.group_session_paths_override = Some(dir.session_paths());
        app.state.workspaces = vec![Workspace::test_new("source")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let public_id = app.public_pane_id(0, pane_id).expect("public pane id");
        (app, dir, public_id)
    }

    fn created_group(response: &str) -> GroupRecord {
        let response: SuccessResponse = serde_json::from_str(response).expect("success response");
        let ResponseResult::GroupMutation { record, .. } = response.result else {
            panic!("expected group mutation response");
        };
        record
    }

    #[test]
    fn create_rename_delete_and_snapshot_keep_one_group_identity() {
        let (mut app, dir, _) = app_with_groups("lifecycle");
        let created = created_group(&app.dispatch_api_request(
            "create",
            Method::GroupCreate(GroupCreateParams {
                name: " Work ".into(),
                expected_revision: 0,
            }),
        ));
        let renamed = created_group(&app.dispatch_api_request(
            "rename",
            Method::GroupRename(GroupRenameParams {
                group_id: created.id.clone(),
                name: "Focus".into(),
                expected_revision: created.revision,
            }),
        ));
        assert_eq!(renamed.id, created.id);
        let deleted = created_group(&app.dispatch_api_request(
            "delete",
            Method::GroupDelete(GroupDeleteParams {
                group_id: created.id.clone(),
                expected_revision: renamed.revision,
            }),
        ));
        assert!(matches!(deleted.state, GroupState::Deleted));

        app.group_runtime = crate::groups::Runtime::load(&dir.0.join("groups"));
        let response: SuccessResponse = serde_json::from_str(
            &app.dispatch_api_request("snapshot", Method::GroupHostSnapshot(Default::default())),
        )
        .expect("snapshot response");
        let ResponseResult::GroupHostSnapshot { snapshot } = response.result else {
            panic!("expected host snapshot");
        };
        assert_eq!(snapshot.groups, vec![deleted]);
    }

    #[test]
    fn two_creates_from_the_same_base_revision_have_one_winner() {
        let (mut app, _dir, _) = app_with_groups("create-conflict");
        let first = app.handle_group_create(
            "first".into(),
            GroupCreateParams {
                name: "One".into(),
                expected_revision: 0,
            },
        );
        let second = app.handle_group_create(
            "second".into(),
            GroupCreateParams {
                name: "Two".into(),
                expected_revision: 0,
            },
        );

        assert!(serde_json::from_str::<SuccessResponse>(&first).is_ok());
        let second: serde_json::Value = serde_json::from_str(&second).unwrap();
        assert_eq!(second["error"]["code"], "revision_conflict");
    }

    #[test]
    fn delete_rejects_a_stale_expected_revision() {
        let (mut app, _dir, _) = app_with_groups("delete-conflict");
        let created = created_group(&app.handle_group_create(
            "create".into(),
            GroupCreateParams {
                name: "Work".into(),
                expected_revision: 0,
            },
        ));
        let renamed = created_group(&app.handle_group_rename(
            "rename".into(),
            GroupRenameParams {
                group_id: created.id.clone(),
                name: "Focus".into(),
                expected_revision: created.revision,
            },
        ));

        let response: serde_json::Value = serde_json::from_str(&app.handle_group_delete(
            "delete".into(),
            GroupDeleteParams {
                group_id: created.id,
                expected_revision: created.revision,
            },
        ))
        .unwrap();

        assert_eq!(response["error"]["code"], "revision_conflict");
        assert!(response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains(&renamed.revision.to_string())));
    }

    #[test]
    fn membership_compare_and_set_is_durable_before_live_state_changes() {
        let (mut app, dir, pane_public_id) = app_with_groups("membership");
        let group = created_group(&app.handle_group_create(
            "create".into(),
            GroupCreateParams {
                name: "Work".into(),
                expected_revision: 0,
            },
        ));

        let first = app.handle_pane_group_set(
            "first".into(),
            PaneGroupSetParams {
                pane_id: pane_public_id.clone(),
                group_id: Some(group.id.clone()),
                expected_revision: 0,
            },
        );
        let first: SuccessResponse = serde_json::from_str(&first).expect("first assignment");
        let ResponseResult::PaneGroupSet { membership, .. } = first.result else {
            panic!("expected pane group response");
        };
        assert_eq!(membership.revision, 1);
        assert_eq!(membership.group_id.as_ref(), Some(&group.id));

        let loser: serde_json::Value = serde_json::from_str(&app.handle_pane_group_set(
            "loser".into(),
            PaneGroupSetParams {
                pane_id: pane_public_id,
                group_id: None,
                expected_revision: 0,
            },
        ))
        .unwrap();
        assert_eq!(loser["error"]["code"], "revision_conflict");

        let session_json = std::fs::read_to_string(dir.session_paths().0).unwrap();
        let snapshot: crate::persist::SessionSnapshot =
            serde_json::from_str(&session_json).expect("persisted session");
        let saved = snapshot.workspaces[0].tabs[0]
            .panes
            .values()
            .next()
            .expect("saved pane");
        assert_eq!(saved.group_membership, membership);
    }

    #[test]
    fn membership_persists_for_a_local_tab_behind_an_omitted_proxy_tab() {
        let (mut app, dir, _) = app_with_groups("membership-after-proxy");
        let local_tab = app.state.workspaces[0].test_add_tab(Some("later local"));
        app.state.ensure_test_terminals();
        let local_pane = app.state.workspaces[0].tabs[local_tab].root_pane;
        let local_public_id = app.public_pane_id(0, local_pane).expect("local pane id");

        let proxy_pane = crate::layout::PaneId::alloc();
        let proxy_terminal = crate::terminal::TerminalId::alloc();
        let (proxy_runtime, _channels) = crate::terminal::TerminalRuntime::spawn_remote_proxy(
            proxy_pane,
            24,
            80,
            0,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(crate::render_signal::RenderSignal::default()),
            crate::remote::RemoteFocusOperationState::new(),
        )
        .expect("proxy runtime");
        app.state.terminals.insert(
            proxy_terminal.clone(),
            crate::terminal::TerminalState::new(proxy_terminal.clone(), "/remote".into()),
        );
        app.terminal_runtimes
            .insert(proxy_terminal.clone(), proxy_runtime);
        let events = app.state.workspaces[0].tabs[0].events.clone();
        let proxy_tab = app.state.workspaces[0].create_tab_from_existing_pane(
            crate::workspace::MovedPane {
                pane_id: proxy_pane,
                pane_state: crate::pane::PaneState::new(proxy_terminal),
            },
            Some("proxy only".into()),
            events,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(crate::render_signal::RenderSignal::default()),
        );
        assert!(app.state.workspaces[0].move_tab(proxy_tab, 1));
        assert_eq!(
            app.state.workspaces[0].find_tab_index_for_pane(local_pane),
            Some(2)
        );

        let response = app.handle_pane_group_set(
            "clear".into(),
            PaneGroupSetParams {
                pane_id: local_public_id,
                group_id: None,
                expected_revision: 0,
            },
        );
        assert!(
            serde_json::from_str::<SuccessResponse>(&response).is_ok(),
            "pane membership should persist by identity: {response}"
        );

        let session_json = std::fs::read_to_string(dir.session_paths().0).unwrap();
        let snapshot: crate::persist::SessionSnapshot =
            serde_json::from_str(&session_json).expect("persisted session");
        let saved = snapshot.workspaces[0]
            .tabs
            .iter()
            .find_map(|tab| tab.panes.get(&local_pane.raw()))
            .expect("local pane persisted after compacting proxy-only tab");
        assert_eq!(saved.group_membership.revision, 1);
        assert!(saved.group_membership.group_id.is_none());
    }

    #[test]
    fn failed_membership_write_leaves_the_live_pane_unchanged() {
        let (mut app, dir, pane_public_id) = app_with_groups("membership-failure");
        let group = created_group(&app.handle_group_create(
            "create".into(),
            GroupCreateParams {
                name: "Work".into(),
                expected_revision: 0,
            },
        ));
        let blocker = dir.0.join("not-a-directory");
        std::fs::write(&blocker, "block").unwrap();
        app.group_session_paths_override = Some((
            blocker.join("session.json"),
            blocker.join("session-history.json"),
        ));

        let response: serde_json::Value = serde_json::from_str(&app.handle_pane_group_set(
            "assign".into(),
            PaneGroupSetParams {
                pane_id: pane_public_id,
                group_id: Some(group.id),
                expected_revision: 0,
            },
        ))
        .unwrap();

        assert_eq!(response["error"]["code"], "persistence_failed");
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        assert_eq!(
            app.state.workspaces[0]
                .pane_state(pane_id)
                .unwrap()
                .group_membership,
            PaneGroupMembership::default()
        );
    }

    #[test]
    fn membership_follows_a_pane_when_its_public_locator_changes() {
        let (mut app, _dir, source_public_id) = app_with_groups("move");
        let group = created_group(&app.handle_group_create(
            "create".into(),
            GroupCreateParams {
                name: "Work".into(),
                expected_revision: 0,
            },
        ));
        let response = app.handle_pane_group_set(
            "assign".into(),
            PaneGroupSetParams {
                pane_id: source_public_id.clone(),
                group_id: Some(group.id.clone()),
                expected_revision: 0,
            },
        );
        assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        let source_pane = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces.push(Workspace::test_new("target"));
        let target_tab_index = app.state.workspaces[1].test_add_tab(Some("destination"));
        app.state.ensure_test_terminals();
        let target_pane = app.state.workspaces[1].tabs[target_tab_index].root_pane;
        let target_tab_id = app.public_tab_id(1, target_tab_index).unwrap();
        let target_pane_id = app.public_pane_id(1, target_pane).unwrap();

        let response = app.handle_pane_move(
            "move".into(),
            PaneMoveParams {
                pane_id: source_public_id.clone(),
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab_id,
                    target_pane_id: Some(target_pane_id),
                    split: SplitDirection::Down,
                    ratio: None,
                },
                focus: false,
            },
        );
        assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        let moved = app.state.workspaces[0]
            .pane_state(source_pane)
            .expect("moved pane");
        assert_ne!(
            app.public_pane_id(0, source_pane).as_deref(),
            Some(source_public_id.as_str())
        );
        assert_eq!(moved.group_membership.group_id.as_ref(), Some(&group.id));

        let before_alias_change = moved.group_membership.clone();
        app.state.agent_host_name = "renamed-alias".into();
        assert_eq!(
            app.state.workspaces[0]
                .pane_state(source_pane)
                .unwrap()
                .group_membership,
            before_alias_change
        );
    }

    #[test]
    fn host_snapshot_never_serves_a_foreign_group_record() {
        let (mut app, foreign_dir, _) = app_with_groups("source-only");
        let local = created_group(&app.handle_group_create(
            "create".into(),
            GroupCreateParams {
                name: "Local".into(),
                expected_revision: 0,
            },
        ));
        let other_dir = TestDir::new("other-authority");
        let other = crate::groups::Runtime::load(&other_dir.0);
        let foreign_id = GroupId {
            owner: other.authority().unwrap().authority_id().clone(),
            local: 99,
        };
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0]
            .pane_state_mut(pane_id)
            .unwrap()
            .group_membership = PaneGroupMembership {
            group_id: Some(foreign_id.clone()),
            revision: 4,
        };
        let proxy_pane = crate::layout::PaneId::alloc();
        let proxy_terminal = crate::terminal::TerminalId::alloc();
        let (proxy_runtime, _channels) = crate::terminal::TerminalRuntime::spawn_remote_proxy(
            proxy_pane,
            24,
            80,
            0,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(crate::render_signal::RenderSignal::default()),
            crate::remote::RemoteFocusOperationState::new(),
        )
        .expect("proxy runtime");
        app.state.terminals.insert(
            proxy_terminal.clone(),
            crate::terminal::TerminalState::new(proxy_terminal.clone(), "/remote".into()),
        );
        app.terminal_runtimes
            .insert(proxy_terminal.clone(), proxy_runtime);
        let events = app.state.workspaces[0].tabs[0].events.clone();
        let mut proxy_state = crate::pane::PaneState::new(proxy_terminal);
        proxy_state.group_membership = PaneGroupMembership {
            group_id: Some(foreign_id.clone()),
            revision: 5,
        };
        app.state.workspaces[0].create_tab_from_existing_pane(
            crate::workspace::MovedPane {
                pane_id: proxy_pane,
                pane_state: proxy_state,
            },
            Some("remote proxy".into()),
            events,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(crate::render_signal::RenderSignal::default()),
        );
        let proxy_public_id = app.public_pane_id(0, proxy_pane).unwrap();

        let response: SuccessResponse =
            serde_json::from_str(&app.handle_group_host_snapshot("snapshot".into())).unwrap();
        let ResponseResult::GroupHostSnapshot { snapshot } = response.result else {
            panic!("expected host snapshot");
        };
        assert_eq!(snapshot.groups, vec![local]);
        assert!(snapshot
            .groups
            .iter()
            .all(|record| record.id.owner == snapshot.authority_id));
        assert_eq!(
            snapshot.memberships[0].membership.group_id.as_ref(),
            Some(&foreign_id)
        );
        assert_eq!(snapshot.memberships.len(), 1);
        assert_ne!(snapshot.memberships[0].pane_id, proxy_public_id);
        drop(foreign_dir);
    }

    #[test]
    fn foreign_mutations_name_the_authority_when_no_fresh_route_exists() {
        let (mut app, _dir, pane_public_id) = app_with_groups("foreign-mutation");
        let other_dir = TestDir::new("foreign-owner");
        let other = crate::groups::Runtime::load(&other_dir.0);
        let foreign_id = GroupId {
            owner: other.authority().unwrap().authority_id().clone(),
            local: 1,
        };

        let responses = [
            app.handle_group_rename(
                "rename".into(),
                GroupRenameParams {
                    group_id: foreign_id.clone(),
                    name: "Foreign".into(),
                    expected_revision: 1,
                },
            ),
            app.handle_group_delete(
                "delete".into(),
                GroupDeleteParams {
                    group_id: foreign_id.clone(),
                    expected_revision: 1,
                },
            ),
            app.handle_pane_group_set(
                "assign".into(),
                PaneGroupSetParams {
                    pane_id: pane_public_id,
                    group_id: Some(foreign_id.clone()),
                    expected_revision: 0,
                },
            ),
        ];

        for response in responses {
            let response: serde_json::Value = serde_json::from_str(&response).unwrap();
            assert_eq!(response["error"]["code"], "authority_not_fresh");
            assert!(response["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(foreign_id.owner.as_str())));
        }
    }

    #[test]
    fn receiver_only_mutation_rejects_a_second_forward() {
        let (mut app, _dir, _) = app_with_groups("receiver-only");
        let authority = app
            .group_runtime
            .authority()
            .expect("local authority")
            .authority_id()
            .clone();
        let response: serde_json::Value = serde_json::from_str(&app.handle_group_authority_mutate(
            "forward".into(),
            AuthorityMutationParams {
                expected_authority: authority,
                forwarded: false,
                mutation: AuthorityMutation::Delete(GroupDeleteParams {
                    group_id: GroupId {
                        owner: crate::groups::AuthorityId::from_random_bytes([3; 16]),
                        local: 1,
                    },
                    expected_revision: 1,
                }),
            },
        ))
        .unwrap();

        assert_eq!(response["error"]["code"], "invalid_authority_route");
    }

    #[test]
    fn clearing_a_reachable_local_pane_ignores_foreign_catalog_freshness() {
        let (mut app, _dir, pane_public_id) = app_with_groups("clear-stale-foreign");
        let foreign = GroupId {
            owner: crate::groups::AuthorityId::from_random_bytes([4; 16]),
            local: 1,
        };
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0]
            .pane_state_mut(pane_id)
            .expect("local pane")
            .group_membership = PaneGroupMembership {
            group_id: Some(foreign),
            revision: 7,
        };

        let response = app.handle_pane_group_set(
            "clear".into(),
            PaneGroupSetParams {
                pane_id: pane_public_id,
                group_id: None,
                expected_revision: 7,
            },
        );

        assert!(
            serde_json::from_str::<SuccessResponse>(&response).is_ok(),
            "clearing a reachable pane must not need the old group owner: {response}"
        );
    }

    #[test]
    fn clearing_a_remote_pane_names_its_stale_owner() {
        let (mut app, _dir, _) = app_with_groups("clear-stale-pane-owner");
        let pane_owner = crate::groups::AuthorityId::from_random_bytes([5; 16]);
        app.state.fleet_snapshot.group_catalogs = vec![crate::fleet::GroupCatalog {
            host: "remote".into(),
            target: "remote".into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::GroupCatalogState::Stale,
            snapshot: Some(GroupAuthoritySnapshot {
                authority_id: pane_owner.clone(),
                revision: 0,
                groups: Vec::new(),
                memberships: vec![OwnedPaneMembership {
                    pane_id: "remote-pane".into(),
                    membership: PaneGroupMembership::default(),
                }],
            }),
            error: Some("offline".into()),
        }];

        let response: serde_json::Value = serde_json::from_str(&app.handle_pane_group_set(
            "clear".into(),
            PaneGroupSetParams {
                pane_id: "remote-pane".into(),
                group_id: None,
                expected_revision: 0,
            },
        ))
        .unwrap();

        assert_eq!(response["error"]["code"], "authority_not_fresh");
        assert!(response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains(pane_owner.as_str())));
    }
}
