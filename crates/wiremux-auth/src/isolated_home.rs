//! Process-global HOME isolation for TokenProvider tests.
//!
//! Not a runtime API. Public only behind the `test-util` feature.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::keychain_guard::{KeychainIsolation, TestKeychain};

/// Acquire before any `HOME` / `USERPROFILE` mutation.
static HOME_TEST_LOCK: Mutex<()> = Mutex::new(());

const COMMON_ENVS: &[&str] = &[
    "WIREMUX_NO_SHIPPED_PRESETS",
    "WIREMUX_PROFILE_DIR",
    "XDG_CONFIG_HOME",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "OPENROUTER_API_KEY",
    "XAI_API_KEY",
    "GROK_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "GOOGLE_OAUTH_ACCESS_TOKEN",
    "CLOUDSDK_CONFIG",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_PROFILE",
    "AWS_DEFAULT_PROFILE",
    "AWS_REGION",
    "AWS_DEFAULT_REGION",
    "AWS_SHARED_CREDENTIALS_FILE",
    "AWS_CONFIG_FILE",
    "AWS_BEARER_TOKEN_BEDROCK",
    "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
    "AWS_CONTAINER_CREDENTIALS_FULL_URI",
    "AWS_CONTAINER_AUTHORIZATION_TOKEN",
    "AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE",
    "AWS_EC2_METADATA_DISABLED",
    "AWS_EC2_METADATA_SERVICE_ENDPOINT",
    "AWS_EC2_METADATA_V1_DISABLED",
    "AWS_ENDPOINT_URL",
    "AWS_ENDPOINT_URL_SSO",
    "AWS_SSO_CACHE_DIR",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
];

/// Isolate `HOME` / `USERPROFILE` / common API-key envs / keychain.
///
/// The process-global mutex is acquired **before** the tempdir is created
/// so parallel tests cannot race on `std::env::set_var`.
pub struct IsolatedHome {
    dir: tempfile::TempDir,
    saved: Vec<(OsString, Option<OsString>)>,
    _keychain: KeychainIsolation,
    _test_keychain: TestKeychain,
    _lock: std::sync::MutexGuard<'static, ()>,
}

/// What [`IsolatedHome::plant_credentials`] writes under the isolated home.
#[derive(Debug, Clone)]
pub enum PlantCredentials<'a> {
    /// Nested Claude `~/.claude/.credentials.json` plus a keep-me field.
    Claude {
        /// Access token.
        access: &'a str,
        /// Optional refresh token.
        refresh: Option<&'a str>,
        /// Optional `expiresAt` milliseconds. Default is far-future.
        expires_at_ms: Option<u64>,
    },
    /// Generic JSON document at `relative_path` under HOME.
    JsonPointer {
        /// Path relative to the isolated home.
        relative_path: &'a str,
        /// Full document to write.
        document: serde_json::Value,
    },
}

impl IsolatedHome {
    /// Lock HOME, create a temp dir, point HOME/USERPROFILE at it, clear keys.
    pub fn new() -> Self {
        Self::with_extra_envs(&[])
    }

    /// Same as [`Self::new`], also clearing `extra` environment names.
    pub fn with_extra_envs(extra: &[&str]) -> Self {
        let lock = HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("IsolatedHome tempdir");

        let mut names: Vec<&str> = COMMON_ENVS.to_vec();
        names.extend_from_slice(extra);
        names.push("HOME");
        names.push("USERPROFILE");
        names.push("APPDATA");
        names.push("LOCALAPPDATA");

        let mut saved = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for name in names {
            if !seen.insert(name) {
                continue;
            }
            let key = OsString::from(name);
            saved.push((key.clone(), std::env::var_os(&key)));
        }

        // SAFETY: HOME_TEST_LOCK is held for the lifetime of this value.
        // Drop restores every saved binding. Tests must not call set_var
        // without this lock.
        unsafe {
            std::env::set_var("HOME", dir.path());
            std::env::set_var("USERPROFILE", dir.path());
            std::env::set_var("APPDATA", dir.path());
            std::env::set_var("LOCALAPPDATA", dir.path());
            for name in COMMON_ENVS.iter().chain(extra.iter()) {
                std::env::remove_var(name);
            }
        }

        Self {
            dir,
            saved,
            _keychain: KeychainIsolation::hold(),
            _test_keychain: TestKeychain::hold(),
            _lock: lock,
        }
    }

