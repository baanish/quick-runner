pub mod alias;
pub mod config_cmd;
pub mod do_cmd;
pub mod doctor;
pub mod go;
pub mod learn;
pub mod run;
pub mod stats;

use std::process::ExitStatus;

/// The process exit code, mapping a signal death to the shell convention
/// `128 + signal` (130 = SIGINT/Ctrl-C, 137 = SIGKILL, 139 = SIGSEGV) instead of
/// a flat 1, so callers can tell a signal-killed child from a real exit 1.
pub fn exit_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    1
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::exit_code;

    #[test]
    fn exit_code_returns_the_real_code() {
        let status = Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 3")
            .status()
            .unwrap();
        assert_eq!(exit_code(status), 3);
    }

    #[cfg(unix)]
    #[test]
    fn exit_code_maps_a_signal_to_128_plus_signal() {
        // A child killed by SIGINT (2) must map to 130, not a flat 1.
        let status = Command::new("/bin/sh")
            .arg("-c")
            .arg("kill -INT $$")
            .status()
            .unwrap();
        assert_eq!(exit_code(status), 130);
    }
}
