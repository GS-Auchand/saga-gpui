//! Scoped macOS Secure Event Input. Native behavior is UNVALIDATED until the
//! five lifecycle checks run on macOS; the pure tests prove only ownership.
#[derive(Default)]
struct ProcessState {
    owners: usize,
    app_active: bool,
    enabled: bool,
}
impl ProcessState {
    fn sync(&mut self, mut apply: impl FnMut(bool) -> bool) {
        let wanted = self.app_active && self.owners != 0;
        if wanted != self.enabled && apply(wanted) {
            self.enabled = wanted;
        }
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::ProcessState;
    use parking_lot::Mutex;
    static PROCESS: Mutex<ProcessState> = Mutex::new(ProcessState {
        owners: 0,
        app_active: false,
        enabled: false,
    });
    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn EnableSecureEventInput() -> i32;
        fn DisableSecureEventInput() -> i32;
    }
    fn apply(enable: bool) -> bool {
        // SAFETY: process-level calls are serialized and paired by PROCESS.
        // They take no pointers and are invoked on the AppKit lifecycle thread.
        let status = unsafe {
            if enable {
                EnableSecureEventInput()
            } else {
                DisableSecureEventInput()
            }
        };
        if status != 0 {
            if enable {
                log::error!(target: "gpui::secure_input", "macos-secure-input-enable-failed");
            } else {
                log::error!(target: "gpui::secure_input", "macos-secure-input-disable-failed");
            }
        }
        status == 0
    }
    pub(crate) fn set_app_active(active: bool) {
        let mut state = PROCESS.lock();
        state.app_active = active;
        state.sync(apply);
    }
    #[derive(Default)]
    pub(crate) struct Lease {
        held: bool,
    }
    impl Lease {
        pub(crate) fn set_requested(&mut self, requested: bool) {
            if self.held == requested {
                return;
            }
            let mut state = PROCESS.lock();
            if requested {
                state.owners += 1;
            } else {
                state.owners -= 1;
            }
            self.held = requested;
            state.sync(apply);
        }
    }
    impl Drop for Lease {
        fn drop(&mut self) {
            self.set_requested(false);
        }
    }
}
#[cfg(target_os = "macos")]
pub(crate) use native::{Lease, set_app_active};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn password_focus_in() {
        let mut state = ProcessState {
            owners: 1,
            app_active: true,
            ..Default::default()
        };
        let mut calls = vec![];
        state.sync(|on| {
            calls.push(on);
            true
        });
        state.sync(|on| {
            calls.push(on);
            true
        });
        assert_eq!(calls, vec![true]);
        assert!(state.enabled);
    }
    #[test]
    fn password_focus_out() {
        let mut state = ProcessState {
            owners: 1,
            app_active: true,
            enabled: true,
        };
        state.owners = 0;
        let mut calls = vec![];
        state.sync(|on| {
            calls.push(on);
            true
        });
        assert_eq!(calls, vec![false]);
        assert!(!state.enabled);
    }
    #[test]
    fn window_close() {
        let mut state = ProcessState {
            owners: 2,
            app_active: true,
            enabled: true,
        };
        state.owners -= 1;
        let mut calls = vec![];
        state.sync(|on| {
            calls.push(on);
            true
        });
        assert!(calls.is_empty(), "closing one owner must not release another");
        state.owners -= 1;
        state.sync(|on| {
            calls.push(on);
            true
        });
        assert_eq!(calls, vec![false]);
    }
    #[test]
    fn app_deactivation() {
        let mut state = ProcessState {
            owners: 1,
            app_active: true,
            enabled: true,
        };
        state.app_active = false;
        let mut calls = vec![];
        state.sync(|on| {
            calls.push(on);
            true
        });
        assert_eq!(calls, vec![false]);
        assert_eq!(state.owners, 1);
    }
    #[test]
    fn ordinary_field_restores_normal_input() {
        let mut state = ProcessState {
            owners: 1,
            app_active: true,
            enabled: true,
        };
        state.owners = 0;
        let mut calls = vec![];
        state.sync(|on| {
            calls.push(on);
            true
        });
        state.app_active = false;
        state.sync(|on| {
            calls.push(on);
            true
        });
        state.app_active = true;
        state.sync(|on| {
            calls.push(on);
            true
        });
        assert_eq!(calls, vec![false]);
        assert!(!state.enabled);
    }
}
