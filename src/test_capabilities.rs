//! Lightweight capability probes used by tests in constrained environments.
//!
//! These helpers let tests skip gracefully when sandbox restrictions prevent
//! operations like loopback binds or writable-home access.

use std::env;
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Return the configured home directory from environment variables.
pub fn home_dir_from_env() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Check that a directory is writable by creating and deleting a tiny probe file.
pub fn check_writable_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|err| {
        format!(
            "failed to create directory {} for capability probe: {err}",
            path.display()
        )
    })?;

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let probe_name = format!(".llamafarm-capability-probe-{}-{nanos}", std::process::id());
    let probe_path = path.join(probe_name);

    fs::write(&probe_path, b"probe")
        .map_err(|err| format!("failed to write probe file {}: {err}", probe_path.display()))?;

    if let Err(err) = fs::remove_file(&probe_path) {
        return Err(format!(
            "failed to clean up probe file {}: {err}",
            probe_path.display()
        ));
    }

    Ok(())
}

/// Verify loopback bind capability for local mock servers used in tests.
pub fn check_loopback_bind() -> Result<(), String> {
    TcpListener::bind("127.0.0.1:0")
        .map(|listener| {
            drop(listener);
        })
        .map_err(|err| format!("loopback bind unavailable: {err}"))
}
