//! Linux calibration backend for ADtune: a native PipeWire client library
//! shared by the UI and the `adtune-service` daemon.
//!
//! The audio path is PipeWire's `module-filter-chain`, hosted **in-process**
//! by the daemon (see `src/bin/adtune-service.rs`) — the same graph a second
//! `pipewire -c` process hosted in versions ≤ 1.0, minus systemd, host CLI
//! tools, and config files, which is what lets the backend run identically in
//! a .deb, a Flatpak, and a strict Snap.
//!
//! Control plane at a glance: the UI writes `desired.json` ([`ipc`]) and the
//! daemon reconciles the graph against it, acknowledging through
//! `status.json`. Reads (device list, default sink) are native registry
//! queries ([`registry`]); nothing shells out and nothing needs systemd.
//!
//! [`PipeWire::apply`] writes the desired state, makes sure the daemon runs,
//! and waits for its acknowledgement. [`PipeWire::update_live`] does the same
//! for gain-only changes (the daemon pushes them onto the running filter
//! without a reload), and [`PipeWire::disable`] flips `enabled` off, which
//! unloads the filter and restores the physical output as default.

pub mod conn;
pub mod ipc;
pub mod module_host;
pub mod paths;
pub mod portal;
pub mod registry;
mod render;
pub mod state;

pub use render::{filter_chain_args, VIRTUAL_SINK_NAME, VIRTUAL_SINK_PREFIX};

