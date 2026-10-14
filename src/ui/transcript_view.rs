//! Read-only chat projection of the live Claude transcript.

use std::time::{Duration, SystemTime};

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::app::state::Palette;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ChatRow {
    User {
        body: String,
        time: Option<String>,
    },
    Assistant {
        body: String,
    },
    Worked {
        duration: String,
        tools: Vec<String>,
    },
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TranscriptChat {
    rows: Vec<ChatRow>,
    pending_tools: Vec<String>,
    pending_since: Option<SystemTime>,
    pending_prose: String,
    content_revision: u64,
    layout: Option<TranscriptLayout>,
}

#[derive(Clone, Debug)]
struct TranscriptLayout {
    content_revision: u64,
    width: u16,
    expand_tools: bool,
    palette: Palette,
    lines: Vec<Line<'static>>,
}

impl PartialEq for TranscriptChat {
    fn eq(&self, other: &Self) -> bool {
        self.rows == other.rows
            && self.pending_tools == other.pending_tools
            && self.pending_since == other.pending_since
            && self.pending_prose == other.pending_prose
    }
}

impl Eq for TranscriptChat {}

impl TranscriptChat {
    fn push_row(&mut self, row: ChatRow) {
        self.rows.push(row);
        if self.rows.len() > 500 {
            self.rows.drain(..100);
        }
        self.content_revision = self.content_revision.wrapping_add(1);
    }

    pub(crate) fn prepare_layout(
        &mut self,
        palette: &Palette,
        width: u16,
        expand_tools: bool,
    ) -> bool {
        if self.layout.as_ref().is_some_and(|layout| {
            layout.content_revision == self.content_revision
                && layout.width == width
                && layout.expand_tools == expand_tools
                && layout.palette == *palette
        }) {
            return false;
        }
        let lines = layout_lines(&self.rows, palette, width, expand_tools);
        self.layout = Some(TranscriptLayout {
            content_revision: self.content_revision,
            width,
            expand_tools,
            palette: palette.clone(),
            lines,
        });
        true
    }

    fn layout_lines(&self) -> &[Line<'static>] {
        self.layout
            .as_ref()
            .map_or(&[], |layout| layout.lines.as_slice())
    }

    pub(crate) fn update_from(&mut self, mut updated: Self) {
        if self.content_revision == updated.content_revision && self.rows == updated.rows {
            updated.layout = self.layout.take();
        }
        *self = updated;
    }

    pub(crate) fn ingest(&mut self, value: &serde_json::Value, at: Option<SystemTime>) {
        if crate::turns::real_user(value) {
            let body = crate::turns::message_text(value);
            if body.trim().is_empty() || hidden_assistant_text(&body) {
                return;
            }
            self.pending_tools.clear();
            self.pending_since = at;
            self.pending_prose.clear();
            self.push_row(ChatRow::User {
                body,
                time: at
                    .and_then(crate::agent_state::format_rfc3339)
                    .and_then(|v| v.get(11..16).map(str::to_owned)),
            });
            return;
        }
        if value["type"] != "assistant" || value["isSidechain"] == true || value["isMeta"] == true {
            return;
        }
        let Some(items) = value["message"]["content"].as_array() else {
            return;
        };
        let mut prose = Vec::new();
        for item in items {
            match item["type"].as_str() {
                Some("tool_use") => {
                    let name = item["name"].as_str().unwrap_or("tool");
                    let input = compact_input(&item["input"]);
                    self.pending_tools.push(if input.is_empty() {
                        name.to_owned()
                    } else {
                        format!("{name} {input}")
                    });
                    if self.pending_tools.len() > 500 {
                        self.pending_tools.drain(..100);
                    }
                }
                Some("text") => {
                    if let Some(text) = item["text"].as_str() {
                        prose.push(text);
                    }
                }
                _ => {}
            }
        }
        let body = prose.join("\n");
        if !body.trim().is_empty() && !hidden_assistant_text(&body) {
            if !self.pending_prose.is_empty() {
                self.pending_prose.push('\n');
            }
            self.pending_prose.push_str(&body);
        }
        if !matches!(
            value["message"]["stop_reason"].as_str(),
            Some("end_turn" | "stop_sequence")
        ) {
            return;
        }
        let body = std::mem::take(&mut self.pending_prose);
        if body.trim().is_empty() {
            return;
        }
        if !self.pending_tools.is_empty() {
            let duration = self.pending_since.zip(at).map_or_else(
                || "0s".to_owned(),
                |(start, end)| format_duration(end.duration_since(start).unwrap_or_default()),
            );
            let tools = std::mem::take(&mut self.pending_tools);
            self.push_row(ChatRow::Worked { duration, tools });
        }
        self.pending_since = None;
        self.push_row(ChatRow::Assistant { body });
    }
}

fn compact_input(value: &serde_json::Value) -> String {
    let text = match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => value.to_string(),
        _ => String::new(),
    };
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(60)
        .collect()
}

