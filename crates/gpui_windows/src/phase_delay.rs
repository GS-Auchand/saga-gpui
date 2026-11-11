//! Native timer ownership stays on the VSyncProvider thread, never on the UI thread.
use gpui::PhaseDelayBackend;
use gpui_util::ResultExt;
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_FAILED},
        System::Threading::{
            CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW,
            SYNCHRONIZATION_SYNCHRONIZE, SetWaitableTimerEx, TIMER_MODIFY_STATE,
            WaitForSingleObject,
        },
    },
    core::PCWSTR,
};

#[derive(Default)]
pub(crate) struct PhaseDelayTimer {
    handle: Option<HANDLE>,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl PhaseDelayBackend for PhaseDelayTimer {
    fn create(&mut self) -> Result<(), i32> {
        if self.handle.is_some() {
            return Ok(());
        }
        let handle = unsafe {
            CreateWaitableTimerExW(
                None,
                PCWSTR::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                (TIMER_MODIFY_STATE | SYNCHRONIZATION_SYNCHRONIZE).0,
            )
        }
        .map_err(|error| error.code().0)?;
        self.handle = Some(handle);
        Ok(())
    }
    fn arm(&mut self, due_time: i64) -> Result<(), i32> {
        let Some(handle) = self.handle else {
            return Err(0x80070006u32 as i32);
        };
        unsafe { SetWaitableTimerEx(handle, &due_time, 0, None, None, None, 0) }
            .map_err(|error| error.code().0)
    }
    fn wait(&mut self, timeout_ms: u32) -> Result<u32, i32> {
        let Some(handle) = self.handle else {
            return Err(0x80070006u32 as i32);
        };
        let result = unsafe { WaitForSingleObject(handle, timeout_ms) };
        if result == WAIT_FAILED {
            Err(windows::core::Error::from_thread().code().0)
        } else {
            Ok(result.0)
        }
    }
}
impl Drop for PhaseDelayTimer {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            // Only this thread owns the handle, and its finite wait has already returned.
            unsafe { CloseHandle(handle) }.log_err();
        }
    }
}
