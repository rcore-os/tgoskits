//! Hidden runtime boundary for the OS-independent lock wrappers.

use core::{
    panic::Location,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize},
};

/// Do not alter the current execution context.
pub const CONTEXT_RAW: u8 = 0;
/// Disable preemption while the lock is held.
pub const CONTEXT_PREEMPT: u8 = 1;
/// Save and disable local IRQs while the guard is alive.
pub const CONTEXT_IRQSAVE: u8 = 2;
/// Disable preemption, then save and disable local IRQs.
pub const CONTEXT_PREEMPT_IRQSAVE: u8 = 3;

/// An exclusive spin lock operation.
pub const LOCK_KIND_SPIN: u8 = 0;
/// A spin read-write lock operation.
pub const LOCK_KIND_RW: u8 = 1;
/// A sleepable mutex operation.
pub const LOCK_KIND_MUTEX: u8 = 2;

/// Exclusive ownership.
pub const LOCK_MODE_EXCLUSIVE: u8 = 0;
/// Shared read ownership.
pub const LOCK_MODE_READ: u8 = 1;
/// Exclusive write ownership.
pub const LOCK_MODE_WRITE: u8 = 2;

/// Result of one complete non-sleeping acquisition transaction.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct AcquireResult {
    acquired: u8,
    _reserved: [u8; 7],
    context_state: usize,
}

impl AcquireResult {
    /// Creates a provider result.
    #[doc(hidden)]
    pub const fn new(acquired: bool, context_state: usize) -> Self {
        Self {
            acquired: acquired as u8,
            _reserved: [0; 7],
            context_state,
        }
    }

    pub(crate) const fn acquired(self) -> bool {
        self.acquired != 0
    }

    pub(crate) const fn context_state(self) -> usize {
        self.context_state
    }
}

/// Lock-class storage whose layout is shared with the native provider.
#[repr(C)]
pub struct LockMetadata {
    class_id: AtomicU32,
    class_key: AtomicPtr<Location<'static>>,
}

impl LockMetadata {
    /// Creates metadata for a statically constructed lock class.
    #[track_caller]
    pub const fn new() -> Self {
        Self {
            class_id: AtomicU32::new(0),
            class_key: AtomicPtr::new(
                Location::caller() as *const Location<'static> as *mut Location<'static>
            ),
        }
    }

    /// Returns the class-id storage for the runtime adapter.
    #[doc(hidden)]
    pub const fn class_id(&self) -> &AtomicU32 {
        &self.class_id
    }

    /// Returns the class-key storage for the runtime adapter.
    #[doc(hidden)]
    pub const fn class_key(&self) -> &AtomicPtr<Location<'static>> {
        &self.class_key
    }
}

impl Default for LockMetadata {
    fn default() -> Self {
        Self::new()
    }
}

/// Complete execution-context operations used by standalone guards.
#[ax_crate_interface::def_interface]
pub trait ContextOps {
    /// Enters `context` and returns its opaque restore token.
    fn enter(context: u8) -> usize;

    /// Leaves `context` using the matching token.
    fn exit(context: u8, state: usize);
}

/// Complete spin-lock acquisition and release operations.
#[ax_crate_interface::def_interface]
pub trait SpinOps {
    fn acquire(
        locked: &AtomicBool,
        metadata: &LockMetadata,
        lock_addr: usize,
        context: u8,
        subclass: u32,
        is_try: bool,
        caller: &'static Location<'static>,
    ) -> AcquireResult;

    fn release(locked: &AtomicBool, lock_addr: usize, context: u8, context_state: usize);

    fn force_release(locked: &AtomicBool, lock_addr: usize, context: u8);

    fn is_locked(locked: &AtomicBool) -> bool;
}

/// Complete spin read-write lock operations.
#[ax_crate_interface::def_interface]
pub trait RwLockOps {
    fn acquire(
        state: &AtomicUsize,
        metadata: &LockMetadata,
        lock_addr: usize,
        context: u8,
        mode: u8,
        is_try: bool,
        caller: &'static Location<'static>,
    ) -> AcquireResult;

    fn release(state: &AtomicUsize, lock_addr: usize, context: u8, context_state: usize, mode: u8);

    fn force_read_decrement(state: &AtomicUsize, lock_addr: usize, context: u8);
}

/// Complete sleepable mutex operations.
#[ax_crate_interface::def_interface]
pub trait MutexOps {
    fn acquire(
        wait_queue: &AtomicPtr<()>,
        owner_id: &AtomicU64,
        metadata: &LockMetadata,
        lock_addr: usize,
        subclass: u32,
        is_try: bool,
        caller: &'static Location<'static>,
    ) -> bool;

    fn release(wait_queue: &AtomicPtr<()>, owner_id: &AtomicU64, lock_addr: usize);

