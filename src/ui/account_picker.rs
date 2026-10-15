//! One geometry contract for Home/Spawn rendering and mouse targets.
use super::text::truncate_end;
use crate::app::{
    account_picker::{quota_lines, PickerFocus},
    home::HomeState,
    state::AppState,
};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickerHit {
    Account(usize),
    Model(usize),
    Close,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PickerGeometry {
    pub(crate) popup: Rect,
    pub(crate) accounts: Rect,
    pub(crate) search: Rect,
    pub(crate) models: Rect,
    pub(crate) usage: Rect,
    pub(crate) footer: Rect,
}

pub(crate) fn geometry(home: &HomeState, area: Rect, anchor: Rect) -> PickerGeometry {
    let account_rows = home.account_picker.rows.len().clamp(1, 8) as u16;
    let model_rows = home.filtered_account_models().len().clamp(1, 8) as u16;
    let rail_width = home
        .account_picker
        .rows
        .iter()
        .map(|row| row.label.chars().count() as u16 + 12)
        .max()
        .unwrap_or(24)
        .clamp(24, 36);
    let model_width = home
        .model_options()
        .iter()
        .map(|model| model.short_name().chars().count() as u16 + 4)
        .max()
        .unwrap_or(24)
        .clamp(24, 40);
    let width = area.width.min((rail_width + model_width + 2).clamp(64, 88));
    let body_height = if width >= 64 {
        account_rows.max(model_rows + 1)
    } else {
        account_rows + model_rows + 1
    };
    let desired_height = (body_height + 8 + 3).min(28);
    let below = area
        .bottom()
        .saturating_sub(anchor.bottom().clamp(area.y, area.bottom()));
    let above = anchor.y.clamp(area.y, area.bottom()).saturating_sub(area.y);
    let use_below = below >= desired_height || below >= above;
    let height = desired_height
        .min(if use_below { below } else { above })
        .min(area.height);
    let x = anchor.x.clamp(area.x, area.right().saturating_sub(width));
    let y = if use_below {
        anchor.bottom().clamp(area.y, area.bottom())
    } else {
        anchor.y.clamp(area.y, area.bottom()).saturating_sub(height)
    };
    let popup = Rect::new(x, y, width, height);
    let inner = Block::default().borders(Borders::ALL).inner(popup);
    let footer = Rect::new(
        inner.x,
        inner.bottom().saturating_sub(1).max(inner.y),
        inner.width,
        u16::from(inner.height > 0),
    );
    let usage_height = inner.height.saturating_sub(5).min(8);
    let usage = Rect::new(
        inner.x,
        footer.y.saturating_sub(usage_height).max(inner.y),
        inner.width,
        usage_height,
    );
    let body_height = usage.y.saturating_sub(inner.y);
    let (accounts, search, models) = if width >= 64 {
        let rail_width = rail_width.min(inner.width.saturating_sub(20));
        let accounts = Rect::new(inner.x, inner.y, rail_width, body_height);
        let model_x = accounts.right().min(inner.right());
        let model_width = inner.right().saturating_sub(model_x);
        (
            accounts,
            Rect::new(model_x, inner.y, model_width, u16::from(body_height > 0)),
            Rect::new(
                model_x,
                inner.y.saturating_add(1).min(usage.y),
                model_width,
                body_height.saturating_sub(1),
            ),
        )
    } else {
        let rail_height = account_rows
            .min(body_height.saturating_sub(2))
            .max(u16::from(body_height > 0));
        let accounts = Rect::new(inner.x, inner.y, inner.width, rail_height);
        let search_height = u16::from(body_height > rail_height);
        let search = Rect::new(inner.x, accounts.bottom(), inner.width, search_height);
        let models = Rect::new(
            inner.x,
            search.bottom(),
            inner.width,
            body_height.saturating_sub(rail_height + search_height),
        );
        (accounts, search, models)
    };
    PickerGeometry {
        popup,
        accounts,
        search,
        models,
        usage,
        footer,
    }
}

pub(crate) fn scroll_start(cursor: usize, height: u16, len: usize) -> usize {
    cursor
        .min(len.saturating_sub(1))
        .saturating_sub(usize::from(height).saturating_sub(1))
}

fn contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.right() && y >= rect.y && y < rect.bottom()
}

