//! The one authority for what this build serves.
//!
//! Every route Axvisor answers appears here exactly once. Both consumers walk
//! this table instead of restating it: [`router`] turns each entry into the
//! Axum route its `method` and `path` name, and [`super::descriptor`] turns the
//! same entries into the links the dashboard navigates by. Editing one place is
//! therefore enough to change both, which is the property the two hand-written
//! tables it replaces did not have.
//!
//! # Endpoints
//!
//! The paths below are the ones the hypervisor already served; this is where
//! they moved to, not where they changed. Status codes belong to the handlers
//! and stay documented next to them.
//!
//! | Method | Path | Handler | Entry |
//! | --- | --- | --- | --- |
//! | GET | `/api/manifest` | `descriptor::get_manifest` | (bootstrap, not a panel) |
//! | GET | `/api/vms` | `vm::list_vms` | list |
//! | GET | `/api/vms/schema` | `vm::vm_schema` | schema |
//! | GET | `/api/vms/{id}` | `vm::vm_detail` | detail |
//! | DELETE | `/api/vms/{id}` | `vm::vm_delete` | delete |
//! | POST | `/api/vms/create` | `vm::vm_create` | create |
//! | POST | `/api/vms/{id}/start` | `vm::vm_start` | start |
//! | POST | `/api/vms/{id}/stop` | `vm::vm_stop` | stop |
//! | POST | `/api/vms/{id}/pause` | `vm::vm_pause` | pause |
//! | POST | `/api/vms/{id}/resume` | `vm::vm_resume` | resume |
//! | GET | `/api/vms/pool` | `vm::vm_pool` | pool (`fs`) |
//! | POST | `/api/vms/pool` | `vm::vm_pool_save` | pool_save (`fs`) |
//! | GET | `/api/vms/browse` | `vm::vm_browse` | browse (`fs`) |
//! | GET | `/api/files` | `files::list_files` | list (`fs`) |
//! | POST | `/api/files` | `files::open_file` | open (`fs`) |
//! | HEAD | `/api/files/{id}` | `files::resume_file` | resume (`fs`) |
//! | PATCH | `/api/files/{id}` | `files::send_chunk` | send (`fs`) |
//! | POST | `/api/files/{id}/place` | `files::place_file` | place (`fs`) |
//! | DELETE | `/api/files/{id}` | `files::drop_file` | drop (`fs`) |
//! | POST | `/api/files/dirs` | `files::make_directory` | mkdir (`fs`) |
//! | GET | `/ws/events` | `events::upgrade_events` | events (`http-axum` + `browser-console`) |
//! | GET | `/api/consoles` | `browser_console::console_descriptions` | list |
//! | GET | `/ws/{endpoint}` | `browser_console::upgrade_console` | stream |
//!
//! An id that is not registered yet may still be startable: with the `fs`
//! feature, `start` creates a VM from the matching config in the VM pool first
//! (see [`crate::control::domain::pool`]), and `/api/vms/pool` reports those
//! candidates. Starting an id that is neither registered nor pooled returns 404.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use axum::Router;
#[cfg(all(feature = "fs", feature = "http-axum"))]
use axum::extract::DefaultBodyLimit;
use axum::routing::get;
#[cfg(feature = "http-axum")]
use axum::routing::{delete, post};
#[cfg(all(feature = "fs", feature = "http-axum"))]
use axum::routing::{head, patch};

#[cfg(all(feature = "fs", feature = "http-axum"))]
use crate::control::transport::api::files;
#[cfg(feature = "http-axum")]
use crate::control::transport::api::vm;
#[cfg(feature = "browser-console")]
use crate::control::transport::browser_console;
#[cfg(all(feature = "browser-console", feature = "http-axum"))]
use crate::control::transport::events;

use super::{Endpoint, MANIFEST_PATH, Method, Resource, Verb};

