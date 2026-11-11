//! Test-only phase experiment policy, separated from native handle ownership for deterministic tests.
use serde::Serialize;

/// Evidence from one optional delay; missing counters invalidate precise timing claims.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PhaseDelayTrace {
    /// Exact accepted delay; None denotes invalid configuration.
    pub requested_us: Option<u32>,
    /// baseline, applied, stopped, failed, or failed-latched.
    pub status: String,
    /// Timer creation result, zero on success, retained across samples.
    pub creation_hresult: Option<i32>,
    /// Counter before arming the timer.
    pub arm_start_qpc: Option<u64>,
    /// Counter after arming the timer.
    pub arm_end_qpc: Option<u64>,
    /// Counter before waiting for the timer.
    pub wait_start_qpc: Option<u64>,
    /// Counter after the wait returns.
    pub wait_end_qpc: Option<u64>,
    /// Arm result, zero on success.
    pub arm_hresult: Option<i32>,
    /// Native wait result: zero means signaled, 258 means timeout.
    pub wait_result: Option<u32>,
    /// Wait failure HRESULT when the native wait fails.
    pub wait_error_hresult: Option<i32>,
    /// First failed stage: config, create, arm, wait, or clock; never retried.
    pub failure_stage: Option<String>,
}
/// Minimal timer operations; implementations retain ownership until no wait is pending.
pub trait PhaseDelayBackend {
    /// Create the high-resolution timer without a lower-resolution fallback.
    fn create(&mut self) -> Result<(), i32>;
    /// Arm a relative one-shot timer in negative 100-nanosecond units.
    fn arm(&mut self, due_time: i64) -> Result<(), i32>;
    /// Wait without an APC callback or spinning, with a finite timeout.
    fn wait(&mut self, timeout_ms: u32) -> Result<u32, i32>;
}
/// Latched experiment policy; zero never invokes any timer operation.
pub struct PhaseDelayController {
    requested: Option<u32>,
    creation: Option<i32>,
    failure: Option<PhaseDelayTrace>,
}
impl PhaseDelayController {
    /// Parse only the exact supported decimal values; absent means baseline zero.
    pub fn new(value: Option<&str>) -> Self {
        let requested = match value {
            None | Some("0") => Some(0),
            Some("3000") => Some(3000),
            Some("6000") => Some(6000),
            Some("9000") => Some(9000),
            Some("12000") => Some(12000),
            _ => None,
        };
        Self {
            requested,
            creation: None,
            failure: None,
        }
    }
    fn fail(&mut self, mut trace: PhaseDelayTrace, stage: &str) -> PhaseDelayTrace {
        trace.status = "failed".into();
        trace.failure_stage = Some(stage.into());
        self.failure = Some(trace.clone());
        trace
    }
    /// Apply once after a provider wait; callers gate this method on opt-in capture.
    /// Stop checks bound added work, but OS scheduling and the provider wait are not real-time guarantees.
    pub fn run(
        &mut self,
        backend: &mut impl PhaseDelayBackend,
        mut clock: impl FnMut() -> Option<u64>,
        mut stopped: impl FnMut() -> bool,
    ) -> PhaseDelayTrace {
        let mut trace = PhaseDelayTrace {
            requested_us: self.requested,
            creation_hresult: self.creation,
            ..Default::default()
        };
        if let Some(failure) = &self.failure {
            trace.status = "failed-latched".into();
            trace.failure_stage = failure.failure_stage.clone();
            trace.arm_hresult = failure.arm_hresult;
            trace.wait_result = failure.wait_result;
            trace.wait_error_hresult = failure.wait_error_hresult;
            return trace;
        }
        let Some(requested) = self.requested else {
            return self.fail(trace, "config");
        };
        if requested == 0 {
            trace.status = "baseline".into();
            return trace;
        }
        if stopped() {
            trace.status = "stopped".into();
            return trace;
        }
        if self.creation.is_none() {
            match backend.create() {
                Ok(()) => {
                    self.creation = Some(0);
                    trace.creation_hresult = Some(0);
                }
                Err(error) => {
                    self.creation = Some(error);
                    trace.creation_hresult = Some(error);
                    return self.fail(trace, "create");
                }
            }
        }
        if stopped() {
            trace.status = "stopped".into();
            return trace;
        }
        trace.arm_start_qpc = clock();
        if trace.arm_start_qpc.is_none() {
            return self.fail(trace, "clock");
        }
        let result = backend.arm(-i64::from(requested) * 10);
        trace.arm_end_qpc = clock();
        trace.arm_hresult = Some(match result {
            Ok(()) => 0,
            Err(error) => error,
        });
        if result.is_err() {
            return self.fail(trace, "arm");
        }
        if stopped() {
            trace.status = "stopped".into();
            return trace;
        }
        trace.wait_start_qpc = clock();
        if trace.arm_end_qpc.is_none()
            || trace.wait_start_qpc.is_none()
            || trace
                .arm_start_qpc
                .zip(trace.arm_end_qpc)
                .is_some_and(|(start, end)| end < start)
            || trace
                .arm_end_qpc
                .zip(trace.wait_start_qpc)
                .is_some_and(|(start, end)| end < start)
        {
            return self.fail(trace, "clock");
        }
        let result = backend.wait(50);
        trace.wait_end_qpc = clock();
        match result {
            Ok(result) => {
                trace.wait_result = Some(result);
                if result != 0 {
                    return self.fail(trace, "wait");
                }
            }
            Err(error) => {
                trace.wait_error_hresult = Some(error);
                return self.fail(trace, "wait");
            }
        }
        if trace.wait_end_qpc.is_none()
            || trace
                .wait_start_qpc
                .zip(trace.wait_end_qpc)
                .is_some_and(|(start, end)| end < start)
        {
            return self.fail(trace, "clock");
        }
        trace.status = if stopped() { "stopped" } else { "applied" }.into();
        trace
    }
}
