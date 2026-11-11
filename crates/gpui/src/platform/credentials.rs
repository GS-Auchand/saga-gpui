//! Owned UTF-16 credential buffers. Deliberately no Debug, Clone or serialization.
use zeroize::Zeroizing;

/// Native credential output, zeroized on drop (including unused buffer capacity).
/// Borrow only for authentication; copies made by consumers need their own cleanup.
pub struct NativeCredentials {
    account: Zeroizing<Vec<u16>>,
    password: Zeroizing<Vec<u16>>,
    account_len: usize,
    password_len: usize,
}

impl NativeCredentials {
    /// Adopt bounded, NUL-terminated native output buffers without making copies.
    /// Errors drop and zeroize both buffers; no buffer contents enter the error.
    pub fn from_utf16_buffers(
        account: Zeroizing<Vec<u16>>,
        password: Zeroizing<Vec<u16>>,
    ) -> anyhow::Result<Self> {
        let account_len = account.iter().position(|unit| *unit == 0);
        let password_len = password.iter().position(|unit| *unit == 0);
        match (account_len, password_len) {
            (Some(account_len), Some(password_len)) => Ok(Self {
                account,
                password,
                account_len,
                password_len,
            }),
            _ => Err(anyhow::anyhow!("invalid native credential output")),
        }
    }

    /// Account UTF-16, excluding the terminator. Never log this value.
    pub fn account_utf16(&self) -> &[u16] {
        &self.account[..self.account_len]
    }

    /// Password UTF-16, excluding the terminator. Never log this value.
    pub fn password_utf16(&self) -> &[u16] {
        &self.password[..self.password_len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_output_keeps_unicode_and_rejects_missing_terminators() {
        let value = NativeCredentials::from_utf16_buffers(
            Zeroizing::new(vec![65, 0, 99]),
            Zeroizing::new(vec![0xd83d, 0xde00, 0, 99]),
        )
        .expect("synthetic terminated buffers");
        assert_eq!(value.account_utf16(), &[65]);
        assert_eq!(value.password_utf16(), &[0xd83d, 0xde00]);
        assert!(
            NativeCredentials::from_utf16_buffers(
                Zeroizing::new(vec![65]),
                Zeroizing::new(vec![0]),
            )
            .is_err()
        );
    }
}
