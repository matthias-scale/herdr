//! The compact pull-request surface: the 5a detail without the list and
//! without the description body.
//!
//! Pure presentation over the same `PrItem` projection the full-screen view
//! uses, so the header, checks and comments cannot drift between the two.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame,
};

use crate::app::state::{AppState, PrCheckoutChoice, WorkItemKey};
use crate::ui::dropdown::{layout_dropdown, DropdownLayout, DropdownSpec};
use crate::ui::work_list_detail::{
    comment_header, section_separator, PrActionKind, PrActionPlacement, PrItem, WorkItem as _,
};
use crate::work_context::PaneWorkContext;

/// Row of the action buttons inside the surface, counted from its top.
const ACTION_ROW: u16 = 2;

/// The pull request this pane is about: the first binding it actively owns,
/// else the first one it has.
pub(crate) fn primary_pr_url(context: &PaneWorkContext) -> Option<&str> {
    context
        .pr_urls
        .iter()
        .find(|url| context.is_active_owner_of(url))
        .or_else(|| context.pr_urls.first())
        .map(String::as_str)
}

/// `owner/repo` out of a pull-request URL, when it looks like one.
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

/// Detail-cache key of the focused pane's primary pull request, built exactly
/// as the home projection builds it so both read the same cache entry.
pub(crate) fn focused_pr_key(app: &AppState) -> Option<WorkItemKey> {
    let (context, _) = super::chooser::focused_availability(app);
    let url = app
        .active_dock_object(crate::app::DockSurface::Pr)
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

/// The focused pane's primary pull request as the shared `WorkItem`
/// projection, or `None` while the work index has not seen it yet.
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
    render_pr_item(app, frame, area, &item);
}

/// Body of the surface for one pull request. Split out so a fixture item can
/// be rendered without a pane, a work index, or a terminal behind it.
pub(crate) fn render_pr_item(app: &AppState, frame: &mut Frame, area: Rect, item: &PrItem<'_>) {
    let palette = &app.palette;
    let detail = item.detail();
    let checkout_available = detail.open_url.is_some()
        && item
            .cached_detail
            .and_then(|detail| detail.head_ref_name.as_ref())
            .is_some();
    let actions = item.action_table(app.pr_merge_method, checkout_available);
    let passing = detail
        .checks
        .iter()
        .filter(|(_, state)| state == "SUCCESS")
        .count();
    let number = item
        .summary
        .pr_number
        .map(|number| format!("#{number}"))
        .unwrap_or_else(|| "#—".into());
    let mut lines = vec![
        Line::from(Span::styled(
            format!(
                " {number} {}  ✓ {passing}/{}",
                detail.title,
                detail.checks.len()
            ),
            Style::default()
                .fg(palette.text)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!(" {}", detail.branches),
            Style::default().fg(palette.subtext0),
        )),
        action_row(app, &actions),
    ];
    lines.extend(section_separator(
        palette,
        format!("Checks  {}", detail.checks.len()),
        area.width,
    ));
    for (name, state) in &detail.checks {
        let glyph = match state.as_str() {
            "SUCCESS" => "✓",
            "FAILURE" => "✗",
            _ => "◌",
        };
        lines.push(Line::from(Span::styled(
            format!("  {glyph} {name}  {state}"),
            Style::default().fg(palette.subtext0),
        )));
    }
    lines.extend(section_separator(
        palette,
        format!("Comments  {}  newest first", detail.comments.len()),
        area.width,
    ));
    for (index, comment) in detail.comments.iter().enumerate() {
        if index > 0 {
            lines.push(Line::default());
        }
        lines.push(Line::from(Span::styled(
            format!("  ✦ {}", comment_header(comment, item.observed_at)),
            Style::default()
                .fg(palette.subtext0)
                .add_modifier(Modifier::DIM),
        )));
        lines.extend(crate::ui::markdown::body_lines(
            palette,
            Some(&comment.body),
            usize::from(area.width.saturating_sub(4)),
            "    ",
        ));
    }
    if let Some(write) = app.dock_pending_write.as_ref() {
        lines.push(Line::from(Span::styled(
            format!(" Confirm {}? [y/N]", write.describe()),
            Style::default()
                .fg(palette.yellow)
                .add_modifier(Modifier::BOLD),
        )));
    } else if let Some(notice) = app.dock_write_notice.as_ref() {
        lines.push(Line::from(Span::styled(
            format!(" {notice}"),
            Style::default().fg(palette.subtext0),
        )));
    }
    frame.render_widget(Paragraph::new(lines).scroll((app.dock_scroll, 0)), area);

    if let Some(choice) = app.dock_pr_checkout_menu {
        render_checkout_menu(app, frame, area, choice);
    } else if let Some(menu) = app.dock_pr_action_menu {
        let anchor = action_menu_anchor(area);
        if !crate::ui::pr_actions::render(app, frame, frame.area(), anchor, &actions, menu) {
            frame.render_widget(
                Paragraph::new("action menu needs space below")
                    .style(Style::default().fg(app.palette.red)),
                Rect::new(area.x, area.y, area.width, 1),
            );
        }
    }
}

