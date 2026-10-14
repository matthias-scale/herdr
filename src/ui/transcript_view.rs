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
            self.push_row(ChatRow::User { body });
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
                    self.pending_tools
                        .push(summarize_tool(name, &item["input"]));
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

fn summarize_tool(name: &str, input: &serde_json::Value) -> String {
    let argument = match name {
        "Bash" => string_field(input, &["command"]).map(str::to_owned),
        "Read" | "Edit" | "Write" => string_field(input, &["file_path", "path", "file"])
            .map(|path| path.rsplit(['/', '\\']).next().unwrap_or(path).to_owned()),
        "Grep" => string_field(input, &["pattern"]).map(str::to_owned),
        "WebFetch" => string_field(input, &["url"]).map(web_fetch_target),
        _ => first_string_argument(input).map(str::to_owned),
    }
    .or_else(|| first_string_argument(input).map(str::to_owned))
    .map(|argument| compact_text(&argument))
    .filter(|argument| !argument.is_empty());

    argument.map_or_else(|| name.to_owned(), |argument| format!("{name} {argument}"))
}

fn string_field<'a>(value: &'a serde_json::Value, fields: &[&str]) -> Option<&'a str> {
    fields
        .iter()
        .find_map(|field| value.get(*field).and_then(serde_json::Value::as_str))
}

fn first_string_argument(value: &serde_json::Value) -> Option<&str> {
    match value {
        serde_json::Value::String(text) => Some(text),
        serde_json::Value::Array(values) => values.iter().find_map(serde_json::Value::as_str),
        serde_json::Value::Object(values) => values.values().find_map(serde_json::Value::as_str),
        _ => None,
    }
}