fn hidden_assistant_text(text: &str) -> bool {
    let body = text.trim_start();
    [
        "<system-reminder",
        "<task-notification",
        "<closing-state",
        "```closing-state",
        "Stop hook feedback",
        "<local-command",
    ]
    .iter()
    .any(|prefix| body.starts_with(prefix) || body.contains(prefix))
}

fn format_duration(duration: Duration) -> String {
    if duration.as_secs() >= 60 {
        format!("{}m {}s", duration.as_secs() / 60, duration.as_secs() % 60)
    } else {
        format!("{}s", duration.as_secs())
    }
}

fn user_block_metrics(area_width: u16, text_width: usize) -> (usize, usize) {
    let max_width = (area_width as usize * 4 / 5)
        .max(1)
        .min(area_width as usize);
    let content = text_width.min(max_width.saturating_sub(2));
    let block = (content + 2).min(area_width as usize);
    (area_width as usize - block, block)
}

fn worked_row_lines(duration: &str, tools: &[String], expand: bool) -> Vec<String> {
    let tool_count = tools.len();
    let tool_label = if tool_count == 1 { "tool" } else { "tools" };
    let mut lines = vec![format!(
        "Worked for {duration} · {tool_count} {tool_label} ›"
    )];
    if expand {
        lines.extend(tools.iter().cloned());
    }
    lines
}

fn layout_lines(
    rows: &[ChatRow],
    palette: &Palette,
    width: u16,
    expand_tools: bool,
) -> Vec<Line<'static>> {
    let mut lines = Vec::<Line<'static>>::new();
    for row in rows {
        match row {
            ChatRow::User { body, time } => {
                let max_width = (width as usize * 4 / 5).max(1).min(width as usize);
                let inner_width = max_width.saturating_sub(2).max(1);
                let mut body_lines = crate::ui::markdown::transcript_body_lines(
                    palette,
                    Some(body),
                    inner_width,
                    "",
                );
                for line in &mut body_lines {
                    let used = line.width().min(max_width.saturating_sub(2));
                    let (left, _) = user_block_metrics(width, used);
                    let bubble = Style::default().fg(palette.text).bg(palette.surface0);
                    let mut padded = vec![Span::raw(" ".repeat(left)), Span::styled(" ", bubble)];
                    padded.extend(
                        std::mem::take(&mut line.spans)
                            .into_iter()
                            .map(|span| Span::styled(span.content.to_string(), bubble)),
                    );
                    padded.push(Span::styled(" ", bubble));
                    lines.push(Line::from(padded));
                }
                if let Some(time) = time {
                    lines.push(Line::from(Span::styled(
                        format!("{time:>width$}", width = width as usize),
                        Style::default().fg(palette.overlay0),
                    )));
                }
                lines.push(Line::default());
            }
            ChatRow::Assistant { body } => {
                let mut body_lines = crate::ui::markdown::transcript_body_lines(
                    palette,
                    Some(body),
                    width.saturating_sub(4).min(100) as usize,
                    "  ",
                );
                lines.append(&mut body_lines);
                lines.push(Line::default());
                lines.push(Line::default());
            }
            ChatRow::Worked { duration, tools } => {
                for row in worked_row_lines(duration, tools, expand_tools) {
                    lines.push(Line::from(Span::styled(
                        row,
                        Style::default().fg(palette.overlay0),
                    )));
                }
            }
        }
    }
    if lines.last().is_some_and(|line| line.width() == 0) {
        lines.pop();
    }
    lines
}

