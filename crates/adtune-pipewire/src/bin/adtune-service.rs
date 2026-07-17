//! The ADtune calibration daemon.
//!
//! A small, always-on native PipeWire client that hosts the filter-chain
//! module in-process and reconciles the audio graph against the desired-state
//! file the UI writes (see `adtune_pipewire::ipc`). It replaces the systemd
//! user service + second `pipewire` process of versions ≤ 1.0, and runs
//! identically in a .deb, a Flatpak, and a strict Snap.
//!
//! Event-driven by design: the PipeWire main loop runs continuously (the
//! hosted module's control plane is serviced by it), a persistent registry
//! listener tracks sinks as they come and go, and an inotify watch on the
//! config directory wakes reconciliation the instant `desired.json` changes
//! (a slow timer handles timeouts, retries, and the no-inotify fallback).
//! Nothing here ever blocks the loop.

use adtune_pipewire::conn::Session;
use adtune_pipewire::ipc::{self, DesiredState, ServiceStatus};
use adtune_pipewire::module_host::{live_props_pod, FilterModule};
use adtune_pipewire::{filter_chain_args, paths, VIRTUAL_SINK_PREFIX};
use pipewire::metadata::Metadata;
use pipewire::node::Node;
use pipewire::spa::pod::Pod;
use pipewire::types::ObjectType;
use std::cell::RefCell;
use std::os::fd::AsRawFd;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

/// How long a freshly loaded filter gets to publish its virtual sink before
/// the daemon declares the apply failed and unloads it again.
const SINK_TIMEOUT: Duration = Duration::from_secs(8);

/// How long after a failed apply before it is retried unprompted — covers
/// early-login races where the target device hasn't been enumerated yet.
const RETRY_AFTER: Duration = Duration::from_secs(10);

/// Set by the SIGTERM/SIGINT handlers; checked from the reconcile timer (a
/// plain flag keeps the handler async-signal-safe).
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn on_terminate(_sig: libc::c_int) {
    SHUTDOWN.store(true, Ordering::Relaxed);
}

/// Everything the reconcile loop mutates, shared (single-threaded) between
/// the registry listener and the poll timer.
#[derive(Default)]
struct Daemon {
    /// The session manager's `default` metadata object, once seen.
    default_meta: Option<Metadata>,
    /// The loaded filter module, when calibration is on.
    module: Option<FilterModule>,
    /// Bound proxy of our own virtual-sink node, for live set_param pushes.
    virtual_node: Option<(u32, Node)>,
    /// Set while waiting for a freshly loaded module's sink to appear.
    awaiting_sink_since: Option<Instant>,
    /// Default-sink write that couldn't happen yet (no metadata object seen).
    pending_default: Option<String>,
    /// The last desired state successfully applied (None = nothing loaded).
    applied: Option<DesiredState>,
    /// mtime of `desired.json` at the last poll, to detect changes cheaply.
    desired_mtime: Option<SystemTime>,
    /// The revision currently being applied (ack'd once the sink appears).
    applying_revision: u64,
    /// When to re-attempt a failed apply without waiting for a new revision.
    retry_at: Option<Instant>,
}

impl Daemon {
    /// Write `status.json` reflecting the current state; failures are
    /// non-fatal (the UI just sees a stale status until the next write).
    fn publish_status(&self, error: Option<String>) {
        let bypassed = self
            .applied
            .as_ref()
            .map(|d| d.state.tone.wet <= 0.0)
            .unwrap_or(false);
        let status = ServiceStatus {
            revision_applied: self.applying_revision,
            active: self.module.is_some() && self.virtual_node.is_some(),
            virtual_node_id: self.virtual_node.as_ref().map(|(id, _)| *id),
            error,
            pid: std::process::id(),
            bypassed,
        };
        let _ = ipc::write_json_atomic(&paths::status_path(), &status);
    }

    /// Ask WirePlumber to make `node_name` the default sink, or queue the
    /// request until the `default` metadata object has been seen.
    fn set_default(&mut self, node_name: &str) {
        let value = serde_json::json!({ "name": node_name }).to_string();
        match &self.default_meta {
            Some(md) => {
                md.set_property(
                    0,
                    "default.configured.audio.sink",
                    Some("Spa:String:JSON"),
                    Some(&value),
                );
                self.pending_default = None;
            }
            None => self.pending_default = Some(node_name.to_string()),
        }
    }
}

