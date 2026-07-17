//! The Windows COM implementation of ADtune's system-effects APO.
//!
//! Windows creates system-effects APOs through COM aggregation. `windows-rs`
//! provides an excellent implementation for ordinary COM objects, but it does
//! not own the audio engine's controlling unknown in this case. This module
//! therefore keeps the small amount of aggregation glue that is necessary,
//! while dispatching each COM vtable directly to one Rust-owned [`ApoState`].
//! There are no proxy objects, borrowed COM interfaces, or `transmute` calls.

#![allow(non_snake_case)]

use crate::processor::{Dsp, Processor, MAX_CHANNELS};
use arc_swap::ArcSwap;
use core::ffi::c_void;
use std::cell::UnsafeCell;
use std::sync::atomic::{fence, AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use windows::core::PCWSTR;
use windows::core::{implement, IUnknown, IUnknown_Vtbl, Interface, Result, GUID, HRESULT};
use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, E_FAIL, E_NOINTERFACE, E_POINTER, HANDLE, S_FALSE, S_OK,
};
use windows::Win32::Media::Audio::Apo::{
    IAudioMediaType, IAudioProcessingObject, IAudioProcessingObjectConfiguration,
    IAudioProcessingObjectConfiguration_Vtbl, IAudioProcessingObjectNotifications,
    IAudioProcessingObjectNotifications_Vtbl, IAudioProcessingObjectRT,
    IAudioProcessingObjectRT_Vtbl, IAudioProcessingObject_Vtbl, IAudioSystemEffects,
    IAudioSystemEffects2, IAudioSystemEffects2_Vtbl, IAudioSystemEffects3,
    IAudioSystemEffects3_Vtbl, IAudioSystemEffects_Vtbl, APOERR_FORMAT_NOT_SUPPORTED,
    APOERR_INVALID_CONNECTION_FORMAT, APOERR_NUM_CONNECTIONS_INVALID, APO_CONNECTION_DESCRIPTOR,
    APO_CONNECTION_PROPERTY, APO_FLAG, APO_FLAG_BITSPERSAMPLE_MUST_MATCH,
    APO_FLAG_FRAMESPERSECOND_MUST_MATCH, APO_FLAG_INPLACE, APO_NOTIFICATION,
    APO_NOTIFICATION_DESCRIPTOR, APO_REG_PROPERTIES, AUDIO_SYSTEMEFFECT, AUDIO_SYSTEMEFFECT_STATE,
    BUFFER_SILENT, BUFFER_VALID,
};
use windows::Win32::Media::Audio::{WAVEFORMATEX, WAVEFORMATEXTENSIBLE};
use windows::Win32::System::Com::{CoTaskMemAlloc, IClassFactory, IClassFactory_Impl};
use windows::Win32::System::Diagnostics::Debug::OutputDebugStringW;

/// ADtune APO class id. (Stable, generated once for ADtune.)
pub const CLSID_ADTUNE_APO: GUID = GUID::from_u128(0x7f4a1e02_9c3b_4d5a_8e21_ad7c0c0ffee1);

/// Advertise only the classic APO interface surface.
///
/// Observed on Windows 11 in testing: when an endpoint-registered APO answers
/// QI for IAudioSystemEffects2/3 + IAudioProcessingObjectNotifications, the
/// engine runs its modern controllable-effects path, which repeatedly probes
/// the APO (CreateInstance → Initialize → IsInputFormatSupported, all
/// succeeding) and then abandons the endpoint graph without ever calling
/// LockForProcess — leaving the endpoint with no audio at all. Advertising the
/// classic profile instead (IAudioProcessingObject/RT/Configuration +
/// IAudioSystemEffects only, the same surface the in-box Windows system-effect
/// APOs expose) makes the engine take the legacy path that actually streams.
/// The registry registration (NumAPOInterfaces/APOInterfaceN) must stay in sync
/// with this flag — see `register.rs` and `adtune.iss`. Only flip it back on
/// with a real Windows test rig to re-validate the modern path end to end.
const ADVERTISE_MODERN_INTERFACES: bool = false;

/// `IAudioProcessingObjectPreferredFormatSupport` (audioengineextensionapo.h,
/// Windows 11 23H2+). Not yet in windows-rs metadata, hence the local IID and
/// vtable. The engine QIs for it on every APO instantiation (observed live).
const IID_PREFERRED_FORMAT_SUPPORT: GUID = GUID::from_u128(0x51cbd3c4_f1f3_4d2f_a0e1_7e9c4dd0feb3);

const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xfffe;
const WAVEFORMATEXTENSIBLE_EXTRA_BYTES: u16 = 22;
const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: GUID =
    GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

static ACTIVE_OBJECTS: AtomicU32 = AtomicU32::new(0);
static SERVER_LOCKS: AtomicU32 = AtomicU32::new(0);

/// `[ADtune APO <host-exe>:<pid>]` — the host process matters: endpoint
/// validation (AudioEndpointBuilder/Audiosrv) and streaming (audiodg.exe)
/// create separate APO instances, and telling them apart in a trace is the
/// only way to see which stage fails.
fn trace_prefix() -> &'static str {
    static PREFIX: OnceLock<String> = OnceLock::new();
    PREFIX.get_or_init(|| {
        let host = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "?".into());
        format!("[ADtune APO {host}:{}]", std::process::id())
    })
}

/// Emits diagnostics without doing file I/O from inside `audiodg.exe`.
///
/// The real-time callback never calls this function. View these messages with
/// Sysinternals DebugView, with global Win32 capture enabled.
fn trace(message: &str) {
    let mut wide: Vec<u16> = format!("{} {message}\r\n", trace_prefix())
        .encode_utf16()
        .collect();
    wide.push(0);
    unsafe { OutputDebugStringW(PCWSTR(wide.as_ptr())) };
}

fn config_path() -> std::path::PathBuf {
    let base = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into());
    std::path::PathBuf::from(base)
        .join("ADtune")
        .join("config.txt")
}

/// Read the live correction, refusing to buffer more than 1 MiB. A real config
/// is a few KB (≤32 bands); the cap keeps a `Users`-writable `config.txt` from
/// feeding an unbounded read into `audiodg.exe` (a truncated read just fails to
/// parse cleanly and falls back toward passthrough).
fn read_config() -> String {
    use std::io::Read;
    let Ok(file) = std::fs::File::open(config_path()) else {
        return String::new();
    };
    let mut buf = String::new();
    let _ = file.take(1 << 20).read_to_string(&mut buf);
    buf
}