    /// Isolated home directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Set an env var while the isolation lock is held. Restored on drop
    /// when `key` was already tracked (common API keys or `with_extra_envs`).
    pub fn set_env(&self, key: &str, value: &str) {
        // SAFETY: lock is held by `self`.
        unsafe {
            std::env::set_var(key, value);
        }
    }

    /// Plant a `claude-credentials` or `json-pointer` store under HOME.
    pub fn plant_credentials(&self, spec: PlantCredentials<'_>) -> PathBuf {
        match spec {
            PlantCredentials::Claude {
                access,
                refresh,
                expires_at_ms,
            } => {
                let path = PathBuf::from(".claude/.credentials.json");
                let abs = self.path().join(&path);
                if let Some(parent) = abs.parent() {
                    std::fs::create_dir_all(parent).expect("mkdir .claude");
                }
                let refresh = refresh.unwrap_or("");
                let expires = expires_at_ms.unwrap_or(4_000_000_000_000);
                let doc = serde_json::json!({
                    "claudeAiOauth": {
                        "accessToken": access,
                        "refreshToken": refresh,
                        "expiresAt": expires
                    },
                    "otherField": "keep-me"
                });
                std::fs::write(&abs, serde_json::to_vec_pretty(&doc).expect("json"))
                    .expect("plant claude credentials");
                abs
            }
            PlantCredentials::JsonPointer {
                relative_path,
                document,
            } => {
                let abs = self.path().join(relative_path);
                if let Some(parent) = abs.parent() {
                    std::fs::create_dir_all(parent).expect("mkdir plant parents");
                }
                std::fs::write(&abs, serde_json::to_vec_pretty(&document).expect("json"))
                    .expect("plant json-pointer store");
                abs
            }
        }
    }

    /// Plant a JSON secret in the in-memory test keychain.
    pub fn plant_keychain(&self, service: &str, account: &str, secret: &serde_json::Value) {
        crate::keychain_guard::test_keychain_set(
            service,
            account,
            &serde_json::to_string(secret).expect("keychain json"),
        );
    }
}

impl Default for IsolatedHome {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        // SAFETY: still holding HOME_TEST_LOCK.
        unsafe {
            for (key, prev) in self.saved.drain(..) {
                match prev {
                    Some(v) => std::env::set_var(&key, v),
                    None => std::env::remove_var(&key),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolated_home_sets_and_restores_home() {
        let before = std::env::var_os("HOME");
        {
            let home = IsolatedHome::new();
            assert_eq!(
                std::env::var_os("HOME").as_deref(),
                Some(home.path().as_os_str())
            );
            assert!(crate::keychain_guard::keychain_disabled());
        }
        assert_eq!(std::env::var_os("HOME"), before);
        // Other tests may still hold a disable; this test's guard dropped.
    }

    #[test]
    fn plant_claude_and_json_pointer() {
        let home = IsolatedHome::new();
        let claude = home.plant_credentials(PlantCredentials::Claude {
            access: "sk-ant-oat01-plant",
            refresh: Some("rt-plant"),
            expires_at_ms: None,
        });
        assert!(claude.starts_with(home.path()));
        let text = std::fs::read_to_string(&claude).unwrap();
        assert!(text.contains("sk-ant-oat01-plant"));
        assert!(text.contains("keep-me"));

        let json = home.plant_credentials(PlantCredentials::JsonPointer {
            relative_path: ".config/wiremux/other-vendor.json",
            document: serde_json::json!({"tokens":{"access":"a","refresh":"r"}}),
        });
        assert!(json.ends_with("other-vendor.json"));
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
        assert_eq!(doc["tokens"]["access"].as_str(), Some("a"));
    }
}
