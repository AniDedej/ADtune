//! Build script: compiles the Slint UI to Rust and, on Windows, embeds the app
//! icon and version-info into the executable.

fn main() {
    // Compile `ui/app.slint` to Rust; `include_modules!()` in main.rs then pulls
    // in the generated `MainWindow`, its callbacks, and the shared struct types.
    slint_build::compile("ui/app.slint").expect("compile app.slint");

    // Embed the app icon into the Windows executable. The `#[cfg(windows)]`
    // matches the host (winresource is a host-gated build-dependency); the env
    // check matches the *target*, so a Windows host building a non-Windows
    // target skips this. Non-fatal so a missing toolchain never breaks the build.
    #[cfg(windows)]
    {
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
            println!("cargo:rerun-if-changed=../../packaging/windows/adtune.ico");
            let mut res = winresource::WindowsResource::new();
            res.set_icon("../../packaging/windows/adtune.ico");
            // Windows exe version-info (winresource defaults these to the crate name).
            res.set("ProductName", "ADtune");
            res.set("FileDescription", "System-wide audio calibration");
            res.set("CompanyName", "Antonio DEDEJ");
            res.set("LegalCopyright", "Copyright (c) 2026 Antonio DEDEJ");
            // The installer ships this binary renamed to adtune.exe.
            res.set("OriginalFilename", "adtune.exe");
            let _ = res.compile();
        }
    }
}
