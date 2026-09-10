//! The sidebar's idle animation: a star field the operator can stop.
//!
//! [`HyperspaceState`] owns nothing but the frame counter and the operator's
//! pause decision, which keeps it testable without a terminal. The drawing
//! lives in [`crate::ui::hyperspace`]; the clock lives in the event loop, which
//! calls [`HyperspaceState::tick`] and asks [`HyperspaceState::next_deadline`]
//! when to come back.
//!
//! An always-on animation costs a redraw per frame, so every path here is
//! written to stop cheaply: paused or disabled means no deadline, which means
//! the loop is never woken on the animation's behalf.

use std::time::{Duration, Instant};

/// Wall-clock gap between frames. Roughly eight frames a second: enough for the
/// star streaks to read as motion, slow enough that the redraws it forces stay
/// well under the loop's own 16 ms floor.
pub const FRAME_INTERVAL: Duration = Duration::from_millis(120);

/// Frames advanced at most in one tick. A suspended laptop can come back with
/// hours on the clock; replaying that many frames would be pure waste when only
/// the final one is ever drawn.
const MAX_CATCHUP_FRAMES: u32 = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HyperspaceState {
    /// Whether the panel exists at all. Owned by `[ui] sidebar_animation`.
    pub enabled: bool,
    paused: bool,
    step: u32,
    next_frame_at: Instant,
}

impl Default for HyperspaceState {
    fn default() -> Self {
        Self::new(true, Instant::now())
    }
}

impl HyperspaceState {
    pub fn new(enabled: bool, now: Instant) -> Self {
        Self {
            enabled,
            paused: false,
            step: 0,
            next_frame_at: now + FRAME_INTERVAL,
        }
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    /// Whether the animation should be advancing. Disabled and paused are
    /// separate facts: disabled hides the panel, paused freezes a visible one.
    pub fn running(&self) -> bool {
        self.enabled && !self.paused
    }

    /// The frame to draw. Callers take it modulo the field's loop length.
    pub fn step(&self) -> u32 {
        self.step
    }

    pub fn set_paused(&mut self, paused: bool, now: Instant) {
        if self.paused == paused {
            return;
        }
        self.paused = paused;
        // Resuming starts a fresh interval rather than firing immediately for
        // however long the pause lasted.
        self.next_frame_at = now + FRAME_INTERVAL;
    }

    /// Flips the pause and reports that the frame has to be redrawn, since the
    /// button's own glyph just changed.
    pub fn toggle_paused(&mut self, now: Instant) -> bool {
        self.set_paused(!self.paused, now);
        true
    }

    pub fn set_enabled(&mut self, enabled: bool, now: Instant) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        self.next_frame_at = now + FRAME_INTERVAL;
    }

    /// Advances the field. Returns whether anything the operator can see moved,
    /// which is what the loop turns into a redraw.
    pub fn tick(&mut self, now: Instant) -> bool {
        if !self.running() || now < self.next_frame_at {
            return false;
        }
        let behind = now.saturating_duration_since(self.next_frame_at);
        let skipped = (behind.as_millis() / FRAME_INTERVAL.as_millis()) as u32;
        self.step = self
            .step
            .wrapping_add(1)
            .wrapping_add(skipped.min(MAX_CATCHUP_FRAMES));
        self.next_frame_at = now + FRAME_INTERVAL;
        true
    }

    /// When the loop should wake for the next frame, or `None` when the
    /// animation is not asking to be woken at all.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.running().then_some(self.next_frame_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(now: Instant) -> HyperspaceState {
        HyperspaceState::new(true, now)
    }

    #[test]
    fn a_tick_before_the_interval_changes_nothing() {
        let now = Instant::now();
        let mut s = state(now);
        assert!(!s.tick(now + FRAME_INTERVAL / 2));
        assert_eq!(s.step(), 0);
    }

    #[test]
    fn a_tick_on_the_interval_advances_one_frame() {
        let now = Instant::now();
        let mut s = state(now);
        assert!(s.tick(now + FRAME_INTERVAL));
        assert_eq!(s.step(), 1);
    }

    #[test]
    fn a_long_stall_does_not_replay_every_missed_frame() {
        // The machine slept for an hour. Only the frame about to be drawn
        // matters, so the counter must not walk through the backlog.
        let now = Instant::now();
        let mut s = state(now);
        assert!(s.tick(now + Duration::from_secs(3600)));
        assert!(s.step() <= 1 + MAX_CATCHUP_FRAMES, "step {}", s.step());
    }

    #[test]
    fn a_paused_animation_neither_advances_nor_asks_to_be_woken() {
        let now = Instant::now();
        let mut s = state(now);
        s.set_paused(true, now);
        assert!(!s.tick(now + FRAME_INTERVAL * 10));
        assert_eq!(s.step(), 0);
        assert_eq!(s.next_deadline(), None);
    }

    #[test]
    fn a_disabled_animation_never_asks_to_be_woken() {
        let now = Instant::now();
        let mut s = HyperspaceState::new(false, now);
        assert_eq!(s.next_deadline(), None);
        assert!(!s.tick(now + FRAME_INTERVAL * 10));
    }

    #[test]
    fn resuming_keeps_the_frame_it_was_frozen_on() {
        let now = Instant::now();
        let mut s = state(now);
        s.tick(now + FRAME_INTERVAL);
        s.tick(now + FRAME_INTERVAL * 2);
        let frozen = s.step();
        s.set_paused(true, now + FRAME_INTERVAL * 2);
        s.set_paused(false, now + FRAME_INTERVAL * 9);
        assert_eq!(s.step(), frozen, "pausing is not a reset");
        // ...and it does not fire for the whole time it spent paused.
        assert!(!s.tick(now + FRAME_INTERVAL * 9));
        assert!(s.tick(now + FRAME_INTERVAL * 10));
    }

    #[test]
    fn toggling_always_redraws_because_the_button_glyph_changed() {
        let now = Instant::now();
        let mut s = state(now);
        assert!(s.toggle_paused(now));
        assert!(s.paused());
        assert!(s.toggle_paused(now));
        assert!(!s.paused());
    }

    #[test]
    fn the_counter_wraps_instead_of_overflowing() {
        let now = Instant::now();
        let mut s = state(now);
        s.step = u32::MAX;
        assert!(s.tick(now + FRAME_INTERVAL));
        assert_eq!(s.step(), 0);
    }
}
