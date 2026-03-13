//! Integration tests for simple traits (without weak_default).
//!
//! This binary tests that:
//! 1. Traits can be defined in one crate and implemented in another
//! 2. call_interface! macro works correctly across crates
//! 3. Namespaced traits work correctly
//! 4. gen_caller helper functions work correctly
//!
//! Exit code 0 means all tests passed.

use crate_interface::call_interface;

// Import the implementation crate to link the implementations
use impl_simple_traits::{AdvancedImpl, CallerImpl, NamespacedImpl, SimpleImpl};

// Suppress unused warnings - these are used for linking
const _: () = {
    let _ = std::any::type_name::<SimpleImpl>;
    let _ = std::any::type_name::<NamespacedImpl>;
    let _ = std::any::type_name::<CallerImpl>;
    let _ = std::any::type_name::<AdvancedImpl>;
};

fn test_simple_interface() {
    assert_eq!(
        call_interface!(define_simple_traits::SimpleIf::get_value),
        12345
    );
    assert_eq!(
        call_interface!(define_simple_traits::SimpleIf::compute, 10, 5),
        60 // 10 * 5 + 10 = 60
    );
    assert_eq!(
        call_interface!(define_simple_traits::SimpleIf::get_name),
        "SimpleImpl"
    );
    println!("  [PASS] test_simple_interface");
}

fn test_namespaced_interface() {
    assert_eq!(
        call_interface!(namespace = SimpleNs, define_simple_traits::NamespacedIf::get_id),
        999
    );
    println!("  [PASS] test_namespaced_interface");
}

fn test_caller_interface() {
    assert_eq!(
        call_interface!(define_simple_traits::CallerIf::add_one, 41),
        42
    );
    assert_eq!(
        call_interface!(define_simple_traits::CallerIf::multiply, 6, 7),
        42
    );

    // Test gen_caller helper functions
    use define_simple_traits::{add_one, multiply};
    assert_eq!(add_one(99), 100);
    assert_eq!(multiply(3, 4), 12);
    println!("  [PASS] test_caller_interface");
}

fn test_advanced_interface() {
    assert_eq!(
        call_interface!(namespace = AdvancedNs, define_simple_traits::AdvancedIf::process, 50),
        200 // 50 * 2 + 100 = 200
    );

    use define_simple_traits::process;
    assert_eq!(process(100), 300); // 100 * 2 + 100 = 300
    println!("  [PASS] test_advanced_interface");
}

fn test_multiple_calls() {
    for i in 0..10 {
        let result = call_interface!(define_simple_traits::SimpleIf::compute, i, i);
        assert_eq!(result, i * i + 10);
    }
    println!("  [PASS] test_multiple_calls");
}

fn main() {
    println!("Running simple trait tests...");

    test_simple_interface();
    test_namespaced_interface();
    test_caller_interface();
    test_advanced_interface();
    test_multiple_calls();

    println!("All simple trait tests passed!");
}
