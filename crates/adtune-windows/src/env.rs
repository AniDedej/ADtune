//! Device APO-enable detection via the Windows registry. Non-Windows builds get
//! a stub so the crate compiles and its OS-independent modules stay testable on
//! Linux.

#[cfg(windows)]
mod imp {
    use winreg::enums::HKEY_LOCAL_MACHINE;
    use winreg::RegKey;

    /// Whether the render endpoint has the APO whose CLSID contains `clsid_hex`
    /// (a lowercase fragment) registered in any effect slot.
    ///
    /// The value name `{d04e05a6-…},N` is Microsoft's `PKEY_FX_StreamEffectClsid`
    /// (and its mode/legacy variants); the value data is the CLSID of the APO to
    /// load in that slot.
    pub fn is_apo_enabled(endpoint_id: &str, clsid_hex: &str) -> bool {
        let sub = format!(
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render\{}\FxProperties",
            crate::endpoint_guid(endpoint_id)
        );
        let Ok(key) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(sub) else {
            return false;
        };
        // SFX (5) / MFX (6) / legacy LFX (1) / GFX (2) stage slots.
        // Also check modern Windows 10/11 composite slots: SFX (13) / MFX (14) / EFX (15).
        for slot in ["5", "6", "1", "2", "13", "14", "15"] {
            let value = format!("{{d04e05a6-594b-4fb6-a80d-01af5eed7d1d}},{slot}");
            // A slot may hold a chain of APO CLSIDs (REG_MULTI_SZ) or a single
            // one (REG_SZ); winreg types are strict, so probe the multi-string
            // form first and fall back to the scalar. Match case-insensitively
            // since the registry may store the GUID in either case.
            if let Ok(v) = key.get_value::<Vec<String>, _>(&value) {
                for s in v {
                    if s.to_ascii_lowercase()
                        .contains(&clsid_hex.to_ascii_lowercase())
                    {
                        return true;
                    }
                }
            } else if let Ok(s) = key.get_value::<String, _>(&value) {
                if s.to_ascii_lowercase()
                    .contains(&clsid_hex.to_ascii_lowercase())
                {
                    return true;
                }
            }
        }
        false
    }

    /// Whether audio enhancements are on for the endpoint. The Windows 11
    /// Settings page ("Audio enhancements: Off") writes
    /// `PKEY_AudioEndpoint_Disable_SysFx` = 1 into FxProperties; absent or 0
    /// means "Device Default Effects". With enhancements off, NO APO loads —
    /// including ADtune's — so the enable flow must clear this (elevated).
    pub fn enhancements_enabled(endpoint_id: &str) -> bool {
        let sub = format!(
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render\{}\FxProperties",
            crate::endpoint_guid(endpoint_id)
        );
        let Ok(key) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(sub) else {
            return true; // no FxProperties key → nothing disabled
        };
        !matches!(
            key.get_value::<u32, _>("{1da5d803-d492-4edd-8c23-e0c0ffee7f0e},5"),
            Ok(1)
        )
    }
}

// Non-Windows stub: there is no HKLM to read. The values mirror the "clean"
// Windows state (APO not registered, enhancements on) so any caller compiled on
// Linux behaves sanely; the controller is only ever exercised for real on Windows.
#[cfg(not(windows))]
mod imp {
    pub fn is_apo_enabled(_endpoint_id: &str, _clsid_hex: &str) -> bool {
        false
    }
    pub fn enhancements_enabled(_endpoint_id: &str) -> bool {
        true
    }
}

pub use imp::*;
