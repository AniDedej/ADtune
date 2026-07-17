//! ADtune's native-APO Windows backend: writes the correction where ADtune's own
//! APO reads it (`%ProgramData%\ADtune\config.txt`) and reports whether the APO
//! is enabled on a device. The APO itself is `crates/adtune-apo`; enabling it on
//! a device (registry FxProperties, admin) is done by the installer.
//!
//! Config/state writing is portable `std::fs`; the device enumeration and
//! registry checks are Windows-only (stubbed elsewhere), so the whole controller
//! still compiles on Linux (it is only *used* on Windows via the UI's `sys`).

use crate::devices::{enumerate, OutputDevice};
use crate::state::{read_state, write_state};
use crate::{env, writer, ApplyOutcome, Result};
use adtune_core::{render_parametric_eq, AudioProfile, ToneSettings};
use std::path::PathBuf;

/// The ADtune APO CLSID body (must match `crates/adtune-apo`). Presence of this
/// full GUID in a device's FxProperties means our APO is active on it. Using the
/// whole GUID rather than an 8-hex fragment avoids ever mistaking an unrelated
/// vendor CLSID that happens to contain `7f4a1e02` for ours.
pub const APO_CLSID_HEX: &str = "7f4a1e02-9c3b-4d5a-8e21-ad7c0c0ffee1";

/// `%ProgramData%\ADtune` — the directory the ADtune APO reads `config.txt` from.
pub fn config_dir() -> PathBuf {
    let base = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".to_string());
    PathBuf::from(base).join("ADtune")
}

/// `%ProgramData%\ADtune\state.json` — the last applied profile/target/tone,
/// persisted alongside `config.txt` so the UI can restore on next launch.
fn state_path() -> PathBuf {
    config_dir().join("state.json")
}

/// Controller for ADtune's own APO — the analog of `PipeWire`.
#[derive(Default)]
pub struct NativeApo;

impl NativeApo {
    pub fn new() -> Self {
        NativeApo
    }

    /// The active render endpoints to offer as calibration targets.
    pub fn list_outputs(&self) -> Result<Vec<OutputDevice>> {
        enumerate()
    }

    /// Whether ADtune's APO is registered in an effect slot on `endpoint_id`.
    /// A registry read only — it does not imply enhancements are on (that gate
    /// is separate; see [`Self::enhancements_enabled_on`]).
    pub fn is_enabled_on(&self, endpoint_id: &str) -> bool {
        env::is_apo_enabled(endpoint_id, APO_CLSID_HEX)
    }

    /// False when the user turned "Audio enhancements" off for the endpoint in
    /// Windows Settings — a state in which no APO (ours included) loads.
    pub fn enhancements_enabled_on(&self, endpoint_id: &str) -> bool {
        env::enhancements_enabled(endpoint_id)
    }

    /// Persist a correction: the rendered ParametricEQ `body` the APO reads, plus
    /// the state snapshot the UI restores from. The config is written atomically
    /// (temp + rename) so the APO's watcher never sees a partial file; state is a
    /// separate, best-ordered write (config first, so a crash between the two
    /// never leaves state pointing at a correction that was never written).
    fn write(
        &self,
        body: &str,
        profile: &AudioProfile,
        target: &OutputDevice,
        tone: &ToneSettings,
    ) -> Result<()> {
        std::fs::create_dir_all(config_dir()).map_err(|e| e.to_string())?;
        writer::atomic_write(&config_dir(), "config.txt", body).map_err(|e| e.to_string())?;
        write_state(
            &state_path(),
            profile,
            &target.id,
            &target.friendly_name,
            tone,
        )
        .map_err(|e| e.to_string())
    }

    /// Write the correction (the APO live-reloads it). Reports whether the target
    /// device still needs the one-time (admin) enable — either because the APO
    /// isn't registered on it, or because Windows' "Audio enhancements" toggle
    /// is off (which blocks every APO from loading).
    pub fn apply(
        &self,
        profile: &AudioProfile,
        target: &OutputDevice,
        tone: &ToneSettings,
    ) -> Result<ApplyOutcome> {
        self.write(&render_parametric_eq(profile, tone), profile, target, tone)?;
        let needs_enable =
            !self.is_enabled_on(&target.id) || !self.enhancements_enabled_on(&target.id);
        Ok(ApplyOutcome { needs_enable })
    }

    /// Live tone/bypass update — same as apply (the config file is the live channel).
    pub fn update_live(
        &self,
        profile: &AudioProfile,
        target: &OutputDevice,
        tone: &ToneSettings,
    ) -> Result<bool> {
        self.write(&render_parametric_eq(profile, tone), profile, target, tone)?;
        Ok(true)
    }

    /// Make the APO transparent (correction off). Keeps it loaded on the device.
    pub fn disable(&self) -> Result<()> {
        std::fs::create_dir_all(config_dir()).map_err(|e| e.to_string())?;
        if let Some(s) = read_state(&state_path()) {
            let profile = s.profile.to_profile();
            let tone = ToneSettings {
                wet: 0.0,
                ..s.tone.to_tone()
            };
            let target = OutputDevice {
                id: s.target_id.clone(),
                friendly_name: s.target_name.clone(),
                is_default: false,
            };
            self.write(
                &render_parametric_eq(&profile, &tone),
                &profile,
                &target,
                &tone,
            )
        } else {
            writer::atomic_write(&config_dir(), "config.txt", "# ADtune disabled\r\n")
                .map_err(|e| e.to_string())
        }
    }

    /// The profile from the last apply, or `None` if nothing has been applied.
    pub fn active_profile(&self) -> Option<AudioProfile> {
        read_state(&state_path()).map(|s| s.profile.to_profile())
    }

    /// The tone (wet/bass/tilt) from the last apply; defaults if unset.
    pub fn active_tone(&self) -> ToneSettings {
        read_state(&state_path())
            .map(|s| s.tone.to_tone())
            .unwrap_or_default()
    }

    /// The last target as `(endpoint_id, friendly_name)`, for re-selecting it in
    /// the device list on launch.
    pub fn saved_target(&self) -> Option<(String, String)> {
        read_state(&state_path()).map(|s| (s.target_id, s.target_name))
    }

    /// A `(healthy, human_message)` summary for the UI, derived from persisted
    /// state cross-checked against the live registry. The arms are ordered by
    /// precedence: an explicit bypass wins; otherwise a registered-but-muted APO
    /// (enhancements off) is called out because it silently defeats correction;
    /// then the normal "correcting" and "registered but not yet applied" cases;
    /// finally "nothing configured". `healthy` is false whenever the correction
    /// is not actually reaching the output.
    pub fn status(&self) -> (bool, String) {
        match read_state(&state_path()) {
            Some(s) if s.bypassed => (true, format!("Bypassed on {}", s.target_name)),
            Some(s)
                if self.is_enabled_on(&s.target_id)
                    && !self.enhancements_enabled_on(&s.target_id) =>
            {
                (
                    false,
                    format!(
                        "Audio enhancements are off on {} — turn Calibration on to fix",
                        s.target_name
                    ),
                )
            }
            Some(s) if self.is_enabled_on(&s.target_id) => {
                (true, format!("Correcting {}", s.target_name))
            }
            Some(s) => (false, format!("Not active on {} yet", s.target_name)),
            None => (false, "Not configured.".into()),
        }
    }
}
