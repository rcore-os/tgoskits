use trait_ffi::{call_interface, def_extern_trait, impl_extern_trait};

#[def_extern_trait(gen_caller, namespace = Platform)]
pub trait Arithmetic {
    fn add(left: usize, right: usize) -> usize;
    fn doubled(value: usize) -> usize {
        Self::add(value, value)
    }
}

use Arithmetic as ArithmeticAlias;
struct Provider;

#[impl_extern_trait]
#[cfg_attr(all(), cfg(all()))]
impl ArithmeticAlias for Provider {
    fn add(left: usize, right: usize) -> usize {
        left + right
    }
}

#[test]
fn attribute_binding_and_trait_path_calls_share_the_definition_contract() {
    assert_eq!(call_interface!(Arithmetic::add(2, 3)), 5);
    assert_eq!(call_interface!(crate::Arithmetic::add(4, 5)), 9);
    assert_eq!(call_interface!(ArithmeticAlias::add, 7, 8), 15);
    assert_eq!(call_interface!(Arithmetic::doubled(6)), 12);
    assert_eq!(add(9, 10), 19);
}

#[def_extern_trait]
pub trait Type {
    fn value() -> u8;
}

struct TypeProvider;
#[impl_extern_trait]
impl Type for TypeProvider {
    fn value() -> u8 {
        7
    }
}

#[test]
fn keyword_derived_module_names_still_support_trait_path_calls() {
    assert_eq!(call_interface!(Type::value()), 7);
    assert_eq!(r#type::value(), 7);
    assert_eq!(call_interface!(Crate::value()), 11);
    assert_eq!(crate_api::value(), 11);
}

/// # Safety
/// Providers must always return the documented sentinel.
#[def_extern_trait]
pub unsafe trait Trusted {
    fn sentinel() -> u8;
}

#[impl_extern_trait]
// SAFETY: the sole method returns the documented sentinel 42.
unsafe impl Trusted for Provider {
    fn sentinel() -> u8 {
        42
    }
}

#[test]
fn attribute_binding_preserves_the_unsafe_trait_implementation() {
    assert_eq!(call_interface!(Trusted::sentinel()), 42);
}

#[def_extern_trait(module = "crate_api")]
pub trait Crate {
    fn value() -> u8;
}

#[impl_extern_trait]
impl Crate for TypeProvider {
    fn value() -> u8 {
        11
    }
}
