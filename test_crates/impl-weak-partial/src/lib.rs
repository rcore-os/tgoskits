//! Partial implementation of WeakDefaultIf trait.
//!
//! This crate ONLY implements the required methods, relying entirely on
//! weak symbol default implementations for the optional methods.
//!
//! This is a separate crate from impl-weak-traits to allow testing the
//! weak symbol mechanism in isolation (without FullImpl's strong symbols).

#![feature(linkage)]

use crate_interface::impl_interface;
use define_weak_traits::{SelfRefIf, WeakDefaultIf};

/// Partial implementation - only implements required methods.
pub struct PartialOnlyImpl;

#[impl_interface]
impl WeakDefaultIf for PartialOnlyImpl {
    fn required_value() -> u32 {
        5555
    }

    fn required_name() -> &'static str {
        "PartialOnlyImpl"
    }
    // default_value(), default_add(), and default_greeting() are NOT implemented.
    // They will use the weak symbol default implementations from define-weak-traits.
}

/// Partial implementation of SelfRefIf - does NOT override base_value or transform.
///
/// This tests that Self:: references correctly use the default implementations.
pub struct SelfRefPartialImpl;

#[impl_interface]
impl SelfRefIf for SelfRefPartialImpl {
    fn required_id() -> u32 {
        1
    }
    // base_value() is NOT implemented - uses default (100)
    // transform() is NOT implemented - uses default (v + 1)
    // All derived methods use default implementations with Self:: references
}
