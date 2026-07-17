//! Manual harness: connect natively and print the registry snapshot.
//!
//!     cargo run -p adtune-pipewire --example native-probe

use adtune_pipewire::{conn::Session, registry};

fn main() {
    let session = match Session::connect() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("connect failed: {e}");
            std::process::exit(1);
        }
    };
    match registry::snapshot(&session) {
        Ok(snap) => {
            println!("default sink: {:?}", snap.default_sink);
            for s in &snap.sinks {
                println!("sink #{:<4} {:<40} {}", s.id, s.name, s.description);
            }
        }
        Err(e) => {
            eprintln!("snapshot failed: {e}");
            std::process::exit(1);
        }
    }
}
