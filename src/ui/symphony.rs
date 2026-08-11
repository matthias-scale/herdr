use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::{
    layout::{Constraint, Rect},
    style::{Modifier, Style},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Frame,
};

use crate::app::state::Palette;
use crate::symphony::Snapshot;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SymphonyRowProjection {
    pub(crate) name: String,
    pub(crate) phase: String,
    pub(crate) wait: String,
    pub(crate) age: String,
}

pub(crate) fn project(snapshot: &Snapshot, now: SystemTime) -> Vec<SymphonyRowProjection> {
    let now = now
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok());
    snapshot
        .workflows
        .iter()
        .map(|workflow| SymphonyRowProjection {
            name: workflow.name.clone(),
            phase: workflow.phase.clone(),
            wait: workflow.wait.clone().unwrap_or_else(|| "—".to_string()),
            age: workflow
                .started_at
                .as_deref()
                .and_then(parse_utc_timestamp)
                .zip(now)
                .map(|(started, now)| age_label(now.saturating_sub(started).max(0) as u64))
                .unwrap_or_else(|| "—".to_string()),
        })
        .collect()
}

pub(crate) fn render(
    palette: &Palette,
    snapshot: &Snapshot,
    selected: usize,
    area: Rect,
    now: SystemTime,
    frame: &mut Frame,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Symphony · open workflows · Enter open window · Esc close ")
        .border_style(Style::default().fg(palette.accent));
    if let Some(message) = snapshot.unavailable.as_deref() {
        frame.render_widget(
            Paragraph::new(format!("Symphony runtime unavailable\n{message}"))
                .style(Style::default().fg(palette.red))
                .block(block),
            area,
        );
        return;
    }
    if snapshot.workflows.is_empty() {
        frame.render_widget(
            Paragraph::new("No open Symphony workflows.")
                .style(Style::default().fg(palette.subtext0))
                .block(block),
            area,
        );
        return;
    }

    let rows = project(snapshot, now)
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            let style = if index == selected {
                Style::default()
                    .fg(palette.text)
                    .bg(palette.surface1)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.text)
            };
            Row::new([
                Cell::from(row.name),
                Cell::from(row.phase),
                Cell::from(row.wait),
                Cell::from(row.age),
            ])
            .style(style)
        });
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(40),
            Constraint::Percentage(20),
            Constraint::Percentage(25),
            Constraint::Percentage(15),
        ],
    )
    .header(
        Row::new(["name", "phase", "named wait", "age"]).style(
            Style::default()
                .fg(palette.subtext0)
                .add_modifier(Modifier::BOLD),
        ),
    )
    .block(block)
    .column_spacing(1);
    frame.render_widget(table, area);
}

fn age_label(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3_600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

fn parse_utc_timestamp(value: &str) -> Option<i64> {
    let value = value.strip_suffix('Z')?;
    let (date, time) = value.split_once('T')?;
    let mut date = date.split('-').map(str::parse::<i64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    if date.next().is_some() || !(1..=12).contains(&month) {
        return None;
    }
    let mut time = time.split(':');
    let hour = time.next()?.parse::<i64>().ok()?;
    let minute = time.next()?.parse::<i64>().ok()?;
    let second = time.next()?.split('.').next()?.parse::<i64>().ok()?;
    if time.next().is_some()
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=59).contains(&second)
    {
        return None;
    }
    let max_day = match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(1..=max_day).contains(&day) {
        return None;
    }
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let adjusted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppState;
    use crate::symphony::Workflow;
    use ratatui::{backend::TestBackend, Terminal};

    fn workflow() -> Workflow {
        Workflow {
            workflow_id: "wf".to_string(),
            run_id: "run".to_string(),
            name: "Temporal blocker dashboard".to_string(),
            phase: "runFlowStep".to_string(),
            wait: Some("plan-sign-off".to_string()),
            started_at: Some("2026-08-11T08:00:00Z".to_string()),
            ticket: Some("MAT-138".to_string()),
            repo: Some("matthias-scale/herdr".to_string()),
            pr: None,
            receipts: None,
        }
    }

    #[test]
    fn projects_required_columns_with_injected_time() {
        let snapshot = Snapshot {
            workflows: vec![workflow()],
            unavailable: None,
        };
        let rows = project(
            &snapshot,
            UNIX_EPOCH + std::time::Duration::from_secs(1_786_521_600),
        );
        assert_eq!(rows[0].name, "Temporal blocker dashboard");
        assert_eq!(rows[0].phase, "runFlowStep");
        assert_eq!(rows[0].wait, "plan-sign-off");
        assert_eq!(rows[0].age, "1d");
    }

    #[test]
    fn renders_empty_and_unavailable_states_without_a_pty() {
        let app = AppState::test_new();
        for (snapshot, expected) in [
            (Snapshot::default(), "No open Symphony workflows"),
            (
                Snapshot {
                    unavailable: Some("Temporal runtime is unreachable".to_string()),
                    ..Snapshot::default()
                },
                "Symphony runtime unavailable",
            ),
        ] {
            let backend = TestBackend::new(80, 8);
            let mut terminal = Terminal::new(backend).expect("test terminal");
            terminal
                .draw(|frame| {
                    render(
                        &app.palette,
                        &snapshot,
                        0,
                        Rect::new(0, 0, 80, 8),
                        UNIX_EPOCH,
                        frame,
                    );
                })
                .expect("render Symphony");
            let contents = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(contents.contains(expected));
        }
    }
}
