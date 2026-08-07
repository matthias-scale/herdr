use std::sync::atomic::{AtomicU64, Ordering};

use crate::detect::{Agent, AgentDetection, AgentState};

pub(super) const AGENT_PENDING_IDLE_RECHECK: std::time::Duration =
    std::time::Duration::from_millis(100);
const AGENT_PENDING_IDLE_CONFIRMATIONS: u8 = 3;
pub(super) const AGENT_PENDING_IDLE_CAP: std::time::Duration =
    std::time::Duration::from_millis(700);
/// This only has to cover a stall *inside* a turn: a codex pane waiting on a
/// tool keeps its progress line on screen for the whole wait and is held
/// Working by the rule that reads it, not by this window. The gap this covers
/// is the silent one after codex erases that line and before the first token
/// arrives -- measured at 3.6s on a live 0.147 pane, which a two-second window
/// let show as `done` mid-turn. Eight seconds gives that measurement room to
/// double and costs nothing at the real end of a turn, because the turn-end
/// hook reports idle directly and outranks this.
pub(super) const AGENT_RECENT_OUTPUT_WINDOW: std::time::Duration =
    std::time::Duration::from_secs(8);
pub(crate) const STABLE_VISIBLE_SIGNAL_REFRESH: std::time::Duration =
    std::time::Duration::from_millis(800);
pub(super) const AGENT_STARTUP_GRACE_WINDOW: std::time::Duration =
    std::time::Duration::from_secs(3);
pub(super) const FULL_LIFECYCLE_HOOK_OUTPUT_RETIREMENT_GRACE: std::time::Duration =
    std::time::Duration::from_secs(5);

#[derive(Debug, Default)]
pub(super) struct FullLifecycleHookOutputRetirement {
    authority_baseline_content_seq: Option<u64>,
    last_content_seq: Option<u64>,
    output_started_at: Option<std::time::Instant>,
}

impl FullLifecycleHookOutputRetirement {
    pub(super) fn clear(&mut self) {
        self.authority_baseline_content_seq = None;
        self.last_content_seq = None;
        self.output_started_at = None;
    }