pub(crate) fn hit_test(
    home: &HomeState,
    area: Rect,
    anchor: Rect,
    x: u16,
    y: u16,
) -> Option<PickerHit> {
    if !home.account_picker.active {
        return None;
    }
    let g = geometry(home, area, anchor);
    if !contains(g.popup, x, y) {
        return Some(PickerHit::Close);
    }
    if contains(g.accounts, x, y) {
        let start = scroll_start(
            home.account_picker.account_cursor,
            g.accounts.height,
            home.account_picker.rows.len(),
        );
        let index = start + usize::from(y - g.accounts.y);
        return (index < home.account_picker.rows.len()).then_some(PickerHit::Account(index));
    }
    if contains(g.models, x, y) {
        let len = home.filtered_account_models().len();
        let index = scroll_start(home.account_picker.model_cursor, g.models.height, len)
            + usize::from(y - g.models.y);
        return (index < len).then_some(PickerHit::Model(index));
    }
    None
}

pub(crate) fn scroll(home: &mut HomeState, area: Rect, anchor: Rect, x: u16, y: u16, down: bool) {
    let g = geometry(home, area, anchor);
    let focus = if contains(g.accounts, x, y) {
        PickerFocus::Accounts
    } else if contains(g.models, x, y) {
        PickerFocus::Models
    } else {
        return;
    };
    home.account_picker.focus = focus;
    home.account_picker_key(crossterm::event::KeyEvent::new(
        if down {
            crossterm::event::KeyCode::Down
        } else {
            crossterm::event::KeyCode::Up
        },
        crossterm::event::KeyModifiers::NONE,
    ));
}

pub(crate) fn apply_hit(home: &mut HomeState, hit: PickerHit) {
    match hit {
        PickerHit::Account(index) => {
            home.select_account_row(index);
            home.account_picker.focus = PickerFocus::Accounts;
        }
        PickerHit::Model(index) => {
            home.select_account_model(index);
        }
        PickerHit::Close => home.close_account_picker(),
    }
}

fn provider_token(app: &AppState, agent: crate::detect::Agent) -> &'static str {
    if app.nerd_font {
        super::icons::agent_icon(agent)
    } else {
        super::icons::agent_letter_tag(agent)
    }
    .unwrap_or("?")
}

fn row_agent(
    home: &HomeState,
    row: &crate::app::account_picker::AccountRow,
) -> crate::detect::Agent {
    home.profiles()
        .iter()
        .find(|profile| profile.id == row.profile)
        .map(|profile| profile.agent)
        .unwrap_or(home.agent)
}

fn styled_token_line(label: &str, token: &str, color: Color, base: Style) -> Line<'static> {
    let Some(start) = label.find(token) else {
        return Line::from(Span::styled(label.to_string(), base));
    };
    let end = start + token.len();
    let mut spans = Vec::with_capacity(3);
    if start > 0 {
        spans.push(Span::styled(label[..start].to_string(), base));
    }
    spans.push(Span::styled(token.to_string(), base.fg(color)));
    if end < label.len() {
        spans.push(Span::styled(label[end..].to_string(), base));
    }
    Line::from(spans)
}

pub(crate) fn selected_label_line(
    app: &AppState,
    home: &HomeState,
    label: &str,
    base: Style,
) -> Line<'static> {
    let agent = home
        .account_picker
        .rows
        .iter()
        .find(|row| row.profile == home.profile_id() && row.selection == home.account_selection)
        .map(|row| row_agent(home, row))
        .unwrap_or(home.profile().agent);
    styled_token_line(
        label,
        provider_token(app, agent),
        super::icons::agent_color(agent, &app.palette),
        base,
    )
}

