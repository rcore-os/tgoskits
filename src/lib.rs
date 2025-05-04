#![doc = include_str!("../README.md")]
#![no_std]
#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

extern crate alloc;

mod arc;

mod def;
pub use def::{RESOURCES, ResCurrent, ResWrapper, Resource};

mod ns;
pub use ns::Namespace;

/// Get a static reference to the global namespace.
///
/// This is useful for threads that do not have a isolated namespace but use the
/// global namespace. Typically you should call this in your implementation of
/// [`CurrentNs`].
pub fn global_ns() -> &'static Namespace {
    use spin::Lazy;
    static NS: Lazy<Namespace> = Lazy::new(Namespace::new);
    &NS
}

/// Get the current namespace.
///
/// Most of the time, namespaces are likely to be stored in containers with
/// internal mutability such as `RefCell`/`Mutex`, and your implementation will
/// need to be a RAII guard.
///
/// # Safety
/// See [`extern_trait`].
#[cfg(feature = "thread-local")]
// FIXME: why doc_auto_cfg doesn't work?
#[cfg_attr(docsrs, doc(cfg(feature = "thread-local")))]
#[extern_trait::extern_trait(CurrentNsImpl)]
pub unsafe trait CurrentNs: AsRef<Namespace> {
    /// Get an instance of [`CurrentNs`].
    fn new() -> Self;
}

#[cfg(not(feature = "thread-local"))]
struct CurrentNsImpl;

#[cfg(not(feature = "thread-local"))]
impl AsRef<Namespace> for CurrentNsImpl {
    fn as_ref(&self) -> &Namespace {
        global_ns()
    }
}

fn current_ns() -> CurrentNsImpl {
    #[cfg(feature = "thread-local")]
    return CurrentNsImpl::new();
    #[cfg(not(feature = "thread-local"))]
    CurrentNsImpl
}
