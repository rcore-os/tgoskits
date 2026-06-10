use alloc::{boxed::Box, sync::Arc};

use ax_errno::{AxResult, ax_err_type};

use super::BindSlot;

/// Path-based Unix socket namespace provider.
///
/// Provides filesystem backing for Unix domain socket path bindings.
/// Abstract namespace sockets are handled separately within ax-net.
pub trait UnixNamespace: Send + Sync {
    /// Resolve an existing socket path binding.
    fn resolve(&self, path: &str) -> AxResult<Arc<BindSlot>>;

    /// Create or get a socket path binding.
    fn bind(&self, path: &str) -> AxResult<Arc<BindSlot>>;

    /// Remove a socket path binding.
    fn unbind(&self, path: &str) -> AxResult<()>;
}

static UNIX_NS: spin::Once<Box<dyn UnixNamespace>> = spin::Once::new();

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
            "Unix socket path operations require filesystem support (enable 'fs' or 'fs-ng' \
             feature)"
        )),
    }
}
