//! Hidden runtime boundary for the OS-independent lock wrappers.

use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    panic::Location,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU32, AtomicU64, AtomicUsize},
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

/// Number of pointer-sized words reserved for the scheduler-owned PI waiter tree.
pub const PI_MUTEX_WAIT_STORAGE_WORDS: usize = 5;

/// Fixed external storage for one native PI-mutex core.
///
/// The provider interprets this storage as the native `ax-task` PI core. The
/// wrapper never reads or mutates the state machine itself.
#[repr(C)]
pub struct PiMutexStorage {
    owner_word: AtomicU64,
    generation: AtomicU64,
    wait_state: AtomicU8,
    wait_storage: UnsafeCell<[MaybeUninit<usize>; PI_MUTEX_WAIT_STORAGE_WORDS]>,
}

/// Exclusive borrow of every field in one external PI-mutex storage object.
#[doc(hidden)]
pub struct PiMutexStoragePartsMut<'lock> {
    pub owner_word: &'lock mut AtomicU64,
    pub generation: &'lock mut AtomicU64,
    pub wait_state: &'lock mut AtomicU8,
    pub wait_storage: &'lock mut UnsafeCell<[MaybeUninit<usize>; PI_MUTEX_WAIT_STORAGE_WORDS]>,
}

impl PiMutexStorage {
    /// Creates storage for an unlocked, generation-free PI mutex.
    pub const fn new() -> Self {
        Self {
            owner_word: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            wait_state: AtomicU8::new(0),
            wait_storage: UnsafeCell::new([MaybeUninit::uninit(); PI_MUTEX_WAIT_STORAGE_WORDS]),
        }
    }

    /// Returns the native owner-word storage for provider layout validation.
    #[doc(hidden)]
    pub const fn owner_word(&self) -> &AtomicU64 {
        &self.owner_word
    }

    /// Returns the native lock-generation storage for provider layout validation.
    #[doc(hidden)]
    pub const fn generation(&self) -> &AtomicU64 {
        &self.generation
    }

    /// Returns the inline waiter lifecycle storage for provider layout validation.
    #[doc(hidden)]
    pub const fn wait_state(&self) -> &AtomicU8 {
        &self.wait_state
    }

    /// Returns the native inline waiter storage borrowed by the provider.
    #[doc(hidden)]
    pub const fn wait_storage(
        &self,
    ) -> &UnsafeCell<[MaybeUninit<usize>; PI_MUTEX_WAIT_STORAGE_WORDS]> {
        &self.wait_storage
    }

    /// Exclusively borrows every field for the native destruction transaction.
    #[doc(hidden)]
    pub fn parts_mut(&mut self) -> PiMutexStoragePartsMut<'_> {
        PiMutexStoragePartsMut {
            owner_word: &mut self.owner_word,
            generation: &mut self.generation,
            wait_state: &mut self.wait_state,
            wait_storage: &mut self.wait_storage,
        }
    }
}

impl Default for PiMutexStorage {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: the provider publishes initialization through `wait_state` and
// serializes every access to the concrete object stored in `wait_storage`.
unsafe impl Sync for PiMutexStorage {}

/// Opaque execution-context restore state returned by the provider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct ContextState {
    preempt: usize,
    irq: usize,
}

impl ContextState {
    /// Creates a provider context result.
    #[doc(hidden)]
    pub const fn new(preempt: usize, irq: usize) -> Self {
        Self { preempt, irq }
    }

    /// Returns the provider's preemption restore token.
    #[doc(hidden)]
    pub const fn preempt(self) -> usize {
        self.preempt
    }

    /// Returns the provider's raw local-IRQ restore state.
    #[doc(hidden)]
    pub const fn irq(self) -> usize {
        self.irq
    }
}

/// Result of one complete non-sleeping acquisition transaction.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct AcquireResult {
    acquired: u8,
    _reserved: [u8; 7],
    context_state: ContextState,
}

impl AcquireResult {
    /// Creates a provider result.
    #[doc(hidden)]
    pub const fn new(acquired: bool, context_state: ContextState) -> Self {
        Self {
            acquired: acquired as u8,
            _reserved: [0; 7],
            context_state,
        }
    }

    pub(crate) const fn acquired(self) -> bool {
        self.acquired != 0
    }

