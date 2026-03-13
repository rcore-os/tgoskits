//! Integration tests for weak_default traits with FULL implementations.
//!
//! This binary links FullImpl which overrides ALL methods of WeakDefaultIf.
//! It verifies that strong symbols correctly override weak symbol defaults.
//!
//! Exit code 0 means all tests passed.

#![feature(linkage)]

use crate_interface::call_interface;

// Import the implementation crate to link the implementations
use impl_weak_traits::{
    AllDefaultImpl, CallerWeakImpl, FullImpl, NamespacedWeakImpl, SelfRefFullImpl,
};

// Suppress unused warnings - these are used for linking
const _: () = {
    let _ = std::any::type_name::<FullImpl>;
    let _ = std::any::type_name::<AllDefaultImpl>;
    let _ = std::any::type_name::<NamespacedWeakImpl>;
    let _ = std::any::type_name::<CallerWeakImpl>;
    let _ = std::any::type_name::<SelfRefFullImpl>;
};

fn test_full_impl_required_methods() {
    assert_eq!(
        call_interface!(define_weak_traits::WeakDefaultIf::required_value),
        2000
    );
    assert_eq!(
        call_interface!(define_weak_traits::WeakDefaultIf::required_name),
        "FullImpl"
    );
    println!("  [PASS] test_full_impl_required_methods");
}

fn test_full_impl_overridden_defaults() {
    // Strong symbols should win over weak symbols
    assert_eq!(
        call_interface!(define_weak_traits::WeakDefaultIf::default_value),
        99
    );
    assert_eq!(
        call_interface!(define_weak_traits::WeakDefaultIf::default_add, 10, 20),
        200 // 10 * 20, not 10 + 20
    );
    assert_eq!(
        call_interface!(define_weak_traits::WeakDefaultIf::default_greeting),
        "Hello from FullImpl override!"
    );
    println!("  [PASS] test_full_impl_overridden_defaults");
}

fn test_all_default_interface() {
    let a = call_interface!(define_weak_traits::AllDefaultIf::method_a);
    let b = call_interface!(define_weak_traits::AllDefaultIf::method_b);
    let c = call_interface!(define_weak_traits::AllDefaultIf::method_c, 5);

    assert_eq!(a, 111); // Strong symbol override
    assert_eq!(b, 200); // Weak symbol default
    assert_eq!(c, 10); // Weak symbol default: 5 * 2
    println!("  [PASS] test_all_default_interface");
}

fn test_namespaced_weak_interface() {
    let id = call_interface!(namespace = WeakNs, define_weak_traits::NamespacedWeakIf::get_id);
    let multiplier =
        call_interface!(namespace = WeakNs, define_weak_traits::NamespacedWeakIf::get_default_multiplier);

    assert_eq!(id, 12345);
    assert_eq!(multiplier, 10);
    println!("  [PASS] test_namespaced_weak_interface");
}

fn test_caller_weak_interface() {
    let computed = call_interface!(define_weak_traits::CallerWeakIf::compute, 100);
    let offset = call_interface!(define_weak_traits::CallerWeakIf::default_offset);

    assert_eq!(computed, 300); // 100 * 3
    assert_eq!(offset, 1000); // Weak symbol default

    use define_weak_traits::{compute, default_offset};
    assert_eq!(compute(50), 150);
    assert_eq!(default_offset(), 1000);
    println!("  [PASS] test_caller_weak_interface");
}

fn test_mixed_strong_and_weak() {
    for i in 1..5 {
        assert_eq!(
            call_interface!(define_weak_traits::AllDefaultIf::method_a),
            111
        );
        assert_eq!(
            call_interface!(define_weak_traits::AllDefaultIf::method_b),
            200
        );
        assert_eq!(
            call_interface!(define_weak_traits::AllDefaultIf::method_c, i),
            i * 2
        );
    }
    println!("  [PASS] test_mixed_strong_and_weak");
}

/// Test Self::method references with overridden base_value and transform.
///
/// SelfRefFullImpl overrides:
/// - base_value() to return 500 (default: 100)
/// - transform() to multiply by 10 (default: v + 1)
fn test_self_ref_full() {
    // Test direct call: Self::base_value()
    assert_eq!(
        call_interface!(define_weak_traits::SelfRefIf::base_value),
        500
    );

    // derived_value calls Self::base_value() directly
    // 500 * 2 = 1000
    assert_eq!(
        call_interface!(define_weak_traits::SelfRefIf::derived_value),
        1000
    );

    // derived_with_offset calls Self::base_value() directly
    // 500 + 50 = 550
    assert_eq!(
        call_interface!(define_weak_traits::SelfRefIf::derived_with_offset, 50),
        550
    );

    // Test indirect reference: Self::transform as value
    assert_eq!(
        call_interface!(define_weak_traits::SelfRefIf::transform, 5),
        50
    );

    // call_via_ref uses `let f = Self::transform; f(v)` pattern
    assert_eq!(
        call_interface!(define_weak_traits::SelfRefIf::call_via_ref, 5),
        50
    );

    // call_twice applies transform twice: 5 * 10 * 10 = 500
    assert_eq!(
        call_interface!(define_weak_traits::SelfRefIf::call_twice, 5),
        500
    );

    // Verify required_id
    assert_eq!(
        call_interface!(define_weak_traits::SelfRefIf::required_id),
        2
    );

    println!("  [PASS] test_self_ref_full");
}

fn main() {
    println!("Running weak_default trait tests (full implementation)...");

    test_full_impl_required_methods();
    test_full_impl_overridden_defaults();
    test_all_default_interface();
    test_namespaced_weak_interface();
    test_caller_weak_interface();
    test_mixed_strong_and_weak();
    test_self_ref_full();

    println!("All weak_default trait tests (full impl) passed!");
}
