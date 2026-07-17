//! A user profile library: one file per saved/custom profile in a directory.
//! Native files use the `.adtuneprofile` extension (JSON content). Portable
//! `std::fs`; the OS-specific directory is chosen by the caller (the UI), so
//! core stays platform-free.

use crate::parametric_eq::parse_parametric_eq;
use crate::profile::AudioProfile;
use crate::state::{profile_from_json, profile_to_json};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// The file extension for ADtune's native profile files (JSON content).
pub const PROFILE_EXT: &str = "adtuneprofile";

/// Upper bound on a profile/config file read. Profiles are a few KB; this stops
/// a malformed or hostile multi-gigabyte file from exhausting memory (a
/// truncated read simply fails to parse and the file is skipped).
pub const MAX_PROFILE_BYTES: u64 = 8 << 20;

/// Read a file to a UTF-8 string, refusing to buffer more than `MAX_PROFILE_BYTES`.
pub(crate) fn read_capped(path: &Path) -> io::Result<String> {
    let mut buf = String::new();
    std::fs::File::open(path)?
        .take(MAX_PROFILE_BYTES)
        .read_to_string(&mut buf)?;
    Ok(buf)
}

/// A filesystem-safe stem derived from a profile's key (or name if unkeyed).
fn slug(profile: &AudioProfile) -> String {
    let base = if profile.key.trim().is_empty() {
        &profile.name
    } else {
        &profile.key
    };
    let s: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "profile".into()
    } else {
        s
    }
}

/// Whether `p` has the native `.adtuneprofile` extension (case-insensitive), used
/// to pick out library files when scanning a directory.
fn is_profile_file(p: &Path) -> bool {
    p.extension()
        .and_then(|x| x.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(PROFILE_EXT))
}

/// Every saved profile in `dir`, paired with its file path, sorted by name. A
/// directory that does not exist yields an empty list (not an error).
pub fn list_profiles(dir: &Path) -> Vec<(PathBuf, AudioProfile)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(PathBuf, AudioProfile)> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| is_profile_file(p))
        .filter_map(|p| {
            let prof = read_capped(&p)
                .ok()
                .and_then(|t| profile_from_json(&t).ok())?;
            Some((p, prof))
        })
        .collect();
    out.sort_by_cached_key(|(_, prof)| prof.name.to_lowercase());
    out
}

/// Save `profile` into `dir` as a **new** `<slug>.adtuneprofile` file (creating
/// `dir` if needed). Always writes a fresh file — a numeric suffix is appended
/// if the name is taken — so a Save of a not-yet-in-library profile never
/// overwrites an existing one. Updating a profile already in the library is
/// [`overwrite_profile`] against its known path. Returns the written path.
pub fn save_profile(dir: &Path, profile: &AudioProfile) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let base = slug(profile);
    let mut path = dir.join(format!("{base}.{PROFILE_EXT}"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{base}-{n}.{PROFILE_EXT}"));
        n += 1;
    }
    std::fs::write(&path, profile_to_json(profile))?;
    Ok(path)
}

/// Overwrite an existing library file in place (used when re-saving a profile
/// that was loaded from the library, so edits update the same file).
pub fn overwrite_profile(path: &Path, profile: &AudioProfile) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, profile_to_json(profile))
}

/// Delete a specific saved-profile file (a no-op if it is already gone).
pub fn delete_profile(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// A human name for an imported file: its stem, unless that is the generic
/// AutoEq `ParametricEQ`, in which case the parent folder (the headphone name).
fn import_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Imported profile");
    if stem.eq_ignore_ascii_case("parametriceq") || stem.eq_ignore_ascii_case("parametric") {
        path.parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(stem)
            .to_string()
    } else {
        stem.to_string()
    }
}

