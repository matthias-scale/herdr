//! Pull-request object resolution for the dock.
//!
//! Rendering lives in `ui::work_view`; the dock is another host of that full
//! detail view.

use ratatui::{layout::Rect, style::Style, widgets::Paragraph, Frame};

use crate::app::state::{AppState, ObjectViewState, WorkItemKey};
use crate::ui::work_list_detail::PrItem;
use crate::work_context::PaneWorkContext;

pub(crate) fn primary_pr_url(context: &PaneWorkContext) -> Option<&str> {
    context
        .pr_urls
        .iter()
        .find(|url| context.is_active_owner_of(url))
        .or_else(|| context.pr_urls.first())
        .map(String::as_str)
}

fn repo_slug(pr_url: &str) -> Option<String> {
    let mut parts = pr_url.split('/');
    match (parts.next_back(), parts.next_back(), parts.next_back()) {
        (Some(_number), Some("pull"), Some(repo)) => {
            let owner = parts.next_back()?;
            Some(format!("{owner}/{repo}"))
        }
        _ => None,
    }
}

fn pr_number(pr_url: &str) -> Option<u64> {
    pr_url.rsplit('/').next()?.parse().ok()
}

pub(crate) fn focused_pr_key(app: &AppState) -> Option<WorkItemKey> {
    let (context, _) = super::chooser::focused_availability(app);
    let url = app
        .presented_dock_object(crate::app::DockSurface::Pr)
        .map(|object| object.key.as_str())
        .or_else(|| primary_pr_url(&context))?;
    let item = app.work_index_snapshot.as_ref().and_then(|snapshot| {
        snapshot
            .items
            .iter()
            .find(|item| item.pr_url.as_deref() == Some(url))
    });
    Some(WorkItemKey {
        repo: item
            .map(|item| item.repo.clone())
            .or_else(|| repo_slug(url))
            .or_else(|| context.repo.clone())
            .unwrap_or_default(),
        pr_number: item
            .and_then(|item| item.pr_number)
            .or_else(|| pr_number(url)),
        pr_url: Some(url.to_string()),
        ticket_id: None,
    })
}

fn focused_pr_item(app: &AppState) -> Option<PrItem<'_>> {
    let key = focused_pr_key(app)?;
    let snapshot = app.work_index_snapshot.as_ref()?;
    let url = key.pr_url.as_deref()?;
    let summary = snapshot
        .items
        .iter()
        .find(|item| item.pr_url.as_deref() == Some(url))?;
    Some(PrItem {
        summary,
        cached_detail: app.work_item_detail_cache.get(&key),
        observed_at: snapshot.observed_at,
    })
}

pub(crate) fn render_pr(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(item) = focused_pr_item(app) else {
        let message = match app.work_index_snapshot.as_ref() {
            Some(snapshot) => snapshot
                .short_unavailable_reason(
                    crate::work_index::WorkIndexSource::Github,
                    std::time::SystemTime::now(),
                )
                .map(|reason| format!(" GitHub: {reason}"))
                .unwrap_or_else(|| " pull request absent from latest index".into()),
            None => " pull request not indexed yet".into(),
        };
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(app.palette.overlay1)),
            Rect::new(area.x, area.y, area.width, 1),
        );
        return;
    };
    // Key the view exactly as the input layer does, so scroll, sub-tab and
    // comment expansion land in the entry this render reads.
    let key = focused_pr_key(app).unwrap_or_else(|| item.stable_key());
    let view = app
        .dock_object_views
        .get(&key)
        .cloned()
        .unwrap_or_else(ObjectViewState::default);
    crate::ui::work_view::render_pr_detail(
        app,
        &item,
        &view,
        crate::ui::work_view::PrDetailControls {
            checkout_menu: app.dock_pr_checkout_menu,
            action_menu: app.dock_pr_action_menu,
            reviewer_picker: view.reviewer_picker.as_ref(),
            pending_write: app.dock_pending_write.as_ref(),
            notice: app.dock_write_notice.as_deref(),
        },
        area,
        frame,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dock_pr_is_unavailable_without_an_object() {
        let app = AppState::test_new();
        assert!(focused_pr_key(&app).is_none());
        assert!(focused_pr_item(&app).is_none());
    }
}
