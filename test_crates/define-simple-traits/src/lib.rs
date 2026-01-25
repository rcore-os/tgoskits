//! Define simple traits without default implementations.
//!
//! This crate defines several traits using `#[def_interface]` that do NOT have
//! any default implementations. These traits must be fully implemented.

use crate_interface::def_interface;

/// A simple interface with basic methods.
#[def_interface]
pub trait SimpleIf {
    /// A simple function that returns a u32.
    fn get_value() -> u32;

    /// A function that takes arguments.
    fn compute(a: u32, b: u32) -> u32;

    /// A function that returns a string slice.
    fn get_name() -> &'static str;
}

/// An interface demonstrating namespace feature.
#[def_interface(namespace = SimpleNs)]
pub trait NamespacedIf {
    /// Get an identifier.
    fn get_id() -> i32;
}

/// An interface with gen_caller option.
#[def_interface(gen_caller)]
pub trait CallerIf {
    /// A function that will have a helper caller generated.
    fn add_one(x: i32) -> i32;

    /// Another function with helper caller.
    fn multiply(a: i32, b: i32) -> i32;
}

/// An interface with both namespace and gen_caller.
#[def_interface(gen_caller, namespace = AdvancedNs)]
pub trait AdvancedIf {
    /// Complex computation.
    fn process(input: u64) -> u64;
}
