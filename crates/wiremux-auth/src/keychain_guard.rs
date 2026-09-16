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

#[cfg_attr(not(any(feature = "net", test)), allow(dead_code))]
pub(crate) fn keychain_disabled() -> bool {
    KEYCHAIN_DISABLES.load(Ordering::SeqCst) > 0
}

/// In-memory keychain for IsolatedHome tests. OS keyring stays disabled.
#[cfg(any(test, feature = "test-util"))]
mod test_store {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    static STORE: Mutex<BTreeMap<(String, String), String>> = Mutex::new(BTreeMap::new());
    static HOLDS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    pub struct TestKeychain {
        _private: (),
    }

    impl TestKeychain {
        pub fn hold() -> Self {
            HOLDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Self { _private: () }
        }
    }

    impl Drop for TestKeychain {
        fn drop(&mut self) {
            let left = HOLDS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            if left == 1 {
                STORE.lock().unwrap_or_else(|e| e.into_inner()).clear();
            }
        }
    }

    #[cfg_attr(not(any(feature = "net", test)), allow(dead_code))]
    pub fn active() -> bool {
        HOLDS.load(std::sync::atomic::Ordering::SeqCst) > 0
    }

    #[cfg_attr(not(any(feature = "net", test)), allow(dead_code))]
    pub fn get(service: &str, account: &str) -> Option<String> {
        STORE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(service.to_owned(), account.to_owned()))
            .cloned()
    }

    pub fn set(service: &str, account: &str, secret: &str) {
        STORE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((service.to_owned(), account.to_owned()), secret.to_owned());
    }
}

#[cfg(any(test, feature = "test-util"))]
pub use test_store::TestKeychain;

#[cfg(any(test, feature = "test-util"))]
#[cfg_attr(not(any(feature = "net", test)), allow(dead_code, unused_imports))]
pub(crate) use test_store::{
    active as test_keychain_active, get as test_keychain_get, set as test_keychain_set,
};
