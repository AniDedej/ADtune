//! The persisted-state serde model (`state.json`), shared by every OS backend.
//!
//! Only the `target_id` type differs per platform (a PipeWire node id vs a
//! Windows endpoint id), so [`StoredState`] is generic over it: backends use
//! `StoredState<i64>` / `StoredState<String>` and keep their exact on-disk JSON.

use crate::profile::{
    sane_preamp, sane_str, AudioProfile, BandType, FilterBand, ToneSettings, MAX_BANDS,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Serde default for a stored band's `q` when the on-disk JSON predates the field.
fn one() -> f64 {
    1.0
}

/// On-disk representation of one [`FilterBand`] (a plain, serde-friendly mirror
/// with the type stored as its string name).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StoredBand {
    #[serde(rename = "type")]
    pub kind: String,
    pub frequency: f64,
    pub gain: f64,
    #[serde(default = "one")]
    pub q: f64,
}

/// On-disk representation of a [`AudioProfile`]. Optional fields default so
/// state files written by older versions still deserialize.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StoredProfile {
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub preamp: f64,
    pub bands: Vec<StoredBand>,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub form: String,
}

/// On-disk representation of [`ToneSettings`].
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StoredTone {
    pub wet: f64,
    pub bass: f64,
    pub tilt: f64,
    pub headroom: bool,
}

/// The full persisted app state (`state.json`). Generic over the target id type
/// so each backend keeps its own native endpoint identifier on disk.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StoredState<Id> {
    pub profile: StoredProfile,
    /// Platform-specific output identifier (PipeWire node id / Windows endpoint id).
    pub target_id: Id,
    pub target_name: String,
    pub tone: StoredTone,
    pub bypassed: bool,
}

impl StoredProfile {
    /// Snapshot a live profile into its serde form for writing to disk.
    pub fn from_profile(p: &AudioProfile) -> Self {
        StoredProfile {
            key: p.key.clone(),
            name: p.name.clone(),
            detail: p.detail.clone(),
            preamp: p.preamp,
            bands: p
                .bands
                .iter()
                .map(|b| StoredBand {
                    kind: b.kind.as_str().to_string(),
                    frequency: b.frequency,
                    gain: b.gain,
                    q: b.q,
                })
                .collect(),
            source: p.source.clone(),
            form: p.form.clone(),
        }
    }

    /// Rebuild a live profile from disk, sanitizing every field on the way in
    /// (the on-disk JSON is an untrusted input).
    pub fn to_profile(&self) -> AudioProfile {
        // Bound every untrusted string field: a crafted profile file's name/key
        // is otherwise limited only by the whole-file cap and would flow into a
        // rendered config comment and the on-disk library entry.
        AudioProfile {
            key: sane_str(&self.key),
            name: sane_str(&self.name),
            detail: sane_str(&self.detail),
            source: sane_str(&self.source),
            form: sane_str(&self.form),
            preamp: sane_preamp(self.preamp),
            // Cap the band count: a crafted state/profile file with a huge
            // `bands` array would otherwise expand into a runaway PipeWire config
            // and O(n) response recomputation. `.take` before `filter_map` bounds
            // the work regardless of how many entries the file declares.
            bands: self
                .bands
                .iter()
                .take(MAX_BANDS)
                .filter_map(|b| {
                    BandType::parse(&b.kind).map(|k| FilterBand::new(k, b.frequency, b.gain, b.q))
                })
                .collect(),
        }
    }
}

impl StoredTone {
    /// Snapshot live tone settings for writing to disk.
    pub fn from_tone(t: &ToneSettings) -> Self {
        StoredTone {
            wet: t.wet,
            bass: t.bass,
            tilt: t.tilt,
            headroom: t.headroom,
        }
    }
    /// Rebuild tone settings from disk, clamping each value into its valid range.
    pub fn to_tone(&self) -> ToneSettings {
        ToneSettings {
            wet: self.wet.clamp(0.0, 1.0),
            bass: self.bass.clamp(-6.0, 6.0),
            tilt: self.tilt.clamp(-6.0, 6.0),
            headroom: self.headroom,
        }
    }
}

impl<Id> StoredState<Id> {
    /// Build the on-disk state from live values (`bypassed` is derived from wet).
    pub fn new(
        profile: &AudioProfile,
        target_id: Id,
        target_name: impl Into<String>,
        tone: &ToneSettings,
    ) -> Self {
        StoredState {
            profile: StoredProfile::from_profile(profile),
            target_id,
            target_name: target_name.into(),
            tone: StoredTone::from_tone(tone),
            bypassed: tone.wet <= 0.0,
        }
    }
}

/// Serialize a profile to ADtune's native JSON (the `StoredProfile` shape), for
/// exporting/saving a profile as a portable, fully round-trippable `.json`.
pub fn profile_to_json(p: &AudioProfile) -> String {
    serde_json::to_string_pretty(&StoredProfile::from_profile(p)).unwrap_or_default() + "\n"
}

/// Parse a profile from ADtune's native JSON (the inverse of [`profile_to_json`]).
pub fn profile_from_json(text: &str) -> Result<AudioProfile, String> {
    serde_json::from_str::<StoredProfile>(text)
        .map(|s| s.to_profile())
        .map_err(|e| e.to_string())
}

/// Read and deserialize the state file, or `None` if it is missing/unreadable.
/// The read is size-capped so a hostile or corrupt state file can't exhaust
/// memory (a truncated read simply fails to deserialize).
pub fn read_state<Id: DeserializeOwned>(path: &Path) -> Option<StoredState<Id>> {
    use std::io::Read;
    let mut buf = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(8 << 20)
        .read_to_string(&mut buf)
        .ok()?;
    serde_json::from_str(&buf).ok()
}

/// Serialize the state to `path` (pretty JSON, trailing newline).
pub fn write_state<Id: Serialize>(path: &Path, state: &StoredState<Id>) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;
    std::fs::write(path, json + "\n")
}

#[cfg(test)]
mod state_tests {
    use super::*;

    /// A hostile huge finite preamp from a crafted state file is clamped into
    /// range on the way in (serde_json rejects bare NaN, so magnitudes are the
    /// reachable attack).
    #[test]
    fn hostile_json_preamp_is_sanitized() {
        // A crafted profile file must never carry a non-finite or absurd preamp
        // into a rendered config. serde_json rejects a bare NaN literal, so the
        // reachable hostile values are huge finite magnitudes.
        let json = r#"{"key":"x","name":"x","preamp":-1e308,"bands":[]}"#;
        let p = profile_from_json(json).unwrap();
        assert!(p.preamp.is_finite() && (-24.0..=24.0).contains(&p.preamp));
    }

    /// Out-of-range band values from a crafted state file are clamped into their
    /// valid ranges when the profile is rebuilt.
    #[test]
    fn hostile_json_bands_are_sanitized() {
        let json = r#"{"key":"x","name":"x","preamp":0,
            "bands":[{"type":"peaking","frequency":9e99,"gain":9e99,"q":0}]}"#;
        let p = profile_from_json(json).unwrap();
        let b = p.bands[0];
        assert!((20.0..=20000.0).contains(&b.frequency));
        assert!((-24.0..=24.0).contains(&b.gain));
        assert!((0.1..=20.0).contains(&b.q));
    }
}
