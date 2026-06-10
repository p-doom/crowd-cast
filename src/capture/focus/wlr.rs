//! wlroots follow-focus provider: binds `zwlr_foreign_toplevel_manager_v1` and tracks the
//! `activated` toplevel. Complete coverage on wlroots compositors (sway/Hyprland/...),
//! independent of accessibility. Validated against sway.

use std::collections::HashMap;
use std::sync::Arc;

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self as handle, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self as manager, ZwlrForeignToplevelManagerV1},
};

use super::{FocusInfo, FocusState};

/// state enum: maximized=0 minimized=1 activated=2 fullscreen=3.
const ACTIVATED: u32 = 2;
/// Manager event opcode that creates a toplevel handle child object.
const EVT_TOPLEVEL: u16 = 0;

#[derive(Default)]
struct Toplevel {
    app_id: String,
    activated: bool,
}

#[derive(Default)]
struct Toplevels {
    by_id: HashMap<u32, Toplevel>,
}

impl Dispatch<WlRegistry, GlobalListContents> for Toplevels {
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

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for Toplevels {
    fn event(
        state: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let manager::Event::Toplevel { toplevel } = event {
            state.by_id.insert(toplevel.id().protocol_id(), Toplevel::default());
        }
    }
    event_created_child!(Toplevels, ZwlrForeignToplevelManagerV1, [
        EVT_TOPLEVEL => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for Toplevels {
    fn event(
        state: &mut Self,
        h: &ZwlrForeignToplevelHandleV1,
        event: handle::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = h.id().protocol_id();
        let tl = state.by_id.entry(id).or_default();
        match event {
            handle::Event::AppId { app_id } => tl.app_id = app_id,
            handle::Event::State { state: bytes } => {
                // Array of u32 state values; focused toplevel carries ACTIVATED.
                tl.activated = bytes
                    .chunks_exact(4)
                    .any(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]) == ACTIVATED);
            }
            handle::Event::Closed => {
                state.by_id.remove(&id);
            }
            _ => {}
        }
    }
}

/// Spawn the wlroots focus watcher on its own thread. On bind it marks the provider live;
/// on any fatal error it marks it not-live (so recording stays gated off).
pub fn spawn(state: Arc<FocusState>) {
    std::thread::Builder::new()
        .name("focus-wlr".into())
        .spawn(move || {
            if let Err(e) = run(&state) {
                tracing::warn!("follow-focus(wlr): {e}; recording will be gated off");
            }
            state.set_live(false);
            state.set(None);
        })
        .ok();
}

fn run(state: &Arc<FocusState>) -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<Toplevels>(&conn)?;
    let qh = queue.handle();
    // Hold the manager for the lifetime of the loop (named binding is not dropped early).
    let _manager: ZwlrForeignToplevelManagerV1 = globals
        .bind(&qh, 1..=3, ())
        .map_err(|e| format!("compositor lacks zwlr_foreign_toplevel_manager_v1: {e}"))?;

    state.set_live(true);
    tracing::info!("follow-focus(wlr): foreign-toplevel manager bound");

    let mut toplevels = Toplevels::default();
    loop {
        // Block until events arrive, processing a whole batch; then publish the *net*
        // activated toplevel (so the brief deactivate→activate transition doesn't flap).
        queue.blocking_dispatch(&mut toplevels)?;
        let focused = toplevels
            .by_id
            .values()
            .find(|t| t.activated)
            .map(|t| FocusInfo { app_id: t.app_id.clone(), pid: None });
        state.set(focused);
    }
}
