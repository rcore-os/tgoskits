use trait_ffi_test_interface::{other, platform};
use trait_ffi_test_provider as _;

#[test]
fn renamed_crates_and_private_default_types_link_without_caller_imports() {
    assert_eq!(platform::clock::elapsed().as_nanos(), 123);
    assert_eq!(other::clock::ticks(), 456);
}

#[test]
fn declaration_features_choose_exports_even_when_provider_has_no_feature() {
    assert_eq!(platform::clock::extra(), 99);
}

#[test]
fn relative_type_and_array_paths_resolve_in_the_definition_module() {
    assert_eq!(platform::clock::relative(17), 17);
    assert_eq!(platform::clock::array([3, 4]), [3, 4]);
}

#[test]
fn conditional_attributes_are_selected_by_the_interface_crate() {
    assert_eq!(platform::clock::conditional_default(), 77);
}

#[test]
fn attributed_provider_overrides_weak_defaults_across_renamed_crates() {
    use trait_ffi_test_interface::WeakClock as ClockAlias;
    assert_eq!(trait_ffi::call_interface!(ClockAlias::base()), 90);
    assert_eq!(trait_ffi::call_interface!(ClockAlias::derived), 91);
}
