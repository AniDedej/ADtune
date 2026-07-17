//! One-shot privilege elevation: relaunch ADtune's own executable elevated to run
//! a maintenance subcommand (`--enable-apo` / `--disable-apo`) that writes the
//! machine-wide HKLM registration and bounces the audio engine. The UI process
//! itself stays unprivileged — Windows shows a single UAC prompt for the child.
//!
//! Because the elevated child is a windowless GUI-subsystem process, its stderr
//! goes nowhere, so it records its outcome to `%ProgramData%\ADtune\last-op.txt`
//! and the unprivileged parent reads that back to show the *real* failure reason
//! (e.g. an "access denied" on a protected endpoint key) instead of a generic
//! "it didn't work". Windows-only; a stub elsewhere keeps the crate building.

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::PathBuf;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};

    /// Longest we wait on the elevated helper. It stops/starts the audio
    /// services (`net stop AudioEndpointBuilder /y` …), which is normally a few
    /// seconds; the cap keeps a hung service from blocking the UI thread forever.
    const ELEVATED_TIMEOUT_MS: u32 = 120_000;

    /// Encode to a NUL-terminated UTF-16 buffer for the wide (`…W`) Win32 APIs.
    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// `%ProgramData%\ADtune\last-op.txt` — the file the elevated child writes its
    /// outcome to and the parent reads back (the cross-privilege result channel).
    fn result_path() -> PathBuf {
        crate::native::config_dir().join("last-op.txt")
    }

    /// Called by the elevated child to record why a maintenance op failed (an empty
    /// file means success), so the unprivileged UI can read the real reason.
    pub fn record_op_result(result: &Result<(), String>) {
        let _ = std::fs::create_dir_all(crate::native::config_dir());
        let body = match result {
            Ok(()) => String::new(),
            Err(e) => e.clone(),
        };
        let _ = std::fs::write(result_path(), body);
    }

    /// Read and consume the child's recorded error, if any. Deletes the file so a
    /// later run can't mistake this outcome for its own. An empty/whitespace body
    /// (the success marker) reads back as `None`.
    fn take_op_result() -> Option<String> {
        let p = result_path();
        let msg = std::fs::read_to_string(&p)
            .ok()
            .filter(|s| !s.trim().is_empty());
        let _ = std::fs::remove_file(&p);
        msg
    }

    /// Relaunch our own executable elevated with `arg` and wait for it. `Ok(())` =
    /// the elevated run succeeded; `Err` carries the reason — the child's recorded
    /// error if it ran and failed, or a launch/UAC error if it never started.
    pub fn run_elevated(arg: &str) -> Result<(), String> {
        // Clear any stale result so we never misread a previous run's outcome.
        let _ = std::fs::remove_file(result_path());

        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        // These buffers must outlive the ShellExecuteExW call (the struct holds
        // raw pointers into them).
        let file = wide(exe.as_os_str());
        let verb = wide(std::ffi::OsStr::new("runas"));
        let params = wide(std::ffi::OsStr::new(arg));

        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS, // keep hProcess so we can wait on it
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: 0, // SW_HIDE — the child is a windowless maintenance run
            ..Default::default()
        };

        unsafe {
            ShellExecuteExW(&mut info).map_err(|_| {
                "Setup was cancelled (administrator permission was not granted).".to_string()
            })?;
            if info.hProcess.is_invalid() {
                return Err("Could not start the elevated ADtune helper.".to_string());
            }
            let waited = WaitForSingleObject(info.hProcess, ELEVATED_TIMEOUT_MS);
            if waited != WAIT_OBJECT_0 {
                // Timed out (or the wait failed). Don't block forever and don't
                // read a half-written result; leave the child to finish on its
                // own and report a timeout.
                let _ = CloseHandle(info.hProcess);
                return Err(
                    "The elevated ADtune helper did not finish in time (the audio service may be busy). Try again.".to_string(),
                );
            }
            let mut code: u32 = 1;
            let read = GetExitCodeProcess(info.hProcess, &mut code).is_ok();
            let _ = CloseHandle(info.hProcess);
            if read && code == 0 {
                Ok(())
            } else {
                Err(take_op_result().unwrap_or_else(|| {
                    format!("The elevated ADtune helper failed (exit code {code}).")
                }))
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn run_elevated(_arg: &str) -> Result<(), String> {
        Err("Elevation is only available on Windows.".into())
    }
    pub fn record_op_result(_result: &Result<(), String>) {}
}

pub use imp::{record_op_result, run_elevated};
