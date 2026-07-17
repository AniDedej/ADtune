#!/usr/bin/env bash
#
# ADtune installer for Linux. Builds the app with cargo and installs it.
#
#   ./install.sh                 # per-user install to ~/.local  (no sudo)
#   ./install.sh --system        # system-wide to /usr/local     (uses sudo)
#   ./install.sh --prefix=DIR    # custom prefix
#   ./install.sh --uninstall     # remove (honours --system/--prefix)
#
set -euo pipefail

SRC="$(cd "$(dirname "$0")" && pwd)"
APP_ID="io.github.anidedej.ADtune"

MODE="user"
ACTION="install"
PREFIX=""
for arg in "$@"; do
    case "$arg" in
        --system)    MODE="system" ;;
        --user)      MODE="user" ;;
        --uninstall) ACTION="uninstall" ;;
        --prefix=*)  PREFIX="${arg#*=}" ;;
        -h|--help)   sed -n '3,10p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "Unknown option: $arg (try --help)" >&2; exit 2 ;;
    esac
done

if [ -z "$PREFIX" ]; then
    [ "$MODE" = "system" ] && PREFIX="/usr/local" || PREFIX="$HOME/.local"
fi

SUDO=""
parent="$PREFIX"; while [ ! -e "$parent" ] && [ "$parent" != "/" ]; do parent="$(dirname "$parent")"; done
if [ ! -w "$parent" ]; then
    command -v sudo >/dev/null && SUDO="sudo" || { echo "No write access to $PREFIX and no sudo." >&2; exit 1; }
fi

BINDIR="$PREFIX/bin"
APPDIR="$PREFIX/share/applications"
ICONDIR="$PREFIX/share/icons/hicolor/scalable/apps"
# System installs autostart for all users; user installs use ~/.config/autostart.
AUTOSTART_DIR="/etc/xdg/autostart"

run()   { [ -n "$SUDO" ] && $SUDO "$@" || "$@"; }
write() { [ -n "$SUDO" ] && $SUDO tee "$1" >/dev/null || cat >"$1"; }
info()  { printf '  \033[36m•\033[0m %s\n' "$1"; }
warn()  { printf '  \033[33m!\033[0m %s\n' "$1"; }
ok()    { printf '  \033[32m✓\033[0m %s\n' "$1"; }

do_uninstall() {
    printf 'Uninstalling ADtune from %s …\n' "$PREFIX"
    # Stop the calibration daemon: its in-process filter dies with it and
    # WirePlumber falls back to the physical output.
    pkill -x adtune-service 2>/dev/null || true
    info "stopped the calibration daemon"
    # Pre-1.1 leftovers (systemd-based architecture), harmless if absent.
    if command -v systemctl >/dev/null; then
        systemctl --user disable --now adtune.service 2>/dev/null || true
        systemctl --user daemon-reload 2>/dev/null || true
    fi
    rm -f "$HOME/.config/systemd/user/adtune.service"
    # Remove runtime config: desired state, saved profiles.
    rm -rf "$HOME/.config/adtune"
    info "removed ~/.config/adtune (settings + saved profiles)"
    # Remove the installed files.
    rm -f "$HOME/.config/autostart/$APP_ID.Service.desktop"
    run rm -f "$BINDIR/adtune" "$BINDIR/adtune-service" \
        "$APPDIR/$APP_ID.desktop" "$ICONDIR/$APP_ID.svg" \
        "$AUTOSTART_DIR/$APP_ID.Service.desktop"
    run update-desktop-database "$APPDIR" 2>/dev/null || true
    run gtk-update-icon-cache -qtf "$PREFIX/share/icons/hicolor" 2>/dev/null || true
    ok "Removed ADtune completely. Your audio output is back to normal."
}

do_install() {
    printf 'Checking dependencies…\n'
    command -v cargo >/dev/null && ok "cargo (Rust toolchain)" || { warn "cargo not found — install Rust from https://rustup.rs"; exit 1; }
    command -v pkg-config >/dev/null && pkg-config --exists libpipewire-0.3 && ok "libpipewire-0.3 dev headers" \
        || { warn "libpipewire-0.3 headers not found — install libpipewire-0.3-dev (plus libclang-dev)"; exit 1; }
    command -v pipewire >/dev/null && ok "PipeWire" \
        || warn "PipeWire not found — ADtune needs a PipeWire audio session at runtime"

    printf '\nBuilding ADtune (release)…\n'
    ( cd "$SRC" && cargo build --release --locked -p adtune-ui -p adtune-pipewire )
    BIN="$SRC/target/release/adtune-ui"
    SVC="$SRC/target/release/adtune-service"
    [ -x "$BIN" ] || { echo "Build did not produce $BIN" >&2; exit 1; }
    [ -x "$SVC" ] || { echo "Build did not produce $SVC" >&2; exit 1; }

    printf '\nInstalling to %s …\n' "$PREFIX"
    run install -Dm755 "$BIN" "$BINDIR/adtune"
    run install -Dm755 "$SVC" "$BINDIR/adtune-service"
    info "adtune + adtune-service (self-contained; catalog embedded)"
    run install -Dm644 "$SRC/packaging/linux/$APP_ID.svg" "$ICONDIR/$APP_ID.svg"
    run install -d "$APPDIR"
    sed "s|@BINDIR@|$BINDIR|g; s|@ICON@|$APP_ID|g" "$SRC/packaging/linux/adtune.desktop.in" | write "$APPDIR/$APP_ID.desktop"
    run chmod 644 "$APPDIR/$APP_ID.desktop"
    # Login autostart for the calibration daemon (inert until first use: the
    # daemon exits immediately when calibration was never enabled).
    if [ "$MODE" = "user" ]; then
        install -d "$HOME/.config/autostart"
        sed "s|@BINDIR@|$BINDIR|g" "$SRC/packaging/linux/adtune-service.desktop.in" > "$HOME/.config/autostart/$APP_ID.Service.desktop"
    else
        run install -d "$AUTOSTART_DIR"
        sed "s|@BINDIR@|$BINDIR|g" "$SRC/packaging/linux/adtune-service.desktop.in" | write "$AUTOSTART_DIR/$APP_ID.Service.desktop"
        run chmod 644 "$AUTOSTART_DIR/$APP_ID.Service.desktop"
    fi
    info "desktop entry + icon + login autostart"
    run update-desktop-database "$APPDIR" 2>/dev/null || true
    run gtk-update-icon-cache -qtf "$PREFIX/share/icons/hicolor" 2>/dev/null || true

    printf '\n'; ok "ADtune installed."
    case ":$PATH:" in
        *":$BINDIR:"*) printf '  Run \033[1madtune\033[0m or launch it from your app menu.\n' ;;
        *) warn "$BINDIR is not on your PATH — add it or launch from the app menu." ;;
    esac
    printf '  On first Apply, ADtune creates the "ADtune Calibrated" PipeWire output and routes audio through it.\n'
}

[ "$ACTION" = "uninstall" ] && do_uninstall || do_install
