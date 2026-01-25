//! Implement the simple traits defined in `define-simple-traits`.
//!
//! This crate demonstrates that trait definition and implementation can be in
//! separate crates, which is a key feature of `crate_interface`.

use crate_interface::impl_interface;
use define_simple_traits::{AdvancedIf, CallerIf, NamespacedIf, SimpleIf};

/// Implementation struct for SimpleIf.
pub struct SimpleImpl;

#[impl_interface]
impl SimpleIf for SimpleImpl {
    fn get_value() -> u32 {
        12345
    }

    fn compute(a: u32, b: u32) -> u32 {
        a * b + 10
    }

    fn get_name() -> &'static str {
        "SimpleImpl"
    }
}

/// Implementation struct for NamespacedIf.
pub struct NamespacedImpl;

#[impl_interface(namespace = SimpleNs)]
impl NamespacedIf for NamespacedImpl {
    fn get_id() -> i32 {
        999
    }
}

/// Implementation struct for CallerIf.
pub struct CallerImpl;

#[impl_interface]
impl CallerIf for CallerImpl {
    fn add_one(x: i32) -> i32 {
        x + 1
    }

    fn multiply(a: i32, b: i32) -> i32 {
        a * b
    }
}

/// Implementation struct for AdvancedIf.
pub struct AdvancedImpl;

#[impl_interface(namespace = AdvancedNs)]
impl AdvancedIf for AdvancedImpl {
    fn process(input: u64) -> u64 {
        input * 2 + 100
    }
}
