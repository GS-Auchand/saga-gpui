use windows::{
    Win32::{
        Foundation::HWND,
        System::{
            Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
            Variant::VARIANT,
        },
        UI::{Input::Ime::*, TextServices::*},
    },
    core::{Interface, Result},
};

/// Owned by one native window, used only on its UI thread. No entered text is retained.
#[derive(Default)]
pub(crate) struct SecureInput {
    active: Option<Protection>,
}

struct Protection {
    hwnd: HWND,
    manager: ITfThreadMgr,
    document: ITfDocumentMgr,
    previous: Option<ITfDocumentMgr>,
    previous_ime: HIMC,
}

impl SecureInput {
    pub(crate) fn is_active(&self) -> bool {
        self.active.is_some()
    }

    pub(crate) fn update(&mut self, hwnd: HWND, password: bool) {
        if password == self.is_active() {
            return;
        }
        if password {
            match Protection::acquire(hwnd) {
                Ok(protection) => self.active = Some(protection),
                Err(_) => {
                    log::error!(target: "gpui::secure_input", "windows-protection-acquire-failed")
                }
            }
        } else {
            self.active.take();
        }
    }
}

impl Protection {
    fn acquire(hwnd: HWND) -> Result<Self> {
        unsafe {
            let manager: ITfThreadMgr =
                CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER)?;
            let client = manager.Activate()?;
            let initialized = (|| -> Result<ITfDocumentMgr> {
                let document = manager.CreateDocumentMgr()?;
                let mut context = None;
                let mut cookie = 0;
                document.CreateContext(client, 0, None, &mut context, &mut cookie)?;
                let context = context.ok_or_else(|| {
                    windows::core::Error::from_hresult(windows::Win32::Foundation::E_UNEXPECTED)
                })?;
                let compartments: ITfCompartmentMgr = context.cast()?;
                for guid in [
                    GUID_COMPARTMENT_KEYBOARD_DISABLED,
                    GUID_COMPARTMENT_EMPTYCONTEXT,
                ] {
                    compartments
                        .GetCompartment(&guid)?
                        .SetValue(client, &VARIANT::from(1i32))?;
                }
                document.Push(&context)?;
                Ok(document)
            })();
            let document = match initialized {
                Ok(document) => document,
                Err(error) => {
                    if manager.Deactivate().is_err() {
                        log::error!(target: "gpui::secure_input", "windows-tsf-deactivate-failed");
                    }
                    return Err(error);
                }
            };
            let previous = match associate(&manager, hwnd, Some(&document)) {
                Ok(previous) => previous,
                Err(error) => {
                    if manager.Deactivate().is_err() {
                        log::error!(target: "gpui::secure_input", "windows-tsf-deactivate-failed");
                    }
                    return Err(error);
                }
            };
            // Cancel before dissociation: a normal field's unfinished composition must
            // never be committed to the new password owner.
            let previous_ime = ImmGetContext(hwnd);
            if !previous_ime.is_invalid() {
                if ImmGetCompositionStringW(previous_ime, GCS_COMPSTR, None, 0) > 0
                    && !ImmNotifyIME(previous_ime, NI_COMPOSITIONSTR, CPS_CANCEL, 0).as_bool()
                {
                    log::error!(target: "gpui::secure_input", "windows-ime-cancel-failed");
                }
                if !ImmReleaseContext(hwnd, previous_ime).as_bool() {
                    log::error!(target: "gpui::secure_input", "windows-ime-release-failed");
                }
            }
            let protection = Self {
                hwnd,
                manager,
                document,
                previous,
                previous_ime,
            };
            if !ImmAssociateContextEx(hwnd, HIMC::default(), 0).as_bool() {
                log::error!(target: "gpui::secure_input", "windows-ime-disable-failed");
            }
            if protection.manager.SetFocus(&protection.document).is_err() {
                log::error!(target: "gpui::secure_input", "windows-tsf-focus-failed");
            }
            Ok(protection)
        }
    }
}

impl Drop for Protection {
    fn drop(&mut self) {
        unsafe {
            if associate(&self.manager, self.hwnd, self.previous.as_ref()).is_err() {
                log::error!(target: "gpui::secure_input", "windows-tsf-restore-failed");
            }
            // Restore only our focus. Another window may already own the thread's TSF focus.
            if let Ok(current) = self.manager.GetFocus() {
                if current == self.document
                    && self.manager.SetFocus(self.previous.as_ref()).is_err()
                {
                    log::error!(target: "gpui::secure_input", "windows-tsf-focus-restore-failed");
                }
            }
            ImmAssociateContext(self.hwnd, self.previous_ime);
            let restored = ImmGetContext(self.hwnd);
            if restored != self.previous_ime {
                log::error!(target: "gpui::secure_input", "windows-ime-restore-failed");
            }
            if !restored.is_invalid() && !ImmReleaseContext(self.hwnd, restored).as_bool() {
                log::error!(target: "gpui::secure_input", "windows-ime-release-failed");
            }
            if self.manager.Deactivate().is_err() {
                log::error!(target: "gpui::secure_input", "windows-tsf-deactivate-failed");
            }
        }
    }
}

// windows-rs's convenience wrapper rejects a successful null previous document.
// Null is normal for an IMM-only window, and must remain distinguishable from failure.
unsafe fn associate(
    manager: &ITfThreadMgr,
    hwnd: HWND,
    document: Option<&ITfDocumentMgr>,
) -> Result<Option<ITfDocumentMgr>> {
    let mut previous = std::ptr::null_mut();
    unsafe {
        (manager.vtable().AssociateFocus)(
            manager.as_raw(),
            hwnd,
            document.map_or(std::ptr::null_mut(), Interface::as_raw),
            &mut previous,
        )
        .ok()?;
        Ok(if previous.is_null() {
            None
        } else {
            Some(ITfDocumentMgr::from_raw(previous))
        })
    }
}
