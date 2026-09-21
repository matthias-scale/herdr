use std::collections::HashMap;
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

#[cfg(test)]
const MAX_OPEN_SEQUENCE_BYTES: usize = 8 * 1024;
// Publish complete URLs through 8 KiB. Once a URL exceeds this bound, discard the
// entire open sequence until its terminator so no printed prefix can escape.
const MAX_URL_BYTES: usize = 8 * 1024;
// RFC schemes are much smaller in practice; cap lookbehind and reject the whole
// URL when the scheme before `://` is longer than 64 bytes.
const MAX_SCHEME_BYTES: usize = 64;
const MAX_MARKER_TAIL_BYTES: usize = MAX_SCHEME_BYTES + 2;
const MAX_SCHEME_CANDIDATE_BYTES: usize = MAX_SCHEME_BYTES + 3;
#[cfg(test)]
const LARGE_TEST_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
#[cfg(test)]
const OLD_STAGING_BOUNDARY_BYTES: usize = LARGE_TEST_OUTPUT_BYTES / 2;

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
    scanner: LinkStreamScanner,
    dirty: bool,
    queued_output_urls: Vec<String>,
    queued_osc8_urls: Vec<String>,
}

#[derive(Debug, Default)]
struct LinkStreamScanner {
    visible: VisibleLinkState,
    terminal: TerminalScanState,
}

#[derive(Debug, Default)]
enum VisibleLinkState {
    #[default]
    Empty,
    Scheme(SchemeCandidate),
    AfterColon(SchemeCandidate),
    AfterSlash(SchemeCandidate),
    Url(Vec<u8>),
    InvalidScheme,
    DiscardUrl,
}

#[derive(Debug)]
struct SchemeCandidate {
    bytes: [u8; MAX_SCHEME_CANDIDATE_BYTES],
    len: usize,
}

impl SchemeCandidate {
    fn new(byte: u8) -> Self {
        let mut bytes = [0; MAX_SCHEME_CANDIDATE_BYTES];
        bytes[0] = byte;
        Self { bytes, len: 1 }
    }

    fn push(&mut self, byte: u8) {
        debug_assert!(self.len < self.bytes.len());
        self.bytes[self.len] = byte;
        self.len += 1;
    }

    fn into_vec(self) -> Vec<u8> {
        self.bytes[..self.len].to_vec()
    }
}

#[derive(Debug, Default)]
enum TerminalScanState {
    #[default]
    Visible,
    Escape,
    Charset,
    Csi,
    Osc(OscScanState),
}

#[derive(Debug)]
struct OscScanState {
    phase: OscPhase,
    escaped: bool,
}

impl Default for OscScanState {
    fn default() -> Self {
        Self {
            phase: OscPhase::Prefix(0),
            escaped: false,
        }
    }
}