use adtune_core::{AudioProfile, ToneSettings};
use ipc::{DesiredState, ServiceStatus};
use state::StoredState;
use std::os::fd::AsRawFd;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Render the full PipeWire filter-chain config for a profile (exposed for
/// inspection / comparison; the daemon itself loads [`filter_chain_args`]).
pub fn render_config(profile: &AudioProfile, target_name: &str, tone: &ToneSettings) -> String {
    render::filter_config(profile, target_name, tone)
}

/// A PipeWire output sink, physical or virtual.
///
/// The frontend hands one of these back to [`PipeWire::apply`] to pick the
/// calibration target.
#[derive(Clone, Debug)]
pub struct OutputDevice {
    /// PipeWire node id. Ephemeral — it changes across reboots and graph
    /// restarts, so it is only used within a single session; the stable
    /// `node_name` is what gets persisted for later restore.
    pub node_id: i64,
    /// Stable node name (e.g. `alsa_output.usb-…`), used to re-find the device
    /// after ids have been reassigned.
    pub node_name: String,
    /// Human-readable label shown in the UI.
    pub description: String,
    /// Whether this is currently the system default sink.
    pub is_default: bool,
}

/// Backend result type: everything surfaces failures as a display string, since
/// the only consumer is the UI, which shows the message verbatim.
pub type Result<T> = std::result::Result<T, String>;

/// How long an apply may take end-to-end before the UI gives up: must exceed
/// the daemon's own 8 s sink timeout so its error (not a generic UI timeout)
/// is what the user sees.
const ACK_TIMEOUT: Duration = Duration::from_secs(12);

/// The PipeWire calibration controller — the crate's entry point.
///
/// Stateless: reads go straight to the PipeWire registry or the daemon's
/// status file, writes go through `desired.json`, so an instance is free.
#[derive(Default)]
pub struct PipeWire;

impl PipeWire {
    /// Construct a controller.
    pub fn new() -> Self {
        PipeWire
    }

    /// Whether a PipeWire socket exists for this session. Cheap enough to run
    /// on every UI status refresh; [`PipeWire::ensure_available`] actually
    /// connects. `PIPEWIRE_RUNTIME_DIR` takes precedence over
    /// `XDG_RUNTIME_DIR`, mirroring libpipewire — under Snap the runtime dir
    /// is snap-private and the socket lives in the real one.
    pub fn available() -> bool {
        let remote = std::env::var("PIPEWIRE_REMOTE").unwrap_or_else(|_| "pipewire-0".to_string());
        std::env::var_os("PIPEWIRE_RUNTIME_DIR")
            .or_else(|| std::env::var_os("XDG_RUNTIME_DIR"))
            .map(|dir| std::path::Path::new(&dir).join(remote).exists())
            .unwrap_or(false)
    }

    /// Whether we are sandboxed away from PipeWire and the user has to grant
    /// access: running as a snap whose `pipewire` interface is unconnected
    /// (the socket may still be visible while connects are AppArmor-denied).
    pub fn needs_permission_grant() -> bool {
        std::env::var_os("SNAP").is_some()
            && (!Self::available() || Self::new().ensure_available().is_err())
    }

    /// Verify PipeWire actually answers on the socket, so failures surface
    /// here rather than mid-apply. In a sandbox this is also the "was the
    /// socket granted" check.
    pub fn ensure_available(&self) -> Result<()> {
        conn::Session::connect().map(|_| ())
    }

    // -- reads --------------------------------------------------------------

    /// The output sinks a user can calibrate (ADtune's own virtual sink is
    /// filtered out).
    pub fn list_outputs(&self) -> Result<Vec<OutputDevice>> {
        let session = conn::Session::connect()?;
        let snap = registry::snapshot(&session)?;
        Ok(snap
            .sinks
            .iter()
            .filter(|s| !s.name.starts_with(VIRTUAL_SINK_PREFIX))
            .map(|s| OutputDevice {
                node_id: s.id as i64,
                node_name: s.name.clone(),
                description: s.description.clone(),
                is_default: snap.default_sink.as_deref() == Some(s.name.as_str()),
            })
            .collect())
    }

    /// Whether the daemon is alive and reports the filter active.
    pub fn is_active(&self) -> bool {
        daemon_running() && read_status().map(|s| s.active).unwrap_or(false)
    }

    /// Overall calibration status as `(active, human-readable message)`.
    pub fn status(&self) -> (bool, String) {
        if !Self::available() {
            // Under Snap the usual cause is the not-yet-connected pipewire
            // interface, not a missing PipeWire — say so, actionably (GUI
            // path first; the terminal alternative for completeness).
            let message = if std::env::var_os("SNAP").is_some() {
                "Allow PipeWire in App Center → ADtune → Permissions (or: sudo snap connect adtune:pipewire)"
            } else {
                "PipeWire is not running (or not accessible)."
            };
            return (false, message.into());
        }
        // The socket file being visible is not the same as being allowed to
        // connect: under Snap an unconnected pipewire interface leaves the
        // socket in view but AppArmor denies the connect, which otherwise
        // surfaces as a silently empty device list.
        if Self::needs_permission_grant() {
            return (
                false,
                "Allow PipeWire in App Center → ADtune → Permissions (or: sudo snap connect adtune:pipewire)"
                    .into(),
            );
        }
        if !daemon_running() {
            return (false, "Calibration is off.".into());
        }
        match read_status() {
            Some(st) if st.active => {
                let mode = if st.bypassed { " · bypassed" } else { "" };
                let output = st
                    .virtual_node_id
                    .map(|id| id.to_string())
                    .unwrap_or_default();
                (true, format!("Active{mode} — output {output}"))
            }
            Some(st) => match st.error {
                Some(e) => (false, e),
                None => (false, "Calibration is off.".into()),
            },
            None => (false, "Calibration is off.".into()),
        }
    }

    /// The profile currently applied — `None` when calibration is off (the
    /// desired state remembers the last profile even when disabled, but
    /// "active" means enabled).
    pub fn active_profile(&self) -> Option<AudioProfile> {
        read_desired()
            .filter(|d| d.enabled)
            .map(|d| d.state.profile.to_profile())
    }

    /// The tone settings last applied, or defaults if none exist yet.
    pub fn active_tone(&self) -> ToneSettings {
        read_desired()
            .map(|d| d.state.tone.to_tone())
            .unwrap_or_default()
    }

    /// The stable name of the last-applied physical target, used by the
    /// frontend to reselect it.
    pub fn saved_target_name(&self) -> Option<String> {
        read_desired().map(|d| d.state.target_name)
    }

    // -- writes -------------------------------------------------------------

    /// Apply `profile`/`tone` to `target`: persist the desired state, make
    /// sure the daemon runs, and wait for it to publish the virtual sink and
    /// route the system default to it.
    pub fn apply(
        &self,
        profile: &AudioProfile,
        target: &OutputDevice,
        tone: &ToneSettings,
    ) -> Result<()> {
        // Guard against selecting the virtual sink as its own target: the
        // filter would capture its own output and feed back. Checked on both
        // the description and the node name because either may be what the UI
        // holds.
        if target.description.contains(VIRTUAL_SINK_NAME)
            || target.node_name.starts_with(VIRTUAL_SINK_PREFIX)
        {
            return Err(
                "Choose your physical output device, not the ADtune virtual output.".into(),
            );
        }
        paths::require_home()?;
        let revision = write_desired(true, profile, target, tone)?;
        ensure_daemon()?;
        wait_ack(revision).map(|_| ())
    }

    /// Push tone/bypass changes to the running filter. Returns `Ok(false)`
    /// when nothing is running (the caller falls back to a full apply); the
    /// daemon itself decides between a live gain push and a module reload.
    pub fn update_live(
        &self,
        profile: &AudioProfile,
        target: &OutputDevice,
        tone: &ToneSettings,
    ) -> Result<bool> {
        if target.description.contains(VIRTUAL_SINK_NAME)
            || target.node_name.starts_with(VIRTUAL_SINK_PREFIX)
        {
            return Err(
                "Choose your physical output device, not the ADtune virtual output.".into(),
            );
        }
        if !self.is_active() {
            return Ok(false);
        }
        paths::require_home()?;
        let revision = write_desired(true, profile, target, tone)?;
        wait_ack(revision).map(|_| true)
    }

    /// Turn calibration off: the daemon unloads the filter and restores the
    /// physical target as the default output.
    pub fn disable(&self) -> Result<()> {
        let Some(mut desired) = read_desired() else {
            return Ok(()); // never enabled — nothing to tear down
        };
        desired.revision += 1;
        desired.enabled = false;
        ipc::write_json_atomic(&paths::desired_path(), &desired)
            .map_err(|e| format!("Could not write the ADtune state file: {e}"))?;
        // The daemon must be running to process the teardown (it also restores
        // the default sink if a previous daemon died with the filter up).
        ensure_daemon()?;
        wait_ack(desired.revision).map(|_| ())
    }
}

// -- desired/status plumbing -------------------------------------------------

fn read_desired() -> Option<DesiredState> {
    ipc::read_json(&paths::desired_path())
}

fn read_status() -> Option<ServiceStatus> {
    ipc::read_json(&paths::status_path())
}

/// Persist a new desired state (revision = last + 1) and return the revision
/// to wait on.
fn write_desired(
    enabled: bool,
    profile: &AudioProfile,
    target: &OutputDevice,
    tone: &ToneSettings,
) -> Result<u64> {
    let revision = read_desired().map(|d| d.revision + 1).unwrap_or(1);
    let desired = DesiredState {
        revision,
        enabled,
        state: StoredState::new(profile, target.node_id, &target.node_name, tone),
    };
    ipc::write_json_atomic(&paths::desired_path(), &desired)
        .map_err(|e| format!("Could not write the ADtune state file: {e}"))?;
    Ok(revision)
}

/// Poll `status.json` until the daemon acknowledges `revision` (returning its
/// status, or its error verbatim), or time out.
fn wait_ack(revision: u64) -> Result<ServiceStatus> {
    let deadline = Instant::now() + ACK_TIMEOUT;
    loop {
        if let Some(st) = read_status() {
            if st.revision_applied >= revision {
                return match st.error {
                    Some(e) => Err(e),
                    None => Ok(st),
                };
            }
        }
        if !daemon_running() {
            return Err("The ADtune service exited unexpectedly.".into());
        }
        if Instant::now() > deadline {
            return Err("Timed out waiting for the ADtune service.".into());
        }
        // Fine-grained: live updates ack within a few ms (inotify-driven
        // daemon), and this poll granularity is the UI-visible latency floor.
        std::thread::sleep(Duration::from_millis(5));
    }
}

// -- daemon lifecycle ----------------------------------------------------------

/// Whether a daemon instance holds the single-instance lock. Probing takes
/// the lock non-blockingly and releases it immediately when acquired — so a
/// positive answer means "locked by someone else".
fn daemon_running() -> bool {
    let Ok(file) = std::fs::OpenOptions::new()
        .write(true)
        .open(paths::lock_path())
    else {
        return false; // no lock file yet → no daemon has ever started
    };
    // SAFETY: flock on an owned, open fd; the lock (if taken) dies with `file`.
    let taken = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    !taken
}

/// Minimal `which`: resolve `cmd` against `$PATH`.
fn which(cmd: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(cmd))
            .find(|p| p.is_file())
    })
}

