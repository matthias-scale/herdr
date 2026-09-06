//! The dock surface chooser: the empty-dock card grid and the `+` dropdown.
//!
//! Pure presentation. Availability is derived from the focused pane's work
//! context, never from the server, and every layout function is a function of
//! its rect so the click targets and the drawn cells cannot disagree.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame,
};

use std::path::Path;

use crate::app::state::{AppState, DockSurface};
use crate::ui::dropdown::{layout_dropdown, DropdownLayout, DropdownSpec};
use crate::work_context::PaneWorkContext;
use crate::workspace::Workspace;

/// Width of the `+` menu. Wide enough for the longest title plus its hint,
/// narrow enough for the minimum dock.
const MENU_WIDTH: u16 = 20;

/// Can this surface do anything for the focused pane right now?
///
/// Diff needs a repository to diff, PR needs a pull request, Linear needs a
/// ticket. Everything else is always available.
pub(crate) fn surface_available(
    surface: DockSurface,
    ctx: &PaneWorkContext,
    in_git_repo: bool,
) -> bool {
    match surface {
        DockSurface::Diff => in_git_repo,
        DockSurface::Pr => !ctx.pr_urls.is_empty(),
        DockSurface::Linear => !ctx.ticket_ids.is_empty(),
        _ => true,
    }
}

/// Work context and repository state of the focused pane, or the empty context
/// when no pane is focused.
pub(crate) fn focused_availability(app: &AppState) -> (PaneWorkContext, bool) {
    let Some(workspace) = app.active.and_then(|index| app.workspaces.get(index)) else {
        return (PaneWorkContext::default(), false);
    };
    let context = workspace
        .focused_pane_id()
        .and_then(|pane_id| workspace.terminal_id(pane_id))
        .and_then(|terminal_id| app.terminals.get(terminal_id))
        .map(|terminal| terminal.effective_work_context().clone())
        .unwrap_or_default();
    let in_git_repo = focused_in_git_repo(app);
    (context, in_git_repo)
}

/// Whether the focused pane's cwd is inside a repository, using the shared
/// work-context Git observation cache.
pub(crate) fn focused_in_git_repo(app: &AppState) -> bool {
    let Some(workspace) = app.active.and_then(|index| app.workspaces.get(index)) else {
        return false;
    };
    app.status_focused_cwd
        .clone()
        .or_else(|| workspace.focused_cached_cwd(&app.terminals))
        .is_some_and(|cwd| cwd_in_git_repo(app, workspace, &cwd))
}

/// Is `cwd` inside a repository? Answered from the cache the Git work context
/// refresh fills, never by touching the filesystem during render.
///
/// A pane the refresh has not observed yet falls back to the workspace's own
/// repository, and only when the pane sits inside it: a workspace rooted in a
/// repository says nothing about a pane that walked out of it.
fn cwd_in_git_repo(app: &AppState, workspace: &Workspace, cwd: &Path) -> bool {
    if let Some(root) = app.git_root_for_cwd.get(cwd) {
        return root.is_some();
    }
    workspace
        .git_space()
        .is_some_and(|space| cwd.starts_with(&space.repo_root))
}

/// Rows of the card grid, top to bottom, one rect per `DockSurface::CARDS`
/// entry. Cards that do not fit are omitted rather than clipped, so a click can
/// never land on a card the user cannot see.
pub(crate) fn card_hit_areas(area: Rect) -> Vec<Rect> {
    if area.width < 12 || area.height < HEADER_ROWS + 3 {
        return Vec::new();
    }
    let columns: u16 = if area.width >= 24 { 2 } else { 1 };
    let card_rows = DockSurface::CARDS.len().div_ceil(usize::from(columns));
    let available = area.height.saturating_sub(HEADER_ROWS);
    let card_height = if usize::from(available) >= card_rows * 4 {
        4
    } else {
        3
    };
    let card_width = (area.width.saturating_sub(columns + 1)) / columns;
    if card_width < 8 {
        return Vec::new();
    }

    let mut areas = Vec::new();
    for index in 0..DockSurface::CARDS.len() {
        let index = u16::try_from(index).unwrap_or(u16::MAX);
        let row = index / columns;
        let column = index % columns;
        let y = area
            .y
            .saturating_add(HEADER_ROWS)
            .saturating_add(row.saturating_mul(card_height));
        if y.saturating_add(card_height) > area.bottom() {
            break;
        }
        let x = area
            .x
            .saturating_add(1)
            .saturating_add(column.saturating_mul(card_width.saturating_add(1)));
        areas.push(Rect::new(x, y, card_width, card_height));
    }
    areas
}

