//! Nofault access to user-space words.

/// Direction of an architecture-level user-memory access check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UserAccessType {
    /// The kernel intends to read bytes supplied by user space.
    Read,
    /// The kernel intends to write bytes into user space.
    Write,
}

/// Failure returned by a nofault user read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UserAccessError {
    /// The user word was not accessible without resolving a page fault.
    Fault,
}

/// Atomic operation supported by [`user_atomic_u32`].
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UserAtomicU32Op {
    /// Replaces the word with the supplied argument.
    Set    = 0,
    /// Adds the supplied argument with wrapping semantics.
    Add    = 1,
    /// ORs the word with the supplied argument.
    Or     = 2,
    /// ANDs the word with the inverse of the supplied argument.
    AndNot = 3,
    /// XORs the word with the supplied argument.
    Xor    = 4,
}

/// Failure returned by a nofault user atomic operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UserAtomicError {
    /// The user word was not accessible without resolving a page fault.
    Fault,
    /// A bounded load-linked/store-conditional sequence made no progress.
    Retry,
}

unsafe extern "C" {
    fn __axcpu_user_read_u32(address: *const u32, value: *mut u32) -> u32;

    fn __axcpu_user_atomic_u32(
        address: *mut u32,
        operation: u32,
        argument: u32,
        old_value: *mut u32,
    ) -> u32;
}

/// Reads one aligned user-space word without resolving faults.
///
/// # Safety
///
/// `address` must be aligned for `u32` and point into the calling task's user
/// address range. The caller must resolve [`UserAccessError::Fault`] only after
/// releasing every lock whose critical section must remain nofault.
pub unsafe fn user_read_u32(address: *const u32) -> Result<u32, UserAccessError> {
    let mut value = 0;
    let status = unsafe { __axcpu_user_read_u32(address, &mut value) };
    match status {
        0 => Ok(value),
        1 => Err(UserAccessError::Fault),
        _ => unreachable!("architecture returned an invalid user read status"),
    }
}

/// Atomically updates one aligned user-space word without resolving faults.
///
/// The previous value is returned on success. A fault is redirected through
/// the dedicated nofault exception table before the OS page-fault handler is
/// invoked, so this function never sleeps or allocates.
///
/// # Safety
///
/// `address` must be aligned for `u32` and point into the calling task's user
/// address range. The caller must serialize this operation with the protocol
/// that consumes its result and must resolve [`UserAtomicError::Fault`] only
/// after releasing every lock whose critical section must remain nofault.
pub unsafe fn user_atomic_u32(
    address: *mut u32,
    operation: UserAtomicU32Op,
    argument: u32,
) -> Result<u32, UserAtomicError> {
    let mut old_value = 0;
    let status =
        unsafe { __axcpu_user_atomic_u32(address, operation as u32, argument, &mut old_value) };
    match status {
        0 => Ok(old_value),
        1 => Err(UserAtomicError::Fault),
        2 => Err(UserAtomicError::Retry),
        _ => unreachable!("architecture returned an invalid user atomic status"),
    }
}
