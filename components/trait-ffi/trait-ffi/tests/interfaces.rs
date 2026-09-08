use trait_ffi::def_extern_trait;

#[def_extern_trait]
pub trait Clock {
    fn init() -> usize;
    fn borrowed(value: &str) -> &str {
        value
    }
    fn apply(value: &str, callback: fn(&str) -> &str) -> &str {
        callback(value)
    }
    fn byte(value: &u8) -> u8 {
        *value
    }
    fn ignored(_: usize) -> usize {
        9
    }
    #[cfg(any())]
    fn disabled(_: MissingType);
}

#[def_extern_trait(abi = "C")]
pub trait Console {
    fn init() -> usize;
    /// # Safety
    /// `value` must be valid for a read of one byte.
    unsafe fn byte(value: *const u8) -> u8 {
        unsafe { *value }
    }
}

struct Provider;
clock::impl_trait! { impl Clock for Provider { fn init() -> usize { 11 } } }
console::impl_trait! { impl Console for Provider { fn init() -> usize { 22 } } }

#[test]
fn interfaces_with_identical_method_names_link_to_distinct_providers() {
    assert_eq!(clock::init(), 11);
    assert_eq!(console::init(), 22);
}

#[test]
fn borrowed_defaults_and_ignored_arguments_keep_their_contract() {
    let text = String::from("borrowed");
    assert_eq!(clock::borrowed(&text), "borrowed");
    assert_eq!(clock::apply(&text, |value| value), "borrowed");
    assert_eq!(clock::byte(&7), 7);
    assert_eq!(clock::ignored(123), 9);
    // SAFETY: the reference is valid for the synchronous byte read.
    assert_eq!(unsafe { console::byte(&8) }, 8);
}
