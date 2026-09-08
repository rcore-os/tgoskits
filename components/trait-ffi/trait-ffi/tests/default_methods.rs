use trait_ffi::def_extern_trait;

#[def_extern_trait(impl_macro = "impl_trait")]
pub trait Defaults {
    fn required() -> usize;
    fn inherited() -> usize {
        Self::required() + 1
    }
}

struct Provider;
impl_trait! {
    impl Defaults for Provider {
        fn required() -> usize { 41 }
    }
}

#[test]
fn omitted_default_method_is_exported_and_calls_the_provider() {
    assert_eq!(defaults::inherited(), 42);
}
