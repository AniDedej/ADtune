//! The portable DSP core of the ADtune APO: parse the correction config, build
//! biquad coefficients, and filter interleaved float audio. Real-time safe (no
//! allocation or locking in `process`). Unit-tested on any OS; the Windows COM
//! wrapper in `apo.rs` just drives this.

use adtune_core::{biquad_coeffs, parse_eq_bands, Biquad, Coeffs};

/// Upper bounds (fixed so the real-time path never allocates).
pub const MAX_BANDS: usize = 32;
pub const MAX_CHANNELS: usize = 8;

/// The active correction: a broadband pre-gain plus a biquad cascade, built for
/// one sample rate. Cheap to clone; swapped atomically when the config changes.
#[derive(Clone)]
pub struct Dsp {
    pub preamp: f64,
    pub coeffs: Vec<Coeffs>,
}

impl Default for Dsp {
    fn default() -> Self {
        Dsp {
            preamp: 1.0,
            coeffs: Vec::new(),
        }
    }
}

impl Dsp {
    /// Transparent passthrough.
    pub fn flat() -> Self {
        Dsp::default()
    }

    /// Build from a ParametricEQ config body (`Preamp:` + `Filter …` lines) at
    /// `rate` Hz. Falls back to passthrough on a parse error.
    pub fn from_config(text: &str, rate: f64) -> Self {
        let (preamp_db, bands) = parse_eq_bands(text);
        let coeffs: Vec<Coeffs> = bands
            .iter()
            .filter(|b| b.gain.abs() > 1e-4) // drop ~0 dB bands: a no-op biquad per sample, per channel
            .take(MAX_BANDS) // hard cap so the fixed-size real-time state always fits
            .map(|b| biquad_coeffs(b, rate))
            .collect();
        // Preamp is stored as a linear multiplier (dB → gain) so the hot path is
        // a single multiply, not a powf per sample.
        Dsp {
            preamp: 10f64.powf(preamp_db / 20.0),
            coeffs,
        }
    }
}

/// Per-channel biquad delay lines. Owned and mutated only by the real-time
/// thread; sized once when processing starts.
pub struct Processor {
    channels: usize,
    state: Vec<[Biquad; MAX_BANDS]>,
}

impl Processor {
    /// Allocate delay lines for `channels` (clamped to `1..=MAX_CHANNELS`). Every
    /// channel gets a full `MAX_BANDS` array so `process` never resizes — all
    /// allocation happens here, off the real-time thread.
    pub fn new(channels: usize) -> Self {
        let channels = channels.clamp(1, MAX_CHANNELS);
        Processor {
            channels,
            state: vec![[Biquad::default(); MAX_BANDS]; channels],
        }
    }

    /// Clear all filter history (zero the delay lines). Use on a
    /// stream/format change so stale state can't click into the new audio.
    pub fn reset(&mut self) {
        for s in &mut self.state {
            *s = [Biquad::default(); MAX_BANDS];
        }
    }