    fn force_release(wait_queue: &AtomicPtr<()>, owner_id: &AtomicU64, lock_addr: usize);

    fn is_owned_by_current(owner_id: &AtomicU64) -> bool;

    fn is_locked(owner_id: &AtomicU64) -> bool;

    fn drop_wait_queue(wait_queue: *mut ());
}

/// Runtime lockdep diagnostics which do not belong to one lock acquisition.
#[ax_crate_interface::def_interface]
pub trait LockdepOps {
    fn set_trace_enabled(enabled: bool);
    fn dump_trace();
}

pub(crate) fn context_enter(context: u8) -> usize {
    #[cfg(all(feature = "host-test", not(target_os = "none")))]
    return crate::host::context_enter(context);
    #[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
    return ax_crate_interface::call_interface!(ContextOps::enter, context);
}

pub(crate) fn context_exit(context: u8, state: usize) {
    #[cfg(all(feature = "host-test", not(target_os = "none")))]
    return crate::host::context_exit(context, state);
    #[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
    ax_crate_interface::call_interface!(ContextOps::exit, context, state);
}

pub(crate) fn spin_acquire(
    locked: &AtomicBool,
    metadata: &LockMetadata,
    lock_addr: usize,
    context: u8,
    subclass: u32,
    is_try: bool,
    caller: &'static Location<'static>,
) -> AcquireResult {
    #[cfg(all(feature = "host-test", not(target_os = "none")))]
    return crate::host::spin_acquire(
        locked, metadata, lock_addr, context, subclass, is_try, caller,
    );
    #[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
    return ax_crate_interface::call_interface!(
        SpinOps::acquire,
        locked,
        metadata,
        lock_addr,
        context,
        subclass,
        is_try,
        caller
    );
}

pub(crate) fn spin_release(
    locked: &AtomicBool,
    lock_addr: usize,
    context: u8,
    context_state: usize,
) {
    #[cfg(all(feature = "host-test", not(target_os = "none")))]
    return crate::host::spin_release(locked, lock_addr, context, context_state);
    #[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
    ax_crate_interface::call_interface!(
        SpinOps::release,
        locked,
        lock_addr,
        context,
        context_state
    );
}

pub(crate) fn spin_force_release(locked: &AtomicBool, lock_addr: usize, context: u8) {
    #[cfg(all(feature = "host-test", not(target_os = "none")))]
    return crate::host::spin_force_release(locked, lock_addr, context);
    #[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
    ax_crate_interface::call_interface!(SpinOps::force_release, locked, lock_addr, context);
}

pub(crate) fn spin_is_locked(locked: &AtomicBool) -> bool {
    #[cfg(all(feature = "host-test", not(target_os = "none")))]
    return crate::host::spin_is_locked(locked);
    #[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
    return ax_crate_interface::call_interface!(SpinOps::is_locked, locked);
}

pub(crate) fn rwlock_acquire(
    state: &AtomicUsize,
    metadata: &LockMetadata,
    lock_addr: usize,
    context: u8,
    mode: u8,
    is_try: bool,
    caller: &'static Location<'static>,
) -> AcquireResult {
    #[cfg(all(feature = "host-test", not(target_os = "none")))]
    return crate::host::rwlock_acquire(state, metadata, lock_addr, context, mode, is_try, caller);
    #[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
    return ax_crate_interface::call_interface!(
        RwLockOps::acquire,
        state,
        metadata,
        lock_addr,
        context,
        mode,
        is_try,
        caller
    );
}

pub(crate) fn rwlock_release(
    state: &AtomicUsize,
    lock_addr: usize,
    context: u8,
    context_state: usize,
    mode: u8,
) {
    #[cfg(all(feature = "host-test", not(target_os = "none")))]
    return crate::host::rwlock_release(state, lock_addr, context, context_state, mode);
    #[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
    ax_crate_interface::call_interface!(
        RwLockOps::release,
        state,
        lock_addr,
        context,
        context_state,
        mode
    );
}

pub(crate) fn rwlock_force_read_decrement(state: &AtomicUsize, lock_addr: usize, context: u8) {
    #[cfg(all(feature = "host-test", not(target_os = "none")))]
    return crate::host::rwlock_force_read_decrement(state, lock_addr, context);
    #[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
    ax_crate_interface::call_interface!(RwLockOps::force_read_decrement, state, lock_addr, context);
}

#[cfg(feature = "sleep")]
pub(crate) fn mutex_acquire(
    wait_queue: &AtomicPtr<()>,
    owner_id: &AtomicU64,
    metadata: &LockMetadata,
    lock_addr: usize,
    subclass: u32,
    is_try: bool,
    caller: &'static Location<'static>,
) -> bool {
    #[cfg(all(feature = "host-test", not(target_os = "none")))]
    return crate::host::mutex_acquire(
        wait_queue, owner_id, metadata, lock_addr, subclass, is_try, caller,
    );
    #[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
    return ax_crate_interface::call_interface!(
        MutexOps::acquire,
        wait_queue,
        owner_id,
        metadata,
        lock_addr,
        subclass,
        is_try,
        caller
    );
}