/// Per-instance real-time state. Windows serializes `APOProcess` against the
/// non-real-time configuration calls, so this cell is only ever mutated by the
/// processing thread while the APO is locked.
struct RtCell(UnsafeCell<Processor>);

// SAFETY: see the type-level contract above. The audio engine owns the process
// callback lifetime and never invokes it concurrently for this APO instance.
unsafe impl Sync for RtCell {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AudioFormat {
    channels: u16,
    sample_rate: u32,
}

/// All processing state, shared by the manually laid out COM interfaces below.
struct ApoState {
    dsp: Arc<ArcSwap<Dsp>>,
    rt: RtCell,
    channels: AtomicU32,
    watcher_stop: Arc<AtomicBool>,
    watcher: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl ApoState {
    fn new() -> Self {
        Self {
            dsp: Arc::new(ArcSwap::from_pointee(Dsp::flat())),
            rt: RtCell(UnsafeCell::new(Processor::new(2))),
            channels: AtomicU32::new(2),
            watcher_stop: Arc::new(AtomicBool::new(false)),
            watcher: Mutex::new(None),
        }
    }

    fn watcher_guard(&self) -> MutexGuard<'_, Option<std::thread::JoinHandle<()>>> {
        self.watcher
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn rebuild(&self, sample_rate: u32) {
        self.dsp.store(Arc::new(Dsp::from_config(
            &read_config(),
            sample_rate as f64,
        )));
    }

    fn stop_watcher(&self) {
        self.watcher_stop.store(true, Ordering::Release);
        if let Some(handle) = self.watcher_guard().take() {
            let _ = handle.join();
        }
    }

    /// Configuration polling belongs off the real-time audio callback. A
    /// failed thread creation merely disables live reload; it must never abort
    /// or destabilize the audio graph.
    fn start_watcher(&self, sample_rate: u32) {
        self.watcher_stop.store(false, Ordering::Release);
        let dsp = Arc::clone(&self.dsp);
        let stop = Arc::clone(&self.watcher_stop);
        let spawn = std::thread::Builder::new()
            .name("adtune-config".into())
            .spawn(move || {
                // Equalizer-APO-style: block on a directory change
                // notification so a config write is applied within
                // milliseconds. The bounded wait keeps the stop flag
                // responsive; a failed setup falls back to mtime polling.
                // Either way the mtime comparison is the reload gate (the
                // notification also fires for unrelated files in the dir).
                let notification = change_notification();
                let mut last_modified = modified_config_time();
                while !stop.load(Ordering::Acquire) {
                    match &notification {
                        Some(handle) => wait_for_change(handle, 250),
                        None => std::thread::sleep(std::time::Duration::from_millis(150)),
                    }
                    let now = modified_config_time();
                    if now != last_modified {
                        last_modified = now;
                        dsp.store(Arc::new(Dsp::from_config(
                            &read_config(),
                            sample_rate as f64,
                        )));
                    }
                }
                if let Some(handle) = notification {
                    // SAFETY: handle came from FindFirstChangeNotificationW.
                    unsafe {
                        let _ = windows::Win32::Storage::FileSystem::FindCloseChangeNotification(
                            handle,
                        );
                    }
                }
            });

        match spawn {
            Ok(handle) => *self.watcher_guard() = Some(handle),
            Err(error) => trace(&format!("config watcher unavailable: {error}")),
        }
    }

    fn reset(&self) {
        // SAFETY: Reset is serialized with APOProcess by the audio engine.
        unsafe { (*self.rt.0.get()).reset() };
    }

    fn lock(
        &self,
        input_count: u32,
        input_connections: *const *const APO_CONNECTION_DESCRIPTOR,
        output_count: u32,
        output_connections: *const *const APO_CONNECTION_DESCRIPTOR,
    ) -> Result<()> {
        trace("ApoState::lock: entering");
        if input_count != 1 || output_count != 1 {
            trace("ApoState::lock: invalid input/output connection count");
            return Err(APOERR_NUM_CONNECTIONS_INVALID.into());
        }
        if input_connections.is_null() || output_connections.is_null() {
            trace("ApoState::lock: null connection pointers");
            return Err(E_POINTER.into());
        }

        let (input, output) = unsafe {
            let input = (*input_connections).as_ref().ok_or(E_POINTER)?;
            let output = (*output_connections).as_ref().ok_or(E_POINTER)?;
            (input, output)
        };
        let input_format = format_from_descriptor(input)?;
        let output_format = format_from_descriptor(output)?;
        trace(&format!(
            "ApoState::lock: input_format={:?}, output_format={:?}",
            input_format, output_format
        ));
        if input_format != output_format {
            trace("ApoState::lock: format mismatch");
            return Err(APOERR_INVALID_CONNECTION_FORMAT.into());
        }

        self.stop_watcher();
        self.channels
            .store(input_format.channels as u32, Ordering::Release);
        // SAFETY: LockForProcess completes before the engine may invoke
        // APOProcess for this instance.
        unsafe { *self.rt.0.get() = Processor::new(input_format.channels as usize) };
        self.rebuild(input_format.sample_rate);
        self.start_watcher(input_format.sample_rate);
        trace("ApoState::lock: succeeded");
        Ok(())
    }

    fn unlock(&self) {
        self.stop_watcher();
    }

    fn process(
        &self,
        input_count: u32,
        input_connections: *const *const APO_CONNECTION_PROPERTY,
        output_count: u32,
        output_connections: *mut *mut APO_CONNECTION_PROPERTY,
    ) {
        if input_count != 1
            || output_count != 1
            || input_connections.is_null()
            || output_connections.is_null()
        {
            return;
        }

        unsafe {
            let Some(input) = (*input_connections).as_ref() else {
                return;
            };
            let Some(output) = (*output_connections).as_mut() else {
                return;
            };

            // Preserve the engine's validity status (including silence) and
            // establish the output frame count before touching any data.
            output.u32ValidFrameCount = input.u32ValidFrameCount;
            output.u32BufferFlags = input.u32BufferFlags;

            let channels = self.channels.load(Ordering::Acquire) as usize;
            let frames = input.u32ValidFrameCount as usize;
            let Some(sample_count) = frames.checked_mul(channels) else {
                output.u32ValidFrameCount = 0;
                return;
            };
            if sample_count == 0 || input.pBuffer == 0 || output.pBuffer == 0 {
                return;
            }

            let source = input.pBuffer as *const f32;
            let destination = output.pBuffer as *mut f32;

            if input.u32BufferFlags.0 != BUFFER_VALID.0 {
                if input.u32BufferFlags.0 == BUFFER_SILENT.0 && source != destination {
                    std::ptr::write_bytes(destination, 0, sample_count);
                }
                return;
            }

            if source != destination {
                // The documented in-place path uses the same buffer, but a
                // copy is harmless for a distinct buffer and safe if ranges
                // happen to overlap.
                std::ptr::copy(source, destination, sample_count);
            }

            let samples = std::slice::from_raw_parts_mut(destination, sample_count);
            let dsp = self.dsp.load();
            (*self.rt.0.get()).process(&dsp, samples, channels);
        }
    }
}

impl Drop for ApoState {
    fn drop(&mut self) {
        self.stop_watcher();
    }
}

fn modified_config_time() -> Option<std::time::SystemTime> {
    std::fs::metadata(config_path())
        .and_then(|metadata| metadata.modified())
        .ok()
}

/// A change-notification handle on the config directory — Equalizer APO's
/// live-reload mechanism: signaled the instant any file under
/// `%ProgramData%\ADtune` changes, so edits apply in milliseconds instead of
/// at the next poll. `None` (directory missing, restricted token) falls back
/// to polling.
fn change_notification() -> Option<windows::Win32::Foundation::HANDLE> {
    use windows::Win32::Storage::FileSystem::{
        FindFirstChangeNotificationW, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
        FILE_NOTIFY_CHANGE_SIZE,
    };
    let dir = config_path().parent()?.to_owned();
    let dir = windows::core::HSTRING::from(dir.as_os_str());
    // SAFETY: plain Win32 call over an owned wide string.
    unsafe {
        FindFirstChangeNotificationW(
            &dir,
            false,
            FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_SIZE | FILE_NOTIFY_CHANGE_FILE_NAME,
        )
        .ok()
    }
}

/// Wait (bounded, so the stop flag stays responsive) for the next directory
/// change and re-arm the notification per the FindNextChangeNotification
/// protocol. Timeouts simply return — the caller's mtime check is the gate.
fn wait_for_change(handle: &windows::Win32::Foundation::HANDLE, timeout_ms: u32) {
    use windows::Win32::Foundation::WAIT_OBJECT_0;
    use windows::Win32::Storage::FileSystem::FindNextChangeNotification;
    use windows::Win32::System::Threading::WaitForSingleObject;
    // SAFETY: valid notification handle owned by the watcher thread.
    unsafe {
        if WaitForSingleObject(*handle, timeout_ms) == WAIT_OBJECT_0 {
            let _ = FindNextChangeNotification(*handle);
        }
    }
}

fn format_from_descriptor(descriptor: &APO_CONNECTION_DESCRIPTOR) -> Result<AudioFormat> {
    let media_type = descriptor.pFormat.as_ref().ok_or(E_POINTER)?;
    unsafe { format_from_media_type(media_type) }
}

/// `IAudioMediaType::GetAudioFormat` returns a pointer *owned by the media-type
/// object* (valid while that interface is referenced) — unlike e.g.
/// `IAudioClient::GetMixFormat`, the caller must NOT `CoTaskMemFree` it. Freeing
/// it corrupts the audio engine's heap and takes down audiodg.exe.
unsafe fn format_from_media_type(media_type: &IAudioMediaType) -> Result<AudioFormat> {
    let format = media_type.GetAudioFormat();
    if format.is_null() {
        return Err(APOERR_FORMAT_NOT_SUPPORTED.into());
    }
    format_from_wave_format(&*format)
}

fn format_from_wave_format(format: &WAVEFORMATEX) -> Result<AudioFormat> {
    let channels = format.nChannels;
    if channels == 0 || channels as usize > MAX_CHANNELS || format.nSamplesPerSec == 0 {
        return Err(APOERR_FORMAT_NOT_SUPPORTED.into());
    }
    if format.wBitsPerSample != 32 || format.nBlockAlign != channels * 4 {
        return Err(APOERR_FORMAT_NOT_SUPPORTED.into());
    }

    let is_float = match format.wFormatTag {
        WAVE_FORMAT_IEEE_FLOAT => true,
        WAVE_FORMAT_EXTENSIBLE if format.cbSize >= WAVEFORMATEXTENSIBLE_EXTRA_BYTES => {
            // SAFETY: an extensible WAVEFORMATEX has the documented trailing
            // WAVEFORMATEXTENSIBLE fields when cbSize is at least 22.
            let extensible = unsafe { *(format as *const _ as *const WAVEFORMATEXTENSIBLE) };
            let valid_bits = unsafe { extensible.Samples.wValidBitsPerSample };
            let subformat = extensible.SubFormat;
            (valid_bits == 32 || valid_bits == 0) && subformat == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
        }
        _ => false,
    };
    if !is_float {
        return Err(APOERR_FORMAT_NOT_SUPPORTED.into());
    }

    Ok(AudioFormat {
        channels,
        sample_rate: format.nSamplesPerSec,
    })
}

#[repr(C)]
struct NonDelegatingUnknown {
    vtable: *const IUnknown_Vtbl,
}

#[repr(C)]
struct InterfaceSlot {
    vtable: *const c_void,
    owner: *mut ApoObject,
}

impl InterfaceSlot {
    const fn new(vtable: *const c_void) -> Self {
        Self {
            vtable,
            owner: std::ptr::null_mut(),
        }
    }
}

/// One allocation owns every interface and all DSP state. Interface slots hold
/// only a back-pointer, so a COM reference can never outlive its Rust storage.
#[repr(C)]
struct ApoObject {
    non_delegating_unknown: NonDelegatingUnknown,
    ref_count: AtomicU32,
    outer_unknown: *mut c_void,
    processing: InterfaceSlot,
    configuration: InterfaceSlot,
    realtime: InterfaceSlot,
    effects: InterfaceSlot,
    effects2: InterfaceSlot,
    effects3: InterfaceSlot,
    notifications: InterfaceSlot,
    preferred_format: InterfaceSlot,
    state: ApoState,
}

impl ApoObject {
    fn new(outer_unknown: *mut c_void) -> Self {
        ACTIVE_OBJECTS.fetch_add(1, Ordering::Relaxed);
        Self {
            non_delegating_unknown: NonDelegatingUnknown {
                vtable: &NON_DELEGATING_UNKNOWN_VTBL,
            },
            ref_count: AtomicU32::new(0),
            outer_unknown,
            processing: InterfaceSlot::new(&PROCESSING_VTBL as *const _ as *const c_void),
            configuration: InterfaceSlot::new(&CONFIGURATION_VTBL as *const _ as *const c_void),
            realtime: InterfaceSlot::new(&REALTIME_VTBL as *const _ as *const c_void),
            effects: InterfaceSlot::new(&EFFECTS_VTBL as *const _ as *const c_void),
            effects2: InterfaceSlot::new(&EFFECTS2_VTBL as *const _ as *const c_void),
            effects3: InterfaceSlot::new(&EFFECTS3_VTBL as *const _ as *const c_void),
            notifications: InterfaceSlot::new(&NOTIFICATIONS_VTBL as *const _ as *const c_void),
            preferred_format: InterfaceSlot::new(
                &PREFERRED_FORMAT_VTBL as *const _ as *const c_void,
            ),
            state: ApoState::new(),
        }
    }

