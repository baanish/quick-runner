//! Thin wrapper over the OS keychain (macOS Keychain, Linux Secret Service,
//! Windows Credential Manager) via the `keyring` crate.
//!
//! Every operation degrades gracefully: when no keychain backend is available
//! (a headless CI runner, no Secret Service on Linux, etc.) reads return `None`
//! and writes return an `Err` the caller can surface — nothing panics, and the
//! rest of `qr` keeps working from environment variables or config.

const SERVICE: &str = "quick-runner";

/// Keychain account name for an API key: the custom env-var name when one is
/// configured, otherwise `default` (the protocol's well-known env var). Keeps
/// `qr init`'s store and the AI client's lookup in agreement.
pub fn account_for(api_key_env: &str, default: &str) -> String {
    if api_key_env.trim().is_empty() {
        default.to_string()
    } else {
        api_key_env.to_string()
    }
}

/// Fetch a stored secret by account name. Returns `None` if there is no entry or
/// the keychain is unavailable for any reason.
pub fn get(account: &str) -> Option<String> {
    keyring::Entry::new(SERVICE, account)
        .ok()?
        .get_password()
        .ok()
}

/// Store a secret under the given account name. Returns an error (rather than
/// panicking) when the keychain is unavailable, so `qr init` can fall back to
/// storing the key in the config file.
pub fn set(account: &str, secret: &str) -> anyhow::Result<()> {
    keyring::Entry::new(SERVICE, account)
        .and_then(|entry| entry.set_password(secret))
        .map_err(|error| anyhow::anyhow!("keychain unavailable: {error}"))
}