fn row_label(
    app: &AppState,
    home: &HomeState,
    row: &crate::app::account_picker::AccountRow,
) -> String {
    let agent = row_agent(home, row);
    format!("{} {}", provider_token(app, agent), row.label)
}

/// Shared presentation for popup rows and chosen-provider triggers; no semantic IDs change.
pub(crate) fn selected_label(app: &AppState, home: &HomeState) -> String {
    if let Some(row) = home
        .account_picker
        .rows
        .iter()
        .find(|row| row.profile == home.profile_id() && row.selection == home.account_selection)
    {
        return row_label(app, home, row);
    }
    let profile = home.profile();
    let account = home
        .account_selection
        .as_ref()
        .map(|selection| selection.account.as_str())
        .unwrap_or("inherited");
    let builtin = profile
        .id
        .eq_ignore_ascii_case(crate::detect::agent_label(profile.agent))
        && profile
            .label
            .eq_ignore_ascii_case(crate::detect::agent_label(profile.agent));
    let identity = if builtin {
        account.to_string()
    } else {
        format!("{} / {account}", profile.label)
    };
    format!("{} {identity}", provider_token(app, profile.agent))
}

pub(crate) fn render(
    app: &AppState,
    home: &HomeState,
    frame: &mut Frame,
    area: Rect,
    anchor: Rect,
) {
    if !home.account_picker.active {
        return;
    }
    let g = geometry(home, area, anchor);
    let base = Style::default()
        .fg(app.palette.text)
        .bg(app.palette.panel_bg);
    frame.render_widget(Clear, g.popup);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(" provider / account / model ")
            .border_style(Style::default().fg(app.palette.accent))
            .style(base),
        g.popup,
    );
    let start = scroll_start(
        home.account_picker.account_cursor,
        g.accounts.height,
        home.account_picker.rows.len(),
    );
    for (offset, row) in home
        .account_picker
        .rows
        .iter()
        .skip(start)
        .take(g.accounts.height.into())
        .enumerate()
    {
        let active = start + offset == home.account_picker.account_cursor;
        let prefix = if active { "> " } else { "  " };
        let usage = row
            .selection
            .as_ref()
            .and_then(|selection| {
                app.provider_usage.accounts.iter().find(|account| {
                    account.provider == selection.provider
                        && account.profile_id == selection.account
                })
            })
            .map(|account| &account.usage);
        let unavailable = home.account_row_unavailable(row);
        let quota = compact_quota(usage, unavailable);
        let token = provider_token(app, row_agent(home, row));
        let label = format!("{prefix}{}", row_label(app, home, row));
        let label_width = g
            .accounts
            .width
            .saturating_sub(super::text::display_width(&quota) as u16 + 1);
        let shown = truncate_end(&label, label_width.into());
        let full = format!("{shown} {quota}");
        render_row(
            app,
            frame,
            Rect::new(
                g.accounts.x,
                g.accounts.y + offset as u16,
                g.accounts.width,
                1,
            ),
            styled_token_line(
                &full,
                token,
                super::icons::agent_color(row_agent(home, row), &app.palette),
                Style::default().fg(app.palette.text),
            ),
            active,
            home.account_picker.focus == PickerFocus::Accounts,
        );
    }
    frame.render_widget(
        Paragraph::new(truncate_end(
            &format!("/ {}", home.account_picker.query),
            g.search.width.into(),
        ))
        .style(base.add_modifier(Modifier::BOLD)),
        g.search,
    );
    let matches = home.filtered_account_models();
    let start = scroll_start(
        home.account_picker.model_cursor,
        g.models.height,
        matches.len(),
    );
    if matches.is_empty() {
        let label = if home.profile().owns_its_flags() {
            "model flags owned by launcher"
        } else {
            "no matching model"
        };
        frame.render_widget(
            Paragraph::new(truncate_end(label, g.models.width.into())).style(base),
            g.models,
        );
    }
    for (offset, index) in matches
        .iter()
        .skip(start)
        .take(g.models.height.into())
        .enumerate()
    {
        let active = start + offset == home.account_picker.model_cursor;
        let model = &home.model_options()[*index];
        let marker = if model.id == home.model { "* " } else { "  " };
        let label = format!("{marker}{}", model.short_name());
        let shown = truncate_end(&label, g.models.width.into());
        let model_start = marker.len().min(shown.len());
        let line = Line::from(vec![
            Span::raw(shown[..model_start].to_string()),
            Span::styled(
                shown[model_start..].to_string(),
                Style::default().fg(super::icons::agent_color(
                    home.profile().agent,
                    &app.palette,
                )),
            ),
        ]);
        render_row(
            app,
            frame,
            Rect::new(g.models.x, g.models.y + offset as u16, g.models.width, 1),
            line,
            active,
            home.account_picker.focus == PickerFocus::Models,
        );
    }
    let mut lines = vec![home.account_label()];
    if let Err(reason) = home.account_launch_env() {
        lines.push(reason);
    } else if home
        .account_selection
        .as_ref()
        .is_some_and(|selection| selection.provider == crate::provider_usage::QuotaProvider::Claude)
    {
        lines.push("subscription authentication checked at launch".into());
    }
    lines.extend(quota_lines(
        home.selected_account_usage(&app.provider_usage),
        app.provider_usage.collected_unix,
    ));
    for (offset, line) in lines.iter().take(g.usage.height.into()).enumerate() {
        frame.render_widget(
            Paragraph::new(truncate_end(line, g.usage.width.into())).style(base),
            Rect::new(g.usage.x, g.usage.y + offset as u16, g.usage.width, 1),
        );
    }
    frame.render_widget(
        Paragraph::new(truncate_end(
            "tab/arrows · search · enter · esc | ⊘ unavailable ! auth ◷ stale ■ exhausted ? unknown",
            g.footer.width.into(),
        ))
        .style(base),
        g.footer,
    );
}