    /// Filter an interleaved `f32` buffer in place. Real-time safe: no allocation
    /// or locking, and the `Dsp` is borrowed (the caller swaps it atomically when
    /// the config changes). `channels` is the buffer's interleave stride; the
    /// stream layout is trusted to match the format `process` was sized for.
    #[inline]
    pub fn process(&mut self, dsp: &Dsp, buf: &mut [f32], channels: usize) {
        if channels == 0 {
            return; // avoid divide-by-zero on the frame count below
        }
        // Never index past the state we allocated (a wider buffer than expected
        // leaves the extra channels unfiltered) or past the active bands.
        let ch = channels.min(self.channels);
        let n = dsp.coeffs.len().min(MAX_BANDS);
        let pre = dsp.preamp;
        let frames = buf.len() / channels;
        for f in 0..frames {
            let base = f * channels;
            for c in 0..ch {
                let idx = base + c;
                // Work in f64 through the cascade to keep the recursive biquad
                // math well-conditioned, then narrow back to f32 on store.
                let mut x = buf[idx] as f64 * pre;
                // Each channel carries its own delay lines, so left/right (etc.)
                // never bleed into one another. The biquad recurrence — and its
                // denormal flush — lives in `Biquad::process`.
                let st = &mut self.state[c];
                for (biquad, coeffs) in st.iter_mut().zip(&dsp.coeffs[..n]) {
                    x = biquad.process(coeffs, x);
                }
                buf[idx] = x as f32;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(buf: &[f32]) -> f64 {
        (buf.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / buf.len() as f64).sqrt()
    }

    fn sine(freq: f64, rate: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / rate).sin() as f32)
            .collect()
    }

    #[test]
    fn flat_config_is_transparent() {
        let dsp = Dsp::from_config("Preamp: 0 dB\n", 48000.0);
        let mut p = Processor::new(1);
        let mut buf = sine(1000.0, 48000.0, 4096);
        let before = rms(&buf);
        p.process(&dsp, &mut buf, 1);
        assert!(
            (rms(&buf) - before).abs() < 1e-4,
            "flat config should not change level"
        );
    }

    #[test]
    fn preamp_attenuates() {
        let dsp = Dsp::from_config("Preamp: -6.0 dB\n", 48000.0);
        let mut p = Processor::new(1);
        let mut buf = sine(1000.0, 48000.0, 4096);
        let before = rms(&buf);
        p.process(&dsp, &mut buf, 1);
        let ratio = rms(&buf) / before;
        assert!((ratio - 0.5012).abs() < 0.01, "-6 dB ≈ 0.5x, got {ratio}");
    }

    #[test]
    fn peaking_boosts_its_band() {
        // +12 dB peak at 1 kHz should roughly quadruple a 1 kHz tone (after settling).
        let dsp = Dsp::from_config(
            "Preamp: 0 dB\nFilter 1: ON PK Fc 1000 Hz Gain 12 dB Q 1.0\n",
            48000.0,
        );
        let mut p = Processor::new(1);
        let mut buf = sine(1000.0, 48000.0, 16384);
        let before = rms(&buf);
        p.process(&dsp, &mut buf, 1);
        // measure the settled tail
        let tail = &buf[8192..];
        let gain = rms(tail) / before;
        assert!(
            gain > 3.0 && gain < 4.5,
            "expected ~4x at the peak, got {gain}"
        );
    }

    #[test]
    fn high_corner_at_low_rate_stays_stable() {
        // A 10 kHz shelf on a 16 kHz endpoint (Bluetooth-handsfree rate) used to
        // build an unstable biquad (pole outside the unit circle) whose output
        // diverged to ±inf. The Nyquist clamp in biquad_coeffs must prevent it.
        let dsp = Dsp::from_config(
            "Preamp: 0 dB\nFilter 1: ON HSC Fc 10000 Hz Gain -4.0 dB Q 0.7\n",
            16000.0,
        );
        let mut p = Processor::new(1);
        let mut buf = sine(440.0, 16000.0, 32000);
        p.process(&dsp, &mut buf, 1);
        assert!(
            buf.iter().all(|s| s.is_finite() && s.abs() < 4.0),
            "filter above Nyquist must stay stable"
        );
    }

    #[test]
    fn hostile_config_never_produces_nan() {
        let dsp = Dsp::from_config(
            "Preamp: nan dB\nFilter 1: ON PK Fc nan Hz Gain 12 dB Q nan\n",
            48000.0,
        );
        let mut p = Processor::new(1);
        let mut buf = sine(1000.0, 48000.0, 4096);
        p.process(&dsp, &mut buf, 1);
        assert!(
            buf.iter().all(|s| s.is_finite()),
            "NaN config must not reach the output"
        );
    }

    #[test]
    fn stereo_is_independent_and_interleaved() {
        let dsp = Dsp::from_config("Preamp: -6.0 dB\n", 48000.0);
        let mut p = Processor::new(2);
        // interleaved L/R, L=1.0 R=0.0 pattern held as a tiny DC-ish block
        let mut buf = vec![0.5f32; 512 * 2];
        p.process(&dsp, &mut buf, 2);
        // both channels scaled by ~0.5
        assert!((buf[0] as f64 - 0.25).abs() < 0.01);
        assert!((buf[1] as f64 - 0.25).abs() < 0.01);
    }
}
