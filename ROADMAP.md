# ADtune roadmap

Directions planned for after the 1.0 base. Nothing here is committed to a release date;
this is a shared picture of where the app is headed so contributors can pick something up.
Ordering is rough priority, not a schedule.

## 1. Per-application and per-device routing

Today one active profile applies to the whole system. The goal is **automatic, context-aware
correction**:

- **Per-device profiles.** Remember a profile per output device and re-apply it automatically
  when the active/default device changes (unplug the USB DAC, and the laptop-speaker profile
  takes over without a manual switch).
- **Per-application correction.** Route a different correction to a specific app's audio (e.g.
  a music player vs. a video call), rather than a single global filter.

**Approach sketch.** The state layer already keys the last profile to a target device id, so
per-device is mostly a matter of storing a map instead of a single entry plus a device-change
watcher. On Linux, per-app routing fits PipeWire's node graph naturally (route a specific
stream through a per-app filter node); on Windows the APO model is per-endpoint, so per-app
correction is a bigger design question (likely a stream-effect APO keyed on the session) and
should be scoped separately.

## 2. Microphone-based room measurement

Manual and imported profiles cover headphones well, but speakers in a room need **measured
correction**. The goal is an in-app measurement flow:

- Play a sweep or pink noise, capture it with a microphone, compute the room/speaker frequency
  response, and derive a correction curve automatically.
- Feed the result straight into the existing profile model (bands + preamp), so the graph,
  tone controls, A/B bypass, and the DSP engine all work unchanged.

**Approach sketch.** The DSP core and profile model are already the right target for the output;
the new work is the capture + analysis front end (FFT of the captured sweep, smoothing, target-
curve matching, converting the delta into a bounded set of biquad bands within `MAX_BANDS`).
Keep the measurement code isolated in its own module so the real-time and calibration paths stay
untouched. Mind the same input-safety discipline the rest of the app uses — clamp and bound every
derived band.

## 3. Tray, hotkeys, and auto-update

Quality-of-life features for daily use:

- **System-tray presence** with quick profile switching and an on/off (bypass) toggle, so
  common actions don't require opening the main window.
- **Global hotkeys** for bypass and profile cycling.
- **Auto-update** so new versions reach users without a manual reinstall — this becomes far
  more valuable once the app is code-signed (see the packaging README's signing notes), since
  an update channel wants a trusted signature.

**Approach sketch.** Tray and hotkeys are UI-layer additions (Slint plus a small platform shim);
they don't touch the DSP or backends. Auto-update is mostly a packaging/distribution concern and
should be designed alongside a signing story rather than before it.

---

Have an idea that isn't here? Open an issue describing the use case before writing code, so the
design can be discussed against the existing core/backend split.
