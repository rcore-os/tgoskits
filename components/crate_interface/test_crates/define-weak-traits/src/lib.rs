//! Define traits with default implementations using weak_default feature.
//!
//! This crate defines traits that have default implementations. With the
//! `weak_default` feature enabled, these default implementations are compiled
//! as weak symbols, allowing implementors to optionally override them.
//!
//! IMPORTANT: This crate requires nightly Rust and `#![feature(linkage)]`.

#![feature(linkage)]

use crate_interface::def_interface;

/// A trait with some methods having default implementations.
///
/// Implementors can choose to implement only the required methods (those without
/// defaults), and the default implementations will be used for the rest via
/// weak symbol linkage.
#[def_interface]
pub trait WeakDefaultIf {
    /// A required method - must be implemented.
    fn required_value() -> u32;

    /// A method with default implementation - can be skipped.
    fn default_value() -> u32 {
        42
    }

    /// A method with default implementation that uses arguments.
    fn default_add(a: u32, b: u32) -> u32 {
        a + b
    }

    /// Another required method.
    fn required_name() -> &'static str;

    /// Default implementation returning a constant string.
    fn default_greeting() -> &'static str {
        "Hello from weak default!"
    }
}

/// A trait where ALL methods have default implementations.
/// Implementors can implement an empty impl block (just to register the implementation).
#[def_interface]
pub trait AllDefaultIf {
    /// Default method 1.
    fn method_a() -> i32 {
        100
    }

    /// Default method 2.
    fn method_b() -> i32 {
        200
    }

    /// Default method with computation.
    fn method_c(x: i32) -> i32 {
        x * 2
    }
}

/// A trait with namespace and default implementations.
#[def_interface(namespace = WeakNs)]
pub trait NamespacedWeakIf {
    /// Required method.
    fn get_id() -> u64;

    /// Default method.
    fn get_default_multiplier() -> u64 {
        10
    }
}

/// A trait with gen_caller and default implementations.
#[def_interface(gen_caller)]
pub trait CallerWeakIf {
    /// Required method with helper caller.
    fn compute(x: i64) -> i64;

    /// Default method with helper caller.
    fn default_offset() -> i64 {
        1000
    }
}

/// A trait demonstrating Self::method references in default implementations.
///
/// This tests both:
/// 1. Direct calls: `Self::base_value()` - method called directly
/// 2. Indirect references: `Self::transform` - method used as a value/function pointer
///
/// Both cases are handled uniformly by generating proxy functions.
#[def_interface]
pub trait SelfRefIf {
    /// Base method with default implementation.
    /// Can be overridden by implementors.
    fn base_value() -> u32 {
        100
    }

    /// Derived method that calls `base_value()` directly.
    fn derived_value() -> u32 {
        Self::base_value() * 2
    }

    /// Derived method using base_value with additional computation.
    fn derived_with_offset(offset: u32) -> u32 {
        Self::base_value() + offset
    }

    /// Transform function that can be overridden.
    fn transform(v: i32) -> i32 {
        v + 1
    }

    /// Method that uses `Self::transform` as a value (indirect reference).
    fn call_via_ref(v: i32) -> i32 {
        let f = Self::transform;
        f(v)
    }

    /// Method that uses multiple indirect references to the same method.
    fn call_twice(v: i32) -> i32 {
        let f1 = Self::transform;
        let f2 = Self::transform;
        f2(f1(v))
    }

    /// Required method to register the impl.
    fn required_id() -> u32;
}