/// Title, subtitle and one blank row above the grid.
const HEADER_ROWS: u16 = 3;

pub(crate) fn render_chooser(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (context, in_git_repo) = focused_availability(app);

    let title = Style::default()
        .fg(app.palette.text)
        .add_modifier(Modifier::BOLD);
    let subtitle = Style::default().fg(app.palette.overlay1);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled("Open a surface", title)).centered(),
            Line::from(Span::styled("choose what to show here", subtitle)).centered(),
        ]),
        Rect::new(area.x, area.y, area.width, area.height.min(2)),
    );

    for (surface, card) in DockSurface::CARDS
        .into_iter()
        .zip(app.view.dock_surface_card_hit_areas.iter().copied())
    {
        let enabled = surface_available(surface, &context, in_git_repo);
        render_card(app, frame, card, surface, enabled);
    }
}

fn render_card(app: &AppState, frame: &mut Frame, card: Rect, surface: DockSurface, enabled: bool) {
    if card.width < 4 || card.height < 3 {
        return;
    }
    let border = Style::default().fg(if enabled {
        app.palette.surface_dim
    } else {
        app.palette.overlay0
    });
    let label = Style::default().fg(if enabled {
        app.palette.text
    } else {
        app.palette.overlay0
    });
    let key = Style::default().fg(if enabled {
        app.palette.accent
    } else {
        app.palette.overlay0
    });
    let hint = Style::default().fg(app.palette.overlay1);

    let inner = usize::from(card.width.saturating_sub(2));
    let shortcut = surface.shortcut().unwrap_or(' ');
    let title = surface.title();
    let gap = inner.saturating_sub(title.chars().count() + 1);

    let mut lines = vec![
        Line::from(Span::styled(format!("┌{}┐", "─".repeat(inner)), border)),
        Line::from(vec![
            Span::styled("│", border),
            Span::styled(format!("{title}{}", " ".repeat(gap)), label),
            Span::styled(shortcut.to_string(), key),
            Span::styled("│", border),
        ]),
    ];
    if card.height >= 4 {
        let text = surface.hint();
        let padded: String = text.chars().take(inner).collect();
        lines.push(Line::from(vec![
            Span::styled("│", border),
            Span::styled(
                format!(
                    "{padded}{}",
                    " ".repeat(inner.saturating_sub(padded.chars().count()))
                ),
                hint,
            ),
            Span::styled("│", border),
        ]));
    }
    lines.push(Line::from(Span::styled(
        format!("└{}┘", "─".repeat(inner)),
        border,
    )));

    frame.render_widget(Paragraph::new(lines), card);
}

/// Popup geometry for the `+` menu, anchored below the `+` and clamped to the
/// dock. `None` when the menu is closed or nothing fits below the anchor.
pub(crate) fn menu_layout(app: &AppState, dock: Rect) -> Option<DropdownLayout> {
    let menu = app.dock_surface_menu?;
    layout_dropdown(
        &DropdownSpec {
            anchor: app.view.dock_plus_rect,
            item_count: DockSurface::ALL.len(),
            selected: menu.selected,
            has_filter: false,
            max_rows: DockSurface::ALL.len(),
            min_width: MENU_WIDTH,
        },
        dock,
    )
}

