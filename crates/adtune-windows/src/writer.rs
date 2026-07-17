//! Atomic write of the APO's config file. OS-independent (`std::fs` only), so it
//! is unit-tested on Linux.

use std::fs;
use std::io;
use std::path::Path;

/// Write `contents` to `dir/name` atomically (temp in the same dir + rename) so
/// the APO's file watcher never observes a half-written file. UTF-8, no BOM.
pub fn atomic_write(dir: &Path, name: &str, contents: &str) -> io::Result<()> {
    // Temp lives in the destination dir (rename is only atomic within one
    // filesystem) and is tagged with the pid so two processes writing at once
    // can't clobber each other's temp file. Leading dot keeps it out of the way.
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    fs::write(&tmp, contents.as_bytes())?;
    fs::rename(&tmp, dir.join(name)).inspect_err(|_| {
        let _ = fs::remove_file(&tmp); // never leave a stale temp behind
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join(format!("adtune-writer-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        atomic_write(&dir, "config.txt", "Preamp: -3.0 dB\r\n").unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("config.txt")).unwrap(),
            "Preamp: -3.0 dB\r\n"
        );
        atomic_write(&dir, "config.txt", "Preamp: 0.0 dB\r\n").unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("config.txt")).unwrap(),
            "Preamp: 0.0 dB\r\n"
        );
        let temps = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .count();
        assert_eq!(temps, 0);
    }
}
