use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::LazyLock;
use std::sync::Mutex;
#[cfg(test)]
use std::sync::{Arc, Barrier};
use std::time::SystemTime;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::api::schema::AgentStatus;
use crate::layout::PaneId;

pub(crate) const MAX_LINKS: usize = 200;

const LINK_LOOKBEHIND_BYTES: usize = 256;
// Ingest retains at most 2 MiB: the first and last 1 MiB when output overflows.
// The middle and any open sequence crossing either cut are dropped.
const MAX_PENDING_LINK_BYTES: usize = 2 * 1024 * 1024;
const STAGING_WINDOW_BYTES: usize = MAX_PENDING_LINK_BYTES / 2;
const MAX_OPEN_SEQUENCE_BYTES: usize = 8 * 1024;
// Publish complete URLs through 8 KiB. Once a URL exceeds this bound, discard the
// entire open sequence until its terminator so no printed prefix can escape.
const MAX_URL_BYTES: usize = 8 * 1024;
// RFC schemes are much smaller in practice; cap lookbehind and reject the whole
// URL when the scheme before `://` is longer than 64 bytes.
const MAX_SCHEME_BYTES: usize = 64;
const MAX_MARKER_TAIL_BYTES: usize = MAX_SCHEME_BYTES + 2;

#[derive(Debug)]
pub(crate) struct LinkExtractionGate {
    processing: Mutex<()>,
    pending: Mutex<PendingLinkBytes>,
    extracting: AtomicBool,
    active: AtomicBool,
    marker_tail: AtomicMarkerTail,
    extractions: AtomicU64,
    #[cfg(test)]
    lock_acquisitions: AtomicU64,
    #[cfg(test)]
    parse_passes: AtomicU64,
    #[cfg(test)]
    parses_outside_processing: AtomicU64,
    #[cfg(test)]
    scanned_bytes: AtomicU64,
    #[cfg(test)]
    observe_race_hook: Mutex<Option<(Arc<Barrier>, Arc<Barrier>)>>,
}

impl Default for LinkExtractionGate {
    fn default() -> Self {
        Self {
            processing: Mutex::new(()),
            pending: Mutex::default(),
            extracting: AtomicBool::new(false),
            active: AtomicBool::new(false),
            marker_tail: AtomicMarkerTail::default(),
            extractions: AtomicU64::new(0),
            #[cfg(test)]
            lock_acquisitions: AtomicU64::new(0),
            #[cfg(test)]
            parse_passes: AtomicU64::new(0),
            #[cfg(test)]
            parses_outside_processing: AtomicU64::new(0),
            #[cfg(test)]
            scanned_bytes: AtomicU64::new(0),
            #[cfg(test)]
            observe_race_hook: Mutex::new(None),
        }
    }
}

#[derive(Debug)]
struct AtomicMarkerTail {
    words: [AtomicU64; MAX_MARKER_TAIL_BYTES.div_ceil(8)],
    len: AtomicU64,
    sequence: AtomicU64,
}

impl Default for AtomicMarkerTail {
    fn default() -> Self {
        Self {
            words: std::array::from_fn(|_| AtomicU64::new(0)),
            len: AtomicU64::new(0),
            sequence: AtomicU64::new(0),
        }
    }
}

impl AtomicMarkerTail {
    fn load(&self) -> ([u8; MAX_MARKER_TAIL_BYTES], usize, u64) {
        loop {
            let sequence = self.sequence.load(Ordering::Acquire);
            if !sequence.is_multiple_of(2) {
                std::hint::spin_loop();
                continue;
            }
            let len = (self.len.load(Ordering::Relaxed) as usize).min(MAX_MARKER_TAIL_BYTES);
            let mut bytes = [0; MAX_MARKER_TAIL_BYTES];
            for (word_index, word) in self.words.iter().enumerate() {
                let unpacked = word.load(Ordering::Relaxed).to_le_bytes();
                let start = word_index * 8;
                let end = (start + 8).min(MAX_MARKER_TAIL_BYTES);
                bytes[start..end].copy_from_slice(&unpacked[..end - start]);
            }
            if self.sequence.load(Ordering::Acquire) == sequence {
                return (bytes, len, sequence);
            }
        }
    }

    fn store(&self, bytes: &[u8]) {
        loop {
            let (_, _, sequence) = self.load();
            if self.store_if_sequence(bytes, sequence) {
                return;
            }
        }
    }

    fn store_if_sequence(&self, bytes: &[u8], sequence: u64) -> bool {
        debug_assert!(bytes.len() <= MAX_MARKER_TAIL_BYTES);
        if self
            .sequence
            .compare_exchange(
                sequence,
                sequence.wrapping_add(1),
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .is_err()
        {
            return false;
        }
        for (word_index, word) in self.words.iter().enumerate() {
            let start = word_index * 8;
            let end = (start + 8).min(bytes.len());
            let mut packed = [0; 8];
            if start < end {
                packed[..end - start].copy_from_slice(&bytes[start..end]);
            }
            word.store(u64::from_le_bytes(packed), Ordering::Relaxed);
        }
        self.len.store(bytes.len() as u64, Ordering::Relaxed);
        self.sequence
            .store(sequence.wrapping_add(2), Ordering::Release);
        true
    }
}

#[derive(Debug, Default)]
struct PendingLinkBytes {
    bytes: Vec<u8>,
    unprocessed: Vec<u8>,
    overflow_tail: VecDeque<u8>,
    overflow_transition: OverflowTransition,
    overflow_at_url_boundary: bool,
    overflow_dropped: DroppedLinkBytes,
    staging_truncated: bool,
    open_url_scan_at: usize,
    dirty: bool,
    queued_output_urls: Vec<String>,
    queued_osc8_urls: Vec<String>,
    truncated_sequence: Option<OpenSequenceKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenSequenceKind {
    Url,
    Csi,
    Osc,
    OscEscape,
    Escape,
}

#[derive(Debug, Default)]
struct DroppedLinkBytes {
    bytes: Vec<u8>,
    truncated_sequence: Option<OpenSequenceKind>,
    safe_to_extract: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverflowTerminalState {
    Visible,
    Csi,
    Osc,
    OscEscape,
    Escape,
}

impl OverflowTerminalState {
    const ALL: [Self; 5] = [
        Self::Visible,
        Self::Csi,
        Self::Osc,
        Self::OscEscape,
        Self::Escape,
    ];

    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Debug)]
struct OverflowTransition {
    states: [OverflowTerminalState; 5],
}

impl Default for OverflowTransition {
    fn default() -> Self {
        Self {
            states: OverflowTerminalState::ALL,
        }
    }
}

impl OverflowTransition {
    fn record_dropped<'a>(&mut self, bytes: impl IntoIterator<Item = &'a u8>) {
        for byte in bytes {
            for state in &mut self.states {
                *state = advance_overflow_terminal_state(*state, *byte);
            }
        }
    }

