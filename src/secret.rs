//! Thin wrapper over the OS keychain (macOS Keychain, Linux Secret Service,
//! Windows Credential Manager) via the `keyring` crate.
//!
//! Every operation degrades gracefully: when no keychain backend is available
//! (a headless CI runner, no Secret Service on Linux, etc.) reads return `None`
//! and writes return an `Err` the caller can surface — nothing panics, and the
//! rest of `qr` keeps working from environment variables or config.

use std::sync::Once;

const SERVICE: &str = "quick-runner";

/// Keychain account name for an API key. `role` distinguishes primary vs fallback
/// when both use the same env-var name (e.g. two openai-compatible endpoints).
pub fn account_for(role: &str, api_key_env: &str, default: &str) -> String {
    let base = if api_key_env.trim().is_empty() {
        default.to_string()
    } else {
        api_key_env.to_string()
    };
    format!("{role}:{base}")
}

/// Look up a role-scoped keychain entry, falling back to the legacy bare account
/// for primary keys stored before role-prefixing shipped.
pub fn get_for_role(role: &str, api_key_env: &str, default: &str) -> Option<String> {
    get(&account_for(role, api_key_env, default)).or_else(|| {
        if role == "primary" {
            get(&legacy_account(api_key_env, default))
        } else {
            None
        }
    })
}

fn legacy_account(api_key_env: &str, default: &str) -> String {
    if api_key_env.trim().is_empty() {
        default.to_string()
    } else {
        api_key_env.to_string()
    }
}

/// Fetch a stored secret by account name. Returns `None` if there is no entry or
/// the keychain is unavailable for any reason.
pub fn get(account: &str) -> Option<String> {
    configure_test_backend();
    keyring::Entry::new(SERVICE, account)
        .ok()?
        .get_password()
        .ok()
}

/// Store a secret under the given account name. Returns an error (rather than
/// panicking) when the keychain is unavailable, so `qr init` can fall back to
/// storing the key in the config file.
pub fn set(account: &str, secret: &str) -> anyhow::Result<()> {
    configure_test_backend();
    keyring::Entry::new(SERVICE, account)
        .and_then(|entry| entry.set_password(secret))
        .map_err(|error| anyhow::anyhow!("keychain unavailable: {error}"))
}

fn configure_test_backend() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if std::env::var_os("QR_TEST_USE_MOCK_KEYCHAIN").is_some() {
            keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_and_fallback_accounts_differ_when_env_name_matches() {
        assert_eq!(
            account_for("primary", "", "OPENAI_API_KEY"),
            "primary:OPENAI_API_KEY"
        );
        assert_eq!(
            account_for("fallback", "", "OPENAI_API_KEY"),
            "fallback:OPENAI_API_KEY"
        );
        assert_ne!(
            account_for("primary", "", "OPENAI_API_KEY"),
            account_for("fallback", "", "OPENAI_API_KEY")
        );
    }
}
