//! Enable/disable ADtune's own APO on a playback device (writes HKLM
//! `FxProperties`, so it needs administrator rights — run from the installer or
//! an elevated `adtune.exe --enable-apo`). Windows-only; stubbed elsewhere.

/// ADtune APO CLSID (must match `crates/adtune-apo`).
pub const APO_CLSID: &str = "{7f4a1e02-9c3b-4d5a-8e21-ad7c0c0ffee1}";

/// The CLSID body (no braces), lowercased — used to recognize our own value in a
/// registry slot. Matching the *full* GUID (not an 8-hex fragment) means a
/// vendor CLSID that merely happens to contain `7f4a1e02` is never mistaken for
/// ours and stripped. GUIDs are unique, so a full-body match is exact.
pub const APO_CLSID_BODY: &str = "7f4a1e02-9c3b-4d5a-8e21-ad7c0c0ffee1";

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::devices::enumerate;
    use crate::{endpoint_guid, is_valid_endpoint_id};
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_SET_VALUE};
    use winreg::RegKey;

    /// The fixed FxProperties PKEY that names the effect for each pipeline stage.
    /// Slot indices: 1 = LFX, 2 = GFX (Win7-era); 5 = SFX, 6 = MFX, 7 = EFX
    /// (Win8.1+); 13/14/15 = the composite (multi-CLSID) SFX/MFX/EFX lists that
    /// Windows 10 1809+ prefers when present.
    const FX_PKEY: &str = "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d}";

    /// PKEY_AudioEndpoint_Disable_SysFx: 1 = the user disabled all enhancements
    /// on the endpoint (which would keep our APO from loading).
    const DISABLE_SYSFX: &str = "{1da5d803-d492-4edd-8c23-e0c0ffee7f0e},5";

    /// PKEY_{SFX,MFX,EFX}_ProcessingModes_Supported_For_Streaming (index 5/6/7).
    /// The effects host only places a stage's APO into the graph of a
    /// signal-processing mode the endpoint declares here. Without it, the
    /// Windows 11 builder loads the APO, probes it (Initialize + format check,
    /// all succeeding), then abandons the endpoint graph without ever calling
    /// LockForProcess — and the endpoint stops playing entirely. Drivers
    /// declare this in their INF, so a software-installed system-effect APO must
    /// write it explicitly to be placed into the DEFAULT-mode stream graph.
    const PROCESSING_MODES_PKEY: &str = "{d3993a3f-99c2-4402-b5ec-a92a0367664b}";
    /// AUDIO_SIGNALPROCESSINGMODE_DEFAULT — the mode every shared stream uses.
    const MODE_DEFAULT: &str = "{C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}";

    fn fx_key(endpoint_id: &str) -> String {
        format!(
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render\{}\FxProperties",
            endpoint_guid(endpoint_id)
        )
    }

    fn contains_us(value: &str) -> bool {
        value.to_ascii_lowercase().contains(APO_CLSID_BODY)
    }

    /// Where a vendor CLSID we displaced from a legacy slot is preserved so
    /// `disable_on` can restore it. The audio engine ignores value names that
    /// aren't PKEY-shaped, so these extra values are inert.
    fn backup_name(slot: &str) -> String {
        format!("ADtune.backup.{slot}")
    }
    const SYSFX_BACKUP: &str = "ADtune.backup.sysfx";

    /// Make sure the endpoint declares DEFAULT-mode streaming support for the
    /// stage we occupy (`stage` = 5 SFX, 6 MFX, 7 EFX; legacy LFX/GFX predate
    /// modes and need none). Non-destructive: an existing declaration is only
    /// extended, and backups let `strip_ours` restore the prior state exactly.
    fn ensure_streaming_mode(key: &RegKey, stage: &str) -> Result<(), String> {
        let name = format!("{PROCESSING_MODES_PKEY},{stage}");
        let created_marker = format!("ADtune.backup.modescreated.{stage}");
        let has_default =
            |list: &[String]| list.iter().any(|m| m.eq_ignore_ascii_case(MODE_DEFAULT));

        let existing: Option<Vec<String>> = key
            .get_value::<Vec<String>, _>(&name)
            .ok()
            .or_else(|| key.get_value::<String, _>(&name).ok().map(|s| vec![s]));
        match existing {
            Some(list) if has_default(&list) => Ok(()),
            Some(list) => {
                // Preserve the vendor's list so disable can restore it.
                key.set_value(format!("ADtune.backup.modes.{stage}"), &list)
                    .map_err(|e| format!("could not back up processing modes ({stage}): {e}"))?;
                let mut extended = list;
                extended.push(MODE_DEFAULT.to_string());
                key.set_value(&name, &extended)
                    .map_err(|e| format!("processing-modes set-value ({stage}) failed: {e}"))
            }
            None => {
                key.set_value(&name, &vec![MODE_DEFAULT.to_string()])
                    .map_err(|e| format!("processing-modes set-value ({stage}) failed: {e}"))?;
                let _ = key.set_value(&created_marker, &1u32);
                Ok(())
            }
        }
    }

    /// Append our CLSID to a composite (REG_MULTI_SZ) slot if that slot exists.
    /// Composite slots hold a list, so this never displaces a vendor effect.
    /// Returns None when the slot is absent, Some(result) when it was handled.
    fn join_composite(key: &RegKey, slot: &str) -> Option<Result<(), String>> {
        let name = format!("{FX_PKEY},{slot}");
        if let Ok(mut list) = key.get_value::<Vec<String>, _>(&name) {
            if !list.iter().any(|x| contains_us(x)) {
                list.push(APO_CLSID.to_string());
                return Some(key.set_value(&name, &list).map_err(|e| {
                    format!("FxProperties set-value (composite {slot}) failed: {e}")
                }));
            }
            return Some(Ok(()));
        }
        // Some drivers write composite slots as a plain string; normalize to a list.
        if let Ok(s) = key.get_value::<String, _>(&name) {
            if contains_us(&s) {
                return Some(Ok(()));
            }
            let list = if s.is_empty() {
                vec![APO_CLSID.to_string()]
            } else {
                vec![s, APO_CLSID.to_string()]
            };
            return Some(
                key.set_value(&name, &list)
                    .map_err(|e| format!("FxProperties set-value (composite {slot}) failed: {e}")),
            );
        }
        None
    }

    /// Take over a legacy single-CLSID slot if it exists, preserving the vendor
    /// CLSID in a backup value so `disable_on` can put it back.
    /// Returns None when the slot is absent, Some(result) when it was handled.
    fn take_legacy(key: &RegKey, slot: &str) -> Option<Result<(), String>> {
        let name = format!("{FX_PKEY},{slot}");
        let existing = key.get_value::<String, _>(&name).ok()?;
        if contains_us(&existing) {
            return Some(Ok(()));
        }
        if !existing.trim().is_empty() {
            if let Err(e) = key.set_value(backup_name(slot), &existing) {
                return Some(Err(format!(
                    "could not preserve the existing effect in slot {slot}: {e}"
                )));
            }
        }
        Some(
            key.set_value(&name, &APO_CLSID)
                .map_err(|e| format!("FxProperties set-value ({slot}) failed: {e}")),
        )
    }

    /// Register ADtune's APO on one endpoint. Endpoint FxProperties keys grant
    /// Administrators value-write but not key create/delete, so open the
    /// existing key for `KEY_SET_VALUE` first and only fall back to creating it
    /// (which may need key ownership) if it's absent.
    fn enable_on(endpoint_id: &str) -> Result<(), String> {
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let path = fx_key(endpoint_id);
        let key = match hklm.open_subkey_with_flags(&path, KEY_QUERY_VALUE | KEY_SET_VALUE) {
            Ok(k) => k,
            Err(_) => hklm.create_subkey(&path).map(|(k, _)| k).map_err(|e| {
                format!("FxProperties write failed (needs administrator / key ownership): {e}")
            })?,
        };

        // Start from a clean slate: remove any previous ADtune registration
        // (restoring whatever it displaced) so re-enabling never compounds
        // stale state written by an older version of this code.
        strip_ours(&key);

        // Attach the APO FIRST, then flip enhancements on last. If attaching
        // fails (e.g. access-denied on a slot), the endpoint keeps its existing
        // enhancement state rather than being left enhancements-forced-on with
        // no APO — the only ordering that can't leave audio in a changed state
        // on a partial failure.
        attach_apo(&key)?;

        // Enhancements must be on for any APO to load, and the Windows 11
        // Settings page ("Audio enhancements: Off") sets this value to 1.
        // Remember the user's explicit "Off" so disable_on can restore it,
        // then DELETE the value — that is exactly the "Device Default
        // Effects" state the Settings page itself writes (it never writes 0).
        if let Ok(1u32) = key.get_value::<u32, _>(DISABLE_SYSFX) {
            let _ = key.set_value(SYSFX_BACKUP, &1u32);
        }
        let _ = key.delete_value(DISABLE_SYSFX);
        Ok(())
    }

    /// Attach ADtune to the first effect stage the endpoint already uses (SFX,
    /// then MFX, then EFX; composite lists preferred), declaring DEFAULT-mode
    /// streaming support for the stage we land in. Falls back to a fresh legacy
    /// SFX registration on an endpoint with no effects at all.
    fn attach_apo(key: &RegKey) -> Result<(), String> {
        let stages: [(&str, &str, &[&str]); 3] = [
            ("13", "5", &["5", "1"]),
            ("14", "6", &["6", "2"]),
            ("15", "7", &["7"]),
        ];
        for (composite, mode_stage, legacies) in stages {
            if let Some(result) = join_composite(key, composite) {
                return result.and_then(|()| ensure_streaming_mode(key, mode_stage));
            }
            for legacy in legacies {
                if let Some(result) = take_legacy(key, legacy) {
                    // SFX/MFX/EFX slots need the mode declaration; the Win7-era
                    // LFX/GFX slots (1/2) predate processing modes.
                    return result.and_then(|()| {
                        if *legacy == mode_stage {
                            ensure_streaming_mode(key, mode_stage)
                        } else {
                            Ok(())
                        }
                    });
                }
            }
        }

        // Fresh endpoint with no effects at all (typical for VMs and generic
        // drivers): register as a legacy stream effect (SFX) ONLY — the
        // configuration proven to stream on Windows 11. Do NOT write the composite slot
        // (13) here: drivers always pair composite FX values with INF-declared
        // processing-mode properties, and an unpaired composite entry sends the
        // Windows 11 effects host down its mode-aware path, where it repeatedly
        // probes the APO (Create → Initialize → format check) and abandons the
        // graph without ever calling LockForProcess — no audio.
        key.set_value(format!("{FX_PKEY},5"), &APO_CLSID)
            .map_err(|e| format!("FxProperties set-value (5) failed: {e}"))?;
        ensure_streaming_mode(key, "5")
    }

    /// Allow an unsigned APO to load into audiodg.exe. This weakens a
    /// machine-wide Windows security boundary (it lets ANY HKLM-registered APO
    /// load into the protected audio engine) and is only necessary because the
    /// ADtune DLL is unsigned. Authenticode-signing the DLL is the proper fix,
    /// after which this switch — and the value in the installer — should be
    /// dropped entirely. Idempotent: only writes when not already set, so a
    /// re-enable doesn't needlessly re-assert the downgrade.
    fn disable_protected_audio_dg() -> Result<(), String> {
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let (key, _) = hklm
            .create_subkey(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Audio")
            .map_err(|e| e.to_string())?;
        if matches!(key.get_value::<u32, _>("DisableProtectedAudioDG"), Ok(1)) {
            return Ok(());
        }
        key.set_value("DisableProtectedAudioDG", &1u32)
            .map_err(|e| e.to_string())
    }

    fn register_com_and_apo(dll_path: &std::path::Path) -> Result<(), String> {
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);

        // 1. COM registration
        let clsid_path = format!(r"SOFTWARE\Classes\CLSID\{APO_CLSID}");
        let (clsid_key, _) = hklm
            .create_subkey(&clsid_path)
            .map_err(|e| format!("Failed to create CLSID key: {e}"))?;
        clsid_key
            .set_value("", &"ADtune APO")
            .map_err(|e| format!("Failed to set CLSID default value: {e}"))?;

        let (inproc_key, _) = hklm
            .create_subkey(format!(r"{clsid_path}\InprocServer32"))
            .map_err(|e| format!("Failed to create InprocServer32 key: {e}"))?;
        inproc_key
            .set_value("", &dll_path.to_string_lossy().as_ref())
            .map_err(|e| format!("Failed to set InprocServer32 default value: {e}"))?;
        inproc_key
            .set_value("ThreadingModel", &"Both")
            .map_err(|e| format!("Failed to set ThreadingModel: {e}"))?;

        // 2. AudioEngine registration
        let apo_path = format!(r"SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{APO_CLSID}");
        let (apo_key, _) = hklm
            .create_subkey(&apo_path)
            .map_err(|e| format!("Failed to create AudioProcessingObjects key: {e}"))?;
        apo_key
            .set_value("FriendlyName", &"ADtune APO")
            .map_err(|e| format!("Failed to set FriendlyName: {e}"))?;
        // APOInterface0 must declare the FUNCTIONAL interface the engine drives
        // the APO through: IAudioProcessingObject. Every working system-effect
        // APO (every in-box Windows system-effect APO) declares exactly this; declaring
        // IAudioSystemEffects here instead makes the engine probe the APO and
        // then abandon the endpoint graph without ever calling LockForProcess.
        // Also prune the stale entries older ADtune registrations wrote.
        apo_key
            .set_value("APOInterface0", &"{FD7F2B29-24D0-4B5C-B177-592C39F9CA10}")
            .map_err(|e| format!("Failed to set APOInterface0: {e}"))?;
        let _ = apo_key.delete_value("APOInterface1");
        let _ = apo_key.delete_value("APOInterface2");
        // 0xD = INPLACE | FRAMESPERSECOND_MUST_MATCH | BITSPERSAMPLE_MUST_MATCH:
        // the exact flags the in-box WM-audio system-effect APOs register with.
        apo_key
            .set_value("Flags", &0xDu32)
            .map_err(|e| format!("Failed to set Flags: {e}"))?;
        apo_key
            .set_value("NumAPOInterfaces", &1u32)
            .map_err(|e| format!("Failed to set NumAPOInterfaces: {e}"))?;
        apo_key
            .set_value("Copyright", &"Copyright (c) 2026 Antonio DEDEJ")
            .map_err(|e| format!("Failed to set Copyright: {e}"))?;
        apo_key
            .set_value("MajorVersion", &1u32)
            .map_err(|e| format!("Failed to set MajorVersion: {e}"))?;
        apo_key
            .set_value("MinorVersion", &0u32)
            .map_err(|e| format!("Failed to set MinorVersion: {e}"))?;
        apo_key
            .set_value("MinInputConnections", &1u32)
            .map_err(|e| format!("Failed to set MinInputConnections: {e}"))?;
        apo_key
            .set_value("MaxInputConnections", &1u32)
            .map_err(|e| format!("Failed to set MaxInputConnections: {e}"))?;
        apo_key
            .set_value("MinOutputConnections", &1u32)
            .map_err(|e| format!("Failed to set MinOutputConnections: {e}"))?;
        apo_key
            .set_value("MaxOutputConnections", &1u32)
            .map_err(|e| format!("Failed to set MaxOutputConnections: {e}"))?;
        apo_key
            .set_value("MaxInstances", &u32::MAX)
            .map_err(|e| format!("Failed to set MaxInstances: {e}"))?;

        Ok(())
    }

    /// Make sure the APO's COM class is registered and points at a DLL that
    /// exists, refreshing it from the copy next to our exe when there is one.
    /// Attaching an endpoint to a CLSID nothing can create would break audio on
    /// that endpoint, so a missing DLL is a hard error, not a silent skip.
    fn ensure_com_registration() -> Result<(), String> {
        let local_dll = std::env::current_exe()
            .ok()
            .and_then(|exe| Some(exe.parent()?.join("adtune_apo.dll")))
            .filter(|p| p.exists());
        if let Some(dll) = local_dll {
            return register_com_and_apo(&dll);
        }
        let registered = RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey(format!(
                r"SOFTWARE\Classes\CLSID\{APO_CLSID}\InprocServer32"
            ))
            .and_then(|k| k.get_value::<String, _>(""))
            .map(|dll| std::path::Path::new(&dll).exists())
            .unwrap_or(false);
        if registered {
            Ok(())
        } else {
            Err(
                "adtune_apo.dll was not found next to adtune.exe and no installed \
                 ADtune APO registration exists. Reinstall ADtune."
                    .into(),
            )
        }
    }

    /// Enable ADtune's APO on a specific render endpoint id.
    pub fn enable_apo_on(endpoint_id: &str) -> Result<(), String> {
        // Reject anything that isn't a well-formed endpoint id before it reaches
        // a registry path (this runs elevated). Ids come from device
        // enumeration, so a rejection means something is wrong, not hostile.
        if !is_valid_endpoint_id(endpoint_id) {
            return Err("Refusing to enable: malformed audio endpoint id.".into());
        }
        ensure_com_registration()?;
        enable_on(endpoint_id)?;
        disable_protected_audio_dg()
    }

    /// Enable ADtune's APO on the current default render device.
    pub fn enable_default_apo() -> Result<String, String> {
        let devices = enumerate()?;
        let dev = devices
            .iter()
            .find(|d| d.is_default)
            .or_else(|| devices.first())
            .ok_or("No playback device found.")?;
        enable_apo_on(&dev.id)?;
        Ok(dev.friendly_name.clone())
    }

    /// Bounce the Windows audio engine so a just-changed APO registration is
    /// (un)loaded immediately, instead of only at the next device init (which is
    /// what made enabling feel like it "needed a reinstall"). Restarting
    /// AudioEndpointBuilder rebuilds the endpoint effect graph (re-reading
    /// FxProperties); Audiosrv (its dependent) is stopped with it and restarted
    /// after. Needs admin, so it runs inside the elevated `--enable-apo` child.
    pub fn restart_audio_engine() -> Result<(), String> {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        // Resolve tools from the system directory rather than trusting PATH:
        // this runs elevated, so a PATH-hijacked `net` would run with admin
        // rights. %SystemRoot% is set on every Windows install.
        let sys32 = {
            let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
            format!(r"{root}\System32")
        };
        let run = |exe: &str, args: &[&str]| -> (bool, String) {
            std::process::Command::new(format!(r"{sys32}\{exe}"))
                .args(args)
                .creation_flags(CREATE_NO_WINDOW) // no console flash
                .output()
                .map(|o| {
                    (
                        o.status.success(),
                        String::from_utf8_lossy(&o.stdout).into_owned(),
                    )
                })
                .unwrap_or((false, String::new()))
        };
        // `/y` confirms stopping the DEPENDENT Windows Audio (Audiosrv) along
        // with the builder it depends on.
        run("net.exe", &["stop", "AudioEndpointBuilder", "/y"]);
        // Starting a service starts its DEPENDENCIES, never its dependents:
        // `start Audiosrv` pulls AudioEndpointBuilder back up with it, whereas
        // starting only the builder would leave Windows Audio stopped — dead
        // sound until a manual service restart.
        let started = run("net.exe", &["start", "Audiosrv"]).0;
        // `net start` also fails when the service is already running (e.g. the
        // stop above was refused), so check the real state before reporting
        // failure.
        if started || run("sc.exe", &["query", "Audiosrv"]).1.contains("RUNNING") {
            Ok(())
        } else {
            Err("Could not restart the Windows audio service.".into())
        }
    }

    /// Strip ADtune's APO from one endpoint's FxProperties (every stage slot),
    /// restoring any vendor CLSID we displaced when we enabled.
    fn disable_on(endpoint_id: &str) {
        let Ok(key) = RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey_with_flags(fx_key(endpoint_id), KEY_QUERY_VALUE | KEY_SET_VALUE)
        else {
            return;
        };
        strip_ours(&key);
    }

    /// Remove ADtune from every FX slot of an open FxProperties key, restoring
    /// displaced vendor values and the user's enhancements setting. Also used
    /// by `enable_on` so a re-enable starts from a clean slate instead of
    /// compounding stale state left by an older writer.
    fn strip_ours(key: &RegKey) {
        for slot in ["5", "6", "7", "1", "2"] {
            let name = format!("{FX_PKEY},{slot}");
            if let Ok(v) = key.get_value::<String, _>(&name) {
                if contains_us(&v) {
                    match key.get_value::<String, _>(&backup_name(slot)) {
                        Ok(original) => {
                            let _ = key.set_value(&name, &original);
                        }
                        Err(_) => {
                            let _ = key.delete_value(&name);
                        }
                    }
                }
            }
            let _ = key.delete_value(backup_name(slot));
        }
        for slot in ["13", "14", "15"] {
            let name = format!("{FX_PKEY},{slot}");
            let mut list: Vec<String> = match key.get_value::<Vec<String>, _>(&name) {
                Ok(l) => l,
                Err(_) => {
                    if let Ok(s) = key.get_value::<String, _>(&name) {
                        if contains_us(&s) {
                            let _ = key.delete_value(&name);
                        }
                    }
                    continue;
                }
            };
            let orig_len = list.len();
            list.retain(|x| !contains_us(x));
            if list.len() != orig_len {
                if list.is_empty() {
                    let _ = key.delete_value(&name);
                } else {
                    let _ = key.set_value(&name, &list);
                }
            }
        }
        // Undo our processing-mode declarations: delete the ones we created,
        // restore the vendor lists we extended.
        for stage in ["5", "6", "7"] {
            let name = format!("{PROCESSING_MODES_PKEY},{stage}");
            let created_marker = format!("ADtune.backup.modescreated.{stage}");
            let backup = format!("ADtune.backup.modes.{stage}");
            if key.get_value::<u32, _>(&created_marker).is_ok() {
                let _ = key.delete_value(&name);
            } else if let Ok(original) = key.get_value::<Vec<String>, _>(&backup) {
                let _ = key.set_value(&name, &original);
            }
            let _ = key.delete_value(created_marker);
            let _ = key.delete_value(backup);
        }

        // Put back the user's "enhancements disabled" choice if we overrode it.
        if let Ok(1u32) = key.get_value::<u32, _>(SYSFX_BACKUP) {
            let _ = key.set_value(DISABLE_SYSFX, &1u32);
        }
        let _ = key.delete_value(SYSFX_BACKUP);
    }

    /// Remove ADtune's APO from one render endpoint (the in-app "disable").
    pub fn disable_apo_on(endpoint_id: &str) -> Result<(), String> {
        if !is_valid_endpoint_id(endpoint_id) {
            return Err("Refusing to disable: malformed audio endpoint id.".into());
        }
        disable_on(endpoint_id);
        Ok(())
    }

    /// Remove ADtune's APO from every render endpoint (uninstall). Sweeps the
    /// registry rather than the live device list so endpoints that are currently
    /// disabled or unplugged get cleaned too. Subkey names come straight from the
    /// registry, so they need no endpoint-id validation (they are already the
    /// trailing-GUID form `endpoint_guid` expects).
    pub fn disable_apo_everywhere() -> Result<(), String> {
        let render = RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey(r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render");
        if let Ok(render) = render {
            for endpoint in render.enum_keys().flatten() {
                disable_on(&endpoint);
            }
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn enable_apo_on(_endpoint_id: &str) -> Result<(), String> {
        Err("The ADtune APO is Windows-only.".into())
    }
    pub fn enable_default_apo() -> Result<String, String> {
        Err("The ADtune APO is Windows-only.".into())
    }
    pub fn disable_apo_on(_endpoint_id: &str) -> Result<(), String> {
        Ok(())
    }
    pub fn disable_apo_everywhere() -> Result<(), String> {
        Ok(())
    }
    pub fn restart_audio_engine() -> Result<(), String> {
        Ok(())
    }
}

pub use imp::*;