pub(crate) fn render(
    frame: &mut Frame,
    area: Rect,
    chat: &TranscriptChat,
    palette: &Palette,
    scroll: usize,
    return_key: &str,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(Clear, area);
    let footer_height = 1;
    let body_height = area.height.saturating_sub(footer_height) as usize;
    let lines = chat.layout_lines();
    let max_scroll = lines.len().saturating_sub(body_height);
    let offset = max_scroll.saturating_sub(scroll.min(max_scroll));
    let visible = lines
        .iter()
        .skip(offset)
        .take(body_height)
        .cloned()
        .collect::<Vec<_>>();
    let body = Rect::new(
        area.x,
        area.y,
        area.width,
        area.height.saturating_sub(footer_height),
    );
    frame.render_widget(Paragraph::new(visible), body);
    let footer = Rect::new(area.x, area.y + area.height - 1, area.width, 1);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("chat view · {return_key} to return"),
            Style::default().fg(palette.overlay0),
        )))
        .block(Block::default().borders(Borders::NONE)),
        footer,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_keeps_prose_and_collapses_tool_calls() {
        let mut chat = TranscriptChat::default();
        chat.ingest(
            &serde_json::json!({"type":"user","message":{"content":"hello"}}),
            None,
        );
        chat.ingest(&serde_json::json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file":"src/main.rs"}}]}}), None);
        chat.ingest(&serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","content":"secret"}]}}), None);
        chat.ingest(&serde_json::json!({"type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"Done."}]}}), None);
        assert_eq!(chat.rows.len(), 3);
        assert!(
            matches!(&chat.rows[1], ChatRow::Worked { tools, .. } if tools == &["Read {\"file\":\"src/main.rs\"}".to_owned()])
        );
        assert!(!format!("{:?}", chat.rows).contains("secret"));
    }

    #[test]
    fn hides_system_and_task_text() {
        for text in [
            "<system-reminder>hidden",
            "<task-notification>hidden",
            "**Needs you: nothing.**\n```closing-state\n{}\n```",
            "Stop hook feedback",
        ] {
            let mut chat = TranscriptChat::default();
            chat.ingest(
                &serde_json::json!({"type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"text","text":text}]}}),
                None,
            );
            assert!(chat.rows.is_empty(), "unexpectedly rendered {text:?}");
        }
    }

    #[test]
    fn user_blocks_stay_within_eighty_percent_and_right_align() {
        for width in [60, 100, 160] {
            let (left, block) = user_block_metrics(width, usize::MAX);
            assert_eq!(left + block, width as usize);
            assert!(block <= width as usize * 4 / 5);
        }
    }

    #[test]
    fn worked_rows_collapse_and_expand_to_tool_summaries() {
        let duration = "29s";
        let tools = vec!["Read src/main.rs".to_owned(), "Bash cargo check".to_owned()];
        assert_eq!(
            worked_row_lines(duration, &tools, false),
            ["Worked for 29s · 2 tools ›"]
        );
        assert_eq!(
            worked_row_lines(duration, &tools, true),
            [
                "Worked for 29s · 2 tools ›",
                "Read src/main.rs",
                "Bash cargo check"
            ]
        );
    }

    #[test]
    fn layout_cache_reuses_lines_and_rebuilds_for_content_width_and_tool_expansion() {
        let palette = Palette::catppuccin();
        let mut chat = TranscriptChat::default();
        chat.push_row(ChatRow::Assistant {
            body: "A long transcript reply that wraps at narrower widths.".into(),
        });
        chat.push_row(ChatRow::Worked {
            duration: "1s".into(),
            tools: vec!["Read file.rs".into()],
        });

        assert!(chat.prepare_layout(&palette, 40, false));
        assert!(!chat.prepare_layout(&palette, 40, false));
        assert!(chat.prepare_layout(&palette, 30, false));
        assert!(chat.prepare_layout(&palette, 30, true));
        assert!(chat
            .layout_lines()
            .iter()
            .any(|line| line.to_string() == "Read file.rs"));

        chat.push_row(ChatRow::Assistant {
            body: "New content".into(),
        });
        assert!(chat.prepare_layout(&palette, 30, true));
    }

    #[test]
    fn updating_pending_transcript_state_preserves_layout_for_unchanged_rows() {
        let palette = Palette::catppuccin();
        let mut chat = TranscriptChat::default();
        chat.push_row(ChatRow::Assistant {
            body: "Already visible".into(),
        });
        assert!(chat.prepare_layout(&palette, 80, false));

        let mut updated = chat.clone();
        updated.layout = None;
        updated.pending_prose = "More text is still streaming".into();
        chat.update_from(updated);

        assert!(!chat.prepare_layout(&palette, 80, false));
    }

    #[test]
    fn worked_row_uses_singular_tool_label() {
        assert_eq!(
            worked_row_lines("1s", &["Read file.rs".into()], false),
            ["Worked for 1s · 1 tool ›"]
        );
    }
}
