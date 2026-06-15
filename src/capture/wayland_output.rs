//! Wayland display resolution via `wl_output`.
//!
//! Reports the physical-pixel size of the largest connected output's *current* mode — used as
//! the capture canvas / recording-metadata resolution on Wayland sessions. Core `wl_output`
//! has no "primary output" concept, so we deterministically pick the largest current mode
//! (the common single-output case is unambiguous; this is a defined selection policy, not a
//! fallback). Returns `None` if no output reports a current mode, so the caller fails closed
//! rather than guessing a size.
#![cfg(target_os = "linux")]

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};

#[derive(Default)]
struct Outputs {
    /// (width, height) of every output's CURRENT mode, in physical pixels.
    current_modes: Vec<(u32, u32)>,
}

impl Dispatch<WlRegistry, GlobalListContents> for Outputs {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlOutput, ()> for Outputs {
    fn event(
        state: &mut Self,
        _: &WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // `Mode` carries physical pixel dimensions; only the one flagged CURRENT is live.
        if let wl_output::Event::Mode { flags, width, height, .. } = event {
            let is_current = matches!(
                flags,
                wayland_client::WEnum::Value(m) if m.contains(wl_output::Mode::Current)
            );
            if is_current && width > 0 && height > 0 {
                state.current_modes.push((width as u32, height as u32));
            }
        }
    }
}

/// Every connected output's current-mode pixel size (physical pixels). Empty if the Wayland
/// connection fails or no output reported a current mode. Used for the multi-monitor capture
/// canvas envelope (`monitor_layout`); `wayland_output_size` picks the largest from this.
pub fn wayland_output_sizes() -> Vec<(u32, u32)> {
    let Ok(conn) = Connection::connect_to_env() else {
        return Vec::new();
    };
    let Ok((globals, mut queue)) = registry_queue_init::<Outputs>(&conn) else {
        return Vec::new();
    };
    let qh = queue.handle();

    // Bind every advertised wl_output so each emits its Mode events on the next roundtrip.
    // Keep the proxies alive across the roundtrip so their events still route to us.
    let mut _keep: Vec<WlOutput> = Vec::new();
    for global in globals.contents().clone_list() {
        if global.interface.as_str() == WlOutput::interface().name {
            let version = global.version.min(4);
            _keep.push(globals.registry().bind(global.name, version, &qh, ()));
        }
    }

    let mut state = Outputs::default();
    if queue.roundtrip(&mut state).is_err() {
        return Vec::new();
    }
    state.current_modes
}

/// Largest connected output's current-mode pixel size, or `None` if none reported one.
pub fn wayland_output_size() -> Option<(u32, u32)> {
    wayland_output_sizes()
        .into_iter()
        .max_by_key(|&(w, h)| (w as u64) * (h as u64))
}