    pub(crate) const fn context_state(self) -> ContextState {
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
    fn enter(context: u8) -> ContextState;

    /// Leaves `context` using the matching token.
    fn exit(context: u8, state: ContextState);

    /// Enters the preemption scope consumed by a hard-IRQ return epilogue.
    fn irq_return_preempt_enter() -> usize;

    /// Leaves one IRQ-return preemption scope while raw local IRQs stay disabled.
    fn irq_return_preempt_exit(state: usize);

    /// Publishes entry into the runtime hard-interrupt lifecycle.
    fn hardirq_enter();

    /// Publishes exit from the runtime hard-interrupt lifecycle.
    fn hardirq_exit();
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
        caller: &'static Location<'static>,
    ) -> ContextState;

    fn try_acquire(
        locked: &AtomicBool,
        metadata: &LockMetadata,
        lock_addr: usize,
        context: u8,
        subclass: u32,
        caller: &'static Location<'static>,
    ) -> AcquireResult;

    fn release(locked: &AtomicBool, lock_addr: usize, context: u8, context_state: ContextState);

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
        caller: &'static Location<'static>,
    ) -> ContextState;

    fn try_acquire(
        state: &AtomicUsize,
        metadata: &LockMetadata,
        lock_addr: usize,
        context: u8,
        mode: u8,
        caller: &'static Location<'static>,
    ) -> AcquireResult;

    fn release(
        state: &AtomicUsize,
        lock_addr: usize,
        context: u8,
        context_state: ContextState,
        mode: u8,
    );

    fn force_read_decrement(state: &AtomicUsize, lock_addr: usize, context: u8);
}

/// Complete sleepable mutex operations.
#[ax_crate_interface::def_interface]
pub trait MutexOps {
    fn acquire(
        storage: &PiMutexStorage,
        next_waiter_sequence: &AtomicU64,
        metadata: &LockMetadata,
        lock_addr: usize,
        subclass: u32,
        caller: &'static Location<'static>,
    );

    fn try_acquire(
        storage: &PiMutexStorage,
        next_waiter_sequence: &AtomicU64,
        metadata: &LockMetadata,
        lock_addr: usize,
        subclass: u32,
        caller: &'static Location<'static>,
    ) -> bool;

    fn release(storage: &PiMutexStorage, lock_addr: usize);

    fn force_release(storage: &PiMutexStorage, lock_addr: usize);

    fn is_owned_by_current(storage: &PiMutexStorage) -> bool;

    fn is_locked(storage: &PiMutexStorage) -> bool;

    fn destroy(storage: &mut PiMutexStorage);
}

/// Runtime lockdep diagnostics which do not belong to one lock acquisition.
#[ax_crate_interface::def_interface]
pub trait LockdepOps {
    fn set_trace_enabled(enabled: bool);
    fn dump_trace();
}

pub(crate) fn context_enter(context: u8) -> ContextState {
    ax_crate_interface::call_interface!(ContextOps::enter, context)
}

pub(crate) fn context_exit(context: u8, state: ContextState) {
    ax_crate_interface::call_interface!(ContextOps::exit, context, state);
}

pub(crate) fn irq_return_preempt_enter() -> usize {
    ax_crate_interface::call_interface!(ContextOps::irq_return_preempt_enter)
}

pub(crate) fn irq_return_preempt_exit(state: usize) {
    ax_crate_interface::call_interface!(ContextOps::irq_return_preempt_exit, state);
}

pub(crate) fn hardirq_enter() {
    ax_crate_interface::call_interface!(ContextOps::hardirq_enter);
}

pub(crate) fn hardirq_exit() {
    ax_crate_interface::call_interface!(ContextOps::hardirq_exit);
}

pub(crate) fn spin_acquire(
    locked: &AtomicBool,
    metadata: &LockMetadata,
    lock_addr: usize,
    context: u8,
    subclass: u32,
    caller: &'static Location<'static>,
) -> ContextState {
    ax_crate_interface::call_interface!(
        SpinOps::acquire,
        locked,
        metadata,
        lock_addr,
        context,
        subclass,
        caller
    )
}

pub(crate) fn spin_try_acquire(
    locked: &AtomicBool,
    metadata: &LockMetadata,
    lock_addr: usize,
    context: u8,
    subclass: u32,
    caller: &'static Location<'static>,
) -> AcquireResult {
    ax_crate_interface::call_interface!(
        SpinOps::try_acquire,
        locked,
        metadata,
        lock_addr,
        context,
        subclass,
        caller
    )
}

