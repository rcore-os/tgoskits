use alloc::vec::Vec;
use core::{mem::MaybeUninit, ptr::NonNull};

use axtest::prelude::*;
use starry_vm::{
    VmError, VmMutPtr, VmPtr, vm_load, vm_load_until_nul, vm_read_slice, vm_write_slice,
};

#[axtest]
fn starry_vm_pointer_and_error_mapping_rules_hold() {
    let null_ptr = core::ptr::null::<u32>();
    ax_assert!(null_ptr.nullable().is_none());

    let dangling = NonNull::<u32>::dangling();
    ax_assert!(dangling.nullable().is_some());
    ax_assert_eq!(dangling.as_ptr().vm_read(), Err(VmError::AccessDenied));
    ax_assert_eq!(dangling.vm_write(42), Err(VmError::AccessDenied));

    ax_assert_eq!(
        ax_errno::AxError::from(VmError::BadAddress),
        ax_errno::AxError::BadAddress
    );
    ax_assert_eq!(
        ax_errno::AxError::from(VmError::AccessDenied),
        ax_errno::AxError::BadAddress
    );
    ax_assert_eq!(
        ax_errno::AxError::from(VmError::TooLong),
        ax_errno::AxError::NameTooLong
    );
}

#[axtest]
fn starry_vm_slice_access_rejects_invalid_user_ranges() {
    let mut one_byte = [MaybeUninit::<u8>::uninit()];
    ax_assert_eq!(
        vm_read_slice(core::ptr::null::<u8>(), &mut one_byte),
        Err(VmError::AccessDenied)
    );

    ax_assert_eq!(
        vm_write_slice(core::ptr::null_mut::<u8>(), &[1]),
        Err(VmError::AccessDenied)
    );

    ax_assert_eq!(vm_write_slice(core::ptr::null_mut::<u8>(), &[]), Ok(()));
    ax_assert_eq!(vm_read_slice(core::ptr::null::<u8>(), &mut []), Ok(()));
}

#[axtest]
fn starry_vm_alloc_helpers_validate_bad_inputs_before_copying() {
    let mut unaligned = [0_u16; 2];
    let unaligned_ptr = unaligned
        .as_mut_ptr()
        .cast::<u8>()
        .wrapping_add(1)
        .cast::<u16>();
    ax_assert_eq!(vm_load_until_nul(unaligned_ptr), Err(VmError::BadAddress));

    ax_assert_eq!(
        vm_load(core::ptr::null::<u8>(), 1),
        Err(VmError::AccessDenied)
    );

    let empty: Vec<u8> = vm_load(core::ptr::null::<u8>(), 0).unwrap();
    ax_assert!(empty.is_empty());
}

#[axtest]
fn starry_vm_alloc_is_zero_and_max_bytes_hold() {
    use starry_vm::vm_alloc_is_zero_and_max_bytes_rules_hold_for_test;
    ax_assert!(vm_alloc_is_zero_and_max_bytes_rules_hold_for_test());
}