/// Structural changes (band layout, target device) need a module reload;
/// everything else (gains: wet / bass / tilt / headroom / band-gain edits)
/// can be pushed live onto the running filter. Frequency/Q edits are
/// structural because the graph bakes them into the biquad controls only
/// gain-updates are pushed for.
fn needs_reload(applied: &DesiredState, desired: &DesiredState) -> bool {
    let (a, d) = (&applied.state, &desired.state);
    if a.target_name != d.target_name || a.profile.bands.len() != d.profile.bands.len() {
        return true;
    }
    a.profile
        .bands
        .iter()
        .zip(&d.profile.bands)
        .any(|(x, y)| x.kind != y.kind || x.frequency != y.frequency || x.q != y.q)
}

/// Remove leftovers of the ≤ 1.0 systemd architecture (unconfined installs
/// only): stop/disable the old unit, delete it and the old filter config, and
/// seed `desired.json` from the old `state.json` so the user's calibration
/// carries over — enabled exactly when the old unit was still installed.
fn migrate_legacy() {
    let confined =
        std::path::Path::new("/.flatpak-info").exists() || std::env::var_os("SNAP").is_some();
    if confined {
        return;
    }
    let unit = paths::legacy_unit_path();
    let unit_existed = unit.exists();
    if unit_existed {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", "--now", "adtune.service"])
            .output();
        let _ = std::fs::remove_file(&unit);
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .output();
        eprintln!("adtune-service: removed legacy systemd unit");
    }
    let _ = std::fs::remove_file(paths::legacy_filter_conf_path());

    let legacy_state = paths::legacy_state_path();
    if !paths::desired_path().exists() {
        if let Some(state) = adtune_pipewire::state::read_state(&legacy_state) {
            let desired = DesiredState {
                revision: 1,
                enabled: unit_existed,
                state,
            };
            if ipc::write_json_atomic(&paths::desired_path(), &desired).is_ok() {
                eprintln!("adtune-service: migrated legacy state.json to desired.json");
            }
        }
    }
    let _ = std::fs::remove_file(legacy_state);
}

/// Under Snap, autostart works by the snap writing its own autostart entry
/// into its per-snap `~/.config/autostart`; snapd matches the file name
/// against snapcraft.yaml's `autostart:` and launches the app at login.
/// No-op outside Snap.
fn ensure_snap_autostart() {
    if std::env::var_os("SNAP").is_none() {
        return;
    }
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let dir = std::path::Path::new(&home).join(".config/autostart");
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::write(
            dir.join("adtune-service.desktop"),
            "[Desktop Entry]\nType=Application\nName=ADtune Calibration Service\nExec=adtune-service\nX-GNOME-Autostart-enabled=true\n",
        );
    }
}

/// An inotify watch on the config directory, as an owned fd for the loop's IO
/// source. Watches the directory (not the file): the UI replaces desired.json
/// via rename, which registers as `IN_MOVED_TO` on the parent. `None` (exotic
/// filesystems, exhausted watches) falls back to pure timer polling.
fn setup_config_watch() -> Option<std::fs::File> {
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;
    let dir = paths::config_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    // SAFETY: fd is a fresh, owned inotify descriptor; File closes it on drop.
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let cpath = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mask = libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_CREATE;
    let wd = unsafe { libc::inotify_add_watch(fd, cpath.as_ptr(), mask) };
    (wd >= 0).then_some(file)
}

/// Take the single-instance lock, or exit quietly if another daemon holds it.
/// The returned file must stay open for the daemon's lifetime.
fn take_instance_lock() -> Option<std::fs::File> {
    let path = paths::lock_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok()?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .ok()?;
    // SAFETY: flock on an owned, open fd.
    let taken = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    taken.then_some(file)
}

