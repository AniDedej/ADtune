//! PipeWire persisted state (`~/.config/adtune/state.json`). The serde model is
//! shared via `adtune_core::state`; here `target_id` is the i64 PipeWire node id,
//! so the state is specialized to `i64`.

use adtune_core::state::StoredState as CoreState;
use adtune_core::{AudioProfile, ToneSettings};
use std::path::Path;

/// The persisted state specialised to this backend: `target_id` is the i64
/// PipeWire node id. Everything else (profile, tone, bypass) is shared.
pub type StoredState = CoreState<i64>;

/// Read `state.json`, or `None` if it is missing/unreadable. Thin wrapper over
/// the core reader that pins the id type to `i64` for this backend.
pub fn read_state(path: &Path) -> Option<StoredState> {
    adtune_core::state::read_state(path)
}

/// Build and persist the state file from live values. Thin wrapper that
/// assembles a [`StoredState`] and hands it to the core writer.
pub fn write_state(
    path: &Path,
    profile: &AudioProfile,
    target_id: i64,
    target_name: &str,
    tone: &ToneSettings,
) -> std::io::Result<()> {
    adtune_core::state::write_state(
        path,
        &StoredState::new(profile, target_id, target_name, tone),
    )
}