    unsafe fn initialize_slots(&mut self) {
        let owner = self as *mut Self;
        self.processing.owner = owner;
        self.configuration.owner = owner;
        self.realtime.owner = owner;
        self.effects.owner = owner;
        self.effects2.owner = owner;
        self.effects3.owner = owner;
        self.notifications.owner = owner;
        self.preferred_format.owner = owner;
    }
}

impl Drop for ApoObject {
    fn drop(&mut self) {
        trace(&format!("ApoObject::drop: pointer={:?}", self as *mut Self));
        // Join the config-watcher thread BEFORE the active-object count can
        // reach zero: once it does, DllCanUnloadNow may report S_OK and COM may
        // unload the DLL, which must never happen while a thread this object
        // spawned can still execute the DLL's code.
        self.state.stop_watcher();
        ACTIVE_OBJECTS.fetch_sub(1, Ordering::Release);
    }
}

unsafe fn object_add_ref(object: *mut ApoObject) -> u32 {
    (*object).ref_count.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe fn object_release(object: *mut ApoObject) -> u32 {
    let previous = (*object).ref_count.fetch_sub(1, Ordering::Release);
    if previous == 1 {
        fence(Ordering::Acquire);
        drop(Box::from_raw(object));
        0
    } else {
        previous - 1
    }
}

unsafe fn owner_from_slot(this: *mut c_void) -> *mut ApoObject {
    (*(this as *mut InterfaceSlot)).owner
}

unsafe fn outer_query_interface(
    outer: *mut c_void,
    riid: *const GUID,
    object: *mut *mut c_void,
) -> HRESULT {
    let vtable = *(outer as *const *const IUnknown_Vtbl);
    ((*vtable).QueryInterface)(outer, riid, object)
}

unsafe fn outer_add_ref(outer: *mut c_void) -> u32 {
    let vtable = *(outer as *const *const IUnknown_Vtbl);
    ((*vtable).AddRef)(outer)
}

unsafe fn outer_release(outer: *mut c_void) -> u32 {
    let vtable = *(outer as *const *const IUnknown_Vtbl);
    ((*vtable).Release)(outer)
}

unsafe fn return_interface(
    object: *mut ApoObject,
    slot: *mut InterfaceSlot,
    output: *mut *mut c_void,
) -> HRESULT {
    if !(*object).outer_unknown.is_null() {
        outer_add_ref((*object).outer_unknown);
    } else {
        object_add_ref(object);
    }
    *output = slot.cast();
    S_OK
}

unsafe extern "system" fn non_delegating_query_interface(
    this: *mut c_void,
    riid: *const GUID,
    output: *mut *mut c_void,
) -> HRESULT {
    if riid.is_null() || output.is_null() {
        return E_POINTER;
    }
    *output = std::ptr::null_mut();
    let object = this as *mut ApoObject;
    let guid = unsafe { *riid };
    match guid {
        guid if guid == IUnknown::IID => {
            trace("QI ok: IUnknown");
            *output = object.cast();
            object_add_ref(object);
            S_OK
        }
        guid if guid == IAudioProcessingObject::IID => {
            trace("QI ok: IAudioProcessingObject");
            return_interface(object, &mut (*object).processing, output)
        }
        guid if guid == IAudioProcessingObjectConfiguration::IID => {
            trace("QI ok: IAudioProcessingObjectConfiguration");
            return_interface(object, &mut (*object).configuration, output)
        }
        guid if guid == IAudioProcessingObjectRT::IID => {
            trace("QI ok: IAudioProcessingObjectRT");
            return_interface(object, &mut (*object).realtime, output)
        }
        guid if guid == IAudioSystemEffects::IID => {
            trace("QI ok: IAudioSystemEffects");
            return_interface(object, &mut (*object).effects, output)
        }
        guid if ADVERTISE_MODERN_INTERFACES && guid == IAudioSystemEffects2::IID => {
            trace("QI ok: IAudioSystemEffects2");
            return_interface(object, &mut (*object).effects2, output)
        }
        guid if ADVERTISE_MODERN_INTERFACES && guid == IAudioSystemEffects3::IID => {
            trace("QI ok: IAudioSystemEffects3");
            return_interface(object, &mut (*object).effects3, output)
        }
        guid if ADVERTISE_MODERN_INTERFACES && guid == IAudioProcessingObjectNotifications::IID => {
            trace("QI ok: IAudioProcessingObjectNotifications");
            return_interface(object, &mut (*object).notifications, output)
        }
        guid if ADVERTISE_MODERN_INTERFACES && guid == IID_PREFERRED_FORMAT_SUPPORT => {
            trace("QI ok: IAudioProcessingObjectPreferredFormatSupport");
            return_interface(object, &mut (*object).preferred_format, output)
        }
        _ => {
            trace(&format!("QI unsupported: {guid:?}"));
            E_NOINTERFACE
        }
    }
}

unsafe extern "system" fn non_delegating_add_ref(this: *mut c_void) -> u32 {
    object_add_ref(this as *mut ApoObject)
}

unsafe extern "system" fn non_delegating_release(this: *mut c_void) -> u32 {
    object_release(this as *mut ApoObject)
}

unsafe extern "system" fn delegating_query_interface(
    this: *mut c_void,
    riid: *const GUID,
    output: *mut *mut c_void,
) -> HRESULT {
    let object = owner_from_slot(this);
    if (*object).outer_unknown.is_null() {
        non_delegating_query_interface(object.cast(), riid, output)
    } else {
        outer_query_interface((*object).outer_unknown, riid, output)
    }
}

unsafe extern "system" fn delegating_add_ref(this: *mut c_void) -> u32 {
    let object = owner_from_slot(this);
    if (*object).outer_unknown.is_null() {
        object_add_ref(object)
    } else {
        outer_add_ref((*object).outer_unknown)
    }
}

unsafe extern "system" fn delegating_release(this: *mut c_void) -> u32 {
    let object = owner_from_slot(this);
    if (*object).outer_unknown.is_null() {
        object_release(object)
    } else {
        outer_release((*object).outer_unknown)
    }
}

fn hresult(result: Result<()>) -> HRESULT {
    match result {
        Ok(()) => S_OK,
        Err(error) => error.code(),
    }
}

/// Contains a Rust panic at the COM boundary. A panic unwinding out of an
/// `extern "system"` fn aborts the process — here that process is audiodg.exe,
/// i.e. the whole Windows audio engine. Catching it fails only the single COM
/// call and keeps the engine alive. Zero-cost unless a panic actually fires,
/// so wrapping the `APOProcess` hot path is free too.
fn com_guard(f: impl FnOnce() -> HRESULT) -> HRESULT {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| {
        trace("panic contained at the COM boundary");
        E_FAIL
    })
}

unsafe extern "system" fn apo_reset(this: *mut c_void) -> HRESULT {
    trace("apo_reset: entering");
    (*owner_from_slot(this)).state.reset();
    S_OK
}

unsafe extern "system" fn apo_get_latency(_this: *mut c_void, latency: *mut i64) -> HRESULT {
    trace("apo_get_latency: entering");
    if latency.is_null() {
        return E_POINTER;
    }
    *latency = 0;
    S_OK
}

unsafe extern "system" fn apo_get_registration_properties(
    _this: *mut c_void,
    properties: *mut *mut APO_REG_PROPERTIES,
) -> HRESULT {
    trace("apo_get_registration_properties: entering");
    if properties.is_null() {
        return E_POINTER;
    }
    *properties = std::ptr::null_mut();
    com_guard(|| apo_registration_properties_body(properties))
}

unsafe fn apo_registration_properties_body(properties: *mut *mut APO_REG_PROPERTIES) -> HRESULT {
    // APO_REG_PROPERTIES already embeds one interface GUID; allocate space for
    // the extras only when the modern surface is advertised.
    let extra_interfaces = if ADVERTISE_MODERN_INTERFACES { 2 } else { 0 };
    let total_size =
        std::mem::size_of::<APO_REG_PROPERTIES>() + extra_interfaces * std::mem::size_of::<GUID>();
    let allocation = CoTaskMemAlloc(total_size) as *mut APO_REG_PROPERTIES;
    if allocation.is_null() {
        trace("  CoTaskMemAlloc failed");
        return E_FAIL;
    }
    std::ptr::write_bytes(allocation.cast::<u8>(), 0, total_size);
    let registration = &mut *allocation;
    registration.clsid = CLSID_ADTUNE_APO;
    // 0xD — matches the flags every working sysfx APO registers with (no
    // SAMPLESPERFRAME_MUST_MATCH); must agree with register.rs / adtune.iss.
    registration.Flags = APO_FLAG(
        APO_FLAG_INPLACE.0
            | APO_FLAG_FRAMESPERSECOND_MUST_MATCH.0
            | APO_FLAG_BITSPERSAMPLE_MUST_MATCH.0,
    );
    write_wstr(&mut registration.szFriendlyName, "ADtune APO");
    write_wstr(
        &mut registration.szCopyrightInfo,
        "Copyright (c) 2026 Antonio DEDEJ",
    );
    registration.u32MajorVersion = 1;
    registration.u32MinorVersion = 0;
    registration.u32MinInputConnections = 1;
    registration.u32MaxInputConnections = 1;
    registration.u32MinOutputConnections = 1;
    registration.u32MaxOutputConnections = 1;
    registration.u32MaxInstances = u32::MAX;
    registration.u32NumAPOInterfaces = 1 + extra_interfaces as u32;

    // The functional interface the engine drives the APO through — the same
    // declaration as APOInterface0 in the registry (see register.rs).
    let guid_ptr = registration.iidAPOInterfaceList.as_mut_ptr();
    *guid_ptr = IAudioProcessingObject::IID;
    if ADVERTISE_MODERN_INTERFACES {
        *guid_ptr.add(1) = IAudioSystemEffects2::IID;
        *guid_ptr.add(2) = IAudioSystemEffects3::IID;
    }

    *properties = allocation;
    trace("  apo_get_registration_properties succeeded");
    S_OK
}

unsafe extern "system" fn apo_initialize(
    _this: *mut c_void,
    data_size: u32,
    data: *const u8,
) -> HRESULT {
    if data_size != 0 && data.is_null() {
        trace("apo_initialize: non-zero size with null data");
        return E_POINTER;
    }
    // Dump the APOInitSystemEffects* blob: it carries the processing-mode GUID
    // and (v2/v3) the InitializeForDiscoveryOnly flag, which distinguish a
    // real streaming instantiation from an effects-enumeration probe.
    if !data.is_null() && data_size >= 20 {
        let bytes = std::slice::from_raw_parts(data, data_size.min(160) as usize);
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let clsid: GUID = std::ptr::read_unaligned(data.add(4) as *const GUID);
        trace(&format!(
            "apo_initialize: data_size={data_size}, clsid={clsid:?}, blob={hex}"
        ));
    } else {
        trace(&format!("apo_initialize: data_size={data_size}"));
    }
    S_OK
}

unsafe extern "system" fn apo_is_input_format_supported(
    _this: *mut c_void,
    opposite: *mut c_void,
    requested: *mut c_void,
    supported: *mut *mut c_void,
) -> HRESULT {
    com_guard(|| negotiate_format("IsInputFormatSupported", opposite, requested, supported))
}

unsafe extern "system" fn apo_is_output_format_supported(
    _this: *mut c_void,
    opposite: *mut c_void,
    requested: *mut c_void,
    supported: *mut *mut c_void,
) -> HRESULT {
    com_guard(|| negotiate_format("IsOutputFormatSupported", opposite, requested, supported))
}

/// Full WAVEFORMATEX details of a media type, for the bring-up traces.
unsafe fn describe_media_type(media: Option<&IAudioMediaType>) -> String {
    let Some(media) = media else {
        return "null".into();
    };
    let format = media.GetAudioFormat();
    if format.is_null() {
        return "no-waveformat".into();
    }
    // WAVEFORMATEX is packed — copy fields to locals before formatting
    // (format! takes references, which must be aligned).
    let f: WAVEFORMATEX = std::ptr::read_unaligned(format);
    let (tag, ch, rate, bits, align, cb) = (
        f.wFormatTag,
        f.nChannels,
        f.nSamplesPerSec,
        f.wBitsPerSample,
        f.nBlockAlign,
        f.cbSize,
    );
    let mut s = format!("tag=0x{tag:04x} ch={ch} rate={rate} bits={bits} align={align} cb={cb}");
    if tag == WAVE_FORMAT_EXTENSIBLE && cb >= WAVEFORMATEXTENSIBLE_EXTRA_BYTES {
        let ext: WAVEFORMATEXTENSIBLE =
            std::ptr::read_unaligned(format as *const _ as *const WAVEFORMATEXTENSIBLE);
        let valid_bits = unsafe { ext.Samples.wValidBitsPerSample };
        let sub = ext.SubFormat;
        s.push_str(&format!(" valid_bits={valid_bits} sub={sub:?}"));
    }
    s
}

/// Shared body of `IsInputFormatSupported`/`IsOutputFormatSupported`.
///
/// The APO runs on 32-bit-float PCM with identical formats on both pins. A
/// qualifying request is echoed back (S_OK). A request that doesn't qualify is
/// answered with the opposite pin's format as the suggestion (S_FALSE) when
/// that one qualifies, per the APO negotiation contract; otherwise the format
/// is rejected outright.
unsafe fn negotiate_format(
    direction: &str,
    opposite: *mut c_void,
    requested: *mut c_void,
    supported: *mut *mut c_void,
) -> HRESULT {
    if !supported.is_null() {
        *supported = std::ptr::null_mut();
    }
    let Some(requested_media) = IAudioMediaType::from_raw_borrowed(&requested) else {
        return E_POINTER;
    };
    let opposite_media = IAudioMediaType::from_raw_borrowed(&opposite);
    trace(&format!(
        "{direction}: requested [{}], opposite [{}], out={}",
        describe_media_type(Some(requested_media)),
        describe_media_type(opposite_media),
        if supported.is_null() { "null" } else { "set" },
    ));
    let requested_format = format_from_media_type(requested_media).ok();
    let opposite_format = opposite_media.and_then(|m| format_from_media_type(m).ok());

    let matches_opposite = opposite.is_null() || opposite_format == requested_format;
    if requested_format.is_some() && matches_opposite {
        trace(&format!("{direction}: accepted {:?}", requested_format));
        if !supported.is_null() {
            *supported = requested_media.clone().into_raw();
        }
        return S_OK;
    }
    if let (Some(media), Some(format)) = (opposite_media, opposite_format) {
        trace(&format!(
            "{direction}: rejected {:?}, suggesting opposite {:?}",
            requested_format, format
        ));
        if !supported.is_null() {
            *supported = media.clone().into_raw();
            return S_FALSE;
        }
    }
    trace(&format!(
        "{direction}: rejected {:?}, no suggestion",
        requested_format
    ));
    APOERR_FORMAT_NOT_SUPPORTED
}

unsafe extern "system" fn apo_get_input_channel_count(
    this: *mut c_void,
    channel_count: *mut u32,
) -> HRESULT {
    trace("apo_get_input_channel_count: entering");
    if channel_count.is_null() {
        return E_POINTER;
    }
    let count = (*owner_from_slot(this))
        .state
        .channels
        .load(Ordering::Acquire);
    *channel_count = count;
    trace(&format!(
        "  apo_get_input_channel_count returning count={}",
        count
    ));
    S_OK
}

unsafe extern "system" fn apo_lock_for_process(
    this: *mut c_void,
    input_count: u32,
    input_connections: *const *const APO_CONNECTION_DESCRIPTOR,
    output_count: u32,
    output_connections: *const *const APO_CONNECTION_DESCRIPTOR,
) -> HRESULT {
    trace(&format!(
        "apo_lock_for_process: entering, input_count={}, output_count={}",
        input_count, output_count
    ));
    let hr = com_guard(|| {
        hresult((*owner_from_slot(this)).state.lock(
            input_count,
            input_connections,
            output_count,
            output_connections,
        ))
    });
    trace(&format!("apo_lock_for_process: returning hr={}", hr.0));
    hr
}

unsafe extern "system" fn apo_unlock_for_process(this: *mut c_void) -> HRESULT {
    trace("apo_unlock_for_process: entering");
    com_guard(|| {
        (*owner_from_slot(this)).state.unlock();
        S_OK
    })
}

unsafe extern "system" fn apo_process(
    this: *mut c_void,
    input_count: u32,
    input_connections: *const *const APO_CONNECTION_PROPERTY,
    output_count: u32,
    output_connections: *mut *mut APO_CONNECTION_PROPERTY,
) {
    // A panic escaping this `extern "system"` fn would abort audiodg.exe;
    // catch_unwind costs nothing unless a panic actually fires. The one-time
    // bring-up trace lives INSIDE the guard: it allocates and calls
    // OutputDebugString, so a panic there (e.g. OOM) must be contained here on
    // the real-time thread rather than taking down the whole audio engine. The
    // `swap` keeps every later call fully trace-free and allocation-free.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        static LOGGED_PROCESS: AtomicBool = AtomicBool::new(false);
        if !LOGGED_PROCESS.swap(true, Ordering::Relaxed) {
            trace(&format!(
                "apo_process: first call! input_count={input_count}, output_count={output_count}"
            ));
        }
        (*owner_from_slot(this)).state.process(
            input_count,
            input_connections,
            output_count,
            output_connections,
        );
    }));
}