/// The router for everything this build serves.
///
/// A path may appear more than once — `GET /api/vms/{id}` and
/// `DELETE /api/vms/{id}` are two entries on one route — and Axum merges those.
/// The same `(path, method)` appearing twice is registered once instead: the
/// console and shell panels share `/ws/{endpoint}`, and mounting the same
/// handler twice would panic rather than serve.
pub fn router() -> Router {
    let mut router = Router::new();
    let mut mounted: BTreeSet<(&'static str, Method)> = BTreeSet::new();

    router = router.route(MANIFEST_PATH, get(super::descriptor::get_manifest));

    for resource in resources() {
        for endpoint in resource.endpoints {
            if mounted.insert((endpoint.path, endpoint.method)) {
                router = router.route(endpoint.path, (endpoint.build)());
            }
        }
    }

    router
}

/// The resource kinds this build serves, in the order the dashboard shows them.
///
/// A kind is present exactly when its routes are: there is no way to declare a
/// panel this build cannot answer, which is what the manifest used to get wrong
/// when it listed `GET /ws/vm-{id}` for a route that never existed.
pub fn resources() -> Vec<&'static Resource> {
    let mut all = Vec::new();

    #[cfg(feature = "http-axum")]
    all.push(&VMS_RESOURCE);

    #[cfg(all(feature = "fs", feature = "http-axum"))]
    all.push(&FILES_RESOURCE);

    #[cfg(feature = "browser-console")]
    all.push(&CONSOLE_RESOURCE);

    #[cfg(feature = "browser-console")]
    all.push(&SHELL_RESOURCE);

    all
}

#[cfg(feature = "http-axum")]
static VMS_RESOURCE: Resource = Resource {
    kind: "vms",
    title: "虚拟机",
    root: "/api/vms",
    verbs: &[Verb::Read, Verb::Write],
    endpoints: VMS_ENDPOINTS,
};

#[cfg(feature = "http-axum")]
static VMS_ENDPOINTS: &[Endpoint] = &[
    Endpoint {
        name: "list",
        verb: Verb::Read,
        method: Method::Get,
        path: "/api/vms",
        build: || get(vm::list_vms),
    },
    // The creation form's field set: a read of the configuration model, so it
    // sits with the other reads of this panel rather than in a panel of its own —
    // the panel that creates a VM is the one that asks for it.
    Endpoint {
        name: "schema",
        verb: Verb::Read,
        method: Method::Get,
        path: "/api/vms/schema",
        build: || get(vm::vm_schema),
    },
    Endpoint {
        name: "detail",
        verb: Verb::Read,
        method: Method::Get,
        path: "/api/vms/{id}",
        build: || get(vm::vm_detail),
    },
    // The pool is a directory on the host filesystem, so its routes only exist
    // in builds that can read one.
    #[cfg(feature = "fs")]
    Endpoint {
        name: "pool",
        verb: Verb::Read,
        method: Method::Get,
        path: "/api/vms/pool",
        build: || get(vm::vm_pool),
    },
    #[cfg(feature = "fs")]
    Endpoint {
        name: "pool_save",
        verb: Verb::Write,
        method: Method::Post,
        path: "/api/vms/pool",
        build: || post(vm::vm_pool_save),
    },
    #[cfg(feature = "fs")]
    Endpoint {
        name: "browse",
        verb: Verb::Read,
        method: Method::Get,
        path: "/api/vms/browse",
        build: || get(vm::vm_browse),
    },
    // Only the dashboard subscribes to registry changes, and reaching the
    // stream needs both the socket feature and a panel to hang it on: without
    // `http-axum` there is no `vms` panel, so the watcher has no subscriber to
    // wake and the route would be one nothing could name.
    #[cfg(all(feature = "http-axum", feature = "browser-console"))]
    Endpoint {
        name: "events",
        verb: Verb::Read,
        method: Method::Get,
        path: "/ws/events",
        build: events::events_stream_route,
    },
    Endpoint {
        name: "delete",
        verb: Verb::Write,
        method: Method::Delete,
        path: "/api/vms/{id}",
        build: || delete(vm::vm_delete),
    },
    Endpoint {
        name: "create",
        verb: Verb::Write,
        method: Method::Post,
        path: "/api/vms/create",
        build: || post(vm::vm_create),
    },
    Endpoint {
        name: "start",
        verb: Verb::Write,
        method: Method::Post,
        path: "/api/vms/{id}/start",
        build: || post(vm::vm_start),
    },
    Endpoint {
        name: "stop",
        verb: Verb::Write,
        method: Method::Post,
        path: "/api/vms/{id}/stop",
        build: || post(vm::vm_stop),
    },
    Endpoint {
        name: "pause",
        verb: Verb::Write,
        method: Method::Post,
        path: "/api/vms/{id}/pause",
        build: || post(vm::vm_pause),
    },
    Endpoint {
        name: "resume",
        verb: Verb::Write,
        method: Method::Post,
        path: "/api/vms/{id}/resume",
        build: || post(vm::vm_resume),
    },
];

