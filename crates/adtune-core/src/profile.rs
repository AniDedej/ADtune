//! Profiles, bands, and the tone layer — the portable data model shared by
//! every ADtune frontend and OS backend.

/// A single biquad filter type. ADtune models the three shapes that the
/// AutoEq / ParametricEQ ecosystem uses (`PK`, `LSC`, `HSC`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BandType {
    LowShelf,
    Peaking,
    HighShelf,
}

impl BandType {
    /// Parse a band type from any of the common spellings (with or without the
    /// PipeWire `bq_` prefix); returns `None` for unsupported shapes.
    pub fn parse(s: &str) -> Option<BandType> {
        let lowered = s.trim().to_lowercase();
        let t = lowered.strip_prefix("bq_").unwrap_or(lowered.as_str());
        match t {
            "lowshelf" | "low_shelf" | "lsc" | "ls" => Some(BandType::LowShelf),
            "peaking" | "peak" | "pk" | "bell" => Some(BandType::Peaking),
            "highshelf" | "high_shelf" | "hsc" | "hs" => Some(BandType::HighShelf),
            _ => None,
        }
    }

    /// The short name used in ADtune JSON (`lowshelf` / `peaking` / `highshelf`).
    pub fn as_str(&self) -> &'static str {
        match self {
            BandType::LowShelf => "lowshelf",
            BandType::Peaking => "peaking",
            BandType::HighShelf => "highshelf",
        }
    }

    /// The PipeWire builtin label (`bq_lowshelf`, …).
    pub fn pw_label(&self) -> &'static str {
        match self {
            BandType::LowShelf => "bq_lowshelf",
            BandType::Peaking => "bq_peaking",
            BandType::HighShelf => "bq_highshelf",
        }
    }

    /// The ParametricEQ / AutoEq filter-type code (`LSC`/`PK`/`HSC`) — the exact
    /// inverse of [`BandType::parse`], used when generating ParametricEQ text.
    pub fn eq_code(&self) -> &'static str {
        match self {
            BandType::LowShelf => "LSC",
            BandType::Peaking => "PK",
            BandType::HighShelf => "HSC",
        }
    }
}

/// One correction band: a single biquad described by its shape, centre/corner
/// frequency, gain, and Q. Always built through [`FilterBand::new`] so the values
/// are sanitized before they reach the DSP.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilterBand {
    /// Filter shape (peaking / low-shelf / high-shelf).
    pub kind: BandType,
    /// Centre (peaking) or corner (shelf) frequency, in Hz.
    pub frequency: f64,
    /// Gain in dB (positive boosts, negative cuts).
    pub gain: f64,
    /// Quality factor (bandwidth for peaking, slope for shelves).
    pub q: f64,
}

impl FilterBand {
    /// Every entry path (bundled catalog, imported JSON, ParametricEQ text, UI
    /// edits) constructs bands through here, so sanitize on construction: a
    /// non-finite or out-of-range value must never reach the DSP — NaN survives
    /// `clamp()` (all comparisons on NaN are false), a zero Q divides by zero
    /// in the biquad math, and an oversized gain can build unstable filters.
    pub fn new(kind: BandType, frequency: f64, gain: f64, q: f64) -> Self {
        fn sane(v: f64, fallback: f64, lo: f64, hi: f64) -> f64 {
            if v.is_finite() {
                v.clamp(lo, hi)
            } else {
                fallback
            }
        }
        FilterBand {
            kind,
            frequency: sane(frequency, 1000.0, 20.0, 20000.0),
            gain: sane(gain, 0.0, -24.0, 24.0),
            q: sane(q, 1.0, 0.1, 20.0),
        }
    }

    /// This band with its gain scaled (used by the dry/wet control).
    pub fn scaled(&self, factor: f64) -> Self {
        FilterBand {
            gain: self.gain * factor,
            ..*self
        }
    }
}

#[cfg(test)]
mod band_tests {
    use super::*;

    /// [`FilterBand::new`] clamps out-of-range values and replaces non-finite ones
    /// with safe fallbacks, so nothing hostile reaches the DSP.
    #[test]
    fn filter_band_sanitizes_on_construction() {
        // NaN/inf and zero-Q must never reach the DSP (imported JSON is the
        // main path that would otherwise carry them through unclamped).
        let b = FilterBand::new(BandType::Peaking, f64::NAN, f64::INFINITY, 0.0);
        assert_eq!(b.frequency, 1000.0);
        assert_eq!(b.gain, 0.0);
        assert!((b.q - 0.1).abs() < 1e-12);
    }
}