fn main() {
    migrate_legacy();

    if !paths::desired_path().exists() {
        // Never configured (fresh autostart before first use): nothing to do.
        return;
    }
    let Some(_lock) = take_instance_lock() else {
        // Another instance is already reconciling; this one is redundant.
        return;
    };

    // Logout / scope teardown / Ctrl-C ask for a clean exit; the handler only
    // sets a flag (async-signal-safe) and the reconcile timer quits the loop.
    // SAFETY: installing a signal handler that touches nothing but an atomic.
    let handler = on_terminate as extern "C" fn(libc::c_int);
    unsafe {
        libc::signal(libc::SIGTERM, handler as libc::sighandler_t);
        libc::signal(libc::SIGINT, handler as libc::sighandler_t);
    }

    // Inside Flatpak, ask the Background portal to autostart us at login
    // (idempotent; the packages outside Flatpak ship a plain autostart file).
    // Off-thread so a slow portal can't delay reconciliation.
    if adtune_pipewire::portal::in_flatpak() {
        std::thread::spawn(
            || match adtune_pipewire::portal::request_background_autostart() {
                Ok(true) => eprintln!("adtune-service: background autostart granted"),
                Ok(false) => eprintln!("adtune-service: background autostart denied"),
                Err(e) => eprintln!("adtune-service: background portal: {e}"),
            },
        );
    }
    ensure_snap_autostart();

    // At login the autostart entry can fire before PipeWire's socket exists,
    // so retry for a while instead of giving up on the first attempt.
    let connect_deadline = Instant::now() + Duration::from_secs(60);
    let session = loop {
        match Session::connect() {
            Ok(s) => break Rc::new(s),
            Err(e) if Instant::now() < connect_deadline => {
                eprintln!("adtune-service: {e} — retrying");
                std::thread::sleep(Duration::from_secs(2));
            }
            Err(e) => {
                eprintln!("adtune-service: {e}");
                std::process::exit(1);
            }
        }
    };

    let daemon = Rc::new(RefCell::new(Daemon::default()));

    // --- persistent registry listener: track sinks, bind the default
    // metadata object, and notice our own virtual sink appearing. -----------
    let registry = match session.core.get_registry_rc() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("adtune-service: registry: {e}");
            std::process::exit(1);
        }
    };
    let _reg_listener = registry
        .add_listener_local()
        .global({
            let daemon = daemon.clone();
            let registry = registry.clone();
            move |global| {
                let Some(props) = &global.props else { return };
                match global.type_ {
                    ObjectType::Node if props.get("media.class") == Some("Audio/Sink") => {
                        let name = props.get("node.name").unwrap_or_default().to_string();
                        let mut d = daemon.borrow_mut();
                        // Our own sink appearing completes an in-flight apply:
                        // bind its Node proxy (for live pushes) and route the
                        // system default to it.
                        if name.starts_with(VIRTUAL_SINK_PREFIX) && d.awaiting_sink_since.is_some()
                        {
                            if let Ok(node) = registry.bind::<Node, _>(global) {
                                d.virtual_node = Some((global.id, node));
                                d.awaiting_sink_since = None;
                                d.set_default(&name);
                                d.publish_status(None);
                            }
                        }
                    }
                    ObjectType::Metadata if props.get("metadata.name") == Some("default") => {
                        if let Ok(md) = registry.bind::<Metadata, _>(global) {
                            let mut d = daemon.borrow_mut();
                            d.default_meta = Some(md);
                            if let Some(name) = d.pending_default.take() {
                                d.set_default(&name);
                            }
                        }
                    }
                    _ => {}
                }
            }
        })
        .global_remove({
            let daemon = daemon.clone();
            move |id| {
                let mut d = daemon.borrow_mut();
                if d.virtual_node.as_ref().is_some_and(|(vid, _)| *vid == id) {
                    d.virtual_node = None;
                }
            }
        })
        .register();

    // --- exit when the connection dies (autostart / the UI respawns us). ---
    // Error events are NOT all fatal: destroying the filter module reliably
    // provokes a core-level "unknown resource N" complaint as the server's
    // in-flight messages race the teardown. Only a broken socket — EPIPE /
    // ECONNRESET in `res` — means the connection is gone.
    let _core_listener = session
        .core
        .add_listener_local()
        .error({
            let mainloop = session.mainloop.clone();
            move |id, _seq, res, message| {
                if res == -libc::EPIPE || res == -libc::ECONNRESET {
                    eprintln!("adtune-service: PipeWire connection lost: {message}");
                    mainloop.quit();
                } else {
                    eprintln!("adtune-service: PipeWire error on {id} (ignored): {message}");
                }
            }
        })
        .register();

    // --- instant wake-up: an inotify watch on the config dir plugged into the
    // PipeWire loop, so a desired.json write reaches set_param within
    // milliseconds — no polling latency, no idle wake-ups. ------------------
    let inotify = setup_config_watch();
    let has_inotify = inotify.is_some();
    let _io_source = inotify.map(|file| {
        session
            .mainloop
            .loop_()
            .add_io(file, pipewire::spa::support::system::IoFlags::IN, {
                let daemon = daemon.clone();
                let session = session.clone();
                move |file: &mut std::fs::File| {
                    // Drain the event queue (edge clears readiness); the events
                    // themselves don't matter — reconcile re-reads the file.
                    let mut buf = [0u8; 4096];
                    loop {
                        let n = unsafe {
                            libc::read(
                                file.as_raw_fd(),
                                buf.as_mut_ptr() as *mut libc::c_void,
                                buf.len(),
                            )
                        };
                        if n <= 0 {
                            break;
                        }
                    }
                    reconcile_tick(&daemon, &session);
                }
            })
    });

    // --- the reconcile timer: shutdown/timeout/retry bookkeeping, and the
    // polling fallback when inotify isn't available. ------------------------
    let tick_every = if has_inotify {
        Duration::from_millis(250)
    } else {
        Duration::from_millis(100)
    };
    let timer = session.mainloop.loop_().add_timer({
        let daemon = daemon.clone();
        let session = session.clone();
        move |_| reconcile_tick(&daemon, &session)
    });
    timer
        .update_timer(Some(Duration::from_millis(1)), Some(tick_every))
        .into_result()
        .expect("failed to arm the reconcile timer");

    session.mainloop.run();
    // Drop the module cleanly and report inactive — with no error when this
    // is an orderly shutdown (logout), so the UI just shows "off".
    let mut d = daemon.borrow_mut();
    d.module = None;
    d.virtual_node = None;
    let reason = if SHUTDOWN.load(Ordering::Relaxed) {
        None
    } else {
        Some("Lost the PipeWire connection.".into())
    };
    d.publish_status(reason);
}

