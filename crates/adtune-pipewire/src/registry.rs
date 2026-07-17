//! Registry snapshot queries — the graph's output sinks and the current
//! default-sink selection — replacing the old `wpctl status` text parsing.

use crate::conn::Session;
use pipewire::metadata::Metadata;
use pipewire::types::ObjectType;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

/// The metadata key WirePlumber keeps the *effective* default sink under.
const DEFAULT_SINK_KEY: &str = "default.audio.sink";

/// One `Audio/Sink` node from the registry (physical, or ADtune's own virtual
/// sink — callers filter by node-name prefix as needed).
#[derive(Clone, Debug)]
pub struct SinkInfo {
    /// PipeWire node id (ephemeral across reboots/graph restarts).
    pub id: u32,
    /// Stable node name (e.g. `alsa_output.usb-…`).
    pub name: String,
    /// Human-readable label for the UI.
    pub description: String,
}

/// A point-in-time view of the graph.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub sinks: Vec<SinkInfo>,
    /// `node.name` of the current default sink, from the `default` metadata
    /// object's `default.audio.sink` value.
    pub default_sink: Option<String>,
}

/// Collect all sinks and the default-sink selection in two roundtrips: the
/// first delivers the registry globals (and binds the `default` metadata
/// object as a side effect), the second flushes the property events that the
/// fresh metadata binding triggers.
pub fn snapshot(session: &Session) -> Result<Snapshot, String> {
    let registry = session
        .core
        .get_registry_rc()
        .map_err(|e| format!("Could not get the PipeWire registry: {e}"))?;
    let sinks = Rc::new(RefCell::new(Vec::<SinkInfo>::new()));
    let default_sink = Rc::new(RefCell::new(None::<String>));
    // Bound metadata proxies and their listeners must outlive the roundtrips,
    // or their property events are dropped before delivery.
    type MetadataHolder = (Metadata, Box<dyn pipewire::proxy::Listener>);
    let holders: Rc<RefCell<Vec<MetadataHolder>>> = Rc::new(RefCell::new(Vec::new()));

    let _reg_listener = registry
        .add_listener_local()
        .global({
            let registry = registry.clone();
            let sinks = sinks.clone();
            let default_sink = default_sink.clone();
            let holders = holders.clone();
            move |global| {
                let props = match &global.props {
                    Some(p) => p,
                    None => return,
                };
                match global.type_ {
                    ObjectType::Node => {
                        if props.get("media.class") == Some("Audio/Sink") {
                            let name = props.get("node.name").unwrap_or_default().to_string();
                            let description = props
                                .get("node.description")
                                .or_else(|| props.get("node.nick"))
                                .unwrap_or(&name)
                                .to_string();
                            sinks.borrow_mut().push(SinkInfo {
                                id: global.id,
                                name,
                                description,
                            });
                        }
                    }
                    ObjectType::Metadata => {
                        // Only the session manager's `default` metadata object
                        // carries the default-device selections.
                        if props.get("metadata.name") != Some("default") {
                            return;
                        }
                        if let Ok(md) = registry.bind::<Metadata, _>(global) {
                            let listener = md
                                .add_listener_local()
                                .property({
                                    let default_sink = default_sink.clone();
                                    move |_subject, key, _type, value| {
                                        if key == Some(DEFAULT_SINK_KEY) {
                                            *default_sink.borrow_mut() =
                                                value.and_then(parse_default_name);
                                        }
                                        0
                                    }
                                })
                                .register();
                            holders.borrow_mut().push((md, Box::new(listener)));
                        }
                    }
                    _ => {}
                }
            }
        })
        .register();

    session.roundtrip(Duration::from_secs(5))?;
    session.roundtrip(Duration::from_secs(5))?;

    Ok(Snapshot {
        sinks: sinks.take(),
        default_sink: default_sink.take(),
    })
}

/// Point the system's default sink at `node_name` — what `wpctl set-default`
/// does: write `default.configured.audio.sink` on the `default` metadata
/// object. WirePlumber applies it (moving existing streams) and persists it.
pub fn set_default_sink(session: &Session, node_name: &str) -> Result<(), String> {
    let registry = session
        .core
        .get_registry_rc()
        .map_err(|e| format!("Could not get the PipeWire registry: {e}"))?;
    // serde_json handles escaping; node names are charset-limited but a
    // hostile description must not be able to break out of the JSON.
    let value = serde_json::json!({ "name": node_name }).to_string();

    let holders: Rc<RefCell<Vec<Metadata>>> = Rc::new(RefCell::new(Vec::new()));
    let _reg_listener = registry
        .add_listener_local()
        .global({
            let registry = registry.clone();
            let holders = holders.clone();
            let value = value.clone();
            move |global| {
                if global.type_ != ObjectType::Metadata {
                    return;
                }
                let is_default =
                    global.props.as_ref().and_then(|p| p.get("metadata.name")) == Some("default");
                if !is_default {
                    return;
                }
                if let Ok(md) = registry.bind::<Metadata, _>(global) {
                    md.set_property(
                        0,
                        "default.configured.audio.sink",
                        Some("Spa:String:JSON"),
                        Some(&value),
                    );
                    holders.borrow_mut().push(md);
                }
            }
        })
        .register();

    // First roundtrip delivers the metadata global (and issues the write);
    // the second confirms the server processed the write.
    session.roundtrip(Duration::from_secs(5))?;
    session.roundtrip(Duration::from_secs(5))?;

    if holders.borrow().is_empty() {
        return Err("No default-device metadata found (is WirePlumber running?).".into());
    }
    Ok(())
}

/// `default.audio.sink` metadata values look like `{"name":"alsa_output…"}`.
fn parse_default_name(value: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct V {
        name: String,
    }
    serde_json::from_str::<V>(value).ok().map(|v| v.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_name_parses_metadata_json() {
        assert_eq!(
            parse_default_name(r#"{"name":"alsa_output.usb"}"#).as_deref(),
            Some("alsa_output.usb")
        );
        assert_eq!(parse_default_name("garbage"), None);
    }
}
