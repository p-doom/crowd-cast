//! AT-SPI focus listener (Linux): flips the secure gate on while a password field is
//! focused. Validated against GTK and Firefox (web `<input type=password>`) on
//! Wayland/sway: the focused element's role is queried per focus-gained event and
//! compared against `Role::PasswordText`.

use super::{SecureInputState, Transition};
use crate::data::{EventType, InputEvent, RedactedEvent};
use anyhow::Result;
use atspi::connection::AccessibilityConnection;
use atspi::events::event_wrappers::ObjectEvents;
use atspi::events::object::StateChangedEvent;
use atspi::proxy::accessible::ObjectRefExt;
use atspi::{Event, Role, State};
use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, info, warn};

pub async fn run(
    state: Arc<SecureInputState>,
    marker_tx: UnboundedSender<InputEvent>,
    enable_accessibility: bool,
) -> Result<()> {
    if enable_accessibility {
        match set_accessibility_enabled(true).await {
            Ok(()) => {
                info!("secure-input: enabled system accessibility so apps expose password fields")
            }
            Err(e) => warn!(
                "secure-input: could not enable accessibility ({e:#}); password-field \
                 detection limited to already-accessible apps"
            ),
        }
    }

    let a11y = AccessibilityConnection::new().await?;
    a11y.register_event::<StateChangedEvent>().await?;

    let events = a11y.event_stream();
    futures::pin_mut!(events);
    info!("secure-input: AT-SPI focus listener active");

    while let Some(ev) = events.next().await {
        let Ok(Event::Object(ObjectEvents::StateChanged(sc))) = ev else {
            continue;
        };
        if sc.state != State::Focused {
            continue;
        }
        // Only focus-GAINED is authoritative: the newly focused element determines the
        // gate. Focus-lost is ignored; the following focus-gained reclassifies. This is
        // reorder-safe and, per policy, never over-suppresses on classification gaps.
        if !sc.enabled {
            continue;
        }

        let role = match sc.item.as_accessible_proxy(a11y.connection()).await {
            Ok(proxy) => proxy.get_role().await.ok(),
            Err(_) => None,
        };
        let secure = role == Some(Role::PasswordText);
        let reason = secure.then(|| "secure-field".to_string());

        match state.set_atspi_secure(secure, reason) {
            Transition::Entered => {
                debug!("secure-input: password field focused; suppressing key capture");
                // Label the gap for post-processing. timestamp_us=0 is re-stamped to
                // recording time by the sync engine before buffering.
                let _ = marker_tx.send(InputEvent {
                    timestamp_us: 0,
                    event: EventType::Redacted(RedactedEvent {
                        reason: "secure-field".to_string(),
                    }),
                });
            }
            Transition::Left => {
                debug!("secure-input: password field blurred; resuming key capture")
            }
            Transition::Unchanged => {}
        }
    }

    warn!("secure-input: AT-SPI event stream ended");
    Ok(())
}

/// Enable system accessibility via the session-bus `org.a11y.Status` interface. This is
/// an unprivileged, per-user, session-scoped change (no root). Already-running apps pick
/// it up dynamically (~200ms); newly launched apps activate at startup.
async fn set_accessibility_enabled(on: bool) -> Result<()> {
    let session = atspi::zbus::Connection::session().await?;
    let proxy = atspi::zbus::Proxy::new(
        &session,
        "org.a11y.Bus",
        "/org/a11y/bus",
        "org.a11y.Status",
    )
    .await?;
    proxy.set_property("IsEnabled", on).await?;
    // Best-effort; some stacks also gate activation on this.
    let _ = proxy.set_property("ScreenReaderEnabled", on).await;
    Ok(())
}
