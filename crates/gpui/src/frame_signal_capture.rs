//! Opt-in scheduling provenance. Pending invalidations are correlations, not proof of WM_PAINT origin.
use parking_lot::Mutex;
use serde::Serialize;
use std::{
    cell::RefCell,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Debug, Default, Serialize)]
/// One native compositor wait and its optional fallback, sampled in the presentation QPC domain.
pub struct FrameWaitTrace {
    /// Original provider return before optional phase delay; includes any existing fallback sleep.
    pub provider_return_qpc: Option<u64>,
    /// Separate opt-in phase experiment; absent in earlier instrumentation or when not sampled.
    pub phase_delay: Option<crate::PhaseDelayTrace>,
    /// Counter before entering the native compositor wait; None means clock failure.
    pub native_start_qpc: Option<u64>,
    /// Counter after the native compositor wait and elapsed-time measurement.
    pub native_end_qpc: Option<u64>,
    /// Counter before fallback sleep, absent when no fallback was required or the clock failed.
    pub fallback_start_qpc: Option<u64>,
    /// Counter after fallback sleep, absent when no fallback was required or the clock failed.
    pub fallback_end_qpc: Option<u64>,
    /// Native error HRESULT, or zero for Ok normalized by the Windows binding; not a raw positive success code.
    pub native_hresult: i32,
}
#[derive(Clone, Debug, Default, Serialize)]
/// One synchronous paint callback and its pending-signal batch; association is not exclusive OS causation.
pub struct FramePaintToken {
    /// Native HWND value used to reject cross-window scope inheritance.
    pub window_id: u64,
    /// Monotonic paint-handler entry ID within this window journal, including deferred entries.
    pub paint_id: u64,
    /// First pending diagnostic signal consumed at callback entry, or None for an unsignaled callback.
    pub first_signal_id: Option<u64>,
    /// Last pending diagnostic signal consumed at callback entry.
    pub last_signal_id: Option<u64>,
    /// Number of pending signals consumed together; zero does not imply no other OS invalidation.
    pub signal_count: u64,
    /// Counter at paint-handler entry, before direct-manipulation and callback work.
    pub paint_entry_qpc: Option<u64>,
    /// Counter sampled before consuming the pending batch and invoking the synchronous callback.
    pub callback_entry_qpc: Option<u64>,
}
#[derive(Clone, Debug, Default, Serialize)]
/// One bounded per-window scheduling observation; cross-thread append order is not causal order.
pub struct FrameSignalRecord {
    /// Stage: signal-visible, signal-hidden, redraw-request, redraw, paint-entry, paint-deferred, paint-no-callback, callback-begin, or callback-end.
    pub kind: String,
    /// Window-local signal ID for signal/redraw stages; unrelated paint entries have no signal ID.
    pub signal_id: Option<u64>,
    /// Paint/callback identity when this observation concerns paint handling.
    pub paint: Option<FramePaintToken>,
    /// Stage counter sample, or None on clock failure; use IDs to resolve concurrent append order.
    pub qpc: Option<u64>,
    /// Compositor wait evidence copied into this signal record; several windows may share the same wait.
    pub wait: Option<FrameWaitTrace>,
    /// Redraw completion: zero for success, converted Win32 failure HRESULT otherwise; absent at other stages.
    pub result: Option<i32>,
    /// First pending signal discarded when the window was hidden/minimized.
    pub cleared_first_signal_id: Option<u64>,
    /// Last pending signal discarded when the window was hidden/minimized.
    pub cleared_last_signal_id: Option<u64>,
    /// Count discarded on a hidden signal, recorded explicitly rather than attributed to a later callback.
    pub cleared_signal_count: u64,
}
#[derive(Debug, Default, Serialize)]
/// Drain result; any overflow invalidates complete reconstruction of scheduling provenance.
pub struct FrameSignalSnapshot {
    /// Oldest retained records since the previous drain, in journal append order.
    pub records: Vec<FrameSignalRecord>,
    /// Records dropped since the previous drain; IDs and pending state still advance.
    pub overflow: u64,
}
#[derive(Default)]
struct State {
    capacity: usize,
    signal_sequence: u64,
    paint_sequence: u64,
    first: Option<u64>,
    last: Option<u64>,
    count: u64,
    records: Vec<FrameSignalRecord>,
    overflow: u64,
}
impl State {
    fn push(&mut self, record: FrameSignalRecord) {
        if self.records.len() < self.capacity {
            self.records.push(record);
        } else {
            self.overflow = self.overflow.saturating_add(1);
        }
    }
}
#[derive(Default)]
/// Opt-in bounded per-window journal with atomically coalesced pending signal identity.
pub struct FrameSignalJournal {
    enabled: AtomicBool,
    state: Mutex<State>,
}
impl FrameSignalJournal {
    /// Whether capture has been enabled; disabled capture does not allocate or record.
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }
    /// Enable once with capacity clamped to 1..=65,536; repeated calls fail without discarding evidence.
    pub fn enable(&self, capacity: usize) -> bool {
        let mut state = self.state.lock();
        if self.enabled() {
            return false;
        }
        state.capacity = capacity.clamp(1, 65_536);
        state.records = Vec::with_capacity(state.capacity);
        self.enabled.store(true, Ordering::Release);
        true
    }
    /// Record a signal attempt; visible attempts join the pending batch, hidden attempts explicitly clear it.
    pub fn signal(
        &self,
        visible: bool,
        wait: Option<FrameWaitTrace>,
        qpc: Option<u64>,
    ) -> Option<u64> {
        if !self.enabled() {
            return None;
        }
        let mut s = self.state.lock();
        s.signal_sequence += 1;
        let id = s.signal_sequence;
        let mut record = FrameSignalRecord {
            kind: if visible {
                "signal-visible"
            } else {
                "signal-hidden"
            }
            .into(),
            signal_id: Some(id),
            wait,
            qpc,
            ..Default::default()
        };
        if visible {
            if s.first.is_none() {
                s.first = Some(id);
            }
            s.last = Some(id);
            s.count += 1;
        } else {
            record.cleared_first_signal_id = s.first.take();
            record.cleared_last_signal_id = s.last.take();
            record.cleared_signal_count = std::mem::take(&mut s.count);
        }
        s.push(record);
        Some(id)
    }
    /// Append a stage observation when enabled, retaining oldest records and counting capacity losses.
    pub fn record(&self, record: FrameSignalRecord) {
        if self.enabled() {
            self.state.lock().push(record);
        }
    }
    /// Record handler entry without consuming pending signals; return None while capture is disabled.
    pub fn begin_paint(&self, window_id: u64, qpc: Option<u64>) -> Option<FramePaintToken> {
        if !self.enabled() {
            return None;
        }
        let mut s = self.state.lock();
        s.paint_sequence += 1;
        let token = FramePaintToken {
            window_id,
            paint_id: s.paint_sequence,
            paint_entry_qpc: qpc,
            ..Default::default()
        };
        s.push(FrameSignalRecord {
            kind: "paint-entry".into(),
            paint: Some(token.clone()),
            qpc,
            ..Default::default()
        });
        Some(token)
    }
    /// Only a callback consumes pending signals; reentrant/no-callback paints leave them pending.
    pub fn begin_callback(&self, mut token: FramePaintToken, qpc: Option<u64>) -> FramePaintToken {
        let mut s = self.state.lock();
        token.first_signal_id = s.first.take();
        token.last_signal_id = s.last.take();
        token.signal_count = std::mem::take(&mut s.count);
        token.callback_entry_qpc = qpc;
        s.push(FrameSignalRecord {
            kind: "callback-begin".into(),
            paint: Some(token.clone()),
            qpc,
            ..Default::default()
        });
        token
    }
    /// Transfer retained observations and overflow count without resetting IDs or the pending batch.
    pub fn drain(&self) -> FrameSignalSnapshot {
        if !self.enabled() {
            return Default::default();
        }
        let mut s = self.state.lock();
        if s.records.is_empty() && s.overflow == 0 {
            return Default::default();
        }
        let capacity = s.capacity;
        FrameSignalSnapshot {
            records: std::mem::replace(&mut s.records, Vec::with_capacity(capacity)),
            overflow: std::mem::take(&mut s.overflow),
        }
    }
}
thread_local! { static CURRENT_PAINT: RefCell<Option<FramePaintToken>> = const { RefCell::new(None) }; }
/// Restores the enclosing scope on unwind; a different window cannot inherit its token.
pub struct FramePaintScope {
    previous: Option<FramePaintToken>,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl FramePaintScope {
    /// Enter a thread-bound scope; None masks any enclosing token until this guard is dropped.
    pub fn enter(token: Option<FramePaintToken>) -> Self {
        Self {
            previous: CURRENT_PAINT.with(|current| current.replace(token)),
            _thread_bound: std::marker::PhantomData,
        }
    }
    /// Clone only the current scope token for this native window; never return a previous completed scope.
    pub fn current(window_id: u64) -> Option<FramePaintToken> {
        CURRENT_PAINT.with(|current| {
            current
                .borrow()
                .as_ref()
                .filter(|token| token.window_id == window_id)
                .cloned()
        })
    }
}
impl Drop for FramePaintScope {
    fn drop(&mut self) {
        CURRENT_PAINT.with(|current| current.replace(self.previous.take()));
    }
}
