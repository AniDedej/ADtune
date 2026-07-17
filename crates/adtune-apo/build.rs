//! Build script: on Windows, embeds version-info into the APO DLL so Explorer,
//! Task Manager, and crash dumps identify it (a bare cdylib carries no
//! VERSIONINFO resource at all).

fn main() {
    // The `#[cfg(windows)]` matches the host (winresource is a host-gated
    // build-dependency); the env check matches the *target*, so a Windows host
    // building a non-Windows target skips this. Non-fatal so a missing
    // toolchain never breaks the build.
    #[cfg(windows)]
    {
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
            let mut res = winresource::WindowsResource::new();
            // FileVersion/ProductVersion default to the workspace version.
            res.set("ProductName", "ADtune");
            res.set("FileDescription", "ADtune Audio Processing Object");
            res.set("CompanyName", "Antonio DEDEJ");
            res.set("LegalCopyright", "Copyright (c) 2026 Antonio DEDEJ");
            res.set("OriginalFilename", "adtune_apo.dll");
            let _ = res.compile();
        }
    }
}