// The frame-calc callbacks are 1:1 passthroughs on the real-time interface;
// they do no work and are deliberately trace-free to keep the RT path clean.
unsafe extern "system" fn apo_calc_input_frames(_this: *mut c_void, output_frames: u32) -> u32 {
    output_frames
}

unsafe extern "system" fn apo_calc_output_frames(_this: *mut c_void, input_frames: u32) -> u32 {
    input_frames
}

static NON_DELEGATING_UNKNOWN_VTBL: IUnknown_Vtbl = IUnknown_Vtbl {
    QueryInterface: non_delegating_query_interface,
    AddRef: non_delegating_add_ref,
    Release: non_delegating_release,
};

static PROCESSING_VTBL: IAudioProcessingObject_Vtbl = IAudioProcessingObject_Vtbl {
    base__: IUnknown_Vtbl {
        QueryInterface: delegating_query_interface,
        AddRef: delegating_add_ref,
        Release: delegating_release,
    },
    Reset: apo_reset,
    GetLatency: apo_get_latency,
    GetRegistrationProperties: apo_get_registration_properties,
    Initialize: apo_initialize,
    IsInputFormatSupported: apo_is_input_format_supported,
    IsOutputFormatSupported: apo_is_output_format_supported,
    GetInputChannelCount: apo_get_input_channel_count,
};

