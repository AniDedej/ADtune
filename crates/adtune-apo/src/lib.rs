//! ADtune's own Windows Audio Processing Object (APO): a system-wide correction
//! EQ for any output device that plugs directly into the Windows audio pipeline,
//! so users don't need to install any separate program.
//!
//! - [`processor`] — the portable, unit-tested DSP (config → biquads → filtering).
//! - `apo` (Windows only) — the COM object implementing the APO interfaces,
//!   the class factory, the DLL exports, and self-registration. It only drives
//!   [`processor`], so the audio math is identical to (and shares code with) the
//!   Linux backend and the on-screen graph.
//!
//! Built entirely from Microsoft's documented APO model and ADtune's own DSP,
//! so ADtune stays MIT.

// LNK4104: MSVC warns that the COM entry points DllGetClassObject/DllCanUnloadNow
// "should be PRIVATE". That only concerns the DLL's import library, which nothing
// links against for a COM in-process server (it's loaded via CoCreateInstance).
// Rust can't mark cdylib exports PRIVATE (rust-lang/rust#98449), so silence the
// benign linker warning. `unknown_lints` guards toolchains predating the lint.
#![cfg_attr(windows, allow(unknown_lints, linker_messages))]

pub mod processor;

#[cfg(windows)]
mod apo;