#[cfg(feature = "sleep")]
pub(crate) fn mutex_release(wait_queue: &AtomicPtr<()>, owner_id: &AtomicU64, lock_addr: usize) {
    #[cfg(all(feature = "host-test", not(target_os = "none")))]
    return crate::host::mutex_release(wait_queue, owner_id, lock_addr);
    #[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
    ax_crate_interface::call_interface!(MutexOps::release, wait_queue, owner_id, lock_addr);
}

#[cfg(feature = "sleep")]
pub(crate) fn mutex_force_release(
    wait_queue: &AtomicPtr<()>,
    owner_id: &AtomicU64,
    lock_addr: usize,
) {
    #[cfg(all(feature = "host-test", not(target_os = "none")))]
    return crate::host::mutex_force_release(wait_queue, owner_id, lock_addr);
    #[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
    ax_crate_interface::call_interface!(MutexOps::force_release, wait_queue, owner_id, lock_addr);
}

#[cfg(feature = "sleep")]
pub(crate) fn mutex_is_owned_by_current(owner_id: &AtomicU64) -> bool {
    #[cfg(all(feature = "host-test", not(target_os = "none")))]
    return crate::host::mutex_is_owned_by_current(owner_id);
    #[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
    return ax_crate_interface::call_interface!(MutexOps::is_owned_by_current, owner_id);
}

#[cfg(feature = "sleep")]
pub(crate) fn mutex_is_locked(owner_id: &AtomicU64) -> bool {
    #[cfg(all(feature = "host-test", not(target_os = "none")))]
    return crate::host::mutex_is_locked(owner_id);
    #[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
    return ax_crate_interface::call_interface!(MutexOps::is_locked, owner_id);
}

#[cfg(feature = "sleep")]
pub(crate) fn mutex_drop_wait_queue(wait_queue: *mut ()) {
    #[cfg(all(feature = "host-test", not(target_os = "none")))]
    return crate::host::mutex_drop_wait_queue(wait_queue);
    #[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
    ax_crate_interface::call_interface!(MutexOps::drop_wait_queue, wait_queue);
}

pub(crate) fn set_trace_enabled(enabled: bool) {
    #[cfg(all(feature = "host-test", not(target_os = "none")))]
    return crate::host::set_trace_enabled(enabled);
    #[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
    ax_crate_interface::call_interface!(LockdepOps::set_trace_enabled, enabled);
}

pub(crate) fn dump_trace() {
    #[cfg(all(feature = "host-test", not(target_os = "none")))]
    return crate::host::dump_trace();
    #[cfg(not(all(feature = "host-test", not(target_os = "none"))))]
    ax_crate_interface::call_interface!(LockdepOps::dump_trace);
}

#[cfg(test)]
mod tests {
    use core::mem::{align_of, offset_of, size_of};

    use super::*;

    #[test]
    fn acquire_result_has_fixed_bridge_layout() {
        assert_eq!(offset_of!(AcquireResult, acquired), 0);
        assert_eq!(offset_of!(AcquireResult, context_state), 8);
        assert_eq!(align_of::<AcquireResult>(), align_of::<usize>());
        assert_eq!(size_of::<AcquireResult>(), 8 + size_of::<usize>());

        let failed = AcquireResult::new(false, usize::MAX);
        assert!(!failed.acquired());
        assert_eq!(failed.context_state(), usize::MAX);

        let acquired = AcquireResult::new(true, 0x5a5a);
        assert!(acquired.acquired());
        assert_eq!(acquired.context_state(), 0x5a5a);
    }

    #[test]
    fn lock_metadata_has_fixed_bridge_layout() {
        let pointer_offset = offset_of!(LockMetadata, class_key);
        let pointer_alignment = align_of::<AtomicPtr<Location<'static>>>();
        let expected_pointer_offset = size_of::<AtomicU32>().next_multiple_of(pointer_alignment);
        let expected_alignment = align_of::<AtomicU32>().max(pointer_alignment);
        let expected_size = (pointer_offset + size_of::<AtomicPtr<Location<'static>>>())
            .next_multiple_of(expected_alignment);

        assert_eq!(offset_of!(LockMetadata, class_id), 0);
        assert_eq!(pointer_offset, expected_pointer_offset);
        assert_eq!(align_of::<LockMetadata>(), expected_alignment);
        assert_eq!(size_of::<LockMetadata>(), expected_size);
    }
}