    pub(super) fn observe(
        &mut self,
        content_seq: u64,
        authority_baseline_content_seq: u64,
        now: std::time::Instant,
    ) -> Option<std::time::Instant> {
        if self.authority_baseline_content_seq != Some(authority_baseline_content_seq) {
            self.authority_baseline_content_seq = Some(authority_baseline_content_seq);
            self.last_content_seq = Some(authority_baseline_content_seq);
            self.output_started_at = None;
        }

        if self.last_content_seq != Some(content_seq) {
            self.last_content_seq = Some(content_seq);
            self.output_started_at.get_or_insert(now);
        }

        self.output_started_at.filter(|started_at| {
            now.duration_since(*started_at) >= FULL_LIFECYCLE_HOOK_OUTPUT_RETIREMENT_GRACE
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DetectionPublishState {
    pub(super) state: AgentState,
    pub(super) visible_idle: bool,
    pub(super) visible_blocker: bool,
    pub(super) visible_working: bool,
}

#[derive(Debug, Default)]
pub(super) struct PendingIdleConfirmation {
    started_at: Option<std::time::Instant>,
    confirmations: u8,
}

impl PendingIdleConfirmation {
    pub(super) fn active(&self) -> bool {
        self.started_at.is_some()
    }

    pub(super) fn clear(&mut self) {
        self.started_at = None;
        self.confirmations = 0;
    }

    pub(super) fn should_hold_working_to_idle(
        &mut self,
        previous: DetectionPublishState,
        next: DetectionPublishState,
        agent_changed: bool,
        process_exited: bool,
        now: std::time::Instant,
    ) -> bool {
        let is_working_to_plain_idle = previous.state == AgentState::Working
            && next.state == AgentState::Idle
            && !next.visible_idle
            && !next.visible_blocker
            && !agent_changed
            && !process_exited;

        if !is_working_to_plain_idle {
            self.clear();
            return false;
        }

        let Some(started_at) = self.started_at else {
            self.started_at = Some(now);
            self.confirmations = 0;
            return true;
        };

        if now.duration_since(started_at) >= AGENT_PENDING_IDLE_CAP {
            self.clear();
            return false;
        }

        self.confirmations = self.confirmations.saturating_add(1);
        if self.confirmations >= AGENT_PENDING_IDLE_CONFIRMATIONS {
            self.clear();
            return false;
        }

        true
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct IdleScreenScanSkipInput {
    pub(super) state: AgentState,
    pub(super) agent: Option<Agent>,
    pub(super) pending_idle_active: bool,
    pub(super) agent_changed: bool,
    pub(super) process_exited: bool,
    pub(super) current_detection_content_seq: Option<u64>,
    pub(super) last_screen_scan_detection_content_seq: Option<u64>,
}

pub(super) fn should_skip_idle_screen_scan(input: IdleScreenScanSkipInput) -> bool {
    if input.state != AgentState::Idle
        || input.agent.is_none()
        || input.pending_idle_active
        || input.agent_changed
        || input.process_exited
    {
        return false;
    }

    input.current_detection_content_seq.is_some()
        && input.last_screen_scan_detection_content_seq == input.current_detection_content_seq
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DetectionScreenReadDecision {
    Read,
    Skip,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct DetectionScreenReadInput {
    pub(super) state: AgentState,
    pub(super) agent: Option<Agent>,
    pub(super) pending_idle_active: bool,
    pub(super) agent_changed: bool,
    pub(super) process_exited: bool,
    pub(super) current_detection_content_seq: Option<u64>,
    pub(super) last_screen_scan_detection_content_seq: Option<u64>,
}

pub(super) fn decide_detection_screen_read(
    input: DetectionScreenReadInput,
) -> DetectionScreenReadDecision {
    if should_skip_idle_screen_scan(IdleScreenScanSkipInput {
        state: input.state,
        agent: input.agent,
        pending_idle_active: input.pending_idle_active,
        agent_changed: input.agent_changed,
        process_exited: input.process_exited,
        current_detection_content_seq: input.current_detection_content_seq,
        last_screen_scan_detection_content_seq: input.last_screen_scan_detection_content_seq,
    }) {
        DetectionScreenReadDecision::Skip
    } else {
        DetectionScreenReadDecision::Read
    }
}

pub(super) fn should_publish_detection_update(
    previous: DetectionPublishState,
    next: DetectionPublishState,
    agent_changed: bool,
    process_exited: bool,
    stable_visible_signal_refresh_due: bool,
) -> bool {
    next.state != previous.state
        || next.visible_idle != previous.visible_idle
        || next.visible_blocker != previous.visible_blocker
        || next.visible_working != previous.visible_working
        || agent_changed
        || process_exited
        || (stable_visible_signal_refresh_due && next.visible_blocker && previous.visible_blocker)
}

pub(super) fn stable_visible_signal_refresh_due(
    previous: DetectionPublishState,
    next: DetectionPublishState,
    last_refresh: Option<std::time::Instant>,
    now: std::time::Instant,
) -> bool {
    let stable_visible_signal = next.visible_blocker && previous.visible_blocker;

    stable_visible_signal
        && last_refresh.is_none_or(|last_refresh| {
            now.duration_since(last_refresh) >= STABLE_VISIBLE_SIGNAL_REFRESH
        })
}

pub(super) fn recent_agent_output(
    agent: Option<Agent>,
    output_seq: u64,
    last_output_seq: &mut Option<u64>,
    last_output_at: &mut Option<std::time::Instant>,
    now: std::time::Instant,
) -> bool {
    if *last_output_seq != Some(output_seq) {
        *last_output_seq = Some(output_seq);
        if agent.is_some() {
            *last_output_at = Some(now);
        }
    }

    if agent.is_none() {
        *last_output_at = None;
        return false;
    }

    last_output_at
        .is_some_and(|last_output| now.duration_since(last_output) <= AGENT_RECENT_OUTPUT_WINDOW)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DetectionTransitionDecision {
    NoPublish,
    PublishNext,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct DetectionTransitionInput {
    pub(super) previous_publish: DetectionPublishState,
    pub(super) next_publish: DetectionPublishState,
    pub(super) agent_changed: bool,
    pub(super) process_exited: bool,
    pub(super) stable_refresh_due: bool,
    pub(super) now: std::time::Instant,
}

pub(super) fn decide_detection_transition(
    input: DetectionTransitionInput,
    pending_idle: &mut PendingIdleConfirmation,
) -> DetectionTransitionDecision {
    if pending_idle.should_hold_working_to_idle(
        input.previous_publish,
        input.next_publish,
        input.agent_changed,
        input.process_exited,
        input.now,
    ) {
        return DetectionTransitionDecision::NoPublish;
    }

    if should_publish_detection_update(
        input.previous_publish,
        input.next_publish,
        input.agent_changed,
        input.process_exited,
        input.stable_refresh_due,
    ) {
        return DetectionTransitionDecision::PublishNext;
    }

    DetectionTransitionDecision::NoPublish
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DetectionPublishDecision {
    NoPublish,
    Publish {
        state: AgentState,
        visible_idle: bool,
        visible_blocker: bool,
        visible_working: bool,
        process_exited: bool,
    },
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ScreenDetectionPublishInput {
    pub(super) current_state: AgentState,
    pub(super) last_visible_idle: bool,
    pub(super) last_visible_blocker: bool,
    pub(super) last_visible_working: bool,
    pub(super) last_visible_signal_refresh: Option<std::time::Instant>,
    pub(super) screen_detection: AgentDetection,
    pub(super) recent_output: bool,
    pub(super) process_exited: bool,
    pub(super) agent_changed: bool,
    pub(super) now: std::time::Instant,
}

pub(super) fn decide_screen_detection_publish(
    input: ScreenDetectionPublishInput,
    pending_idle: &mut PendingIdleConfirmation,
) -> DetectionPublishDecision {
    let detection = input.screen_detection;
    let detected_state = crate::terminal::state::stabilize_agent_detection(detection);
    let new_state = if input.recent_output
        && detected_state == AgentState::Idle
        && !detection.visible_idle
        && !detection.visible_blocker
    {
        AgentState::Working
    } else {
        detected_state
    };
    let visible_idle = detection.visible_idle && new_state == AgentState::Idle;
    let visible_blocker = detection.visible_blocker && new_state == AgentState::Blocked;
    let visible_working = detection.visible_working && new_state == AgentState::Working;

    let previous_publish = DetectionPublishState {
        state: input.current_state,
        visible_idle: input.last_visible_idle,
        visible_blocker: input.last_visible_blocker,
        visible_working: input.last_visible_working,
    };
    let next_publish = DetectionPublishState {
        state: new_state,
        visible_idle,
        visible_blocker,
        visible_working,
    };
    let stable_refresh_due = stable_visible_signal_refresh_due(
        previous_publish,
        next_publish,
        input.last_visible_signal_refresh,
        input.now,
    );

    match decide_detection_transition(
        DetectionTransitionInput {
            previous_publish,
            next_publish,
            agent_changed: input.agent_changed,
            process_exited: input.process_exited,
            stable_refresh_due,
            now: input.now,
        },
        pending_idle,
    ) {
        DetectionTransitionDecision::NoPublish => DetectionPublishDecision::NoPublish,
        DetectionTransitionDecision::PublishNext => DetectionPublishDecision::Publish {
            state: new_state,
            visible_idle,
            visible_blocker,
            visible_working,
            process_exited: input.process_exited,
        },
    }
}

#[allow(dead_code)] // shim for tests; detection_update_for_publish_with_osc is the real path
pub(super) fn detection_update_for_publish(
    agent: Option<Agent>,
    content: &str,
    process_exited: bool,
) -> Option<crate::detect::AgentDetection> {
    detection_update_for_publish_with_osc(agent, content, "", "", process_exited)
}

pub(super) fn detection_update_for_publish_with_osc(
    agent: Option<Agent>,
    content: &str,
    osc_title: &str,
    osc_progress: &str,
    process_exited: bool,
) -> Option<crate::detect::AgentDetection> {
    if process_exited {
        return Some(crate::detect::AgentDetection {
            state: AgentState::Idle,
            skip_state_update: false,
            visible_idle: true,
            visible_blocker: false,
            visible_working: false,
        });
    }

    let detection = crate::detect::detect_agent_with_osc(agent, content, osc_title, osc_progress);
    (!detection.skip_state_update).then_some(detection)
}

pub(super) fn observe_detection_content_change(bytes: &[u8], detection_content_seq: &AtomicU64) {
    if !bytes.is_empty() {
        detection_content_seq.fetch_add(1, Ordering::Relaxed);
    }
}

pub(super) fn observe_agent_output(bytes: &[u8], output_seq: &AtomicU64) {
    if !bytes.is_empty() {
        output_seq.fetch_add(1, Ordering::Relaxed);
    }
}

pub(super) fn mark_detection_content_changed(detection_content_seq: &AtomicU64) {
    detection_content_seq.fetch_add(1, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn publish_state(state: AgentState) -> DetectionPublishState {
        DetectionPublishState {
            state,
            visible_idle: false,
            visible_blocker: false,
            visible_working: false,
        }
    }

    fn screen_detection(state: AgentState) -> AgentDetection {
        AgentDetection {
            state,
            skip_state_update: false,
            visible_idle: state == AgentState::Idle,
            visible_blocker: false,
            visible_working: state == AgentState::Working,
        }
    }

    fn screen_publish_input(
        current_state: AgentState,
        screen_detection: AgentDetection,
        now: std::time::Instant,
    ) -> ScreenDetectionPublishInput {
        ScreenDetectionPublishInput {
            current_state,
            last_visible_idle: false,
            last_visible_blocker: false,
            last_visible_working: false,
            last_visible_signal_refresh: None,
            screen_detection,
            recent_output: false,
            process_exited: false,
            agent_changed: false,
            now,
        }
    }

    fn screen_read_input(state: AgentState, current_seq: u64) -> DetectionScreenReadInput {
        DetectionScreenReadInput {
            state,
            agent: Some(Agent::Codex),
            pending_idle_active: false,
            agent_changed: false,
            process_exited: false,
            current_detection_content_seq: Some(current_seq),
            last_screen_scan_detection_content_seq: Some(10),
        }
    }

    #[test]
    fn screen_read_skips_unchanged_idle_bottom_buffer() {
        assert_eq!(
            decide_detection_screen_read(screen_read_input(AgentState::Idle, 10)),
            DetectionScreenReadDecision::Skip
        );
    }

    #[test]
    fn screen_read_reads_when_idle_bottom_buffer_changes() {
        assert_eq!(
            decide_detection_screen_read(screen_read_input(AgentState::Idle, 11)),
            DetectionScreenReadDecision::Read
        );
    }

    #[test]
    fn screen_read_reads_for_transitions_and_missing_agent() {
        let mut input = screen_read_input(AgentState::Idle, 10);
        input.pending_idle_active = true;
        assert_eq!(
            decide_detection_screen_read(input),
            DetectionScreenReadDecision::Read
        );

        let mut input = screen_read_input(AgentState::Idle, 10);
        input.agent_changed = true;
        assert_eq!(
            decide_detection_screen_read(input),
            DetectionScreenReadDecision::Read
        );

        let mut input = screen_read_input(AgentState::Idle, 10);
        input.process_exited = true;
        assert_eq!(
            decide_detection_screen_read(input),
            DetectionScreenReadDecision::Read
        );

        let mut input = screen_read_input(AgentState::Idle, 10);
        input.agent = None;
        assert_eq!(
            decide_detection_screen_read(input),
            DetectionScreenReadDecision::Read
        );
    }

    #[test]
    fn pending_idle_holds_working_to_plain_idle_until_confirmed() {
        let now = std::time::Instant::now();
        let previous = publish_state(AgentState::Working);
        let next = publish_state(AgentState::Idle);
        let mut pending = PendingIdleConfirmation::default();

        assert!(pending.should_hold_working_to_idle(previous, next, false, false, now));
        assert!(pending.should_hold_working_to_idle(
            previous,
            next,
            false,
            false,
            now + AGENT_PENDING_IDLE_RECHECK
        ));
        assert!(pending.should_hold_working_to_idle(
            previous,
            next,
            false,
            false,
            now + AGENT_PENDING_IDLE_RECHECK * 2
        ));
        assert!(!pending.should_hold_working_to_idle(
            previous,
            next,
            false,
            false,
            now + AGENT_PENDING_IDLE_RECHECK * 3
        ));
    }

    #[test]
    fn visible_idle_bypasses_plain_idle_hold() {
        let now = std::time::Instant::now();
        let previous = publish_state(AgentState::Working);
        let mut next = publish_state(AgentState::Idle);
        next.visible_idle = true;
        let mut pending = PendingIdleConfirmation::default();

        assert!(!pending.should_hold_working_to_idle(previous, next, false, false, now));
    }

    #[test]
    fn transition_decision_publishes_next_for_visible_blocker() {
        let now = std::time::Instant::now();
        let mut pending_idle = PendingIdleConfirmation::default();
        let mut blocked = publish_state(AgentState::Blocked);
        blocked.visible_blocker = true;

        assert_eq!(
            decide_detection_transition(
                DetectionTransitionInput {
                    previous_publish: publish_state(AgentState::Idle),
                    next_publish: blocked,
                    agent_changed: false,
                    process_exited: false,
                    stable_refresh_due: false,
                    now,
                },
                &mut pending_idle,
            ),
            DetectionTransitionDecision::PublishNext
        );
    }

    #[test]
    fn screen_publish_keeps_visible_working_without_pty_activity() {
        let now = std::time::Instant::now();
        let mut pending_idle = PendingIdleConfirmation::default();

        assert_eq!(
            decide_screen_detection_publish(
                screen_publish_input(AgentState::Idle, screen_detection(AgentState::Working), now,),
                &mut pending_idle,
            ),
            DetectionPublishDecision::Publish {
                state: AgentState::Working,
                visible_idle: false,
                visible_blocker: false,
                visible_working: true,
                process_exited: false,
            }
        );
    }

    #[test]
    fn screen_publish_can_publish_idle_without_input_taint_delay() {
        let now = std::time::Instant::now();
        let mut pending_idle = PendingIdleConfirmation::default();

        assert_eq!(
            decide_screen_detection_publish(
                screen_publish_input(AgentState::Blocked, screen_detection(AgentState::Idle), now,),
                &mut pending_idle,
            ),
            DetectionPublishDecision::Publish {
                state: AgentState::Idle,
                visible_idle: true,
                visible_blocker: false,
                visible_working: false,
                process_exited: false,
            }
        );
    }

    #[test]
    fn recent_agent_output_promotes_plain_idle_detection_to_working() {
        let now = std::time::Instant::now();
        let mut input = screen_publish_input(
            AgentState::Idle,
            AgentDetection {
                state: AgentState::Idle,
                skip_state_update: false,
                visible_idle: false,
                visible_blocker: false,
                visible_working: false,
            },
            now,
        );
        input.recent_output = true;
        let mut pending_idle = PendingIdleConfirmation::default();

        assert_eq!(
            decide_screen_detection_publish(input, &mut pending_idle),
            DetectionPublishDecision::Publish {
                state: AgentState::Working,
                visible_idle: false,
                visible_blocker: false,
                visible_working: false,
                process_exited: false,
            }
        );
    }

    #[test]
    fn positive_idle_observation_wins_immediately_over_recent_agent_output() {
        let now = std::time::Instant::now();
        let mut input =
            screen_publish_input(AgentState::Working, screen_detection(AgentState::Idle), now);
        input.recent_output = true;
        let mut pending_idle = PendingIdleConfirmation::default();

        assert_eq!(
            decide_screen_detection_publish(input, &mut pending_idle),
            DetectionPublishDecision::Publish {
                state: AgentState::Idle,
                visible_idle: true,
                visible_blocker: false,
                visible_working: false,
                process_exited: false,
            }
        );
    }

    #[test]
    fn recent_agent_output_expires_before_a_plain_idle_transition_completes() {
        let now = std::time::Instant::now();
        let mut last_output_seq = None;
        let mut last_output_at = None;
        assert!(recent_agent_output(
            Some(Agent::Codex),
            1,
            &mut last_output_seq,
            &mut last_output_at,
            now,
        ));

        let expired_at = now + AGENT_RECENT_OUTPUT_WINDOW + std::time::Duration::from_millis(1);
        assert!(!recent_agent_output(
            Some(Agent::Codex),
            1,
            &mut last_output_seq,
            &mut last_output_at,
            expired_at,
        ));

        let plain_idle = AgentDetection {
            state: AgentState::Idle,
            skip_state_update: false,
            visible_idle: false,
            visible_blocker: false,
            visible_working: false,
        };
        let mut input = screen_publish_input(AgentState::Working, plain_idle, expired_at);
        input.recent_output = false;
        let mut pending_idle = PendingIdleConfirmation::default();
        assert_eq!(
            decide_screen_detection_publish(input, &mut pending_idle),
            DetectionPublishDecision::NoPublish
        );

        for confirmation in 1..=3 {
            input.now = expired_at + AGENT_PENDING_IDLE_RECHECK * confirmation;
            let decision = decide_screen_detection_publish(input, &mut pending_idle);
            if confirmation < 3 {
                assert_eq!(decision, DetectionPublishDecision::NoPublish);
            } else {
                assert_eq!(
                    decision,
                    DetectionPublishDecision::Publish {
                        state: AgentState::Idle,
                        visible_idle: false,
                        visible_blocker: false,
                        visible_working: false,
                        process_exited: false,
                    }
                );
            }
        }
    }

    #[test]
    fn detection_content_change_tracks_raw_nonempty_reads_for_scan_scheduling() {
        let seq = AtomicU64::new(0);

        observe_detection_content_change(b"", &seq);
        assert_eq!(seq.load(Ordering::Relaxed), 0);

        observe_detection_content_change(b"\x1b[?2026h", &seq);
        assert_eq!(seq.load(Ordering::Relaxed), 1);

        observe_detection_content_change(b"body bytes", &seq);
        assert_eq!(seq.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn local_terminal_mutations_can_invalidate_idle_scan_skip() {
        let seq = AtomicU64::new(0);

        mark_detection_content_changed(&seq);

        assert_eq!(seq.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn output_retires_a_hook_gate_after_the_grace_period() {
        let started = std::time::Instant::now();
        let mut retirement = FullLifecycleHookOutputRetirement::default();

        assert_eq!(retirement.observe(4, 4, started), None);
        assert_eq!(
            retirement.observe(5, 4, started + std::time::Duration::from_secs(1)),
            None
        );
        assert_eq!(
            retirement.observe(
                5,
                4,
                started
                    + std::time::Duration::from_secs(1)
                    + FULL_LIFECYCLE_HOOK_OUTPUT_RETIREMENT_GRACE
            ),
            Some(started + std::time::Duration::from_secs(1))
        );
    }

    #[test]
    fn a_silent_hook_gate_does_not_expire() {
        let started = std::time::Instant::now();
        let mut retirement = FullLifecycleHookOutputRetirement::default();

        assert_eq!(retirement.observe(4, 4, started), None);
        assert_eq!(
            retirement.observe(
                4,
                4,
                started + FULL_LIFECYCLE_HOOK_OUTPUT_RETIREMENT_GRACE * 3,
            ),
            None
        );
    }

    #[test]
    fn output_before_the_first_detection_tick_starts_the_grace_period() {
        let started = std::time::Instant::now();
        let mut retirement = FullLifecycleHookOutputRetirement::default();

        assert_eq!(retirement.observe(5, 4, started), None);
        assert_eq!(
            retirement.observe(5, 4, started + FULL_LIFECYCLE_HOOK_OUTPUT_RETIREMENT_GRACE),
            Some(started)
        );
    }
}