fn action_menu_anchor(area: Rect) -> Rect {
    Rect::new(
        area.right().saturating_sub(3),
        area.y.saturating_add(ACTION_ROW),
        3.min(area.width),
        1,
    )
}

fn action_row(app: &AppState, actions: &[crate::ui::work_list_detail::PrAction]) -> Line<'static> {
    let checkout = actions.iter().find(|action| {
        action.kind == PrActionKind::CheckOut && action.placement == PrActionPlacement::Header
    });
    let merge = actions.iter().find(|action| {
        matches!(action.kind, PrActionKind::Merge(_))
            && action.placement == PrActionPlacement::Header
    });
    let checkout_style = if checkout.is_some_and(|action| action.enabled()) {
        Style::default().fg(app.palette.accent)
    } else {
        Style::default()
            .fg(app.palette.overlay0)
            .add_modifier(Modifier::DIM)
    };
    let merge_style = if merge.is_some_and(|action| action.enabled()) {
        Style::default()
            .fg(app.palette.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(app.palette.overlay0)
            .add_modifier(Modifier::DIM)
    };
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(
            format!(
                "[{}]",
                checkout.map_or("Check out ▾", |action| action.label.as_str())
            ),
            checkout_style,
        ),
        Span::raw(" · "),
        Span::styled(
            format!(
                "[{}]",
                merge.map_or("Merge", |action| action.label.as_str())
            ),
            merge_style,
        ),
        Span::raw(" · "),
        Span::styled("[⋯]", Style::default().fg(app.palette.accent)),
    ];
    if let Some(reason) = merge.and_then(|action| action.disabled_reason) {
        spans.push(Span::styled(
            format!(" · {reason}"),
            Style::default()
                .fg(app.palette.overlay0)
                .add_modifier(Modifier::DIM),
        ));
    }
    Line::from(spans)
}

/// Downward dropdown of the checkout choices, anchored on the action row.
pub(crate) fn checkout_menu_layout(area: Rect) -> Option<DropdownLayout> {
    let anchor = Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(ACTION_ROW),
        14.min(area.width),
        1,
    );
    layout_dropdown(
        &DropdownSpec {
            anchor,
            item_count: 2,
            selected: 0,
            has_filter: false,
            max_rows: 2,
            min_width: 18,
        },
        area,
    )
}

