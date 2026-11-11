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
