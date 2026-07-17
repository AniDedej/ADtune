//! Host `libpipewire-module-filter-chain` inside this process.
//!
//! Versions ≤ 1.0 ran the filter by having systemd spawn a second `pipewire`
//! process whose config loaded the module. Loading the module into our own
//! context does exactly the same thing — same graph, same virtual sink —
//! without systemd, a host `pipewire` binary, or a config file on disk, which
//! is what makes the backend work under Flatpak/Snap confinement.
//!
//! This module holds the only FFI in the crate: the safe bindings don't wrap
//! `pw_context_load_module` / `pw_impl_module_destroy` yet.

use crate::conn::Session;
use crate::render;
use adtune_core::{profile_preamp_db, AudioProfile, ToneSettings};
use pipewire::spa::pod::serialize::PodSerializer;
use pipewire::spa::pod::{Object, Property, PropertyFlags, Value};
use std::ffi::CString;
use std::io::Cursor;
use std::ptr::NonNull;

/// A filter-chain module loaded into the session's context. Dropping the
/// handle unloads the module, which tears down the virtual sink.
pub struct FilterModule {
    module: NonNull<pipewire::sys::pw_impl_module>,
}

impl FilterModule {
    /// Load `module-filter-chain` with `args` (from
    /// [`render::filter_chain_args`]). Errors carry the OS-level cause —
    /// typically a malformed graph or an exhausted server.
    pub fn load(session: &Session, args: &str) -> Result<FilterModule, String> {
        let name = c"libpipewire-module-filter-chain";
        let args = CString::new(args).map_err(|_| "filter args contain a NUL byte".to_string())?;
        // SAFETY: context pointer is valid for the session's lifetime; name
        // and args outlive the call (load_module copies what it keeps).
        let ptr = unsafe {
            pipewire::sys::pw_context_load_module(
                session.context.as_raw_ptr(),
                name.as_ptr(),
                args.as_ptr(),
                std::ptr::null_mut(),
            )
        };
        NonNull::new(ptr)
            .map(|module| FilterModule { module })
            .ok_or_else(|| {
                format!(
                    "Could not load the PipeWire filter-chain module: {}",
                    std::io::Error::last_os_error()
                )
            })
    }
}

impl Drop for FilterModule {
    fn drop(&mut self) {
        // SAFETY: the pointer came from pw_context_load_module and is
        // destroyed exactly once, on the loop thread that created it.
        unsafe { pipewire::sys::pw_impl_module_destroy(self.module.as_ptr()) }
    }
}

/// The live-update payload as a SPA pod: a `Props` object whose `params`
/// struct lists alternating control names and values — the same content as
/// [`render::live_params`], in binary form for `Node::set_param`.
///
/// Only gain-type controls are pushed live (pre-gain multiplier and per-band
/// gains); frequency/Q/structure changes reload the module instead, exactly
/// like the old restart path.
pub fn live_props_pod(profile: &AudioProfile, tone: &ToneSettings) -> Result<Vec<u8>, String> {
    let pregain = profile_preamp_db(profile, tone);
    let mut params = vec![
        Value::String("pre_gain:Mult".into()),
        Value::Double(10f64.powf(pregain / 20.0)),
    ];
    for (slot, band) in render::graph_bands(profile, tone) {
        params.push(Value::String(format!("{slot}:Gain")));
        params.push(Value::Double(band.gain));
    }
    let props = Value::Object(Object {
        type_: pipewire::spa::sys::SPA_TYPE_OBJECT_Props,
        id: pipewire::spa::sys::SPA_PARAM_Props,
        properties: vec![Property {
            key: pipewire::spa::sys::SPA_PROP_params,
            flags: PropertyFlags::empty(),
            value: Value::Struct(params),
        }],
    });
    PodSerializer::serialize(Cursor::new(Vec::new()), &props)
        .map(|(cursor, _len)| cursor.into_inner())
        .map_err(|e| format!("Could not serialize the live-update pod: {e:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use adtune_core::{BandType, FilterBand};

    #[test]
    fn live_pod_serializes_and_deserializes() {
        let profile = AudioProfile {
            name: "test".into(),
            bands: vec![FilterBand::new(BandType::Peaking, 1000.0, 3.0, 1.0)],
            ..Default::default()
        };
        let bytes = live_props_pod(&profile, &ToneSettings::default()).unwrap();
        assert!(!bytes.is_empty());

        // Round-trip through the deserializer: the pod must parse back into a
        // Props object whose params struct interleaves names and doubles
        // (1 pre_gain + 1 band + 3 tone shelves = 5 pairs).
        let (_rest, value) =
            pipewire::spa::pod::deserialize::PodDeserializer::deserialize_any_from(&bytes).unwrap();
        let Value::Object(obj) = value else {
            panic!("not an object")
        };
        assert_eq!(obj.type_, pipewire::spa::sys::SPA_TYPE_OBJECT_Props);
        let Value::Struct(fields) = &obj.properties[0].value else {
            panic!("params not a struct")
        };
        assert_eq!(fields.len(), 10);
        assert!(matches!(&fields[0], Value::String(s) if s == "pre_gain:Mult"));
        assert!(matches!(fields[1], Value::Double(_)));
    }
}
