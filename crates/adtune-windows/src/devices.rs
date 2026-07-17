//! Enumerate active render (playback) endpoints via WASAPI/MMDevice on Windows.
//! Non-Windows builds get a stub.

/// A playback device. `id` is the stable IMMDevice endpoint id (rename-proof).
#[derive(Clone, Debug, Default)]
pub struct OutputDevice {
    pub id: String,
    pub friendly_name: String,
    pub is_default: bool,
}

/// Enumerate the active render (playback) endpoints via the WASAPI MMDevice API,
/// flagging the current default. Each entry carries the stable endpoint id, a
/// display name, and a default flag; devices that are disabled/unplugged/absent
/// are skipped (only `DEVICE_STATE_ACTIVE` is requested).
#[cfg(windows)]
pub fn enumerate() -> Result<Vec<OutputDevice>, String> {
    use windows::core::PWSTR;
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Media::Audio::{
        eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
    };
    use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
        STGM_READ,
    };

    /// Copy a COM-allocated wide string into an owned `String` and free it.
    /// `GetId` and `PropVariantToStringAlloc` hand back `PWSTR`s allocated with
    /// `CoTaskMemAlloc`; the caller owns them and must `CoTaskMemFree` each one
    /// or the memory leaks. Doing the copy-then-free here keeps that ownership
    /// contract in one place so no call site can forget it.
    unsafe fn pwstr_string(p: PWSTR) -> String {
        if p.is_null() {
            return String::new();
        }
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.as_ptr() as *const _));
        s
    }

    unsafe {
        // COM must be initialised on this thread before any interface call.
        // S_FALSE (already initialised) is fine; RPC_E_CHANGED_MODE (a different
        // apartment model was set earlier) we ignore — either way COM is up and
        // the MMDevice calls below work regardless of apartment.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| e.to_string())?;

        // Resolve the default console-render endpoint's id up front so each
        // device can be tagged. A missing default (no output at all) is not an
        // error — leave it `None` and flag nothing.
        let default_id = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .ok()
            .and_then(|d| d.GetId().ok())
            .map(|p| pwstr_string(p));

        let collection = enumerator
            .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
            .map_err(|e| e.to_string())?;
        let count = collection.GetCount().map_err(|e| e.to_string())?;

        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            let dev = collection.Item(i).map_err(|e| e.to_string())?;
            let id = pwstr_string(dev.GetId().map_err(|e| e.to_string())?);
            // Friendly name is best-effort: open the property store, read the
            // FriendlyName PROPVARIANT, stringify it. Any step can legitimately
            // fail (or yield an empty name), so fall back to the endpoint id —
            // the device stays selectable either way.
            let name = dev
                .OpenPropertyStore(STGM_READ)
                .ok()
                .and_then(|store| store.GetValue(&PKEY_Device_FriendlyName).ok())
                .and_then(|prop| PropVariantToStringAlloc(&prop).ok())
                .map(|p| pwstr_string(p))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| id.clone());
            let is_default = default_id.as_deref() == Some(id.as_str());
            out.push(OutputDevice {
                id,
                friendly_name: name,
                is_default,
            });
        }
        Ok(out)
    }
}

#[cfg(not(windows))]
pub fn enumerate() -> Result<Vec<OutputDevice>, String> {
    Err("The ADtune Windows backend is Windows-only.".into())
}
