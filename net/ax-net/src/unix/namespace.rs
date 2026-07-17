//! Filesystem-backed Unix socket namespace hook.
//!
//! Abstract Unix socket names are managed inside ax-net. Path-based Unix socket
//! names are delegated to an optional filesystem provider through this trait.
//!
//! # Integration Boundary
//!
//! The network crate only needs to resolve, create, and remove bind slots for a
//! path. It does not own dentries, permissions, mount namespaces, or lifecycle
//! rules beyond unbinding the slot when the Unix socket transport is dropped.
//! Kernels that do not enable filesystem support can leave this provider
//! unregistered and still use unnamed or abstract Unix sockets.

use alloc::{boxed::Box, sync::Arc};

use ax_errno::{AxResult, ax_err_type};
use ax_kspin::PreemptOnce;

use super::BindSlot;

/// Filesystem slot reserved for a transport bind.
///
/// A dynamically created pathname is removed when transport publication
/// fails. A pre-existing socket node (for example a devfs-provided `/dev/log`)
/// is retained, but its still-empty slot can be retried.
pub struct NamespaceBindSlot {
    slot: Arc<BindSlot>,
    remove_on_rollback: bool,
}

impl NamespaceBindSlot {
    /// Build a reservation for a pathname created by this bind attempt.
    pub fn created(slot: Arc<BindSlot>) -> Self {
        Self {
            slot,
            remove_on_rollback: true,
        }
    }

    /// Build a reservation backed by a pre-existing socket node.
    pub fn preexisting(slot: Arc<BindSlot>) -> Self {
        Self {
            slot,
            remove_on_rollback: false,
        }
    }

    pub(super) fn into_parts(self) -> (Arc<BindSlot>, bool) {
        (self.slot, self.remove_on_rollback)
    }
}

/// Path-based Unix socket namespace provider.
///
/// Provides filesystem backing for Unix domain socket path bindings.
/// Abstract namespace sockets are handled separately within ax-net.
pub trait UnixNamespace: Send + Sync {
    /// Resolve an existing socket path binding.
    fn resolve(&self, path: &str) -> AxResult<Arc<BindSlot>>;

    /// Exclusively create an unpublished socket path binding.
    ///
    /// Implementations may reserve a deliberately pre-created socket node, but
    /// must mark it with [`NamespaceBindSlot::preexisting`]. The caller invokes
    /// [`Self::rollback_bind`] only for paths marked as newly created.
    fn reserve_bind(&self, path: &str) -> AxResult<NamespaceBindSlot>;

    /// Remove an unpublished path created by [`Self::reserve_bind`].
    fn rollback_bind(&self, path: &str) -> AxResult<()>;
}

static UNIX_NS: PreemptOnce<Box<dyn UnixNamespace>> = PreemptOnce::new();

/// Register Unix namespace provider.
///
/// Must be called before using path-based Unix sockets.
pub fn register_unix_namespace(ns: impl UnixNamespace + 'static) {
    UNIX_NS.call_once(|| Box::new(ns));
}

/// Access the registered Unix namespace.
///
/// Returns `AxError::Unsupported` if no filesystem-backed namespace is available.
pub(crate) fn with_namespace<R>(f: impl FnOnce(&dyn UnixNamespace) -> AxResult<R>) -> AxResult<R> {
    match UNIX_NS.get() {
        Some(ns) => f(&**ns),
        None => Err(ax_err_type!(
            Unsupported,
            "Unix socket path operations require filesystem support (enable 'fs-ng' feature)"
        )),
    }
}