/// Start `adtune-service` if it isn't running: preferring the binary next to
/// the current executable (how all three package formats lay it out), falling
/// back to `$PATH`. Waits briefly for the instance lock to be taken.
fn ensure_daemon() -> Result<()> {
    if daemon_running() {
        return Ok(());
    }
    // All three package layouts install the daemon next to the app binary;
    // the grandparent covers cargo's dev layout (examples/ and deps/ live one
    // level below target/debug). $PATH is the last resort.
    let exe = std::env::current_exe().ok();
    let adjacent = exe
        .iter()
        .flat_map(|p| [p.parent(), p.parent().and_then(|d| d.parent())])
        .flatten()
        .map(|d| d.join("adtune-service"))
        .find(|p| p.is_file());
    let program = adjacent.unwrap_or_else(|| "adtune-service".into());

    // On systemd desktops GNOME runs the app in a transient per-app scope and
    // kills everything left in it when the app closes — a directly spawned
    // daemon would die with the UI. `systemd-run --user` detaches it into its
    // own scope. Inside Flatpak/Snap the sandbox supervisor owns the daemon's
    // lifetime instead, and the plain spawn is correct.
    let confined = portal::in_flatpak() || std::env::var_os("SNAP").is_some();
    let use_systemd_run = !confined
        && std::path::Path::new("/run/systemd/system").exists()
        && which("systemd-run").is_some();
    let mut command = if use_systemd_run {
        let mut c = Command::new("systemd-run");
        c.args(["--user", "--collect", "--quiet"]).arg(&program);
        c
    } else {
        Command::new(&program)
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            format!(
                "Could not start the ADtune service ({}): {e}",
                program.display()
            )
        })?;

    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if daemon_running() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("The ADtune service did not start.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use adtune_core::{BandType, FilterBand};

    fn m50x() -> AudioProfile {
        AudioProfile {
            name: "Audio-Technica ATH-M50x".into(),
            bands: vec![
                FilterBand::new(BandType::LowShelf, 110.0, -3.5, 1.0),
                FilterBand::new(BandType::Peaking, 2900.0, 2.7, 1.2),
                FilterBand::new(BandType::HighShelf, 9000.0, -4.4, 1.0),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn config_has_expected_nodes() {
        let cfg = render::filter_config(&m50x(), "alsa_output.usb", &ToneSettings::default());
        // pre_gain (linear) + 3 correction + 3 tone slots = 7 builtin nodes
        assert_eq!(cfg.matches("type = builtin").count(), 7);
        assert!(cfg.contains("name = pre_gain"));
        assert!(cfg.contains("label = linear"));
        assert!(cfg.contains("name = eq_band_1"));
        assert!(cfg.contains("name = eq_tilt_hi"));
        assert!(cfg.contains("node.target = \"alsa_output.usb\""));
        assert!(cfg.contains(VIRTUAL_SINK_PREFIX));
    }

    #[test]
    fn bypass_zeroes_correction_gain() {
        let bypass = ToneSettings {
            wet: 0.0,
            ..Default::default()
        };
        let cfg = render::filter_config(&m50x(), "sink", &bypass);
        // eq_band_1 gain scaled by wet=0 -> 0.000
        assert!(cfg.contains("name = eq_band_1"));
        assert!(cfg.contains("\"Gain\" = 0.000"));
    }

    #[test]
    fn filter_chain_args_is_a_standalone_object() {
        // The daemon hands this string directly to pw_context_load_module, so
        // it must be a complete SPA-JSON object carrying the whole graph.
        let args = render::filter_chain_args(&m50x(), "alsa_output.usb", &ToneSettings::default());
        assert!(args.starts_with('{') && args.ends_with('}'));
        assert_eq!(args.matches("type = builtin").count(), 7);
        assert!(args.contains("media.class = Audio/Sink"));
        assert!(args.contains("node.target = \"alsa_output.usb\""));
        // And the legacy full-config renderer embeds the exact same args.
        let cfg = render::filter_config(&m50x(), "alsa_output.usb", &ToneSettings::default());
        assert!(cfg.contains(&args));
    }
}
