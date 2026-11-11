//! Phase 0 Windows-only credential entry. No persistence, logging or authentication.
use gpui::NativeCredentials;
use windows::{
    Win32::{
        Foundation::{ERROR_CANCELLED, ERROR_SUCCESS, HWND},
        Security::Credentials::{
            CREDUI_FLAGS_ALWAYS_SHOW_UI, CREDUI_FLAGS_DO_NOT_PERSIST,
            CREDUI_FLAGS_GENERIC_CREDENTIALS, CREDUI_INFOW, CredUIPromptForCredentialsW,
        },
    },
    core::{BOOL, w},
};
use zeroize::Zeroizing;

// Fixed allocation includes a terminator. Consumer validates its protocol limits;
// no truncation or lossy UTF-16 conversion is permitted before authentication.
const ACCOUNT_UNITS: usize = 514;
// Windows SDK 10.0.26100.0 um/wincred.h:718: CREDUI_MAX_PASSWORD_LENGTH=(512/2).
const PASSWORD_UNITS: usize = 257;

pub(crate) fn prompt(owner: HWND) -> anyhow::Result<Option<NativeCredentials>> {
    // Called only by show_dialog's dedicated STA thread. Its owner lock keeps
    // HWND alive, and the foreground executor continues pumping owner messages.
    let info = CREDUI_INFOW {
        cbSize: std::mem::size_of::<CREDUI_INFOW>() as u32,
        hwndParent: owner,
        pszMessageText: w!("Enter your Saga account credentials. They will not be saved."),
        pszCaptionText: w!("Saga Sign In"),
        ..Default::default()
    };
    collect(|account, password| {
        let mut save = BOOL(0);
        // SAFETY: owner is protected by DialogOwner until this call returns;
        // info/static strings and writable bounded buffers outlive the call.
        // All credential buffers are zeroized by RAII on success/error/cancel.
        unsafe {
            CredUIPromptForCredentialsW(
                Some(&info),
                w!("Saga Phase 0"),
                None,
                0,
                account,
                password,
                Some(&mut save),
                CREDUI_FLAGS_GENERIC_CREDENTIALS
                    | CREDUI_FLAGS_DO_NOT_PERSIST
                    | CREDUI_FLAGS_ALWAYS_SHOW_UI,
            )
        }
    })
}

fn collect(
    invoke: impl FnOnce(&mut [u16], &mut [u16]) -> windows::Win32::Foundation::WIN32_ERROR,
) -> anyhow::Result<Option<NativeCredentials>> {
    let mut account = Zeroizing::new(vec![0u16; ACCOUNT_UNITS]);
    let mut password = Zeroizing::new(vec![0u16; PASSWORD_UNITS]);
    let status = invoke(&mut account, &mut password);
    if status == ERROR_CANCELLED {
        return Ok(None);
    }
    if status != ERROR_SUCCESS {
        return Err(anyhow::anyhow!("native credential entry failed"));
    }
    let credentials = NativeCredentials::from_utf16_buffers(account, password)?;
    // A native control may have limited pasted/typed input. Reject a full field
    // rather than authenticate an ambiguous potentially truncated password.
    if credentials.account_utf16().len() >= ACCOUNT_UNITS - 1
        || credentials.password_utf16().len() >= PASSWORD_UNITS - 1
    {
        return Err(anyhow::anyhow!("native credential field reached its limit"));
    }
    Ok(Some(credentials))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_limit_reaching_output_without_truncating() {
        assert!(
            collect(|_, secret| {
                secret[..PASSWORD_UNITS - 1].fill(65);
                ERROR_SUCCESS
            })
            .is_err()
        );
        assert!(
            collect(|account, _| {
                account[..ACCOUNT_UNITS - 1].fill(65);
                ERROR_SUCCESS
            })
            .is_err()
        );
        let value = collect(|_, secret| {
            secret[..PASSWORD_UNITS - 2].fill(65);
            ERROR_SUCCESS
        })
        .expect("below limit")
        .expect("credentials");
        assert_eq!(value.password_utf16().len(), PASSWORD_UNITS - 2);
    }
    #[test]
    fn scripted_cancel_error_and_success_never_open_a_dialog() {
        assert!(
            collect(|_, secret| {
                secret[0] = 65;
                ERROR_CANCELLED
            })
            .expect("cancel")
            .is_none()
        );
        assert!(
            collect(|_, secret| {
                secret[0] = 65;
                windows::Win32::Foundation::ERROR_ACCESS_DENIED
            })
            .is_err()
        );
        let value = collect(|account, secret| {
            account[0] = 65;
            secret[0] = 66;
            ERROR_SUCCESS
        })
        .expect("scripted success")
        .expect("credentials");
        assert_eq!(value.account_utf16(), &[65]);
        assert_eq!(value.password_utf16(), &[66]);
        assert!(
            collect(|account, _| {
                account.fill(65);
                ERROR_SUCCESS
            })
            .is_err()
        );
    }
}
