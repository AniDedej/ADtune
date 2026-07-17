//! XDG-correct filesystem locations shared by the UI and `adtune-service`.
//!
//! Inside Flatpak/Snap, `XDG_CONFIG_HOME` / `XDG_RUNTIME_DIR` resolve to
//! app-private directories that the UI and the daemon share (they run in the
//! same sandbox), so the same helpers work confined and unconfined. The legacy
//! helpers at the bottom deliberately bypass XDG: they point at where versions
//! ≤ 1.0 wrote files, and exist only so the daemon can migrate/clean them up.

use std::path::PathBuf;

/// An environment variable as an absolute path. Relative values are ignored,
/// as the XDG base-directory spec requires.
fn env_abs(var: &str) -> Option<PathBuf> {
    let p = std::env::var_os(var).map(PathBuf::from)?;
    p.is_absolute().then_some(p)
}

/// The user's home directory, or an empty path if `$HOME` is unset.
/// [`require_home`] guards against the empty case before any file write.
pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// Fail before any file write when `$HOME` is unset or not absolute. Otherwise
/// the config fallbacks below would resolve to CWD-relative locations that the
/// UI and the daemon would each interpret differently — a silent, confusing
/// breakage.
pub fn require_home() -> Result<(), String> {
    let h = home();
    if h.as_os_str().is_empty() || !h.is_absolute() {
        return Err(
            "HOME is not set to an absolute path; cannot locate the ADtune config directory."
                .into(),
        );
    }
    Ok(())
}

/// ADtune's config directory: `$XDG_CONFIG_HOME/adtune`, defaulting to
/// `~/.config/adtune`. Holds `desired.json` and the profile library.
pub fn config_dir() -> PathBuf {
    env_abs("XDG_CONFIG_HOME")
        .unwrap_or_else(|| home().join(".config"))
        .join("adtune")
}

/// Ephemeral runtime directory: `$XDG_RUNTIME_DIR/adtune`, falling back to a
/// subdirectory of the config dir for sessions without a runtime dir. Holds
/// `status.json` and the daemon's single-instance lock — nothing here needs to
/// survive logout.
pub fn runtime_dir() -> PathBuf {
    env_abs("XDG_RUNTIME_DIR")
        .map(|p| p.join("adtune"))
        .unwrap_or_else(|| config_dir().join("runtime"))
}

/// The desired-state file the UI writes and the daemon reconciles.
pub fn desired_path() -> PathBuf {
    config_dir().join("desired.json")
}

/// The status file the daemon writes and the UI reads.
pub fn status_path() -> PathBuf {
    runtime_dir().join("status.json")
}

/// The daemon's single-instance flock target. A held lock means a daemon is
/// alive; the file's presence alone means nothing (stale files are harmless).
pub fn lock_path() -> PathBuf {
    runtime_dir().join("service.lock")
}

/// The user's saved-profile library.
pub fn profiles_dir() -> PathBuf {
    config_dir().join("profiles")
}

// ---- legacy locations (versions ≤ 1.0, systemd architecture) -------------

/// The old persisted state (`state.json`), superseded by `desired.json`.
pub fn legacy_state_path() -> PathBuf {
    config_dir().join("state.json")
}

/// The old rendered filter-chain config that the systemd unit's ExecStart
/// loaded via `pipewire -c`.
pub fn legacy_filter_conf_path() -> PathBuf {
    config_dir().join("adtune-filter.conf")
}

/// The old systemd user unit. Hardcoded `~/.config` on purpose: that is where
/// old versions wrote it, regardless of XDG overrides.
pub fn legacy_unit_path() -> PathBuf {
    home().join(".config/systemd/user/adtune.service")
}
