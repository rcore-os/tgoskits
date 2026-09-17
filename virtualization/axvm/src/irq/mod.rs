//! VM interrupt delivery models and deferred runtime wake plumbing.

#[cfg(any(not(target_arch = "loongarch64"), test))]
pub(crate) mod deferred;
pub(crate) mod model;
pub(crate) mod sender;
