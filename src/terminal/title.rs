const CLAUDE_ACTIVITY_GLYPHS: &str = "·✢✳✶✻✽";
/// Circle/quadrant spinner frames (U+25D0..=U+25D7, U+25DC..=U+25DF, U+25F4..=U+25F7 plus the
/// filled/hollow bullets that agents cycle with them). Left in the title they animate the sidebar
/// row on every agent frame, which is exactly the noise the braille strip already removes.
const CIRCLE_ACTIVITY_GLYPHS: &str = "◐◑◒◓◔◕◖◗◜◝◞◟◴◵◶◷●○◉◎";

pub(crate) fn stripped_terminal_title(title: &str) -> Option<String> {
    let title = title.trim();
    if title.is_empty() {
        return None;
    }

    let mut chars = title.char_indices();
    let (_, first) = chars.next()?;
    let after_first = &title[first.len_utf8()..];
    let recognized = matches!(first, '\u{2800}'..='\u{28ff}')
        || CLAUDE_ACTIVITY_GLYPHS.contains(first)
        || CIRCLE_ACTIVITY_GLYPHS.contains(first);
    let stripped = if recognized
        && (after_first.is_empty() || after_first.chars().next().is_some_and(char::is_whitespace))
    {
        after_first.trim()
    } else {
        title
    };

    (!stripped.is_empty()).then(|| stripped.to_string())
}

#[cfg(test)]
mod tests {
    use super::stripped_terminal_title;

    #[test]
    fn strips_one_recognized_leading_activity_glyph() {
        for title in ["⠋ task", "✳ task", "  ⠙   task  ", "✢ task", "✻ task"] {
            assert_eq!(stripped_terminal_title(title).as_deref(), Some("task"));
        }
        assert_eq!(
            stripped_terminal_title("⠋ ⠙ task").as_deref(),
            Some("⠙ task")
        );
    }

    #[test]
    fn strips_circle_spinner_frames_so_the_title_stops_animating() {
        for frame in [
            "◐", "◑", "◒", "◓", "◔", "◕", "◖", "◗", "◜", "◝", "◞", "◟", "◴", "◵", "◶", "◷", "●",
            "○", "◉", "◎",
        ] {
            assert_eq!(
                stripped_terminal_title(&format!("{frame} task")).as_deref(),
                Some("task"),
                "frame {frame} must not reach the sidebar title"
            );
        }
    }

    #[test]
    fn preserves_unrecognized_or_unbounded_symbols() {
        for (title, expected) in [
            ("★task", "★task"),
            ("★ production", "★ production"),
            ("✨ task", "✨ task"),
            ("☼ status", "☼ status"),
            ("@ task", "@ task"),
            ("task ⠋ detail", "task ⠋ detail"),
            ("[prod] task", "[prod] task"),
        ] {
            assert_eq!(stripped_terminal_title(title).as_deref(), Some(expected));
        }
    }

    #[test]
    fn preserves_unicode_text_and_elides_empty_results() {
        assert_eq!(
            stripped_terminal_title(" ⠋ 修复🙂标题 ").as_deref(),
            Some("修复🙂标题")
        );
        assert_eq!(stripped_terminal_title("  "), None);
        assert_eq!(stripped_terminal_title("⠋   "), None);
    }
}
