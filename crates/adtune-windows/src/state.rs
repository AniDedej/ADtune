//! Windows persisted state (`state.json`). The serde model is shared via
//! `adtune_core::state`; here `target_id` is the Windows audio endpoint id
//! (`IMMDevice::GetId`, rename-proof), so the state is specialized to `String`.

use adtune_core::state::StoredState as CoreState;
use adtune_core::{AudioProfile, ToneSettings};
use std::path::Path;

/// The shared state model with the target-id type pinned to `String` (a Windows
/// audio endpoint id). The Linux backend instantiates the same generic with its
/// own device-id type.
pub type StoredState = CoreState<String>;

/// Load persisted state, or `None` if the file is missing or unparseable (a bad
/// or partial file is treated as "no state", never an error).
pub fn read_state(path: &Path) -> Option<StoredState> {
    adtune_core::state::read_state(path)
}

/// Persist the current profile, target endpoint, and tone to `path`.
pub fn write_state(
    path: &Path,
    profile: &AudioProfile,
    target_id: &str,
    target_name: &str,
    tone: &ToneSettings,
) -> std::io::Result<()> {
    adtune_core::state::write_state(
        path,
        &StoredState::new(profile, target_id.to_string(), target_name, tone),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use adtune_core::{BandType, FilterBand};

    #[test]
    fn state_round_trips() {
        let p = AudioProfile {
            key: "k".into(),
            name: "HP".into(),
            bands: vec![FilterBand::new(BandType::Peaking, 1000.0, 3.0, 1.0)],
            ..Default::default()
        };
        let dir = std::env::temp_dir().join(format!("adtune-windows-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.json");
        write_state(
            &path,
            &p,
            "{id}.render",
            "USB Headphones",
            &ToneSettings::default(),
        )
        .unwrap();
        let s = read_state(&path).unwrap();
        assert_eq!(s.target_id, "{id}.render");
        assert_eq!(s.profile.to_profile().name, "HP");
        assert!(!s.bypassed);
    }
}