static CONFIGURATION_VTBL: IAudioProcessingObjectConfiguration_Vtbl =
    IAudioProcessingObjectConfiguration_Vtbl {
        base__: IUnknown_Vtbl {
            QueryInterface: delegating_query_interface,
            AddRef: delegating_add_ref,
            Release: delegating_release,
        },
        LockForProcess: apo_lock_for_process,
        UnlockForProcess: apo_unlock_for_process,
    };

static REALTIME_VTBL: IAudioProcessingObjectRT_Vtbl = IAudioProcessingObjectRT_Vtbl {
    base__: IUnknown_Vtbl {
        QueryInterface: delegating_query_interface,
        AddRef: delegating_add_ref,
        Release: delegating_release,
    },
    APOProcess: apo_process,
    CalcInputFrames: apo_calc_input_frames,
    CalcOutputFrames: apo_calc_output_frames,
};

static EFFECTS_VTBL: IAudioSystemEffects_Vtbl = IAudioSystemEffects_Vtbl {
    base__: IUnknown_Vtbl {
        QueryInterface: delegating_query_interface,
        AddRef: delegating_add_ref,
        Release: delegating_release,
    },
};

static EFFECTS2_VTBL: IAudioSystemEffects2_Vtbl = IAudioSystemEffects2_Vtbl {
    base__: IAudioSystemEffects_Vtbl {
        base__: IUnknown_Vtbl {
            QueryInterface: delegating_query_interface,
            AddRef: delegating_add_ref,
            Release: delegating_release,
        },
    },
    GetEffectsList: apo_get_effects_list,
};

