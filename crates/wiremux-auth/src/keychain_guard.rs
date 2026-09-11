//! Process-global keychain disable counter (IsolatedHome and tests).

use std::sync::atomic::{AtomicU32, Ordering};

static KEYCHAIN_DISABLES: AtomicU32 = AtomicU32::new(0);

/// Skip live keychain reads until this guard is dropped.
#[must_use]
#[allow(dead_code)] // constructed from IsolatedHome (`test` / `test-util`)
pub struct KeychainIsolation {
    _private: (),
}

impl KeychainIsolation {
    /// Disable keychain lookups for the lifetime of the returned guard.
    #[allow(dead_code)]
    pub fn hold() -> Self {
        KEYCHAIN_DISABLES.fetch_add(1, Ordering::SeqCst);
        Self { _private: () }
    }
}

impl Drop for KeychainIsolation {
    fn drop(&mut self) {
        KEYCHAIN_DISABLES.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(crate) fn keychain_disabled() -> bool {
    KEYCHAIN_DISABLES.load(Ordering::SeqCst) > 0
}
