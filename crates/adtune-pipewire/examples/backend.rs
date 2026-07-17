//! Manual verification harness for the PipeWire backend against a live system.
//!   cargo run -p adtune-pipewire --example backend -- probe|config|apply <id>|bypass|unbypass|disable
//!
//! Not part of the library's automated tests — it drives the real daemon and
//! mutates the running audio graph, so it is meant to be run by hand on a
//! machine with PipeWire up.

use adtune_core::{AudioProfile, BandType, FilterBand, ToneSettings};
use adtune_pipewire::{OutputDevice, PipeWire};

/// A fixed sample profile (Audio-Technica ATH-M50x correction) so the harness
/// has something concrete to apply without depending on the bundled catalog.
fn m50x() -> AudioProfile {
    AudioProfile {
        key: "ath-m50x-rs".into(),
        name: "Audio-Technica ATH-M50x".into(),
        detail: "backend test".into(),
        bands: vec![
            FilterBand::new(BandType::LowShelf, 110.0, -3.5, 1.0),
            FilterBand::new(BandType::Peaking, 2900.0, 2.7, 1.2),
            FilterBand::new(BandType::HighShelf, 9000.0, -4.4, 1.0),
        ],
        ..Default::default()
    }
}

/// Re-resolve the persisted target name to a live [`OutputDevice`], so the
/// bypass/unbypass subcommands can act on whatever the last apply targeted.
fn target_from_state(pw: &PipeWire) -> Option<OutputDevice> {
    let name = pw.saved_target_name()?;
    pw.list_outputs()
        .ok()?
        .into_iter()
        .find(|o| o.node_name == name)
}

/// Dispatch on the first CLI argument; each arm exercises one backend entry
/// point and prints the outcome.
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let pw = PipeWire::new();
    match args.get(1).map(String::as_str) {
        // Read-only: report availability, status, and the physical outputs.
        Some("probe") => {
            println!("available: {}", PipeWire::available());
            let (a, m) = pw.status();
            println!("status: active={a}  {m}");
            match pw.list_outputs() {
                Ok(outs) => {
                    for o in outs {
                        let d = if o.is_default { " (default)" } else { "" };
                        println!(
                            "  {}: {}{}\n      {}",
                            o.node_id, o.description, d, o.node_name
                        );
                    }
                }
                Err(e) => eprintln!("list_outputs error: {e}"),
            }
        }
        // Render-only: dump the filter config to stdout for inspection, without
        // touching the system (uses a placeholder target name).
        Some("config") => {
            // Print the generated config via a dry apply target name.
            print!(
                "{}",
                adtune_pipewire::render_config(&m50x(), "SOME_TARGET", &ToneSettings::default())
            );
        }
        // Mutating: apply the sample profile to the output with the given id.
        Some("apply") => {
            let id: i64 = args
                .get(2)
                .and_then(|s| s.parse().ok())
                .expect("apply <output_id>");
            let target = pw
                .list_outputs()
                .unwrap()
                .into_iter()
                .find(|o| o.node_id == id)
                .expect("output id");
            match pw.apply(&m50x(), &target, &ToneSettings::default()) {
                Ok(()) => {
                    let (a, m) = pw.status();
                    println!("applied -> active={a} {m}");
                }
                Err(e) => {
                    eprintln!("apply error: {e}");
                    std::process::exit(1);
                }
            }
        }
        // Live tweak: flip the wet amount (0 = bypass, 1 = full) on the running
        // filter with no restart, reusing the persisted profile/target/tone.
        Some(cmd @ ("bypass" | "unbypass")) => {
            let wet = if cmd == "bypass" { 0.0 } else { 1.0 };
            let profile = pw.active_profile().expect("no active profile");
            let target = target_from_state(&pw).expect("no target");
            let tone = ToneSettings {
                wet,
                ..pw.active_tone()
            };
            match pw.update_live(&profile, &target, &tone) {
                Ok(live) => println!("{cmd}: update_live accepted={live}"),
                Err(e) => {
                    eprintln!("{cmd} error: {e}");
                    std::process::exit(1);
                }
            }
        }
        // Mutating: stop the service and restore the previous default output.
        Some("disable") => match pw.disable() {
            Ok(()) => println!("disabled; default restored to {:?}", pw.saved_target_name()),
            Err(e) => {
                eprintln!("disable error: {e}");
                std::process::exit(1);
            }
        },
        _ => eprintln!("usage: probe | config | apply <id> | bypass | unbypass | disable"),
    }
}