fn compact_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn web_fetch_target(url: &str) -> String {
    let target = url.split_once("://").map_or(url, |(_, target)| target);
    target
        .split(['?', '#'])
        .next()
        .unwrap_or(target)
        .trim_end_matches('/')
        .to_owned()
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

fn worked_row_lines(duration: &str, tools: &[String], expand: bool, width: usize) -> Vec<String> {
    let tool_count = tools.len();
    let tool_label = if tool_count == 1 { "tool" } else { "tools" };
    let mut lines = vec![format!(
        "Worked for {duration} · {tool_count} {tool_label} ›"
    )];
    if expand {
        lines.extend(
            tools
                .iter()
                .map(|tool| crate::ui::text::truncate_end(tool, width)),
        );
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
            ChatRow::User { body } => {
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
                lines.push(Line::default());
            }
            ChatRow::Assistant { body } => {
                let mut body_lines = crate::ui::markdown::transcript_chat_body_lines(
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
                for row in worked_row_lines(duration, tools, expand_tools, width as usize) {
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
            matches!(&chat.rows[1], ChatRow::Worked { tools, .. } if tools == &["Read main.rs".to_owned()])
        );
        assert!(!format!("{:?}", chat.rows).contains("secret"));
    }

    #[test]
    fn tool_summaries_use_short_arguments() {
        assert_eq!(
            summarize_tool(
                "Bash",
                &serde_json::json!({"command":"ls -la /private/var/tmp/repro-586"})
            ),
            "Bash ls -la /private/var/tmp/repro-586"
        );
        assert_eq!(
            summarize_tool(
                "Read",
                &serde_json::json!({"file_path":"/private/var/tmp/notes.md"})
            ),
            "Read notes.md"
        );
        assert_eq!(
            summarize_tool("Edit", &serde_json::json!({"file_path":"src/register.tsx"})),
            "Edit register.tsx"
        );
        assert_eq!(
            summarize_tool("Grep", &serde_json::json!({"pattern":"TODO|FIXME"})),
            "Grep TODO|FIXME"
        );
        assert_eq!(
            summarize_tool(
                "WebFetch",
                &serde_json::json!({"url":"https://example.com/docs/page?q=1"})
            ),
            "WebFetch example.com/docs/page"
        );
        assert_eq!(
            summarize_tool(
                "Mystery",
                &serde_json::json!({"first":"first argument","second":"ignored"})
            ),
            "Mystery first argument"
        );
    }

    #[test]
    fn tool_rows_stay_hidden_until_expanded_and_truncate_to_width() {
        let mut chat = TranscriptChat::default();
        chat.ingest(
            &serde_json::json!({"type":"assistant","message":{"content":[
                {"type":"tool_use","name":"Bash","input":{"command":"ls -la /private/var/tmp/repro-586"}},
                {"type":"tool_use","name":"Read","input":{"file_path":"/private/var/tmp/notes.md"}},
                {"type":"tool_use","name":"Mystery","input":{"first":"first argument"}}
            ]}}),
            None,
        );
        chat.ingest(
            &serde_json::json!({"type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"Done."}]}}),
            None,
        );

        let palette = Palette::catppuccin();
        chat.prepare_layout(&palette, 32, false);
        let collapsed = chat
            .layout_lines()
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>();
        assert!(collapsed
            .iter()
            .any(|line| line == "Worked for 0s · 3 tools ›"));
        assert!(collapsed.iter().all(|line| {
            !line.starts_with("Bash") && !line.starts_with("Read") && !line.starts_with("Mystery")
        }));

        chat.prepare_layout(&palette, 32, true);
        let expanded = chat.layout_lines();
        let rows = expanded.iter().map(Line::to_string).collect::<Vec<_>>();
        assert!(rows.iter().any(|line| line == "Read notes.md"));
        assert!(rows.iter().any(|line| line == "Mystery first argument"));
        let bash_index = rows
            .iter()
            .position(|line| line.starts_with("Bash"))
            .expect("expanded Bash row");
        let bash = &expanded[bash_index];
        assert_eq!(crate::ui::text::display_width(&bash.to_string()), 32);
        assert!(bash.to_string().ends_with('…'));
        assert!(bash
            .spans
            .iter()
            .all(|span| span.style.fg == Some(palette.overlay0)));
        assert!(bash.spans.iter().all(|span| !span
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD)));
    }

    #[test]
    fn user_message_has_no_timestamp_row() {
        let mut chat = TranscriptChat::default();
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(12 * 60 * 60 + 34 * 60);
        chat.ingest(
            &serde_json::json!({"type":"user","message":{"content":"hello"}}),
            Some(at),
        );
        chat.prepare_layout(&Palette::catppuccin(), 80, false);

        let visible = chat
            .layout_lines()
            .iter()
            .filter(|line| line.width() > 0)
            .collect::<Vec<_>>();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].to_string().trim(), "hello");
    }

    #[test]
    fn assistant_prose_uses_text_colour_for_light_and_dark_palettes() {
        for palette in [
            Palette::github_light_high_contrast(),
            Palette::github_dark_high_contrast(),
        ] {
            let mut chat = TranscriptChat::default();
            chat.push_row(ChatRow::Assistant {
                body: "## Heading\nAssistant prose with `inline code`.".into(),
            });
            chat.prepare_layout(&palette, 80, false);
            let spans = chat
                .layout_lines()
                .iter()
                .flat_map(|line| line.spans.iter())
                .collect::<Vec<_>>();

            assert!(spans.iter().any(|span| {
                span.content.contains("Heading") && span.style.fg == Some(palette.text)
            }));
            assert!(spans.iter().any(|span| {
                span.content.contains("Assistant prose") && span.style.fg == Some(palette.text)
            }));
            assert!(spans.iter().any(|span| {
                span.content.contains("inline code") && span.style.fg == Some(palette.mauve)
            }));
        }
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
            worked_row_lines(duration, &tools, false, 80),
            ["Worked for 29s · 2 tools ›"]
        );
        assert_eq!(
            worked_row_lines(duration, &tools, true, 80),
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
            worked_row_lines("1s", &["Read file.rs".into()], false, 80),
            ["Worked for 1s · 1 tool ›"]
        );
    }
}
