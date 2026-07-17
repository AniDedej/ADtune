# Snap auto-connect request — post to https://forum.snapcraft.io/c/store-requests

**Title:** Auto-connect request: adtune → pipewire

**Body:**

ADtune (https://snapcraft.io/adtune) is a system-wide audio calibration app:
it applies parametric-EQ correction to system audio via a PipeWire
filter-chain. Native PipeWire socket access is the app's core function —
without the `pipewire` interface connected it cannot list outputs or apply
any correction, so the app is non-functional out of the box.

The app only creates a virtual sink and adjusts routing/EQ; it does not
record audio (it also plugs `audio-playback`, not `audio-record`).

Source: https://github.com/AniDedej/ADtune (MIT). I am the upstream author
and snap publisher (adedej96).

Requesting auto-connect of the `pipewire` interface for `adtune`.