fn compact_quota(usage: Option<&crate::provider_usage::AccountUsage>, unavailable: bool) -> String {
    use crate::provider_usage::UsageUnavailable;
    let mut status = String::new();
    if unavailable {
        status.push('⊘');
    }
    if let Some(usage) = usage {
        if usage.unavailable == Some(UsageUnavailable::AuthExpired) {
            status.push('!');
        } else if usage.unavailable.is_some() && !unavailable {
            status.push('⊘');
        }
        if usage.stale {
            status.push('◷');
        }
        match usage.peak_percent() {
            Some(used) => {
                if used >= 100 {
                    status.push('■');
                }
                status.push_str(&format!("{}%", 100u8.saturating_sub(used)));
            }
            None => status.push('?'),
        }
    } else {
        status.push('?');
    }
    status
}

fn render_row(
    app: &AppState,
    frame: &mut Frame,
    rect: Rect,
    label: Line<'static>,
    active: bool,
    focused: bool,
) {
    let style = Style::default().fg(app.palette.text).bg(if active {
        app.palette.selection_bg
    } else {
        app.palette.panel_bg
    });
    frame.render_widget(
        Paragraph::new(label).style(if active && focused {
            style.add_modifier(Modifier::BOLD)
        } else {
            style
        }),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::home::HomePicker;
    use ratatui::{backend::TestBackend, Terminal};
    #[test]
    fn account_picker_selected_labels_share_sidebar_tokens_and_short_lane_identity() {
        use crate::app::launch_profiles::LaunchProfile;
        use crate::provider_usage::{ProviderAccountUsage, ProviderUsageSnapshot, QuotaProvider};
        for nerd_font in [false, true] {
            let mut app = AppState::test_new();
            app.nerd_font = nerd_font;
            for agent in [crate::detect::Agent::Claude, crate::detect::Agent::Codex] {
                let token = if nerd_font {
                    super::super::icons::agent_icon(agent)
                } else {
                    super::super::icons::agent_letter_tag(agent)
                }
                .expect("sidebar token");
                for custom in [false, true] {
                    let mut profile = LaunchProfile::builtin(agent);
                    if custom {
                        profile.id = "lab".into();
                        profile.label = "Lab".into();
                    }
                    let mut home = HomeState::default();
                    home.set_profiles(vec![profile]);
                    let lane = if custom { "Lab / " } else { "" };
                    assert_eq!(
                        selected_label(&app, &home),
                        format!("{token} {lane}inherited"),
                        "fallback before rows exist"
                    );
                    let mut snapshot = ProviderUsageSnapshot::default();
                    snapshot.accounts.push(ProviderAccountUsage {
                        provider: if agent == crate::detect::Agent::Claude {
                            QuotaProvider::Claude
                        } else {
                            QuotaProvider::Codex
                        },
                        profile_id: "scalablehq".into(),
                        label: "SHQ".into(),
                        usage: Default::default(),
                    });
                    home.refresh_account_rows(&snapshot);
                    assert_eq!(
                        selected_label(&app, &home),
                        format!("{token} {lane}inherited")
                    );
                    home.select_account_row(1);
                    assert_eq!(selected_label(&app, &home), format!("{token} {lane}SHQ"));
                    assert_eq!(
                        home.account_selection.as_ref().expect("selection").account,
                        "scalablehq"
                    );
                    assert_eq!(
                        selected_label(&app, &home),
                        row_label(&app, &home, &home.account_picker.rows[1])
                    );
                    assert!(
                        !selected_label(&app, &home).contains('%'),
                        "unknown never fabricates quota"
                    );
                }
            }
        }
    }

    #[test]
    fn account_picker_rows_use_sidebar_icons_short_identity_and_each_quota() {
        use crate::provider_usage::{
            AccountUsage, ProviderAccountUsage, ProviderUsageSnapshot, QuotaProvider, QuotaWindow,
        };
        for nerd_font in [false, true] {
            let mut app = AppState::test_new();
            app.nerd_font = nerd_font;
            let mut snapshot = ProviderUsageSnapshot::default();
            for (id, used) in [("one", 41), ("two", 73)] {
                snapshot.accounts.push(ProviderAccountUsage {
                    provider: QuotaProvider::Claude,
                    profile_id: id.into(),
                    label: "team".into(),
                    usage: AccountUsage {
                        five_hour: Some(QuotaWindow {
                            used_percent: used,
                            resets_at: None,
                        }),
                        ..AccountUsage::default()
                    },
                });
            }
            app.provider_usage = snapshot;
            let mut home = HomeState::default();
            home.refresh_account_rows(&app.provider_usage);
            home.open_account_picker(HomePicker::Agent);
            let area = Rect::new(0, 0, 100, 40);
            let anchor = Rect::new(12, 5, 10, 1);
            let g = geometry(&home, area, anchor);
            let mut terminal = Terminal::new(TestBackend::new(100, 40)).expect("terminal");
            terminal
                .draw(|frame| render(&app, &home, frame, area, anchor))
                .expect("render");
            let rail: String = (g.accounts.y..g.accounts.bottom())
                .flat_map(|y| (g.accounts.x..g.accounts.right()).map(move |x| (x, y)))
                .map(|point| terminal.backend().buffer()[point].symbol())
                .collect();
            let icon = if nerd_font {
                super::super::icons::agent_icon(crate::detect::Agent::Claude)
            } else {
                super::super::icons::agent_letter_tag(crate::detect::Agent::Claude)
            }
            .expect("provider token");
            assert!(rail.contains(icon));
            for label in ["inherited", "team (one)", "team (two)", "59%", "27%", "⊘"] {
                assert!(rail.contains(label), "{label}: {rail:?}");
            }
            assert!(
                !rail.contains("Claude /")
                    && !rail.contains("% left")
                    && !rail.contains("unavailable")
            );
        }
    }

    #[test]
    fn account_picker_brand_colors_match_provider_and_keep_selected_background() {
        for theme in ["catppuccin-latte", "catppuccin"] {
            let mut app = AppState::test_new();
            app.nerd_font = true;
            app.palette = crate::app::state::Palette::from_name(theme).expect("theme");
            let mut home = HomeState::default();
            home.refresh_account_rows(&app.provider_usage);
            home.open_account_picker(HomePicker::Model);
            let area = Rect::new(0, 0, 100, 40);
            let anchor = Rect::new(12, 5, 10, 1);
            let g = geometry(&home, area, anchor);
            let mut terminal = Terminal::new(TestBackend::new(100, 40)).expect("terminal");
            terminal
                .draw(|frame| render(&app, &home, frame, area, anchor))
                .expect("render");
            let buffer = terminal.backend().buffer();

            let claude_row = home
                .account_picker
                .rows
                .iter()
                .position(|row| row.profile == "claude")
                .expect("Claude account row");
            let codex_row = home
                .account_picker
                .rows
                .iter()
                .position(|row| row.profile == "codex")
                .expect("Codex account row");
            let claude_icon =
                super::super::icons::agent_icon(crate::detect::Agent::Claude).expect("Claude icon");
            let codex_icon =
                super::super::icons::agent_icon(crate::detect::Agent::Codex).expect("Codex icon");
            assert_eq!(
                buffer[(g.accounts.x + 2, g.accounts.y + claude_row as u16)].symbol(),
                claude_icon
            );
            assert_eq!(
                buffer[(g.accounts.x + 2, g.accounts.y + codex_row as u16)].symbol(),
                codex_icon
            );
            assert_eq!(
                buffer[(g.accounts.x + 2, g.accounts.y + claude_row as u16)].fg,
                super::super::icons::agent_color(crate::detect::Agent::Claude, &app.palette),
                "Claude provider icon color on {theme}"
            );
            assert_eq!(
                buffer[(g.accounts.x + 2, g.accounts.y + codex_row as u16)].fg,
                app.palette.text,
                "Codex remains monochrome on {theme}"
            );
            assert_eq!(
                buffer[(g.accounts.x, g.accounts.y)].bg,
                app.palette.selection_bg,
                "selected row background on {theme}"
            );
            assert_eq!(
                buffer[(g.models.x + 2, g.models.y)].fg,
                super::super::icons::agent_color(crate::detect::Agent::Claude, &app.palette),
                "Claude model name color on {theme}"
            );
        }
    }

    #[test]
    fn account_picker_compact_quota_preserves_unknown_stale_expired_and_exhausted() {
        use crate::provider_usage::{AccountUsage, QuotaWindow, UsageUnavailable};
        let mut usage = AccountUsage::default();
        assert_eq!(compact_quota(None, false), "?");
        assert_eq!(compact_quota(Some(&usage), true), "⊘?");
        usage.five_hour = Some(QuotaWindow {
            used_percent: 41,
            resets_at: None,
        });
        assert_eq!(compact_quota(Some(&usage), false), "59%");
        assert_eq!(compact_quota(Some(&usage), true), "⊘59%");
        usage.stale = true;
        assert_eq!(compact_quota(Some(&usage), false), "◷59%");
        usage.unavailable = Some(UsageUnavailable::AuthExpired);
        assert_eq!(compact_quota(Some(&usage), false), "!◷59%");
        usage.stale = false;
        usage.unavailable = None;
        usage.five_hour.as_mut().expect("window").used_percent = 100;
        assert_eq!(compact_quota(Some(&usage), false), "■0%");
        usage.unavailable = Some(UsageUnavailable::HelperMissing);
        assert_eq!(compact_quota(Some(&usage), false), "⊘■0%");
    }

    #[test]
    fn account_picker_scroll_hits_rendered_rows_in_offset_narrow_view() {
        let mut app = AppState::test_new();
        let mut home = HomeState::default();
        home.refresh_account_rows(&Default::default());
        home.open_account_picker(HomePicker::Model);
        let area = Rect::new(3, 2, 52, 36);
        let anchor = Rect::new(42, 33, 10, 1);
        let g = geometry(&home, area, anchor);
        let original_model = home.model.clone();
        for _ in 0..20 {
            scroll(&mut home, area, anchor, g.models.x, g.models.y, true);
        }
        assert_eq!(home.model, original_model, "scroll is not model activation");
        let g = geometry(&home, area, anchor);
        let matches = home.filtered_account_models();
        let start = scroll_start(
            home.account_picker.model_cursor,
            g.models.height,
            matches.len(),
        );
        let mut terminal = Terminal::new(TestBackend::new(60, 40)).expect("terminal");
        terminal
            .draw(|frame| render(&app, &home, frame, area, anchor))
            .expect("render");
        let expected = home.model_options()[matches[start]].short_name();
        let row: String = (g.models.x..g.models.right())
            .map(|x| terminal.backend().buffer()[(x, g.models.y)].symbol())
            .collect();
        assert!(row.contains(expected), "{row:?} must show {expected}");
        assert_eq!(
            hit_test(&home, area, anchor, g.models.x, g.models.y),
            Some(PickerHit::Model(start))
        );
        let cursor = home.account_picker.model_cursor;
        scroll(&mut home, area, anchor, g.footer.x, g.footer.y, false);
        assert_eq!(
            home.account_picker.model_cursor, cursor,
            "footer is not a scroll target"
        );
        app.home = Some(home);
    }

    #[test]
    fn account_picker_anchor_content_bounds_flip_and_clamp() {
        let home = HomeState::default();
        let area = Rect::new(0, 0, 194, 120);
        let anchor = Rect::new(70, 22, 12, 1);
        let g = geometry(&home, area, anchor);
        assert_eq!(g.popup.y, anchor.bottom());
        assert_eq!(g.popup.x, anchor.x);
        assert!(g.popup.height < 24, "small catalogs must size to content");
        for area in [
            Rect::new(5, 7, 52, 36),
            Rect::new(3, 4, 18, 10),
            Rect::new(1, 2, 2, 2),
        ] {
            let anchor = Rect::new(area.right() - 1, area.bottom() - 1, 1, 1);
            let g = geometry(&home, area, anchor);
            assert!(g.popup.x >= area.x && g.popup.y >= area.y);
            assert!(g.popup.right() <= area.right() && g.popup.bottom() <= area.bottom());
            assert!(g.popup.bottom() <= anchor.y);
            assert_eq!(g.accounts.x, g.models.x, "narrow layout remains stacked");
        }
    }
    #[test]
    fn account_picker_normal_and_narrow_render_hit_geometry_and_scroll_match() {
        for (width, height) in [(100, 30), (40, 24), (30, 12), (18, 10), (2, 2)] {
            let mut app = AppState::test_new();
            let mut home = HomeState::default();
            home.refresh_account_rows(&Default::default());
            home.open_account_picker(HomePicker::Agent);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
            let area = Rect::new(0, 0, width, height);
            let anchor = Rect::new(0, 0, width.min(12), 1);
            terminal
                .draw(|frame| render(&app, &home, frame, area, anchor))
                .expect("render");
            let g = geometry(&home, area, anchor);
            assert!(g.popup.right() <= area.right() && g.popup.bottom() <= area.bottom());
            if g.accounts.height > 0 && g.accounts.width > 0 {
                assert_eq!(
                    hit_test(&home, area, anchor, g.accounts.x, g.accounts.y),
                    Some(PickerHit::Account(0))
                );
                apply_hit(&mut home, PickerHit::Account(1));
                assert_eq!(home.profile_id(), "codex");
            }
            app.home = Some(home);
        }
        assert_eq!(scroll_start(12, 3, 15), 10);
    }
}