/// One poll tick: pick up desired.json changes and drive in-flight applies to
/// completion or timeout. Runs on the loop thread; must never block.
fn reconcile_tick(daemon: &Rc<RefCell<Daemon>>, session: &Rc<Session>) {
    if SHUTDOWN.load(Ordering::Relaxed) {
        session.mainloop.quit();
        return;
    }
    // Sandboxed installs: if our snap revision's mount is gone the snap was
    // removed (or refreshed) under us, and nothing else can reach us — snapd
    // can't signal a daemon that detached from the launch scope. Tear the
    // filter down instead of applying EQ from beyond the grave.
    if let Some(snap) = std::env::var_os("SNAP") {
        if !std::path::Path::new(&snap).exists() {
            SHUTDOWN.store(true, Ordering::Relaxed);
            session.mainloop.quit();
            return;
        }
    }
    let mut d = daemon.borrow_mut();

    // An apply is in flight: fail it if the sink never materialized.
    if let Some(since) = d.awaiting_sink_since {
        if since.elapsed() > SINK_TIMEOUT {
            d.awaiting_sink_since = None;
            d.module = None;
            d.applied = None;
            d.retry_at = Some(Instant::now() + RETRY_AFTER);
            d.publish_status(Some(
                "The virtual output did not appear after loading the filter.".into(),
            ));
        }
        return; // wait for the registry event (or the timeout) before more work
    }

    // Cheap change detection: only re-read the file when its mtime moved. A
    // pending retry (after a failed apply) forces a re-read regardless.
    let retry_due = d.retry_at.is_some_and(|t| Instant::now() >= t);
    let mtime = std::fs::metadata(paths::desired_path())
        .and_then(|m| m.modified())
        .ok();
    if mtime == d.desired_mtime && !retry_due {
        return;
    }
    d.desired_mtime = mtime;
    d.retry_at = None;
    let Some(desired) = ipc::read_json::<DesiredState>(&paths::desired_path()) else {
        return; // torn write can't happen (atomic rename); treat as no-op
    };
    if d.applied
        .as_ref()
        .is_some_and(|a| a.revision == desired.revision)
    {
        return;
    }
    d.applying_revision = desired.revision;

    if !desired.enabled {
        // Teardown: unload the filter, hand the default back to the physical
        // target the user calibrated.
        d.module = None;
        d.virtual_node = None;
        if !desired.state.target_name.is_empty() {
            let target = desired.state.target_name.clone();
            d.set_default(&target);
        }
        d.applied = Some(desired);
        d.publish_status(None);
        return;
    }

    let profile = desired.state.profile.to_profile();
    let tone = desired.state.tone.to_tone();

    // Live path: same structure, gains only — push params onto the running node.
    let live_capable = d.module.is_some()
        && d.virtual_node.is_some()
        && d.applied
            .as_ref()
            .is_some_and(|a| a.enabled && !needs_reload(a, &desired));
    if live_capable {
        match live_props_pod(&profile, &tone) {
            Ok(bytes) => {
                if let Some(pod) = Pod::from_bytes(&bytes) {
                    if let Some((_, node)) = &d.virtual_node {
                        node.set_param(pipewire::spa::param::ParamType::Props, 0, pod);
                    }
                    d.applied = Some(desired);
                    d.publish_status(None);
                    return;
                }
            }
            Err(e) => eprintln!("adtune-service: live pod: {e} (falling back to reload)"),
        }
    }

    // Structural path: replace the module and wait for its sink to appear.
    d.module = None; // unload the old graph first so names can't collide
    d.virtual_node = None;
    let args = filter_chain_args(&profile, &desired.state.target_name, &tone);
    match FilterModule::load(session, &args) {
        Ok(module) => {
            d.module = Some(module);
            d.awaiting_sink_since = Some(Instant::now());
            d.applied = Some(desired);
            // status is published when the sink appears (or on timeout)
        }
        Err(e) => {
            d.applied = None;
            d.retry_at = Some(Instant::now() + RETRY_AFTER);
            d.publish_status(Some(e));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adtune_core::{AudioProfile, BandType, FilterBand, ToneSettings};
    use adtune_pipewire::state::StoredState;

    fn desired(profile: &AudioProfile, target: &str, tone: &ToneSettings) -> DesiredState {
        DesiredState {
            revision: 1,
            enabled: true,
            state: StoredState::new(profile, 1, target, tone),
        }
    }

    fn profile(bands: &[(f64, f64, f64)]) -> AudioProfile {
        AudioProfile {
            name: "p".into(),
            bands: bands
                .iter()
                .map(|&(f, g, q)| FilterBand::new(BandType::Peaking, f, g, q))
                .collect(),
            ..Default::default()
        }
    }

    /// Gain-only differences (wet, band gain, headroom-driven preamp) ride the
    /// live set_param path; anything touching the graph shape reloads.
    #[test]
    fn reload_rule_matches_live_capability() {
        let base = profile(&[(100.0, 3.0, 1.0), (2000.0, -2.0, 0.7)]);
        let tone = ToneSettings::default();
        let a = desired(&base, "sink-a", &tone);

        // Same structure, different gains → live.
        let gains = profile(&[(100.0, -6.0, 1.0), (2000.0, 4.0, 0.7)]);
        assert!(!needs_reload(&a, &desired(&gains, "sink-a", &tone)));
        // Tone-only change (wet/bass/tilt scale gains of fixed shelves) → live.
        let bypass = ToneSettings {
            wet: 0.0,
            ..Default::default()
        };
        assert!(!needs_reload(&a, &desired(&base, "sink-a", &bypass)));

        // Frequency moved → reload.
        let moved = profile(&[(120.0, 3.0, 1.0), (2000.0, -2.0, 0.7)]);
        assert!(needs_reload(&a, &desired(&moved, "sink-a", &tone)));
        // Q changed → reload.
        let q = profile(&[(100.0, 3.0, 2.0), (2000.0, -2.0, 0.7)]);
        assert!(needs_reload(&a, &desired(&q, "sink-a", &tone)));
        // Band added → reload.
        let added = profile(&[(100.0, 3.0, 1.0), (2000.0, -2.0, 0.7), (9000.0, 1.0, 1.0)]);
        assert!(needs_reload(&a, &desired(&added, "sink-a", &tone)));
        // Different output device → reload.
        assert!(needs_reload(&a, &desired(&base, "sink-b", &tone)));
    }
}