    fn apply(&self, state: OverflowTerminalState) -> OverflowTerminalState {
        self.states[state.index()]
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ExtractedAgentLinks {
    pub(crate) output_urls: Vec<String>,
    pub(crate) osc8_urls: Vec<String>,
}

impl LinkExtractionGate {
    /// The pane's PTY `on_read` callback is the sole producer for a gate.
    /// Detection may consume links concurrently through `take_links`.
    pub(crate) fn observe_chunk(&self, bytes: &[u8]) {
        let was_active = self.active.load(Ordering::Acquire);
        let (marker_tail, marker_tail_len, marker_sequence) = self.marker_tail.load();
        let has_scheme_marker = memchr::memmem::find(bytes, b"://").is_some();
        let has_osc8_marker = memchr::memmem::find(bytes, b"\x1b]8;").is_some();
        if !was_active
            && !has_scheme_marker
            && !has_osc8_marker
            && !marker_tail_can_continue(&marker_tail[..marker_tail_len])
        {
            let (tail, tail_len) = append_marker_tail(&marker_tail[..marker_tail_len], bytes);
            #[cfg(test)]
            if let Some((arrived, resume)) = self
                .observe_race_hook
                .lock()
                .ok()
                .and_then(|hook| hook.clone())
            {
                arrived.wait();
                resume.wait();
            }
            if self
                .marker_tail
                .store_if_sequence(&tail[..tail_len], marker_sequence)
            {
                return;
            }
        }

        let Ok(_processing) = self.processing.lock() else {
            return;
        };
        if self.active.load(Ordering::Acquire) {
            #[cfg(test)]
            self.lock_acquisitions.fetch_add(1, Ordering::Relaxed);
            let Ok(mut pending) = self.pending.lock() else {
                return;
            };
            stage_unprocessed(&mut pending, bytes);
            pending.dirty = true;
            self.active.store(true, Ordering::Release);
            return;
        }

        let (marker_tail, marker_tail_len, _) = self.marker_tail.load();
        let mut candidate = Vec::with_capacity(marker_tail_len + bytes.len());
        candidate.extend_from_slice(&marker_tail[..marker_tail_len]);
        candidate.extend_from_slice(bytes);
        let Some(marker_start) =
            find_scheme_start(&candidate).or_else(|| find_osc8_start(&candidate))
        else {
            let (tail, tail_len) = marker_tail_suffix(&candidate);
            self.marker_tail.store(&tail[..tail_len]);
            return;
        };

        #[cfg(test)]
        self.lock_acquisitions.fetch_add(1, Ordering::Relaxed);
        let Ok(mut pending) = self.pending.lock() else {
            return;
        };
        stage_unprocessed(&mut pending, &candidate[marker_start..]);
        pending.dirty = true;
        self.active.store(true, Ordering::Release);
        self.marker_tail.store(&[]);
    }

    #[cfg(test)]
    pub(crate) fn take_dirty(&self) -> bool {
        self.take_links().is_some()
    }

    pub(crate) fn take_links(&self) -> Option<ExtractedAgentLinks> {
        if !self.active.load(Ordering::Acquire) {
            return None;
        }
        if self
            .extracting
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }
        let _claim = ExtractionClaim(&self.extracting);
        let mut pending = {
            let _processing = self.processing.lock().ok()?;
            let mut stored = self.pending.lock().ok()?;
            if !stored.dirty {
                return None;
            }
            std::mem::take(&mut *stored)
        };
        let unprocessed = std::mem::take(&mut pending.unprocessed);
        let overflow_tail = std::mem::take(&mut pending.overflow_tail)
            .into_iter()
            .collect::<Vec<_>>();
        let overflow_transition = std::mem::take(&mut pending.overflow_transition);
        let staging_truncated = std::mem::take(&mut pending.staging_truncated);
        let overflow_start_state = if staging_truncated {
            let initial = match pending.truncated_sequence {
                Some(OpenSequenceKind::Csi) => OverflowTerminalState::Csi,
                Some(OpenSequenceKind::Osc) => OverflowTerminalState::Osc,
                Some(OpenSequenceKind::OscEscape) => OverflowTerminalState::OscEscape,
                Some(OpenSequenceKind::Escape) => OverflowTerminalState::Escape,
                Some(OpenSequenceKind::Url) | None => OverflowTerminalState::Visible,
            };
            let after_pending = overflow_terminal_state_after(initial, &pending.bytes);
            let after_prefix = overflow_terminal_state_after(after_pending, &unprocessed);
            overflow_transition.apply(after_prefix)
        } else {
            OverflowTerminalState::Visible
        };
        if !staging_truncated
            && pending.truncated_sequence.is_none()
            && pending.bytes.len().saturating_add(unprocessed.len()) <= MAX_PENDING_LINK_BYTES
        {
            pending.bytes.extend_from_slice(&unprocessed);
        } else {
            append_pending(&mut pending, &unprocessed);
        }
        if staging_truncated {
            // Bytes beyond the staging bound were discarded at ingest. The retained
            // prefix may therefore end inside a URL or terminal sequence; discard that
            // open sequence rather than ever publishing a truncated prefix.
            pending.open_url_scan_at = 0;
            if let Some(safe_start) = overflow_suffix_start(&overflow_tail, overflow_start_state) {
                let mut tail_extracted = extract_agent_links(&overflow_tail[safe_start..]);
                queue_links(
                    &mut pending.queued_output_urls,
                    &mut tail_extracted.links.output_urls,
                );
                queue_links(
                    &mut pending.queued_osc8_urls,
                    &mut tail_extracted.links.osc8_urls,
                );
            }
        }
        self.record_parse_pass();
        let mut deferred_open_url = false;
        let mut discarded_open_url = false;
        if !staging_truncated && pending.open_url_scan_at > 0 {
            self.record_scanned_bytes(pending.bytes.len().saturating_sub(pending.open_url_scan_at));
            match resume_output_url(&pending.bytes, pending.open_url_scan_at) {
                ResumedOutputUrl::Completed {
                    url,
                    remaining,
                    mut osc8_urls,
                } => {
                    if let Some(url) = url {
                        pending.queued_output_urls.push(url);
                    }
                    queue_links(&mut pending.queued_osc8_urls, &mut osc8_urls);
                    pending.bytes = remaining;
                    pending.open_url_scan_at = 0;
                }
                ResumedOutputUrl::Deferred {
                    bytes,
                    scan_at,
                    mut osc8_urls,
                } => {
                    queue_links(&mut pending.queued_osc8_urls, &mut osc8_urls);
                    pending.bytes = bytes;
                    pending.open_url_scan_at = scan_at;
                    deferred_open_url = true;
                }
                ResumedOutputUrl::Oversized { mut osc8_urls } => {
                    queue_links(&mut pending.queued_osc8_urls, &mut osc8_urls);
                    pending.bytes.clear();
                    pending.open_url_scan_at = 0;
                    pending.truncated_sequence = Some(OpenSequenceKind::Url);
                    discarded_open_url = true;
                }
            }
        }
        let mut extracted = if deferred_open_url {
            AgentLinkExtraction {
                links: ExtractedAgentLinks {
                    output_urls: Vec::new(),
                    osc8_urls: Vec::new(),
                },
                unterminated_url: true,
                incomplete_terminal_sequence: false,
                tail_output_url: None,
            }
        } else if discarded_open_url {
            // The open URL exceeded its publication bound without reaching a
            // terminator. Drop it whole and consume its continuation later.
            AgentLinkExtraction {
                links: ExtractedAgentLinks {
                    output_urls: Vec::new(),
                    osc8_urls: Vec::new(),
                },
                unterminated_url: false,
                incomplete_terminal_sequence: false,
                tail_output_url: None,
            }
        } else {
            self.record_scanned_bytes(pending.bytes.len());
            extract_agent_links(&pending.bytes)
        };
        extracted
            .links
            .output_urls
            .append(&mut pending.queued_output_urls);
        extracted
            .links
            .osc8_urls
            .append(&mut pending.queued_osc8_urls);
        extracted.links.output_urls.sort_unstable();
        extracted.links.output_urls.dedup();
        extracted.links.osc8_urls.sort_unstable();
        extracted.links.osc8_urls.dedup();
        pending.dirty = false;
        let (tail, tail_len) = if staging_truncated || deferred_open_url {
            ([0; MAX_MARKER_TAIL_BYTES], 0)
        } else {
            marker_tail_suffix(&pending.bytes)
        };
        if staging_truncated {
            pending.bytes.clear();
            pending.truncated_sequence = None;
            pending.open_url_scan_at = 0;
        } else if deferred_open_url {
            // The bytes already scanned remain the sole occurrence owner until
            // a later chunk supplies a URL terminator.
        } else if pending.truncated_sequence.is_none()
            && (extracted.unterminated_url || extracted.incomplete_terminal_sequence)
        {
            let (carry, truncated_sequence) = pending_sequence_carry(&pending.bytes);
            pending.bytes = carry;
            pending.truncated_sequence = truncated_sequence;
            pending.open_url_scan_at =
                if extracted.unterminated_url && pending.truncated_sequence.is_none() {
                    strip_terminal_sequences(&pending.bytes)
                        .incomplete_terminal_sequence_start
                        .map_or(pending.bytes.len(), |(start, _)| start)
                } else {
                    0
                };
        } else if pending.truncated_sequence.is_none() {
            pending.bytes.clear();
            pending.open_url_scan_at = 0;
        }
        let found_links =
            !extracted.links.output_urls.is_empty() || !extracted.links.osc8_urls.is_empty();
        {
            let _processing = self.processing.lock().ok()?;
            let mut stored = self.pending.lock().ok()?;
            let received_while_parsing = !stored.unprocessed.is_empty();
            if received_while_parsing && pending.bytes.is_empty() {
                pending.bytes.extend_from_slice(&tail[..tail_len]);
            }
            debug_assert!(stored.bytes.is_empty());
            debug_assert!(stored.truncated_sequence.is_none());
            stored.bytes = pending.bytes;
            stored.truncated_sequence = pending.truncated_sequence;
            stored.open_url_scan_at = pending.open_url_scan_at;
            queue_links(
                &mut stored.queued_output_urls,
                &mut pending.queued_output_urls,
            );
            queue_links(&mut stored.queued_osc8_urls, &mut pending.queued_osc8_urls);
            if received_while_parsing {
                stored.dirty = true;
                self.active.store(true, Ordering::Release);
            } else {
                stored.dirty = false;
                let has_carry = !stored.bytes.is_empty()
                    || stored.truncated_sequence.is_some()
                    || !stored.queued_output_urls.is_empty()
                    || !stored.queued_osc8_urls.is_empty();
                if has_carry {
                    self.active.store(true, Ordering::Release);
                } else {
                    self.marker_tail.store(&tail[..tail_len]);
                    self.active.store(false, Ordering::Release);
                }
            }
        }
        if !found_links {
            return None;
        }
        self.extractions.fetch_add(1, Ordering::Relaxed);
        Some(extracted.links)
    }

    #[cfg(test)]
    pub(crate) fn extraction_count(&self) -> u64 {
        self.extractions.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn lock_acquisition_count(&self) -> u64 {
        self.lock_acquisitions.load(Ordering::Relaxed)
    }

    fn record_parse_pass(&self) {
        #[cfg(test)]
        {
            self.parse_passes.fetch_add(1, Ordering::Relaxed);
            if self.processing.try_lock().is_ok() {
                self.parses_outside_processing
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn record_scanned_bytes(&self, bytes: usize) {
        let _ = bytes;
        #[cfg(test)]
        self.scanned_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    #[cfg(test)]
    fn staged_byte_count(&self) -> usize {
        self.pending
            .lock()
            .map(|pending| pending.unprocessed.len() + pending.overflow_tail.len())
            .unwrap_or_default()
    }

    #[cfg(test)]
    fn set_observe_race_hook(&self, arrived: Arc<Barrier>, resume: Arc<Barrier>) {
        if let Ok(mut hook) = self.observe_race_hook.lock() {
            *hook = Some((arrived, resume));
        }
    }
}

fn stage_unprocessed(pending: &mut PendingLinkBytes, bytes: &[u8]) {
    if pending.staging_truncated {
        append_overflow_tail(pending, bytes);
        return;
    }
    let remaining = MAX_PENDING_LINK_BYTES.saturating_sub(pending.unprocessed.len());
    let appended = remaining.min(bytes.len());
    pending.unprocessed.extend_from_slice(&bytes[..appended]);
    if appended == bytes.len() {
        return;
    }

    pending.staging_truncated = true;
    let displaced = pending.unprocessed.split_off(STAGING_WINDOW_BYTES);
    pending.overflow_tail.extend(displaced);
    pending.overflow_at_url_boundary = overflow_prefix_ends_at_url_boundary(pending);
    append_overflow_tail(pending, &bytes[appended..]);
}

fn append_overflow_tail(pending: &mut PendingLinkBytes, bytes: &[u8]) {
    let capacity = STAGING_WINDOW_BYTES;
    if bytes.len() >= capacity {
        let evicted = pending.overflow_tail.drain(..).collect::<Vec<_>>();
        extract_dropped_links(pending, &evicted);
        extract_dropped_links(pending, &bytes[..bytes.len() - capacity]);
        pending
            .overflow_tail
            .extend(&bytes[bytes.len() - capacity..]);
        return;
    }
    let overflow = pending
        .overflow_tail
        .len()
        .saturating_add(bytes.len())
        .saturating_sub(capacity);
    if overflow > 0 {
        let evicted = pending.overflow_tail.drain(..overflow).collect::<Vec<_>>();
        extract_dropped_links(pending, &evicted);
    }
    pending.overflow_tail.extend(bytes);
}

fn extract_dropped_links(pending: &mut PendingLinkBytes, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    let initial = match pending.truncated_sequence {
        Some(OpenSequenceKind::Csi) => OverflowTerminalState::Csi,
        Some(OpenSequenceKind::Osc) => OverflowTerminalState::Osc,
        Some(OpenSequenceKind::OscEscape) => OverflowTerminalState::OscEscape,
        Some(OpenSequenceKind::Escape) => OverflowTerminalState::Escape,
        Some(OpenSequenceKind::Url) | None => OverflowTerminalState::Visible,
    };
    let after_pending = overflow_terminal_state_after(initial, &pending.bytes);
    let after_prefix = overflow_terminal_state_after(after_pending, &pending.unprocessed);
    let start_state = pending.overflow_transition.apply(after_prefix);
    let safe_start = if pending.overflow_dropped.safe_to_extract {
        Some(0)
    } else {
        let safe_start =
            if start_state == OverflowTerminalState::Visible && pending.overflow_at_url_boundary {
                Some(0)
            } else {
                visible_url_boundary(bytes, start_state)
            };
        if safe_start.is_some() {
            pending.overflow_dropped.safe_to_extract = true;
        }
        safe_start
    };
    if let Some(safe_start) = safe_start {
        append_dropped_link_bytes(
            &mut pending.overflow_dropped,
            &mut pending.queued_output_urls,
            &mut pending.queued_osc8_urls,
            &bytes[safe_start..],
        );
    }
    pending.overflow_at_url_boundary =
        overflow_url_boundary_after(start_state, pending.overflow_at_url_boundary, bytes);
    pending.overflow_transition.record_dropped(bytes.iter());
}

fn append_dropped_link_bytes(
    dropped: &mut DroppedLinkBytes,
    output_urls: &mut Vec<String>,
    osc8_urls: &mut Vec<String>,
    mut bytes: &[u8],
) {
    const SCAN_WINDOW_BYTES: usize = 64 * 1024;

    while !bytes.is_empty() {
        if let Some(mut kind) = dropped.truncated_sequence {
            let split_st = kind == OpenSequenceKind::OscEscape && bytes.first() == Some(&b'\\');
            if kind == OpenSequenceKind::OscEscape && !split_st {
                kind = OpenSequenceKind::Osc;
            }
            let terminator = split_st
                .then_some((0, 1))
                .or_else(|| open_sequence_terminator(kind, bytes));
            let Some((terminator, terminator_len)) = terminator else {
                dropped.truncated_sequence = Some(truncated_sequence_kind(kind, bytes));
                return;
            };
            bytes = &bytes[terminator + terminator_len..];
            dropped.truncated_sequence = None;
            if bytes.is_empty() {
                return;
            }
        }

        let appended = bytes.len().min(SCAN_WINDOW_BYTES);
        dropped.bytes.extend_from_slice(&bytes[..appended]);
        bytes = &bytes[appended..];
        flush_dropped_links(dropped, output_urls, osc8_urls);
    }
}

fn flush_dropped_links(
    dropped: &mut DroppedLinkBytes,
    output_urls: &mut Vec<String>,
    osc8_urls: &mut Vec<String>,
) {
    let mut extracted = extract_agent_links(&dropped.bytes);
    queue_links(output_urls, &mut extracted.links.output_urls);
    queue_links(osc8_urls, &mut extracted.links.osc8_urls);
    if extracted
        .tail_output_url
        .as_deref()
        .is_some_and(|url| !url_within_bounds(url))
    {
        dropped.bytes.clear();
        dropped.truncated_sequence = Some(OpenSequenceKind::Url);
        return;
    }
    let (carry, truncated_sequence) = pending_sequence_carry(&dropped.bytes);
    dropped.bytes = carry;
    dropped.truncated_sequence = truncated_sequence;
}

fn overflow_prefix_ends_at_url_boundary(pending: &PendingLinkBytes) -> bool {
    let initial = match pending.truncated_sequence {
        Some(OpenSequenceKind::Url) => return false,
        Some(OpenSequenceKind::Csi) => OverflowTerminalState::Csi,
        Some(OpenSequenceKind::Osc) => OverflowTerminalState::Osc,
        Some(OpenSequenceKind::OscEscape) => OverflowTerminalState::OscEscape,
        Some(OpenSequenceKind::Escape) => OverflowTerminalState::Escape,
        None => OverflowTerminalState::Visible,
    };
    let boundary = pending.truncated_sequence.is_none();
    let boundary = overflow_url_boundary_after(initial, boundary, &pending.bytes);
    let state = overflow_terminal_state_after(initial, &pending.bytes);
    overflow_url_boundary_after(state, boundary, &pending.unprocessed)
}

fn visible_url_boundary(bytes: &[u8], mut state: OverflowTerminalState) -> Option<usize> {
    for (index, byte) in bytes.iter().copied().enumerate() {
        let previous = state;
        state = advance_overflow_terminal_state(state, byte);
        if previous == OverflowTerminalState::Visible && byte != b'\x1b' && is_url_terminator(byte)
        {
            return Some(index + 1);
        }
    }
    None
}

fn overflow_url_boundary_after(
    mut state: OverflowTerminalState,
    mut boundary: bool,
    bytes: &[u8],
) -> bool {
    for byte in bytes.iter().copied() {
        let previous = state;
        state = advance_overflow_terminal_state(state, byte);
        if previous == OverflowTerminalState::Visible && byte != b'\x1b' {
            boundary = is_url_terminator(byte);
        }
    }
    boundary
}

fn advance_overflow_terminal_state(
    state: OverflowTerminalState,
    byte: u8,
) -> OverflowTerminalState {
    match state {
        OverflowTerminalState::Visible => {
            if byte == b'\x1b' {
                OverflowTerminalState::Escape
            } else {
                OverflowTerminalState::Visible
            }
        }
        OverflowTerminalState::Csi => {
            if (0x40..=0x7e).contains(&byte) {
                OverflowTerminalState::Visible
            } else {
                OverflowTerminalState::Csi
            }
        }
        OverflowTerminalState::Osc => match byte {
            b'\x07' => OverflowTerminalState::Visible,
            b'\x1b' => OverflowTerminalState::OscEscape,
            _ => OverflowTerminalState::Osc,
        },
        OverflowTerminalState::OscEscape => match byte {
            b'\\' | b'\x07' => OverflowTerminalState::Visible,
            b'\x1b' => OverflowTerminalState::OscEscape,
            _ => OverflowTerminalState::Osc,
        },
        OverflowTerminalState::Escape => match byte {
            b'[' => OverflowTerminalState::Csi,
            b']' => OverflowTerminalState::Osc,
            _ => OverflowTerminalState::Visible,
        },
    }
}

fn overflow_terminal_state_after(
    mut state: OverflowTerminalState,
    bytes: &[u8],
) -> OverflowTerminalState {
    for byte in bytes {
        state = advance_overflow_terminal_state(state, *byte);
    }
    state
}

fn overflow_suffix_start(bytes: &[u8], mut state: OverflowTerminalState) -> Option<usize> {
    let started_in_sequence = state != OverflowTerminalState::Visible;
    for (index, byte) in bytes.iter().copied().enumerate() {
        let previous = state;
        state = advance_overflow_terminal_state(state, byte);
        if started_in_sequence
            && previous != OverflowTerminalState::Visible
            && state == OverflowTerminalState::Visible
        {
            return Some(index + 1);
        }
        if !started_in_sequence
            && previous == OverflowTerminalState::Visible
            && byte.is_ascii_whitespace()
        {
            return Some(index + 1);
        }
    }
    None
}

struct ExtractionClaim<'a>(&'a AtomicBool);

impl Drop for ExtractionClaim<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn append_pending(pending: &mut PendingLinkBytes, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        if let Some(mut kind) = pending.truncated_sequence {
            let split_st = kind == OpenSequenceKind::OscEscape && bytes.first() == Some(&b'\\');
            if kind == OpenSequenceKind::OscEscape && !split_st {
                kind = OpenSequenceKind::Osc;
            }
            let terminator = split_st
                .then_some((0, 1))
                .or_else(|| open_sequence_terminator(kind, bytes));
            let Some((terminator, terminator_len)) = terminator else {
                pending.truncated_sequence = Some(truncated_sequence_kind(kind, bytes));
                return;
            };
            bytes = &bytes[terminator + terminator_len..];
            pending.truncated_sequence = None;
            if bytes.is_empty() {
                break;
            }
        }
        let remaining = MAX_PENDING_LINK_BYTES.saturating_sub(pending.bytes.len());
        let appended = bytes.len().min(remaining);
        pending.bytes.extend_from_slice(&bytes[..appended]);
        bytes = &bytes[appended..];
        discard_overlong_url(pending);
        if bytes.is_empty() {
            discard_overlong_terminal_sequence(pending);
            break;
        }

        let mut extracted = extract_agent_links(&pending.bytes);
        queue_links(
            &mut pending.queued_output_urls,
            &mut extracted.links.output_urls,
        );
        queue_links(
            &mut pending.queued_osc8_urls,
            &mut extracted.links.osc8_urls,
        );
        let (carry, truncated_sequence) = pending_sequence_carry(&pending.bytes);
        pending.bytes = carry;
        pending.truncated_sequence = truncated_sequence;
    }
}

fn discard_overlong_url(pending: &mut PendingLinkBytes) {
    if pending.truncated_sequence.is_some() {
        return;
    }
    let mut extracted = extract_agent_links(&pending.bytes);
    let Some(tail_url) = extracted.tail_output_url.as_deref() else {
        return;
    };
    if url_within_bounds(tail_url) {
        return;
    }
    queue_links(
        &mut pending.queued_output_urls,
        &mut extracted.links.output_urls,
    );
    queue_links(
        &mut pending.queued_osc8_urls,
        &mut extracted.links.osc8_urls,
    );
    pending.bytes.clear();
    pending.truncated_sequence = Some(OpenSequenceKind::Url);
}

fn discard_overlong_terminal_sequence(pending: &mut PendingLinkBytes) {
    let stripped = strip_terminal_sequences(&pending.bytes);
    let Some((start, kind)) = stripped.incomplete_terminal_sequence_start else {
        return;
    };
    if kind == OpenSequenceKind::Escape
        || (kind == OpenSequenceKind::Osc
            && open_osc8_target_within_bounds(&pending.bytes[start..]))
        || pending.bytes.len().saturating_sub(start) <= MAX_OPEN_SEQUENCE_BYTES
    {
        return;
    }
    let mut extracted = extract_agent_links(&pending.bytes[..start]);
    queue_links(
        &mut pending.queued_output_urls,
        &mut extracted.links.output_urls,
    );
    queue_links(
        &mut pending.queued_osc8_urls,
        &mut extracted.links.osc8_urls,
    );
    let kind = truncated_sequence_kind(kind, &pending.bytes);
    pending.bytes.clear();
    pending.truncated_sequence = Some(kind);
}

fn pending_sequence_carry(bytes: &[u8]) -> (Vec<u8>, Option<OpenSequenceKind>) {
    let stripped = strip_terminal_sequences(bytes);
    let open_url_start =
        unterminated_output_url_start(&stripped.visible_bytes, &stripped.visible_offsets);
    if let Some((url, visible_url)) = open_url_start {
        if let Some((terminal, _)) = stripped.incomplete_terminal_sequence_start {
            if url < terminal {
                let mut carry = stripped.visible_bytes[visible_url..].to_vec();
                carry.extend_from_slice(&bytes[terminal..]);
                return (carry, None);
            }
        } else {
            return (stripped.visible_bytes[visible_url..].to_vec(), None);
        }
    }
    let open = stripped.incomplete_terminal_sequence_start;
    let Some((start, kind)) = open else {
        let tail_start = bytes.len().saturating_sub(LINK_LOOKBEHIND_BYTES);
        return (bytes[tail_start..].to_vec(), None);
    };
    if kind == OpenSequenceKind::Osc && open_osc8_target_within_bounds(&bytes[start..]) {
        return (bytes[start..].to_vec(), None);
    }
    let end = (start + MAX_OPEN_SEQUENCE_BYTES).min(bytes.len());
    if end < bytes.len() {
        return (
            Vec::new(),
            Some(truncated_sequence_kind(kind, &bytes[start..])),
        );
    }
    (bytes[start..end].to_vec(), None)
}

fn open_osc8_target_within_bounds(bytes: &[u8]) -> bool {
    let Some(osc8) = bytes.strip_prefix(b"\x1b]8;") else {
        return false;
    };
    let Some(parameter_end) = osc8.iter().position(|byte| *byte == b';') else {
        return true;
    };
    let target = &osc8[parameter_end + 1..];
    target.strip_suffix(b"\x1b").unwrap_or(target).len() <= MAX_URL_BYTES
}

fn unterminated_output_url_start(
    visible_bytes: &[u8],
    visible_offsets: &[usize],
) -> Option<(usize, usize)> {
    let tail_start = visible_bytes
        .iter()
        .rposition(|byte| is_url_terminator(*byte))
        .map_or(0, |index| index + 1);
    let visible_start = tail_start + find_scheme_start(&visible_bytes[tail_start..])?;
    Some((*visible_offsets.get(visible_start)?, visible_start))
}

fn truncated_sequence_kind(kind: OpenSequenceKind, bytes: &[u8]) -> OpenSequenceKind {
    if kind == OpenSequenceKind::Osc && bytes.last() == Some(&b'\x1b') {
        OpenSequenceKind::OscEscape
    } else {
        kind
    }
}

fn open_sequence_terminator(kind: OpenSequenceKind, bytes: &[u8]) -> Option<(usize, usize)> {
    match kind {
        OpenSequenceKind::Url => bytes
            .iter()
            .position(|byte| is_url_terminator(*byte))
            .map(|index| (index, 1)),
        OpenSequenceKind::Csi => bytes
            .iter()
            .position(|byte| (0x40..=0x7e).contains(byte))
            .map(|index| (index, 1)),
        OpenSequenceKind::Osc => osc_terminator(bytes),
        OpenSequenceKind::OscEscape => (bytes.first() == Some(&b'\\')).then_some((0, 1)),
        OpenSequenceKind::Escape => (!bytes.is_empty()).then_some((0, 1)),
    }
}

fn is_url_terminator(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || byte.is_ascii_control()
        || matches!(byte, b'"' | b'\'' | b'<' | b'>')
}

fn queue_links(queue: &mut Vec<String>, links: &mut Vec<String>) {
    queue.append(links);
    queue.sort_unstable();
    queue.dedup();
    if queue.len() > MAX_LINKS {
        queue.drain(..queue.len() - MAX_LINKS);
    }
}

fn append_marker_tail(previous: &[u8], bytes: &[u8]) -> ([u8; MAX_MARKER_TAIL_BYTES], usize) {
    let mut candidate = [0; MAX_MARKER_TAIL_BYTES * 2];
    let candidate_len = if bytes.len() > MAX_MARKER_TAIL_BYTES {
        let start = bytes.len() - (MAX_MARKER_TAIL_BYTES + 1);
        candidate[..MAX_MARKER_TAIL_BYTES + 1].copy_from_slice(&bytes[start..]);
        MAX_MARKER_TAIL_BYTES + 1
    } else {
        candidate[..previous.len()].copy_from_slice(previous);
        candidate[previous.len()..previous.len() + bytes.len()].copy_from_slice(bytes);
        previous.len() + bytes.len()
    };
    marker_tail_suffix(&candidate[..candidate_len])
}

fn marker_tail_suffix(bytes: &[u8]) -> ([u8; MAX_MARKER_TAIL_BYTES], usize) {
    const OSC8_INTRODUCER: &[u8] = b"\x1b]8;";
    for len in (1..OSC8_INTRODUCER.len()).rev() {
        if bytes.ends_with(&OSC8_INTRODUCER[..len]) {
            let mut tail = [0; MAX_MARKER_TAIL_BYTES];
            tail[..len].copy_from_slice(&OSC8_INTRODUCER[..len]);
            return (tail, len);
        }
    }
    let earliest = bytes.len().saturating_sub(MAX_MARKER_TAIL_BYTES);
    for start in earliest..bytes.len() {
        if start > 0 && is_scheme_byte(bytes[start - 1]) {
            continue;
        }
        let candidate = &bytes[start..];
        let Some((&first, rest)) = candidate.split_first() else {
            continue;
        };
        if !first.is_ascii_alphabetic() {
            continue;
        }
        let scheme_len = rest
            .iter()
            .position(|byte| !is_scheme_byte(*byte))
            .map_or(candidate.len(), |index| index + 1);
        if scheme_len > MAX_SCHEME_BYTES {
            continue;
        }
        let remainder = &candidate[scheme_len..];
        if remainder.is_empty() || remainder == b":" || remainder == b":/" {
            let mut tail = [0; MAX_MARKER_TAIL_BYTES];
            tail[..candidate.len()].copy_from_slice(candidate);
            return (tail, candidate.len());
        }
    }
    ([0; MAX_MARKER_TAIL_BYTES], 0)
}

fn is_scheme_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'.' | b'-')
}

fn marker_tail_can_continue(tail: &[u8]) -> bool {
    tail.ends_with(b":")
        || tail.ends_with(b":/")
        || (!tail.is_empty() && b"\x1b]8;".starts_with(tail))
}

fn find_scheme_start(bytes: &[u8]) -> Option<usize> {
    memchr::memchr_iter(b':', bytes).find_map(|colon| {
        let scheme_start = scheme_start_at(bytes, colon)?;
        Some(
            bytes[..scheme_start]
                .windows(4)
                .rposition(|window| window == b"\x1b]8;")
                .unwrap_or(scheme_start),
        )
    })
}

fn find_osc8_start(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\x1b]8;")
}

fn scheme_start_at(bytes: &[u8], colon: usize) -> Option<usize> {
    if bytes.get(colon + 1..colon + 3) != Some(b"//") {
        return None;
    }
    let start = bytes[..colon]
        .iter()
        .rposition(|byte| !is_scheme_byte(*byte))
        .map_or(0, |index| index + 1);
    (colon.saturating_sub(start) <= MAX_SCHEME_BYTES
        && bytes.get(start).is_some_and(u8::is_ascii_alphabetic))
    .then_some(start)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentTask {
    pub text: String,
    pub status: AgentTaskStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentSubagentSource {
    Observed,
    Reported,
}

fn reported_source() -> AgentSubagentSource {
    AgentSubagentSource::Reported
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentSubagent {
    pub name: String,
    pub status: AgentStatus,
    #[serde(default)]
    pub last_active_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default = "reported_source")]
    pub source: AgentSubagentSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentLinkSource {
    Output,
    Osc8,
    Transcript,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentLink {
    pub url: String,
    pub domain: String,
    pub first_seen: String,
    pub last_seen: String,
    pub source: AgentLinkSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservedAgentLink {
    pub(crate) url: String,
    pub(crate) first_seen: SystemTime,
    pub(crate) last_seen: SystemTime,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentReportPayload {
    #[serde(default)]
    pub status_text: Option<String>,
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub tasks: Option<Vec<AgentTask>>,
    #[serde(default)]
    pub subagents: Vec<AgentSubagent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentStateSnapshot {
    pub status: AgentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_text: Option<String>,
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub last_acted_at: Option<String>,
    pub tasks: Vec<AgentTask>,
    pub subagents: Vec<AgentSubagent>,
    pub links: Vec<AgentLink>,
}

#[derive(Debug, Clone, Default)]
struct PaneAgentState {
    status_text: Option<String>,
    goal: Option<String>,
    last_report_at: Option<SystemTime>,
    last_working_at: Option<SystemTime>,
    last_transcript_at: Option<SystemTime>,
    reported_tasks: Vec<AgentTask>,
    reported_tasks_at: Option<SystemTime>,
    transcript_session_id: Option<String>,
    transcript_tasks: Option<Vec<AgentTask>>,
    transcript_tasks_at: Option<SystemTime>,
    reported_subagents: Vec<AgentSubagent>,
    observed_subagents: Vec<AgentSubagent>,
    links: Vec<StoredLink>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredLink {
    url: String,
    domain: String,
    first_seen: SystemTime,
    last_seen: SystemTime,
    source: AgentLinkSource,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AgentStateStore {
    panes: HashMap<PaneId, PaneAgentState>,
}

impl AgentStateStore {
    pub(crate) fn remove(&mut self, pane_id: PaneId) {
        self.panes.remove(&pane_id);
    }

    pub(crate) fn report(
        &mut self,
        pane_id: PaneId,
        payload: AgentReportPayload,
        observed_at: SystemTime,
    ) -> Result<(), String> {
        validate_report(&payload)?;

        let pane = self.panes.entry(pane_id).or_default();
        pane.status_text = payload.status_text;
        pane.goal = payload.goal;
        pane.last_report_at = Some(observed_at);
        if let Some(tasks) = payload.tasks {
            pane.reported_tasks = tasks;
            pane.reported_tasks_at = Some(observed_at);
        }
        pane.reported_subagents = payload
            .subagents
            .into_iter()
            .map(|mut subagent| {
                subagent.source = AgentSubagentSource::Reported;
                subagent
            })
            .collect();
        Ok(())
    }

    pub(crate) fn snapshot(&self, pane_id: PaneId, status: AgentStatus) -> AgentStateSnapshot {
        let Some(pane) = self.panes.get(&pane_id) else {
            return AgentStateSnapshot {
                status,
                status_text: None,
                goal: None,
                last_acted_at: None,
                tasks: Vec::new(),
                subagents: Vec::new(),
                links: Vec::new(),
            };
        };
        let last_acted_at = [
            pane.last_report_at,
            pane.last_working_at,
            pane.last_transcript_at,
        ]
        .into_iter()
        .flatten()
        .max()
        .and_then(format_rfc3339);
        let mut subagents = pane.observed_subagents.clone();
        subagents.extend(pane.reported_subagents.clone());
        AgentStateSnapshot {
            status,
            status_text: pane.status_text.clone(),
            goal: pane.goal.clone(),
            last_acted_at,
            tasks: if pane.transcript_tasks_at > pane.reported_tasks_at {
                pane.transcript_tasks.clone().unwrap_or_default()
            } else {
                pane.reported_tasks.clone()
            },
            subagents,
            links: pane
                .links
                .iter()
                .filter_map(|link| {
                    Some(AgentLink {
                        url: link.url.clone(),
                        domain: link.domain.clone(),
                        first_seen: format_rfc3339(link.first_seen)?,
                        last_seen: format_rfc3339(link.last_seen)?,
                        source: link.source,
                    })
                })
                .collect(),
        }
    }

    pub(crate) fn observe_working(&mut self, pane_id: PaneId, observed_at: SystemTime) {
        self.panes.entry(pane_id).or_default().last_working_at = Some(observed_at);
    }

    pub(crate) fn observe_transcript(
        &mut self,
        pane_id: PaneId,
        observed_at: Option<SystemTime>,
        tasks: Option<Vec<AgentTask>>,
        tasks_observed_at: Option<SystemTime>,
        subagents: Vec<AgentSubagent>,
        links: Vec<ObservedAgentLink>,
    ) -> bool {
        let pane = self.panes.entry(pane_id).or_default();
        let previous_last = pane.last_transcript_at;
        let previous_tasks = pane.transcript_tasks.clone();
        let previous_tasks_at = pane.transcript_tasks_at;
        let previous_subagents = pane.observed_subagents.clone();
        if let Some(observed_at) = observed_at {
            pane.last_transcript_at = Some(
                pane.last_transcript_at
                    .map_or(observed_at, |current| current.max(observed_at)),
            );
        }
        if let (Some(tasks), Some(tasks_observed_at)) = (tasks, tasks_observed_at) {
            pane.transcript_tasks = Some(tasks);
            pane.transcript_tasks_at = Some(tasks_observed_at);
        }
        pane.observed_subagents = subagents;
        let links_changed = observe_transcript_links_in_pane(pane, links);
        previous_last != pane.last_transcript_at
            || previous_tasks != pane.transcript_tasks
            || previous_tasks_at != pane.transcript_tasks_at
            || previous_subagents != pane.observed_subagents
            || links_changed
    }

    pub(crate) fn observe_transcript_session(&mut self, pane_id: PaneId, session_id: &str) -> bool {
        let pane = self.panes.entry(pane_id).or_default();
        if pane.transcript_session_id.as_deref() == Some(session_id) {
            return false;
        }
        pane.transcript_session_id = Some(session_id.to_owned());
        pane.transcript_tasks = Some(Vec::new());
        pane.transcript_tasks_at = None;
        true
    }

    pub(crate) fn observe_links(
        &mut self,
        pane_id: PaneId,
        urls: impl IntoIterator<Item = String>,
        source: AgentLinkSource,
        observed_at: SystemTime,
    ) {
        let _ = observe_links_in_pane(
            self.panes.entry(pane_id).or_default(),
            urls,
            source,
            observed_at,
        );
    }
}

fn validate_report(payload: &AgentReportPayload) -> Result<(), String> {
    if payload
        .tasks
        .as_deref()
        .unwrap_or_default()
        .iter()
        .any(|task| task.text.trim().is_empty() || task.text.chars().any(char::is_control))
    {
        return Err("task text must not be empty or contain control characters".into());
    }
    if payload.subagents.iter().any(|subagent| {
        subagent.name.trim().is_empty() || subagent.name.chars().any(char::is_control)
    }) {
        return Err("subagent name must not be empty or contain control characters".into());
    }
    if payload.subagents.iter().any(|subagent| {
        subagent
            .last_active_at
            .as_ref()
            .is_some_and(|value| parse_rfc3339(value).is_none())
    }) {
        return Err("subagent last_active_at must be an RFC3339 timestamp".into());
    }
    Ok(())
}

pub(crate) fn format_rfc3339(value: SystemTime) -> Option<String> {
    time::OffsetDateTime::from(value)
        .format(&time::format_description::well_known::Rfc3339)
        .ok()
}

pub(crate) fn parse_rfc3339(value: &str) -> Option<SystemTime> {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(SystemTime::from)
}

fn observe_links_in_pane(
    pane: &mut PaneAgentState,
    urls: impl IntoIterator<Item = String>,
    source: AgentLinkSource,
    observed_at: SystemTime,
) -> bool {
    let before = pane.links.clone();
    for url in urls {
        let Some(domain) = url_domain(&url) else {
            continue;
        };
        if let Some(existing) = pane.links.iter_mut().find(|link| link.url == url) {
            existing.last_seen = existing.last_seen.max(observed_at);
            continue;
        }
        pane.links.push(StoredLink {
            url,
            domain,
            first_seen: observed_at,
            last_seen: observed_at,
            source,
        });
    }
    pane.links.sort_by_key(|link| link.last_seen);
    if pane.links.len() > MAX_LINKS {
        pane.links.drain(..pane.links.len() - MAX_LINKS);
    }
    pane.links != before
}

fn observe_transcript_links_in_pane(
    pane: &mut PaneAgentState,
    links: Vec<ObservedAgentLink>,
) -> bool {
    let before = pane.links.clone();
    for link in links {
        let Some(domain) = url_domain(&link.url) else {
            continue;
        };
        if let Some(existing) = pane.links.iter_mut().find(|stored| stored.url == link.url) {
            existing.first_seen = existing.first_seen.min(link.first_seen);
            existing.last_seen = existing.last_seen.max(link.last_seen);
            continue;
        }
        pane.links.push(StoredLink {
            url: link.url,
            domain,
            first_seen: link.first_seen,
            last_seen: link.last_seen,
            source: AgentLinkSource::Transcript,
        });
    }
    pane.links.sort_by_key(|link| link.last_seen);
    if pane.links.len() > MAX_LINKS {
        pane.links.drain(..pane.links.len() - MAX_LINKS);
    }
    pane.links != before
}

static URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"[A-Za-z][A-Za-z0-9+.-]*://[^\s<>\"'\x00-\x1f]+"#).expect("static URL regex")
});
pub(crate) fn extract_urls(text: &str) -> Vec<String> {
    URL_RE
        .find_iter(text)
        .map(|matched| trim_url_suffix(matched.as_str()).to_string())
        .filter(|url| url_within_bounds(url) && url_domain(url).is_some())
        .collect()
}

fn url_within_bounds(url: &str) -> bool {
    url.len() <= MAX_URL_BYTES
        && url
            .split_once("://")
            .is_some_and(|(scheme, _)| scheme.len() <= MAX_SCHEME_BYTES)
}

fn trim_url_suffix(mut url: &str) -> &str {
    loop {
        let Some(last) = url.chars().next_back() else {
            return url;
        };
        if matches!(last, '.' | ',' | ';' | ':' | '!' | '?') {
            url = &url[..url.len() - last.len_utf8()];
            continue;
        }
        let opener = match last {
            ')' => '(',
            ']' => '[',
            '}' => '{',
            _ => return url,
        };
        if url.chars().filter(|character| *character == last).count()
            > url.chars().filter(|character| *character == opener).count()
        {
            url = &url[..url.len() - last.len_utf8()];
            continue;
        }
        return url;
    }
}

struct AgentLinkExtraction {
    links: ExtractedAgentLinks,
    unterminated_url: bool,
    incomplete_terminal_sequence: bool,
    tail_output_url: Option<String>,
}

enum ResumedOutputUrl {
    Completed {
        url: Option<String>,
        remaining: Vec<u8>,
        osc8_urls: Vec<String>,
    },
    Deferred {
        bytes: Vec<u8>,
        scan_at: usize,
        osc8_urls: Vec<String>,
    },
    Oversized {
        osc8_urls: Vec<String>,
    },
}

struct StrippedTerminalSequences {
    visible_bytes: Vec<u8>,
    osc8_urls: Vec<String>,
    incomplete_terminal_sequence_start: Option<(usize, OpenSequenceKind)>,
    visible_offsets: Vec<usize>,
}

fn resume_output_url(bytes: &[u8], scan_at: usize) -> ResumedOutputUrl {
    let continuation = &bytes[scan_at..];
    let stripped = strip_terminal_sequences(continuation);
    if let Some(boundary) = stripped
        .visible_bytes
        .iter()
        .position(|byte| is_url_terminator(*byte))
    {
        let raw_boundary = stripped.visible_offsets[boundary];
        let mut url_bytes = bytes[..scan_at].to_vec();
        url_bytes.extend_from_slice(&stripped.visible_bytes[..boundary]);
        let url = String::from_utf8_lossy(&url_bytes);
        let url = trim_url_suffix(&url);
        return ResumedOutputUrl::Completed {
            url: (url_within_bounds(url) && url_domain(url).is_some()).then(|| url.to_owned()),
            remaining: continuation[raw_boundary + 1..].to_vec(),
            osc8_urls: stripped.osc8_urls,
        };
    }

    let mut normalized = bytes[..scan_at].to_vec();
    normalized.extend_from_slice(&stripped.visible_bytes);
    let next_scan_at = normalized.len();
    if next_scan_at > MAX_URL_BYTES {
        return ResumedOutputUrl::Oversized {
            osc8_urls: stripped.osc8_urls,
        };
    }
    if let Some((incomplete_at, _)) = stripped.incomplete_terminal_sequence_start {
        normalized.extend_from_slice(&continuation[incomplete_at..]);
    }
    ResumedOutputUrl::Deferred {
        bytes: normalized,
        scan_at: next_scan_at,
        osc8_urls: stripped.osc8_urls,
    }
}

fn extract_agent_links(bytes: &[u8]) -> AgentLinkExtraction {
    let mut stripped = strip_terminal_sequences(bytes);
    let visible = String::from_utf8_lossy(&stripped.visible_bytes);
    let mut unterminated_url = false;
    let mut tail_output_url = None;
    let mut output_urls = URL_RE
        .find_iter(&visible)
        .filter_map(|matched| {
            let url = trim_url_suffix(matched.as_str()).to_string();
            let valid_url = url_within_bounds(&url) && url_domain(&url).is_some();
            if matched.end() == visible.len() {
                unterminated_url = true;
                tail_output_url = Some(url.clone());
                return None;
            }
            valid_url.then_some(url)
        })
        .collect::<Vec<_>>();
    if has_unterminated_url_candidate(&visible) {
        unterminated_url = true;
    }
    output_urls.sort_unstable();
    output_urls.dedup();
    stripped.osc8_urls.sort_unstable();
    stripped.osc8_urls.dedup();
    AgentLinkExtraction {
        links: ExtractedAgentLinks {
            output_urls,
            osc8_urls: stripped.osc8_urls,
        },
        unterminated_url,
        incomplete_terminal_sequence: stripped.incomplete_terminal_sequence_start.is_some(),
        tail_output_url,
    }
}

fn has_unterminated_url_candidate(text: &str) -> bool {
    let tail_start = text
        .char_indices()
        .rev()
        .find(|(_, character)| {
            character.is_whitespace()
                || character.is_control()
                || matches!(character, '"' | '\'' | '<' | '>')
        })
        .map_or(0, |(index, character)| index + character.len_utf8());
    find_scheme_start(&text.as_bytes()[tail_start..]).is_some()
}

fn strip_terminal_sequences(bytes: &[u8]) -> StrippedTerminalSequences {
    let mut visible_bytes = Vec::with_capacity(bytes.len());
    let mut visible_offsets = Vec::with_capacity(bytes.len());
    let mut osc8_urls = Vec::new();
    let mut incomplete_terminal_sequence_start = None;
    let mut offset = 0;
    while offset < bytes.len() {
        if bytes[offset] != b'\x1b' {
            visible_bytes.push(bytes[offset]);
            visible_offsets.push(offset);
            offset += 1;
            continue;
        }

        match bytes.get(offset + 1).copied() {
            Some(b'[') => {
                let Some(end) = bytes[offset + 2..]
                    .iter()
                    .position(|byte| (0x40..=0x7e).contains(byte))
                    .map(|relative| offset + 2 + relative + 1)
                else {
                    incomplete_terminal_sequence_start = Some((offset, OpenSequenceKind::Csi));
                    break;
                };
                offset = end;
            }
            Some(b']') => {
                let content_start = offset + 2;
                let Some((relative_end, terminator_len)) = osc_terminator(&bytes[content_start..])
                else {
                    incomplete_terminal_sequence_start = Some((offset, OpenSequenceKind::Osc));
                    break;
                };
                let content_end = content_start + relative_end;
                let content = &bytes[content_start..content_end];
                if let Some(osc8) = content.strip_prefix(b"8;") {
                    if let Some(parameter_end) = osc8.iter().position(|byte| *byte == b';') {
                        let target = &osc8[parameter_end + 1..];
                        if target.len() <= MAX_URL_BYTES {
                            osc8_urls.extend(extract_urls(&String::from_utf8_lossy(target)));
                        }
                    }
                }
                offset = content_end + terminator_len;
            }
            Some(b'(' | b')' | b'*' | b'+' | b'-' | b'.' | b'/') => {
                if offset + 2 >= bytes.len() {
                    incomplete_terminal_sequence_start = Some((offset, OpenSequenceKind::Escape));
                    break;
                }
                offset += 3;
            }
            Some(_) => offset += 2,
            None => {
                incomplete_terminal_sequence_start = Some((offset, OpenSequenceKind::Escape));
                break;
            }
        }
    }
    StrippedTerminalSequences {
        visible_bytes,
        osc8_urls,
        incomplete_terminal_sequence_start,
        visible_offsets,
    }
}

fn osc_terminator(bytes: &[u8]) -> Option<(usize, usize)> {
    bytes.iter().enumerate().find_map(|(index, byte)| {
        if *byte == b'\x07' {
            Some((index, 1))
        } else if *byte == b'\x1b' && bytes.get(index + 1) == Some(&b'\\') {
            Some((index, 2))
        } else {
            None
        }
    })
}

fn url_domain(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit('@').next()?;
    let host = if host_port.starts_with('[') {
        host_port.split_once(']')?.0.trim_start_matches('[')
    } else {
        host_port.split(':').next()?
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_round_trips_goal_status_tasks_and_subagents() {
        let pane_id = PaneId::from_raw(7);
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let mut store = AgentStateStore::default();
        store
            .report(
                pane_id,
                AgentReportPayload {
                    status_text: Some("checking tests".into()),
                    goal: Some("ship MAT-160".into()),
                    tasks: Some(vec![AgentTask {
                        text: "run focused tests".into(),
                        status: AgentTaskStatus::InProgress,
                    }]),
                    subagents: vec![AgentSubagent {
                        name: "reviewer".into(),
                        status: AgentStatus::Working,
                        last_active_at: None,
                        pane_id: None,
                        source: AgentSubagentSource::Reported,
                    }],
                },
                now,
            )
            .expect("valid report");

        let state = store.snapshot(pane_id, AgentStatus::Working);
        assert_eq!(state.goal.as_deref(), Some("ship MAT-160"));
        assert_eq!(state.status_text.as_deref(), Some("checking tests"));
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.subagents.len(), 1);
        assert_eq!(state.status, AgentStatus::Working);
        assert!(state.last_acted_at.is_some());
    }

    #[test]
    fn last_acted_at_uses_latest_report_working_or_transcript_observation() {
        let pane_id = PaneId::from_raw(8);
        let base = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let mut store = AgentStateStore::default();
        store
            .report(pane_id, AgentReportPayload::default(), base)
            .expect("valid report");
        store.observe_working(pane_id, base + std::time::Duration::from_secs(5));
        store.observe_transcript(
            pane_id,
            Some(base + std::time::Duration::from_secs(10)),
            None,
            None,
            Vec::new(),
            Vec::new(),
        );

        assert_eq!(
            store
                .snapshot(pane_id, AgentStatus::Idle)
                .last_acted_at
                .as_deref(),
            format_rfc3339(base + std::time::Duration::from_secs(10)).as_deref()
        );
    }

    #[test]
    fn chunks_without_scheme_marker_do_not_trigger_link_extraction() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"ordinary terminal output");
        assert!(!gate.take_dirty());
        assert_eq!(gate.extraction_count(), 0);

        gate.observe_chunk(b"visit https://example.test/path\n");
        assert!(gate.take_dirty());
        assert_eq!(gate.extraction_count(), 1);
    }

    #[test]
    fn marker_free_chunk_does_not_take_pending_lock() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"ordinary terminal output without a marker");
        assert_eq!(gate.lock_acquisition_count(), 0);

        gate.observe_chunk(b"visit https://example.test");
        assert_eq!(gate.lock_acquisition_count(), 1);
        gate.observe_chunk(b"/continued\n");
        assert_eq!(gate.lock_acquisition_count(), 2);
    }

    #[test]
    fn pty_callback_only_queues_bytes_and_parser_runs_once_outside_processing_lock() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"https://outside-lock.example");

        assert_eq!(gate.parse_passes.load(Ordering::Relaxed), 0);
        assert!(gate.take_links().is_none());
        assert_eq!(gate.parse_passes.load(Ordering::Relaxed), 1);
        assert_eq!(gate.parses_outside_processing.load(Ordering::Relaxed), 1);

        gate.observe_chunk(b"/path\n");

        assert_eq!(gate.parse_passes.load(Ordering::Relaxed), 1);
        let links = gate.take_links().expect("completed link");
        assert_eq!(links.output_urls, vec!["https://outside-lock.example/path"]);
        assert_eq!(gate.parse_passes.load(Ordering::Relaxed), 2);
        assert_eq!(gate.parses_outside_processing.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn scheme_marker_split_across_chunks_triggers_link_extraction() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"https:");
        assert!(!gate.take_dirty());

        gate.observe_chunk(b"//split.example.test/path\n");
        assert!(gate.take_dirty());
        assert_eq!(gate.extraction_count(), 1);
    }

    #[test]
    fn scheme_prefix_accumulates_across_three_chunks() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"ht");
        gate.observe_chunk(b"tps:");
        gate.observe_chunk(b"//split.example.test/path\n");

