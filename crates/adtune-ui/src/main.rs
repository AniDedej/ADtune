//! ADtune cross-platform desktop UI (Slint) on top of `adtune-core`.
//!
//! This is the frontend shell: catalog search, the frequency-response graph,
//! and live tone controls. Applying to the system audio engine (PipeWire on
//! Linux, its own APO on Windows) is a separate backend layer.

// Release Windows builds are GUI apps — link them against the "windows" subsystem
// so double-clicking (or the Start-menu shortcut) doesn't pop up a console window.
// Debug builds keep the console so `cargo run`, the ADTUNE_* test hooks, and the
// installer-invoked `--enable-apo`/`--disable-apo` maintenance modes still print.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod graph;
mod sys;

use adtune_core::{store, AudioProfile, BandType, Catalog, FilterBand, ToneSettings};
use slint::{Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::time::Duration;

slint::include_modules!();

/// All UI-thread-owned application state. Shared as `Rc<RefCell<State>>` and
/// borrowed inside every Slint callback closure; worker threads never touch it
/// directly, they marshal results back through the `tx`/`file_tx` channels.
struct State {
    /// The bundled (or `$ADTUNE_CATALOG`-overridden) profile catalog; search source.
    catalog: Catalog,
    /// The profile shown on the graph and pushed to the backend on apply.
    current: AudioProfile,
    /// Current search-box text (lowercased at query time).
    query: String,
    /// Last-measured plot size in logical pixels, kept so off-screen refreshes
    /// (tone/profile changes) regenerate the curve at the on-screen geometry.
    plot_w: f64,
    plot_h: f64,
    /// Output devices from the most recent backend query (the combo's source).
    outputs: Vec<sys::Device>,
    /// Channel carrying worker-thread `Outcome`s back to the UI thread.
    tx: Sender<Outcome>,
    /// Whether the graph is in edit mode (handles + overlay controls shown).
    edit_mode: bool,
    /// Snapshot of (`current`, `loaded_path`) taken when edit mode was entered,
    /// so Reset can fully restore the pre-edit state.
    edit_baseline: Option<(AudioProfile, Option<std::path::PathBuf>)>,
    /// Index of the band whose handle is selected (-1 = none).
    selected: i32,
    /// True once `current` has been forked into an editable custom profile.
    editing: bool,
    /// True when the current profile has edits not yet saved to the library
    /// (since it was loaded, saved, or reset). Drives the discard confirmations.
    dirty: bool,
    /// Saved/custom profiles from the on-disk library (shown atop the list).
    user_profiles: Vec<SavedProfile>,
    /// Path of the library file the current profile was loaded from (for the
    /// list highlight and for in-place re-save). `None` for catalog/imported.
    loaded_path: Option<std::path::PathBuf>,
    /// Dialog results marshalled back from worker threads.
    file_tx: Sender<FileMsg>,
    /// A destructive action awaiting the confirmation dialog's result.
    pending: Option<Pending>,
    /// Whether the sandbox-permission dialog was already shown this launch
    /// (the state re-surfaces on every poll; the dialog must not).
    grant_dialog_shown: bool,
    /// The band-handle overlay model. Kept as a single persistent model and
    /// mutated in place (never swapped) so an in-progress drag keeps its grab.
    handles_model: Rc<VecModel<BandHandle>>,
}

/// A profile saved in the on-disk library, with its file path (the identity
/// used for the list row, selection highlight, in-place re-save, and delete).
struct SavedProfile {
    path: std::path::PathBuf,
    profile: AudioProfile,
}

/// Load the on-disk profile library (file path + parsed profile).
fn load_library() -> Vec<SavedProfile> {
    store::list_profiles(&sys::user_profiles_dir())
        .into_iter()
        .map(|(path, profile)| SavedProfile { path, profile })
        .collect()
}

/// A file-dialog result (import/export), sent from a worker to the UI thread.
enum FileMsg {
    Imported(AudioProfile),
    Status(String),
    Failed(String),
}

/// A destructive action deferred until the user confirms it in a dialog.
enum Pending {
    DeleteProfile(std::path::PathBuf),
    ResetEdit,
    SwitchProfile(String),
    ImportProfile,
}

/// Show a modal confirmation (Cancel + a confirm button; `destructive` reddens it).
fn open_confirm(
    ui: &MainWindow,
    title: &str,
    message: &str,
    confirm: &str,
    cancel: &str,
    destructive: bool,
) {
    ui.set_dialog_title(title.into());
    ui.set_dialog_message(message.into());
    ui.set_dialog_confirm_label(confirm.into());
    ui.set_dialog_cancel_label(cancel.into());
    ui.set_dialog_destructive(destructive);
    ui.set_dialog_open(true);
}

/// Show a modal notice (single OK button).
fn open_info(ui: &MainWindow, title: &str, message: &str) {
    open_confirm(ui, title, message, "OK", "", false);
}

/// Result of a backend operation, sent from a worker thread to the UI thread and
/// drained by the polling timer. Two shapes share the struct: a full status
/// snapshot (`run_op`) or a fast live push (`push_live`, see `live`).
struct Outcome {
    /// Error text to surface, or `None` on success.
    error: Option<String>,
    /// `true` for a fast live push: only `error` is meaningful and the UI is not
    /// resynced; the snapshot fields below are left at their defaults.
    live: bool,
    /// Whether calibration is currently engaged on the target device.
    active: bool,
    /// Human-readable status line for the footer.
    message: String,
    /// Fresh device list for the output combo.
    outputs: Vec<sys::Device>,
    /// Whether the A/B correction (tone `wet > 0`) is currently on.
    correction_on: bool,
    /// Whether the sandbox blocks PipeWire pending a one-time user grant.
    needs_grant: bool,
}

/// Run a blocking backend op on a worker thread; report a fresh status snapshot.
fn run_op<F>(ui: &MainWindow, tx: &Sender<Outcome>, work: F)
where
    F: FnOnce() -> Option<String> + Send + 'static,
{
    ui.set_busy(true);
    let tx = tx.clone();
    std::thread::spawn(move || {
        let error = work();
        let (active, message) = sys::status();
        let outputs = sys::list_outputs();
        let correction_on = active && sys::active_tone().wet > 0.0;
        let _ = tx.send(Outcome {
            error,
            live: false,
            active,
            message,
            outputs,
            correction_on,
            needs_grant: sys::needs_permission_grant(),
        });
    });
}

/// Push a live tone/bypass change without the busy/UI resync (fast path).
fn push_live<F>(tx: &Sender<Outcome>, work: F)
where
    F: FnOnce() -> Result<bool, String> + Send + 'static,
{
    let tx = tx.clone();
    std::thread::spawn(move || {
        let error = work().err();
        let _ = tx.send(Outcome {
            error,
            live: true,
            active: false,
            message: String::new(),
            outputs: Vec::new(),
            correction_on: false,
            needs_grant: false,
        });
    });
}

/// Open a native "Import" dialog on a worker thread, load the chosen file (an
/// `.adtuneprofile`, or an AutoEq ParametricEQ `.txt`), and send the result
/// back. Never blocks the UI thread (rfd's blocking API parks the event loop).
fn import_profile(file_tx: Sender<FileMsg>) {
    std::thread::spawn(move || {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Import profile")
            .add_filter("ADtune profile (.adtuneprofile)", &["adtuneprofile"])
            .add_filter("AutoEq ParametricEQ (.txt)", &["txt"])
            .add_filter("All files", &["*"])
            .pick_file()
        else {
            return; // cancelled
        };
        let msg = match store::load_profile_file(&path) {
            Ok(p) => FileMsg::Imported(p),
            Err(e) => FileMsg::Failed(e),
        };
        let _ = file_tx.send(msg);
    });
}

/// Open a native "Export" dialog on a worker thread and write the profile as a
/// native `.adtuneprofile` (JSON) — or an AutoEq `.txt` if the user picks that
/// extension. Format is chosen from the resulting file's extension.
fn export_profile(file_tx: Sender<FileMsg>, profile: AudioProfile) {
    std::thread::spawn(move || {
        let base = if profile.key.trim().is_empty() {
            profile.name.clone()
        } else {
            profile.key.replace(':', "-")
        };
        let base = if base.trim().is_empty() {
            "profile".to_string()
        } else {
            base
        };
        let Some(path) = rfd::FileDialog::new()
            .set_title("Export profile")
            .set_file_name(format!("{base}.{}", adtune_core::store::PROFILE_EXT))
            .add_filter(
                "ADtune profile (.adtuneprofile)",
                &[adtune_core::store::PROFILE_EXT],
            )
            .add_filter("AutoEq ParametricEQ (.txt)", &["txt"])
            .save_file()
        else {
            return; // cancelled
        };
        // The portal doesn't reliably report the chosen filter, so pick the
        // format from the resulting extension. Any extension the importer
        // wouldn't recognize as JSON is normalized to `.adtuneprofile`, so an
        // export always re-imports (import: .adtuneprofile/.json → JSON, else
        // ParametricEQ). `.txt` stays ParametricEQ.
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        let (path, body) = match ext.as_deref() {
            Some("txt") => (path, adtune_core::profile_to_parametric_eq(&profile)),
            Some("json") | Some("adtuneprofile") => (path, adtune_core::profile_to_json(&profile)),
            _ => (
                path.with_extension(adtune_core::store::PROFILE_EXT),
                adtune_core::profile_to_json(&profile),
            ),
        };
        let msg = match std::fs::write(&path, body) {
            Ok(()) => FileMsg::Status(format!("Exported to {}", path.display())),
            Err(e) => FileMsg::Status(format!("⚠ Export failed: {e}")),
        };
        let _ = file_tx.send(msg);
    });
}

/// Choose which output the combo should preselect: the previously saved target
/// if it's still present, else the system default, else the first device (or -1
/// when the list is empty).
fn pick_output_index(outputs: &[sys::Device]) -> i32 {
    if let Some(name) = sys::saved_target_name() {
        if let Some(i) = outputs.iter().position(|d| d.name == name) {
            return i as i32;
        }
    }
    if let Some(i) = outputs.iter().position(|d| d.is_default) {
        return i as i32;
    }
    if outputs.is_empty() {
        -1
    } else {
        0
    }
}

/// Apply a full status snapshot to the UI (runs on the UI thread).
fn apply_outcome(ui: &MainWindow, st: &Rc<RefCell<State>>, o: Outcome) {
    // Fast-path push: nothing to resync, just surface any error and return.
    if o.live {
        if let Some(e) = o.error {
            ui.set_status_text(format!("⚠ {e}").into());
        }
        return;
    }
    ui.set_busy(false);
    st.borrow_mut().outputs = o.outputs.clone();
    let names: Vec<SharedString> = o
        .outputs
        .iter()
        .map(|d| d.description.as_str().into())
        .collect();
    ui.set_output_names(ModelRc::new(VecModel::from(names)));
    let sel = pick_output_index(&o.outputs);
    if sel >= 0 {
        ui.set_selected_output(sel);
    }
    ui.set_is_active(o.active);
    ui.set_correction_on(o.correction_on);
    let status = match o.error {
        Some(e) => format!("⚠ {e}"),
        None => o.message,
    };
    ui.set_status_text(status.into());
    // Sandboxed without PipeWire access: walk the user through the one-time
    // grant in a dialog, once per launch (the status line alone is too easy
    // to miss when the device list is just… empty).
    if o.needs_grant && !st.borrow().grant_dialog_shown {
        st.borrow_mut().grant_dialog_shown = true;
        open_info(
            ui,
            "Allow PipeWire access",
            "ADtune needs a one-time permission to talk to PipeWire — until then it \
             cannot list outputs or apply any correction.\n\n\
             Easiest:   open App Center → ADtune → Permissions and enable PipeWire.\n\
             Terminal:  sudo snap connect adtune:pipewire\n\n\
             Then press Refresh.",
        );
    }
}

/// The physical device currently selected in the combo.
fn selected_device(ui: &MainWindow, st: &State) -> Option<sys::Device> {
    let idx = ui.get_selected_output();
    (idx >= 0)
        .then(|| st.outputs.get(idx as usize).cloned())
        .flatten()
}

/// The bundled catalog, or an override from `$ADTUNE_CATALOG`.
fn load_catalog() -> Catalog {
    match std::env::var("ADTUNE_CATALOG") {
        Ok(p) => Catalog::load(std::path::Path::new(&p)).unwrap_or_else(|_| Catalog::bundled()),
        Err(_) => Catalog::bundled(),
    }
}

/// Read the live tone controls off the UI into a `ToneSettings`. `wet` is a
/// 0–100 slider stored as a 0–1 mix fraction.
fn read_tone(ui: &MainWindow) -> ToneSettings {
    ToneSettings {
        wet: ui.get_wet() as f64 / 100.0,
        bass: ui.get_bass() as f64,
        tilt: ui.get_tilt() as f64,
        headroom: ui.get_headroom(),
    }
}

/// Push freshly built graph geometry (paths + labels) into the UI properties.
fn apply_graph(ui: &MainWindow, g: &graph::GraphData) {
    ui.set_curve_commands(g.curve.as_str().into());
    ui.set_fill_commands(g.fill.as_str().into());
    ui.set_grid_commands(g.grid.as_str().into());
    ui.set_zero_commands(g.zero.as_str().into());
    ui.set_graph_title(g.title.as_str().into());
    ui.set_graph_caption(g.caption.as_str().into());
    let to_model = |labels: &[(String, f32)]| {
        let v: Vec<AxisLabel> = labels
            .iter()
            .map(|(t, f)| AxisLabel {
                text: t.as_str().into(),
                frac: *f,
            })
            .collect();
        ModelRc::new(VecModel::from(v))
    };
    ui.set_x_labels(to_model(&g.x_labels));
    ui.set_y_labels(to_model(&g.y_labels));
}

/// Rebuild and re-push the response curve for the current profile + tone at the
/// last-known plot size. The curve only (no handle overlay); see `refresh_edit`.
/// Also keeps the window title in step with the profile on screen.
fn refresh_graph(ui: &MainWindow, st: &State) {
    apply_graph(
        ui,
        &graph::build_graph(&st.current, &read_tone(ui), st.plot_w, st.plot_h),
    );
    let title = if st.current.key == "flat" || st.current.name.is_empty() {
        "ADtune".to_string()
    } else {
        format!("ADtune - {}", st.current.name)
    };
    ui.set_window_title(title.into());
}

/// `8850` → `"8,850"` for the results-count line.
fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Rebuild the results list: saved/custom library profiles (matching the query)
/// first, then ALL catalog search hits — the ListView is virtualized (it only
/// instantiates visible rows), so the full 8,850-model catalog scrolls fine
/// and nothing is silently truncated. Also updates the count line under the
/// search box. `selected`/`deletable` drive the row's highlight and delete
/// affordance.
fn refresh_results(ui: &MainWindow, st: &State, model: &VecModel<ResultItem>) {
    let q = st.query.to_lowercase();
    // Saved/custom profiles first, filtered by the same query. Their list key is
    // "lib:<stem>" (unique per file) so imported profiles that share a name stay
    // distinguishable for selection + delete.
    let mut items: Vec<ResultItem> = st
        .user_profiles
        .iter()
        .filter(|sp| q.is_empty() || sp.profile.name.to_lowercase().contains(&q))
        .map(|sp| ResultItem {
            // The full path is the list identity (unique per file), so files that
            // happen to share a stem stay individually addressable.
            key: format!("lib:{}", sp.path.to_string_lossy()).into(),
            name: sp.profile.name.as_str().into(),
            sub: "★ saved profile".into(),
            selected: st.loaded_path.as_deref() == Some(sp.path.as_path()),
            deletable: true,
        })
        .collect();
    let saved = items.len();
    let hits = st.catalog.search(&st.query, "", usize::MAX);
    items.extend(hits.iter().map(|p| ResultItem {
        key: p.key.as_str().into(),
        name: p.name.as_str().into(),
        sub: format!("{} · {}", p.form, p.source).into(),
        selected: st.loaded_path.is_none() && p.key == st.current.key,
        deletable: false,
    }));
    // The count line answers "am I seeing everything?" without adding UI.
    let count = if st.query.is_empty() {
        match saved {
            0 => format!("{} models", thousands(hits.len())),
            n => format!("{} models · {n} saved", thousands(hits.len())),
        }
    } else {
        match items.len() {
            1 => "1 match".to_string(),
            n => format!("{} matches", thousands(n)),
        }
    };
    ui.set_results_count(count.into());
    model.set_vec(items);
}

/// Band type → the integer the Slint type dropdown uses.
fn kind_to_i(k: BandType) -> i32 {
    match k {
        BandType::Peaking => 0,
        BandType::LowShelf => 1,
        BandType::HighShelf => 2,
    }
}

/// Inverse of [`kind_to_i`]: dropdown index → band type (unknown → peaking).
fn i_to_kind(i: i32) -> BandType {
    match i {
        1 => BandType::LowShelf,
        2 => BandType::HighShelf,
        _ => BandType::Peaking,
    }
}

/// Format a frequency for band labels (kHz above 1000 Hz, otherwise whole Hz).
fn fmt_hz(f: f64) -> String {
    if f >= 1000.0 {
        format!("{:.1} kHz", f / 1000.0)
    } else {
        format!("{:.0} Hz", f)
    }
}

/// The launch default when nothing is actively calibrating: a flat, no-op
/// profile the user can shape from scratch or replace from the catalog.
fn flat_profile() -> AudioProfile {
    AudioProfile {
        key: "flat".into(),
        name: "Flat".into(),
        detail: "no correction".into(),
        ..Default::default()
    }
}

/// One draggable handle for band `i`, placed in plot pixel space. `range` is
/// the plot's dB half-range (see [`graph::db_range`]) — it already fits every
/// band, the clamp is only a safety net.
fn build_handle(b: &FilterBand, i: usize, w: f64, h: f64, range: f64) -> BandHandle {
    BandHandle {
        x: graph::x_of(b.frequency, w) as f32,
        y: graph::y_of(b.gain.clamp(-range, range), h, range) as f32,
        idx: i as i32,
        kind: kind_to_i(b.kind),
    }
}

/// Push the current selection into the inline-editor properties. Does NOT touch
/// the handle model, so it is safe to call during a drag (the tint is driven by
/// the `selected-band` property binding, not by rebuilding rows).
fn update_editor(ui: &MainWindow, st: &State) {
    ui.set_selected_band(st.selected);
    match (st.selected >= 0)
        .then(|| st.current.bands.get(st.selected as usize))
        .flatten()
    {
        Some(b) => {
            ui.set_has_selection(true);
            ui.set_sel_type(kind_to_i(b.kind));
            ui.set_sel_q(b.q as f32);
            ui.set_sel_label(
                format!(
                    "Band {} · {} · {} · {:+.1} dB",
                    st.selected + 1,
                    b.kind.eq_code(),
                    fmt_hz(b.frequency),
                    b.gain
                )
                .into(),
            );
        }
        None => ui.set_has_selection(false),
    }
}

/// Rebuild every handle row from the current bands. Only call this when NOT in
/// the middle of a drag gesture (profile switch, add/remove, resize, tone) — an
/// active drag updates a single row in place via `set_row_data` instead, so the
/// grabbed repeater item survives. Handles only exist in edit mode.
fn refresh_handles(ui: &MainWindow, st: &State) {
    if !st.edit_mode {
        st.handles_model.set_vec(Vec::new());
        ui.set_has_selection(false);
        ui.set_selected_band(-1);
        return;
    }
    let (w, h) = (st.plot_w, st.plot_h);
    let range = graph::db_range(&st.current);
    let v: Vec<BandHandle> = st
        .current
        .bands
        .iter()
        .enumerate()
        .map(|(i, b)| build_handle(b, i, w, h, range))
        .collect();
    st.handles_model.set_vec(v);
    update_editor(ui, st);
}

/// Regenerate both the curve and the (full) handle overlay. Not for use mid-drag.
fn refresh_edit(ui: &MainWindow, st: &State) {
    refresh_graph(ui, st);
    refresh_handles(ui, st);
}

/// On the first edit of a pristine catalog profile, fork it into an editable
/// custom copy (re-keyed + renamed) so the catalog entry stays untouched and
/// re-selecting the original key reloads pristine data.
fn ensure_editable(st: &mut State) {
    if !st.editing {
        if st.catalog.get(&st.current.key).is_some() {
            let base = st.current.key.clone();
            st.current.key = format!("custom:{base}");
            st.current.name = format!("{} (custom)", st.current.name);
        }
        st.editing = true;
    }
    st.dirty = true; // any edit marks the profile as having unsaved changes
}

/// Load `key` (a `lib:<path>` library item or a catalog key) as the current
/// profile, ending any edit session. Clears the dirty flag.
fn perform_select(ui: &MainWindow, st: &Rc<RefCell<State>>, rm: &VecModel<ResultItem>, key: &str) {
    {
        let mut s = st.borrow_mut();
        if let Some(path) = key.strip_prefix("lib:") {
            let found = s
                .user_profiles
                .iter()
                .find(|sp| sp.path.as_os_str() == path)
                .map(|sp| (sp.profile.clone(), sp.path.clone()));
            if let Some((profile, spath)) = found {
                s.current = profile;
                s.loaded_path = Some(spath);
                s.selected = -1;
                s.editing = true;
            }
        } else if let Some(p) = s.catalog.get(key).cloned() {
            s.current = p;
            s.loaded_path = None;
            s.selected = -1;
            s.editing = s.current.key.starts_with("custom:");
        }
        // A fresh load ends any edit session and is clean.
        s.edit_mode = false;
        s.edit_baseline = None;
        s.dirty = false;
    }
    ui.set_edit_mode(false);
    let s = st.borrow();
    refresh_edit(ui, &s);
    refresh_results(ui, &s, rm);
}

/// Discard the current edit session: restore the pre-edit snapshot, leave edit
/// mode, and push the reverted profile live if calibration is active.
fn perform_reset(ui: &MainWindow, st: &Rc<RefCell<State>>, rm: &VecModel<ResultItem>) {
    {
        let mut s = st.borrow_mut();
        if let Some((prof, path)) = s.edit_baseline.take() {
            s.current = prof;
            s.loaded_path = path;
        }
        s.editing = s.current.key.starts_with("custom:");
        s.selected = -1;
        s.edit_mode = false;
        s.dirty = false;
    }
    ui.set_edit_mode(false);
    let s = st.borrow();
    refresh_edit(ui, &s);
    refresh_results(ui, &s, rm); // name/key may have reverted → fix highlight
    if ui.get_is_active() {
        if let Some(dev) =
            sys::saved_target_name().and_then(|n| s.outputs.iter().find(|d| d.name == n).cloned())
        {
            let profile = s.current.clone();
            let tone = read_tone(ui);
            push_live(&s.tx, move || sys::update_live(&profile, &dev, &tone));
        }
    }
}

/// Render one representative frame off-screen with the pure-Rust software
/// renderer and write it as a PNG, then exit. Used to produce marketing/README
/// shots headlessly (no event loop, no GPU): it does a first layout pass to size
/// the plot, regenerates the graph at those pixels, populates edit mode with a
/// selected band, draws the final frame, and converts the RGB565 buffer to RGBA.
/// Size comes from `$ADTUNE_SHOT_SIZE` (`WxH`), defaulting to 980x660.
#[cfg(feature = "screenshot")]
fn render_screenshot(path: &str, state: &State) -> Result<(), Box<dyn std::error::Error>> {
    use slint::platform::software_renderer::{
        MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel,
    };
    use slint::platform::{Platform, PlatformError, WindowAdapter};
    use std::time::{Duration, Instant};

    struct P {
        window: Rc<MinimalSoftwareWindow>,
        start: Instant,
    }
    impl Platform for P {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
            Ok(self.window.clone())
        }
        fn duration_since_start(&self) -> Duration {
            self.start.elapsed()
        }
    }

    let (w, h): (u32, u32) = std::env::var("ADTUNE_SHOT_SIZE")
        .ok()
        .and_then(|s| {
            let (a, b) = s.split_once('x')?;
            Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
        })
        .unwrap_or((980, 660));
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(P {
        window: window.clone(),
        start: Instant::now(),
    }))
    .map_err(|e| format!("set_platform: {e:?}"))?;

    let ui = MainWindow::new()?;
    let results_model = Rc::new(VecModel::<ResultItem>::default());
    ui.set_results(ModelRc::from(results_model.clone()));
    refresh_results(&ui, state, &results_model);
    refresh_graph(&ui, state);
    // sample backend state so the System-output card previews representatively
    let devices = [
        "USB PnP Audio Device Analog Stereo",
        "Built-in Audio Analog Stereo",
    ];
    ui.set_output_names(ModelRc::new(VecModel::from(
        devices
            .iter()
            .map(|d| SharedString::from(*d))
            .collect::<Vec<_>>(),
    )));
    ui.set_selected_output(0);
    ui.set_is_active(true);
    ui.set_correction_on(true);
    ui.set_status_text("Active — output 76".into());
    // Edit mode before the first draw so the toggle's opacity isn't mid-animation.
    ui.set_edit_mode(true);

    window.set_size(slint::PhysicalSize::new(w, h));
    let mut buf = vec![Rgb565Pixel(0); (w * h) as usize];
    // First layout pass to size the plot, then regenerate the graph at that
    // pixel size (the resize callback needs an event loop, which we don't run
    // here), then render the final frame.
    window.draw_if_needed(|r| {
        r.render(&mut buf, w as usize);
    });
    let (pw, ph) = (ui.get_plot_px_w() as f64, ui.get_plot_px_h() as f64);
    if pw > 10.0 && ph > 10.0 {
        apply_graph(
            &ui,
            &graph::build_graph(&state.current, &ToneSettings::default(), pw, ph),
        );
        // Show edit mode with handles + a selected band for a representative shot.
        ui.set_edit_mode(true);
        let range = graph::db_range(&state.current);
        let handles: Vec<BandHandle> = state
            .current
            .bands
            .iter()
            .enumerate()
            .map(|(i, b)| build_handle(b, i, pw, ph, range))
            .collect();
        ui.set_band_handles(ModelRc::new(VecModel::from(handles)));
        if let Some(b) = state.current.bands.first() {
            ui.set_selected_band(0);
            ui.set_has_selection(true);
            ui.set_sel_type(kind_to_i(b.kind));
            ui.set_sel_q(b.q as f32);
            ui.set_sel_label(
                format!(
                    "Band 1 · {} · {} · {:+.1} dB",
                    b.kind.eq_code(),
                    fmt_hz(b.frequency),
                    b.gain
                )
                .into(),
            );
        }
    }
    window.draw_if_needed(|r| {
        r.render(&mut buf, w as usize);
    });

    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for px in &buf {
        let v = px.0;
        let r5 = ((v >> 11) & 0x1f) as u8;
        let g6 = ((v >> 5) & 0x3f) as u8;
        let b5 = (v & 0x1f) as u8;
        rgba.push((r5 << 3) | (r5 >> 2));
        rgba.push((g6 << 2) | (g6 >> 4));
        rgba.push((b5 << 3) | (b5 >> 2));
        rgba.push(255);
    }
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&rgba)?;
    println!("wrote screenshot {path} ({w}x{h})");
    Ok(())
}

