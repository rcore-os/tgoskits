#![feature(linkage)]

use trait_ffi::def_extern_trait;

#[def_extern_trait(weak_default)]
pub trait Optional {
    fn required_but_unused() -> usize;
    fn base() -> usize {
        20
    }
    fn derived() -> usize {
        Self::base() + 1
    }
    fn via_function_pointer() -> usize {
        let method = Self::base;
        method() + 2
    }
    fn via_macro() -> usize {
        macro_rules! invoke {
            ($method:path) => {
                $method()
            };
        }
        invoke!(Self::base) + 3
    }
    fn borrowed(value: &str) -> &str {
        value
    }
    /// # Safety
    /// `value` must point to a readable byte.
    unsafe fn read(value: *const u8) -> u8 {
        unsafe { *value }
    }
}

#[test]
fn defaults_link_without_registering_a_provider() {
    assert_eq!(optional::base(), 20);
    assert_eq!(optional::derived(), 21);
    assert_eq!(optional::via_function_pointer(), 22);
    assert_eq!(optional::via_macro(), 23);
    let value = String::from("borrowed");
    assert_eq!(optional::borrowed(&value), "borrowed");
    // SAFETY: the reference remains valid throughout the call.
    assert_eq!(unsafe { optional::read(&9) }, 9);
}

#[def_extern_trait(weak_default, abi = "C")]
pub trait OptionalC {
    fn add(left: u32, right: u32) -> u32 {
        left + right
    }
}

#[test]
fn c_abi_defaults_link_without_a_provider() {
    assert_eq!(optional_c::add(2, 3), 5);
}

pub struct __DefaultDispatch(pub u8);

#[def_extern_trait(weak_default)]
pub trait QualifiedReturn {
    fn value() -> self::__DefaultDispatch {
        self::__DefaultDispatch(17)
    }
}

#[test]
fn weak_default_qualified_types_are_not_captured_by_the_dispatch_type() {
    assert_eq!(qualified_return::value().0, 17);
}
