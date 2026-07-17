//! Biquad magnitude response (RBJ cookbook), matching PipeWire's builtin
//! `bq_lowshelf` / `bq_peaking` / `bq_highshelf` so the drawn curve is exactly
//! what gets applied. Pure `f64`, no external math crates.

use crate::profile::{BandType, FilterBand};
use std::f64::consts::PI;

/// Default sample rate used for drawing the response curve. The audible shape
/// (20 Hz–20 kHz) is effectively identical at 44.1/48 kHz.
pub const GRAPH_RATE: f64 = 48000.0;

/// Normalised biquad coefficients (a0 = 1). Shared by the response graph and
/// the real-time filter so the drawn curve is exactly what is applied.
#[derive(Clone, Copy, Debug, Default)]
pub struct Coeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

/// A running direct-form-I biquad, for real-time filtering. Carries the two
/// previous input and output samples (the filter's delay-line state) between
/// calls to [`Biquad::process`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Biquad {
    // Previous two input samples (x[n-1], x[n-2]).
    x1: f64,
    x2: f64,
    // Previous two output samples (y[n-1], y[n-2]).
    y1: f64,
    y2: f64,
}

impl Biquad {
    /// Process one sample. Real-time safe: no allocation or locking.
    #[inline]
    pub fn process(&mut self, c: &Coeffs, x: f64) -> f64 {
        let mut y = c.b0 * x + c.b1 * self.x1 + c.b2 * self.x2 - c.a1 * self.y1 - c.a2 * self.y2;
        // Flush tiny values to zero: once the input goes silent the feedback
        // terms decay into subnormal range, where every operation takes a
        // microcode assist — a classic audio-thread CPU spike. 1e-30 is far
        // below audibility (~ -600 dBFS) yet well above f64 subnormals.
        if y.abs() < 1e-30 {
            y = 0.0;
        }
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// The normalised biquad coefficients for a band at a given sample rate,
/// following the RBJ cookbook so the result matches PipeWire's builtin filters.
pub fn biquad_coeffs(band: &FilterBand, rate: f64) -> Coeffs {
    // RBJ's `A`: the linear amplitude whose square is the linear gain. Gain is in
    // dB, and shelf/peak formulas below are written in terms of A and sqrt(A).
    let a = 10f64.powf(band.gain / 40.0);
    // Keep the corner frequency below Nyquist. For w0 > π, sin(w0) goes
    // negative, flipping alpha's sign and pushing a pole outside the unit
    // circle — an unstable filter whose output diverges to ±inf (full-scale
    // noise) while the drawn magnitude response still looks normal. This is
    // reachable with perfectly legitimate configs on low-rate endpoints, e.g.
    // a 10 kHz shelf on a 16 kHz Bluetooth-handsfree link.
    let f = band.frequency.min((0.49 * rate).max(1.0)).max(1.0);
    let w0 = 2.0 * PI * f / rate;
    let (cw, sw) = (w0.cos(), w0.sin());
    let (b0, b1, b2, a0, a1, a2) = match band.kind {
        BandType::Peaking => {
            // Bell filter: bandwidth is set directly by Q via alpha.
            let alpha = sw / (2.0 * band.q);
            (
                1.0 + alpha * a,
                -2.0 * cw,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cw,
                1.0 - alpha / a,
            )
        }
        BandType::LowShelf => {
            // Shelf slope factor. `arg` is the term under RBJ's sqrt; for low Q it
            // can go negative (an impossibly steep shelf), so guard it and fall
            // back to alpha = 0 rather than take the sqrt of a negative and get NaN.
            let arg = (a + 1.0 / a) * (1.0 / band.q - 1.0) + 2.0;
            let alpha = if arg > 0.0 {
                sw / 2.0 * arg.sqrt()
            } else {
                0.0
            };
            // `tsa` = 2·sqrt(A)·alpha, the recurring cross term in the shelf coeffs.
            let tsa = 2.0 * a.sqrt() * alpha;
            (
                a * ((a + 1.0) - (a - 1.0) * cw + tsa),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cw),
                a * ((a + 1.0) - (a - 1.0) * cw - tsa),
                (a + 1.0) + (a - 1.0) * cw + tsa,
                -2.0 * ((a - 1.0) + (a + 1.0) * cw),
                (a + 1.0) + (a - 1.0) * cw - tsa,
            )
        }
        BandType::HighShelf => {
            // Mirror of the low shelf (same slope guard); the coefficient signs
            // below are flipped so the boost/cut sits above the corner instead.
            let arg = (a + 1.0 / a) * (1.0 / band.q - 1.0) + 2.0;
            let alpha = if arg > 0.0 {
                sw / 2.0 * arg.sqrt()
            } else {
                0.0
            };
            let tsa = 2.0 * a.sqrt() * alpha;
            (
                a * ((a + 1.0) + (a - 1.0) * cw + tsa),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cw),
                a * ((a + 1.0) + (a - 1.0) * cw - tsa),
                (a + 1.0) - (a - 1.0) * cw + tsa,
                2.0 * ((a - 1.0) - (a + 1.0) * cw),
                (a + 1.0) - (a - 1.0) * cw - tsa,
            )
        }
    };
    // Normalise so a0 = 1 (the form the running filter and the graph both expect).
    Coeffs {
        b0: b0 / a0,
        b1: b1 / a0,
        b2: b2 / a0,
        a1: a1 / a0,
        a2: a2 / a0,
    }
}

