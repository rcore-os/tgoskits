//! Full implementation of weak_default traits defined in `define-weak-traits`.
//!
//! This crate provides FULL implementations that override ALL methods,
//! including those with default implementations. This tests that strong
//! symbols correctly override weak symbol defaults.
//!
//! IMPORTANT: This crate requires nightly Rust and `#![feature(linkage)]`.

#![feature(linkage)]

use crate_interface::impl_interface;
use define_weak_traits::{
    AllDefaultIf, CallerWeakIf, NamespacedWeakIf, SelfRefIf, WeakDefaultIf,
};

/// Full implementation - overrides ALL methods including defaults.
/// This creates strong symbols that override the weak symbol defaults.
pub struct FullImpl;

#[impl_interface]
impl WeakDefaultIf for FullImpl {
    fn required_value() -> u32 {
        2000
    }

    fn required_name() -> &'static str {
        "FullImpl"
    }

    // Override the default implementations with strong symbols.
    fn default_value() -> u32 {
        99
    }

    fn default_add(a: u32, b: u32) -> u32 {
        a * b // Multiply instead of add
    }

    fn default_greeting() -> &'static str {
        "Hello from FullImpl override!"
    }
}

/// Implementation for AllDefaultIf - overrides some methods.
pub struct AllDefaultImpl;

#[impl_interface]
impl AllDefaultIf for AllDefaultImpl {
    // Override one method with strong symbol
    fn method_a() -> i32 {
        111
    }
    // method_b() and method_c() will use weak symbol defaults.
}

/// Implementation for NamespacedWeakIf.
pub struct NamespacedWeakImpl;

#[impl_interface(namespace = WeakNs)]
impl NamespacedWeakIf for NamespacedWeakImpl {
    fn get_id() -> u64 {
        12345
    }
    // get_default_multiplier() will use the weak symbol default.
}

/// Implementation for CallerWeakIf.
pub struct CallerWeakImpl;

#[impl_interface]
impl CallerWeakIf for CallerWeakImpl {
    fn compute(x: i64) -> i64 {
        x * 3
    }
    // default_offset() will use the weak symbol default.
}

/// Implementation of SelfRefIf that OVERRIDES base_value and transform.
///
/// This tests that:
/// - derived_value() correctly calls the overridden base_value()
/// - call_via_ref() correctly uses the overridden transform via proxy function
pub struct SelfRefFullImpl;

#[impl_interface]
impl SelfRefIf for SelfRefFullImpl {
    fn required_id() -> u32 {
        2
    }

    // Override base_value to return 500 instead of default 100
    fn base_value() -> u32 {
        500
    }

    // Override transform: multiply by 10 instead of adding 1
    fn transform(v: i32) -> i32 {
        v * 10
    }
    // derived_value(), derived_with_offset(), call_via_ref(), call_twice()
    // all use default implementations with Self:: references
}