static EFFECTS3_VTBL: IAudioSystemEffects3_Vtbl = IAudioSystemEffects3_Vtbl {
    base__: IAudioSystemEffects2_Vtbl {
        base__: IAudioSystemEffects_Vtbl {
            base__: IUnknown_Vtbl {
                QueryInterface: delegating_query_interface,
                AddRef: delegating_add_ref,
                Release: delegating_release,
            },
        },
        GetEffectsList: apo_get_effects_list,
    },
    GetControllableSystemEffectsList: apo_get_controllable_system_effects_list,
    SetAudioSystemEffectState: apo_set_audio_system_effect_state,
};

unsafe extern "system" fn apo_get_effects_list(
    _this: *mut c_void,
    effects_ids: *mut *mut GUID,
    effects_count: *mut u32,
    _event: HANDLE,
) -> HRESULT {
    trace("apo_get_effects_list: entering");
    if effects_count.is_null() || effects_ids.is_null() {
        return E_POINTER;
    }
    com_guard(|| apo_effects_list_body(effects_ids, effects_count))
}

unsafe fn apo_effects_list_body(effects_ids: *mut *mut GUID, effects_count: *mut u32) -> HRESULT {
    let allocation = CoTaskMemAlloc(std::mem::size_of::<GUID>()) as *mut GUID;
    if allocation.is_null() {
        trace("  CoTaskMemAlloc failed in GetEffectsList");
        return E_FAIL;
    }
    *allocation = GUID::from_u128(0x6f64adc3_8211_11e2_8c70_2c27d7f001fa); // AUDIO_EFFECT_TYPE_EQUALIZER
    *effects_ids = allocation;
    *effects_count = 1;
    trace("apo_get_effects_list: returning 1 effect");
    S_OK
}