#[cfg(all(feature = "fs", feature = "http-axum"))]
static FILES_RESOURCE: Resource = Resource {
    kind: "files",
    title: "文件",
    root: "/api/files",
    verbs: &[Verb::Read, Verb::Write],
    endpoints: FILES_ENDPOINTS,
};

/// The transfer steps, in the order they follow each other.
///
/// One upload is split into the steps an interrupted transfer needs rather than
/// offered as a single call: `open` names the target directory and the length,
/// `send` appends one chunk, `resume` reports where the bytes stopped, `place`
/// moves the finished file to its final name and `drop` forgets an attempt.
/// `list` is what shows a client which objects are waiting to be placed; a
/// session whose bytes are still arriving is deliberately absent from it.
#[cfg(all(feature = "fs", feature = "http-axum"))]
static FILES_ENDPOINTS: &[Endpoint] = &[
    Endpoint {
        name: "list",
        verb: Verb::Read,
        method: Method::Get,
        path: "/api/files",
        build: || get(files::list_files),
    },
    Endpoint {
        name: "open",
        verb: Verb::Write,
        method: Method::Post,
        path: "/api/files",
        build: || post(files::open_file),
    },
    // A transfer target has to exist already, so the interface has to be able to
    // walk to one. The read is the same guest-filesystem walk the configuration
    // pool browses with, declared here because a panel may only use the
    // operations its own resource declares.
    Endpoint {
        name: "browse",
        verb: Verb::Read,
        method: Method::Get,
        path: "/api/files/browse",
        build: || get(files::browse),
    },
    Endpoint {
        name: "resume",
        verb: Verb::Read,
        method: Method::Head,
        path: "/api/files/{id}",
        build: || head(files::resume_file),
    },
    // The only route with a body limit of its own: one chunk per request is what
    // bounds both the memory a transfer can hold and the time it holds the
    // control-plane thread, so the limit comes from the same constant the
    // handler reads the body with.
    Endpoint {
        name: "send",
        verb: Verb::Write,
        method: Method::Patch,
        path: "/api/files/{id}",
        build: || {
            patch(files::send_chunk).layer(DefaultBodyLimit::max(
                crate::control::domain::files::CHUNK_LIMIT,
            ))
        },
    },
    Endpoint {
        name: "place",
        verb: Verb::Write,
        method: Method::Post,
        path: "/api/files/{id}/place",
        build: || post(files::place_file),
    },
    Endpoint {
        name: "drop",
        verb: Verb::Write,
        method: Method::Delete,
        path: "/api/files/{id}",
        build: || delete(files::drop_file),
    },
    // Not part of a transfer: this is the interface's "new folder", and it is
    // here because the transfer refuses a target directory that is not there.
    Endpoint {
        name: "mkdir",
        verb: Verb::Write,
        method: Method::Post,
        path: "/api/files/dirs",
        build: || post(files::make_directory),
    },
];

#[cfg(feature = "browser-console")]
static CONSOLE_RESOURCE: Resource = Resource {
    kind: "console",
    title: "客户机终端",
    root: "/api/consoles",
    verbs: &[Verb::Read, Verb::Write, Verb::Stream],
    endpoints: &[
        Endpoint {
            name: "list",
            verb: Verb::Read,
            method: Method::Get,
            path: "/api/consoles",
            build: browser_console::console_list_route,
        },
        Endpoint {
            name: "stream",
            verb: Verb::Stream,
            method: Method::Get,
            path: "/ws/{endpoint}",
            build: browser_console::console_stream_route,
        },
    ],
};

/// The management lane.
///
/// It has no route of its own: `/ws/{endpoint}` already serves it, and the
/// console gateway names the management endpoint. Declaring the socket again
/// under this kind is what tells the dashboard that a management terminal
/// exists and that its link is the same template with a different parameter.
#[cfg(feature = "browser-console")]
static SHELL_RESOURCE: Resource = Resource {
    kind: "shell",
    title: "管理终端",
    root: "/ws",
    verbs: &[Verb::Read, Verb::Write, Verb::Stream],
    endpoints: &[Endpoint {
        name: "stream",
        verb: Verb::Stream,
        method: Method::Get,
        path: "/ws/{endpoint}",
        build: browser_console::console_stream_route,
    }],
};
