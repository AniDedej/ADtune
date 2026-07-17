//! ADtune's Windows control-plane: writes the correction where ADtune's own APO
//! (`crates/adtune-apo`) reads it, enumerates output devices, and
//! enables/detects the APO on a device.
//!
//! Config/state/writing are pure `std::fs` (unit-tested on any OS); device
//! enumeration and registry access are Windows-only (stubbed elsewhere), so the
//! crate builds and tests on Linux and is only *used* on Windows.

mod devices;
mod elevate;
mod env;
mod native;
pub mod register;
mod state;
mod writer;

pub use devices::OutputDevice;
pub use elevate::{record_op_result, run_elevated};
pub use native::NativeApo;
pub use register::{
    disable_apo_everywhere, disable_apo_on, enable_apo_on, enable_default_apo, restart_audio_engine,
};

/// Crate-wide result. Errors are already-formatted, user-facing strings (the UI
/// surfaces them directly), so there is no structured error type to thread.
pub type Result<T> = std::result::Result<T, String>;

/// The `MMDevices\Audio\Render\…` subkey for an endpoint is named with only the
/// endpoint GUID (the trailing `{…}` group), NOT the full `IMMDevice::GetId`
/// string `{0.0.0.00000000}.{GUID}`. Return that trailing group (the whole
/// string if it has no brace, so a bare GUID passes through unchanged).
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn endpoint_guid(id: &str) -> &str {
    match id.rfind('{') {
        Some(i) => &id[i..],
        None => id,
    }
}

/// Whether `id` is a well-formed render-endpoint id — the shape
/// `IMMDevice::GetId` returns (`{0.0.0.00000000}.{GUID}`) or a bare GUID.
///
/// Endpoint ids flow into an elevated child's command line and into HKLM
/// registry paths. Ids come from Windows enumeration (not user input), so this
/// is defense in depth: validating the charset before either use guarantees a
/// malformed or injected id can't split the elevated argv (no spaces/quotes),
/// masquerade as a `--flag`, or steer an `endpoint_guid`-derived registry write
/// to an arbitrary subkey. The real format uses only hex digits, braces, dots,
/// and dashes; reject everything else. Pure string logic, so it is unit-tested
/// on any OS.
pub fn is_valid_endpoint_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id.contains('{')
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() || matches!(b, b'{' | b'}' | b'.' | b'-'))
}

/// Result of an apply: `needs_enable` is true when the APO is not yet registered
/// on the target device (the installer performs the one-time, admin enable).
pub struct ApplyOutcome {
    pub needs_enable: bool,
}

#[cfg(test)]
mod tests {
    use super::{endpoint_guid, is_valid_endpoint_id};

    #[test]
    fn extracts_trailing_guid_from_endpoint_id() {
        // The real IMMDevice::GetId shape.
        assert_eq!(
            endpoint_guid("{0.0.0.00000000}.{e6327cad-dcec-4949-ae8a-991e976a79d2}"),
            "{e6327cad-dcec-4949-ae8a-991e976a79d2}"
        );
        // A bare GUID (or already-stripped) passes through unchanged.
        assert_eq!(
            endpoint_guid("{b39fc22d-4c5d-4e65-8276-db7f999d2d06}"),
            "{b39fc22d-4c5d-4e65-8276-db7f999d2d06}"
        );
        // No brace at all → unchanged (defensive).
        assert_eq!(endpoint_guid("plain"), "plain");
    }

    #[test]
    fn accepts_real_endpoint_ids() {
        assert!(is_valid_endpoint_id(
            "{0.0.0.00000000}.{e6327cad-dcec-4949-ae8a-991e976a79d2}"
        ));
        assert!(is_valid_endpoint_id(
            "{b39fc22d-4c5d-4e65-8276-db7f999d2d06}"
        ));
    }

    #[test]
    fn rejects_injection_shaped_ids() {
        assert!(!is_valid_endpoint_id("")); // empty
        assert!(!is_valid_endpoint_id("--disable-apo")); // flag lookalike (no brace, has letters)
        assert!(!is_valid_endpoint_id("{guid} --enable-apo {other}")); // space → argv split
        assert!(!is_valid_endpoint_id("{a\"b}")); // quote
        assert!(!is_valid_endpoint_id("{a}\\..\\evil")); // path separators
        assert!(!is_valid_endpoint_id("plain-no-brace")); // no brace
        assert!(!is_valid_endpoint_id(&"{".repeat(300))); // over length
    }
}
