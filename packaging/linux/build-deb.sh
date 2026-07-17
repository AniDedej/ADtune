#!/usr/bin/env bash
#
# Build a double-clickable ADtune .deb for Debian/Ubuntu.
# Output: dist/adtune_<version>_amd64.deb
#
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# Derive the package version from the workspace manifest so the .deb version can
# never drift from the crate version. Falls back to 0.0.0 if the parse fails.
VERSION="$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed -E 's/.*"([^"]+)".*/\1/')"
VERSION="${VERSION:-0.0.0}"
APP_ID="io.github.anidedej.ADtune"
cd "$ROOT"

echo "Building ADtune (release)…"
cargo build --release --locked -p adtune-ui -p adtune-pipewire
BIN="$ROOT/target/release/adtune-ui"
SVC="$ROOT/target/release/adtune-service"
[ -x "$BIN" ] || { echo "Build did not produce $BIN" >&2; exit 1; }
[ -x "$SVC" ] || { echo "Build did not produce $SVC" >&2; exit 1; }

PKG="$ROOT/target/deb/adtune_${VERSION}_amd64"
rm -rf "$PKG"
install -Dm755 "$BIN" "$PKG/usr/bin/adtune"
install -Dm755 "$SVC" "$PKG/usr/bin/adtune-service"
install -Dm644 "$ROOT/packaging/linux/$APP_ID.svg" "$PKG/usr/share/icons/hicolor/scalable/apps/$APP_ID.svg"
install -d "$PKG/usr/share/applications"
sed "s|@BINDIR@|/usr/bin|g; s|@ICON@|$APP_ID|g" \
    "$ROOT/packaging/linux/adtune.desktop.in" > "$PKG/usr/share/applications/$APP_ID.desktop"

# Login autostart for the calibration daemon (it exits immediately when the
# user never enabled calibration, so this is inert until first use).
install -d "$PKG/etc/xdg/autostart"
sed "s|@BINDIR@|/usr/bin|g" \
    "$ROOT/packaging/linux/adtune-service.desktop.in" > "$PKG/etc/xdg/autostart/$APP_ID.Service.desktop"

# AppStream metainfo: gives software centers the display name "ADtune",
# license, developer, and description (a bare .deb otherwise shows the
# lowercase package name and "License unknown").
install -d "$PKG/usr/share/metainfo"
sed "s|@VERSION@|$VERSION|g; s|@DATE@|$(date +%Y-%m-%d)|g" \
    "$ROOT/packaging/linux/$APP_ID.metainfo.xml.in" > "$PKG/usr/share/metainfo/$APP_ID.metainfo.xml"

install -d "$PKG/DEBIAN"
cat > "$PKG/DEBIAN/control" <<EOF
Package: adtune
Version: $VERSION
Section: sound
Priority: optional
Architecture: amd64
Maintainer: Antonio DEDEJ
Installed-Size: $(du -ks "$PKG/usr" | cut -f1)
Depends: libc6, libgcc-s1, libpipewire-0.3-0, pipewire, wireplumber
Recommends: pipewire-pulse
Description: System-wide audio calibration
 ADtune applies a parametric-EQ correction to all system audio using PipeWire,
 before it reaches whatever you're listening on: headphones, speakers, a USB
 DAC. It ships with a catalog of thousands of measured headphone corrections
 to start from, and you can build your own curve for any device, with a live
 frequency-response graph and tone controls.
Homepage: https://github.com/AniDedej/ADtune
EOF

# postrm: the calibration daemon and its config are per-user, so apt doesn't
# know about them. On remove/purge, stop every running daemon (its in-process
# filter dies with it and WirePlumber falls back to the physical output); on
# purge, also delete the invoking user's config + saved profiles.
cat > "$PKG/DEBIAN/postrm" <<'POSTRM'
#!/bin/sh
set +e
case "$1" in
    remove|purge)
        pkill -x adtune-service 2>/dev/null
        ;;
esac
u="${SUDO_USER:-}"
home="$(getent passwd "$u" 2>/dev/null | cut -d: -f6)"
if [ -n "$u" ] && [ "$u" != "root" ] && [ -n "$home" ] && [ "$1" = "purge" ]; then
    rm -rf "$home/.config/adtune"
    # Ephemeral daemon state (status + instance lock); a reboot would clear
    # it anyway, but purge should leave nothing findable.
    uid="$(id -u "$u" 2>/dev/null)"
    [ -n "$uid" ] && rm -rf "/run/user/$uid/adtune"
    # Pre-1.1 leftovers, harmless if absent.
    rm -f "$home/.config/systemd/user/adtune.service"
fi
exit 0
POSTRM
chmod 0755 "$PKG/DEBIAN/postrm"

mkdir -p "$ROOT/dist"
OUT="$ROOT/dist/adtune_${VERSION}_amd64.deb"
fakeroot dpkg-deb --build "$PKG" "$OUT"
echo
echo "Built: $OUT"
dpkg-deb --info "$OUT" | sed -n '2,13p'
