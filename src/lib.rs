//! Utilities for working with user-space pointers.
#![no_std]
#![feature(layout_for_ptr)]
#![feature(maybe_uninit_slice)]
#![feature(pointer_is_aligned_to)]
#![feature(ptr_as_uninit)]
#![feature(ptr_sub_ptr)]

use core::{alloc::Layout, ptr::NonNull};

use axerrno::{AxError, AxResult};
use crate_interface::def_interface;
use memory_addr::VirtAddrRange;

/// The interface for checking user memory access.
#[def_interface]
pub trait AxPtrIf {
    /// Acquires a guard for checking user memory access.
    fn acquire_guard() -> NonNull<()>;

    /// Tries to access a specific range of user memory.
    ///
    /// This function should also populate the memory area.
    ///
    /// Returns `Ok(())` if the access is allowed and the memory area can be
    /// populated.
    fn access_range(guard: NonNull<()>, range: VirtAddrRange, write: bool) -> AxResult;

    /// Frees the guard returned by [`AxPtrIf::acquire_guard`].
    fn free_guard(guard: NonNull<()>);
}

struct Guard(NonNull<()>);

impl Guard {
    fn new() -> Self {
        Self(crate_interface::call_interface!(AxPtrIf::acquire_guard))
    }

    fn access_range(&self, range: VirtAddrRange, write: bool) -> AxResult {
        crate_interface::call_interface!(AxPtrIf::access_range(self.0, range, write))
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        crate_interface::call_interface!(AxPtrIf::free_guard(self.0));
    }
}

fn check_access<T: ?Sized>(ptr: *const T, write: bool) -> AxResult {
    let layout = unsafe { Layout::for_value_raw(ptr) };
    if !ptr.is_aligned_to(layout.align()) {
        return Err(AxError::BadAddress);
    }

    let range = VirtAddrRange::from_start_size(ptr.addr().into(), layout.size());
    Guard::new().access_range(range, write)?;
    Ok(())
}

mod thin;
pub use thin::{UserMutPtr, UserPtr};

mod slice;
pub use slice::{
    UserMutSlicePtr, UserSlicePtr, cstr_until_nul, slice_until_nul, slice_until_nul_mut,
};
