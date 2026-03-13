//! Integration tests for weak_default traits with PARTIAL implementation only.
//!
//! This binary ONLY links PartialOnlyImpl, which does NOT implement:
//! - default_value()
//! - default_add()
//! - default_greeting()
//!
//! Therefore, these methods MUST use the weak symbol default implementations.
//!
//! Exit code 0 means all tests passed.

#![feature(linkage)]

use crate_interface::call_interface;

// Import the partial implementation crate to link it
use impl_weak_partial::{PartialOnlyImpl, SelfRefPartialImpl};

// Suppress unused warnings - this is used for linking
const _: () = {
    let _ = std::any::type_name::<PartialOnlyImpl>;
    let _ = std::any::type_name::<SelfRefPartialImpl>;
};

fn test_required_methods() {
    assert_eq!(
        call_interface!(define_weak_traits::WeakDefaultIf::required_value),
        5555
    );
    assert_eq!(
        call_interface!(define_weak_traits::WeakDefaultIf::required_name),
        "PartialOnlyImpl"
    );
    println!("  [PASS] test_required_methods");
}

fn test_weak_default_methods() {
    // These MUST come from weak symbol defaults
    assert_eq!(
        call_interface!(define_weak_traits::WeakDefaultIf::default_value),
        42
    );
    assert_eq!(
        call_interface!(define_weak_traits::WeakDefaultIf::default_add, 10, 20),
        30
    );
    assert_eq!(
        call_interface!(define_weak_traits::WeakDefaultIf::default_add, 100, 200),
        300
    );
    assert_eq!(
        call_interface!(define_weak_traits::WeakDefaultIf::default_greeting),
        "Hello from weak default!"
    );
    println!("  [PASS] test_weak_default_methods");
}

fn test_weak_default_multiple_calls() {
    for i in 0..10 {
        let result = call_interface!(define_weak_traits::WeakDefaultIf::default_add, i, i * 2);
        assert_eq!(result, i + i * 2);
    }
    println!("  [PASS] test_weak_default_multiple_calls");
}

fn test_mixed_required_and_default() {
    let req_val = call_interface!(define_weak_traits::WeakDefaultIf::required_value);
    let def_val = call_interface!(define_weak_traits::WeakDefaultIf::default_value);
    let req_name = call_interface!(define_weak_traits::WeakDefaultIf::required_name);
    let def_greeting = call_interface!(define_weak_traits::WeakDefaultIf::default_greeting);

    assert_eq!(req_val, 5555);
    assert_eq!(def_val, 42);
    assert_eq!(req_name, "PartialOnlyImpl");
    assert_eq!(def_greeting, "Hello from weak default!");
    println!("  [PASS] test_mixed_required_and_default");
}

/// Test Self::method references with default base_value and transform.
///
/// SelfRefPartialImpl does NOT override base_value or transform, so:
/// - base_value() should return 100 (default)
/// - transform(5) should return 6 (default: 5 + 1)
fn test_self_ref_partial() {
    // Test direct call: Self::base_value() uses default
    assert_eq!(
        call_interface!(define_weak_traits::SelfRefIf::base_value),
        100
    );

    // derived_value calls Self::base_value() directly
    // 100 * 2 = 200
    assert_eq!(
        call_interface!(define_weak_traits::SelfRefIf::derived_value),
        200
    );

    // derived_with_offset calls Self::base_value() directly
    // 100 + 50 = 150
    assert_eq!(
        call_interface!(define_weak_traits::SelfRefIf::derived_with_offset, 50),
        150
    );

    // Test indirect reference: Self::transform as value uses default
    assert_eq!(
        call_interface!(define_weak_traits::SelfRefIf::transform, 5),
        6
    );

    // call_via_ref uses `let f = Self::transform; f(v)` pattern
    assert_eq!(
        call_interface!(define_weak_traits::SelfRefIf::call_via_ref, 5),
        6
    );

    // call_twice applies transform twice: (5 + 1) + 1 = 7
    assert_eq!(
        call_interface!(define_weak_traits::SelfRefIf::call_twice, 5),
        7
    );

    // Verify required_id
    assert_eq!(
        call_interface!(define_weak_traits::SelfRefIf::required_id),
        1
    );

    println!("  [PASS] test_self_ref_partial");
}

fn main() {
    println!("Running weak_default trait tests (partial implementation)...");

    test_required_methods();
    test_weak_default_methods();
    test_weak_default_multiple_calls();
    test_mixed_required_and_default();
    test_self_ref_partial();

    println!("All weak_default trait tests (partial impl) passed!");
}
