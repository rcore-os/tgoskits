//! Virtual memory utilities.
#![no_std]
#![feature(maybe_uninit_as_bytes)]
#![warn(missing_docs)]

use core::mem::MaybeUninit;

use ax_errno::AxError;
use bytemuck::NoUninit;

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
/// - Returning `Ok(())` from [`VmIo::read`] guarantees that every byte in `buf`
///   was initialized.
/// - [`VmIo::write`] may read every byte in `buf`, but must not retain the
///   borrowed slice after returning.
/// - The provider must keep its address-space and access authority live for the
///   complete operation and serialize mutable provider state as needed.
/// - Zero-length access validation is provider-defined. Callers must not use an
///   empty transfer as proof that an address is mapped.
pub unsafe trait VmIo {
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
pub fn vm_read_slice<I: VmIo, T>(
    vm: &mut I,
    ptr: *const T,
    buf: &mut [MaybeUninit<T>],
) -> VmResult {
    vm.read(ptr.addr(), buf.as_bytes_mut())
}

/// Writes data to the virtual memory.
///
/// No pointer-alignment requirement (Linux-parity: `copy_to_user` is
/// alignment-agnostic; see [`vm_read_slice`]). The old `is_aligned()` gate made
/// `epoll_pwait` return EFAULT on riscv64/loongarch64: Go's `[]epollevent` is
/// 4-byte-aligned (`data [8]byte`) while `struct epoll_event` is 8-aligned
/// (`u64 data`) on non-x86, so the events buffer failed the check and crashed
/// the Go netpoller (`netpoll failed`).
pub fn vm_write_slice<I: VmIo, T: NoUninit>(vm: &mut I, ptr: *mut T, buf: &[T]) -> VmResult {
    vm.write(ptr.addr(), bytemuck::cast_slice(buf))
}

mod thin;
pub use thin::{VmMutPtr, VmPtr};

#[cfg(feature = "alloc")]
mod alloc;
#[cfg(all(axtest, feature = "alloc"))]
pub use alloc::vm_alloc_is_zero_and_max_bytes_rules_hold_for_test;
#[cfg(feature = "alloc")]
pub use alloc::{vm_load, vm_load_any, vm_load_until_nul};
