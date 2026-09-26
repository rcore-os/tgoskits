//! Live VM registry events for the browser UI (`browser-console` feature).
//!
//! The socket at `/ws/events` — declared in
//! [`crate::control::capability::table`] — upgrades to a WebSocket and pushes
//! two kinds of frame:
//!
//! - one `snapshot` frame at connect with the current list,
//! - one `created` / `removed` / `status` frame per registry change
//!   ([`crate::control::domain::events`]).
//!
//! Frames only tell the browser *that* something changed, with the fields a list
//! needs; `GET /api/vms` and `GET /api/vms/{id}` stay authoritative, so a client
//! that misses a frame is stale, never wrong. The frames are JSON texts carrying
//! `type`, `id`, `name` and `status`; a snapshot carries `vms` instead of the
//! per-VM fields.
//!
//! The socket is read-only: client frames are ignored (they only prove the
//! browser is still there, which `tokio` needs in the same `select!` as the
//! event stream to notice a disconnect).

use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{MethodRouter, get},
};
use serde_json::{Value, json};

use crate::control::domain::events::{EventKind, Subscription, VmEvent};
use crate::manager::AxvmManager;

/// Route for the registry event stream.
pub(crate) fn events_stream_route() -> MethodRouter {
    get(upgrade_events)
}

async fn upgrade_events(
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, StatusCode> {
    // Same cross-origin rule as the console sockets: a page served from another
    // origin must not read the hypervisor's state.
    super::validate_browser_origin(&headers)?;
    let subscription = crate::control::domain::events::subscribe();
    Ok(upgrade
        .on_upgrade(move |socket| stream_events(socket, subscription))
        .into_response())
}

async fn stream_events(mut socket: WebSocket, mut subscription: Subscription) {
    if socket.send(text_frame(snapshot_frame())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            event = subscription.recv() => match event {
                Some(event) => {
                    if socket.send(text_frame(event_frame(&event))).await.is_err() {
                        return;
                    }
                }
                // The watcher dropped the subscription; there is nothing left
                // this socket can report.
                None => return,
            },
            incoming = socket.recv() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(_)) => {}
            },
        }
    }
}

fn text_frame(body: String) -> Message {
    Message::Text(body.into())
}

/// The current VM list, in the shape the UI list needs.
fn snapshot_frame() -> String {
    let vms: Vec<Value> = AxvmManager::vm_list()
        .iter()
        .map(|vm| {
            json!({
                "id": vm.id(),
                "name": vm.name(),
                "status": vm.status().as_str(),
            })
        })
        .collect();
    json!({ "type": "snapshot", "vms": vms }).to_string()
}

fn event_frame(event: &VmEvent) -> String {
    debug_assert!(matches!(
        event.kind,
        EventKind::Created | EventKind::Removed | EventKind::StatusChanged
    ));
    json!({
        "type": event.kind.as_str(),
        "id": event.id,
        "name": event.name,
        "status": event.status,
    })
    .to_string()
}
