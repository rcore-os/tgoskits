//! Virtual memory utilities.
#![no_std]
#![feature(maybe_uninit_as_bytes)]
#![warn(missing_docs)]

use core::{mem::MaybeUninit, slice};

use ax_errno::AxError;
use extern_trait::extern_trait;

/// Errors that can occur during virtual memory operations.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum VmError {
    /// The address is invalid, e.g., not aligned to the required boundary,
    /// out of bounds (including null).
    BadAddress,
    /// The operation is not allowed, e.g., trying to write to read-only memory.
    AccessDenied,
    /// The C-style string or array is too long.
    ///
    /// This error is returned by [`vm_load_until_nul`] when the null terminator
    /// is not found within a predefined search limit.
    #[cfg(feature = "alloc")]
    TooLong,
}

impl From<VmError> for AxError {
    fn from(err: VmError) -> Self {
        match err {
            VmError::BadAddress | VmError::AccessDenied => AxError::BadAddress,
            #[cfg(feature = "alloc")]
            VmError::TooLong => AxError::NameTooLong,
        }
    }
}

/// A result type for virtual memory operations.
pub type VmResult<T = ()> = Result<T, VmError>;

/// The interface for accessing virtual memory.
///
/// # Safety
///
/// - The implementation must ensure that the memory accesses are safe and do
///   not violate any memory safety rules.
#[extern_trait(VmImpl)]
pub unsafe trait VmIo {
    /// Creates an instance of [`VmIo`].
    ///
    /// This is used for implementations which might need to store some state or
    /// data to perform the operations. Implementators may leave this empty
    /// if no state is needed.
    fn new() -> Self;

    /// Reads data from the virtual memory starting at `start` into `buf`.
    fn read(&mut self, start: usize, buf: &mut [MaybeUninit<u8>]) -> VmResult;

    /// Writes data to the virtual memory starting at `start` from `buf`.
    fn write(&mut self, start: usize, buf: &[u8]) -> VmResult;
}

/// Reads a slice from the virtual memory.
///
/// The user pointer need NOT be aligned to `align_of::<T>()`. The underlying
/// `user_copy` is byte-granular on every arch (x86 `rep movsb`;
/// aarch64/riscv64/ loongarch64 byte-align the destination first, then
/// bulk-copy) — exactly like Linux `copy_from_user`, which never requires
/// user-buffer alignment. The old `is_aligned()` gate wrongly rejected valid
/// unaligned user buffers.
pub fn vm_read_slice<T>(ptr: *const T, buf: &mut [MaybeUninit<T>]) -> VmResult {
    VmImpl::new().read(ptr.addr(), buf.as_bytes_mut())
}

/// Writes data to the virtual memory.
///
/// No pointer-alignment requirement (Linux-parity: `copy_to_user` is
/// alignment-agnostic; see [`vm_read_slice`]). The old `is_aligned()` gate made
/// `epoll_pwait` return EFAULT on riscv64/loongarch64: Go's `[]epollevent` is
/// 4-byte-aligned (`data [8]byte`) while `struct epoll_event` is 8-aligned
/// (`u64 data`) on non-x86, so the events buffer failed the check and crashed
/// the Go netpoller (`netpoll failed`).
pub fn vm_write_slice<T>(ptr: *mut T, buf: &[T]) -> VmResult {
    // SAFETY: we don't care about validity, since these bytes are only used for
    // writing to the virtual memory.
    let bytes = unsafe { slice::from_raw_parts(buf.as_ptr().cast::<u8>(), size_of_val(buf)) };
    VmImpl::new().write(ptr.addr(), bytes)
}

mod thin;
pub use thin::{VmMutPtr, VmPtr};

#[cfg(feature = "alloc")]
mod alloc;
#[cfg(all(axtest, feature = "alloc"))]
pub use alloc::vm_alloc_is_zero_and_max_bytes_rules_hold_for_test;
#[cfg(feature = "alloc")]
pub use alloc::{vm_load, vm_load_any, vm_load_until_nul};