pub(crate) fn render_menu(app: &AppState, frame: &mut Frame) {
    let Some(layout) = app.view.dock_surface_menu_layout else {
        return;
    };
    let Some(menu) = app.dock_surface_menu else {
        return;
    };
    let (context, in_git_repo) = focused_availability(app);

    frame.render_widget(Clear, layout.rect);
    frame.render_widget(
        Paragraph::new(
            (0..layout.rect.height)
                .map(|_| Line::from(" ".repeat(usize::from(layout.rect.width))))
                .collect::<Vec<_>>(),
        )
        .style(Style::default().bg(app.palette.surface_dim)),
        layout.rect,
    );

    for row in 0..layout.visible_rows {
        let index = layout.first_visible + row;
        let Some(surface) = DockSurface::ALL.get(index).copied() else {
            break;
        };
        let enabled = surface_available(surface, &context, in_git_repo);
        let selected = index == menu.selected;
        let style = if !enabled {
            Style::default().fg(app.palette.overlay0)
        } else if selected {
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.palette.text)
        };
        let width = usize::from(layout.list_rect.width);
        let title = surface.title();
        let shortcut = surface
            .shortcut()
            .map(|key| key.to_string())
            .unwrap_or_default();
        let gap = width
            .saturating_sub(2)
            .saturating_sub(title.chars().count())
            .saturating_sub(shortcut.chars().count());
        let text = format!(" {title}{}{shortcut} ", " ".repeat(gap));
        let area = Rect::new(
            layout.list_rect.x,
            layout
                .list_rect
                .y
                .saturating_add(u16::try_from(row).unwrap_or(u16::MAX)),
            layout.list_rect.width,
            1,
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(text, style)))
                .style(Style::default().bg(app.palette.surface_dim)),
            area,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::DockSurfaceMenu;

    fn context(prs: &[&str], tickets: &[&str]) -> PaneWorkContext {
        PaneWorkContext {
            pr_urls: prs.iter().map(|url| (*url).to_string()).collect(),
            ticket_ids: tickets.iter().map(|id| (*id).to_string()).collect(),
            ..PaneWorkContext::default()
        }
    }

    #[test]
    fn availability_follows_the_focused_pane_not_the_workspace() {
        use std::path::PathBuf;

        let repo_root = PathBuf::from("/repo/herdr");
        let outside = PathBuf::from("/tmp/t3-3a-outside");

        let mut app = AppState::test_new();
        let mut workspace = crate::workspace::Workspace::test_new("mixed");
        let inside_pane = workspace.focused_pane_id().expect("focused pane");
        let outside_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.cached_git_space = Some(crate::workspace::GitSpaceMetadata {
            key: "herdr".into(),
            checkout_key: repo_root.display().to_string(),
            repo_name: "herdr".into(),
            repo_root: repo_root.clone(),
            is_linked_worktree: false,
        });
        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.ensure_test_terminals();

        let cwd_of = |app: &AppState, pane| {
            app.workspaces[0]
                .terminal_id(pane)
                .expect("terminal")
                .clone()
        };
        let inside_terminal = cwd_of(&app, inside_pane);
        let outside_terminal = cwd_of(&app, outside_pane);
        app.terminals
            .get_mut(&inside_terminal)
            .expect("inside terminal")
            .cwd = repo_root.join("src");
        app.terminals
            .get_mut(&outside_terminal)
            .expect("outside terminal")
            .cwd = outside.clone();

        // The split focused the new pane, which sits outside every repository.
        assert_eq!(app.workspaces[0].focused_pane_id(), Some(outside_pane));
        let (_, in_git_repo) = focused_availability(&app);
        assert!(
            !in_git_repo,
            "a pane outside the repository must not offer Diff"
        );
        assert!(!surface_available(
            DockSurface::Diff,
            &context(&[], &[]),
            in_git_repo
        ));

        // Observations from the Git refresh answer both panes, and still per cwd.
        app.git_root_for_cwd.insert(outside.clone(), None);
        app.git_root_for_cwd
            .insert(repo_root.join("src"), Some(repo_root.clone()));
        let (_, in_git_repo) = focused_availability(&app);
        assert!(!in_git_repo);

        app.workspaces[0]
            .active_tab_mut()
            .expect("active tab")
            .layout
            .focus_pane(inside_pane);
        let (_, in_git_repo) = focused_availability(&app);
        assert!(in_git_repo, "a pane inside the repository offers Diff");
        assert!(surface_available(
            DockSurface::Diff,
            &context(&[], &[]),
            in_git_repo
        ));

        // The runtime projection changes before the terminal snapshot after a
        // shell `cd`, so availability must prefer it when both are present.
        app.status_focused_cwd = Some(outside);
        assert!(!focused_in_git_repo(&app));
        app.status_focused_cwd = Some(repo_root.join("src"));
        assert!(focused_in_git_repo(&app));
    }

    #[test]
    fn availability_matrix_follows_the_focused_pane() {
        let empty = context(&[], &[]);
        let linked = context(&["https://github.com/o/r/pull/1"], &["MAT-1"]);

        for (surface, in_repo, ctx, expected) in [
            (DockSurface::Diff, true, &empty, true),
            (DockSurface::Diff, false, &empty, false),
            (DockSurface::Diff, false, &linked, false),
            (DockSurface::Pr, true, &empty, false),
            (DockSurface::Pr, false, &linked, true),
            (DockSurface::Linear, true, &empty, false),
            (DockSurface::Linear, false, &linked, true),
            (DockSurface::Terminal, false, &empty, true),
            (DockSurface::Files, false, &empty, true),
            (DockSurface::Agents, false, &empty, true),
            (DockSurface::Home, false, &empty, true),
            (DockSurface::Editor, false, &empty, true),
            (DockSurface::Shortcuts, false, &empty, true),
            (DockSurface::Context, false, &empty, true),
            (DockSurface::Scratchpad, false, &empty, true),
        ] {
            assert_eq!(
                surface_available(surface, ctx, in_repo),
                expected,
                "{surface:?} in_git_repo={in_repo}"
            );
        }
    }

    #[test]
    fn card_hit_areas_tile_two_columns_without_overlapping() {
        let area = Rect::new(4, 2, 30, 20);
        let cards = card_hit_areas(area);

        assert_eq!(cards.len(), DockSurface::CARDS.len());
        assert_eq!(cards[0].y, area.y + HEADER_ROWS);
        assert_eq!(cards[0].height, 4);
        assert_eq!(cards[1].y, cards[0].y);
        assert!(cards[1].x >= cards[0].right());
        assert_eq!(cards[2].y, cards[0].y + 4);
        for card in &cards {
            assert!(card.x >= area.x);
            assert!(card.right() <= area.right());
            assert!(card.bottom() <= area.bottom());
        }
    }

    #[test]
    fn a_short_dock_drops_the_cards_that_do_not_fit() {
        let cards = card_hit_areas(Rect::new(0, 0, 30, 10));
        assert_eq!(cards.len(), 4);
        assert!(cards.iter().all(|card| card.bottom() <= 10));
        assert!(card_hit_areas(Rect::new(0, 0, 30, 5)).is_empty());
    }

    #[test]
    fn a_narrow_dock_falls_back_to_one_column() {
        let cards = card_hit_areas(Rect::new(0, 0, 20, 30));
        assert_eq!(cards.len(), DockSurface::CARDS.len());
        assert!(cards.iter().all(|card| card.x == cards[0].x));
    }

    #[test]
    fn the_surface_menu_opens_below_the_plus_and_never_above_it() {
        let mut app = AppState::test_new();
        let dock = Rect::new(80, 1, 32, 24);
        app.view.dock_plus_rect = Rect::new(94, 1, 2, 1);
        assert_eq!(menu_layout(&app, dock), None, "closed menu has no geometry");

        app.dock_surface_menu = Some(DockSurfaceMenu { selected: 0 });
        let layout = menu_layout(&app, dock).expect("the menu fits below the plus");
        assert_eq!(layout.rect.y, app.view.dock_plus_rect.bottom());
        assert!(layout.rect.y > app.view.dock_plus_rect.y);
        assert_eq!(layout.visible_rows, DockSurface::ALL.len());
        assert!(layout.rect.right() <= dock.right());

        // Anchored on the last row there is no space below, and the menu
        // renders nothing rather than flipping upwards.
        app.view.dock_plus_rect = Rect::new(94, dock.bottom() - 1, 2, 1);
        assert_eq!(menu_layout(&app, dock), None);
    }

    #[test]
    fn the_menu_hit_test_maps_rows_to_surfaces() {
        let mut app = AppState::test_new();
        let dock = Rect::new(80, 1, 32, 24);
        app.view.dock_plus_rect = Rect::new(94, 1, 2, 1);
        app.dock_surface_menu = Some(DockSurfaceMenu { selected: 0 });
        let layout = menu_layout(&app, dock).expect("menu geometry");
        app.view.dock_surface_menu_layout = Some(layout);

        assert_eq!(
            app.dock_surface_menu_at(layout.list_rect.x, layout.list_rect.y),
            Some(DockSurface::Terminal)
        );
        assert_eq!(
            app.dock_surface_menu_at(layout.list_rect.x, layout.list_rect.y + 2),
            Some(DockSurface::Diff)
        );
        assert_eq!(
            app.dock_surface_menu_at(layout.list_rect.x, layout.list_rect.y - 1),
            None
        );
        assert_eq!(
            app.dock_surface_menu_at(layout.list_rect.right(), layout.list_rect.y),
            None
        );
    }
}