/// |H(e^jw)| of one normalised biquad at angular frequency `w`. Evaluates the
/// transfer function on the unit circle and returns the magnitude ratio (linear).
fn magnitude(c: &Coeffs, w: f64) -> f64 {
    let (c1, s1) = (w.cos(), w.sin());
    let (c2, s2) = ((2.0 * w).cos(), (2.0 * w).sin());
    let n_re = c.b0 + c.b1 * c1 + c.b2 * c2;
    let n_im = -(c.b1 * s1 + c.b2 * s2);
    let d_re = 1.0 + c.a1 * c1 + c.a2 * c2;
    let d_im = -(c.a1 * s1 + c.a2 * s2);
    ((n_re * n_re + n_im * n_im) / (d_re * d_re + d_im * d_im)).sqrt()
}

/// Convert a linear magnitude ratio to dB. A non-positive magnitude has no real
/// dB value, so floor it at -120 dB (effectively silence) instead of -inf/NaN.
fn to_db(mag: f64) -> f64 {
    if mag > 0.0 {
        20.0 * mag.log10()
    } else {
        -120.0
    }
}

/// Summed magnitude response (dB) of a band cascade at one frequency.
pub fn response_db(bands: &[FilterBand], freq: f64, rate: f64) -> f64 {
    let w = 2.0 * PI * freq / rate;
    let mut mag = 1.0;
    for band in bands {
        // A 0 dB band is unity gain everywhere; skip it to avoid pointless work
        // (and to keep the product clean for the near-flat tone shelves).
        if band.gain.abs() < 1e-6 {
            continue;
        }
        // Cascade of biquads = product of magnitudes.
        mag *= magnitude(&biquad_coeffs(band, rate), w);
    }
    to_db(mag)
}

/// Summed magnitude response (dB) at each of `freqs`. Precomputes each band's
/// coefficients once (rather than per frequency as [`response_db`] does), so
/// drawing a whole curve stays cheap.
pub fn response_curve(bands: &[FilterBand], freqs: &[f64], rate: f64) -> Vec<f64> {
    // Coefficients depend only on the band and rate, not the frequency, so hoist
    // them out of the per-frequency loop; drop 0 dB bands (unity, no contribution).
    let cs: Vec<Coeffs> = bands
        .iter()
        .filter(|b| b.gain.abs() >= 1e-6)
        .map(|b| biquad_coeffs(b, rate))
        .collect();
    freqs
        .iter()
        .map(|&f| {
            let w = 2.0 * PI * f / rate;
            let mut mag = 1.0;
            for c in &cs {
                mag *= magnitude(c, w);
            }
            to_db(mag)
        })
        .collect()
}

/// `n` log-spaced frequencies over `[fmin, fmax]`.
pub fn log_frequencies(n: usize, fmin: f64, fmax: f64) -> Vec<f64> {
    let (lx0, lx1) = (fmin.log10(), fmax.log10());
    (0..n)
        .map(|i| 10f64.powf(lx0 + (lx1 - lx0) * (i as f64) / ((n - 1).max(1) as f64)))
        .collect()
}

/// Peak boost (dB) of the cascade — the amount to attenuate for headroom. Found
/// by sampling the composite response across the audio band and taking the max;
/// negated by the caller to derive the Safe Headroom pre-gain.
pub fn max_positive_gain_db(bands: &[FilterBand], rate: f64) -> f64 {
    if bands.is_empty() {
        return 0.0;
    }
    // 512 log-spaced points over 20 Hz–20 kHz resolves even a narrow peak well
    // enough for a headroom estimate. `fold(0.0, max)` floors the result at 0, so
    // a purely attenuating correction needs no headroom.
    response_curve(bands, &log_frequencies(512, 20.0, 20000.0), rate)
        .into_iter()
        .fold(0.0, f64::max)
}
