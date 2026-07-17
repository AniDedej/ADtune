//! Render the filter-chain graph description the daemon loads into PipeWire.
//!
//! Everything here is pure string rendering — no I/O — so the output can be
//! unit-tested and diffed. The args are SPA-JSON (PipeWire's relaxed JSON
//! dialect); the DSP shape (band scaling, tone shelves, preamp) is computed in
//! `adtune-core` and merely laid out into the graph here.

use adtune_core::{effective_bands, profile_preamp_db, AudioProfile, FilterBand, ToneSettings};

/// Display name of the virtual sink, shown in the OS output picker.
pub const VIRTUAL_SINK_NAME: &str = "ADtune Calibrated";

/// `node.name` of the virtual sink — the stable identifier the backend uses
/// to recognise ADtune's own node in the graph.
pub const VIRTUAL_SINK_PREFIX: &str = "effect_input.adtune";

/// Escape a value for embedding inside a double-quoted SPA-JSON string, and
/// flatten any control chars that would otherwise break the parser or span
/// lines.
///
/// Order is load-bearing: backslashes are doubled **first**, otherwise the
/// backslashes introduced when escaping `"` would themselves be doubled a
/// second time and corrupt the output. Quotes are escaped next, then newlines,
/// carriage returns, and tabs are replaced with spaces (SPA-JSON has no string
/// escape for those, and a literal one would terminate the value early).
pub fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// Named `(slot, band)` pairs in signal order: the PipeWire slot names zipped
/// onto [`adtune_core::effective_bands`] (correction bands scaled by wet, then
/// the three tone shelves) — so the scaling/order lives single-sourced in core.
pub fn graph_bands(profile: &AudioProfile, tone: &ToneSettings) -> Vec<(String, FilterBand)> {
    let slots = (0..profile.bands.len())
        .map(|i| format!("eq_band_{}", i + 1))
        .chain([
            "eq_bass".to_string(),
            "eq_tilt_lo".to_string(),
            "eq_tilt_hi".to_string(),
        ]);
    slots.zip(effective_bands(profile, tone)).collect()
}

/// The `module-filter-chain` args for a profile routed to `target_name`, as a
/// standalone SPA-JSON object — exactly what the daemon passes to
/// `pw_context_load_module` to host the filter in-process.
///
/// Builds a linear signal chain — `pre_gain` -> correction bands -> tone
/// shelves — inside one filter-chain node, exposing a virtual sink
/// (`effect_input.adtune`) that plays out to the physical `target_name`.
/// `target_name` is escaped because it is a device-supplied string dropped
/// into a quoted SPA-JSON field.
pub fn filter_chain_args(profile: &AudioProfile, target_name: &str, tone: &ToneSettings) -> String {
    // Broadband preamp (dB) -> linear multiplier for the `linear` builtin, which
    // scales by `Mult` rather than a dB gain. Keeps headroom so the EQ's boosted
    // bands don't clip.
    let pregain = profile_preamp_db(profile, tone);
    let mult = 10f64.powf(pregain / 20.0);

    // First node: the broadband gain stage. Remaining nodes are appended per
    // band below, and `order` tracks the names so the links can be chained.
    let mut nodes = vec![format!(
        "            {{\n\
        \x20               type = builtin\n\
        \x20               name = pre_gain\n\
        \x20               label = linear\n\
        \x20               control = {{ \"Mult\" = {mult:.5} \"Add\" = 0.0 }}\n\
        \x20           }}"
    )];
    let mut order = vec!["pre_gain".to_string()];
    // One biquad builtin per effective band, in signal order. `pw_label` maps
    // the band type to PipeWire's builtin name (bq_lowshelf / bq_peaking / …).
    for (slot, band) in graph_bands(profile, tone) {
        nodes.push(format!(
            "            {{\n\
            \x20               type = builtin\n\
            \x20               name = {slot}\n\
            \x20               label = {label}\n\
            \x20               control = {{ \"Freq\" = {freq:.1} \"Q\" = {q:.3} \"Gain\" = {gain:.3} }}\n\
            \x20           }}",
            label = band.kind.pw_label(),
            freq = band.frequency,
            q = band.q,
            gain = band.gain,
        ));
        order.push(slot);
    }
    // Chain the nodes head-to-tail: each adjacent pair (windows(2)) becomes one
    // link from the earlier node's `Out` port to the next node's `In` port.
    let links: Vec<String> = order
        .windows(2)
        .map(|w| {
            format!(
                "            {{ output = \"{}:Out\" input = \"{}:In\" }}",
                w[0], w[1]
            )
        })
        .collect();

    // `capture.props` defines the virtual sink apps play into (media.class
    // Audio/Sink, node.virtual); `playback.props` routes the filter output to
    // the physical device via `node.target`, marked `node.passive` so it
    // follows the target and does not itself become a sink.
    format!(
        r#"{{
    node.description = "{sink} — {name}"
    media.name = "{sink}"
    filter.graph = {{
        nodes = [
{nodes}
        ]
        links = [
{links}
        ]
    }}
    audio.channels = 2
    audio.position = [ FL FR ]
    capture.props = {{
        node.name = "{prefix}"
        media.class = Audio/Sink
        node.virtual = true
    }}
    playback.props = {{
        node.name = "effect_output.adtune"
        node.target = "{target}"
        node.passive = true
    }}
}}"#,
        sink = VIRTUAL_SINK_NAME,
        prefix = VIRTUAL_SINK_PREFIX,
        name = escape(&profile.name),
        target = escape(target_name),
        nodes = nodes.join("\n"),
        links = links.join("\n"),
    )
}

/// The full standalone config file wrapping [`filter_chain_args`] — the shape
/// versions ≤ 1.0 loaded via `pipewire -c`. Kept for inspection/diffing (the
/// `render_config` export); the daemon no longer writes it.
pub fn filter_config(profile: &AudioProfile, target_name: &str, tone: &ToneSettings) -> String {
    format!(
        r#"# Generated by ADtune. Changes are overwritten when a profile is applied.
context.properties = {{
    application.name = "ADtune Audio Calibration"
    remote.name = "pipewire-0"
}}

context.spa-libs = {{
    audio.convert.* = audioconvert/libspa-audioconvert
    support.* = support/libspa-support
}}

context.modules = [
    {{ name = libpipewire-module-rt flags = [ ifexists nofail ] }}
    {{ name = libpipewire-module-protocol-native }}
    {{ name = libpipewire-module-client-node }}
    {{ name = libpipewire-module-adapter }}
    {{
        name = libpipewire-module-filter-chain
        args = {args}
    }}
]
"#,
        args = filter_chain_args(profile, target_name, tone),
    )
}
