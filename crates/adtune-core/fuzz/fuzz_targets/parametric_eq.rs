#![no_main]
//! Fuzz the ParametricEQ text parser — the path the Windows APO uses to read
//! `%ProgramData%\ADtune\config.txt` inside audiodg.exe, a `Users`-writable file
//! and therefore a trust boundary. The property under test: NO byte sequence can
//! panic the parser or produce a non-finite / out-of-range coefficient, so a
//! hostile config can at worst sound wrong, never crash the audio engine.

use adtune_core::profile::MAX_BANDS;
use adtune_core::{biquad_coeffs, parse_eq_bands};
use libfuzzer_sys::fuzz_target;

// Common endpoint rates, including the low ones where an above-Nyquist corner
// would otherwise build an unstable (diverging) filter.
const RATES: [f64; 6] = [8000.0, 16000.0, 44100.0, 48000.0, 96000.0, 192000.0];

fuzz_target!(|data: &[u8]| {
    // config.txt is read as UTF-8 text; lossy-convert so every fuzzer byte still
    // reaches the parser (invalid sequences become U+FFFD).
    let text = String::from_utf8_lossy(data);
    let (preamp, bands) = parse_eq_bands(&text);

    // Invariants the audiodg trust boundary relies on.
    assert!(preamp.is_finite(), "preamp non-finite");
    assert!((-24.0..=24.0).contains(&preamp), "preamp out of range: {preamp}");
    assert!(bands.len() <= MAX_BANDS, "band cap exceeded: {}", bands.len());

    for b in &bands {
        assert!(b.frequency.is_finite() && (20.0..=20000.0).contains(&b.frequency), "fc {}", b.frequency);
        assert!(b.gain.is_finite() && (-24.0..=24.0).contains(&b.gain), "gain {}", b.gain);
        assert!(b.q.is_finite() && (0.1..=20.0).contains(&b.q), "q {}", b.q);
        for &rate in &RATES {
            let c = biquad_coeffs(b, rate);
            for v in [c.b0, c.b1, c.b2, c.a1, c.a2] {
                assert!(v.is_finite(), "non-finite coeff {v} at rate {rate}");
            }
        }
    }
});
