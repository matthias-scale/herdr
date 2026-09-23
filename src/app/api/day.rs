use crate::api::schema::{
    DayAddParams, DayBindParams, DayItemTarget, DayLinkParams, DayListParams, DayNoteParams,
    ResponseResult,
};

use super::App;

impl App {
    pub(super) fn handle_day_add(&mut self, id: String, params: DayAddParams) -> String {
        let title = params.title.trim().to_string();
        if title.is_empty() || title.contains(['\n', '\r']) {
            return super::responses::encode_error(
                id,
                "invalid_params",
                "day item title must be one non-empty line",
            );
        }
        let added_at = crate::day::unix_seconds_now();
        let item_id = match crate::day::new_id(added_at.saturating_mul(1_000)) {
            Ok(item_id) => item_id,
            Err(error) => return super::responses::encode_error(id, "internal_error", error),
        };
        let item = crate::day::DayItem {
            id: item_id,
            title,
            kind: params.kind,
            source: params.source,
            note: params.note.filter(|note| !note.is_empty()),
            links: crate::day::DayLinks::default(),
            bindings: std::collections::BTreeMap::new(),
            added_at,
            done_at: None,
            dismissed: false,
            stale_after_seconds: None,
        };
        if let Err(error) = self.persist_day_item(&item) {
            return super::responses::encode_error(id, "store_error", error);
        }
        self.state
            .day_board
            .items
            .insert(item.id.clone(), item.clone());
        self.encode_day_item(id, &item)
    }

    pub(super) fn handle_day_list(&self, id: String, params: DayListParams) -> String {
        let items = self
            .state
            .day_board
            .items
            .values()
            .filter(|item| params.include_dismissed || !item.dismissed)
            .map(|item| self.derived_day_item(item))
            .collect();
        let load_errors = self
            .state
            .day_board
            .load_errors
            .iter()
            .map(|error| crate::api::schema::DayLoadError {
                path: error.path.display().to_string(),
                message: error.message.clone(),
            })
            .collect();
        super::responses::encode_success(id, ResponseResult::DayList { items, load_errors })
    }

