//! UI ⇄ daemon protocol: two JSON files plus mtime polling.
//!
//! The UI is the only writer of `desired.json` (what calibration *should* be);
//! the daemon is the only writer of `status.json` (what calibration *is*).
//! Both writes are atomic — temp file plus rename in the same directory — so a
//! reader polling mtime can never observe a torn file. This mirrors the
//! Windows backend, where the APO watches a config file the app rewrites; a
//! file-based contract needs no sockets or D-Bus and therefore works
//! identically in a .deb, a Flatpak, and a strict Snap.

use crate::state::StoredState;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;

/// What calibration should be. `revision` strictly increases with every write,
/// so the daemon can acknowledge exactly what it applied
/// ([`ServiceStatus::revision_applied`]) and the UI can wait for its own write
/// to take effect.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DesiredState {
    pub revision: u64,
    /// `false` = calibration off: module unloaded, default sink restored to
    /// the physical target.
    pub enabled: bool,
    /// Profile / target / tone, in the same on-disk shape as the old
    /// `state.json` (so a legacy file can be migrated field-for-field).
    #[serde(flatten)]
    pub state: StoredState,
}

/// What calibration is — rewritten by the daemon after every reconcile pass.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ServiceStatus {
    /// The [`DesiredState::revision`] this status reflects.
    pub revision_applied: u64,
    /// Filter module loaded and its virtual sink published.
    pub active: bool,
    /// PipeWire node id of the published virtual sink, when active.
    pub virtual_node_id: Option<u32>,
    /// Why the last reconcile failed; `None` after a success. This is what the
    /// UI surfaces instead of the old `journalctl` tail.
    pub error: Option<String>,
    /// Daemon pid, for diagnostics.
    pub pid: u32,
    /// Convenience mirror of `wet <= 0` so the UI status line can note bypass
    /// without re-reading the desired file.
    pub bypassed: bool,
}

/// Read cap shared with core's state reader: a hostile or corrupt file fails
/// to parse instead of exhausting memory.
const MAX_LEN: u64 = 8 << 20;

/// Read and parse a JSON file, or `None` if missing/oversized/unparseable.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let mut buf = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_LEN)
        .read_to_string(&mut buf)
        .ok()?;
    serde_json::from_str(&buf).ok()
}

/// Serialize `value` to `path` atomically: write a temp file in the same
/// directory (rename is only atomic within one filesystem), then rename over
/// the destination. Creates the parent directory if needed.
pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("path has no parent directory"))?;
    std::fs::create_dir_all(dir)?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    std::fs::write(&tmp, json + "\n")?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use adtune_core::{AudioProfile, ToneSettings};

    #[test]
    fn desired_state_roundtrips() {
        let desired = DesiredState {
            revision: 7,
            enabled: true,
            state: StoredState::new(
                &AudioProfile::default(),
                42,
                "alsa_output.usb",
                &ToneSettings::default(),
            ),
        };
        let dir = std::env::temp_dir().join(format!("adtune-ipc-test-{}", std::process::id()));
        let path = dir.join("desired.json");
        write_json_atomic(&path, &desired).unwrap();
        let back: DesiredState = read_json(&path).unwrap();
        assert_eq!(back.revision, 7);
        assert!(back.enabled);
        assert_eq!(back.state.target_name, "alsa_output.usb");
        assert_eq!(back.state.target_id, 42);
        // No stray temp file left behind.
        assert!(!dir.join("desired.json.tmp").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn legacy_state_json_parses_as_desired_fields() {
        // The flattened `state` keeps the old state.json field names, so a
        // legacy file plus the two new fields is a valid DesiredState.
        let json = r#"{
            "revision": 1, "enabled": true,
            "profile": {"key":"k","name":"n","bands":[]},
            "target_id": 55, "target_name": "sink",
            "tone": {"wet":1.0,"bass":0.0,"tilt":0.0,"headroom":true},
            "bypassed": false
        }"#;
        let d: DesiredState = serde_json::from_str(json).unwrap();
        assert_eq!(d.state.target_id, 55);
    }

    #[test]
    fn status_roundtrips_with_error() {
        let status = ServiceStatus {
            revision_applied: 3,
            active: false,
            virtual_node_id: None,
            error: Some("module load failed".into()),
            pid: 1234,
            bypassed: false,
        };
        let dir =
            std::env::temp_dir().join(format!("adtune-ipc-status-test-{}", std::process::id()));
        let path = dir.join("status.json");
        write_json_atomic(&path, &status).unwrap();
        let back: ServiceStatus = read_json(&path).unwrap();
        assert_eq!(back.revision_applied, 3);
        assert_eq!(back.error.as_deref(), Some("module load failed"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
