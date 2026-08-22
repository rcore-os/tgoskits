extern crate alloc;

use alloc::{format, string::String, vec::Vec};

use ax_lazyinit::LazyInit;

#[test]
fn ax_lazyinit_basic_state_transitions_hold() {
    let value: LazyInit<String> = LazyInit::new();
    assert!(!value.is_inited());
    assert!(value.get().is_none());
    assert_eq!(format!("{value:?}"), "LazyInit { <uninitialized> }");

    let initialized = value.init_once(String::from("ready"));
    assert_eq!(initialized.as_str(), "ready");
    assert!(value.is_inited());
    assert_eq!(value.get().unwrap().as_str(), "ready");
    assert_eq!(value.len(), 5);
    assert_eq!(format!("{value:?}"), "LazyInit { data: \"ready\"}");

    let mut mutable: LazyInit<Vec<u8>> = LazyInit::new();
    assert!(mutable.get_mut().is_none());
    mutable.call_once(|| Vec::from([1, 2, 3])).unwrap();
    mutable.get_mut().unwrap().push(4);
    assert_eq!(&**mutable, &[1, 2, 3, 4]);

    assert!(value.call_once(|| String::from("ignored")).is_none());
    assert_eq!(
        value.get_or_init(|| String::from("also ignored")).as_str(),
        "ready"
    );
}

#[test]
fn ax_lazyinit_unchecked_access_returns_initialized_storage() {
    let mut value: LazyInit<u32> = LazyInit::default();
    assert_eq!(value.get_or_init(|| 7_u32), &7);

    let shared = unsafe { value.get_unchecked() };
    assert_eq!(*shared, 7);

    let exclusive = unsafe { value.get_mut_unchecked() };
    *exclusive += 5;
    assert_eq!(*value, 12);
}

#[test]
fn ax_lazyinit_uninitialized_drop_and_default_formatting_hold() {
    let value: LazyInit<Vec<u8>> = LazyInit::default();
    assert!(!value.is_inited());
    assert_eq!(format!("{value:?}"), "LazyInit { <uninitialized> }");
    drop(value);
}