/// Load a profile from any supported file (case-insensitive extension):
/// `.adtuneprofile`/`.json` → native JSON, anything else → ParametricEQ text.
/// The single entry point the Import dialog uses.
pub fn load_profile_file(path: &Path) -> Result<AudioProfile, String> {
    let text = read_capped(path).map_err(|e| e.to_string())?;
    let ext = path
        .extension()
        .and_then(|x| x.to_str())
        .map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("adtuneprofile") | Some("json") => profile_from_json(&text),
        _ => parse_parametric_eq(&text, &import_name(path)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BandType, FilterBand};

    /// A small keyed two-band fixture profile reused across the store tests.
    fn sample() -> AudioProfile {
        AudioProfile {
            key: "custom:test-hp".into(),
            name: "Test HP".into(),
            preamp: -3.2,
            bands: vec![
                FilterBand::new(BandType::LowShelf, 105.0, 4.0, 0.7),
                FilterBand::new(BandType::Peaking, 2500.0, -2.0, 1.5),
            ],
            ..Default::default()
        }
    }

    /// Save, list, and delete form a consistent lifecycle: a saved profile lists
    /// back with its values intact, and deleting it empties the directory.
    #[test]
    fn save_list_delete_round_trips() {
        let dir = std::env::temp_dir().join(format!("adtune-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = sample();
        let path = save_profile(&dir, &p).unwrap();
        assert!(path.exists());
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some(PROFILE_EXT));

        let listed = list_profiles(&dir);
        assert_eq!(listed.len(), 1);
        let (lpath, back) = &listed[0];
        assert_eq!(lpath, &path);
        assert_eq!(back.name, "Test HP");
        assert!((back.preamp - -3.2).abs() < 1e-9);
        assert_eq!(back.bands.len(), 2);

        delete_profile(&path).unwrap();
        assert!(list_profiles(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two distinct keyless profiles that slug to the same stem get separate files
    /// (the numeric suffix) rather than overwriting each other.
    #[test]
    fn distinct_keyless_profiles_do_not_clobber() {
        let dir = std::env::temp_dir().join(format!("adtune-store-collide-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = AudioProfile {
            key: String::new(),
            name: "ParametricEQ".into(),
            preamp: -1.0,
            ..sample()
        };
        let b = AudioProfile {
            key: String::new(),
            name: "ParametricEQ".into(),
            preamp: -9.0,
            ..sample()
        };
        let pa = save_profile(&dir, &a).unwrap();
        let pb = save_profile(&dir, &b).unwrap();
        assert_ne!(pa, pb, "distinct profiles must not share a file");
        assert_eq!(list_profiles(&dir).len(), 2, "both profiles must survive");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// [`overwrite_profile`] updates the existing file rather than creating a
    /// second one.
    #[test]
    fn overwrite_updates_in_place() {
        let dir =
            std::env::temp_dir().join(format!("adtune-store-overwrite-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut p = sample();
        let path = save_profile(&dir, &p).unwrap();
        p.preamp = -5.0;
        overwrite_profile(&path, &p).unwrap();
        let listed = list_profiles(&dir);
        assert_eq!(listed.len(), 1, "overwrite must not create a second file");
        assert!((listed[0].1.preamp - -5.0).abs() < 1e-9);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Saving the same profile twice creates two distinct files (Save always
    /// writes fresh; it never overwrites).
    #[test]
    fn repeated_save_of_same_profile_makes_distinct_files() {
        let dir = std::env::temp_dir().join(format!("adtune-store-fresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = sample();
        let a = save_profile(&dir, &p).unwrap();
        let b = save_profile(&dir, &p).unwrap();
        assert_ne!(a, b, "each save creates a fresh file");
        assert_eq!(list_profiles(&dir).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// [`load_profile_file`] dispatches on the file extension: ParametricEQ text
    /// vs native JSON, each parsed by the right reader.
    #[test]
    fn load_dispatches_by_extension() {
        let dir = std::env::temp_dir().join(format!("adtune-store-ext-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // ParametricEQ text
        let txt = dir.join("MyEQ.txt");
        std::fs::write(
            &txt,
            "Preamp: -6.0 dB\nFilter 1: ON PK Fc 1000 Hz Gain 3.0 dB Q 1.0\n",
        )
        .unwrap();
        let p = load_profile_file(&txt).unwrap();
        assert_eq!(p.name, "MyEQ");
        assert_eq!(p.bands.len(), 1);
        // native .adtuneprofile (JSON)
        let native = save_profile(&dir, &sample()).unwrap();
        let back = load_profile_file(&native).unwrap();
        assert_eq!(back.name, "Test HP");
        assert_eq!(back.bands.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
