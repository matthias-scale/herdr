//! Attach-local read-only quota rows for the notepad's Usage tab.

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use std::collections::HashSet;

use crate::app::state::AppState;
use crate::provider_usage::{ProviderAccountUsage, QuotaProvider, QuotaWindow};

use super::text::{display_width, truncate_end};

pub(crate) const TAB_LABEL: &str = "usage";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotepadUsageAction {
    None,
    OpenDashboard,
}

#[derive(Debug, Clone)]
pub(crate) struct NotepadUsageRow {
    pub(crate) line: Line<'static>,
    pub(crate) action: NotepadUsageAction,
}

fn provider_presentation(provider: QuotaProvider, app: &AppState) -> (&'static str, Color) {
    match provider {
        QuotaProvider::Claude => ("CC", app.palette.peach),
        QuotaProvider::Codex => ("CX", app.palette.blue),
        QuotaProvider::Kimi => ("KI", app.palette.mauve),
    }
}

fn window_text(label: &str, window: Option<QuotaWindow>, now: i64) -> String {
    let Some(window) = window else {
        return format!("{label} —");
    };
    let reset = window
        .resets_at
        .and_then(|at| crate::provider_usage::reset_label(at, now))
        .unwrap_or_else(|| "—".into());
    format!("{label} {}% {reset}", window.used_percent)
}

fn account_label(account: &ProviderAccountUsage) -> String {
    if account.profile_id == "default" || account.label == account.profile_id {
        account.label.clone()
    } else {
        format!("{}/{}", account.label, account.profile_id)
    }
}

fn window_percent(window: Option<QuotaWindow>) -> String {
    window.map_or_else(|| "—".into(), |window| window.used_percent.to_string())
}

fn compact_window_text(label: &str, window: Option<QuotaWindow>, now: i64) -> String {
    let Some(window) = window else {
        return format!("{label}—");
    };
    let reset = window
        .resets_at
        .and_then(|at| crate::provider_usage::reset_label(at, now));
    reset.map_or_else(
        || format!("{label}{}", window.used_percent),
        |reset| format!("{label}{}@{reset}", window.used_percent),
    )
}

fn narrow_reset_text(window: Option<QuotaWindow>, now: i64) -> String {
    let Some(reset_at) = window.and_then(|window| window.resets_at) else {
        return "—".into();
    };
    let remaining = reset_at.saturating_sub(now);
    if remaining <= 0 {
        return "0m".into();
    }
    let hours = (remaining.saturating_add(3_599) / 3_600).clamp(1, 9);
    if remaining < 86_400 {
        format!("{hours}h")
    } else {
        let days = (remaining.saturating_add(86_399) / 86_400).clamp(1, 9);
        format!("{days}d")
    }
}

fn provider_initial(provider: QuotaProvider) -> char {
    match provider {
        QuotaProvider::Claude => 'C',
        QuotaProvider::Codex => 'X',
        QuotaProvider::Kimi => 'K',
    }
}

fn label_initials(label: &str) -> (char, char) {
    let mut alphanumeric = label.chars().filter(|ch| ch.is_ascii_alphanumeric());
    let first = alphanumeric.next().unwrap_or('A').to_ascii_uppercase();
    let last = alphanumeric
        .next_back()
        .unwrap_or(first)
        .to_ascii_uppercase();
    (first, last)
}

fn narrow_account_labels(accounts: &[ProviderAccountUsage]) -> Vec<String> {
    let mut used = HashSet::new();
    accounts
        .iter()
        .map(|account| {
            let provider = provider_initial(account.provider);
            let (first, last) = label_initials(&account.label);
            let preferred = format!("{provider}{first}{last}");
            if used.insert(preferred.clone()) {
                return preferred;
            }
            for suffix in '0'..='9' {
                let candidate = format!("{provider}{first}{suffix}");
                if used.insert(candidate.clone()) {
                    return candidate;
                }
            }
            for suffix in 'A'..='Z' {
                let candidate = format!("{provider}{first}{suffix}");
                if used.insert(candidate.clone()) {
                    return candidate;
                }
            }
            preferred
        })
        .collect()
}