pub(crate) fn spin_release(
    locked: &AtomicBool,
    lock_addr: usize,
    context: u8,
    context_state: ContextState,
) {
    ax_crate_interface::call_interface!(
        SpinOps::release,
        locked,
        lock_addr,
        context,
        context_state
    );
}

pub(crate) fn spin_force_release(locked: &AtomicBool, lock_addr: usize, context: u8) {
    ax_crate_interface::call_interface!(SpinOps::force_release, locked, lock_addr, context);
}

pub(crate) fn spin_is_locked(locked: &AtomicBool) -> bool {
    ax_crate_interface::call_interface!(SpinOps::is_locked, locked)
}

pub(crate) fn rwlock_acquire(
    state: &AtomicUsize,
    metadata: &LockMetadata,
    lock_addr: usize,
    context: u8,
    mode: u8,
    caller: &'static Location<'static>,
) -> ContextState {
    ax_crate_interface::call_interface!(
        RwLockOps::acquire,
        state,
        metadata,
        lock_addr,
        context,
        mode,
        caller
    )
}

pub(crate) fn rwlock_try_acquire(
    state: &AtomicUsize,
    metadata: &LockMetadata,
    lock_addr: usize,
    context: u8,
    mode: u8,
    caller: &'static Location<'static>,
) -> AcquireResult {
    ax_crate_interface::call_interface!(
        RwLockOps::try_acquire,
        state,
        metadata,
        lock_addr,
        context,
        mode,
        caller
    )
}

pub(crate) fn rwlock_release(
    state: &AtomicUsize,
    lock_addr: usize,
    context: u8,
    context_state: ContextState,
    mode: u8,
) {
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
    ax_crate_interface::call_interface!(RwLockOps::force_read_decrement, state, lock_addr, context);
}

#[cfg(feature = "sleep")]
pub(crate) fn mutex_acquire(
    storage: &PiMutexStorage,
    next_waiter_sequence: &AtomicU64,
    metadata: &LockMetadata,
    lock_addr: usize,
    subclass: u32,
    caller: &'static Location<'static>,
) {
    ax_crate_interface::call_interface!(
        MutexOps::acquire,
        storage,
        next_waiter_sequence,
        metadata,
        lock_addr,
        subclass,
        caller
    );
}

#[cfg(feature = "sleep")]
pub(crate) fn mutex_try_acquire(
    storage: &PiMutexStorage,
    next_waiter_sequence: &AtomicU64,
    metadata: &LockMetadata,
    lock_addr: usize,
    subclass: u32,
    caller: &'static Location<'static>,
) -> bool {
    ax_crate_interface::call_interface!(
        MutexOps::try_acquire,
        storage,
        next_waiter_sequence,
        metadata,
        lock_addr,
        subclass,
        caller
    )
}

#[cfg(feature = "sleep")]
pub(crate) fn mutex_release(storage: &PiMutexStorage, lock_addr: usize) {
    ax_crate_interface::call_interface!(MutexOps::release, storage, lock_addr);
}

#[cfg(feature = "sleep")]
pub(crate) fn mutex_force_release(storage: &PiMutexStorage, lock_addr: usize) {
    ax_crate_interface::call_interface!(MutexOps::force_release, storage, lock_addr);
}

#[cfg(feature = "sleep")]
pub(crate) fn mutex_is_owned_by_current(storage: &PiMutexStorage) -> bool {
    ax_crate_interface::call_interface!(MutexOps::is_owned_by_current, storage)
}

#[cfg(feature = "sleep")]
pub(crate) fn mutex_is_locked(storage: &PiMutexStorage) -> bool {
    ax_crate_interface::call_interface!(MutexOps::is_locked, storage)
}

#[cfg(feature = "sleep")]
pub(crate) fn mutex_destroy(storage: &mut PiMutexStorage) {
    ax_crate_interface::call_interface!(MutexOps::destroy, storage);
}

pub(crate) fn set_trace_enabled(enabled: bool) {
    ax_crate_interface::call_interface!(LockdepOps::set_trace_enabled, enabled);
}

pub(crate) fn dump_trace() {
    ax_crate_interface::call_interface!(LockdepOps::dump_trace);
}

