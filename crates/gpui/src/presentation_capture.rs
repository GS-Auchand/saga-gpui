//! Opt-in diagnostics; records API submission, not confirmed display.
use serde::Serialize;

/// Identity attached to a committed scene, independent of presentation attempts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PresentationScene {
    /// Monotonic within one Window. Zero is never a committed scene.
    pub scene_id: u64,
    /// Caller revision captured at scene commit.
    pub caller_token: Option<u64>,
}

/// Native monotonic counter sample and ticks per second.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct PlatformPresentationClock {
    /// Counter value, in ticks.
    pub qpc: u64,
    /// Counter ticks per second.
    pub frequency: u64,
}

/// One backend Present API invocation. Clock failure remains explicit.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PlatformPresentRecord {
    /// Committed scene identity, or zero if no Window committed it.
    pub scene_id: u64,
    /// Caller revision from that scene, never sampled at Present time.
    pub caller_token: Option<u64>,
    /// Monotonic attempt sequence for this renderer, retained across drains.
    pub present_sequence: u64,
    /// QPC immediately before Present.
    pub qpc_start: Option<u64>,
    /// QPC immediately after Present.
    pub qpc_end: Option<u64>,
    /// Frequency queried before the timed bracket.
    pub qpc_frequency: Option<u64>,
    /// Calling process ID.
    pub process_id: u32,
    /// Calling thread ID.
    pub thread_id: u32,
    /// Raw IDXGISwapChain1 pointer; validate against ETW identity.
    pub swap_chain: u64,
    /// Increments when device recovery replaces the swap chain.
    pub swap_chain_generation: u64,
    /// Native window identity.
    pub hwnd: u64,
    /// Raw HRESULT, including failures and non-error status codes.
    pub hresult: i32,
    /// Optional cache-replayed coverage from this exact committed scene.
    pub text_coverage: Option<std::sync::Arc<TextCoverageSnapshot>>,
}

/// Draining preserves attempt sequence and reports every capacity loss.
#[derive(Debug, Default, Serialize)]
pub struct PlatformPresentSnapshot {
    /// Oldest retained records since the preceding drain.
    pub records: Vec<PlatformPresentRecord>,
    /// Dropped records since the preceding drain. Any loss invalidates completeness.
    pub overflow: u64,
}

/// Bounded backend journal, shared with portable diagnostic tests.
#[doc(hidden)]
#[derive(Default)]
pub struct PresentationJournal {
    capacity: usize,
    sequence: u64,
    records: Vec<PlatformPresentRecord>,
    overflow: u64,
}
impl PresentationJournal {
    /// Enable once, before collection. Re-enabling is rejected to prevent loss.
    pub fn enable(&mut self, capacity: usize) -> bool {
        if self.capacity != 0 {
            return false;
        }
        self.capacity = capacity.clamp(1, 65_536);
        self.records = Vec::with_capacity(self.capacity);
        true
    }
    /// Whether backend sampling is enabled.
    pub fn enabled(&self) -> bool {
        self.capacity != 0
    }
    /// Append after the timed bracket; no allocation until a drain transfers storage.
    pub fn push(&mut self, mut record: PlatformPresentRecord) {
        if !self.enabled() {
            return;
        }
        self.sequence += 1;
        record.present_sequence = self.sequence;
        if self.records.len() < self.capacity {
            self.records.push(record);
        } else {
            self.overflow = self.overflow.saturating_add(1);
        }
    }
    /// Drain without resetting sequence. Replacement storage is allocated here.
    pub fn drain(&mut self) -> PlatformPresentSnapshot {
        if self.records.is_empty() && self.overflow == 0 {
            return PlatformPresentSnapshot::default();
        }
        PlatformPresentSnapshot {
            records: std::mem::replace(&mut self.records, Vec::with_capacity(self.capacity)),
            overflow: std::mem::take(&mut self.overflow),
        }
    }
}

