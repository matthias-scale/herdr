//! Full-screen log view for one aloop run record (MAT-159 AC7). Selecting a
//! clean run in the sidebar opens this surface; it renders the recorded log
//! excerpt read from the producer host, never the live pane.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::aloop::RunRecord;
use crate::app::state::Palette;

fn duration_text(duration_ms: u64) -> String {
    if duration_ms >= 60_000 {
        format!(
            "{}m {}s",
            duration_ms / 60_000,
            (duration_ms % 60_000) / 1_000
        )
    } else {
        format!("{:.1}s", duration_ms as f64 / 1_000.0)
    }
}

pub(crate) fn render_aloop_run_log(
    palette: &Palette,
    loop_name: &str,
    host: &str,
    run: &RunRecord,
    area: Rect,
    frame: &mut Frame,
) {
    let exit_style = if run.exit == 0 {
        Style::default().fg(palette.green)
    } else {
        Style::default().fg(palette.red)
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled("ran ", Style::default().fg(palette.overlay0)),
            Span::styled(run.at.clone(), Style::default().fg(palette.text)),
            Span::styled("  duration ", Style::default().fg(palette.overlay0)),
            Span::styled(
                duration_text(run.duration_ms),
                Style::default().fg(palette.text),
            ),
            Span::styled("  exit ", Style::default().fg(palette.overlay0)),
            Span::styled(run.exit.to_string(), exit_style),
        ]),
        Line::from(vec![
            Span::styled("findings ", Style::default().fg(palette.overlay0)),
            Span::styled(run.findings.to_string(), Style::default().fg(palette.text)),
        ]),
    ];
    if !run.stable_ids.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("stable ids ", Style::default().fg(palette.overlay0)),
            Span::styled(
                run.stable_ids.join(", "),
                Style::default().fg(palette.subtext0),
            ),
        ]));
    }
    lines.push(Line::from(""));
    if run.log_excerpt.is_empty() {
        lines.push(Line::from(Span::styled(
            "no log excerpt recorded",
            Style::default()
                .fg(palette.overlay0)
                .add_modifier(Modifier::DIM),
        )));
    } else {
        lines.extend(run.log_excerpt.lines().map(|line| {
            Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(palette.subtext0),
            ))
        }));
    }
    let title = format!(" aloop: {loop_name} · run log · on {host} ");
    let body = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(palette.accent)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(body, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn run(exit: i32, excerpt: &str) -> RunRecord {
        RunRecord {
            at: "2026-09-18T09:59:00Z".to_string(),
            at_unix_s: 1_789_999_999,
            duration_ms: 1_400,
            exit,
            findings: 1,
            stable_ids: vec!["abc123".to_string()],
            log_excerpt: excerpt.to_string(),
        }
    }

    #[test]
    fn ac7_run_log_renders_the_record_fields_and_excerpt() {
        let palette = Palette::catppuccin();
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_aloop_run_log(
                    &palette,
                    "watch-fleet",
                    "ub2",
                    &run(0, "first log line\nsecond log line"),
                    Rect::new(0, 0, 60, 10),
                    frame,
                );
            })
            .unwrap();
        let rows: Vec<String> = (0..10)
            .map(|y| {
                (0..60)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect()
            })
            .collect();
        let text = rows.join("\n");
        assert!(text.contains("aloop: watch-fleet"), "{text}");
        assert!(text.contains("2026-09-18T09:59:00Z"), "{text}");
        assert!(text.contains("1.4s"), "{text}");
        assert!(text.contains("findings 1"), "{text}");
        assert!(text.contains("abc123"), "{text}");
        assert!(text.contains("first log line"), "{text}");
        assert!(text.contains("second log line"), "{text}");
    }

    #[test]
    fn ac7_run_log_without_excerpt_says_so() {
        let palette = Palette::catppuccin();
        let backend = TestBackend::new(50, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_aloop_run_log(
                    &palette,
                    "loop",
                    "ub2",
                    &run(1, ""),
                    Rect::new(0, 0, 50, 8),
                    frame,
                );
            })
            .unwrap();
        let text: String = (0..8)
            .map(|y| {
                (0..50)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("no log excerpt recorded"), "{text}");
    }
}
