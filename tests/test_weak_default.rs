#![cfg(feature = "weak_default")]
#![cfg_attr(feature = "weak_default", feature(linkage))]

//! Test weak_default feature.
//!
//! This test requires nightly Rust and the `weak_default` feature to be enabled.
//! Run with: cargo +nightly test --features weak_default --test test_weak_default
//!
//! Note: Weak symbols are designed for cross-crate usage. When `def_interface` is
//! in one crate (with weak symbol default impl) and `impl_interface` is in another
//! crate (with strong symbol), the linker will choose the strong symbol.
//! In the same crate, the strong symbol will cause a compile-time conflict.

use crate_interface::*;

/// A trait with default implementations.
/// When `weak_default` feature is enabled, the default implementation will be
/// generated as a weak symbol, so implementors can choose not to implement it.
#[def_interface]
#[allow(dead_code)]
trait DefaultMethodIf {
    /// Method with default implementation - implementor may skip this.
    fn default_method() -> u32 {
        42
    }

    /// Method with default implementation that takes arguments.
    fn default_with_args(a: u32, b: u32) -> u32 {
        a + b
    }

    /// Method without default implementation - must be implemented.
    fn required_method() -> u32;
}

struct PartialImpl;

/// Only implement the required method, skip the ones with default implementations.
/// The methods with default implementations are NOT implemented here.
/// With `weak_default` feature, the weak symbol from def_interface will be used.
#[impl_interface]
impl DefaultMethodIf for PartialImpl {
    fn required_method() -> u32 {
        100
    }
}

#[test]
fn test_weak_default_methods() {
    // Call the required method - should return 100 (from PartialImpl)
    assert_eq!(call_interface!(DefaultMethodIf::required_method), 100);

    // Call the default methods - should return 42 and sum (from weak symbol default impl)
    assert_eq!(call_interface!(DefaultMethodIf::default_method), 42);
    assert_eq!(
        call_interface!(DefaultMethodIf::default_with_args, 10, 20),
        30
    );
}
