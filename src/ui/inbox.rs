use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{inbox::BlockedAgent, AppState, Mode};
use crate::terminal::TerminalRuntimeRegistry;

/// How long the shown agent has been waiting, in the sidebar's own age vocabulary.
fn waited_label(agent: &BlockedAgent) -> String {
    match agent.blocked_since {
        // Minute-floor, like the sidebar: a wait that ticks per second would make
        // a calm queue feel like a countdown.
        Some(since) => crate::activity_age::coarse_label(Some(since), std::time::Instant::now()),
        // The transition was never observed. Saying so beats inventing a duration.
        None => "unknown".to_string(),
    }
}

/// `3 blocked · oldest 12m · workspace — agent`. Counts come first because they
/// tell the operator whether this is a queue or the last one.
fn header_line(app: &AppState, agent: &BlockedAgent, remaining: usize) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            format!(" {remaining} blocked"),
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" · waiting {} · ", waited_label(agent)),
            Style::default().fg(app.palette.overlay0),
        ),
        Span::styled(
            agent.workspace_label.clone(),
            Style::default().fg(app.palette.text),
        ),
    ];
    if !agent.agent_label.is_empty() {
        spans.push(Span::styled(
            format!(" — {}", agent.agent_label),
            Style::default().fg(app.palette.subtext0),
        ));
    }
    Line::from(spans)
}

fn hint_line(app: &AppState, deferred: usize) -> Line<'static> {
    let mut hint = " type to answer · tab defers · esc closes".to_string();
    if deferred > 0 {
        hint.push_str(&format!(" · {deferred} deferred"));
    }
    Line::from(Span::styled(
        hint,
        Style::default()
            .fg(app.palette.overlay0)
            .add_modifier(Modifier::DIM),
    ))
}

/// The zero state is the whole point of the mode, so it says the one thing the
/// operator came to find out and nothing else.
fn render_empty(app: &AppState, frame: &mut Frame, area: Rect) {
    let [_, middle, _] = Layout::vertical([
        Constraint::Percentage(45),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "nothing is blocked",
            Style::default()
                .fg(app.palette.overlay0)
                .add_modifier(Modifier::DIM),
        )))
        .centered(),
        middle,
    );
}

pub(crate) fn render_inbox(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    agent: Option<&BlockedAgent>,
    remaining: usize,
    deferred: usize,
    area: Rect,
    frame: &mut Frame,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(agent) = agent else {
        render_empty(app, frame, area);
        return;
    };
    if area.height < 3 {
        // Too short to frame a terminal; the count still beats rendering nothing.
        frame.render_widget(Paragraph::new(header_line(app, agent, remaining)), area);
        return;
    }

    let [header, body, hint] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);

    frame.render_widget(Paragraph::new(header_line(app, agent, remaining)), header);
    frame.render_widget(Paragraph::new(hint_line(app, deferred)), hint);

    match terminal_runtimes.get(&agent.terminal_id) {
        // The cursor is always shown: keystrokes go to this pane, so it owns the
        // caret even though herdr's own focus never moved here.
        Some(runtime) => runtime.render(frame, body, app.mode == Mode::Terminal),
        None => frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " agent terminal unavailable",
                Style::default()
                    .fg(app.palette.overlay0)
                    .add_modifier(Modifier::DIM),
            ))),
            body,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::PaneId;
    use crate::terminal::{TerminalId, TerminalRuntime};
    use ratatui::{backend::TestBackend, Terminal};
    use std::time::{Duration, Instant};

    fn blocked(terminal_id: TerminalId, blocked_since: Option<Instant>) -> BlockedAgent {
        BlockedAgent {
            ws_idx: 0,
            pane_id: PaneId::alloc(),
            terminal_id,
            workspace_label: "herdr".to_string(),
            agent_label: "codex".to_string(),
            blocked_since,
            seq: None,
        }
    }

    fn draw(
        app: &AppState,
        runtimes: &TerminalRuntimeRegistry,
        agent: Option<&BlockedAgent>,
        remaining: usize,
        deferred: usize,
        area: Rect,
    ) -> String {
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).expect("term");
        terminal
            .draw(|frame| {
                render_inbox(app, runtimes, agent, remaining, deferred, area, frame);
            })
            .expect("render inbox");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn an_empty_queue_says_nothing_is_blocked() {
        let app = AppState::test_new();
        let text = draw(
            &app,
            &TerminalRuntimeRegistry::new(),
            None,
            0,
            0,
            Rect::new(0, 0, 40, 8),
        );
        assert!(text.contains("nothing is blocked"), "rendered: {text:?}");
    }

    // `test_with_screen_bytes` spawns a detection task, so these need a runtime.
    #[tokio::test(flavor = "current_thread")]
    async fn the_header_carries_the_count_the_wait_and_the_workspace() {
        let app = AppState::test_new();
        let terminal_id = TerminalId::alloc();
        let mut runtimes = TerminalRuntimeRegistry::new();
        runtimes.insert(
            terminal_id.clone(),
            TerminalRuntime::test_with_screen_bytes(60, 4, b"WAITING"),
        );
        let agent = blocked(terminal_id, Some(Instant::now() - Duration::from_secs(720)));

        let text = draw(&app, &runtimes, Some(&agent), 3, 0, Rect::new(0, 0, 60, 8));

        assert!(text.contains("3 blocked"), "rendered: {text:?}");
        assert!(text.contains("12m"), "rendered: {text:?}");
        assert!(text.contains("herdr"), "rendered: {text:?}");
    }

    // `test_with_screen_bytes` spawns a detection task, so these need a runtime.
    #[tokio::test(flavor = "current_thread")]
    async fn the_blocked_agents_own_terminal_is_rendered_inline() {
        let app = AppState::test_new();
        let terminal_id = TerminalId::alloc();
        let mut runtimes = TerminalRuntimeRegistry::new();
        runtimes.insert(
            terminal_id.clone(),
            TerminalRuntime::test_with_screen_bytes(60, 4, b"WAITING"),
        );
        let agent = blocked(terminal_id, None);

        let text = draw(&app, &runtimes, Some(&agent), 1, 0, Rect::new(0, 0, 60, 8));

        assert!(text.contains("WAITING"), "rendered: {text:?}");
    }

    #[test]
    fn an_unobserved_wait_says_unknown_rather_than_guessing() {
        let agent = blocked(TerminalId::alloc(), None);
        assert_eq!(waited_label(&agent), "unknown");
    }

    // `test_with_screen_bytes` spawns a detection task, so these need a runtime.
    #[tokio::test(flavor = "current_thread")]
    async fn deferred_agents_are_counted_in_the_hint() {
        let app = AppState::test_new();
        let terminal_id = TerminalId::alloc();
        let mut runtimes = TerminalRuntimeRegistry::new();
        runtimes.insert(
            terminal_id.clone(),
            TerminalRuntime::test_with_screen_bytes(60, 4, b"WAITING"),
        );
        let agent = blocked(terminal_id, None);

        let text = draw(&app, &runtimes, Some(&agent), 4, 2, Rect::new(0, 0, 60, 8));

        assert!(text.contains("2 deferred"), "rendered: {text:?}");
    }

    #[test]
    fn a_missing_runtime_reports_itself_instead_of_rendering_blank() {
        let app = AppState::test_new();
        let agent = blocked(TerminalId::alloc(), None);
        let text = draw(
            &app,
            &TerminalRuntimeRegistry::new(),
            Some(&agent),
            1,
            0,
            Rect::new(0, 0, 60, 8),
        );
        assert!(text.contains("terminal unavailable"), "rendered: {text:?}");
    }
}
