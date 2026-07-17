//! Thin OS-abstraction over the calibration backend so the UI is portable.
//! Linux uses the PipeWire backend; Windows uses ADtune's own APO backend;
//! other platforms get inert stubs.

use adtune_core::{AudioProfile, ToneSettings};
use std::path::PathBuf;

/// A physical output device, backend-agnostic.
/// `id` is an opaque handle, `name` a stable identifier used for matching the
/// saved target, `description` the human label shown in the dropdown.
#[derive(Clone, Debug, Default)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub description: String,
    pub is_default: bool,
}

// Linux backend: every call runs against a fresh, short-lived `PipeWire` handle.
// PipeWire owns filter insertion, so nothing here needs elevation.
#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use adtune_pipewire::{OutputDevice, PipeWire};

    /// Map a PipeWire output node to the backend-agnostic [`Device`].
    fn to_device(o: OutputDevice) -> Device {
        Device {
            id: o.node_id.to_string(),
            name: o.node_name,
            description: o.description,
            is_default: o.is_default,
        }
    }
    /// Inverse of [`to_device`]: recover the PipeWire node from a [`Device`].
    fn to_output(d: &Device) -> OutputDevice {
        OutputDevice {
            node_id: d.id.parse().unwrap_or(-1),
            node_name: d.name.clone(),
            description: d.description.clone(),
            is_default: d.is_default,
        }
    }

    /// `(active, human-readable status line)` for the calibration filter.
    pub fn status() -> (bool, String) {
        PipeWire::new().status()
    }
    /// Whether the sandbox blocks PipeWire access pending a user grant
    /// (snap with an unconnected `pipewire` interface).
    pub fn needs_permission_grant() -> bool {
        PipeWire::needs_permission_grant()
    }
    /// Physical output devices, or empty if the backend can't be queried.
    pub fn list_outputs() -> Vec<Device> {
        PipeWire::new()
            .list_outputs()
            .map(|v| v.into_iter().map(to_device).collect())
            .unwrap_or_default()
    }
    /// The profile currently loaded into the running filter, if any.
    pub fn active_profile() -> Option<AudioProfile> {
        PipeWire::new().active_profile()
    }
    /// The tone settings currently applied to the running filter.
    pub fn active_tone() -> ToneSettings {
        PipeWire::new().active_tone()
    }
    /// `name` of the device the last apply targeted, for reselecting it in the UI.
    pub fn saved_target_name() -> Option<String> {
        PipeWire::new().saved_target_name()
    }
    /// (Re)build and load the filter for `dev` from scratch.
    pub fn apply(p: &AudioProfile, dev: &Device, tone: &ToneSettings) -> Result<(), String> {
        PipeWire::new().apply(p, &to_output(dev), tone)
    }
    /// Update the running filter in place; `Ok(false)` if nothing is loaded yet.
    pub fn update_live(
        p: &AudioProfile,
        dev: &Device,
        tone: &ToneSettings,
    ) -> Result<bool, String> {
        PipeWire::new().update_live(p, &to_output(dev), tone)
    }
    // Master on/off. PipeWire inserts/removes the filter directly — no elevation.
    pub fn enable(p: &AudioProfile, dev: &Device, tone: &ToneSettings) -> Result<(), String> {
        PipeWire::new().apply(p, &to_output(dev), tone)
    }
    /// Master off: remove the filter from the pipeline entirely.
    pub fn disable_device(_dev: &Device) -> Result<(), String> {
        PipeWire::new().disable()
    }
    /// Per-user directory for the saved-profile library (XDG-aware, so it
    /// lands in the app's own config dir inside Flatpak/Snap sandboxes).
    pub fn user_profiles_dir() -> PathBuf {
        adtune_pipewire::paths::profiles_dir()
    }
}

// Windows backend: calls run against ADtune's own APO via a fresh `NativeApo`.
// Registering/unregistering the APO needs elevation; the live-audio writes and
// queries below do not (see `enable`/`disable_device` for the elevated paths).
#[cfg(target_os = "windows")]
mod imp {
    use super::*;
    use adtune_windows::{NativeApo, OutputDevice};

    // Windows endpoint id is the stable identifier; the friendly name is shown.
    /// Map a Windows audio endpoint to the backend-agnostic [`Device`].
    fn to_device(o: OutputDevice) -> Device {
        Device {
            id: o.id.clone(),
            name: o.id,
            description: o.friendly_name,
            is_default: o.is_default,
        }
    }
    /// Inverse of [`to_device`]: recover the Windows endpoint from a [`Device`].
    fn to_output(d: &Device) -> OutputDevice {
        OutputDevice {
            id: d.id.clone(),
            friendly_name: d.description.clone(),
            is_default: d.is_default,
        }
    }