    pub(super) fn handle_day_bind(&mut self, id: String, params: DayBindParams) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return super::responses::encode_error(
                id,
                "pane_not_found",
                format!("pane not found: {}", params.pane_id),
            );
        };
        let Some(canonical_pane_id) = self.public_pane_id(ws_idx, pane_id) else {
            return super::responses::encode_error(
                id,
                "pane_not_found",
                format!("pane not found: {}", params.pane_id),
            );
        };
        let Some(mut item) = self.state.day_board.items.get(&params.id).cloned() else {
            return day_item_not_found(id, &params.id);
        };
        item.bindings.insert(
            self.state.agent_host_name.clone(),
            crate::day::DayBinding {
                pane_id: canonical_pane_id,
                bound_at: crate::day::unix_seconds_now(),
            },
        );
        self.persist_and_publish(id, item)
    }

    pub(super) fn handle_day_link(&mut self, id: String, params: DayLinkParams) -> String {
        if params.ticket.as_deref().is_none_or(str::is_empty)
            && params.pr.as_deref().is_none_or(str::is_empty)
        {
            return super::responses::encode_error(
                id,
                "invalid_params",
                "day.link requires --ticket or --pr",
            );
        }
        let Some(mut item) = self.state.day_board.items.get(&params.id).cloned() else {
            return day_item_not_found(id, &params.id);
        };
        if let Some(ticket) = params.ticket.filter(|ticket| !ticket.is_empty()) {
            if !item.links.tickets.contains(&ticket) {
                item.links.tickets.push(ticket);
            }
        }
        if let Some(pr) = params.pr.filter(|pr| !pr.is_empty()) {
            if !item.links.prs.contains(&pr) {
                item.links.prs.push(pr);
            }
        }
        self.persist_and_publish(id, item)
    }

    pub(super) fn handle_day_note(&mut self, id: String, params: DayNoteParams) -> String {
        let Some(mut item) = self.state.day_board.items.get(&params.id).cloned() else {
            return day_item_not_found(id, &params.id);
        };
        item.note = params.note.filter(|note| !note.is_empty());
        self.persist_and_publish(id, item)
    }

    pub(super) fn handle_day_done(&mut self, id: String, params: DayItemTarget) -> String {
        let Some(mut item) = self.state.day_board.items.get(&params.id).cloned() else {
            return day_item_not_found(id, &params.id);
        };
        item.done_at = Some(crate::day::unix_seconds_now());
        self.persist_and_publish(id, item)
    }

    pub(super) fn handle_day_dismiss(&mut self, id: String, params: DayItemTarget) -> String {
        let Some(mut item) = self.state.day_board.items.get(&params.id).cloned() else {
            return day_item_not_found(id, &params.id);
        };
        item.dismissed = true;
        self.persist_and_publish(id, item)
    }

    fn persist_and_publish(&mut self, id: String, item: crate::day::DayItem) -> String {
        if let Err(error) = self.persist_day_item(&item) {
            return super::responses::encode_error(id, "store_error", error);
        }
        self.state
            .day_board
            .items
            .insert(item.id.clone(), item.clone());
        self.encode_day_item(id, &item)
    }

    fn persist_day_item(&self, item: &crate::day::DayItem) -> Result<(), String> {
        let root = self
            .day_store_root
            .as_deref()
            .ok_or_else(|| "day item store is unavailable".to_string())?;
        crate::day::write_item(root, item)
    }

    fn encode_day_item(&self, id: String, item: &crate::day::DayItem) -> String {
        super::responses::encode_success(
            id,
            ResponseResult::DayItem {
                item: self.derived_day_item(item),
            },
        )
    }

    fn derived_day_item(&self, item: &crate::day::DayItem) -> crate::day::DerivedDayItem {
        self.state.derive_day_item(
            item,
            self.state.day_stale_after,
            self.day_item_links_closed(item),
            std::time::Instant::now(),
        )
    }

    fn day_item_links_closed(&self, item: &crate::day::DayItem) -> bool {
        if item.links.is_empty() {
            return false;
        }
        let Some(snapshot) = self.work_index_snapshot.as_ref() else {
            return false;
        };
        let tickets_closed = item.links.tickets.iter().all(|ticket| {
            snapshot
                .items
                .iter()
                .any(|work| ticket_state_for_link(work, ticket).is_some_and(ticket_state_is_done))
        });
        let prs_closed = item.links.prs.iter().all(|pr| {
            snapshot.items.iter().any(|work| {
                work.pr_url.as_deref() == Some(pr)
                    && work
                        .pr_state
                        .as_deref()
                        .is_some_and(|state| state.eq_ignore_ascii_case("merged"))
            })
        });
        tickets_closed && prs_closed
    }
}

fn ticket_state_for_link<'a>(
    work: &'a crate::work_index::WorkItem,
    ticket: &str,
) -> Option<&'a str> {
    if let Some(detail) = work
        .ticket_details
        .iter()
        .find(|detail| detail.identifier.eq_ignore_ascii_case(ticket))
    {
        return detail.state.as_deref();
    }
    if work.ticket_details.is_empty()
        && work.ticket_ids.len() == 1
        && work.ticket_ids[0].eq_ignore_ascii_case(ticket)
    {
        return work.ticket_state.as_deref();
    }
    None
}

fn ticket_state_is_done(state: &str) -> bool {
    matches!(
        state.trim().to_ascii_lowercase().as_str(),
        "done" | "completed"
    )
}

fn day_item_not_found(id: String, item_id: &str) -> String {
    super::responses::encode_error(
        id,
        "day_item_not_found",
        format!("day item not found: {item_id}"),
    )
}

#[cfg(test)]
mod tests {
    use crate::api::schema::{
        DayAddParams, DayItemKind, DayItemSource, DayListParams, Method, Request, ResponseResult,
        SuccessResponse,
    };