#[derive(Debug)]
enum OscPhase {
    Prefix(usize),
    Parameters,
    Target(Vec<u8>),
    Ignore,
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
            pending.scan(bytes);
            self.record_scanned_bytes(bytes.len());
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
        pending.scan(&candidate[marker_start..]);
        self.record_scanned_bytes(candidate.len() - marker_start);
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
        let (mut output_urls, mut osc8_urls) = {
            let _processing = self.processing.lock().ok()?;
            let mut pending = self.pending.lock().ok()?;
            if !pending.dirty {
                return None;
            }
            pending.dirty = false;
            let output_urls = std::mem::take(&mut pending.queued_output_urls);
            let osc8_urls = std::mem::take(&mut pending.queued_osc8_urls);
            if let Some(tail) = pending.scanner.take_marker_tail() {
                self.marker_tail.store(&tail);
                self.active.store(false, Ordering::Release);
            } else {
                self.active
                    .store(!pending.scanner.is_idle(), Ordering::Release);
            }
            (output_urls, osc8_urls)
        };
        output_urls.sort_unstable();
        output_urls.dedup();
        osc8_urls.sort_unstable();
        osc8_urls.dedup();
        let found_links = !output_urls.is_empty() || !osc8_urls.is_empty();
        if !found_links {
            return None;
        }
        self.extractions.fetch_add(1, Ordering::Relaxed);
        Some(ExtractedAgentLinks {
            output_urls,
            osc8_urls,
        })
    }

    #[cfg(test)]
    pub(crate) fn extraction_count(&self) -> u64 {
        self.extractions.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn lock_acquisition_count(&self) -> u64 {
        self.lock_acquisitions.load(Ordering::Relaxed)
    }

    fn record_scanned_bytes(&self, bytes: usize) {
        let _ = bytes;
        #[cfg(test)]
        self.scanned_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    #[cfg(test)]
    fn retained_byte_count(&self) -> usize {
        self.pending
            .lock()
            .map(|pending| pending.scanner.retained_byte_count())
            .unwrap_or_default()
    }

    #[cfg(test)]
    fn set_observe_race_hook(&self, arrived: Arc<Barrier>, resume: Arc<Barrier>) {
        if let Ok(mut hook) = self.observe_race_hook.lock() {
            *hook = Some((arrived, resume));
        }
    }
}

impl PendingLinkBytes {
    fn scan(&mut self, bytes: &[u8]) {
        for byte in bytes.iter().copied() {
            self.scanner.scan_byte(
                byte,
                &mut self.queued_output_urls,
                &mut self.queued_osc8_urls,
            );
        }
        trim_link_queue(&mut self.queued_output_urls);
        trim_link_queue(&mut self.queued_osc8_urls);
    }
}

impl LinkStreamScanner {
    fn scan_byte(&mut self, byte: u8, output_urls: &mut Vec<String>, osc8_urls: &mut Vec<String>) {
        let terminal = std::mem::take(&mut self.terminal);
        self.terminal = match terminal {
            TerminalScanState::Visible if byte == b'\x1b' => TerminalScanState::Escape,
            TerminalScanState::Visible => {
                self.scan_visible(byte, output_urls);
                TerminalScanState::Visible
            }
            TerminalScanState::Escape => match byte {
                b'[' => TerminalScanState::Csi,
                b']' => TerminalScanState::Osc(OscScanState::default()),
                b'(' | b')' | b'*' | b'+' | b'-' | b'.' | b'/' => TerminalScanState::Charset,
                _ => TerminalScanState::Visible,
            },
            TerminalScanState::Charset => TerminalScanState::Visible,
            TerminalScanState::Csi if (0x40..=0x7e).contains(&byte) => TerminalScanState::Visible,
            TerminalScanState::Csi => TerminalScanState::Csi,
            TerminalScanState::Osc(mut osc) => {
                if osc.scan_byte(byte, osc8_urls) {
                    if matches!(self.visible, VisibleLinkState::InvalidScheme) {
                        self.visible = VisibleLinkState::Empty;
                    }
                    TerminalScanState::Visible
                } else {
                    TerminalScanState::Osc(osc)
                }
            }
        };
    }

    fn scan_visible(&mut self, byte: u8, output_urls: &mut Vec<String>) {
        let state = std::mem::take(&mut self.visible);
        self.visible = match state {
            VisibleLinkState::Empty => start_scheme(byte),
            VisibleLinkState::Scheme(mut scheme) if is_scheme_byte(byte) => {
                if scheme.len < MAX_SCHEME_BYTES {
                    scheme.push(byte);
                    VisibleLinkState::Scheme(scheme)
                } else {
                    VisibleLinkState::InvalidScheme
                }
            }
            VisibleLinkState::Scheme(mut scheme) if byte == b':' => {
                scheme.push(byte);
                VisibleLinkState::AfterColon(scheme)
            }
            VisibleLinkState::Scheme(_) => {
                self.visible = VisibleLinkState::Empty;
                self.scan_visible(byte, output_urls);
                return;
            }
            VisibleLinkState::AfterColon(mut scheme) if byte == b'/' => {
                scheme.push(byte);
                VisibleLinkState::AfterSlash(scheme)
            }
            VisibleLinkState::AfterColon(_) => {
                self.visible = VisibleLinkState::Empty;
                self.scan_visible(byte, output_urls);
                return;
            }
            VisibleLinkState::AfterSlash(mut scheme) if byte == b'/' => {
                scheme.push(byte);
                VisibleLinkState::Url(scheme.into_vec())
            }
            VisibleLinkState::AfterSlash(_) => {
                self.visible = VisibleLinkState::Empty;
                self.scan_visible(byte, output_urls);
                return;
            }
            VisibleLinkState::Url(url) if is_url_terminator(byte) => {
                publish_output_url(url, output_urls);
                VisibleLinkState::Empty
            }
            VisibleLinkState::Url(mut url) if url.len() < MAX_URL_BYTES => {
                url.push(byte);
                VisibleLinkState::Url(url)
            }
            VisibleLinkState::Url(_) => VisibleLinkState::DiscardUrl,
            VisibleLinkState::DiscardUrl if is_url_terminator(byte) => VisibleLinkState::Empty,
            VisibleLinkState::DiscardUrl => VisibleLinkState::DiscardUrl,
            VisibleLinkState::InvalidScheme if is_scheme_byte(byte) => {
                VisibleLinkState::InvalidScheme
            }
            VisibleLinkState::InvalidScheme => start_scheme(byte),
        };
    }

    fn take_marker_tail(&mut self) -> Option<Vec<u8>> {
        if !matches!(self.terminal, TerminalScanState::Visible) {
            return None;
        }
        let visible = std::mem::take(&mut self.visible);
        match visible {
            VisibleLinkState::Empty | VisibleLinkState::InvalidScheme => Some(Vec::new()),
            VisibleLinkState::Scheme(bytes)
            | VisibleLinkState::AfterColon(bytes)
            | VisibleLinkState::AfterSlash(bytes) => Some(bytes.into_vec()),
            state @ (VisibleLinkState::Url(_) | VisibleLinkState::DiscardUrl) => {
                self.visible = state;
                None
            }
        }
    }

    fn is_idle(&self) -> bool {
        matches!(self.terminal, TerminalScanState::Visible)
            && matches!(
                self.visible,
                VisibleLinkState::Empty | VisibleLinkState::InvalidScheme
            )
    }

    #[cfg(test)]
    fn retained_byte_count(&self) -> usize {
        let visible = match &self.visible {
            VisibleLinkState::Scheme(bytes)
            | VisibleLinkState::AfterColon(bytes)
            | VisibleLinkState::AfterSlash(bytes) => bytes.len,
            VisibleLinkState::Url(bytes) => bytes.len(),
            VisibleLinkState::Empty
            | VisibleLinkState::InvalidScheme
            | VisibleLinkState::DiscardUrl => 0,
        };
        let osc = match &self.terminal {
            TerminalScanState::Osc(OscScanState {
                phase: OscPhase::Target(target),
                ..
            }) => target.len(),
            _ => 0,
        };
        visible + osc
    }
}

impl OscScanState {
    fn scan_byte(&mut self, byte: u8, osc8_urls: &mut Vec<String>) -> bool {
        if self.escaped {
            self.escaped = byte == b'\x1b';
            if byte == b'\\' {
                self.publish_target(osc8_urls);
                return true;
            }
            if byte == b'\x07' {
                self.publish_target(osc8_urls);
                return true;
            }
            if byte != b'\x1b' {
                self.scan_content(byte);
            }
            return false;
        }
        match byte {
            b'\x07' => {
                self.publish_target(osc8_urls);
                true
            }
            b'\x1b' => {
                self.escaped = true;
                false
            }
            _ => {
                self.scan_content(byte);
                false
            }
        }
    }

    fn scan_content(&mut self, byte: u8) {
        self.phase = match std::mem::replace(&mut self.phase, OscPhase::Ignore) {
            OscPhase::Prefix(0) if byte == b'8' => OscPhase::Prefix(1),
            OscPhase::Prefix(1) if byte == b';' => OscPhase::Parameters,
            OscPhase::Prefix(_) => OscPhase::Ignore,
            OscPhase::Parameters if byte == b';' => OscPhase::Target(Vec::new()),
            OscPhase::Parameters => OscPhase::Parameters,
            OscPhase::Target(mut target) if target.len() < MAX_URL_BYTES => {
                target.push(byte);
                OscPhase::Target(target)
            }
            OscPhase::Target(_) => OscPhase::Ignore,
            OscPhase::Ignore => OscPhase::Ignore,
        };
    }

    fn publish_target(&mut self, osc8_urls: &mut Vec<String>) {
        let OscPhase::Target(target) = &self.phase else {
            return;
        };
        let mut urls = extract_urls(&String::from_utf8_lossy(target));
        queue_links(osc8_urls, &mut urls);
    }
}

fn start_scheme(byte: u8) -> VisibleLinkState {
    if byte.is_ascii_alphabetic() {
        VisibleLinkState::Scheme(SchemeCandidate::new(byte))
    } else if is_scheme_byte(byte) {
        VisibleLinkState::InvalidScheme
    } else {
        VisibleLinkState::Empty
    }
}

fn publish_output_url(bytes: Vec<u8>, output_urls: &mut Vec<String>) {
    let url = String::from_utf8_lossy(&bytes);
    let url = trim_url_suffix(&url);
    if url_within_bounds(url) && url_domain(url).is_some() {
        queue_links(output_urls, &mut vec![url.to_owned()]);
    }
}

fn trim_link_queue(queue: &mut Vec<String>) {
    queue.sort_unstable();
    queue.dedup();
    if queue.len() > MAX_LINKS {
        queue.drain(..queue.len() - MAX_LINKS);
    }
}

struct ExtractionClaim<'a>(&'a AtomicBool);

