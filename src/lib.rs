pub mod agent_history;
pub mod ai;
pub mod atomic;
pub mod commands;
pub mod config;
pub mod picker;
pub mod pricing;
pub mod project_profile;
pub mod scanner;
pub mod secret;
pub mod shell;
pub mod stats_db;
pub mod terminal;

/// A single process-wide lock for unit tests that mutate environment variables.
/// One shared lock (rather than a per-module copy) is required because env vars
/// are global: separate locks would let a test in one module set a variable while
/// a test in another reads/sets it concurrently in the shared lib-test binary.
#[cfg(test)]
pub(crate) fn test_env_lock() -> &'static std::sync::Mutex<()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}