fn render_checkout_menu(app: &AppState, frame: &mut Frame, area: Rect, choice: PrCheckoutChoice) {
    let Some(layout) = checkout_menu_layout(area) else {
        frame.render_widget(
            Paragraph::new("checkout menu needs space below")
                .style(Style::default().fg(app.palette.red)),
            Rect::new(area.x, area.y, area.width, 1),
        );
        return;
    };
    let menu = [
        (PrCheckoutChoice::CurrentCheckout, "Current checkout"),
        (PrCheckoutChoice::NewWorktree, "New worktree"),
    ]
    .into_iter()
    .map(|(option, label)| {
        Line::from(Span::styled(
            format!("{} {label}", if option == choice { "▸" } else { " " }),
            Style::default()
                .fg(app.palette.text)
                .bg(app.palette.panel_bg),
        ))
    })
    .collect::<Vec<_>>();
    frame.render_widget(Clear, layout.rect);
    frame.render_widget(Paragraph::new(menu), layout.rect);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_index::{
        PrAudience, PrCheckState, WorkIndexSource, WorkIndexUnavailable,
        WorkItem as IndexedWorkItem, WorkItemAction, WorkItemComment,
        WorkItemDetail as IndexedWorkItemDetail, WorkItemSource,
    };
    use ratatui::{backend::TestBackend, Terminal};
    use std::time::{Duration, SystemTime};

    fn summary() -> IndexedWorkItem {
        IndexedWorkItem {
            repo: "owner/repo".into(),
            pr_number: Some(42),
            pr_url: Some("https://github.com/owner/repo/pull/42".into()),
            pr_title: Some("repair parser".into()),
            pr_state: Some("open".into()),
            draft: false,
            review_decision: None,
            created_at: Some(SystemTime::UNIX_EPOCH),
            updated_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(10)),
            additions: 13,
            deletions: 1,
            author: Some("ada".into()),
            assignees: vec!["ada".into()],
            labels: Vec::new(),
            check_state: PrCheckState::Passing,
            audience: PrAudience::Authored,
            cached_pr_detail: None,
            ticket_ids: Vec::new(),
            ticket_title: None,
            ticket_state: None,
            ticket_details: Vec::new(),
            branch: Some("fix/parser".into()),
            preview_urls: Vec::new(),
            panes: Vec::new(),
            source: WorkItemSource::default(),
        }
    }

    fn detail(states: &[&str], merge: &str, with_comment: bool) -> IndexedWorkItemDetail {
        let mut detail = IndexedWorkItemDetail::empty();
        detail.number = Some(42);
        detail.title = Some("repair parser".into());
        detail.author = Some("ada".into());
        detail.base_ref_name = Some("main".into());
        detail.head_ref_name = Some("fix/parser".into());
        detail.merge_state_status = Some(merge.into());
        detail.mergeable = Some(
            if merge == "CLEAN" {
                "MERGEABLE"
            } else {
                "CONFLICTING"
            }
            .into(),
        );
        detail.head_sha = Some("abc123".into());
        detail.review_decision = Some("APPROVED".into());
        detail.body = Some("## Why\n- regression".into());
        detail.actions = states
            .iter()
            .enumerate()
            .map(|(index, state)| WorkItemAction {
                name: format!("check-{index}"),
                state: (*state).into(),
            })
            .collect();
        if with_comment {
            detail.comments.push(WorkItemComment {
                author: Some("grace".into()),
                body: "fix this".into(),
                created_at: Some(SystemTime::UNIX_EPOCH),
            });
        }
        detail
    }

    fn body_text(app: &AppState, item: &PrItem<'_>, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render_pr_item(app, frame, area, item))
            .expect("render compact pr surface");
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|row| {
                (0..width)
                    .map(|col| buffer[(col, row)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn compact_pr_surface_renders_header_checks_and_comments() {
        let app = AppState::test_new();
        let summary = summary();
        let cached = detail(&["SUCCESS", "FAILURE"], "CLEAN", true);
        let item = PrItem {
            summary: &summary,
            cached_detail: Some(&cached),
            observed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(60),
        };
        let rendered = body_text(&app, &item, 44, 12);
        let lines = rendered.lines().collect::<Vec<_>>();
        assert_eq!(lines[0], " #42 repair parser  ✓ 1/2");
        assert!(lines[2].starts_with(" [Check out ▾] · [Merge] · [⋯]"));
        assert!(lines[2].contains("checks fail"));
        assert!(rendered.contains("Checks  2"), "{rendered}");
        assert!(rendered.contains("Comments  1  newest first"), "{rendered}");
        assert!(rendered.contains("✦ grace · 1m"), "{rendered}");
        assert!(rendered.contains("fix this"), "{rendered}");
    }

    #[test]
    fn compact_pr_surface_omits_the_description_body() {
        let app = AppState::test_new();
        let summary = summary();
        let cached = detail(&["SUCCESS"], "CLEAN", false);
        let item = PrItem {
            summary: &summary,
            cached_detail: Some(&cached),
            observed_at: SystemTime::UNIX_EPOCH,
        };
        let rendered = body_text(&app, &item, 44, 8);
        assert!(!rendered.contains("regression"), "{rendered}");
        assert!(
            rendered.contains(" [Check out ▾] · [Merge] · [⋯]"),
            "{rendered}"
        );
    }

    #[test]
    fn compact_pr_uses_the_shared_action_table() {
        let summary = summary();
        let uncached = PrItem {
            summary: &summary,
            cached_detail: None,
            observed_at: SystemTime::UNIX_EPOCH,
        };
        let actions = uncached.action_table(crate::config::MergeMethodConfig::Merge, true);
        assert!(actions
            .iter()
            .any(|action| { action.kind == PrActionKind::CheckOut && action.enabled() }));
        assert!(actions.iter().any(|action| {
            action.kind == PrActionKind::Merge(crate::config::MergeMethodConfig::Merge)
                && !action.enabled()
        }));
        for (states, merge, comment, land) in [
            (vec!["SUCCESS"], "CLEAN", true, true),
            (vec!["FAILURE"], "CLEAN", true, false),
            (vec!["SUCCESS"], "BEHIND", false, false),
            (Vec::new(), "CLEAN", false, true),
        ] {
            let cached = detail(&states, merge, comment);
            let item = PrItem {
                summary: &summary,
                cached_detail: Some(&cached),
                observed_at: SystemTime::UNIX_EPOCH,
            };
            let actions = item.action_table(crate::config::MergeMethodConfig::Merge, true);
            assert!(actions
                .iter()
                .find(|action| action.kind == PrActionKind::CheckOut)
                .is_some_and(|action| action.enabled()));
            assert_eq!(
                actions
                    .iter()
                    .find(|action| {
                        action.kind == PrActionKind::Merge(crate::config::MergeMethodConfig::Merge)
                    })
                    .map(|action| action.enabled()),
                Some(land)
            );
        }
    }

    #[test]
    fn compact_pr_action_menu_opens_downward_from_the_action_row() {
        let summary = summary();
        let detail = detail(&["SUCCESS"], "CLEAN", false);
        let actions = PrItem {
            summary: &summary,
            cached_detail: Some(&detail),
            observed_at: SystemTime::UNIX_EPOCH,
        }
        .action_table(crate::config::MergeMethodConfig::Merge, true);
        let area = Rect::new(60, 4, 40, 20);
        let anchor = action_menu_anchor(area);
        let layout = crate::ui::pr_actions::layout(
            Rect::new(0, 0, 120, 40),
            anchor,
            &actions,
            Default::default(),
        )
        .expect("menu fits below the compact action row");
        assert_eq!(layout.rect.y, anchor.bottom());
    }

    #[test]
    fn primary_pr_prefers_the_actively_owned_binding() {
        let mut context = PaneWorkContext {
            pr_urls: vec![
                "https://github.com/owner/repo/pull/1".into(),
                "https://github.com/owner/repo/pull/2".into(),
            ],
            ..PaneWorkContext::default()
        };
        assert_eq!(
            primary_pr_url(&context),
            Some("https://github.com/owner/repo/pull/1")
        );
        context.active_owner = true;
        assert_eq!(
            primary_pr_url(&context),
            Some("https://github.com/owner/repo/pull/1")
        );
        context.pr_urls.reverse();
        assert_eq!(
            primary_pr_url(&context),
            Some("https://github.com/owner/repo/pull/2")
        );
        assert_eq!(primary_pr_url(&PaneWorkContext::default()), None);
    }

    #[test]
    fn compact_pr_surface_is_unavailable_without_a_pull_request() {
        let mut app = AppState::test_new();
        assert!(!super::super::chooser::surface_available(
            crate::app::DockSurface::Pr,
            &PaneWorkContext::default(),
            true,
            false,
        ));
        assert!(focused_pr_key(&app).is_none());
        assert!(focused_pr_item(&app).is_none());
        app.dock_collapsed = false;
        assert!(!app.activate_dock_surface(crate::app::DockSurface::Pr));
    }

    #[test]
    fn unresolved_pane_pull_request_shows_github_failure_reason() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("pr")];
        app.active = Some(0);
        app.ensure_test_terminals();
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .expect("focused terminal")
            .replace_prevalidated_manual_work_context(PaneWorkContext {
                pr_urls: vec!["https://github.com/owner/repo/pull/77".into()],
                ..Default::default()
            });
        app.work_index_snapshot = Some(crate::work_index::Snapshot {
            items: Vec::new(),
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: Some(WorkIndexUnavailable::only(
                WorkIndexSource::Github,
                "rate limited",
            )),
            observed_at: SystemTime::UNIX_EPOCH,
        });
        let mut terminal = Terminal::new(TestBackend::new(48, 4)).expect("test terminal");
        terminal
            .draw(|frame| render_pr(&app, frame, frame.area()))
            .expect("render PR failure");
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("GitHub: rate limited"), "{text:?}");
        assert!(!text.contains("not indexed yet"), "{text:?}");

        app.work_index_snapshot
            .as_mut()
            .expect("snapshot")
            .unavailable = None;
        terminal
            .draw(|frame| render_pr(&app, frame, frame.area()))
            .expect("render successful empty PR observation");
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            text.contains("pull request absent from latest index"),
            "{text:?}"
        );
        assert!(!text.contains("not indexed yet"), "{text:?}");
    }

    #[test]
    fn checkout_menu_opens_below_the_action_row() {
        let area = Rect::new(0, 0, 40, 12);
        let layout = checkout_menu_layout(area).expect("menu fits");
        assert_eq!(layout.rect.y, area.y + ACTION_ROW + 1);
        assert!(checkout_menu_layout(Rect::new(0, 0, 40, ACTION_ROW + 1)).is_none());
    }
}