/// Slint constructs its renderer lazily (every renderer is `new_suspended`), so
/// when the GPU/OpenGL path only fails once the window resumes — VMs, RDP, headless
/// or ancient GPUs, where FemtoVG can't locate `glCreateShader` — Slint has already
/// committed to it and cannot fall back to software on its own. Detect that specific
/// failure and relaunch ourselves with the pure-Rust software renderer forced. Any
/// other error is returned unchanged, and we never relaunch twice (the child already
/// carries `SLINT_BACKEND`).
fn handle_backend_error<E: std::error::Error + 'static>(
    e: E,
) -> Result<(), Box<dyn std::error::Error>> {
    let msg = e.to_string();
    let gl_missing = msg.contains("glCreateShader") || msg.contains("Failed to initialize OpenGL");
    let already_software = std::env::var("SLINT_BACKEND")
        .map(|v| v.contains("software"))
        .unwrap_or(false);
    if gl_missing && !already_software {
        let status = std::process::Command::new(std::env::current_exe()?)
            .args(std::env::args_os().skip(1))
            .env("SLINT_BACKEND", "winit-software")
            .status()?;
        std::process::exit(status.code().unwrap_or(1));
    }
    Err(Box::new(e))
}

/// Entry point. Handles the Windows elevated maintenance modes first, then boots
/// the catalog + shared `State`, builds the window, wires every Slint callback to
/// its closure, kicks off the initial backend query, and runs the event loop
/// (relaunching with the software renderer if GPU init fails, see
/// [`handle_backend_error`]). The `ADTUNE_*` env vars gate headless test paths.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Elevated maintenance modes the installer (and the in-app master switch on
    // Windows) invoke as a separate elevated process: register/unregister the APO
    // for a device or machine-wide, reload the audio engine, then exit.
    #[cfg(target_os = "windows")]
    {
        let args: Vec<String> = std::env::args().collect();
        if let Some(pos) = args.iter().position(|a| a == "--enable-apo") {
            // Optional endpoint id after the flag → enable that device; otherwise
            // the current default (the installer invokes the bare form).
            let target = args.get(pos + 1).filter(|a| !a.starts_with("--"));
            let result: adtune_windows::Result<()> = match target {
                Some(id) => adtune_windows::enable_apo_on(id),
                None => adtune_windows::enable_default_apo().map(|_| ()),
            };
            // Registration is the critical step; the engine reload is best-effort
            // (the APO would otherwise load at the next device init). Record the
            // registration outcome so the unprivileged UI can show the real reason.
            if result.is_ok() {
                let _ = adtune_windows::restart_audio_engine();
            }
            adtune_windows::record_op_result(&result);
            return match result {
                Ok(()) => Ok(()),
                Err(e) => {
                    eprintln!("Could not enable ADtune (run as administrator): {e}");
                    Err(e.into())
                }
            };
        }
        if let Some(pos) = args.iter().position(|a| a == "--disable-apo") {
            // Optional endpoint id → unregister just that device (the in-app
            // switch); the bare form sweeps every device (uninstall).
            let result: adtune_windows::Result<()> =
                match args.get(pos + 1).filter(|a| !a.starts_with("--")) {
                    Some(id) => adtune_windows::disable_apo_on(id),
                    None => adtune_windows::disable_apo_everywhere(),
                };
            if result.is_ok() {
                let _ = adtune_windows::restart_audio_engine();
            }
            adtune_windows::record_op_result(&result);
            return match result {
                Ok(()) => Ok(()),
                Err(e) => Err(e.into()),
            };
        }
    }

    let catalog = load_catalog();
    // Start from whatever is actively calibrating right now, so the UI opens
    // showing the truth; otherwise a flat, no-op profile — never a surprise
    // correction preselected.
    let current = sys::active_profile().unwrap_or_else(flat_profile);

    let (tx, rx) = std::sync::mpsc::channel::<Outcome>();
    let (file_tx, file_rx) = std::sync::mpsc::channel::<FileMsg>();

    // Live-update worker: a single thread that serializes every streaming
    // push (band drags, tone changes) to the backend, coalescing to the
    // newest state when events arrive faster than pushes complete
    // (latest-wins). One writer means revisions can't race; coalescing means
    // a 60 Hz drag degrades to "as fast as the backend acks" instead of
    // queueing stale intermediate states.
    let (live_tx, live_rx) =
        std::sync::mpsc::channel::<(AudioProfile, sys::Device, ToneSettings)>();
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            while let Ok(mut latest) = live_rx.recv() {
                while let Ok(newer) = live_rx.try_recv() {
                    latest = newer;
                }
                let (profile, dev, tone) = latest;
                let error = sys::update_live(&profile, &dev, &tone).err();
                let _ = tx.send(Outcome {
                    error,
                    live: true,
                    active: false,
                    message: String::new(),
                    outputs: Vec::new(),
                    correction_on: false,
                    needs_grant: false,
                });
            }
        });
    }
    let handles_model = Rc::new(VecModel::<BandHandle>::default());
    let state = Rc::new(RefCell::new(State {
        catalog,
        current,
        grant_dialog_shown: false,
        query: String::new(),
        plot_w: 600.0,
        plot_h: 180.0,
        outputs: Vec::new(),
        tx,
        edit_mode: false,
        edit_baseline: None,
        selected: -1,
        editing: false,
        dirty: false,
        user_profiles: load_library(),
        loaded_path: None,
        file_tx,
        pending: None,
        handles_model: handles_model.clone(),
    }));

    #[cfg(feature = "screenshot")]
    if let Ok(path) = std::env::var("ADTUNE_SCREENSHOT") {
        render_screenshot(&path, &state.borrow())?;
        return Ok(());
    }

    let ui = match MainWindow::new() {
        Ok(ui) => ui,
        Err(e) => return handle_backend_error(e),
    };
    // Tie this window to io.github.anidedej.ADtune.desktop so the GNOME/Wayland
    // dock shows the app name + icon instead of "Unknown". Must run after the
    // backend exists (MainWindow::new created it); no-op on Windows/macOS.
    #[cfg(target_os = "linux")]
    let _ = slint::set_xdg_app_id("io.github.anidedej.ADtune");

    let results_model = Rc::new(VecModel::<ResultItem>::default());
    ui.set_results(ModelRc::from(results_model.clone()));
    // One persistent handle model, mutated in place from here on.
    ui.set_band_handles(ModelRc::from(handles_model.clone()));

    // Search box: store the query and re-filter the results list.
    {
        let st = state.clone();
        let rm = results_model.clone();
        let weak = ui.as_weak();
        ui.on_search(move |text: SharedString| {
            let Some(ui) = weak.upgrade() else { return };
            st.borrow_mut().query = text.to_string();
            refresh_results(&ui, &st.borrow(), &rm);
        });
    }
    // Select a profile from the list (confirm first if edits would be discarded).
    {
        let st = state.clone();
        let rm = results_model.clone();
        let weak = ui.as_weak();
        ui.on_select_profile(move |key: SharedString| {
            let Some(ui) = weak.upgrade() else { return };
            // Warn before switching away from a profile with unsaved edits.
            let dirty = st.borrow().dirty;
            if dirty {
                st.borrow_mut().pending = Some(Pending::SwitchProfile(key.to_string()));
                open_confirm(
                    &ui,
                    "Discard your edits?",
                    "You have unsaved changes to this profile. Switching to another will discard them.",
                    "Discard & switch",
                    "Keep editing",
                    true,
                );
            } else {
                perform_select(&ui, &st, &rm, key.as_str());
            }
        });
    }
    // tone changed -> graph preview + a short-debounced live push through the
    // streaming worker (which serializes + coalesces with any drag pushes)
    {
        let st = state.clone();
        let weak = ui.as_weak();
        let live_tx = live_tx.clone();
        let tone_timer = Rc::new(Timer::default());
        ui.on_tone_changed(move || {
            let Some(ui) = weak.upgrade() else { return };
            refresh_edit(&ui, &st.borrow());
            let st2 = st.clone();
            let weak2 = ui.as_weak();
            let live_tx = live_tx.clone();
            tone_timer.start(
                TimerMode::SingleShot,
                Duration::from_millis(30),
                move || {
                    let Some(ui) = weak2.upgrade() else { return };
                    if !ui.get_is_active() {
                        return;
                    }
                    let Some(profile) = sys::active_profile() else {
                        return;
                    };
                    let s = st2.borrow();
                    let dev = sys::saved_target_name()
                        .and_then(|name| s.outputs.iter().find(|d| d.name == name).cloned());
                    let Some(dev) = dev else { return };
                    let _ = live_tx.send((profile, dev, read_tone(&ui)));
                },
            );
        });
    }
    // plot resized -> regenerate the graph at the new pixel size
    {
        let st = state.clone();
        let weak = ui.as_weak();
        ui.on_graph_resized(move |w: f32, h: f32| {
            let Some(ui) = weak.upgrade() else { return };
            if w as f64 > 10.0 && h as f64 > 10.0 {
                {
                    let mut s = st.borrow_mut();
                    s.plot_w = w as f64;
                    s.plot_h = h as f64;
                }
                refresh_edit(&ui, &st.borrow());
            }
        });
    }

    // --- on-graph band editing ---
    // Toggle edit mode. Entering snapshots (current, loaded_path) as the Reset
    // baseline; leaving (the ✔) commits: edits made to a bundled catalog
    // profile (or an unsaved import) become a NEW library profile right there,
    // so the bundled entry always reloads factory-pristine and the edits are
    // never a floating unsaved copy. Library profiles keep in-place semantics
    // (the Save button overwrites their file).
    {
        let st = state.clone();
        let rm = results_model.clone();
        let weak = ui.as_weak();
        ui.on_toggle_edit(move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut status: Option<String> = None;
            {
                let mut s = st.borrow_mut();
                s.edit_mode = !s.edit_mode;
                if s.edit_mode {
                    s.edit_baseline = Some((s.current.clone(), s.loaded_path.clone()));
                // for Reset
                } else {
                    s.selected = -1;
                    if s.dirty {
                        // The ✔ is the single commit gesture (there is no
                        // separate Save button): library profiles update
                        // their own file, everything else becomes a new
                        // library profile.
                        let result = match s.loaded_path.clone() {
                            Some(path) => store::overwrite_profile(&path, &s.current).map(|_| path),
                            None => store::save_profile(&sys::user_profiles_dir(), &s.current),
                        };
                        match result {
                            Ok(path) => {
                                s.loaded_path = Some(path);
                                s.user_profiles = load_library();
                                s.dirty = false;
                                status =
                                    Some(format!("Saved “{}” to your library.", s.current.name));
                            }
                            Err(e) => {
                                status = Some(format!("⚠ Could not save the edited profile: {e}"))
                            }
                        }
                    }
                }
            }
            let s = st.borrow();
            ui.set_edit_mode(s.edit_mode);
            refresh_edit(&ui, &s);
            if let Some(msg) = status {
                refresh_results(&ui, &s, &rm); // the library list gained an entry
                ui.set_status_text(msg.into());
            }
        });
    }
    // Reset button: revert to the edit baseline (via `perform_reset`).
    {
        let st = state.clone();
        let rm = results_model.clone();
        let weak = ui.as_weak();
        ui.on_reset_edit(move || {
            let Some(ui) = weak.upgrade() else { return };
            // Confirm before discarding — but skip the dialog if nothing changed.
            let dirty = st.borrow().dirty;
            if dirty {
                st.borrow_mut().pending = Some(Pending::ResetEdit);
                open_confirm(
                    &ui,
                    "Discard your edits?",
                    "This resets the graph to how it was before you started editing this profile.",
                    "Discard",
                    "Keep editing",
                    true,
                );
            } else {
                perform_reset(&ui, &st, &rm);
            }
        });
    }
    {
        let st = state.clone();
        let weak = ui.as_weak();
        ui.on_band_select(move |idx| {
            let Some(ui) = weak.upgrade() else { return };
            st.borrow_mut().selected = idx;
            // Fires on pointer-down at the start of a drag: only update the editor
            // + selection tint (a property), never the handle model, or the just-
            // grabbed repeater item would be torn down and the drag would break.
            update_editor(&ui, &st.borrow());
        });
    }
    {
        let st = state.clone();
        let weak = ui.as_weak();
        let live_tx = live_tx.clone();
        ui.on_band_drag(move |idx, x_px, y_px| {
            let Some(ui) = weak.upgrade() else { return };
            {
                let mut s = st.borrow_mut();
                ensure_editable(&mut s);
                let (w, h) = (s.plot_w, s.plot_h);
                // Drag coordinates map through the plot's current range; the
                // range itself only changes on band edits beyond it, and the
                // full overlay rebuild at drag end re-syncs everything.
                let range = graph::db_range(&s.current);
                if let Some(b) = s.current.bands.get_mut(idx as usize) {
                    b.frequency = graph::freq_of_x(x_px as f64, w).clamp(graph::FMIN, graph::FMAX);
                    b.gain = graph::db_of_y(y_px as f64, h, range).clamp(-range, range);
                }
                s.selected = idx;
            }
            // Mid-drag: refresh the curve, and update ONLY the dragged handle row
            // in place (band count is unchanged) so the pointer grab survives.
            let s = st.borrow();
            refresh_graph(&ui, &s);
            if let Some(b) = s.current.bands.get(idx as usize) {
                let range = graph::db_range(&s.current);
                let handle = build_handle(b, idx as usize, s.plot_w, s.plot_h, range);
                if (idx as usize) < s.handles_model.row_count() {
                    s.handles_model.set_row_data(idx as usize, handle);
                }
            }
            update_editor(&ui, &s);
            // Stream the edit to the running filter as the point moves — the
            // worker coalesces to the newest state, so this is audible in
            // near-real-time without flooding the backend.
            if ui.get_is_active() {
                if let Some(dev) = sys::saved_target_name()
                    .and_then(|n| s.outputs.iter().find(|d| d.name == n).cloned())
                {
                    let _ = live_tx.send((s.current.clone(), dev, read_tone(&ui)));
                }
            }
        });
    }
    // Drag released: push the final state through the same worker (it
    // serializes behind any in-flight mid-drag push, so newest always lands).
    {
        let st = state.clone();
        let weak = ui.as_weak();
        let live_tx = live_tx.clone();
        ui.on_band_drag_end(move || {
            let Some(ui) = weak.upgrade() else { return };
            let s = st.borrow();
            if ui.get_is_active() {
                if let Some(dev) = sys::saved_target_name()
                    .and_then(|n| s.outputs.iter().find(|d| d.name == n).cloned())
                {
                    let _ = live_tx.send((s.current.clone(), dev, read_tone(&ui)));
                }
            }
        });
    }
    // Add a band: double-click/tap on empty plot space creates a peaking band at
    // that frequency/gain and selects it. Full overlay rebuild (band count grew).
    {
        let st = state.clone();
        let weak = ui.as_weak();
        ui.on_plot_add_band(move |x_px, y_px| {
            let Some(ui) = weak.upgrade() else { return };
            {
                let mut s = st.borrow_mut();
                ensure_editable(&mut s);
                let (w, h) = (s.plot_w, s.plot_h);
                let range = graph::db_range(&s.current);
                let f = graph::freq_of_x(x_px as f64, w).clamp(graph::FMIN, graph::FMAX);
                let g = graph::db_of_y(y_px as f64, h, range).clamp(-range, range);
                s.current
                    .bands
                    .push(FilterBand::new(BandType::Peaking, f, g, 1.0));
                s.selected = s.current.bands.len() as i32 - 1;
            }
            refresh_edit(&ui, &st.borrow());
        });
    }
    // Remove the selected band and clear the selection. Full overlay rebuild.
    {
        let st = state.clone();
        let weak = ui.as_weak();
        ui.on_band_remove(move || {
            let Some(ui) = weak.upgrade() else { return };
            {
                let mut s = st.borrow_mut();
                ensure_editable(&mut s);
                let i = s.selected;
                if i >= 0 && (i as usize) < s.current.bands.len() {
                    s.current.bands.remove(i as usize);
                }
                s.selected = -1;
            }
            refresh_edit(&ui, &st.borrow());
        });
    }
    {
        let st = state.clone();
        let weak = ui.as_weak();
        // Mouse-wheel over a handle (or the plot with a band selected) tunes Q.
        ui.on_band_q_scroll(move |idx, dy| {
            let Some(ui) = weak.upgrade() else { return };
            {
                let mut s = st.borrow_mut();
                ensure_editable(&mut s);
                // Wheel up (positive dy) sharpens (raises Q); magnitude-scaled and
                // capped so a mouse notch and a touchpad both feel reasonable.
                let d = (dy as f64).clamp(-40.0, 40.0);
                if let Some(b) = (idx >= 0)
                    .then(|| s.current.bands.get_mut(idx as usize))
                    .flatten()
                {
                    b.q = (b.q * (1.0 + d * 0.006)).clamp(0.1, 20.0);
                }
                s.selected = idx;
            }
            // Q doesn't move the handle, so only the curve + editor need refreshing.
            let s = st.borrow();
            refresh_graph(&ui, &s);
            update_editor(&ui, &s);
        });
    }
    // Selected band's type changed in the dropdown (peaking/low-shelf/high-shelf).
    {
        let st = state.clone();
        let weak = ui.as_weak();
        ui.on_band_type_changed(move |t| {
            let Some(ui) = weak.upgrade() else { return };
            {
                let mut s = st.borrow_mut();
                ensure_editable(&mut s);
                let i = s.selected;
                if let Some(b) = (i >= 0)
                    .then(|| s.current.bands.get_mut(i as usize))
                    .flatten()
                {
                    b.kind = i_to_kind(t);
                }
            }
            refresh_edit(&ui, &st.borrow());
        });
    }

    // --- profile library: import / export / save ---
    // Import from a file (confirm first if there are unsaved edits to discard).
    {
        let st = state.clone();
        let weak = ui.as_weak();
        ui.on_import_profile(move || {
            let Some(ui) = weak.upgrade() else { return };
            if st.borrow().dirty {
                st.borrow_mut().pending = Some(Pending::ImportProfile);
                open_confirm(
                    &ui,
                    "Discard your edits?",
                    "Importing a profile will discard your unsaved changes.",
                    "Discard & import",
                    "Keep editing",
                    true,
                );
            } else {
                import_profile(st.borrow().file_tx.clone());
            }
        });
    }
    // Export the current profile to a file the user picks (no dirty check —
    // exporting never loses in-app edits).
    {
        let st = state.clone();
        ui.on_export_profile(move || {
            let s = st.borrow();
            export_profile(s.file_tx.clone(), s.current.clone());
        });
    }
    // (No Save button: leaving edit mode via the ✔ commits to the library.)
    {
        let st = state.clone();
        let weak = ui.as_weak();
        // The list's ✕ opens a confirmation; the actual delete runs on confirm.
        ui.on_delete_profile(move |key: SharedString| {
            let Some(ui) = weak.upgrade() else { return };
            let info = {
                let s = st.borrow();
                key.as_str().strip_prefix("lib:").and_then(|p| {
                    s.user_profiles
                        .iter()
                        .find(|sp| sp.path.as_os_str() == p)
                        .map(|sp| (sp.path.clone(), sp.profile.name.clone()))
                })
            };
            if let Some((path, name)) = info {
                st.borrow_mut().pending = Some(Pending::DeleteProfile(path));
                open_confirm(
                    &ui,
                    "Delete profile?",
                    &format!("“{name}” will be permanently removed from your library."),
                    "Delete",
                    "Cancel",
                    true,
                );
            }
        });
    }
    // Confirm dialog accepted: dispatch whichever destructive action was parked
    // in `pending` (reset / switch / import / delete). Cancel just clears it.
    {
        let st = state.clone();
        let rm = results_model.clone();
        let weak = ui.as_weak();
        ui.on_dialog_confirm(move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_dialog_open(false);
            let action = st.borrow_mut().pending.take();
            if let Some(Pending::ResetEdit) = action {
                perform_reset(&ui, &st, &rm);
            } else if let Some(Pending::SwitchProfile(key)) = action {
                perform_select(&ui, &st, &rm, &key);
            } else if let Some(Pending::ImportProfile) = action {
                import_profile(st.borrow().file_tx.clone());
            } else if let Some(Pending::DeleteProfile(path)) = action {
                let msg = {
                    let mut s = st.borrow_mut();
                    match s.user_profiles.iter().position(|sp| sp.path == path) {
                        Some(pos) => {
                            let sp = s.user_profiles.remove(pos);
                            let name = sp.profile.name.clone();
                            let _ = store::delete_profile(&sp.path);
                            if s.loaded_path.as_deref() == Some(sp.path.as_path()) {
                                // The deleted profile was the one on screen —
                                // never keep showing a ghost. Fall back to the
                                // bundled profile it was forked from (or Flat
                                // for imports), ending any edit session.
                                let base =
                                    s.current.key.strip_prefix("custom:").map(str::to_string);
                                s.current = base
                                    .and_then(|key| s.catalog.get(&key).cloned())
                                    .unwrap_or_else(flat_profile);
                                s.loaded_path = None;
                                s.editing = false;
                                s.dirty = false;
                                s.selected = -1;
                                s.edit_mode = false;
                                s.edit_baseline = None;
                            }
                            format!("Deleted “{name}” from your library.")
                        }
                        None => String::new(),
                    }
                };
                let s = st.borrow();
                ui.set_edit_mode(s.edit_mode);
                refresh_edit(&ui, &s);
                refresh_results(&ui, &s, &rm);
                if !msg.is_empty() {
                    ui.set_status_text(msg.into());
                }
            }
        });
    }
    // Confirm dialog dismissed: drop the parked action, change nothing.
    {
        let st = state.clone();
        let weak = ui.as_weak();
        ui.on_dialog_cancel(move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_dialog_open(false);
            st.borrow_mut().pending = None;
        });
    }

    // --- backend: apply / enable / correction / refresh ---
    // Apply the current profile + tone to the selected device on a worker thread.
    {
        let st = state.clone();
        let weak = ui.as_weak();
        ui.on_apply(move || {
            let Some(ui) = weak.upgrade() else { return };
            let s = st.borrow();
            let Some(dev) = selected_device(&ui, &s) else {
                ui.set_status_text("Select an output device first.".into());
                return;
            };
            let profile = s.current.clone();
            let tone = read_tone(&ui);
            let tx = s.tx.clone();
            drop(s);
            run_op(&ui, &tx, move || sys::apply(&profile, &dev, &tone).err());
        });
    }
    {
        let st = state.clone();
        let weak = ui.as_weak();
        // The master switch. On Windows, enabling registers the APO on the device
        // (one UAC prompt + audio reload) and disabling unregisters it; on Linux
        // it inserts/removes the PipeWire filter. Either way `sys` hides the
        // platform detail. The instant, no-admin bypass is the Correction (A/B)
        // switch, so this one is only touched to turn ADtune on/off for a device.
        ui.on_set_enabled(move |enable: bool| {
            let Some(ui) = weak.upgrade() else { return };
            let s = st.borrow();
            let tx = s.tx.clone();
            let Some(dev) = selected_device(&ui, &s) else {
                ui.set_status_text("Select an output device first.".into());
                ui.set_is_active(false);
                return;
            };
            let profile = s.current.clone();
            let tone = read_tone(&ui);
            drop(s);
            if enable {
                run_op(&ui, &tx, move || sys::enable(&profile, &dev, &tone).err());
            } else {
                run_op(&ui, &tx, move || sys::disable_device(&dev).err());
            }
        });
    }
    // Correction A/B switch: the instant, no-admin bypass. Flips only the tone's
    // `wet` mix (0 = pass-through, >0 = correcting) on the already-active target,
    // preferring an in-place live update and falling back to a full apply.
    {
        let st = state.clone();
        let weak = ui.as_weak();
        ui.on_set_correction(move |on: bool| {
            let Some(ui) = weak.upgrade() else { return };
            let s = st.borrow();
            let tx = s.tx.clone();
            let Some(profile) = sys::active_profile() else {
                return;
            };
            let Some(dev) = sys::saved_target_name()
                .and_then(|name| s.outputs.iter().find(|d| d.name == name).cloned())
            else {
                return;
            };
            let base = sys::active_tone();
            let wet = if on {
                (ui.get_wet() as f64 / 100.0).max(0.01)
            } else {
                0.0
            };
            let tone = ToneSettings { wet, ..base };
            drop(s);
            run_op(&ui, &tx, move || {
                match sys::update_live(&profile, &dev, &tone) {
                    Ok(true) => None,
                    Ok(false) => sys::apply(&profile, &dev, &tone).err(),
                    Err(e) => Some(e),
                }
            });
        });
    }
    // Refresh button: re-query devices + status with an empty op (snapshot only).
    {
        let st = state.clone();
        let weak = ui.as_weak();
        ui.on_refresh_devices(move || {
            let Some(ui) = weak.upgrade() else { return };
            let tx = st.borrow().tx.clone();
            run_op(&ui, &tx, || None);
        });
    }

    // Poll (every 40 ms) both worker channels on the UI thread: backend `Outcome`s
    // update status/devices, file-dialog `FileMsg`s complete import/export. This
    // is the only place worker results touch the UI — nothing else is cross-thread.
    let drain = Timer::default();
    {
        let st = state.clone();
        let rm = results_model.clone();
        let weak = ui.as_weak();
        drain.start(TimerMode::Repeated, Duration::from_millis(40), move || {
            let Some(ui) = weak.upgrade() else { return };
            while let Ok(o) = rx.try_recv() {
                apply_outcome(&ui, &st, o);
            }
            while let Ok(m) = file_rx.try_recv() {
                match m {
                    FileMsg::Imported(p) => {
                        let msg = {
                            let mut s = st.borrow_mut();
                            // Imports land straight in the library: with the
                            // ✔-commit model the app never holds a floating
                            // unsaved profile.
                            let msg = match store::save_profile(&sys::user_profiles_dir(), &p) {
                                Ok(path) => {
                                    s.loaded_path = Some(path);
                                    s.user_profiles = load_library();
                                    format!("Imported “{}” into your library.", p.name)
                                }
                                Err(e) => {
                                    s.loaded_path = None;
                                    format!("⚠ Imported, but could not save to the library: {e}")
                                }
                            };
                            s.current = p;
                            s.selected = -1;
                            s.editing = true; // imported profiles are editable custom copies
                            s.edit_mode = false; // end any edit session
                            s.edit_baseline = None;
                            s.dirty = false; // freshly loaded (and already persisted)
                            msg
                        };
                        ui.set_edit_mode(false);
                        let s = st.borrow();
                        refresh_edit(&ui, &s);
                        refresh_results(&ui, &s, &rm);
                        ui.set_status_text(msg.into());
                    }
                    FileMsg::Status(msg) => ui.set_status_text(msg.into()),
                    FileMsg::Failed(e) => open_info(&ui, "Import failed", &e),
                }
            }
        });
    }

    // initial population
    refresh_results(&ui, &state.borrow(), &results_model);
    refresh_edit(&ui, &state.borrow());
    // load devices + status from the backend
    {
        let tx = state.borrow().tx.clone();
        run_op(&ui, &tx, || None);
    }

    // optional headless self-test: render one frame and exit
    if std::env::var("ADTUNE_SELFTEST").is_ok() {
        let ok = results_model.row_count() > 0
            && !ui.get_curve_commands().is_empty()
            && ui.get_x_labels().row_count() == 10;
        println!(
            "SELFTEST results={} curve_len={} xlabels={} -> {}",
            results_model.row_count(),
            ui.get_curve_commands().len(),
            ui.get_x_labels().row_count(),
            if ok { "PASS" } else { "FAIL" }
        );
        return Ok(());
    }

    // Headless end-to-end backend check: let the initial query populate, then
    // report what the UI received (read-only; applies nothing) and quit.
    if std::env::var("ADTUNE_BACKENDTEST").is_ok() {
        let weak = ui.as_weak();
        let quit_timer = Timer::default();
        quit_timer.start(
            TimerMode::SingleShot,
            Duration::from_millis(1600),
            move || {
                if let Some(ui) = weak.upgrade() {
                    println!(
                        "BACKENDTEST devices={} selected={} active={} status={:?}",
                        ui.get_output_names().row_count(),
                        ui.get_selected_output(),
                        ui.get_is_active(),
                        ui.get_status_text()
                    );
                }
                let _ = slint::quit_event_loop();
            },
        );
        ui.run()?;
        return Ok(());
    }

    match ui.run() {
        Ok(()) => Ok(()),
        Err(e) => {
            // Release the failed GPU window before relaunching with software.
            drop(ui);
            handle_backend_error(e)
        }
    }
}
