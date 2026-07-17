//! Turn a profile + tone into SVG path strings and axis labels for the Slint
//! `Path` element. Paths are generated in the plot's actual pixel coordinate
//! space (the viewbox is bound to the same size), so the curve stretches to
//! fill the plot at any window size instead of letterboxing.
//!
//! The pixel↔value mapping is exposed as public functions (with inverses) so the
//! draggable band handles reshape the profile in the exact same coordinate space
//! the curve is drawn in.

use adtune_core::{dsp, effective_bands, profile_preamp_db, AudioProfile, ToneSettings};

/// Minimum half-height of the plot in dB: the vertical axis runs from
/// `-range` at the bottom to `+range` at the top. The range widens to ±24
/// (via [`db_range`]) when a profile carries bands beyond ±12 dB, so extreme
/// catalog corrections (e.g. a +13 dB shelf paired with a −14 dB dip) show
/// their handles at their true values instead of clamped to the edge.
pub const DB_RANGE: f64 = 12.0;
/// The widened half-range, and the hard ceiling: band gains are clamped to
/// ±24 dB in `adtune-core`, so every band always fits this window.
pub const DB_RANGE_WIDE: f64 = 24.0;
/// Low edge of the log-frequency axis, in Hz.
pub const FMIN: f64 = 20.0;
/// High edge of the log-frequency axis, in Hz.
pub const FMAX: f64 = 20000.0;

/// The plot's half-range in dB for `profile`: ±12 normally, ±24 when any
/// band's gain exceeds 12 dB. Deterministic from the profile, so the curve,
/// the handles, and the drag math always agree without shared state.
pub fn db_range(profile: &AudioProfile) -> f64 {
    let peak = profile
        .bands
        .iter()
        .fold(0.0f64, |m, b| m.max(b.gain.abs()));
    if peak > DB_RANGE {
        DB_RANGE_WIDE
    } else {
        DB_RANGE
    }
}

/// Plot x-pixel for a frequency (log scale). `w` is floored like `build_graph`.
#[inline]
pub fn x_of(f: f64, w: f64) -> f64 {
    let w = w.max(50.0);
    let f = f.max(1e-3); // guard log10 against a non-positive frequency
    let (l0, l1) = (FMIN.log10(), FMAX.log10());
    (f.log10() - l0) / (l1 - l0) * w
}

/// Plot y-pixel for a gain in dB (0 dB centered, `+range` at top).
#[inline]
pub fn y_of(db: f64, h: f64, range: f64) -> f64 {
    (range - db) / (2.0 * range) * h.max(30.0)
}

/// Inverse of [`x_of`]: the frequency at plot x-pixel `px`.
#[inline]
pub fn freq_of_x(px: f64, w: f64) -> f64 {
    let w = w.max(50.0);
    let (l0, l1) = (FMIN.log10(), FMAX.log10());
    10f64.powf(l0 + (px / w).clamp(0.0, 1.0) * (l1 - l0))
}

/// Inverse of [`y_of`]: the gain in dB at plot y-pixel `py`.
#[inline]
pub fn db_of_y(py: f64, h: f64, range: f64) -> f64 {
    let h = h.max(30.0);
    range - (py / h).clamp(0.0, 1.0) * (2.0 * range)
}

/// Rendered geometry for one plot, all in the plot's pixel space. The path
/// fields are SVG path-data strings bound directly to Slint `Path` elements; the
/// label fields are positioned by fraction so they track the grid at any size.
pub struct GraphData {
    /// The response curve as an open polyline (`M`/`L` commands).
    pub curve: String,
    /// The curve closed down to the 0 dB line (`Z`), for the tinted area fill.
    pub fill: String,
    /// Frequency (vertical) and dB (horizontal) gridlines.
    pub grid: String,
    /// The 0 dB reference line, drawn separately so it can be emphasized.
    pub zero: String,
    /// `(text, 0..1 fraction across the plot)` for each frequency tick.
    pub x_labels: Vec<(String, f32)>,
    /// `(text, 0..1 fraction down the plot)` for each dB tick.
    pub y_labels: Vec<(String, f32)>,
    /// Profile name, shown as the plot title.
    pub title: String,
    /// One-line summary: source, correction-band count, and preamp gain.
    pub caption: String,
}

