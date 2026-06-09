//! OS-specific functionality.

/// ArceOS-specific definitions.
///
/// `api` re-exports the public ArceOS API surface. Prefer this entry for
/// ArceOS-specific operations that do not have a std-like wrapper.
///
/// `modules` re-exports lower-level ArceOS modules as an escape hatch for
/// complex systems such as Axvisor. Ordinary applications should prefer
/// `ax_std::{fs, io, thread, sync, time, net}`.
pub mod arceos {
    /// ArceOS public API facade.
    pub use ax_api as api;
    /// Lower-level ArceOS module facade for system components.
    #[doc(no_inline)]
    pub use ax_api::modules;
}

#[cfg(feature = "std-compat")]
pub mod libc_compat;