        let links = gate.take_links().expect("link extraction");
        assert_eq!(links.output_urls, vec!["https://split.example.test/path"]);
    }

    #[test]
    fn long_scheme_prefixes_survive_chunk_boundaries() {
        let colon_split = LinkExtractionGate::default();
        colon_split.observe_chunk(b"postgresql:");
        colon_split.observe_chunk(b"//db.example/path\n");
        assert_eq!(
            colon_split
                .take_links()
                .expect("colon-split URL")
                .output_urls,
            vec!["postgresql://db.example/path"]
        );

        let scheme_split = LinkExtractionGate::default();
        scheme_split.observe_chunk(b"customscheme");
        scheme_split.observe_chunk(b"://longscheme.example.test/path\n");
        assert_eq!(
            scheme_split
                .take_links()
                .expect("scheme-split URL")
                .output_urls,
            vec!["customscheme://longscheme.example.test/path"]
        );

        let mut max_scheme = "a".to_string();
        while max_scheme.len() < MAX_SCHEME_BYTES {
            max_scheme.push_str("1+.-");
        }
        max_scheme.truncate(MAX_SCHEME_BYTES);
        let max_split = LinkExtractionGate::default();
        max_split.observe_chunk(max_scheme.as_bytes());
        max_split.observe_chunk(b"://maxscheme.example.test/path\n");
        assert_eq!(
            max_split
                .take_links()
                .expect("maximum-length scheme URL")
                .output_urls,
            vec![format!("{max_scheme}://maxscheme.example.test/path")]
        );
    }

