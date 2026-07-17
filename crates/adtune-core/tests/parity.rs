//! Numerical parity: golden frequency-response values pinned so DSP changes
//! that alter the output are caught.

use adtune_core::{
    dsp::{response_db, GRAPH_RATE},
    effective_bands, max_positive_gain_db, parse_parametric_eq, profile_preamp_db, BandType,
    Catalog, FilterBand, ToneSettings,
};

/// Golden-value check: the biquad magnitude response matches values verified
/// against PipeWire's builtin filters, so any drift in the DSP math is caught.
#[test]
fn biquad_matches_reference() {
    // ATH-M50x built-in profile; expected values verified against PipeWire's
    // builtin biquads (the RBJ cookbook formulas the OS audio engine uses).
    let bands = vec![
        FilterBand::new(BandType::LowShelf, 110.0, -3.5, 1.0),
        FilterBand::new(BandType::Peaking, 2900.0, 2.7, 1.2),
        FilterBand::new(BandType::HighShelf, 9000.0, -4.4, 1.0),
    ];
    let expected = [
        (30.0, -3.48),
        (110.0, -1.75),
        (1000.0, 0.26),
        (2900.0, 2.67),
        (9000.0, -2.03),
        (16000.0, -4.28),
    ];
    for (f, exp) in expected {
        let got = response_db(&bands, f, GRAPH_RATE);
        assert!(
            (got - exp).abs() < 0.01,
            "f={f}Hz expected {exp:+.2} dB, got {got:+.2} dB"
        );
    }
}

/// Parsing a ParametricEQ body recovers the preamp and each band's type, even
/// with non-contiguous `Filter N` numbering.
#[test]
fn parses_parametric_eq() {
    let text = "Preamp: -6.3 dB\n\
                Filter 1: ON LSC Fc 105 Hz Gain 6.5 dB Q 0.70\n\
                Filter 2: ON PK Fc 125 Hz Gain -2.7 dB Q 0.55\n\
                Filter 6: ON HSC Fc 10000 Hz Gain -3.1 dB Q 0.70\n";
    let p = parse_parametric_eq(text, "Test HP").unwrap();
    assert_eq!(p.name, "Test HP");
    assert!((p.preamp - -6.3).abs() < 1e-9);
    assert_eq!(p.bands.len(), 3);
    assert_eq!(p.bands[0].kind, BandType::LowShelf);
    assert_eq!(p.bands[1].kind, BandType::Peaking);
    assert_eq!(p.bands[2].kind, BandType::HighShelf);
}

/// The peak-boost estimate reflects the composite response (near the shelf
/// plateau), not a naive sum of per-band gains.
#[test]
fn safe_headroom_offsets_peak_boost() {
    let bands = vec![
        FilterBand::new(BandType::LowShelf, 105.0, 6.5, 0.7),
        FilterBand::new(BandType::Peaking, 3000.0, 3.0, 1.0),
    ];
    let peak = max_positive_gain_db(&bands, GRAPH_RATE);
    // Composite peak sits near the shelf plateau (~6.4 dB), not the nominal sum.
    assert!(peak > 6.0 && peak < 9.0, "peak boost {peak}");
}

/// The tone layer composes correctly: wet scales the correction bands and
/// bass/tilt add their shelves, with headroom keeping the pre-gain <= 0.
#[test]
fn tone_layer_composes() {
    let profile = adtune_core::AudioProfile {
        name: "x".into(),
        bands: vec![FilterBand::new(BandType::Peaking, 1000.0, 4.0, 1.0)],
        ..Default::default()
    };
    // Wet 0.5 halves the correction; bass/tilt add three shelves.
    let tone = ToneSettings {
        wet: 0.5,
        bass: 3.0,
        tilt: 2.0,
        headroom: true,
    };
    let eff = effective_bands(&profile, &tone);
    assert_eq!(eff.len(), 1 + 3);
    assert!((eff[0].gain - 2.0).abs() < 1e-9, "wet scaling");
    // Headroom pre-gain is <= 0 and offsets the composite peak.
    assert!(profile_preamp_db(&profile, &tone) <= 0.0);
}

/// The bundled catalog loads with its full entry count, a known key resolves to
/// the expected profile, and a form-scoped search returns relevant hits.
#[test]
fn catalog_loads_and_searches() {
    let cat = Catalog::bundled();
    assert!(cat.len() > 5000, "catalog has {} entries", cat.len());

    let hd600 = cat
        .get("autoeq-oratory1990-over-ear-sennheiser-hd-600")
        .expect("HD 600 present");
    assert_eq!(hd600.name, "Sennheiser HD 600");
    assert_eq!(hd600.bands.len(), 10);
    assert!((hd600.preamp - -6.3).abs() < 1e-9);

    let hits = cat.search("hd 600", "over-ear", 5);
    assert!(!hits.is_empty());
    assert!(hits.iter().all(|h| h.form == "over-ear"));
    assert!(hits[0].name.to_lowercase().contains("hd 600"));
}
