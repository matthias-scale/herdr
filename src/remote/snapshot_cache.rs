//! Bounded, memory-only replay of a full ANSI frame followed by its diffs.
//! Frames move from the existing delivery allocation; no screen is serialized
//! and no frame bytes are copied on the live attach path.
use std::collections::{HashMap, VecDeque};

use crate::api::schema::AgentRef;
use crate::protocol::TerminalFrame;

const MAX_ENTRIES: usize = 32;
const MAX_ENTRY_BYTES: usize = 2 * 1024 * 1024;
const MAX_FRAMES: usize = 256;

#[derive(Debug)]
struct Entry {
    frames: VecDeque<TerminalFrame>,
    bytes: usize,
    used: u64,
}

#[derive(Debug, Default)]
pub(crate) struct SnapshotCache {
    entries: HashMap<AgentRef, Entry>,
    clock: u64,
}

impl SnapshotCache {
    /// Compact once when a proxy closes, including screens whose diff journal
    /// exceeded its bound. Never called from render or live frame delivery.
    pub(crate) fn capture(runtime: &crate::terminal::TerminalRuntime) -> Option<TerminalFrame> {
        let (rows, cols) = runtime.current_size();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(cols, rows)).ok()?;
        terminal
            .draw(|frame| runtime.render(frame, frame.area(), false))
            .ok()?;
        let data = crate::protocol::FrameData::from_ratatui_buffer_with_hyperlinks(
            terminal.backend().buffer(),
            None,
            &[],
        );
        let encoded = crate::protocol::render_ansi::BlitEncoder::new().encode(&data, true);
        Some(TerminalFrame {
            seq: 0,
            width: cols,
            height: rows,
            full: true,
            bytes: encoded.bytes,
        })
    }

    pub(crate) fn insert(&mut self, agent: &AgentRef, frame: TerminalFrame) {
        self.clock = self.clock.saturating_add(1);
        if frame.bytes.len() > MAX_ENTRY_BYTES {
            self.entries.remove(agent);
            return;
        }
        if frame.full && !self.entries.contains_key(agent) {
            if self.entries.len() == MAX_ENTRIES {
                let oldest = self
                    .entries
                    .iter()
                    .min_by_key(|(_, entry)| entry.used)
                    .map(|(key, _)| key.clone());
                if let Some(oldest) = oldest {
                    self.entries.remove(&oldest);
                }
            }
            self.entries.insert(
                agent.clone(),
                Entry {
                    frames: VecDeque::with_capacity(MAX_FRAMES),
                    bytes: 0,
                    used: self.clock,
                },
            );
        }
        let Some(entry) = self.entries.get_mut(agent) else {
            return;
        };
        if frame.full {
            entry.frames.clear();
            entry.bytes = 0;
        } else if entry.frames.back().is_none_or(|previous| {
            previous.seq.checked_add(1) != Some(frame.seq)
                || previous.width != frame.width
                || previous.height != frame.height
        }) {
            self.entries.remove(agent);
            return;
        }
        if entry.bytes + frame.bytes.len() > MAX_ENTRY_BYTES || entry.frames.len() == MAX_FRAMES {
            // A truncated diff chain cannot reconstruct a screen. Prefer a miss.
            self.entries.remove(agent);
            return;
        }
        entry.used = self.clock;
        entry.bytes += frame.bytes.len();
        entry.frames.push_back(frame);
    }

    pub(crate) fn replay(
        &mut self,
        agent: &AgentRef,
        mut paint: impl FnMut(&TerminalFrame),
    ) -> bool {
        let Some(entry) = self.entries.get_mut(agent) else {
            return false;
        };
        self.clock = self.clock.saturating_add(1);
        entry.used = self.clock;
        for frame in &entry.frames {
            paint(frame);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn agent(n: usize) -> AgentRef {
        AgentRef::new("host", format!("w1:p{n}")).expect("ref")
    }
    fn frame(seq: u64, full: bool, text: &str) -> TerminalFrame {
        TerminalFrame {
            seq,
            width: 80,
            height: 24,
            full,
            bytes: text.as_bytes().to_vec(),
        }
    }
    #[test]
    fn first_rendered_frame_equals_cached_screen_before_live_arrives() {
        use crate::terminal::TerminalRuntime;
        use std::sync::Arc;
        fn proxy() -> (TerminalRuntime, crate::pane::RemoteProxyChannels) {
            TerminalRuntime::spawn_remote_proxy(
                crate::layout::PaneId::alloc(),
                24,
                80,
                100_000,
                Arc::new(tokio::sync::Notify::new()),
                Arc::new(crate::render_signal::RenderSignal::new()),
                crate::remote::RemoteFocusOperationState::new(),
            )
            .expect("proxy")
        }
        fn rendered(runtime: &TerminalRuntime) -> ratatui::buffer::Buffer {
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24))
                .expect("terminal");
            terminal
                .draw(|frame| runtime.render(frame, frame.area(), false))
                .expect("draw");
            terminal.backend().buffer().clone()
        }
        let mut cache = SnapshotCache::default();
        let (original, _original_channels) = proxy();
        for frame in [
            frame(1, true, "\x1b[2J\x1b[H\x1b[31mcached frame"),
            frame(2, false, "\x1b[2;1Hsecond line"),
        ] {
            original.process_remote_frame(&frame.bytes);
            cache.insert(&agent(1), frame);
        }
        let expected = rendered(&original);
        let (reopened, _reopened_channels) = proxy();
        assert!(cache.replay(&agent(1), |frame| {
            reopened.process_remote_frame(&frame.bytes);
        }));
        assert_eq!(rendered(&reopened), expected);
        assert_eq!(reopened.remote_proxy_input_enabled(), Some(false));
        reopened.process_remote_frame(b"\x1bc\x1b[Hlive");
        assert_ne!(rendered(&reopened), expected);
        assert!(!reopened.visible_text().contains("cached"));
        original.shutdown();
        reopened.shutdown();
    }

    #[test]
    fn cached_frame_is_first_then_live_full_replaces_it() {
        let mut cache = SnapshotCache::default();
        cache.insert(&agent(1), frame(1, true, "cached"));
        cache.insert(&agent(1), frame(2, false, " diff"));
        let mut painted = Vec::new();
        assert!(cache.replay(&agent(1), |frame| painted.extend_from_slice(&frame.bytes)));
        assert_eq!(painted, b"cached diff");
        cache.insert(&agent(1), frame(1, true, "live"));
        painted.clear();
        cache.replay(&agent(1), |frame| painted.extend_from_slice(&frame.bytes));
        assert_eq!(painted, b"live");
    }
    #[test]
    fn bounded_lru_and_broken_diff_chains() {
        let mut cache = SnapshotCache::default();
        for n in 0..MAX_ENTRIES {
            cache.insert(&agent(n), frame(1, true, "screen"));
        }
        cache.replay(&agent(0), |_| {});
        cache.insert(&agent(MAX_ENTRIES), frame(1, true, "new"));
        assert!(!cache.replay(&agent(1), |_| {}));
        assert!(cache.replay(&agent(0), |_| {}));
        cache.insert(&agent(0), frame(3, false, "gap"));
        assert!(!cache.replay(&agent(0), |_| {}));
        cache.insert(&agent(0), frame(1, true, "screen"));
        for seq in 2..=MAX_FRAMES as u64 + 1 {
            cache.insert(&agent(0), frame(seq, false, "diff"));
        }
        assert!(!cache.replay(&agent(0), |_| {}));
        assert!(cache.entries.len() <= MAX_ENTRIES);
    }
}