#[cfg(test)]
mod tests {
    use core::{
        mem::{align_of, offset_of, size_of},
        sync::atomic::Ordering,
    };

    use super::*;

    #[test]
    fn acquire_result_has_fixed_bridge_layout() {
        assert_eq!(offset_of!(ContextState, preempt), 0);
        assert_eq!(offset_of!(ContextState, irq), size_of::<usize>());
        assert_eq!(align_of::<ContextState>(), align_of::<usize>());
        assert_eq!(size_of::<ContextState>(), 2 * size_of::<usize>());

        assert_eq!(offset_of!(AcquireResult, acquired), 0);
        assert_eq!(offset_of!(AcquireResult, context_state), 8);
        assert_eq!(align_of::<AcquireResult>(), align_of::<usize>());
        assert_eq!(size_of::<AcquireResult>(), 8 + size_of::<ContextState>());

        let failed = AcquireResult::new(false, ContextState::new(usize::MAX, 0));
        assert!(!failed.acquired());
        assert_eq!(failed.context_state(), ContextState::new(usize::MAX, 0));

        let acquired = AcquireResult::new(true, ContextState::new(0x5a5a, 0xa5a5));
        assert!(acquired.acquired());
        assert_eq!(acquired.context_state(), ContextState::new(0x5a5a, 0xa5a5));
    }

    #[test]
    fn blocking_spin_bridges_return_only_context_state() {
        if false {
            let spin = AtomicBool::new(false);
            let rwlock = AtomicUsize::new(0);
            let metadata = LockMetadata::new();
            let caller = Location::caller();

            let _: ContextState = spin_acquire(&spin, &metadata, 0, 0, 0, caller);
            let _: AcquireResult = spin_try_acquire(&spin, &metadata, 0, 0, 0, caller);
            let _: ContextState = rwlock_acquire(&rwlock, &metadata, 0, 0, 0, caller);
            let _: AcquireResult = rwlock_try_acquire(&rwlock, &metadata, 0, 0, 0, caller);
        }
    }

    #[cfg(feature = "sleep")]
    #[test]
    fn blocking_pi_mutex_bridge_has_no_fallible_result() {
        if false {
            let storage = PiMutexStorage::new();
            let sequence = AtomicU64::new(0);
            let metadata = LockMetadata::new();
            let caller = Location::caller();

            let _: () = mutex_acquire(&storage, &sequence, &metadata, 0, 0, caller);
            let _: bool = mutex_try_acquire(&storage, &sequence, &metadata, 0, 0, caller);
        }
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

    #[test]
    fn pi_mutex_storage_has_fixed_bridge_layout_and_mutable_parts() {
        let generation_offset = size_of::<AtomicU64>();
        let wait_state_offset = 2 * size_of::<AtomicU64>();
        let wait_storage_alignment = align_of::<usize>();
        let wait_storage_offset =
            (wait_state_offset + size_of::<AtomicU8>()).next_multiple_of(wait_storage_alignment);
        let expected_alignment = align_of::<AtomicU64>().max(wait_storage_alignment);
        let expected_size = (wait_storage_offset
            + PI_MUTEX_WAIT_STORAGE_WORDS * size_of::<usize>())
        .next_multiple_of(expected_alignment);

        assert_eq!(offset_of!(PiMutexStorage, owner_word), 0);
        assert_eq!(offset_of!(PiMutexStorage, generation), generation_offset);
        assert_eq!(offset_of!(PiMutexStorage, wait_state), wait_state_offset);
        assert_eq!(
            offset_of!(PiMutexStorage, wait_storage),
            wait_storage_offset
        );
        assert_eq!(align_of::<PiMutexStorage>(), expected_alignment);
        assert_eq!(size_of::<PiMutexStorage>(), expected_size);

        let mut storage = PiMutexStorage::new();
        let parts = storage.parts_mut();
        *parts.owner_word.get_mut() = 7;
        *parts.generation.get_mut() = 11;
        *parts.wait_state.get_mut() = 2;
        assert_eq!(storage.owner_word().load(Ordering::Relaxed), 7);
        assert_eq!(storage.generation().load(Ordering::Relaxed), 11);
        assert_eq!(storage.wait_state().load(Ordering::Relaxed), 2);
    }
}
