//! [ArceOS](https://github.com/arceos-org/arceos) namespaces module.
//!
//! Namespaces are used to control system resource sharing between threads. This
//! module provides a unified interface to access system resources in different
//! scenarios.
//!
//! For a unikernel, there is only one global namespace, so all threads share
//! the same system resources, such as virtual address space, working directory,
//! and file descriptors, etc.
//!
//! For a monolithic kernel, each process corresponds to a namespace, all
//! threads in the same process share the same system resources. Different
//! processes have different namespaces and isolated resources.
//!
//! For further container support, some global system resources can also be
//! grouped into a namespace.

#![no_std]

extern crate alloc;

mod arc;

mod def;
pub use def::{RESOURCES, ResCurrent, ResWrapper, Resource};

mod ns;
pub use ns::Namespace;

pub fn global_ns() -> &'static Namespace {
    use spin::Lazy;
    static NS: Lazy<Namespace> = Lazy::new(Namespace::new);
    &NS
}

/// Get the current namespace.
/// # Safety
/// See [`extern_trait`].
#[cfg(feature = "thread-local")]
#[extern_trait::extern_trait(CurrentNsImpl)]
pub unsafe trait CurrentNs: AsRef<Namespace> {
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