fn account_row(
    app: &AppState,
    account: &ProviderAccountUsage,
    narrow_label: &str,
    width: u16,
) -> NotepadUsageRow {
    let (provider, color) = provider_presentation(account.provider, app);
    let now = app
        .status_now_unix
        .unwrap_or_else(|| app.view_observed_unix_s.min(i64::MAX as u64) as i64);
    let stale = if account.usage.stale { " stale" } else { "" };
    let stale_mark = if account.usage.stale { "~" } else { "" };
    let stats = [
        format!(
            "{} · {}{stale}",
            window_text("5h", account.usage.five_hour, now),
            window_text("7d", account.usage.seven_day, now),
        ),
        format!(
            "{} {} {stale_mark}",
            compact_window_text("5h", account.usage.five_hour, now),
            compact_window_text("7d", account.usage.seven_day, now),
        )
        .trim_end()
        .to_string(),
    ]
    .into_iter()
    .find(|stats| display_width(stats) + display_width(provider) + 5 <= usize::from(width))
    .unwrap_or_else(|| {
        format!(
            "{}/{}@{}/{}{stale_mark}",
            window_percent(account.usage.five_hour),
            window_percent(account.usage.seven_day),
            narrow_reset_text(account.usage.five_hour, now),
            narrow_reset_text(account.usage.seven_day, now),
        )
    });
    if display_width(&stats) + display_width(provider) + 5 > usize::from(width) {
        return NotepadUsageRow {
            line: Line::from(Span::styled(
                truncate_end(&format!("{narrow_label} {stats}"), usize::from(width)),
                super::status::provider_style(&account.usage, color, &app.palette),
            )),
            action: NotepadUsageAction::OpenDashboard,
        };
    }
    let identity_width =
        usize::from(width).saturating_sub(display_width(provider) + display_width(&stats) + 2);
    let full_identity = account_label(account);
    let identity = if display_width(&full_identity) <= identity_width {
        full_identity
    } else {
        narrow_label.to_string()
    };
    let text = format!("{provider} {identity} {stats}");
    let style = super::status::provider_style(&account.usage, color, &app.palette);
    NotepadUsageRow {
        line: Line::from(Span::styled(truncate_end(&text, usize::from(width)), style)),
        action: NotepadUsageAction::OpenDashboard,
    }
}

pub(crate) fn usage_rows_window(
    app: &AppState,
    width: u16,
    scroll: usize,
    visible: usize,
) -> (Vec<NotepadUsageRow>, usize) {
    let visible = visible.max(1);
    let max_scroll = app.provider_usage.accounts.len().saturating_sub(visible);
    let scroll = scroll.min(max_scroll);
    if app.provider_usage.accounts.is_empty() {
        return (
            vec![NotepadUsageRow {
                line: Line::from(Span::styled(
                    truncate_end("no account usage", usize::from(width)),
                    Style::default().fg(app.palette.overlay0),
                )),
                action: NotepadUsageAction::None,
            }],
            0,
        );
    }
    (
        narrow_account_labels(&app.provider_usage.accounts)
            .into_iter()
            .zip(app.provider_usage.accounts.iter())
            .skip(scroll)
            .take(visible)
            .map(|(narrow_label, account)| account_row(app, account, &narrow_label, width))
            .collect(),
        max_scroll,
    )
}

