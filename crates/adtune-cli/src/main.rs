//! Cross-platform CLI demo for `adtune-core`: exercises the DSP, catalog, and
//! ParametricEQ import from the shared core — on any OS.

use adtune_core::{
    dsp, effective_bands, parse_parametric_eq, profile_preamp_db, AudioProfile, Catalog,
    ToneSettings,
};
use std::process::ExitCode;

/// The bundled catalog, or an override from `$ADTUNE_CATALOG`.
fn load_catalog() -> Result<Catalog, String> {
    if let Ok(p) = std::env::var("ADTUNE_CATALOG") {
        return Catalog::load(std::path::Path::new(&p))
            .map_err(|e| format!("could not load catalog: {e}"));
    }
    Ok(Catalog::bundled())
}

/// Print a profile's summary and an ASCII bar graph of its correction curve at
/// the default tone settings, sampled at a handful of representative octave ticks.
fn print_response(profile: &AudioProfile) {
    let tone = ToneSettings::default();
    let bands = effective_bands(profile, &tone);
    let preamp = profile_preamp_db(profile, &tone);
    println!(
        "{}  ({} bands, preamp {:+.1} dB)",
        profile.name,
        profile.bands.len(),
        preamp
    );
    let ticks = [
        30.0, 60.0, 120.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
    ];
    let resp = dsp::response_curve(&bands, &ticks, dsp::GRAPH_RATE);
    for (f, d) in ticks.iter().zip(resp.iter()) {
        // Shift the dB value up by 12 and clamp to [0, 24] so the bar length is a
        // non-negative column count centred on 0 dB (a cut still shows a short bar).
        let bar = "#".repeat(((d + 12.0).clamp(0.0, 24.0)) as usize);
        let hz = if *f >= 1000.0 {
            format!("{:>4}k", (*f as u32) / 1000)
        } else {
            format!("{:>5}", *f as u32)
        };
        println!("  {hz} Hz {d:+6.2} dB |{bar}");
    }
}

/// Print the subcommand usage help and return the conventional exit code 2 for
/// "invalid invocation".
fn usage() -> ExitCode {
    eprintln!(
        "adtune (core demo)\n\
         Usage:\n\
        \x20 adtune catalog <query>       search the bundled headphone catalog\n\
        \x20 adtune response <key>        show a profile's correction curve\n\
        \x20 adtune import <file>         parse a ParametricEQ.txt and show its curve\n\
        \x20 adtune count                 catalog size"
    );
    ExitCode::from(2)
}

/// Dispatch the first CLI argument to a subcommand. Returns `Err("")` (empty) to
/// signal "show usage", or `Err(msg)` for a real failure to report and exit non-zero.
fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("count") => {
            println!("{} profiles in catalog", load_catalog()?.len());
        }
        Some("catalog") => {
            let query = args.get(1).cloned().unwrap_or_default();
            let cat = load_catalog()?;
            let hits = cat.search(&query, "", 25);
            if hits.is_empty() {
                println!("No matches.");
            }
            for h in hits {
                println!("{}: {}  ·  {} · {}", h.key, h.name, h.form, h.source);
            }
        }
        Some("response") => {
            let key = args.get(1).ok_or("usage: adtune response <key>")?;
            let cat = load_catalog()?;
            let profile = cat
                .get(key)
                .ok_or_else(|| format!("unknown profile '{key}'"))?;
            print_response(profile);
        }
        Some("import") => {
            let file = args.get(1).ok_or("usage: adtune import <file>")?;
            let text = std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
            let stem = std::path::Path::new(file)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Imported");
            let profile = parse_parametric_eq(&text, stem)?;
            print_response(&profile);
        }
        _ => return Err(String::new()),
    }
    Ok(())
}

/// Map [`run`]'s result onto a process exit code: success, usage help for the
/// empty-error sentinel, or a printed error otherwise.
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.is_empty() => usage(),
        Err(e) => {
            eprintln!("adtune: {e}");
            ExitCode::FAILURE
        }
    }
}
