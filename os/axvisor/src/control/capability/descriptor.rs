//! `GET /api/manifest` — what this hypervisor build can serve.
//!
//! The browser frontend builds its navigation from this response: a panel that
//! is not declared here has no UI, and every declared panel is backed by the
//! routes the same build registered. The response is a projection of
//! [`super::table`], not a second declaration, so it cannot promise an
//! operation the router does not serve — which the hand-written tables this
//! replaces did, by listing a `GET /ws/vm-{id}` route that never existed.
//!
//! Each panel carries five things: `kind` (the panel the frontend renders),
//! `title` (the label it renders, in the language the dashboard is written in),
//! `root` (the panel's URL namespace), `verbs` (what the panel can do as a
//! summary), and `links`, one entry per operation:
//!
//! | Field | Meaning |
//! | --- | --- |
//! | `name` | operation name, unique within the panel |
//! | `verb` | `read`, `write`, or `stream` |
//! | `method` | HTTP method of the link |
//! | `href` | path template, with `{id}` and `{endpoint}` placeholders |
//!
//! `verbs` is the panel's summary and `links` is its URL map, which is why the
//! two are not the same list: a console socket is one `stream` link that reads,
//! writes, and streams.
//!
//! VM links are not narrowed per VM status — `GET /api/vms/{id}` reports the
//! status and the control plane rejects a transition the state machine does not
//! allow, so a panel decides which controls to offer from the status it already
//! has.

use alloc::vec::Vec;

use axum::Json;
use serde_json::{Value, json};

use super::table;

/// Control-plane contract version. Bumped when the response shape changes in a
/// way an older frontend cannot ignore.
const PROTO: u8 = 1;

pub(crate) async fn get_manifest() -> Json<Value> {
    Json(json!({
        "proto": PROTO,
        "panels": panels(),
    }))
}

fn panels() -> Vec<Value> {
    table::resources()
        .iter()
        .map(|resource| {
            let verbs: Vec<&'static str> =
                resource.verbs.iter().map(|verb| verb.as_str()).collect();
            let links: Vec<Value> = resource
                .endpoints
                .iter()
                .map(|endpoint| {
                    json!({
                        "name": endpoint.name,
                        "verb": endpoint.verb.as_str(),
                        "method": endpoint.method.as_str(),
                        "href": endpoint.path,
                    })
                })
                .collect();
            json!({
                "kind": resource.kind,
                "title": resource.title,
                "root": resource.root,
                "verbs": verbs,
                "links": links,
            })
        })
        .collect()
}
