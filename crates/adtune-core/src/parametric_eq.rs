//! Read and write AutoEq / `ParametricEQ.txt` bodies. This module owns *both*
//! directions of the format, so the writer and reader can be pinned together by
//! a single round-trip test.
//!
//! Lines look like:
//! ```text
//! Preamp: -6.3 dB
//! Filter 1: ON LSC Fc 105 Hz Gain 6.5 dB Q 0.70
//! ```

use crate::profile::{
    effective_bands, profile_preamp_db, AudioProfile, BandType, FilterBand, ToneSettings, MAX_BANDS,
};

/// Find `key` (case-insensitively) in a line's whitespace tokens and parse the
/// token immediately after it as an `f64`. This is how the `Fc`/`Gain`/`Q` values
/// are read out of a `Filter …` line regardless of token order or spacing.
fn value_after<'a>(tokens: &'a [&'a str], key: &str) -> Option<f64> {
    tokens
        .iter()
        .position(|&t| t.eq_ignore_ascii_case(key))
        .and_then(|i| tokens.get(i + 1))
        .and_then(|s| s.parse::<f64>().ok())
}

/// Parse the `Preamp:` value and `Filter …` bands from a ParametricEQ body.
/// Lenient: returns `(preamp_db, bands)` and never errors — an empty/preamp-only
/// config yields no bands (a valid flat/gain-only correction, e.g. for the APO).
pub fn parse_eq_bands(text: &str) -> (f64, Vec<FilterBand>) {
    let mut preamp = 0.0;
    let mut bands: Vec<FilterBand> = Vec::new();

    for line in text.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        match tokens.first() {
            Some(t) if t.eq_ignore_ascii_case("Preamp:") => {
                // NaN passes straight through clamp() (every comparison on NaN
                // is false), so non-finite values must be rejected explicitly:
                // this text can come from a user-writable file that the APO
                // parses inside the audio engine.
                if let Some(v) = tokens.get(1).and_then(|s| s.parse::<f64>().ok()) {
                    if v.is_finite() {
                        preamp = v.clamp(-24.0, 24.0);
                    }
                }
            }
            // Cap the band count while parsing (not after), so an enormous
            // config can't balloon memory before a final truncate.
            Some(t) if t.eq_ignore_ascii_case("Filter") && bands.len() < MAX_BANDS => {
                let on = tokens.iter().position(|&t| t.eq_ignore_ascii_case("ON"));
                let kind = on
                    .and_then(|i| tokens.get(i + 1))
                    .and_then(|s| BandType::parse(s));
                let (Some(kind), Some(fc), Some(gain), Some(q)) = (
                    kind,
                    value_after(&tokens, "Fc"),
                    value_after(&tokens, "Gain"),
                    value_after(&tokens, "Q"),
                ) else {
                    continue; // OFF filters, pass/notch types, or malformed lines
                };
                if !(fc.is_finite() && gain.is_finite() && q.is_finite()) {
                    continue; // NaN/inf would survive clamp() — drop the band
                }
                bands.push(FilterBand::new(
                    kind,
                    fc.clamp(20.0, 20000.0),
                    gain.clamp(-24.0, 24.0),
                    q.clamp(0.1, 20.0),
                ));
            }
            _ => {}
        }
    }
    (preamp, bands)
}

/// Parse a ParametricEQ body into a profile. `name` labels it. Requires at least
/// one filter band (used for user-facing imports).
pub fn parse_parametric_eq(text: &str, name: &str) -> Result<AudioProfile, String> {
    let (preamp, bands) = parse_eq_bands(text);
    // Accept a preamp-only body (a gain trim with no filters) so a profile edited
    // down to just a preamp still round-trips; only reject files with no EQ
    // content at all (no `Preamp:` and no `Filter` lines).
    let has_preamp = text
        .lines()
        .any(|l| l.trim_start().to_ascii_lowercase().starts_with("preamp:"));
    if bands.is_empty() && !has_preamp {
        return Err("No 'Filter N: ON PK/LSC/HSC …' lines were found in that file.".into());
    }
    let name = name.trim();
    Ok(AudioProfile {
        key: String::new(),
        name: if name.is_empty() {
            "Imported profile".into()
        } else {
            name.into()
        },
        detail: "Imported ParametricEQ".into(),
        source: String::new(),
        form: String::new(),
        preamp,
        bands,
    })
}