    /// `(active, human-readable status line)` for the APO.
    pub fn status() -> (bool, String) {
        NativeApo::new().status()
    }
    /// No sandbox permission concept on this platform.
    pub fn needs_permission_grant() -> bool {
        false
    }
    /// Physical output endpoints, or empty if enumeration fails.
    pub fn list_outputs() -> Vec<Device> {
        NativeApo::new()
            .list_outputs()
            .map(|v| v.into_iter().map(to_device).collect())
            .unwrap_or_default()
    }
    /// The profile the APO is currently correcting with, if any.
    pub fn active_profile() -> Option<AudioProfile> {
        NativeApo::new().active_profile()
    }
    /// The tone settings the APO is currently applying.
    pub fn active_tone() -> ToneSettings {
        NativeApo::new().active_tone()
    }
    /// Endpoint id the last apply targeted (the id is also the device `name`).
    pub fn saved_target_name() -> Option<String> {
        NativeApo::new().saved_target().map(|(id, _name)| id)
    }
    /// Apply = full activation, matching the Linux backend: if the APO isn't
    /// registered on the device yet (or Windows' enhancements toggle blocks
    /// it), this triggers the one-time elevated registration first, then
    /// writes the correction. Once registered, an apply is just a config
    /// write that the APO live-reloads — no elevation, no engine restart.
    pub fn apply(p: &AudioProfile, dev: &Device, tone: &ToneSettings) -> Result<(), String> {
        enable(p, dev, tone)
    }
    /// Update the live APO config in place; `Ok(false)` if it isn't registered.
    pub fn update_live(
        p: &AudioProfile,
        dev: &Device,
        tone: &ToneSettings,
    ) -> Result<bool, String> {
        NativeApo::new().update_live(p, &to_output(dev), tone)
    }
    /// Master enable for a device: register the APO (one UAC prompt + audio
    /// reload) if it isn't already — or if Windows' "Audio enhancements" toggle
    /// was switched off, which silently blocks every APO from loading; the
    /// elevated helper turns it back on. Then write the correction so it's live.
    pub fn enable(p: &AudioProfile, dev: &Device, tone: &ToneSettings) -> Result<(), String> {
        let apo = NativeApo::new();
        if !apo.is_enabled_on(&dev.id) || !apo.enhancements_enabled_on(&dev.id) {
            // Surfaces the elevated helper's real error (or the UAC-cancel reason).
            adtune_windows::run_elevated(&format!("--enable-apo {}", dev.id))?;
        }
        apo.apply(p, &to_output(dev), tone).map(|_| ())
    }
    /// Master disable: unregister the APO from the device (one UAC prompt + audio
    /// reload), fully removing it from that output's pipeline.
    pub fn disable_device(dev: &Device) -> Result<(), String> {
        adtune_windows::run_elevated(&format!("--disable-apo {}", dev.id))
    }
    pub fn user_profiles_dir() -> PathBuf {
        // Per-user (roaming), distinct from the APO's machine-wide %ProgramData%\ADtune.
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        base.join("ADtune").join("profiles")
    }
}

// Fallback for platforms with no backend: inert stubs that report "unsupported"
// so the UI still builds, runs, and can render the graph without applying audio.
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
mod imp {
    use super::*;
    const MSG: &str = "No audio backend for this OS yet.";
    pub fn status() -> (bool, String) {
        (false, MSG.into())
    }
    pub fn list_outputs() -> Vec<Device> {
        Vec::new()
    }
    pub fn active_profile() -> Option<AudioProfile> {
        None
    }
    pub fn active_tone() -> ToneSettings {
        ToneSettings::default()
    }
    pub fn saved_target_name() -> Option<String> {
        None
    }
    pub fn apply(_: &AudioProfile, _: &Device, _: &ToneSettings) -> Result<(), String> {
        Err(MSG.into())
    }
    pub fn update_live(_: &AudioProfile, _: &Device, _: &ToneSettings) -> Result<bool, String> {
        Ok(false)
    }
    pub fn enable(_: &AudioProfile, _: &Device, _: &ToneSettings) -> Result<(), String> {
        Err(MSG.into())
    }
    pub fn disable_device(_: &Device) -> Result<(), String> {
        Err(MSG.into())
    }
    pub fn user_profiles_dir() -> PathBuf {
        std::env::temp_dir().join("adtune").join("profiles")
    }
}

pub use imp::*;
