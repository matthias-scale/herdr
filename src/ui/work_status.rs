//! Status vocabulary for sidebar group headers (F12-3).
//!
//! A group header names a work item -- a Linear ticket, a pull request, a
//! Missive conversation -- and the one thing an operator scanning the sidebar
//! wants from it besides the title is where that item stands. The glyphs
//! mirror the vocabulary of the system the item comes from, so a header reads
//! the same way Linear or GitHub reads.
//!
//! Colours resolve through the theme palette, never through literals here: a
//! retinted theme retints the glyphs with it.

use ratatui::style::Color;

use crate::app::state::Palette;

/// Where a work item stands, in the vocabulary of the system it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkGroupStatus {
    /// Draft, backlog or ready: known, not queued.
    TicketBacklog,
    TicketTodo,
    TicketInProgress,
    TicketInReview,
    TicketDone,
    /// Cancelled or duplicate: closed without being done.
    TicketCanceled,
    TicketTriage,
    PullRequestOpen,
    PullRequestMerged,
    PullRequestDraft,
    PullRequestClosed,
    ConversationOpen,
    ConversationClosed,
    ConversationUnassigned,
}

impl WorkGroupStatus {
    pub(crate) fn glyph(self) -> &'static str {
        match self {
            Self::TicketBacklog | Self::PullRequestDraft | Self::ConversationUnassigned => "◌",
            Self::TicketTodo | Self::PullRequestOpen | Self::ConversationOpen => "○",
            Self::TicketInProgress => "◐",
            Self::TicketInReview => "◑",
            Self::TicketDone | Self::PullRequestMerged | Self::ConversationClosed => "●",
            Self::TicketCanceled | Self::PullRequestClosed => "⊗",
            Self::TicketTriage => "◍",
        }
    }

    /// Human-readable status, in the vocabulary of the source system. This is
    /// what the glyph means, spelled out for a hover tooltip.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::TicketBacklog => "Backlog",
            Self::TicketTodo => "Todo",
            Self::TicketInProgress => "In Progress",
            Self::TicketInReview => "In Review",
            Self::TicketDone => "Done",
            Self::TicketCanceled => "Canceled",
            Self::TicketTriage => "Triage",
            Self::PullRequestOpen => "PR open",
            Self::PullRequestMerged => "PR merged",
            Self::PullRequestDraft => "PR draft",
            Self::PullRequestClosed => "PR closed",
            Self::ConversationOpen => "Conversation open",
            Self::ConversationClosed => "Conversation closed",
            Self::ConversationUnassigned => "Conversation unassigned",
        }
    }

    pub(crate) fn color(self, palette: &Palette) -> Color {
        match self {
            Self::TicketBacklog
            | Self::TicketTodo
            | Self::TicketCanceled
            | Self::PullRequestDraft
            | Self::PullRequestClosed
            | Self::ConversationUnassigned => palette.work_status_neutral(),
            Self::TicketInProgress => palette.work_status_active(),
            Self::TicketInReview => palette.work_status_review(),
            Self::TicketDone | Self::ConversationClosed => palette.work_status_done(),
            Self::TicketTriage => palette.work_status_triage(),
            Self::PullRequestMerged => palette.work_status_merged(),
            Self::PullRequestOpen | Self::ConversationOpen => palette.work_status_open(),
        }
    }

    /// Linear ships the workflow state as free text, and teams rename their
    /// states, so this matches the state *type* vocabulary Linear itself uses
    /// rather than an exact set of names. An unknown or missing state is
    /// backlog: the ticket exists and nothing claims more than that.
    pub(crate) fn from_ticket_state(state: Option<&str>) -> Self {
        let Some(state) = state.map(str::to_lowercase) else {
            return Self::TicketBacklog;
        };
        let state = state.trim();
        if state.contains("cancel") || state.contains("duplicate") {
            Self::TicketCanceled
        } else if state.contains("triage") {
            Self::TicketTriage
        } else if state.contains("review") {
            Self::TicketInReview
        } else if state.contains("progress") || state.contains("started") {
            Self::TicketInProgress
        } else if state.contains("done") || state.contains("complete") {
            Self::TicketDone
        } else if state.contains("todo") || state.contains("to do") {
            Self::TicketTodo
        } else {
            Self::TicketBacklog
        }
    }

    /// A draft is a draft whatever GitHub says about its state. A pull request
    /// the work index has never fetched reads as open: the pane declaring it
    /// is working on it, and claiming "closed" on no evidence would be worse
    /// than claiming the common case.
    pub(crate) fn from_pull_request(state: Option<&str>, draft: bool) -> Self {
        if draft {
            return Self::PullRequestDraft;
        }
        match state.map(str::to_lowercase).as_deref() {
            Some("merged") => Self::PullRequestMerged,
            Some("closed") => Self::PullRequestClosed,
            _ => Self::PullRequestOpen,
        }
    }

    /// Missive conversations are open until closed; an open conversation
    /// nobody owns is the one that needs a human, so it reads differently
    /// from an assigned one.
    pub(crate) fn from_conversation(closed: bool, assigned: bool) -> Self {
        if closed {
            Self::ConversationClosed
        } else if assigned {
            Self::ConversationOpen
        } else {
            Self::ConversationUnassigned
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette() -> Palette {
        Palette::catppuccin()
    }

    #[test]
    fn linear_glyphs_and_colors_match_linear_states() {
        let p = palette();
        let cases = [
            (Some("Backlog"), "◌", p.work_status_neutral()),
            (Some("Ready"), "◌", p.work_status_neutral()),
            (Some("Todo"), "○", p.work_status_neutral()),
            (Some("In Progress"), "◐", p.work_status_active()),
            (Some("In Review"), "◑", p.work_status_review()),
            (Some("Done"), "●", p.work_status_done()),
            (Some("Canceled"), "⊗", p.work_status_neutral()),
            (Some("Duplicate"), "⊗", p.work_status_neutral()),
            (Some("Triage"), "◍", p.work_status_triage()),
            (None, "◌", p.work_status_neutral()),
        ];
        for (state, glyph, color) in cases {
            let status = WorkGroupStatus::from_ticket_state(state);
            assert_eq!(status.glyph(), glyph, "glyph for {state:?}");
            assert_eq!(status.color(&p), color, "color for {state:?}");
        }
    }

    #[test]
    fn pull_request_glyphs_and_colors_cover_every_state() {
        let p = palette();
        let cases = [
            (Some("open"), false, "○", p.work_status_open()),
            (Some("merged"), false, "●", p.work_status_merged()),
            (Some("open"), true, "◌", p.work_status_neutral()),
            (Some("closed"), false, "⊗", p.work_status_neutral()),
            (None, false, "○", p.work_status_open()),
        ];
        for (state, draft, glyph, color) in cases {
            let status = WorkGroupStatus::from_pull_request(state, draft);
            assert_eq!(status.glyph(), glyph, "glyph for {state:?} draft={draft}");
            assert_eq!(status.color(&p), color, "color for {state:?} draft={draft}");
        }
    }

    #[test]
    fn conversation_glyphs_and_colors_cover_every_state() {
        let p = palette();
        let cases = [
            (false, true, "○", p.work_status_open()),
            (true, true, "●", p.work_status_done()),
            (false, false, "◌", p.work_status_neutral()),
        ];
        for (closed, assigned, glyph, color) in cases {
            let status = WorkGroupStatus::from_conversation(closed, assigned);
            assert_eq!(status.glyph(), glyph, "glyph for closed={closed}");
            assert_eq!(status.color(&p), color, "color for closed={closed}");
        }
    }

    #[test]
    fn palette_override_wins_over_the_theme_tone() {
        let mut p = palette();
        p.work_status.active = Some(Color::Rgb(1, 2, 3));

        assert_eq!(
            WorkGroupStatus::TicketInProgress.color(&p),
            Color::Rgb(1, 2, 3)
        );
        assert_eq!(WorkGroupStatus::TicketInReview.color(&p), p.green);
    }
}