unsafe extern "system" fn apo_get_controllable_system_effects_list(
    _this: *mut c_void,
    effects: *mut *mut AUDIO_SYSTEMEFFECT,
    num_effects: *mut u32,
    _event: HANDLE,
) -> HRESULT {
    trace("apo_get_controllable_system_effects_list: entering");
    if num_effects.is_null() || effects.is_null() {
        return E_POINTER;
    }
    com_guard(|| apo_controllable_effects_body(effects, num_effects))
}

unsafe fn apo_controllable_effects_body(
    effects: *mut *mut AUDIO_SYSTEMEFFECT,
    num_effects: *mut u32,
) -> HRESULT {
    let allocation =
        CoTaskMemAlloc(std::mem::size_of::<AUDIO_SYSTEMEFFECT>()) as *mut AUDIO_SYSTEMEFFECT;
    if allocation.is_null() {
        trace("  CoTaskMemAlloc failed in GetControllableSystemEffectsList");
        return E_FAIL;
    }
    let effect = &mut *allocation;
    effect.id = GUID::from_u128(0x6f64adc3_8211_11e2_8c70_2c27d7f001fa); // AUDIO_EFFECT_TYPE_EQUALIZER
    effect.canSetState = windows::Win32::Foundation::TRUE;
    effect.state = windows::Win32::Media::Audio::Apo::AUDIO_SYSTEMEFFECT_STATE_ON;
    *effects = allocation;
    *num_effects = 1;
    trace("apo_get_controllable_system_effects_list: returning 1 effect");
    S_OK
}

unsafe extern "system" fn apo_set_audio_system_effect_state(
    _this: *mut c_void,
    _effect_id: GUID,
    _state: AUDIO_SYSTEMEFFECT_STATE,
) -> HRESULT {
    trace("apo_set_audio_system_effect_state: entering");
    S_OK
}

static NOTIFICATIONS_VTBL: IAudioProcessingObjectNotifications_Vtbl =
    IAudioProcessingObjectNotifications_Vtbl {
        base__: IUnknown_Vtbl {
            QueryInterface: delegating_query_interface,
            AddRef: delegating_add_ref,
            Release: delegating_release,
        },
        GetApoNotificationRegistrationInfo: apo_get_apo_notification_registration_info,
        HandleNotification: apo_handle_notification,
    };

