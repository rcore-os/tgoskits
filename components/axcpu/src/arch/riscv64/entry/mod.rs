//! Boot and guest entries share the current CPU register layout.

pub(crate) mod boot;
#[cfg(feature = "virtualization")]
mod guest;
#[cfg(feature = "virtualization")]
pub use guest::enter_guest;
