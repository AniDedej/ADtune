#![no_main]
//! Fuzz the JSON profile parser — the path used to load `.adtuneprofile` files a
//! user may import from anywhere (downloaded, shared). The property under test:
//! a successfully parsed profile is ALWAYS sanitized (finite, in range, band
//! count capped, strings bounded), and rendering it to config text never panics.
//! So a hostile profile file can at worst produce a wrong EQ, never corrupt
//! state or crash the app.

use adtune_core::profile::MAX_BANDS;
use adtune_core::{
    biquad_coeffs, profile_from_json, profile_to_parametric_eq, render_parametric_eq, ToneSettings,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    // Most inputs won't be valid JSON; we only assert invariants on the ones the
    // parser accepts (the interesting, security-relevant case).
    let Ok(profile) = profile_from_json(&text) else {
        return;
    };

    // A parsed profile must already be sanitized by `to_profile`.
    assert!(profile.preamp.is_finite() && (-24.0..=24.0).contains(&profile.preamp), "preamp {}", profile.preamp);
    assert!(profile.bands.len() <= MAX_BANDS, "band cap exceeded: {}", profile.bands.len());
    for b in &profile.bands {
        assert!(b.frequency.is_finite() && (20.0..=20000.0).contains(&b.frequency));
        assert!(b.gain.is_finite() && (-24.0..=24.0).contains(&b.gain));
        assert!(b.q.is_finite() && (0.1..=20.0).contains(&b.q));
        let c = biquad_coeffs(b, 48000.0);
        for v in [c.b0, c.b1, c.b2, c.a1, c.a2] {
            assert!(v.is_finite(), "non-finite coeff {v}");
        }
    }
    // Every untrusted string field must be length-bounded (≤ 256 chars).
    for s in [&profile.key, &profile.name, &profile.detail, &profile.source, &profile.form] {
        assert!(s.chars().count() <= 256, "unbounded profile string: {} chars", s.chars().count());
    }

    // Rendering the parsed profile (the route to config.txt / the PipeWire
    // config) must not panic; the name's newlines are stripped so it cannot
    // inject extra config lines.
    let tone = ToneSettings::default();
    let _ = render_parametric_eq(&profile, &tone);
    let _ = profile_to_parametric_eq(&profile);
});