/// A correction profile: a cascade of filter bands plus a broadband preamp.
/// Named for the bundled AutoEq headphone catalog it originated from, but the
/// correction applies to any output device (speaker, headphone, or other).
#[derive(Clone, Debug, Default)]
pub struct AudioProfile {
    /// Stable identifier (e.g. an AutoEq catalog key); empty for imports.
    pub key: String,
    /// Human-readable display name.
    pub name: String,
    /// One-line description shown in the UI (form factor, measurement source, …).
    pub detail: String,
    /// Measurement source/author, when known (attribution for AutoEq data).
    pub source: String,
    /// Form factor tag (e.g. `over-ear`), used to filter catalog searches.
    pub form: String,
    /// Broadband pre-gain baked into the profile (dB, usually negative).
    pub preamp: f64,
    /// The correction filter bands, in application order.
    pub bands: Vec<FilterBand>,
}

/// User adjustments layered on top of a profile's correction.
#[derive(Clone, Copy, Debug)]
pub struct ToneSettings {
    /// Correction amount, 0.0 (flat) .. 1.0 (full).
    pub wet: f64,
    /// Low-shelf trim in dB, -6 .. +6.
    pub bass: f64,
    /// Spectral tilt in dB, -6 .. +6 (bright when positive).
    pub tilt: f64,
    /// Auto Safe Headroom (attenuate to avoid clipping).
    pub headroom: bool,
}

impl Default for ToneSettings {
    /// Neutral defaults: full correction (`wet` 1.0), no bass/tilt trim, and Safe
    /// Headroom enabled — i.e. the profile's correction applied as measured.
    fn default() -> Self {
        ToneSettings {
            wet: 1.0,
            bass: 0.0,
            tilt: 0.0,
            headroom: true,
        }
    }
}

/// Upper bound on the number of bands a profile may carry, enforced at every
/// point a profile is built from external data (catalog, imported JSON,
/// ParametricEQ text). Real corrections use ~10 bands; the bundled catalog's
/// maximum is 10. The cap stops a crafted file with a multi-million-entry band
/// array from exploding the rendered config / response computation into an OOM,
/// and matches the real-time filter's fixed capacity in the APO.
pub const MAX_BANDS: usize = 32;

/// Truncate an untrusted display/identity string to a sane length, on a UTF-8
/// char boundary. Names and other text from a crafted profile file are
/// otherwise bounded only by the whole-file size cap; keeping each field short
/// stops a pathological multi-KB name from bloating a rendered config comment
/// or an on-disk library entry. 256 chars is far more than any real headphone
/// name needs.
pub fn sane_str(s: &str) -> String {
    const MAX: usize = 256;
    if s.len() <= MAX {
        s.to_string()
    } else {
        s.char_indices()
            .take_while(|(i, _)| *i < MAX)
            .map(|(_, c)| c)
            .collect()
    }
}

/// Clamp a broadband pre-gain to a finite, sane dB range. Every profile built
/// from external data (the catalog, imported JSON) runs its preamp through here
/// so a non-finite or absurd value from a crafted file can never reach a config
/// file or the DSP. Bands are sanitized separately in [`FilterBand::new`].
pub fn sane_preamp(db: f64) -> f64 {
    if db.is_finite() {
        db.clamp(-24.0, 24.0)
    } else {
        0.0
    }
}

/// Fixed tone-stage centre frequencies (Hz).
pub const BASS_FREQ: f64 = 105.0;
pub const TILT_FREQ: f64 = 1000.0;

/// The always-present tone shelves (transparent at neutral settings): a bass
/// low-shelf, plus a low/high-shelf pair at `TILT_FREQ` with opposite gains that
/// together pivot the spectrum around 1 kHz (brighter when `tilt` is positive).
pub fn tone_bands(tone: &ToneSettings) -> [FilterBand; 3] {
    [
        FilterBand::new(BandType::LowShelf, BASS_FREQ, tone.bass, 0.7),
        // Tilt is two mirrored shelves about TILT_FREQ: cut the lows and boost the
        // highs (or vice versa) so the level at the pivot stays put.
        FilterBand::new(BandType::LowShelf, TILT_FREQ, -tone.tilt, 0.5),
        FilterBand::new(BandType::HighShelf, TILT_FREQ, tone.tilt, 0.5),
    ]
}

/// Correction bands scaled by the wet amount, followed by the tone shelves —
/// exactly what the graph and the running filter represent (the broadband
/// preamp is applied separately, so the shape stays centred).
pub fn effective_bands(profile: &AudioProfile, tone: &ToneSettings) -> Vec<FilterBand> {
    let wet = tone.wet.clamp(0.0, 1.0);
    let mut bands: Vec<FilterBand> = profile.bands.iter().map(|b| b.scaled(wet)).collect();
    bands.extend_from_slice(&tone_bands(tone));
    bands
}

/// The broadband pre-gain (<= 0 dB) the filter chain will apply.
pub fn profile_preamp_db(profile: &AudioProfile, tone: &ToneSettings) -> f64 {
    if tone.headroom {
        -crate::dsp::max_positive_gain_db(&effective_bands(profile, tone), crate::dsp::GRAPH_RATE)
    } else {
        profile.preamp.min(0.0)
    }
}
