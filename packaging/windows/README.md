# ADtune — Windows installer

Builds a self-contained `ADtune-Setup-<version>.exe` that installs ADtune **and
its own Audio Processing Object (APO)** — nothing else is needed.
The device catalog is embedded in the executable, so it ships just
`adtune.exe`, `adtune_apo.dll`, and shortcuts.

## Prerequisites

- [Rust](https://rustup.rs) (MSVC toolchain — the default on Windows)
- [Inno Setup 6](https://jrsoftware.org/isdl.php)

## Build

From a PowerShell prompt at the repository root:

```powershell
.\packaging\windows\build-installer.ps1
```

This runs `cargo build --release -p adtune-ui -p adtune-apo`, then Inno Setup,
producing `target\installer\ADtune-Setup-<version>.exe` (the version comes from
the workspace `Cargo.toml`). Release builds ship **unsigned**;
pass `-SelfSign` to additionally self-sign the binaries and installer for local
testing (this trusts a self-signed cert machine-wide — never use it on a machine
you don't own, and never distribute the result).

## What the installer does (elevated)

1. Installs `adtune.exe` + `adtune_apo.dll` to Program Files.
2. Registers the APO's COM class and its `AudioProcessingObjects` entry — this
   makes the APO **loadable**, but attaches it to no device.
3. Sets `DisableProtectedAudioDG=1` so the (currently unsigned) APO can load.

Enabling the APO on an output is done **in the app**, not by the installer: the
**Calibration** switch registers the APO on your selected device (behind a UAC
prompt) and reloads the audio engine so it takes effect. ADtune then writes
corrections to `%ProgramData%\ADtune\config.txt`, which the APO reads and
**live-reloads** — no reboot, no second program.

Uninstalling closes a running ADtune, removes the APO from every device,
restarts the audio engine, and deletes `%ProgramData%\ADtune`. It asks (a
Yes/No prompt, defaulting to No) whether to also delete your saved profiles
(`%APPDATA%\ADtune`), which are otherwise kept for a future installation.

## Signing (recommended before distributing)

The APO currently relies on `DisableProtectedAudioDG` because the DLL is unsigned. However, Windows 11 with Hypervisor-Enforced Code Integrity (HVCI/Memory Integrity) enabled may block all unsigned DLLs in `audiodg.exe` regardless of that registry setting.

To test locally without disabling security protections, you can self-sign the built DLL:
1. Open PowerShell as Administrator.
2. Run the signing script:
   ```powershell
   .\packaging\windows\sign-apo.ps1
   ```
This script creates a local self-signed certificate, adds it to the machine's trusted root and publisher stores, and signs `adtune_apo.dll` with it.

> **Status — validated on Windows 11 (24H2, VM).** The APO loads into
> `audiodg.exe`, negotiates the shared-mode float format, and streams through
> `LockForProcess`/`APOProcess` with live config reload. Not yet exercised on a
> wide range of real hardware — still test on a spare output first.

## Windows quirks the app handles

- **"Audio enhancements: Off"** (Settings → Sound → device Properties) writes
  `PKEY_AudioEndpoint_Disable_SysFx = 1` on the endpoint, which blocks *every*
  APO from loading, ADtune's included. The Calibration enable flow detects this,
  remembers the user's choice, and switches the endpoint back to "Device Default
  Effects" (deleting the value, exactly what the Settings page writes); disable
  restores the original state. The status line calls out the condition when it
  appears while calibration is on.
- **`APOInterface0` must be `{FD7F2B29-24D0-4B5C-B177-592C39F9CA10}`**
  (`IAudioProcessingObject`). This is the engine's metadata for the interface it
  drives the APO through; declaring the `IAudioSystemEffects` family there makes
  the engine probe the APO and then abandon the endpoint graph without audio.
  Every in-box Windows system-effect APO declares exactly this value, with
  `Flags = 0xD`.

## Security posture

The APO's DSP core and its COM/real-time internals are memory-safe and the file
parsers are hardened against hostile input (see below). The remaining risks are
**deployment-layer** and gate a public release:

- **The DLL is unsigned**, so the installer sets machine-wide
  `DisableProtectedAudioDG=1` to let it load into `audiodg.exe`. This weakens a
  Windows security boundary for the whole machine. **Before distributing,
  Authenticode-sign `adtune_apo.dll` with a real CA certificate and drop that
  registry switch entirely.** `sign-apo.ps1` is a local-testing shortcut only —
  it trusts a self-signed cert machine-wide and must never run on machines you
  don't own.
- **`%ProgramData%\ADtune\config.txt` is the trust boundary.** The unprivileged
  app writes it and `audiodg.exe` reads it, so the directory is `Users`-writable
  (narrowed from Everyone). The file only ever carries clamped EQ parameters:
  `adtune-core`'s `parse_eq_bands`/`biquad_coeffs` reject non-finite values,
  clamp frequency/gain/Q, cap the band count, and clamp the corner frequency
  below Nyquist. Treat those functions as a **security control** — fuzz them and
  keep them strict when changing the parser.

Untrusted-input hardening already in place (profiles, state, catalog): all band
and preamp values are finite-checked and clamped on construction; band counts
are capped (`MAX_BANDS`); profile/state reads and the gzip catalog are
size-capped against memory-exhaustion (decompression bombs / oversized files);
endpoint ids are charset-validated before they reach an elevated command line or
registry path. No command injection, config-syntax breakout, or path traversal
was found in either OS backend.
