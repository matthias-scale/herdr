use std::io::{self, Write as _};

use crate::config::TerminalNotificationBackend;

pub fn detect_backend() -> Option<TerminalNotificationBackend> {
    let term_program = std::env::var("TERM_PROGRAM").ok();
    let lc_terminal = std::env::var("LC_TERMINAL").ok();
    let term = std::env::var("TERM").ok();
    detect_backend_from(
        term_program.as_deref(),
        lc_terminal.as_deref(),
        std::env::var_os("KITTY_WINDOW_ID").is_some(),
        term.as_deref(),
    )
}

fn detect_backend_from(
    term_program: Option<&str>,
    lc_terminal: Option<&str>,
    kitty_window_id: bool,
    term: Option<&str>,
) -> Option<TerminalNotificationBackend> {
    if matches!(term_program, Some("ghostty" | "iTerm.app" | "WezTerm")) {
        return Some(TerminalNotificationBackend::Osc9);
    }
    if matches!(lc_terminal, Some("WezTerm" | "iTerm2")) {
        return Some(TerminalNotificationBackend::Osc9);
    }
    if kitty_window_id {
        return Some(TerminalNotificationBackend::Osc99);
    }
    match term {
        Some("xterm-ghostty") => Some(TerminalNotificationBackend::Osc9),
        Some("xterm-kitty") => Some(TerminalNotificationBackend::Osc99),
        Some(term) if term.contains("wezterm") => Some(TerminalNotificationBackend::Osc9),
        _ => None,
    }
}

pub fn resolve_backend(
    configured: TerminalNotificationBackend,
) -> Option<TerminalNotificationBackend> {
    match configured {
        TerminalNotificationBackend::Auto => detect_backend(),
        explicit => Some(explicit),
    }
}

pub fn show_notification(
    title: &str,
    body: Option<&str>,
    configured: TerminalNotificationBackend,
) -> io::Result<bool> {
    let Some(backend) = resolve_backend(configured) else {
        return Ok(false);
    };

    let sequence = match backend {
        TerminalNotificationBackend::Osc9 => build_osc9_notification(title, body),
        TerminalNotificationBackend::Osc99 => build_osc99_notification(title, body),
        TerminalNotificationBackend::Auto => unreachable!("auto backend must be resolved"),
    };

    let sequence = if std::env::var_os("TMUX").is_some() {
        wrap_tmux_passthrough(&sequence)
    } else {
        sequence
    };

    let mut stdout = io::stdout();
    stdout.write_all(&sequence)?;
    stdout.flush()?;
    Ok(true)
}

pub fn split_message(message: &str) -> (&str, Option<&str>) {
    match message.split_once(": ") {
        Some((title, body)) if !title.is_empty() && !body.is_empty() => (title, Some(body)),
        _ => (message, None),
    }
}

fn build_osc9_notification(title: &str, body: Option<&str>) -> Vec<u8> {
    let message = sanitize_text(match body {
        Some(body) if !body.is_empty() => format!("{title}: {body}"),
        _ => title.to_string(),
    });
    format!("\x1b]9;{message}\x1b\\").into_bytes()
}

fn build_osc99_notification(title: &str, body: Option<&str>) -> Vec<u8> {
    let title = sanitize_text(title);
    match body {
        Some(body) if !body.is_empty() => {
            let body = sanitize_text(body);
            format!("\x1b]99;i=1:d=0;{title}\x1b\\\x1b]99;i=1:p=body;{body}\x1b\\").into_bytes()
        }
        _ => format!("\x1b]99;;{title}\x1b\\").into_bytes(),
    }
}

fn sanitize_text(text: impl AsRef<str>) -> String {
    text.as_ref()
        .chars()
        .filter(|ch| *ch != '\u{1b}' && *ch != '\u{7}' && *ch != '\u{9c}')
        .map(|ch| match ch {
            '\n' | '\r' | '\t' => ' ',
            _ => ch,
        })
        .collect()
}

fn wrap_tmux_passthrough(sequence: &[u8]) -> Vec<u8> {
    let mut wrapped = Vec::with_capacity(sequence.len() + 16);
    wrapped.extend_from_slice(b"\x1bPtmux;");
    for &byte in sequence {
        if byte == 0x1b {
            wrapped.push(0x1b);
        }
        wrapped.push(byte);
    }
    wrapped.extend_from_slice(b"\x1b\\");
    wrapped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_message_splits_title_and_body() {
        assert_eq!(
            split_message("agent done: ws · 1"),
            ("agent done", Some("ws · 1"))
        );
    }

    #[test]
    fn split_message_leaves_plain_message_alone() {
        assert_eq!(split_message("agent done"), ("agent done", None));
    }

    #[test]
    fn sanitize_text_strips_control_bytes() {
        assert_eq!(sanitize_text("a\n\tb\u{1b}c\u{7}"), "a  bc");
    }

    #[test]
    fn kitty_notification_uses_structured_title_and_body() {
        let sequence = String::from_utf8(build_osc99_notification("pi finished", Some("ws · 1")))
            .expect("utf8");
        assert!(sequence.contains("]99;i=1:d=0;pi finished"));
        assert!(sequence.contains("]99;i=1:p=body;ws · 1"));
    }

    #[test]
    fn tmux_passthrough_wraps_and_escapes() {
        let wrapped = wrap_tmux_passthrough(b"\x1b]9;hi\x1b\\");
        assert_eq!(wrapped, b"\x1bPtmux;\x1b\x1b]9;hi\x1b\x1b\\\x1b\\");
    }

    #[test]
    fn explicit_backend_bypasses_auto_detection() {
        assert_eq!(
            resolve_backend(TerminalNotificationBackend::Osc99),
            Some(TerminalNotificationBackend::Osc99)
        );
    }

    #[test]
    fn auto_detects_lc_terminal_forwarded_over_ssh() {
        assert_eq!(
            detect_backend_from(None, Some("WezTerm"), false, Some("xterm-256color")),
            Some(TerminalNotificationBackend::Osc9)
        );
        assert_eq!(
            detect_backend_from(None, Some("iTerm2"), false, Some("xterm-256color")),
            Some(TerminalNotificationBackend::Osc9)
        );
    }

    #[test]
    fn auto_keeps_existing_terminal_detection_paths() {
        for term_program in ["ghostty", "iTerm.app", "WezTerm"] {
            assert_eq!(
                detect_backend_from(Some(term_program), None, false, None),
                Some(TerminalNotificationBackend::Osc9)
            );
        }
        assert_eq!(
            detect_backend_from(None, None, true, None),
            Some(TerminalNotificationBackend::Osc99)
        );
        assert_eq!(
            detect_backend_from(None, None, false, Some("xterm-ghostty")),
            Some(TerminalNotificationBackend::Osc9)
        );
        assert_eq!(
            detect_backend_from(None, None, false, Some("xterm-kitty")),
            Some(TerminalNotificationBackend::Osc99)
        );
        assert_eq!(
            detect_backend_from(None, None, false, Some("wezterm")),
            Some(TerminalNotificationBackend::Osc9)
        );
        assert_eq!(
            detect_backend_from(None, None, false, Some("xterm-256color")),
            None
        );
    }
}
