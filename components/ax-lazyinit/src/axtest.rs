use alloc::{format, string::String, vec::Vec};

use ax_lazyinit::LazyInit;
use axtest::prelude::*;

use crate as ax_lazyinit;

#[axtest]
fn ax_lazyinit_basic_state_transitions_hold() {
    let value: LazyInit<String> = LazyInit::new();
    ax_assert!(!value.is_inited());
    ax_assert!(value.get().is_none());
    ax_assert_eq!(format!("{value:?}"), "LazyInit { <uninitialized> }");

    let initialized = value.init_once(String::from("ready"));
    ax_assert_eq!(initialized.as_str(), "ready");
    ax_assert!(value.is_inited());
    ax_assert_eq!(value.get().unwrap().as_str(), "ready");
    ax_assert_eq!(value.len(), 5);
    ax_assert_eq!(format!("{value:?}"), "LazyInit { data: \"ready\"}");

    let mut mutable: LazyInit<Vec<u8>> = LazyInit::new();
    ax_assert!(mutable.get_mut().is_none());
    mutable.call_once(|| Vec::from([1, 2, 3])).unwrap();
    mutable.get_mut().unwrap().push(4);
    ax_assert_eq!(&**mutable, &[1, 2, 3, 4]);

    ax_assert!(value.call_once(|| String::from("ignored")).is_none());
    ax_assert_eq!(
        value.get_or_init(|| String::from("also ignored")).as_str(),
        "ready"
    );
}

#[axtest]
fn ax_lazyinit_unchecked_access_returns_initialized_storage() {
    let mut value: LazyInit<u32> = LazyInit::default();
    ax_assert_eq!(value.get_or_init(|| 7_u32), &7);

    let shared = unsafe { value.get_unchecked() };
    ax_assert_eq!(*shared, 7);

    let exclusive = unsafe { value.get_mut_unchecked() };
    *exclusive += 5;
    ax_assert_eq!(*value, 12);
}

#[axtest]
fn ax_lazyinit_uninitialized_drop_and_default_formatting_hold() {
    let value: LazyInit<Vec<u8>> = LazyInit::default();
    ax_assert!(!value.is_inited());
    ax_assert_eq!(format!("{value:?}"), "LazyInit { <uninitialized> }");
    drop(value);
}
