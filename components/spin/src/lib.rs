#![cfg_attr(all(not(feature = "std"), not(test)), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(missing_docs)]

//! This crate provides [spin-based](https://en.wikipedia.org/wiki/Spinlock) versions of the
//! primitives in `std::sync`. Because synchronization is done through spinning,
//! the primitives are suitable for use in `no_std` environments.
//!
//! # Features
//!
//! - `Once`/`SyncOnceCell` and `LazyLock` equivalents
//!
//! - Support for `no_std` environments
//!
//! - [`lock_api`](https://crates.io/crates/lock_api) compatibility
//!
//! - Guards can be sent and shared between threads
//!
//! - Guard leaking
//!
//! - Ticket locks
//!
//! - Different strategies for dealing with contention
//!
//! # Relationship with `std::sync`
//!
//! While `spin` is not a drop-in replacement for `std::sync` (and
//! [should not be considered as such](https://matklad.github.io/2020/01/02/spinlocks-considered-harmful.html))
//! an effort is made to keep this crate reasonably consistent with `std::sync`.
//!
//! Many of the types defined in this crate have 'additional capabilities' when compared to `std::sync`:
//!
//! - Guards support [leaking](https://doc.rust-lang.org/nomicon/leaking.html).
//!
//! - [`Once`] owns the value returned by its `call_once` initializer.
//!
//! Conversely, the types in this crate do not have some of the features `std::sync` has:
//!
//! - Locks do not track [panic poisoning](https://doc.rust-lang.org/nomicon/poisoning.html).
//!
//! ## Feature flags
//!
//! The crate comes with a few feature flags that you may wish to use.
//!
//! - `lock_api` enables support for [`lock_api`](https://crates.io/crates/lock_api)
//!
//! - `std` enables support for thread yielding instead of spinning
//!
//! - `portable-atomic` enables usage of the `portable-atomic` crate
//!   to support platforms without native atomic operations (Cortex-M0, etc.).
//!   See the documentation for the `portable-atomic` crate for more information
//!   with some requirements for no-std build:
//!   <https://github.com/taiki-e/portable-atomic#optional-features>

#[cfg(any(test, feature = "std"))]
extern crate core;

#[cfg(feature = "portable-atomic")]
extern crate portable_atomic;

#[cfg(not(feature = "portable-atomic"))]
use core::sync::atomic;

#[cfg(feature = "portable-atomic")]
use portable_atomic as atomic;

#[cfg(feature = "lazylock")]
#[cfg_attr(docsrs, doc(cfg(feature = "lazylock")))]
pub mod lazylock;
#[cfg(feature = "once")]
#[cfg_attr(docsrs, doc(cfg(feature = "once")))]
pub mod once;
pub mod relax;

#[cfg(feature = "std")]
#[cfg_attr(docsrs, doc(cfg(feature = "std")))]
pub use relax::Yield;
pub use relax::{RelaxStrategy, Spin};

// Avoid confusing inference errors by aliasing away the relax strategy parameter. Users that need to use a different
// relax strategy can do so by accessing the types through their fully-qualified path. This is a little bit horrible
// but sadly adding a default type parameter is *still* a breaking change in Rust (for understandable reasons).

/// A value which is initialized on the first access. See [`lazylock::LazyLock`] for documentation.
///
/// A note for advanced users: this alias exists to avoid subtle type inference errors due to the default relax
/// strategy type parameter. If you need a non-default relax strategy, use the fully-qualified path.
#[cfg(feature = "lazylock")]
#[cfg_attr(docsrs, doc(cfg(feature = "lazylock")))]
pub type LazyLock<T, F = fn() -> T> = crate::lazylock::LazyLock<T, F>;

/// A type alias to [`LazyLock`] for compatibility reasons.
#[deprecated(note = "use `spin::LazyLock` instead")]
#[cfg(feature = "lazylock")]
#[cfg_attr(docsrs, doc(cfg(feature = "lazylock")))]
pub type Lazy<T, F = fn() -> T> = crate::lazylock::LazyLock<T, F>;

/// A primitive that provides lazy one-time initialization. See [`once::Once`] for documentation.
///
/// A note for advanced users: this alias exists to avoid subtle type inference errors due to the default relax
/// strategy type parameter. If you need a non-default relax strategy, use the fully-qualified path.
#[cfg(feature = "once")]
#[cfg_attr(docsrs, doc(cfg(feature = "once")))]
pub type Once<T = ()> = crate::once::Once<T>;

/// Spin synchronisation primitives, but compatible with [`lock_api`](https://crates.io/crates/lock_api).
#[cfg(feature = "lock_api")]
#[cfg_attr(docsrs, doc(cfg(feature = "lock_api")))]
pub mod lock_api {
}
