# ADtune — Linux packaging

Builds the self-contained `adtune` app plus the `adtune-service` calibration
daemon, and packages them for direct install, `.deb`, Flatpak, or Snap. The
device catalog is embedded in the executable, so a release is just the two
binaries plus a `.desktop` entry, an autostart entry, and an icon.

Unlike Windows (which loads ADtune's own APO), the Linux backend is a native
PipeWire client: `adtune-service` hosts PipeWire's filter-chain module
**in-process**, exposing the *ADtune Calibrated* virtual output. The app and
the daemon talk through two small JSON files (`desired.json` / `status.json`)
— no systemd, no host CLI tools — which is why the same architecture runs
unchanged in a `.deb`, a Flatpak, and a strictly confined Snap.

## Prerequisites

**Build:**
- [Rust](https://rustup.rs) (stable) — provides `cargo`.
- `libpipewire-0.3-dev` and `libclang-dev` (native PipeWire bindings):
  `sudo apt install libpipewire-0.3-dev libclang-dev`

**To build the `.deb`, also:**
- `dpkg-deb` (ships with `dpkg` on Debian/Ubuntu) and `fakeroot`
  (`sudo apt install fakeroot`).

**Runtime (on the target machine):**
- PipeWire + WirePlumber (`libpipewire-0.3-0` is a package dependency) —
  standard on modern Linux desktops.

## Option A — install from source

The simplest path (works on any distro). From the repository root:

```bash
./install.sh              # per-user  → ~/.local    (no sudo)
./install.sh --system     # system    → /usr/local  (uses sudo)
./install.sh --prefix=DIR # custom prefix
./install.sh --uninstall  # remove (honours --system / --prefix)
```

Installs `adtune` and `adtune-service` to `<prefix>/bin`, the `.desktop`
entry, the icon, and a login-autostart entry for the daemon (inert until you
first enable calibration — the daemon exits immediately when never
configured).

## Option B — build a Debian/Ubuntu `.deb`

```bash
./packaging/linux/build-deb.sh
```

Stages the package tree and builds (via `fakeroot dpkg-deb`):

```
dist/adtune_1.0.0_amd64.deb
```

(amd64 only; the version is read from the workspace `Cargo.toml`.) Install or
remove it with apt:

```bash
sudo apt install ./dist/adtune_1.0.0_amd64.deb   # pulls in pipewire if missing
sudo apt remove  adtune                           # keeps ~/.config/adtune
sudo apt purge   adtune                           # also removes settings + profiles
```

The package installs `/usr/bin/adtune`, `/usr/bin/adtune-service`, the
`.desktop` entry, the icon, AppStream metainfo, and the autostart entry.
`Depends` covers everything needed to function — `libpipewire-0.3-0` plus the
`pipewire` server and `wireplumber` session manager — so `apt install` pulls
them in when missing (`pipewire-pulse` is a Recommends). Its `postrm` stops
running daemons on remove (audio returns to normal) and on purge deletes
`~/.config/adtune` and the ephemeral `/run/user/<uid>/adtune`.

## Option C — Flatpak

```bash
flatpak-builder --user --install --force-clean build-dir \
    packaging/linux/flatpak/io.github.anidedej.ADtune.yml
flatpak run io.github.anidedej.ADtune
```

The only special permission is read access to the PipeWire socket
(`--filesystem=xdg-run/pipewire-0:ro`) — the same permission EasyEffects
uses. Login autostart goes through the Background portal, which the daemon
requests itself. For Flathub submission the manifest's `--share=network`
build-arg must be swapped for vendored cargo sources (see the note inside the
manifest).

## Option D — Snap (strict confinement)

```bash
snapcraft
sudo snap install --dangerous ./adtune_*.snap
sudo snap connect adtune:pipewire   # grants the native PipeWire socket
```

The `pipewire` interface is not auto-connected by default; until the store
approves auto-connection, users run the `snap connect` line once. The daemon
writes its own autostart entry (`autostart:` in `snap/snapcraft.yaml`), so
calibration survives logout/login.

## What gets installed

| Path | What |
|---|---|
| `<prefix>/bin/adtune` | the app (self-contained; catalog embedded) |
| `<prefix>/bin/adtune-service` | the calibration daemon (native PipeWire client) |
| `<prefix>/share/applications/io.github.anidedej.ADtune.desktop` | app-menu entry |
| `<prefix>/share/icons/hicolor/scalable/apps/io.github.anidedej.ADtune.svg` | icon |
| `/etc/xdg/autostart/io.github.anidedej.ADtune.Service.desktop` | daemon autostart (`~/.config/autostart` for user installs) |

Created per user at runtime (not owned by the package):

- `~/.config/adtune/desired.json` — what calibration should be (profile,
  target, tone, on/off); written by the app, reconciled by the daemon.
- `~/.config/adtune/profiles/` — saved profiles.
- `$XDG_RUNTIME_DIR/adtune/` — daemon status + single-instance lock
  (ephemeral).

Upgrading from ≤ 1.0 (the systemd architecture): on first run the daemon
stops and deletes the old `adtune.service` user unit and migrates
`state.json` into `desired.json` — calibration carries over without user
action.

## Uninstall

- Installed via `install.sh`: `./install.sh --uninstall` (add `--system` if you
  installed system-wide). Stops the daemon **and** deletes `~/.config/adtune`.
- Installed via `.deb`: `sudo apt remove adtune` (keeps your config), or
  `sudo apt purge adtune` to also wipe `~/.config/adtune`.

Either way the *ADtune Calibrated* output disappears and WirePlumber restores
your normal default sink.
