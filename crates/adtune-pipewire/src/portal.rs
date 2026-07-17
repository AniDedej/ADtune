//! Flatpak Background-portal autostart request.
//!
//! Inside a Flatpak sandbox nothing may write `~/.config/autostart`, so
//! autostarting the daemon at login goes through the Background portal
//! (`org.freedesktop.portal.Background.RequestBackground`): the portal writes
//! a host-side autostart entry that runs
//! `flatpak run --command=adtune-service <app-id>` at login. Outside Flatpak
//! this module is a no-op — the packages install a plain XDG autostart file.
//!
//! Talks D-Bus directly through `zbus::blocking` (already in the dependency
//! tree via the UI's file-dialog portal) rather than pulling an async runtime.

use std::collections::HashMap;
use std::time::Duration;

/// Whether this process runs inside a Flatpak sandbox.
pub fn in_flatpak() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
}

/// Ask the portal to autostart `adtune-service` at login. Idempotent — the
/// portal updates the same entry — so callers can fire it on every daemon
/// start. Returns whether the portal granted the request.
///
/// Blocks up to ~5 s waiting for the portal's response; run it off the main
/// loop.
pub fn request_background_autostart() -> Result<bool, String> {
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::{OwnedObjectPath, Value};

    let conn = Connection::session().map_err(|e| format!("session bus: {e}"))?;

    // The portal reports results on a Request object whose path is derived
    // from our unique name and a token we pick, so subscribe before calling.
    let unique = conn.unique_name().ok_or("no unique bus name")?.to_string();
    let sender = unique.trim_start_matches(':').replace('.', "_");
    let token = format!("adtune_{}", std::process::id());
    let request_path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");

    let request_proxy: Proxy<'_> = Proxy::new(
        &conn,
        "org.freedesktop.portal.Desktop",
        request_path.as_str(),
        "org.freedesktop.portal.Request",
    )
    .map_err(|e| format!("request proxy: {e}"))?;
    let mut responses = request_proxy
        .receive_signal("Response")
        .map_err(|e| format!("subscribe: {e}"))?;

    let portal: Proxy<'_> = Proxy::new(
        &conn,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Background",
    )
    .map_err(|e| format!("portal proxy: {e}"))?;

    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));
    options.insert(
        "reason",
        Value::from("Keep audio calibration active after login."),
    );
    options.insert("autostart", Value::from(true));
    options.insert(
        "commandline",
        Value::from(vec!["adtune-service".to_string()]),
    );

    let _handle: OwnedObjectPath = portal
        .call("RequestBackground", &("", options))
        .map_err(|e| format!("RequestBackground: {e}"))?;

    // Wait (bounded) for the Response signal: (code, results); code 0 = granted.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if let Some(msg) = responses.next() {
            let (code, results): (u32, HashMap<String, zbus::zvariant::OwnedValue>) = msg
                .body()
                .deserialize()
                .map_err(|e| format!("response body: {e}"))?;
            let autostart_granted = results
                .get("autostart")
                .and_then(|v| bool::try_from(v.clone()).ok())
                .unwrap_or(code == 0);
            return Ok(code == 0 && autostart_granted);
        }
    }
    Err("timed out waiting for the background portal".into())
}