/// `IAudioProcessingObjectPreferredFormatSupport` (audioengineextensionapo.h,
/// Win11 23H2+): lets an APO steer `IAudioClient::GetMixFormat` toward a
/// preferred format. ADtune is format-agnostic (any float32 layout), so both
/// methods echo the provided opposite-pin format back as the preference.
#[repr(C)]
struct IAudioProcessingObjectPreferredFormatSupport_Vtbl {
    pub base__: IUnknown_Vtbl,
    pub GetPreferredInputFormat:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> HRESULT,
    pub GetPreferredOutputFormat:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> HRESULT,
}

static PREFERRED_FORMAT_VTBL: IAudioProcessingObjectPreferredFormatSupport_Vtbl =
    IAudioProcessingObjectPreferredFormatSupport_Vtbl {
        base__: IUnknown_Vtbl {
            QueryInterface: delegating_query_interface,
            AddRef: delegating_add_ref,
            Release: delegating_release,
        },
        GetPreferredInputFormat: apo_get_preferred_input_format,
        GetPreferredOutputFormat: apo_get_preferred_output_format,
    };

unsafe fn echo_preferred_format(
    direction: &str,
    opposite_format: *mut c_void,
    preferred: *mut *mut c_void,
) -> HRESULT {
    if preferred.is_null() {
        return E_POINTER;
    }
    *preferred = std::ptr::null_mut();
    let Some(media) = IAudioMediaType::from_raw_borrowed(&opposite_format) else {
        trace(&format!("{direction}: null opposite format"));
        return E_POINTER;
    };
    trace(&format!(
        "{direction}: echoing [{}]",
        describe_media_type(Some(media))
    ));
    *preferred = media.clone().into_raw();
    S_OK
}

unsafe extern "system" fn apo_get_preferred_input_format(
    _this: *mut c_void,
    opposite_format: *mut c_void,
    preferred_input_format: *mut *mut c_void,
) -> HRESULT {
    com_guard(|| {
        echo_preferred_format(
            "GetPreferredInputFormat",
            opposite_format,
            preferred_input_format,
        )
    })
}

unsafe extern "system" fn apo_get_preferred_output_format(
    _this: *mut c_void,
    opposite_format: *mut c_void,
    preferred_output_format: *mut *mut c_void,
) -> HRESULT {
    com_guard(|| {
        echo_preferred_format(
            "GetPreferredOutputFormat",
            opposite_format,
            preferred_output_format,
        )
    })
}

unsafe extern "system" fn apo_get_apo_notification_registration_info(
    _this: *mut c_void,
    registration_info: *mut *mut APO_NOTIFICATION_DESCRIPTOR,
    num_descriptors: *mut u32,
) -> HRESULT {
    trace("apo_get_apo_notification_registration_info: entering");
    if registration_info.is_null() || num_descriptors.is_null() {
        return E_POINTER;
    }
    *registration_info = std::ptr::null_mut();
    *num_descriptors = 0;
    S_OK
}

unsafe extern "system" fn apo_handle_notification(
    _this: *mut c_void,
    _apo_notification: *const APO_NOTIFICATION,
) {
    trace("apo_handle_notification: entering");
}

fn write_wstr(destination: &mut [u16], value: &str) {
    let mut index = 0;
    for character in value.encode_utf16() {
        if index + 1 >= destination.len() {
            break;
        }
        destination[index] = character;
        index += 1;
    }
    destination[index] = 0;
}

#[implement(IClassFactory)]
struct Factory;

impl Factory_Impl {
    fn create_instance_body(
        outer: windows_core::Ref<IUnknown>,
        riid: *const GUID,
        object: *mut *mut c_void,
    ) -> Result<()> {
        trace("Factory::CreateInstance: entering");
        if riid.is_null() || object.is_null() {
            trace("Factory::CreateInstance: null arguments");
            return Err(E_POINTER.into());
        }
        unsafe { *object = std::ptr::null_mut() };
        let requested_iid = unsafe { *riid };
        trace(&format!(
            "Factory::CreateInstance: riid={:?}, outer_null={}",
            requested_iid,
            outer.is_null()
        ));
        if !outer.is_null() && requested_iid != IUnknown::IID {
            trace("Factory::CreateInstance: aggregation requested but not asking for IUnknown");
            return Err(E_NOINTERFACE.into());
        }

        // Not AddRef'd: per COM aggregation rules the inner object must not
        // hold a counted reference on its outer unknown.
        let outer_unknown = outer.as_ref().map_or(std::ptr::null_mut(), |o| o.as_raw());
        let instance = Box::into_raw(Box::new(ApoObject::new(outer_unknown)));
        unsafe {
            (*instance).initialize_slots();
            let status = non_delegating_query_interface(instance.cast(), riid, object);
            if status.is_err() {
                drop(Box::from_raw(instance));
                return Err(status.into());
            }
        }
        Ok(())
    }
}

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: windows_core::Ref<IUnknown>,
        riid: *const GUID,
        object: *mut *mut c_void,
    ) -> Result<()> {
        // The #[implement] shim doesn't contain panics, and a panic unwinding
        // into COM would abort audiodg.exe — same rationale as `com_guard`.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Self::create_instance_body(outer, riid, object)
        }))
        .unwrap_or_else(|_| {
            trace("panic contained in Factory::CreateInstance");
            Err(E_FAIL.into())
        })
    }

    fn LockServer(&self, lock: windows_core::BOOL) -> Result<()> {
        if lock.as_bool() {
            SERVER_LOCKS.fetch_add(1, Ordering::Relaxed);
        } else {
            SERVER_LOCKS.fetch_sub(1, Ordering::Release);
        }
        Ok(())
    }
}

/// COM entry point: hand back the class factory for ADtune's CLSID.
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    class_id: *const GUID,
    interface_id: *const GUID,
    object: *mut *mut c_void,
) -> HRESULT {
    if class_id.is_null() || interface_id.is_null() || object.is_null() {
        return E_POINTER;
    }
    *object = std::ptr::null_mut();
    if *class_id != CLSID_ADTUNE_APO {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    let factory: IClassFactory = Factory.into();
    factory.query(interface_id, object)
}

#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if ACTIVE_OBJECTS.load(Ordering::Acquire) == 0 && SERVER_LOCKS.load(Ordering::Acquire) == 0 {
        S_OK
    } else {
        S_FALSE
    }
}