pub(crate) fn render_usage_body(app: &AppState, frame: &mut Frame, body: Rect) {
    for (offset, row) in app
        .view
        .notepad_usage_rows
        .iter()
        .take(usize::from(body.height))
        .enumerate()
    {
        frame.render_widget(
            Paragraph::new(row.line.clone()),
            Rect::new(body.x, body.y.saturating_add(offset as u16), body.width, 1),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_usage::{AccountUsage, ProviderUsageSnapshot, QuotaWindow};

    #[test]
    fn rows_keep_every_account_even_when_labels_match() {
        let mut app = AppState::test_new();
        app.provider_usage = ProviderUsageSnapshot::with_primary_accounts(
            AccountUsage {
                five_hour: Some(QuotaWindow {
                    used_percent: 91,
                    resets_at: Some(1_800_000_000),
                }),
                ..AccountUsage::default()
            },
            AccountUsage::default(),
            AccountUsage::default(),
        );
        app.provider_usage.accounts[0].label = "same".into();
        app.provider_usage.accounts[1].label = "same".into();
        app.provider_usage.accounts[0].profile_id = "lane-a".into();
        app.provider_usage.accounts[1].profile_id = "lane-b".into();

        let (rows, max_scroll) = usage_rows_window(&app, 80, 0, 10);

        assert_eq!(rows.len(), 3);
        assert_eq!(max_scroll, 0);
        assert_eq!(rows[0].action, NotepadUsageAction::OpenDashboard);
        let text = rows
            .iter()
            .map(|row| row.line.spans[0].content.as_ref())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("same/lane-a"), "{text}");
        assert!(text.contains("same/lane-b"), "{text}");
    }

    #[test]
    fn a_wide_row_names_both_windows_resets_and_staleness() {
        let mut app = AppState::test_new();
        app.status_now_unix = Some(1_800_000_000);
        app.provider_usage = ProviderUsageSnapshot::with_primary_accounts(
            AccountUsage {
                five_hour: Some(QuotaWindow {
                    used_percent: 81,
                    resets_at: Some(1_800_000_720),
                }),
                seven_day: Some(QuotaWindow {
                    used_percent: 92,
                    resets_at: Some(1_800_009_900),
                }),
                stale: true,
                ..AccountUsage::default()
            },
            AccountUsage::default(),
            AccountUsage::default(),
        );
        app.provider_usage.accounts[0].label = "SHQ".into();
        app.provider_usage.accounts[0].profile_id = "scalablehq".into();

        let (wide, _) = usage_rows_window(&app, 80, 0, 10);
        let text = wide[0].line.spans[0].content.as_ref();
        for expected in ["SHQ/scalablehq", "5h 81% 12m", "7d 92% 2h45", "stale"] {
            assert!(text.contains(expected), "missing {expected:?}: {text}");
        }
        let (narrow, _) = usage_rows_window(&app, 18, 0, 10);
        let narrow = narrow[0].line.spans[0].content.as_ref();
        assert!(narrow.chars().count() <= 18);
        assert!(narrow.contains("81/92@1h/3h~"), "{narrow}");
        assert!(narrow.starts_with("CSQ "), "{narrow}");
    }

    #[test]
    fn narrow_rows_distinguish_similar_profile_ids() {
        let mut app = AppState::test_new();
        app.status_now_unix = Some(1_800_000_000);
        app.provider_usage = ProviderUsageSnapshot::with_primary_accounts(
            AccountUsage::default(),
            AccountUsage {
                five_hour: Some(QuotaWindow {
                    used_percent: 100,
                    resets_at: Some(1_800_003_600),
                }),
                seven_day: Some(QuotaWindow {
                    used_percent: 100,
                    resets_at: Some(1_800_172_800),
                }),
                stale: true,
                ..AccountUsage::default()
            },
            AccountUsage::default(),
        );
        let mut scalable_so = app.provider_usage.accounts[1].clone();
        scalable_so.label = "SSO".into();
        scalable_so.profile_id = "scalable-so".into();
        let mut scalable_so_2 = scalable_so.clone();
        scalable_so_2.profile_id = "scalable-so-2".into();
        let mut matthias_scalablehq = scalable_so.clone();
        matthias_scalablehq.label = "SHQ".into();
        matthias_scalablehq.profile_id = "matthias-scalablehq".into();
        let mut matthias_scalable_so = scalable_so.clone();
        matthias_scalable_so.profile_id = "matthias-scalable-so".into();
        app.provider_usage.accounts = vec![
            scalable_so,
            scalable_so_2,
            matthias_scalablehq,
            matthias_scalable_so,
        ];

        let (rows, _) = usage_rows_window(&app, 18, 0, 10);
        let labels = rows
            .iter()
            .map(|row| row.line.spans[0].content.split(' ').next().unwrap_or(""))
            .collect::<Vec<_>>();
        assert_eq!(labels, ["XSO", "XS0", "XSQ", "XS1"]);
        assert!(rows
            .iter()
            .all(|row| row.line.spans[0].content.ends_with("100/100@1h/2d~")));

        let (medium_rows, _) = usage_rows_window(&app, 26, 0, 10);
        let medium = medium_rows
            .iter()
            .map(|row| row.line.spans[0].content.as_ref())
            .collect::<Vec<_>>();
        assert!(medium[0].contains("XSO"), "{}", medium[0]);
        assert!(medium[1].contains("XS0"), "{}", medium[1]);
        assert!(medium[2].contains("XSQ"), "{}", medium[2]);
        assert!(medium[3].contains("XS1"), "{}", medium[3]);
    }
}