    #[test]
    fn unfinished_url_waits_for_terminator_before_extraction() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"https://split.example.test");
        assert!(gate.take_links().is_none());

        gate.observe_chunk(b"/path\n");
        let links = gate.take_links().expect("terminated link extraction");
        assert_eq!(links.output_urls, vec!["https://split.example.test/path"]);
    }

    #[test]
    fn unfinished_url_is_not_published_after_a_quiet_period() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"https://quiet.example.test/path");

        assert!(gate.take_links().is_none());

        gate.observe_chunk(b"\n");
        assert_eq!(
            gate.take_links(),
            Some(ExtractedAgentLinks {
                output_urls: vec!["https://quiet.example.test/path".into()],
                osc8_urls: Vec::new(),
            })
        );
    }

    #[test]
    fn split_osc8_before_scheme_stays_an_osc8_link() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"\x1b]8;;htt");
        gate.observe_chunk(b"ps://osc.example.test/path\x1b\\label\x1b]8;;\x1b\\\n");

        let links = gate.take_links().expect("completed OSC 8 link");
        assert!(links.output_urls.is_empty());
        assert_eq!(links.osc8_urls, vec!["https://osc.example.test/path"]);
    }

    #[test]
    fn partial_osc8_introducer_carries_across_chunks() {
        for (first, second) in [
            (
                b"\x1b]8".as_slice(),
                b";;https://osc.example.test/from-eight\x1b\\label\n".as_slice(),
            ),
            (
                b"\x1b]8;".as_slice(),
                b";https://osc.example.test/from-semicolon\x1b\\label\n".as_slice(),
            ),
        ] {
            let gate = LinkExtractionGate::default();
            gate.observe_chunk(first);
            assert!(gate.take_links().is_none());
            gate.observe_chunk(second);

            let links = gate.take_links().expect("split OSC 8 link");
            assert!(links.output_urls.is_empty());
            assert_eq!(links.osc8_urls.len(), 1);
            assert!(links.osc8_urls[0].starts_with("https://osc.example.test/"));
        }
    }

    #[test]
    fn osc8_target_bound_excludes_split_and_unsplit_st_bytes() {
        let prefix = "https://osc.example/";
        for split_st in [false, true] {
            for extra_bytes in [0, 1] {
                let uri = format!(
                    "{prefix}{}",
                    "a".repeat(MAX_URL_BYTES + extra_bytes - prefix.len())
                );
                let gate = LinkExtractionGate::default();
                if split_st {
                    gate.observe_chunk(format!("\x1b]8;;{uri}\x1b").as_bytes());
                    assert!(gate.take_links().is_none());
                    gate.observe_chunk(b"\\label\x1b]8;;\x1b\\\n");
                } else {
                    gate.observe_chunk(
                        format!("\x1b]8;;{uri}\x1b\\label\x1b]8;;\x1b\\\n").as_bytes(),
                    );
                }

                let links = gate.take_links();
                if extra_bytes == 0 {
                    let links = links.unwrap_or_else(|| {
                        panic!("maximum-length OSC 8 target, split ST: {split_st}")
                    });
                    assert!(links.output_urls.is_empty());
                    assert_eq!(links.osc8_urls, vec![uri], "split ST: {split_st}");
                } else {
                    assert!(links.is_none(), "split ST: {split_st}");
                }
            }
        }
    }

    #[test]
    fn completed_link_is_not_blocked_by_open_oversized_url() {
        let gate = LinkExtractionGate::default();
        let mut chunk = b"https://complete.example.test/path\nhttps://oversized.example/".to_vec();
        chunk.extend(std::iter::repeat_n(b'a', MAX_URL_BYTES));
        gate.observe_chunk(&chunk);

        let links = gate.take_links().expect("completed output link");
        assert_eq!(
            links.output_urls,
            vec!["https://complete.example.test/path"]
        );
        assert!(links.osc8_urls.is_empty());

        gate.observe_chunk(b"\n");
        assert!(gate.take_links().is_none());
    }

    #[test]
    fn url_and_scheme_bounds_are_enforced_without_prefix_publication() {
        let exact_url = format!("https://bounds.example/{}", "a".repeat(MAX_URL_BYTES - 23));
        assert_eq!(exact_url.len(), MAX_OPEN_SEQUENCE_BYTES);
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(format!("{exact_url}\n").as_bytes());
        assert_eq!(
            gate.take_links().expect("maximum-length URL").output_urls,
            vec![exact_url]
        );

        let overlong_url = format!(
            "https://bounds.example/{}\n",
            "b".repeat(MAX_PENDING_LINK_BYTES)
        );
        gate.observe_chunk(overlong_url.as_bytes());
        assert!(gate.take_links().is_none());

        let overlong_scheme = format!(
            "{}://scheme.example/path\n",
            "a".repeat(MAX_SCHEME_BYTES + 1)
        );
        gate.observe_chunk(overlong_scheme.as_bytes());
        assert!(gate.take_links().is_none());
    }

    #[test]
    fn colon_without_scheme_marker_does_not_take_pending_lock() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"ordinary: terminal output / without a URL");
        assert_eq!(gate.lock_acquisition_count(), 0);
    }

    #[test]
    fn repeated_output_url_keeps_first_seen_and_advances_only_last_seen() {
        let pane_id = PaneId::from_raw(84);
        let first_seen = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let gate = LinkExtractionGate::default();
        let mut store = AgentStateStore::default();

        gate.observe_chunk(b"https://same.example/path\n");
        store.observe_links(
            pane_id,
            gate.take_links().expect("first print").output_urls,
            AgentLinkSource::Output,
            first_seen,
        );

        gate.observe_chunk(b"https://same.example/path");
        assert!(gate.take_links().is_none());
        gate.observe_chunk(b"\n");
        store.observe_links(
            pane_id,
            gate.take_links().expect("delayed terminator").output_urls,
            AgentLinkSource::Output,
            first_seen + std::time::Duration::from_secs(2),
        );

        gate.observe_chunk(b"https://same.example/path\n");
        store.observe_links(
            pane_id,
            gate.take_links().expect("third print").output_urls,
            AgentLinkSource::Output,
            first_seen + std::time::Duration::from_secs(3),
        );

        let links = store.snapshot(pane_id, AgentStatus::Idle).links;
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].first_seen, format_rfc3339(first_seen).unwrap());
        assert_eq!(
            links[0].last_seen,
            format_rfc3339(first_seen + std::time::Duration::from_secs(3)).unwrap()
        );
    }

    #[test]
    fn interleaved_output_and_osc8_urls_keep_independent_timestamps() {
        let pane_id = PaneId::from_raw(85);
        let first_seen = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let gate = LinkExtractionGate::default();
        let mut store = AgentStateStore::default();

        gate.observe_chunk(b"https://open.example/path");
        assert!(gate.take_links().is_none());
        gate.observe_chunk(b"\x1b]8;;https://interleaved.example/complete\x1b\\\x1b]8;;\x1b\\");
        let completed = gate.take_links().expect("completed OSC 8 link");
        assert!(completed.output_urls.is_empty());
        store.observe_links(
            pane_id,
            completed.osc8_urls,
            AgentLinkSource::Osc8,
            first_seen,
        );

        gate.observe_chunk(b"/tail\n");
        let open = gate.take_links().expect("terminated output link");
        assert!(open.osc8_urls.is_empty());
        store.observe_links(
            pane_id,
            open.output_urls,
            AgentLinkSource::Output,
            first_seen + std::time::Duration::from_secs(2),
        );

        let links = store.snapshot(pane_id, AgentStatus::Idle).links;
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].url, "https://interleaved.example/complete");
        assert_eq!(links[0].first_seen, format_rfc3339(first_seen).unwrap());
        assert_eq!(links[0].last_seen, links[0].first_seen);
        assert_eq!(links[0].source, AgentLinkSource::Osc8);
        assert_eq!(links[1].url, "https://open.example/path/tail");
        assert_eq!(
            links[1].first_seen,
            format_rfc3339(first_seen + std::time::Duration::from_secs(2)).unwrap()
        );
        assert_eq!(links[1].last_seen, links[1].first_seen);
        assert_eq!(links[1].source, AgentLinkSource::Output);
    }

    #[test]
    fn scheme_only_tail_survives_a_detection_tick_between_chunks() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"https://");

        assert!(gate.take_links().is_none());

        gate.observe_chunk(b"split.example.test/path\n");
        let links = gate.take_links().expect("terminated link extraction");
        assert_eq!(links.output_urls, vec!["https://split.example.test/path"]);
    }

    #[test]
    fn completed_url_batch_carries_trailing_scheme_into_next_chunk() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"https://split.example.test");
        assert!(gate.take_links().is_none());

        gate.observe_chunk(b"/path\npostgresql:");
        let completed = gate.take_links().expect("completed URL publication");
        assert_eq!(
            completed.output_urls,
            vec!["https://split.example.test/path"]
        );

        gate.observe_chunk(b"//db.example/path\n");
        let database = gate.take_links().expect("scheme continuation publication");
        assert_eq!(database.output_urls, vec!["postgresql://db.example/path"]);

        let pane_id = PaneId::from_raw(83);
        let first_seen = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let mut store = AgentStateStore::default();
        store.observe_links(
            pane_id,
            completed.output_urls,
            AgentLinkSource::Output,
            first_seen + std::time::Duration::from_millis(300),
        );
        store.observe_links(
            pane_id,
            database.output_urls,
            AgentLinkSource::Output,
            first_seen + std::time::Duration::from_millis(400),
        );

        let urls = store
            .snapshot(pane_id, AgentStatus::Idle)
            .links
            .into_iter()
            .map(|link| link.url)
            .collect::<Vec<_>>();
        assert_eq!(
            urls,
            vec![
                "https://split.example.test/path",
                "postgresql://db.example/path",
            ]
        );
    }

    #[test]
    fn styled_url_ignores_csi_osc_and_charset_select_sequences() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(
            concat!(
                "https://sty",
                "\x1b[31m",
                "led.",
                "\x1b]0;terminal title\x07",
                "example.test",
                "\x1b(B",
                "/path\n"
            )
            .as_bytes(),
        );

        let links = gate.take_links().expect("styled link extraction");
        assert_eq!(links.output_urls, vec!["https://styled.example.test/path"]);
    }

    #[test]
    fn incomplete_osc8_target_resumes_when_terminator_arrives_without_colon() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"\x1b]8;;https://osc.example.test/path");
        assert!(gate.take_links().is_none());

        gate.observe_chunk(b"\x07label\x1b]8;;\x07\n");
        let links = gate.take_links().expect("completed OSC 8 sequence");
        assert!(links.output_urls.is_empty());
        assert_eq!(links.osc8_urls, vec!["https://osc.example.test/path"]);
    }

    #[test]
    fn pending_buffer_cap_extracts_then_keeps_scanning_later_output() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"https://before-cap.example.test/path\n");
        gate.observe_chunk(&vec![b'x'; MAX_PENDING_LINK_BYTES]);
        gate.observe_chunk(b"\nhttps://after-cap.example.test/path\n");

        let links = gate.take_links().expect("bounded link extraction");
        assert_eq!(
            links.output_urls,
            vec![
                "https://after-cap.example.test/path",
                "https://before-cap.example.test/path",
            ]
        );
    }

    #[test]
    fn pending_buffer_cap_keeps_unterminated_url_from_its_scheme() {
        let gate = LinkExtractionGate::default();
        let first = b"https://before-cap.example.test/path\n";
        gate.observe_chunk(first);

        let mut open_url = b"https://at-cap.example.test/".to_vec();
        open_url.resize(1_024, b'a');
        let filler_len = MAX_PENDING_LINK_BYTES - first.len() - open_url.len();
        let mut filler = vec![b'x'; filler_len];
        *filler.last_mut().expect("non-empty filler") = b'\n';
        gate.observe_chunk(&filler);
        gate.observe_chunk(&open_url);
        gate.observe_chunk(b"/path\n");

        let mut completed_url = String::from_utf8(open_url).unwrap();
        completed_url.push_str("/path");
        let links = gate.take_links().expect("bounded link extraction");
        assert_eq!(
            links.output_urls,
            vec![
                completed_url,
                "https://before-cap.example.test/path".to_string(),
            ]
        );
    }

    #[test]
    fn ingest_bounds_staging_and_drops_a_cut_url_whole() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"https://oversized.example/");
        gate.observe_chunk(&vec![b'a'; MAX_PENDING_LINK_BYTES]);

        assert_eq!(gate.staged_byte_count(), MAX_PENDING_LINK_BYTES);
        assert!(gate.take_links().is_none());

        gate.observe_chunk(b"\nhttps://after-overflow.example/path\n");
        assert_eq!(
            gate.take_links().expect("post-overflow URL").output_urls,
            vec!["https://after-overflow.example/path"]
        );
    }

    #[test]
    fn ingest_extracts_url_wholly_contained_in_evicted_middle() {
        let gate = LinkExtractionGate::default();
        let evicted_url = b"https://evicted.example/path";
        let evicted_at = STAGING_WINDOW_BYTES + 128;
        let mut chunk = b"https://before-overflow.example/path\n".to_vec();
        chunk.resize(evicted_at, b'x');
        chunk.push(b' ');
        chunk.extend_from_slice(evicted_url);
        chunk.push(b'\n');
        chunk.resize(MAX_PENDING_LINK_BYTES + 4_096, b'y');

        gate.observe_chunk(&chunk);

        assert_eq!(gate.staged_byte_count(), MAX_PENDING_LINK_BYTES);
        assert_eq!(
            gate.take_links()
                .expect("links spanning staging windows")
                .output_urls,
            vec![
                "https://before-overflow.example/path",
                "https://evicted.example/path",
            ]
        );
    }

    #[test]
    fn ingest_extracts_evicted_url_split_across_observe_chunks() {
        let gate = LinkExtractionGate::default();
        let evicted_url = b"https://evicted-in-pieces.example/path";
        let mut first = b"https://before-piecewise-overflow.example/path\n".to_vec();
        first.resize(STAGING_WINDOW_BYTES + 40, b'x');
        first.push(b'\n');
        first.extend_from_slice(evicted_url);
        first.push(b'\n');
        first.resize(MAX_PENDING_LINK_BYTES, b'y');

        gate.observe_chunk(&first);
        gate.observe_chunk(&[b'z'; 64]);
        gate.observe_chunk(&[b'z'; 64]);

        assert_eq!(gate.staged_byte_count(), MAX_PENDING_LINK_BYTES);
        assert_eq!(
            gate.take_links()
                .expect("piecewise-evicted link")
                .output_urls,
            vec![
                "https://before-piecewise-overflow.example/path",
                "https://evicted-in-pieces.example/path",
            ]
        );
    }

    #[test]
    fn overflow_suffix_resumes_after_open_osc_terminator() {
        for split_st in [false, true] {
            let gate = LinkExtractionGate::default();
            let first = b"https://before-overflow.example/path\n";
            gate.observe_chunk(first);

            let osc = b"\x1b]0;title";
            let mut overflow = vec![b'x'; STAGING_WINDOW_BYTES - first.len() - osc.len()];
            overflow.extend_from_slice(osc);
            overflow.resize(MAX_PENDING_LINK_BYTES + 256, b'y');
            overflow.extend_from_slice(b" hidden https://hidden.example/path ");
            if split_st {
                overflow.push(b'\x1b');
                gate.observe_chunk(&overflow);
                gate.observe_chunk(b"\\https://visible.example/path\n");
            } else {
                overflow.extend_from_slice(b"\x07https://visible.example/path\n");
                gate.observe_chunk(&overflow);
            }

            assert_eq!(gate.staged_byte_count(), MAX_PENDING_LINK_BYTES);
            let links = gate.take_links().expect("visible URL after open OSC");
            assert_eq!(
                links.output_urls,
                vec![
                    "https://before-overflow.example/path",
                    "https://visible.example/path",
                ],
                "split ST: {split_st}"
            );
        }
    }

    #[test]
    fn unfinished_url_scans_each_byte_only_a_constant_number_of_times() {
        let gate = LinkExtractionGate::default();
        let prefix = b"https://incremental.example/";
        gate.observe_chunk(prefix);
        assert!(gate.take_links().is_none());

        for _ in 0..128 {
            gate.observe_chunk(b"a");
            assert!(gate.take_links().is_none());
        }

        let bytes_before_terminator = prefix.len() + 128;
        assert!(
            gate.scanned_bytes.load(Ordering::Relaxed) as usize <= bytes_before_terminator,
            "unfinished URL was rescanned from its first byte"
        );
        let scanned_before_terminator = gate.scanned_bytes.load(Ordering::Relaxed);

        gate.observe_chunk(b"\n");
        let links = gate.take_links().expect("terminated incremental URL");
        assert_eq!(links.output_urls.len(), 1);
        assert!(
            gate.scanned_bytes.load(Ordering::Relaxed) - scanned_before_terminator <= 1,
            "terminator reparsed the retained URL"
        );
    }

    #[test]
    fn styled_unfinished_url_does_not_restart_after_each_split_escape() {
        let gate = LinkExtractionGate::default();
        let prefix = b"https://styled-incremental.example/";
        gate.observe_chunk(prefix);
        assert!(gate.take_links().is_none());

        for _ in 0..64 {
            gate.observe_chunk(b"\x1b[");
            assert!(gate.take_links().is_none());
            gate.observe_chunk(b"31m");
            assert!(gate.take_links().is_none());
            gate.observe_chunk(b"a");
            assert!(gate.take_links().is_none());
        }

        let received_bytes = prefix.len() + 64 * 6;
        assert!(
            gate.scanned_bytes.load(Ordering::Relaxed) as usize <= received_bytes * 2,
            "styled URL was reparsed from its first byte after split escapes"
        );

        gate.observe_chunk(b"\n");
        let links = gate.take_links().expect("terminated styled URL");
        assert_eq!(
            links.output_urls,
            vec![format!(
                "https://styled-incremental.example/{}",
                "a".repeat(64)
            )]
        );
    }

    #[test]
    fn split_scheme_continuation_waits_for_marker_finalization() {
        let gate = LinkExtractionGate::default();
        let arrived = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        gate.set_observe_race_hook(Arc::clone(&arrived), Arc::clone(&resume));

        std::thread::scope(|scope| {
            let processing = gate.processing.lock().expect("processing lock");
            let observer = scope.spawn(|| gate.observe_chunk(b"//db.example/path\n"));
            arrived.wait();
            gate.marker_tail.store(b"postgresql:");
            resume.wait();
            drop(processing);
            observer.join().expect("observer thread");
        });

        gate.observe_race_hook.lock().expect("hook lock").take();
        let links = gate.take_links().expect("split URL after finalization");
        assert_eq!(links.output_urls, vec!["postgresql://db.example/path"]);
    }

    #[test]
    fn overlong_csi_recovers_at_csi_terminator_and_keeps_later_url() {
        let gate = LinkExtractionGate::default();
        let first = b"https://before-csi.example.test/path\n";
        gate.observe_chunk(first);

        let csi_len = MAX_OPEN_SEQUENCE_BYTES + 1_024;
        let filler_len = MAX_PENDING_LINK_BYTES - first.len() - csi_len;
        let mut chunk = vec![b'x'; filler_len];
        *chunk.last_mut().expect("non-empty filler") = b'\n';
        chunk.extend_from_slice(b"\x1b[");
        chunk.resize(chunk.len() + csi_len - 2, b'1');
        gate.observe_chunk(&chunk);
        gate.observe_chunk(b"1");
        assert_eq!(
            gate.take_links()
                .expect("completed link before open CSI")
                .output_urls,
            vec!["https://before-csi.example.test/path"]
        );
        gate.observe_chunk(b"m\nhttps://after-csi.example.test/path\n");

        let links = gate.take_links().expect("link extraction after long CSI");
        assert_eq!(
            links.output_urls,
            vec!["https://after-csi.example.test/path"]
        );
        assert!(links.osc8_urls.is_empty());
    }

    #[test]
    fn overlong_osc8_target_is_dropped_before_later_plain_url() {
        let gate = LinkExtractionGate::default();
        let first = b"https://before-osc.example.test/path\n";
        gate.observe_chunk(first);

        let osc_len = MAX_OPEN_SEQUENCE_BYTES + 1_024;
        let filler_len = MAX_PENDING_LINK_BYTES - first.len() - osc_len;
        let mut chunk = vec![b'x'; filler_len];
        *chunk.last_mut().expect("non-empty filler") = b'\n';
        let mut control = b"\x1b]8;;https://too-long.example.test/".to_vec();
        control.resize(osc_len, b'a');
        chunk.extend_from_slice(&control);
        gate.observe_chunk(&chunk);
        gate.observe_chunk(b"\x1b");
        assert_eq!(
            gate.take_links()
                .expect("completed link before open OSC")
                .output_urls,
            vec!["https://before-osc.example.test/path"]
        );
        gate.observe_chunk(b"\\\nhttps://after-osc.example.test/path\n");

        let links = gate.take_links().expect("link extraction after long OSC 8");
        assert_eq!(
            links.output_urls,
            vec!["https://after-osc.example.test/path"]
        );
        assert!(links.osc8_urls.is_empty());
    }

    #[test]
    fn overlong_osc_discard_preserves_trailing_escape_for_split_st() {
        let gate = LinkExtractionGate::default();
        let mut control = b"\x1b]8;;https://too-long.example.test/".to_vec();
        control.resize(5 + MAX_URL_BYTES + 1, b'a');
        control.push(b'\x1b');
        gate.observe_chunk(&control);

        assert!(gate.take_links().is_none());
        gate.observe_chunk(b"\\\nhttps://after.example/path\n");

        let links = gate.take_links().expect("link extraction after split ST");
        assert_eq!(links.output_urls, vec!["https://after.example/path"]);
        assert!(links.osc8_urls.is_empty());
    }

    #[test]
    fn pending_rollover_preserves_trailing_escape_for_split_st() {
        let gate = LinkExtractionGate::default();
        let mut chunk = b"\x1b]8;;https://too-long.example.test/".to_vec();
        chunk.resize(MAX_PENDING_LINK_BYTES, b'a');
        *chunk.last_mut().expect("non-empty pending buffer") = b'\x1b';
        chunk.extend_from_slice(b"\\\nhttps://after-rollover.example/path\n");

        gate.observe_chunk(&chunk);

        let links = gate
            .take_links()
            .expect("link extraction after pending rollover");
        assert_eq!(
            links.output_urls,
            vec!["https://after-rollover.example/path"]
        );
        assert!(links.osc8_urls.is_empty());
    }

    #[test]
    fn links_dedupe_update_last_seen_and_drop_oldest_over_limit() {
        let pane_id = PaneId::from_raw(9);
        let base = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let mut store = AgentStateStore::default();
        store.observe_links(
            pane_id,
            ["https://example.test/first".into()],
            AgentLinkSource::Output,
            base,
        );
        store.observe_links(
            pane_id,
            ["https://example.test/first".into()],
            AgentLinkSource::Output,
            base + std::time::Duration::from_secs(1),
        );
        let state = store.snapshot(pane_id, AgentStatus::Idle);
        assert_eq!(state.links.len(), 1);
        assert_ne!(state.links[0].first_seen, state.links[0].last_seen);

        for index in 0..=MAX_LINKS {
            store.observe_links(
                pane_id,
                [format!("https://links.test/{index}")],
                AgentLinkSource::Osc8,
                base + std::time::Duration::from_secs(index as u64 + 2),
            );
        }
        let state = store.snapshot(pane_id, AgentStatus::Idle);
        assert_eq!(state.links.len(), MAX_LINKS);
        assert!(state
            .links
            .iter()
            .all(|link| link.url != "https://example.test/first"));
        assert!(state.links.iter().any(|link| {
            link.url == format!("https://links.test/{MAX_LINKS}")
                && link.source == AgentLinkSource::Osc8
        }));
    }

    #[test]
    fn transcript_link_preserves_its_row_times_across_later_rows() {
        let pane_id = PaneId::from_raw(12);
        let first = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let last = first + std::time::Duration::from_secs(3);
        let mut store = AgentStateStore::default();
        let link = ObservedAgentLink {
            url: "https://transcript.test/task".into(),
            first_seen: first,
            last_seen: last,
        };
        store.observe_transcript(
            pane_id,
            Some(last),
            None,
            None,
            Vec::new(),
            vec![link.clone()],
        );
        store.observe_transcript(
            pane_id,
            Some(last + std::time::Duration::from_secs(30)),
            None,
            None,
            Vec::new(),
            vec![link],
        );

        let state = store.snapshot(pane_id, AgentStatus::Idle);
        assert_eq!(state.links.len(), 1);
        assert_eq!(state.links[0].source, AgentLinkSource::Transcript);
        assert_eq!(state.links[0].first_seen, format_rfc3339(first).unwrap());
        assert_eq!(state.links[0].last_seen, format_rfc3339(last).unwrap());
    }

    #[test]
    fn url_extraction_trims_terminal_punctuation() {
        assert_eq!(
            extract_urls("open (https://example.test/path), then git://host/repo."),
            vec!["https://example.test/path", "git://host/repo"]
        );
    }

    #[test]
    fn url_extraction_preserves_balanced_trailing_delimiters() {
        assert_eq!(
            extract_urls(concat!(
                "https://en.wikipedia.org/wiki/Function_(mathematics) ",
                "https://example.test/unbalanced), ",
                "https://example.test/list[item] ",
                "https://example.test/trailing]."
            )),
            vec![
                "https://en.wikipedia.org/wiki/Function_(mathematics)",
                "https://example.test/unbalanced",
                "https://example.test/list[item]",
                "https://example.test/trailing",
            ]
        );
    }

    #[test]
    fn transcript_url_extraction_applies_url_and_scheme_bounds() {
        let valid = format!(
            "https://transcript.example/{}",
            "a".repeat(MAX_URL_BYTES - 27)
        );
        let too_long = format!("https://transcript.example/{}", "b".repeat(MAX_URL_BYTES));
        let long_scheme = format!("{}://transcript.example/path", "s".repeat(65));

        assert_eq!(
            extract_urls(&format!("{valid}\n{too_long}\n{long_scheme}")),
            vec![valid]
        );
    }

    #[test]
    fn newer_reported_tasks_override_older_transcript_tasks() {
        let pane_id = PaneId::from_raw(13);
        let base = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let transcript_task = AgentTask {
            text: "old transcript task".into(),
            status: AgentTaskStatus::Pending,
        };
        let reported_task = AgentTask {
            text: "new reported task".into(),
            status: AgentTaskStatus::InProgress,
        };
        let mut store = AgentStateStore::default();
        store.observe_transcript(
            pane_id,
            Some(base + std::time::Duration::from_secs(2)),
            Some(vec![transcript_task]),
            Some(base),
            Vec::new(),
            Vec::new(),
        );
        store
            .report(
                pane_id,
                AgentReportPayload {
                    tasks: Some(vec![reported_task.clone()]),
                    ..AgentReportPayload::default()
                },
                base + std::time::Duration::from_secs(1),
            )
            .expect("valid report");

        assert_eq!(
            store.snapshot(pane_id, AgentStatus::Working).tasks,
            vec![reported_task]
        );
    }

    #[test]
    fn newer_transcript_tasks_override_reported_tasks() {
        let pane_id = PaneId::from_raw(14);
        let base = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let reported_task = AgentTask {
            text: "old reported task".into(),
            status: AgentTaskStatus::Pending,
        };
        let transcript_task = AgentTask {
            text: "new transcript task".into(),
            status: AgentTaskStatus::Completed,
        };
        let mut store = AgentStateStore::default();
        store
            .report(
                pane_id,
                AgentReportPayload {
                    tasks: Some(vec![reported_task]),
                    ..AgentReportPayload::default()
                },
                base,
            )
            .expect("valid report");
        store.observe_transcript(
            pane_id,
            Some(base + std::time::Duration::from_secs(1)),
            Some(vec![transcript_task.clone()]),
            Some(base + std::time::Duration::from_secs(1)),
            Vec::new(),
            Vec::new(),
        );

        assert_eq!(
            store.snapshot(pane_id, AgentStatus::Working).tasks,
            vec![transcript_task]
        );
    }

    #[test]
    fn explicit_empty_report_and_newer_empty_transcript_clear_tasks() {
        let pane_id = PaneId::from_raw(15);
        let base = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let mut store = AgentStateStore::default();
        store
            .report(
                pane_id,
                serde_json::from_str(
                    r#"{"tasks":[{"text":"reported task","status":"in_progress"}]}"#,
                )
                .expect("report payload"),
                base,
            )
            .expect("valid report");
        store
            .report(
                pane_id,
                serde_json::from_str(r#"{"tasks":[]}"#).expect("empty report payload"),
                base + std::time::Duration::from_secs(1),
            )
            .expect("valid empty report");
        assert!(store
            .snapshot(pane_id, AgentStatus::Working)
            .tasks
            .is_empty());

        store
            .report(
                pane_id,
                serde_json::from_str(r#"{"tasks":[{"text":"new report","status":"pending"}]}"#)
                    .expect("replacement report payload"),
                base + std::time::Duration::from_secs(2),
            )
            .expect("valid replacement report");
        store.observe_transcript(
            pane_id,
            Some(base + std::time::Duration::from_secs(3)),
            Some(Vec::new()),
            Some(base + std::time::Duration::from_secs(3)),
            Vec::new(),
            Vec::new(),
        );
        assert!(store
            .snapshot(pane_id, AgentStatus::Working)
            .tasks
            .is_empty());
    }

    #[test]
    fn observed_and_reported_subagents_are_both_projected() {
        let pane_id = PaneId::from_raw(10);
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let mut store = AgentStateStore::default();
        store
            .report(
                pane_id,
                AgentReportPayload {
                    subagents: vec![AgentSubagent {
                        name: "reported reviewer".into(),
                        status: AgentStatus::Blocked,
                        last_active_at: None,
                        pane_id: Some("w1:p2".into()),
                        source: AgentSubagentSource::Observed,
                    }],
                    ..AgentReportPayload::default()
                },
                now,
            )
            .unwrap();
        store.observe_transcript(
            pane_id,
            Some(now),
            None,
            None,
            vec![AgentSubagent {
                name: "native worker".into(),
                status: AgentStatus::Working,
                last_active_at: format_rfc3339(now),
                pane_id: None,
                source: AgentSubagentSource::Observed,
            }],
            Vec::new(),
        );

        let subagents = store.snapshot(pane_id, AgentStatus::Working).subagents;
        assert_eq!(subagents.len(), 2);
        assert_eq!(subagents[0].source, AgentSubagentSource::Observed);
        assert_eq!(subagents[1].source, AgentSubagentSource::Reported);
    }

    #[test]
    fn invalid_report_is_rejected_without_changing_state() {
        let pane_id = PaneId::from_raw(11);
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let mut store = AgentStateStore::default();
        store
            .report(
                pane_id,
                AgentReportPayload {
                    goal: Some("keep me".into()),
                    ..AgentReportPayload::default()
                },
                now,
            )
            .expect("valid initial report");

        let error = store
            .report(
                pane_id,
                AgentReportPayload {
                    goal: Some("replace me".into()),
                    subagents: vec![AgentSubagent {
                        name: "reviewer".into(),
                        status: AgentStatus::Working,
                        last_active_at: Some("yesterday".into()),
                        pane_id: None,
                        source: AgentSubagentSource::Reported,
                    }],
                    ..AgentReportPayload::default()
                },
                now + std::time::Duration::from_secs(1),
            )
            .expect_err("invalid timestamp must reject the entire report");

        assert!(error.contains("RFC3339"));
        assert_eq!(
            store.snapshot(pane_id, AgentStatus::Idle).goal.as_deref(),
            Some("keep me")
        );
    }
}