/// Build the graph geometry for a plot of `w` x `h` logical pixels. Samples the
/// combined band response across the log-frequency axis and emits SVG paths for
/// the curve, its area fill, the grid, the 0 dB line, and the axis-tick labels.
pub fn build_graph(profile: &AudioProfile, tone: &ToneSettings, w: f64, h: f64) -> GraphData {
    let w = w.max(50.0);
    let h = h.max(30.0);
    let lx0 = FMIN.log10();
    let lx1 = FMAX.log10();

    let range = db_range(profile);
    let bands = effective_bands(profile, tone);
    let n = (w as usize / 3).clamp(120, 600);
    let freqs = dsp::log_frequencies(n, FMIN, FMAX);
    let resp = dsp::response_curve(&bands, &freqs, dsp::GRAPH_RATE);
    let clamp = |d: f64| d.clamp(-range, range);

    let mut curve = String::new();
    for (i, (f, d)) in freqs.iter().zip(&resp).enumerate() {
        let cmd = if i == 0 { "M" } else { "L" };
        curve.push_str(&format!(
            "{cmd} {:.1} {:.1} ",
            x_of(*f, w),
            y_of(clamp(*d), h, range)
        ));
    }

    let mut fill = format!("M {:.1} {:.1} ", x_of(freqs[0], w), y_of(0.0, h, range));
    for (f, d) in freqs.iter().zip(&resp) {
        fill.push_str(&format!(
            "L {:.1} {:.1} ",
            x_of(*f, w),
            y_of(clamp(*d), h, range)
        ));
    }
    fill.push_str(&format!(
        "L {:.1} {:.1} Z",
        x_of(*freqs.last().unwrap(), w),
        y_of(0.0, h, range)
    ));

    let ticks = [
        20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0,
    ];
    let mut grid = String::new();
    for f in ticks {
        let x = x_of(f, w);
        grid.push_str(&format!("M {x:.1} 0 L {x:.1} {h:.1} "));
    }
    let mut db = -(range as i32);
    while db <= range as i32 {
        if db != 0 {
            let y = y_of(db as f64, h, range);
            grid.push_str(&format!("M 0 {y:.1} L {w:.1} {y:.1} "));
        }
        db += 3;
    }

    let zero = format!("M 0 {y:.1} L {w:.1} {y:.1}", y = y_of(0.0, h, range));

    // Labels are positioned in Slint as a fraction of the plot rect, so they
    // stay aligned with the grid regardless of pixel size.
    let x_labels = ticks
        .iter()
        .map(|f| {
            let t = if *f >= 1000.0 {
                format!("{}k", (*f as i32) / 1000)
            } else {
                format!("{}", *f as i32)
            };
            ((t), ((f.log10() - lx0) / (lx1 - lx0)) as f32)
        })
        .collect();

    let mut y_labels = Vec::new();
    let mut db = -(range as i32);
    while db <= range as i32 {
        let t = if db == 0 {
            "0 dB".to_string()
        } else {
            format!("{db:+}")
        };
        y_labels.push((t, ((range - db as f64) / (2.0 * range)) as f32));
        db += 6;
    }

    let preamp = profile_preamp_db(profile, tone) + 0.0; // +0.0 normalizes -0.0
    let src = if profile.source.is_empty() {
        String::new()
    } else {
        format!(" · measured by {}", profile.source)
    };
    GraphData {
        curve,
        fill,
        grid,
        zero,
        x_labels,
        y_labels,
        title: profile.name.clone(),
        caption: format!(
            "{}{src} — {} correction bands · preamp {preamp:+.1} dB",
            profile.name,
            profile.bands.len()
        ),
    }
}