/// Render a profile's own correction (its bands + preamp, with no tone layer) to
/// ParametricEQ text — for **exporting/saving** a profile as a portable `.txt`
/// that any AutoEq-compatible tool (or ADtune's importer) can read back.
/// `\r\n` line endings and `.` decimals (locale-independent).
pub fn profile_to_parametric_eq(profile: &AudioProfile) -> String {
    let mut lines = vec![
        format!("# {}", profile.name.replace(['\r', '\n'], " ")),
        format!("Preamp: {:.1} dB", profile.preamp),
    ];
    for (i, band) in profile.bands.iter().enumerate() {
        lines.push(format!(
            "Filter {n}: ON {ty} Fc {fc:.0} Hz Gain {g:.1} dB Q {q:.2}",
            n = i + 1,
            ty = band.kind.eq_code(),
            fc = band.frequency,
            g = band.gain,
            q = band.q,
        ));
    }
    lines.join("\r\n") + "\r\n"
}

/// The ParametricEQ config body for a profile with tone applied — the format the
/// ADtune APO reads and live-reloads. Correction bands are scaled by the wet
/// amount; near-zero bands are omitted. Numbers use `.` decimals
/// (locale-independent) and `\r\n` line endings.
pub fn render_parametric_eq(profile: &AudioProfile, tone: &ToneSettings) -> String {
    let pregain = profile_preamp_db(profile, tone);
    let mut lines = vec![
        "# Generated by ADtune — overwritten when a profile is applied.".to_string(),
        format!("# Profile: {}", profile.name.replace(['\r', '\n'], " ")),
        format!("Preamp: {:.1} dB", pregain),
    ];
    let mut n = 1;
    for band in effective_bands(profile, tone) {
        if band.gain.abs() < 0.05 {
            continue;
        }
        lines.push(format!(
            "Filter {n}: ON {ty} Fc {fc:.0} Hz Gain {g:.1} dB Q {q:.2}",
            ty = band.kind.eq_code(),
            fc = band.frequency,
            g = band.gain,
            q = band.q,
        ));
        n += 1;
    }
    lines.join("\r\n") + "\r\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic, stable-Rust "poor man's fuzzer" for the config parser —
    /// the trust boundary that `audiodg.exe` reads across (a `Users`-writable
    /// `config.txt`). It builds tens of thousands of pseudo-random,
    /// adversarially-flavored strings from tokens that hit the interesting
    /// branches (`nan`, `inf`, `1e400`, huge/negative numbers, stray control and
    /// quote chars, partial `Filter` lines) and drives the EXACT path the APO
    /// uses: `parse_eq_bands` → `biquad_coeffs`. The invariant is that no input
    /// can panic or yield a non-finite coefficient / out-of-range value — so the
    /// worst a hostile config can do is sound wrong, never crash the audio
    /// engine. This is the cheap CI stand-in for a coverage-guided fuzzer
    /// (`cargo-fuzz`); it explores less but pins the safety property on stable.
    #[test]
    fn parser_survives_adversarial_random_input() {
        use crate::dsp::biquad_coeffs;

        // Deterministic PRNG (SplitMix64) so the test is reproducible.
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = seed;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        };

        const TOKENS: &[&str] = &[
            "Filter",
            "1:",
            "12:",
            "ON",
            "OFF",
            "PK",
            "LSC",
            "HSC",
            "LP",
            "Fc",
            "Hz",
            "Gain",
            "dB",
            "Q",
            "Preamp:",
            "nan",
            "NaN",
            "inf",
            "-inf",
            "1e400",
            "1e-400",
            "0",
            "-0",
            "1000",
            "999999999999",
            "-999999999999",
            "1.5",
            "-24",
            "24.0001",
            "48000",
            "0.0",
            "\n",
            "\r\n",
            " ",
            "\t",
            "{",
            "}",
            "\"",
            ";",
            "e",
            "E",
            "0x1p2",
            ".",
            "-",
            "+",
        ];
        const RATES: &[f64] = &[8000.0, 16000.0, 44100.0, 48000.0, 96000.0, 192000.0];

        for &rate in RATES {
            for _ in 0..12_000 {
                // Assemble a random token soup with random whitespace/newlines.
                let words = (next() % 16) as usize;
                let mut text = String::new();
                for _ in 0..words {
                    text.push_str(TOKENS[(next() as usize) % TOKENS.len()]);
                    text.push(if next() % 3 == 0 { '\n' } else { ' ' });
                }

                let (preamp, bands) = parse_eq_bands(&text);

                // Parser invariants: finite, in range, count-capped.
                assert!(
                    preamp.is_finite() && (-24.0..=24.0).contains(&preamp),
                    "preamp {preamp} from {text:?}"
                );
                assert!(
                    bands.len() <= MAX_BANDS,
                    "band count {} exceeds cap",
                    bands.len()
                );
                for b in &bands {
                    assert!((20.0..=20000.0).contains(&b.frequency));
                    assert!((-24.0..=24.0).contains(&b.gain));
                    assert!((0.1..=20.0).contains(&b.q));
                    // Coefficient invariant: the biquad the APO would run is
                    // always finite (no unstable pole, no NaN, no div-by-zero).
                    let c = biquad_coeffs(b, rate);
                    for v in [c.b0, c.b1, c.b2, c.a1, c.a2] {
                        assert!(
                            v.is_finite(),
                            "non-finite coeff {v} at rate {rate} from {text:?}"
                        );
                    }
                }
            }
        }
    }

    /// A small three-band fixture profile (the ATH-M50x correction) reused across
    /// the render/round-trip tests.
    fn m50x() -> AudioProfile {
        AudioProfile {
            name: "Audio-Technica ATH-M50x".into(),
            bands: vec![
                FilterBand::new(BandType::LowShelf, 110.0, -3.5, 1.0),
                FilterBand::new(BandType::Peaking, 2900.0, 2.7, 1.2),
                FilterBand::new(BandType::HighShelf, 9000.0, -4.4, 1.0),
            ],
            ..Default::default()
        }
    }

    /// A rendered config carries the header, a preamp line, and one contiguous
    /// `Filter` line per correction band in AutoEq syntax.
    #[test]
    fn full_config_has_preamp_and_filters() {
        let cfg = render_parametric_eq(&m50x(), &ToneSettings::default());
        assert!(cfg.starts_with("# Generated by ADtune"));
        assert!(cfg.contains("Preamp: "));
        assert!(cfg.contains("Filter 1: ON LSC Fc 110 Hz Gain -3.5 dB Q 1.00"));
        assert!(cfg.contains("ON PK Fc 2900 Hz Gain 2.7 dB Q 1.20"));
        assert!(cfg.contains("ON HSC Fc 9000 Hz Gain -4.4 dB Q 1.00"));
        assert!(cfg.contains("\r\n"), "uses CRLF line endings");
        // 3 correction bands, tone shelves are 0 dB and omitted
        assert_eq!(cfg.matches("Filter ").count(), 3);
    }

    /// Wet 0 (bypass) renders no correction filters and a 0 dB preamp — a flat pass.
    #[test]
    fn bypass_is_flat() {
        let cfg = render_parametric_eq(
            &m50x(),
            &ToneSettings {
                wet: 0.0,
                ..Default::default()
            },
        );
        // wet 0 -> no correction filters, preamp 0
        assert_eq!(cfg.matches("Filter ").count(), 0);
        assert!(cfg.contains("Preamp: 0.0 dB") || cfg.contains("Preamp: -0.0 dB"));
    }

    /// A non-neutral tone layer contributes its three shelves on top of the
    /// correction bands.
    #[test]
    fn tone_adds_shelves() {
        let cfg = render_parametric_eq(
            &m50x(),
            &ToneSettings {
                wet: 1.0,
                bass: 4.0,
                tilt: 3.0,
                headroom: true,
            },
        );
        // 3 correction + bass + tilt-lo + tilt-hi = 6 filters
        assert_eq!(cfg.matches("Filter ").count(), 6);
    }

    /// Rendered filters are numbered 1..=N with no gaps, regardless of the source
    /// profile's original numbering.
    #[test]
    fn filter_numbers_are_contiguous() {
        let cfg = render_parametric_eq(&m50x(), &ToneSettings::default());
        for expected in 1..=3 {
            assert!(
                cfg.contains(&format!("Filter {expected}: ON")),
                "missing filter {expected}"
            );
        }
    }

    /// Non-finite `Preamp`/`Fc`/`Gain`/`Q` values are dropped during parse, so only
    /// well-formed bands survive.
    #[test]
    fn non_finite_values_are_rejected() {
        let (preamp, bands) = parse_eq_bands(
            "Preamp: nan dB\n\
             Filter 1: ON PK Fc nan Hz Gain 3 dB Q 1.0\n\
             Filter 2: ON PK Fc 1000 Hz Gain inf dB Q 1.0\n\
             Filter 3: ON PK Fc 1000 Hz Gain 3 dB Q nan\n\
             Filter 4: ON PK Fc 1000 Hz Gain 3 dB Q 1.0\n",
        );
        // NaN survives clamp(), so it must be rejected explicitly.
        assert_eq!(preamp, 0.0);
        assert_eq!(bands.len(), 1, "only the well-formed band survives");
        assert!(bands[0].frequency.is_finite());
    }

    /// A config with more than `MAX_BANDS` filters is truncated to the cap while
    /// parsing.
    #[test]
    fn band_count_is_capped_during_parse() {
        let mut text = String::new();
        for i in 1..=100 {
            text.push_str(&format!("Filter {i}: ON PK Fc 1000 Hz Gain 1 dB Q 1.0\n"));
        }
        let (_, bands) = parse_eq_bands(&text);
        assert_eq!(bands.len(), 32);
    }

    /// Exporting a profile to `.txt` and importing it back preserves the preamp
    /// and every band within the format's rounding tolerance.
    #[test]
    fn profile_export_round_trips() {
        let mut profile = m50x();
        profile.preamp = -6.3;
        let text = profile_to_parametric_eq(&profile);
        let back = parse_parametric_eq(&text, "ATH-M50x").unwrap();
        assert!((back.preamp - -6.3).abs() < 1e-9);
        assert_eq!(back.bands.len(), profile.bands.len());
        for (got, exp) in back.bands.iter().zip(&profile.bands) {
            assert_eq!(got.kind, exp.kind);
            assert!((got.frequency - exp.frequency).abs() < 0.5);
            assert!((got.gain - exp.gain).abs() < 0.05);
            assert!((got.q - exp.q).abs() < 0.005);
        }
    }

    /// A preamp-only profile (no bands) round-trips through the `.txt` path, but a
    /// body with neither `Preamp` nor `Filter` lines is still rejected.
    #[test]
    fn preamp_only_profile_round_trips() {
        // A profile with every band removed (just a gain trim) must survive
        // export -> import via the .txt path.
        let profile = AudioProfile {
            name: "Trim only".into(),
            preamp: -4.0,
            ..Default::default()
        };
        let text = profile_to_parametric_eq(&profile);
        let back = parse_parametric_eq(&text, "Trim only").unwrap();
        assert!(back.bands.is_empty());
        assert!((back.preamp - -4.0).abs() < 1e-9);
        // A file with neither Preamp nor Filter lines is still rejected.
        assert!(parse_parametric_eq("just some random text\n", "x").is_err());
    }

    /// The rendered (tone-applied) config parses back to the same effective bands,
    /// pinning the writer and reader together.
    #[test]
    fn render_round_trips_through_parse() {
        let profile = m50x();
        let tone = ToneSettings::default();
        let text = render_parametric_eq(&profile, &tone);
        let (_preamp, bands) = parse_eq_bands(&text);
        // The three correction bands survive; neutral tone shelves are 0 dB, omitted.
        let expected: Vec<FilterBand> = effective_bands(&profile, &tone)
            .into_iter()
            .filter(|b| b.gain.abs() >= 0.05)
            .collect();
        assert_eq!(bands.len(), expected.len());
        for (got, exp) in bands.iter().zip(&expected) {
            assert_eq!(got.kind, exp.kind);
            assert!(
                (got.frequency - exp.frequency).abs() < 0.5,
                "Fc {} vs {}",
                got.frequency,
                exp.frequency
            );
            assert!(
                (got.gain - exp.gain).abs() < 0.05,
                "Gain {} vs {}",
                got.gain,
                exp.gain
            );
            assert!((got.q - exp.q).abs() < 0.005, "Q {} vs {}", got.q, exp.q);
        }
    }
}
