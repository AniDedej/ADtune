//! Native PipeWire session bootstrap, shared by the UI's one-shot queries and
//! the daemon's long-lived connection.
//!
//! Everything here is thread-confined: a [`Session`] must be created, used,
//! and dropped on the same thread (libpipewire proxies are not `Send`). The
//! UI already runs every backend call on a fresh worker thread, which fits.

use pipewire::{self as pw, context::ContextRc, core::CoreRc, main_loop::MainLoopRc};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

/// A connected PipeWire session: main loop + context + core proxy.
pub struct Session {
    pub mainloop: MainLoopRc,
    pub context: ContextRc,
    pub core: CoreRc,
}

/// Terminal states of a [`Session::roundtrip`].
enum Roundtrip {
    Pending,
    Done,
    Error(String),
}

impl Session {
    /// Connect to the user's PipeWire instance (the `pipewire-0` socket under
    /// `XDG_RUNTIME_DIR`). A failure here doubles as the availability probe:
    /// no socket means PipeWire isn't running — or, in a sandbox, wasn't
    /// granted.
    pub fn connect() -> Result<Session, String> {
        pw::init(); // idempotent
        let mainloop = MainLoopRc::new(None)
            .map_err(|e| format!("Could not create a PipeWire main loop: {e}"))?;
        let context = ContextRc::new(&mainloop, None)
            .map_err(|e| format!("Could not create a PipeWire context: {e}"))?;
        let core = context.connect_rc(None).map_err(|_| {
            "Could not connect to PipeWire. Is the PipeWire service running?".to_string()
        })?;
        Ok(Session {
            mainloop,
            context,
            core,
        })
    }

    /// Run the loop until the server confirms it processed everything we sent
    /// before this call, so every event our requests triggered — registry
    /// globals, metadata properties — has been delivered to our listeners.
    ///
    /// Bails out early if the connection reports an error, and gives up after
    /// `timeout` as a hang guard (a dead server never answers the sync).
    pub fn roundtrip(&self, timeout: Duration) -> Result<(), String> {
        let state = Rc::new(RefCell::new(Roundtrip::Pending));
        let pending = self
            .core
            .sync(0)
            .map_err(|e| format!("PipeWire sync failed: {e}"))?;

        let _core_listener = self
            .core
            .add_listener_local()
            .done({
                let state = state.clone();
                let mainloop = self.mainloop.clone();
                move |id, seq| {
                    if id == pw::core::PW_ID_CORE && seq == pending {
                        *state.borrow_mut() = Roundtrip::Done;
                        mainloop.quit();
                    }
                }
            })
            .error({
                let state = state.clone();
                let mainloop = self.mainloop.clone();
                move |_id, _seq, _res, message| {
                    *state.borrow_mut() = Roundtrip::Error(message.to_string());
                    mainloop.quit();
                }
            })
            .register();

        let timer = self.mainloop.loop_().add_timer({
            let mainloop = self.mainloop.clone();
            move |_expirations| mainloop.quit()
        });
        timer
            .update_timer(Some(timeout), None)
            .into_result()
            .map_err(|e| format!("Could not arm the PipeWire timeout timer: {e}"))?;

        self.mainloop.run();

        let result = match &*state.borrow() {
            Roundtrip::Done => Ok(()),
            Roundtrip::Error(e) => Err(format!("PipeWire connection error: {e}")),
            Roundtrip::Pending => Err("Timed out waiting for PipeWire to answer.".into()),
        };
        result
    }
}
