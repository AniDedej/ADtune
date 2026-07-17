//! ADtune core: portable DSP, profiles, AutoEq catalog, and ParametricEQ
//! import — shared by every ADtune frontend, OS backend, and plugin.
//!
//! This crate is pure and platform-independent: no audio I/O, no OS calls.
//! Backends (PipeWire on Linux, an APO on Windows, a plugin host, …)
//! consume these types and turn them into real signal processing.

pub mod catalog;
pub mod dsp;
pub mod parametric_eq;
pub mod profile;
pub mod state;
pub mod store;

pub use catalog::Catalog;
pub use dsp::{
    biquad_coeffs, log_frequencies, max_positive_gain_db, response_curve, response_db, Biquad,
    Coeffs, GRAPH_RATE,
};
pub use parametric_eq::{
    parse_eq_bands, parse_parametric_eq, profile_to_parametric_eq, render_parametric_eq,
};
pub use profile::{
    effective_bands, profile_preamp_db, AudioProfile, BandType, FilterBand, ToneSettings,
};
pub use state::{profile_from_json, profile_to_json};
