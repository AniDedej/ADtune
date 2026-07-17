# ADtune

[![adtune](https://snapcraft.io/adtune/badge.svg)](https://snapcraft.io/adtune)

**System-wide audio calibration for Linux and Windows.**

ADtune applies a parametric-EQ correction to *all* system audio, before it reaches whatever
you're listening on — headphones, desktop speakers, a USB DAC, an HDMI output. It ships with a
large catalog of measured headphone corrections to start from, but the correction engine is
device-agnostic: point it at any output, load or build a profile, and every application on the
machine is corrected in real time.

- 📈 **Live frequency-response graph** for every profile, updating as you edit it
- 🎧 **8,850 headphone corrections** built in — searchable by model, a ready-made starting point
- ✏️ **Build your own curve** for speakers or any device — drag bands directly on the graph
- 📥 Import **ParametricEQ** (`.txt`) and native **`.adtuneprofile`** files; export either
- 🔊 **Preamp / Safe Headroom** so boosts never clip
- 🎚️ **Tone controls** — correction amount, bass, tilt
- ⚡ **Level-matched A/B bypass** for an honest before/after comparison
- 🖥️ One app, native on **Linux** and **Windows** — no extra runtime to install

## How it works

ADtune is one portable Rust DSP core with a thin, native backend per platform:

- **Linux** — creates an *ADtune Calibrated* **PipeWire virtual output**. ADtune selects it as
  your system output on Apply, so every app is corrected before the sound reaches your device.
  A small per-user daemon (`adtune-service`) hosts the filter as a native PipeWire client — no
  systemd units, no shell-outs — so the same build runs from source, in a .deb, or in a strict Snap.
- **Windows** — installs **ADtune's own Audio Processing Object (APO)** into the Windows audio
  pipeline. The app writes the correction to a config file that the APO reads and *live-reloads*
  inside the audio engine — no reboot, no second program, no per-change admin prompt.

The DSP is a cascade of biquad filters evaluated by the OS audio engine itself, so it is
glitch-free and adds no measurable CPU. The correction data is embedded in the binary — no
network connection is ever required.

## Install

### Linux — Snap Store

[![Get it from the Snap Store](https://snapcraft.io/en/dark/install.svg)](https://snapcraft.io/adtune)

```bash
sudo snap install adtune
sudo snap connect adtune:pipewire   # one-time: allow access to your audio server
```

Also available from Ubuntu's **App Center** — search for *ADtune*, and after installing enable
**PipeWire** under *ADtune → Permissions* (same effect as the `snap connect` line; the app
shows a dialog walking you through it on first launch either way).

### Linux — from source

```bash
./install.sh              # per-user install to ~/.local (no sudo)
./install.sh --system     # system-wide to /usr/local (uses sudo)
./install.sh --uninstall  # remove it again
```

Requires **PipeWire + WirePlumber** at runtime and `libpipewire-0.3-dev` + `libclang-dev` to
build — standard on modern Linux desktops. The installer builds the app with `cargo`, installs
the `adtune` command, the calibration daemon, and a desktop entry, and checks your audio stack.
On the first Apply, ADtune creates the PipeWire virtual output and routes your system audio
through it.

### Windows

Run `ADtune-Setup.exe` (built via `packaging/windows/build-installer.ps1`; see
[packaging/windows/README.md](packaging/windows/README.md)). The installer registers ADtune's
APO as a loadable COM server; you then enable it on a chosen output from the app's **Calibration**
switch (a single UAC prompt), after which corrections apply live. Uninstalling removes the APO
from every device and restarts the audio engine cleanly.

> **Status.** The Windows backend has been validated on Windows 11 (24H2, in a VM): the APO loads
> into the audio engine, negotiates the shared-mode float format, and streams with live config
> reload. It has not yet been exercised across a wide range of real hardware — test on a spare
> output first. See the [packaging README](packaging/windows/README.md) for the current signing
> situation and Windows-specific notes.

## Build from source

```bash
cargo run -p adtune-ui                            # launch the app
cargo run -p adtune-cli -- count                  # portable core demo (any OS)
cargo test                                        # unit + parity tests
cargo run -p adtune-pipewire --example backend    # manual PipeWire probe/apply harness (Linux)
```

Toolchain: Rust (stable). The device catalog is embedded in the binary at build time, so the
resulting executable is self-contained.

Fuzz tests for the config/profile parsers live under `crates/adtune-core/fuzz/` (nightly +
`cargo-fuzz`); see that directory's README.

## Repository layout

| Crate | What it is |
|-------|-----------|
| `crates/adtune-core` | Portable core: biquad DSP, profiles, tone/headroom, the embedded catalog, ParametricEQ + JSON import/export |
| `crates/adtune-pipewire` | Linux backend: native PipeWire client + the `adtune-service` daemon hosting the filter-chain in-process |
| `crates/adtune-apo` | ADtune's own Windows APO — a self-contained system-wide EQ that loads into the audio engine |
| `crates/adtune-windows` | Windows control-plane: writes the APO config, enumerates devices, registers/enables the APO |
| `crates/adtune-ui` | Slint desktop app (the frontend for both platforms) |
| `crates/adtune-cli` | Command-line demo of the portable core |

## Profiles

The bundled catalog is generated from the open-source
[AutoEq](https://github.com/jaakkopasanen/AutoEq) measurement database (MIT-licensed); each
profile credits its individual measurement source — see [NOTICE](NOTICE). The profiles are a
starting point, not a limit: import your own `ParametricEQ.txt`, load/save native
`.adtuneprofile` files, or author a curve from scratch in the app for speakers or any other
output.

## Roadmap

Planned directions are tracked in [ROADMAP.md](ROADMAP.md).

## License

MIT — see [LICENSE](LICENSE). Copyright (c) 2026 Antonio DEDEJ.
</content>