impl Drop for ExtractionClaim<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
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
            "b".repeat(LARGE_TEST_OUTPUT_BYTES)
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
        gate.observe_chunk(&vec![b'x'; LARGE_TEST_OUTPUT_BYTES]);
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
        let filler_len = LARGE_TEST_OUTPUT_BYTES - first.len() - open_url.len();
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
        gate.observe_chunk(&vec![b'a'; LARGE_TEST_OUTPUT_BYTES]);

        assert!(gate.retained_byte_count() <= MAX_URL_BYTES);
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
        let evicted_at = OLD_STAGING_BOUNDARY_BYTES + 128;
        let mut chunk = b"https://before-overflow.example/path\n".to_vec();
        chunk.resize(evicted_at, b'x');
        chunk.push(b' ');
        chunk.extend_from_slice(evicted_url);
        chunk.push(b'\n');
        chunk.resize(LARGE_TEST_OUTPUT_BYTES + 4_096, b'y');

        gate.observe_chunk(&chunk);

        assert!(gate.retained_byte_count() <= MAX_URL_BYTES);
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
        first.resize(OLD_STAGING_BOUNDARY_BYTES + 40, b'x');
        first.push(b'\n');
        first.extend_from_slice(evicted_url);
        first.push(b'\n');
        first.resize(LARGE_TEST_OUTPUT_BYTES, b'y');

        gate.observe_chunk(&first);
        gate.observe_chunk(&[b'z'; 64]);
        gate.observe_chunk(&[b'z'; 64]);

        assert!(gate.retained_byte_count() <= MAX_URL_BYTES);
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
    fn complete_urls_at_old_staging_boundaries_survive_large_prefix_and_suffix() {
        for boundary in (1..=4).map(|multiple| OLD_STAGING_BOUNDARY_BYTES * multiple) {
            let gate = LinkExtractionGate::default();
            let url = format!("https://boundary-{boundary}.example/path");
            let mut chunk = vec![b'x'; boundary];
            *chunk.last_mut().expect("non-empty prefix") = b'\n';
            chunk.extend_from_slice(url.as_bytes());
            chunk.push(b'\n');
            chunk.resize(chunk.len() + LARGE_TEST_OUTPUT_BYTES, b'y');

            gate.observe_chunk(&chunk);

            assert!(
                gate.retained_byte_count()
                    <= MAX_URL_BYTES + MAX_SCHEME_BYTES + MAX_OPEN_SEQUENCE_BYTES,
                "old staging boundary {boundary} retained a multi-MiB window"
            );
            assert_eq!(
                gate.take_links()
                    .unwrap_or_else(|| panic!("URL at old staging boundary {boundary}"))
                    .output_urls,
                vec![url],
                "old staging boundary {boundary}"
            );
        }
    }

    #[test]
    fn unterminated_url_survives_many_detection_cycles_until_terminated() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"https://many-cycles.example/");

        let styling = b"\x1b[31m".repeat(LARGE_TEST_OUTPUT_BYTES / 5 + 1);
        let mut max_retained = 0;

        for _ in 0..3 {
            gate.observe_chunk(&styling);
            max_retained = max_retained.max(gate.retained_byte_count());
            assert!(gate.take_links().is_none());
            gate.observe_chunk(b"a");
        }

        assert!(gate.take_links().is_none());
        gate.observe_chunk(b"\n");
        assert_eq!(
            gate.take_links().expect("terminated URL").output_urls,
            vec![format!("https://many-cycles.example/{}", "a".repeat(3))]
        );
        assert!(
            max_retained <= MAX_URL_BYTES + MAX_SCHEME_BYTES + MAX_OPEN_SEQUENCE_BYTES,
            "a detection cycle retained the full styling burst"
        );
    }

    #[test]
    fn independent_chunk_streams_scan_each_received_byte_once() {
        const PANES: usize = 24;
        const CHUNKS: usize = 128;
        let gates = (0..PANES)
            .map(|_| LinkExtractionGate::default())
            .collect::<Vec<_>>();

        let prefix = b"\x1b]8;;https://linear.example/";
        for gate in &gates {
            gate.observe_chunk(prefix);
            assert!(gate.take_links().is_none());
        }
        for _ in 0..CHUNKS {
            for gate in &gates {
                gate.observe_chunk(b"a");
                assert!(gate.take_links().is_none());
            }
        }

        let received = (prefix.len() + CHUNKS) * PANES;
        let scanned = gates
            .iter()
            .map(|gate| gate.scanned_bytes.load(Ordering::Relaxed) as usize)
            .sum::<usize>();
        assert!(
            scanned <= received,
            "already-scanned bytes were revisited: received {received}, scanned {scanned}"
        );
    }

    #[test]
    fn overflow_suffix_resumes_after_open_osc_terminator() {
        for split_st in [false, true] {
            let gate = LinkExtractionGate::default();
            let first = b"https://before-overflow.example/path\n";
            gate.observe_chunk(first);

            let osc = b"\x1b]0;title";
            let mut overflow = vec![b'x'; OLD_STAGING_BOUNDARY_BYTES - first.len() - osc.len()];
            overflow.extend_from_slice(osc);
            overflow.resize(LARGE_TEST_OUTPUT_BYTES + 256, b'y');
            overflow.extend_from_slice(b" hidden https://hidden.example/path ");
            if split_st {
                overflow.push(b'\x1b');
                gate.observe_chunk(&overflow);
                gate.observe_chunk(b"\\https://visible.example/path\n");
            } else {
                overflow.extend_from_slice(b"\x07https://visible.example/path\n");
                gate.observe_chunk(&overflow);
            }

            assert!(gate.retained_byte_count() <= MAX_URL_BYTES);
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
        let filler_len = LARGE_TEST_OUTPUT_BYTES - first.len() - csi_len;
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
        let filler_len = LARGE_TEST_OUTPUT_BYTES - first.len() - osc_len;
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
        chunk.resize(LARGE_TEST_OUTPUT_BYTES, b'a');
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
