//! OS-specific functionality.

/// ArceOS-specific definitions.
pub mod arceos {
    pub use ax_api as api;
    #[doc(no_inline)]
    pub use ax_api::modules;
}