    fn app() -> crate::app::App {
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            rx,
            crate::api::EventHub::default(),
        )
    }

    fn response(raw: &str) -> SuccessResponse {
        serde_json::from_str(raw).expect("success response")
    }

    #[test]
    fn only_done_ticket_states_complete_linked_work() {
        assert!(super::ticket_state_is_done("Done"));
        assert!(super::ticket_state_is_done("completed"));
        assert!(!super::ticket_state_is_done("canceled"));
        assert!(!super::ticket_state_is_done("cancelled"));
        assert!(!super::ticket_state_is_done("in progress"));
    }

    #[test]
    fn linked_ticket_never_inherits_another_tickets_state() {
        let mut work = work_item_with_tickets(vec!["SCA-7".into(), "SCA-8".into()]);
        work.ticket_state = Some("Done".into());
        work.ticket_details = vec![work_ticket("SCA-8", Some("Done"))];

        assert_eq!(super::ticket_state_for_link(&work, "SCA-7"), None);
        assert_eq!(super::ticket_state_for_link(&work, "SCA-8"), Some("Done"));

        work.ticket_details.clear();
        assert_eq!(super::ticket_state_for_link(&work, "SCA-7"), None);

        work.ticket_ids = vec!["SCA-7".into()];
        assert_eq!(super::ticket_state_for_link(&work, "SCA-7"), Some("Done"));
    }

    #[test]
    fn list_observes_staleness_at_request_time() {
        let root = std::env::temp_dir().join(format!(
            "herdr-day-api-stale-{}",
            crate::config::test_unique_suffix()
        ));
        let mut app = app();
        app.day_store_root = Some(root.clone());
        let mut workspace = crate::workspace::Workspace::test_new("day-api-stale");
        let pane_id = workspace.tabs[0].root_pane;
        let quiet_since = std::time::Instant::now() - std::time::Duration::from_secs(601);
        workspace.tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane")
            .activity
            .set_last_at(quiet_since);
        app.state.view_observed_at = quiet_since;
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let public_pane_id = app.public_pane_id(0, pane_id).expect("public pane id");
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .expect("terminal id")
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal")
            .set_raw_agent_state_for_test(crate::detect::AgentState::Working);

        let added = app.handle_api_request(Request {
            id: "add-stale".into(),
            method: Method::DayAdd(DayAddParams {
                title: "Quiet agent".into(),
                kind: DayItemKind::Task,
                source: DayItemSource::Manual,
                note: None,
            }),
        });
        let ResponseResult::DayItem { item } = response(&added).result else {
            panic!("unexpected response: {added}");
        };
        let bound = app.handle_api_request(Request {
            id: "bind-stale".into(),
            method: Method::DayBind(crate::api::schema::DayBindParams {
                id: item.item.id,
                pane_id: public_pane_id,
            }),
        });
        let ResponseResult::DayItem { item } = response(&bound).result else {
            panic!("unexpected response: {bound}");
        };

        assert_eq!(item.column, crate::day::DayColumn::Working);
        assert!(item.stale);
        let _ = std::fs::remove_dir_all(root);
    }

    fn work_item_with_tickets(ticket_ids: Vec<String>) -> crate::work_index::WorkItem {
        crate::work_index::WorkItem {
            repo: String::new(),
            pr_number: None,
            pr_url: None,
            pr_title: None,
            pr_state: None,
            draft: false,
            review_decision: None,
            created_at: None,
            updated_at: None,
            additions: 0,
            deletions: 0,
            author: None,
            assignees: Vec::new(),
            labels: Vec::new(),
            check_state: crate::work_index::PrCheckState::Unknown,
            audience: crate::work_index::PrAudience::Unclassified,
            cached_pr_detail: None,
            ticket_ids,
            ticket_title: None,
            ticket_state: None,
            ticket_details: Vec::new(),
            branch: None,
            preview_urls: Vec::new(),
            panes: Vec::new(),
            source: crate::work_index::WorkItemSource::default(),
        }
    }

    fn work_ticket(identifier: &str, state: Option<&str>) -> crate::work_index::WorkTicket {
        crate::work_index::WorkTicket {
            identifier: identifier.into(),
            title: None,
            description: None,
            state: state.map(str::to_string),
            assignee: None,
            creator: None,
            priority: None,
            cycle: None,
            group: crate::work_index::TicketGroup::default(),
            created_at: None,
            updated_at: None,
            branch: None,
            labels: Vec::new(),
            url: None,
            parent: None,
            relations: Vec::new(),
        }
    }

    #[test]
    fn add_persists_one_item_and_list_returns_derived_json_fields() {
        let root = std::env::temp_dir().join(format!(
            "herdr-day-api-{}",
            crate::config::test_unique_suffix()
        ));
        let mut app = app();
        app.day_store_root = Some(root.clone());

        let added = app.handle_api_request(Request {
            id: "add".into(),
            method: Method::DayAdd(DayAddParams {
                title: "Reply to refund thread".into(),
                kind: DayItemKind::Task,
                source: DayItemSource::Manual,
                note: None,
            }),
        });
        let ResponseResult::DayItem { item } = response(&added).result else {
            panic!("unexpected response: {added}");
        };
        assert_eq!(item.column, crate::day::DayColumn::Todo);
        assert!(!item.stale);
        assert!(root.join(format!("items/{}.md", item.item.id)).is_file());

        let listed = app.handle_api_request(Request {
            id: "list".into(),
            method: Method::DayList(DayListParams::default()),
        });
        let ResponseResult::DayList { items, load_errors } = response(&listed).result else {
            panic!("unexpected response: {listed}");
        };
        assert_eq!(items, vec![item]);
        assert!(load_errors.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn bind_link_note_done_and_dismiss_mutate_only_the_item_file() {
        let root = std::env::temp_dir().join(format!(
            "herdr-day-api-mutations-{}",
            crate::config::test_unique_suffix()
        ));
        let mut app = app();
        app.day_store_root = Some(root.clone());
        let workspace = crate::workspace::Workspace::test_new("day-api");
        let pane_id = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let public_pane_id = app.public_pane_id(0, pane_id).expect("public pane id");
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .expect("terminal id")
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal")
            .set_raw_agent_state_for_test(crate::detect::AgentState::Working);

        let added = app.handle_api_request(Request {
            id: "add".into(),
            method: Method::DayAdd(DayAddParams {
                title: "Ship the slice".into(),
                kind: DayItemKind::Task,
                source: DayItemSource::Manual,
                note: None,
            }),
        });
        let ResponseResult::DayItem { item } = response(&added).result else {
            panic!("unexpected response: {added}");
        };
        let item_id = item.item.id;

        for (request_id, method) in [
            (
                "bind",
                Method::DayBind(crate::api::schema::DayBindParams {
                    id: item_id.clone(),
                    pane_id: public_pane_id.clone(),
                }),
            ),
            (
                "link",
                Method::DayLink(crate::api::schema::DayLinkParams {
                    id: item_id.clone(),
                    ticket: Some("SCA-7".into()),
                    pr: Some("https://github.com/acme/app/pull/9".into()),
                }),
            ),
            (
                "note",
                Method::DayNote(crate::api::schema::DayNoteParams {
                    id: item_id.clone(),
                    note: Some("after CI".into()),
                }),
            ),
            (
                "done",
                Method::DayDone(crate::api::schema::DayItemTarget {
                    id: item_id.clone(),
                }),
            ),
            (
                "dismiss",
                Method::DayDismiss(crate::api::schema::DayItemTarget {
                    id: item_id.clone(),
                }),
            ),
        ] {
            let raw = app.handle_api_request(Request {
                id: request_id.into(),
                method,
            });
            assert!(matches!(
                response(&raw).result,
                ResponseResult::DayItem { .. }
            ));
        }

        let files = std::fs::read_dir(root.join("items"))
            .expect("items directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("item files");
        assert_eq!(files.len(), 1);
        let stored = crate::day::load(&root)
            .items
            .remove(&item_id)
            .expect("item");
        assert_eq!(stored.note.as_deref(), Some("after CI"));
        assert_eq!(stored.links.tickets, vec!["SCA-7"]);
        assert_eq!(
            stored.bindings[&app.state.agent_host_name].pane_id,
            public_pane_id
        );
        assert!(stored.done_at.is_some());
        assert!(stored.dismissed);

        let listed = app.handle_api_request(Request {
            id: "list".into(),
            method: Method::DayList(DayListParams::default()),
        });
        let ResponseResult::DayList { items, .. } = response(&listed).result else {
            panic!("unexpected response: {listed}");
        };
        assert!(items.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }
}
