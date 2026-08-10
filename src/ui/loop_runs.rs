use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::{
    layout::{Constraint, Rect},
    style::{Modifier, Style},
    widgets::{Block, Borders, Cell, Row, Table},
    Frame,
};

use crate::app::state::Palette;
use crate::loop_runs::{duration_label, RunHistory};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoopRunRowProjection {
    pub(crate) run_id: String,
    pub(crate) outcome: String,
    pub(crate) gates: String,
    pub(crate) touches: String,
    pub(crate) duration: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoopRunHistoryProjection {
    pub(crate) loop_id: String,
    pub(crate) rows: Vec<LoopRunRowProjection>,
    pub(crate) skipped_lines: u64,
}

pub(crate) fn project_loop_run_history(
    history: &RunHistory,
    loop_id: &str,
    now: SystemTime,
) -> LoopRunHistoryProjection {
    let now_unix_seconds = now
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok());
    LoopRunHistoryProjection {
        loop_id: loop_id.to_string(),
        rows: history
            .runs
            .iter()
            .map(|run| LoopRunRowProjection {
                run_id: run.run_id.clone(),
                outcome: run.outcome.label().to_string(),
                gates: if run.gates.is_empty() {
                    "0".to_string()
                } else {
                    run.gates
                        .iter()
                        .map(|gate| gate.kind.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                },
                touches: run
                    .human_touches
                    .map(|touches| touches.to_string())
                    .unwrap_or_else(|| "—".to_string()),
                duration: duration_label(run, now_unix_seconds),
            })
            .collect(),
        skipped_lines: history.skipped_lines,
    }
}

pub(crate) fn render_loop_run_history(
    palette: &Palette,
    history: &RunHistory,
    loop_id: &str,
    area: Rect,
    now: SystemTime,
    frame: &mut Frame,
) {
    let projection = project_loop_run_history(history, loop_id, now);
    let title = if projection.skipped_lines == 0 {
        format!(" loop: {} · run history ", projection.loop_id)
    } else {
        format!(
            " loop: {} · run history · skipped {} malformed lines ",
            projection.loop_id, projection.skipped_lines
        )
    };
    let rows = projection.rows.iter().map(|row| {
        Row::new([
            Cell::from(row.run_id.as_str()),
            Cell::from(row.outcome.as_str()),
            Cell::from(row.gates.as_str()),
            Cell::from(row.touches.as_str()),
            Cell::from(row.duration.as_str()),
        ])
        .style(outcome_style(palette, row.outcome.as_str()))
    });
    let table = Table::new(
        rows,
        [
            Constraint::Min(18),
            Constraint::Length(12),
            Constraint::Min(12),
            Constraint::Length(8),
            Constraint::Length(18),
        ],
    )
    .header(
        Row::new(["run", "outcome", "gates", "touches", "duration"]).style(
            Style::default()
                .fg(palette.subtext0)
                .add_modifier(Modifier::BOLD),
        ),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(palette.accent)),
    )
    .column_spacing(1);
    frame.render_widget(table, area);
}

fn outcome_style(palette: &Palette, outcome: &str) -> Style {
    let color = match outcome {
        "merged" => palette.green,
        "vanished" => palette.peach,
        "failed" | "blocked" => palette.red,
        "in_flight" => palette.yellow,
        _ => palette.text,
    };
    Style::default().fg(color)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppState;
    use crate::loop_runs::parse_receipts;
    use crate::workspace::Workspace;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn ac6_projection_surfaces_required_run_fields_with_injected_time() {
        let history = parse_receipts(
            "{\"run_id\":\"run\",\"skill\":\"aship\",\"start\":\"2026-08-10T10:00:00Z\",\"end\":\"2026-08-10T10:01:00Z\",\"wall_min\":1,\"gates\":[{\"kind\":\"preference\"}],\"human_touches\":2,\"outcome\":\"merged\"}",
        );
        let projection = project_loop_run_history(
            &history,
            "daily",
            UNIX_EPOCH + std::time::Duration::from_secs(1_786_272_000),
        );
        assert_eq!(projection.loop_id, "daily");
        assert_eq!(projection.rows[0].outcome, "merged");
        assert_eq!(projection.rows[0].gates, "preference");
        assert_eq!(projection.rows[0].touches, "2");
        assert_eq!(projection.rows[0].duration, "1m");
    }

    #[test]
    fn ac3_vanished_has_distinct_projection_value() {
        let history = parse_receipts(
            "{\"run_id\":\"vanished\",\"skill\":\"aship\",\"start\":\"2026-08-10T10:00:00Z\",\"end\":\"2026-08-10T10:01:00Z\",\"outcome\":\"vanished\"}",
        );
        let projection = project_loop_run_history(&history, "daily", UNIX_EPOCH);
        assert_eq!(projection.rows[0].outcome, "vanished");
    }

    #[test]
    fn ac7_render_is_unit_testable_without_a_pty() {
        let app = AppState::test_new();
        let _workspace = Workspace::test_new("history");
        let history = parse_receipts(
            "{\"run_id\":\"merged\",\"skill\":\"aship\",\"start\":\"2026-08-10T10:00:00Z\",\"end\":\"2026-08-10T10:01:00Z\",\"wall_min\":1,\"gates\":[{\"kind\":\"preference\"}],\"human_touches\":2,\"outcome\":\"merged\"}",
        );
        let backend = TestBackend::new(100, 8);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| {
                render_loop_run_history(
                    &app.palette,
                    &history,
                    "daily",
                    Rect::new(0, 0, 100, 8),
                    UNIX_EPOCH,
                    frame,
                );
            })
            .expect("render history");
        let contents = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(contents.contains("merged"));
        assert!(contents.contains("preference"));
        assert!(contents.contains("2"));
        assert!(contents.contains("1m"));
    }
}