/// Caller identity for one shaped paragraph, with no retained model inference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TextCoverageTag {
    /// Caller-defined view identity.
    pub owner_id: u64,
    /// Stable caller-defined line identity.
    pub item_id: u64,
    /// Full UTF-8 length of this painted revision.
    pub utf8_len: usize,
    /// Whether this painted revision is a complete logical line.
    pub complete: bool,
}
/// Bounds are device pixels, [x, y, width, height], after rasterization/snapping.
#[derive(Clone, Debug, Serialize)]
pub struct TextGlyphCoverage {
    /// Shaper cluster start (UTF-8 byte offset).
    pub byte_start: usize,
    /// Next distinct shaper cluster or end of text.
    pub byte_end: usize,
    /// Actual sprite raster bounds; zero for a glyph with no raster ink.
    pub raster_bounds: [f32; 4],
    /// Raster bounds intersected with the effective content mask.
    pub clipped_bounds: [f32; 4],
    /// Whether the raster has positive extent (not proof every pixel is opaque).
    pub has_ink: bool,
    /// Effective text/element opacity. Positive clip alone does not imply visibility.
    pub effective_opacity: f32,
    /// Complete raster extent fits clip and opacity is positive, or no ink exists.
    pub fully_visible: bool,
}
/// Conservative coverage for one actual painted shaped paragraph.
#[derive(Clone, Debug, Serialize)]
pub struct TextCoverageRow {
    /// Source identity supplied by the element actually being painted.
    pub tag: TextCoverageTag,
    /// Number of shaped glyphs expected, including culled and empty-ink glyphs.
    pub expected_glyphs: usize,
    /// Number observed at raster paint; fewer means no full-visible claim.
    pub accounted_glyphs: usize,
    /// Complete, no missing glyphs, all raster bounds fit, at least one ink glyph.
    pub fully_visible: bool,
    /// Bounded actual glyph observations, not guessed character-cell rectangles.
    pub glyphs: Vec<TextGlyphCoverage>,
}
/// Immutable frame coverage shared by repeated presentations of one scene.
#[derive(Clone, Debug, Default, Serialize)]
pub struct TextCoverageSnapshot {
    /// Includes painted overdraw with clipped/hidden observations; filter explicitly.
    pub rows: Vec<std::sync::Arc<TextCoverageRow>>,
    /// Dropped rows or glyphs. Nonzero invalidates completeness claims.
    pub overflow: u64,
}

pub(crate) struct ActiveTextCoverage {
    pub row: TextCoverageRow,
    pub glyph_range: Option<std::ops::Range<usize>>,
    pub overflow: u64,
}
pub(crate) const MAX_COVERAGE_ROWS: usize = 256;
pub(crate) const MAX_COVERAGE_GLYPHS: usize = 16_384;

/// Device-creation/recovery evidence for an explicitly requested queue experiment.
/// This is not a queue-depth measurement or a live getter-time query.
#[derive(Clone, Debug, Default, Serialize)]
pub struct DeviceFrameLatencyEvidence {
    /// Process-local monotonically increasing device creation identity.
    pub device_generation: u64,
    /// D3D device COM identity, useful only with process/generation identity.
    pub device_address: u64,
    /// Only explicit experimental value 1 is accepted; absent/invalid stays None.
    pub requested: Option<u32>,
    /// default, requested, or invalid. Invalid requests never mutate the setting.
    pub request_status: String,
    /// Initial IDXGIDevice1 maximum-frame-latency query, if successful.
    pub before: Option<u32>,
    /// Readback after the optional setter, if successful.
    pub after: Option<u32>,
    /// QueryInterface status (zero for success).
    pub query_interface_hresult: Option<i32>,
    /// Before-query status (zero for success).
    pub before_hresult: Option<i32>,
    /// Setter status; None means no SetMaximumFrameLatency call occurred.
    pub set_hresult: Option<i32>,
    /// After-query status (zero for success).
    pub after_hresult: Option<i32>,
    /// Explicit final outcome; only applied verifies requested==readback.
    pub result: String,
}
