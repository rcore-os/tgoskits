//! Lock-local state for scheduler-owned PI mutexes.

use core::{
    cell::UnsafeCell,
    fmt,
    mem::MaybeUninit,
    ptr::NonNull,
    sync::atomic::{AtomicU8, AtomicU64, Ordering},
};

static NEXT_PI_MUTEX_GENERATION: AtomicU64 = AtomicU64::new(1);
const OWNER_HAS_WAITERS: u64 = 1 << 63;
const OWNER_ID_MASK: u64 = !OWNER_HAS_WAITERS;
const WAIT_STORAGE_UNINITIALIZED: u8 = 0;
const WAIT_STORAGE_INITIALIZING: u8 = 1;
const WAIT_STORAGE_READY: u8 = 2;

/// Number of pointer-sized words reserved for provider-owned waiter metadata.
///
/// The ArceOS provider stores one ticket lock and one three-word ordered-tree
/// root here. Keeping the storage in the physical mutex matches Linux
/// `rt_mutex_base`: contention never allocates lock-local state.
#[doc(hidden)]
pub const PI_MUTEX_WAIT_STORAGE_WORDS: usize = 5;

/// Inline storage for the scheduler-owned waiter tree.
pub struct PiMutexWaitStorage {
    state: AtomicU8,
    words: UnsafeCell<[MaybeUninit<usize>; PI_MUTEX_WAIT_STORAGE_WORDS]>,
}

impl PiMutexWaitStorage {
    const fn new() -> Self {
        Self {
            state: AtomicU8::new(WAIT_STORAGE_UNINITIALIZED),
            words: UnsafeCell::new([MaybeUninit::uninit(); PI_MUTEX_WAIT_STORAGE_WORDS]),
        }
    }

    /// Returns a borrowed view over this scheduler-owned inline storage.
    #[doc(hidden)]
    pub const fn view(&self) -> PiMutexWaitStorageView<'_> {
        PiMutexWaitStorageView::from_parts(&self.state, &self.words)
    }

    fn take_initialized(&mut self) -> Option<*mut ()> {
        take_initialized_wait_storage(self.state.get_mut(), self.words.get_mut())
    }
}

/// Borrowed scheduler-owned waiter storage for one physical PI mutex.
///
/// Both native locks and fixed-layout external wrappers use this view, so the
/// initialization and destruction state machine has exactly one owner.
#[derive(Clone, Copy, Debug)]
pub struct PiMutexWaitStorageView<'lock> {
    state: &'lock AtomicU8,
    words: &'lock UnsafeCell<[MaybeUninit<usize>; PI_MUTEX_WAIT_STORAGE_WORDS]>,
}

impl<'lock> PiMutexWaitStorageView<'lock> {
    /// Creates a view over storage whose lifetime is owned by the physical lock.
    #[doc(hidden)]
    const fn from_parts(
        state: &'lock AtomicU8,
        words: &'lock UnsafeCell<[MaybeUninit<usize>; PI_MUTEX_WAIT_STORAGE_WORDS]>,
    ) -> Self {
        Self { state, words }
    }

    /// Returns the stable address of the provider-owned inline object.
    #[doc(hidden)]
    pub const fn as_ptr(self) -> *mut () {
        self.words.get().cast()
    }

    /// Returns whether a provider object has been published.
    #[doc(hidden)]
    pub fn is_initialized(self) -> bool {
        self.state.load(Ordering::Acquire) == WAIT_STORAGE_READY
    }

    /// Installs the provider waiter object exactly once without allocation.
    ///
    /// # Safety
    ///
    /// `T` must fit the published storage size and alignment. Every caller for
    /// this storage must use the same `T`, and the provider must destroy it
    /// through the task scheduler's waiter-handle destructor.
    #[doc(hidden)]
    pub unsafe fn get_or_init<T>(self, init: impl FnOnce() -> T) -> &'lock T {
        assert!(
            core::mem::size_of::<T>()
                <= PI_MUTEX_WAIT_STORAGE_WORDS * core::mem::size_of::<usize>(),
            "PI mutex provider waiter state exceeds inline storage"
        );
        assert!(
            core::mem::align_of::<T>() <= core::mem::align_of::<usize>(),
            "PI mutex provider waiter state exceeds inline alignment"
        );

        if self
            .state
            .compare_exchange(
                WAIT_STORAGE_UNINITIALIZED,
                WAIT_STORAGE_INITIALIZING,
                Ordering::Acquire,
                Ordering::Acquire,
            )
            .is_ok()
        {
            // SAFETY: this caller won exclusive initialization and validated
            // the concrete object's size and alignment above.
            unsafe { self.as_ptr().cast::<T>().write(init()) };
            self.state.store(WAIT_STORAGE_READY, Ordering::Release);
        } else {
            while self.state.load(Ordering::Acquire) == WAIT_STORAGE_INITIALIZING {
                core::hint::spin_loop();
            }
            assert_eq!(
                self.state.load(Ordering::Acquire),
                WAIT_STORAGE_READY,
                "PI mutex waiter storage has an invalid lifecycle"
            );
        }

        // SAFETY: READY publishes the unique initialized `T`; the containing
        // mutex retains it until its final safe reference becomes unreachable.
        unsafe { &*self.as_ptr().cast::<T>() }
    }

    /// Returns an already initialized provider object.
    ///
    /// # Safety
    ///
    /// `T` must be the same concrete type used by the successful initializer.
    #[doc(hidden)]
    pub unsafe fn get<T>(self) -> Option<&'lock T> {
        if self.state.load(Ordering::Acquire) != WAIT_STORAGE_READY {
            return None;
        }
        // SAFETY: READY publishes the initialized provider object.
        Some(unsafe { &*self.as_ptr().cast::<T>() })
    }
}

fn take_initialized_wait_storage(
    state: &mut u8,
    words: &mut [MaybeUninit<usize>; PI_MUTEX_WAIT_STORAGE_WORDS],
) -> Option<*mut ()> {
    match *state {
        WAIT_STORAGE_UNINITIALIZED => None,
        WAIT_STORAGE_READY => {
            *state = WAIT_STORAGE_UNINITIALIZED;
            Some(words.as_mut_ptr().cast())
        }
        _ => panic!("destroying PI mutex while waiter storage initializes"),
    }
}

// SAFETY: initialization is published by `state`; concrete access remains
// serialized by the provider object's own wait lock.
unsafe impl Sync for PiMutexWaitStorage {}

/// Non-zero scheduler identity stored in a PI mutex owner word.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PiTaskId(u64);

impl PiTaskId {
    /// Creates an owner identity when `raw` fits the PI owner word.
    pub const fn new(raw: u64) -> Option<Self> {
        if raw == 0 || raw & OWNER_HAS_WAITERS != 0 {
            None
        } else {
            Some(Self(raw))
        }
    }

    /// Returns the scheduler-provided raw identity.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Stable identity of one physical PI mutex lifetime.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PiMutexId(u64);

impl PiMutexId {
    /// Returns the globally unique generation allocated to this lock instance.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Failure of a lock-local PI owner-word transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PiMutexStateError {
    /// The current task attempted to acquire a mutex it already owns.
    WaiterOwnsLock,
    /// The owner word or lock generation violates the PI state machine.
    InvalidState,
}

impl fmt::Display for PiMutexStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::WaiterOwnsLock => "PI mutex waiter already owns the lock",
            Self::InvalidState => "invalid PI mutex state",
        })
    }
}

impl core::error::Error for PiMutexStateError {}

/// Lock-local owner word, identity, and scheduler-owned waiter handle.
///
/// The physical lock owns the opaque waiter handle. The task provider owns the
/// object behind that handle and keeps its per-lock waiter tree alive until
/// lock destruction transfers the unique inline object back to the scheduler.
pub struct PiMutexCore {
    owner: AtomicU64,
    generation: AtomicU64,
    wait_storage: PiMutexWaitStorage,
}

/// Borrowed storage of one native PI-mutex state machine.
///
/// The view keeps the algorithm independent from the physical wrapper. Native
/// [`PiMutexCore`] values and OS-independent fixed-layout storage therefore
/// execute the same owner, generation, and waiter-lifecycle transitions.
#[derive(Clone, Copy, Debug)]
pub struct PiMutexCoreView<'lock> {
    owner: &'lock AtomicU64,
    generation: &'lock AtomicU64,
    wait_storage: PiMutexWaitStorageView<'lock>,
}

impl<'lock> PiMutexCoreView<'lock> {
    /// Creates a PI core view over one physical lock's complete storage.
    #[doc(hidden)]
    pub(in crate::sync) const fn from_parts(
        owner: &'lock AtomicU64,
        generation: &'lock AtomicU64,
        wait_state: &'lock AtomicU8,
        wait_words: &'lock UnsafeCell<[MaybeUninit<usize>; PI_MUTEX_WAIT_STORAGE_WORDS]>,
    ) -> Self {
        Self {
            owner,
            generation,
            wait_storage: PiMutexWaitStorageView::from_parts(wait_state, wait_words),
        }
    }

    /// Attempts the atomic uncontended acquisition path.
    pub fn try_acquire(self, current: PiTaskId) -> Result<PiMutexAcquire, PiMutexStateError> {
        match self
            .owner
            .compare_exchange(0, current.get(), Ordering::Acquire, Ordering::Relaxed)
        {
            Ok(_) => Ok(PiMutexAcquire::Acquired),
            Err(owner) if owner & OWNER_ID_MASK == current.get() => {
                Err(PiMutexStateError::WaiterOwnsLock)
            }
            Err(_) => Ok(PiMutexAcquire::Contended),
        }
    }

    /// Attempts acquisition for an explicitly scheduler-authorized identity.
    ///
    /// # Safety
    ///
    /// The caller must own the scheduler authority to establish `current` as
    /// this physical mutex's executing owner.
    #[doc(hidden)]
    pub unsafe fn try_acquire_for_thread<T>(
        self,
        current: T,
    ) -> Result<PiMutexAcquire, PiMutexStateError>
    where
        T: Into<PiTaskId>,
    {
        self.try_acquire(current.into())
    }

    /// Attempts release for an explicitly scheduler-authorized identity.
    ///
    /// # Safety
    ///
    /// The caller must own scheduler authority for `current` and serialize the
    /// transition with the physical mutex owner.
    #[doc(hidden)]
    pub unsafe fn try_release_for_thread<T>(self, current: T) -> Result<bool, PiMutexStateError>
    where
        T: Into<PiTaskId>,
    {
        let current = current.into();
        match self
            .owner
            .compare_exchange(current.get(), 0, Ordering::Release, Ordering::Relaxed)
        {
            Ok(_) => Ok(true),
            Err(owner) if owner_from_word(owner) == Some(current) => Ok(false),
            Err(_) => Err(PiMutexStateError::InvalidState),
        }
    }

    /// Releases the physical owner named by this mutex's owner word.
    ///
    /// # Safety
    ///
    /// The caller must own this mutex through a higher-level raw-mutex
    /// contract and retain that authority through any contended handoff.
    pub unsafe fn try_release_owned(self) -> Result<PiMutexOwnedRelease, PiMutexStateError> {
        let observed = self.owner.load(Ordering::Acquire);
        let owner = owner_from_word(observed).ok_or(PiMutexStateError::InvalidState)?;
        match self
            .owner
            .compare_exchange(owner.get(), 0, Ordering::Release, Ordering::Relaxed)
        {
            Ok(_) => Ok(PiMutexOwnedRelease::Released),
            Err(current) if owner_from_word(current) == Some(owner) => {
                Ok(PiMutexOwnedRelease::Contended(owner))
            }
            Err(_) => Err(PiMutexStateError::InvalidState),
        }
    }

    /// Returns whether `current` is the physical owner.
    pub fn is_owned_by(self, current: PiTaskId) -> bool {
        owner_from_word(self.owner.load(Ordering::Acquire)) == Some(current)
    }

    /// Returns whether the mutex is owned or in an ownerless handoff window.
    pub fn is_locked(self) -> bool {
        self.owner.load(Ordering::Relaxed) != 0
    }

    /// Borrows this physical lock's generation-bearing scheduler identity.
    pub fn mutex_ref(self) -> Result<PiMutexRef<'lock>, PiMutexStateError> {
        let observed = self.generation.load(Ordering::Acquire);
        if observed != 0 {
            return Ok(PiMutexRef {
                core: self,
                id: PiMutexId(observed),
            });
        }

        let allocated = NEXT_PI_MUTEX_GENERATION
            .try_update(Ordering::AcqRel, Ordering::Acquire, |next| {
                next.checked_add(1)
            })
            .map(PiMutexId)
            .map_err(|_| PiMutexStateError::InvalidState)?;
        match self
            .generation
            .compare_exchange(0, allocated.0, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => Ok(PiMutexRef {
                core: self,
                id: allocated,
            }),
            Err(installed) if installed != 0 => Ok(PiMutexRef {
                core: self,
                id: PiMutexId(installed),
            }),
            Err(_) => Err(PiMutexStateError::InvalidState),
        }
    }

    /// Returns the lock-local owner snapshot protected by a provider wait lock.
    #[doc(hidden)]
    pub fn owner_snapshot(self) -> PiMutexOwnerSnapshot {
        let word = self.owner.load(Ordering::Acquire);
        PiMutexOwnerSnapshot {
            word,
            owner: owner_from_word(word),
        }
    }

    /// Attempts to acquire an unlocked snapshot while the wait lock is held.
    #[doc(hidden)]
    pub fn try_acquire_snapshot(self, snapshot: PiMutexOwnerSnapshot, current: PiTaskId) -> bool {
        debug_assert_eq!(snapshot.word, 0);
        self.owner
            .compare_exchange(
                snapshot.word,
                current.get(),
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .is_ok()
    }

    /// Publishes the waiter bit while the provider wait lock is held.
    #[doc(hidden)]
    pub fn try_mark_waiters(self, snapshot: PiMutexOwnerSnapshot) -> bool {
        if snapshot.has_waiters() {
            return self.owner.load(Ordering::Acquire) == snapshot.word;
        }
        self.owner
            .compare_exchange(
                snapshot.word,
                snapshot.word | OWNER_HAS_WAITERS,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Publishes an owned state after a serialized handoff claim.
    #[doc(hidden)]
    pub fn publish_owner(self, owner: PiTaskId, has_waiters: bool) {
        self.owner.store(
            owner.get() | if has_waiters { OWNER_HAS_WAITERS } else { 0 },
            Ordering::Release,
        );
    }

    /// Publishes the reserved ownerless handoff state.
    #[doc(hidden)]
    pub fn publish_ownerless(self) {
        self.owner.store(OWNER_HAS_WAITERS, Ordering::Release);
    }

    /// Ends an ownerless handoff after its final waiter is removed.
    #[doc(hidden)]
    pub fn publish_unlocked(self) {
        self.owner.store(0, Ordering::Release);
    }

    /// Clears the waiter bit while retaining an existing owner.
    #[doc(hidden)]
    pub fn clear_waiters_bit(self, owner: PiTaskId) {
        self.owner.store(owner.get(), Ordering::Release);
    }

    /// Returns the inline scheduler-owned waiter storage.
    #[doc(hidden)]
    pub const fn wait_storage(self) -> PiMutexWaitStorageView<'lock> {
        self.wait_storage
    }
}

impl PiMutexCore {
    /// Creates an unlocked PI mutex core without allocating waiter state.
    pub const fn new() -> Self {
        Self {
            owner: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            wait_storage: PiMutexWaitStorage::new(),
        }
    }

    /// Attempts the atomic uncontended acquisition path.
    pub fn try_acquire(&self, current: PiTaskId) -> Result<PiMutexAcquire, PiMutexStateError> {
        self.view().try_acquire(current)
    }

    /// Attempts acquisition for an explicitly scheduler-authorized identity.
    ///
    /// # Safety
    ///
    /// The caller must own the scheduler authority to establish `current` as
    /// this physical mutex's executing owner.
    #[doc(hidden)]
    pub unsafe fn try_acquire_for_thread<T>(
        &self,
        current: T,
    ) -> Result<PiMutexAcquire, PiMutexStateError>
    where
        T: Into<PiTaskId>,
    {
        unsafe { self.view().try_acquire_for_thread(current) }
    }

    /// Attempts release for an explicitly scheduler-authorized identity.
    ///
    /// # Safety
    ///
    /// The caller must own scheduler authority for `current` and serialize the
    /// transition with the physical mutex owner.
    #[doc(hidden)]
    pub unsafe fn try_release_for_thread<T>(&self, current: T) -> Result<bool, PiMutexStateError>
    where
        T: Into<PiTaskId>,
    {
        unsafe { self.view().try_release_for_thread(current) }
    }

    /// Releases the physical owner named by this mutex's owner word.
    ///
    /// # Safety
    ///
    /// The caller must own this mutex through a higher-level raw-mutex
    /// contract and retain that authority through any contended handoff.
    pub unsafe fn try_release_owned(&self) -> Result<PiMutexOwnedRelease, PiMutexStateError> {
        unsafe { self.view().try_release_owned() }
    }

    /// Returns whether `current` is the physical owner.
    pub fn is_owned_by(&self, current: PiTaskId) -> bool {
        self.view().is_owned_by(current)
    }

    /// Returns whether the mutex is owned or in an ownerless handoff window.
    pub fn is_locked(&self) -> bool {
        self.view().is_locked()
    }

    /// Borrows this physical lock's generation-bearing scheduler identity.
    pub fn mutex_ref(&self) -> Result<PiMutexRef<'_>, PiMutexStateError> {
        self.view().mutex_ref()
    }

    /// Returns the lock-local owner snapshot protected by a provider wait lock.
    #[doc(hidden)]
    pub fn owner_snapshot(&self) -> PiMutexOwnerSnapshot {
        self.view().owner_snapshot()
    }

    /// Attempts to acquire an unlocked snapshot while the wait lock is held.
    #[doc(hidden)]
    pub fn try_acquire_snapshot(&self, snapshot: PiMutexOwnerSnapshot, current: PiTaskId) -> bool {
        self.view().try_acquire_snapshot(snapshot, current)
    }

    /// Publishes the waiter bit while the provider wait lock is held.
    #[doc(hidden)]
    pub fn try_mark_waiters(&self, snapshot: PiMutexOwnerSnapshot) -> bool {
        self.view().try_mark_waiters(snapshot)
    }

    /// Publishes an owned state after a serialized handoff claim.
    #[doc(hidden)]
    pub fn publish_owner(&self, owner: PiTaskId, has_waiters: bool) {
        self.view().publish_owner(owner, has_waiters);
    }

    /// Publishes the reserved ownerless handoff state.
    #[doc(hidden)]
    pub fn publish_ownerless(&self) {
        self.view().publish_ownerless();
    }

    /// Ends an ownerless handoff after its final waiter is removed.
    #[doc(hidden)]
    pub fn publish_unlocked(&self) {
        self.view().publish_unlocked();
    }

    /// Clears the waiter bit while retaining an existing owner.
    #[doc(hidden)]
    pub fn clear_waiters_bit(&self, owner: PiTaskId) {
        self.view().clear_waiters_bit(owner);
    }

    /// Returns the inline scheduler-owned waiter storage.
    #[doc(hidden)]
    pub const fn wait_storage(&self) -> PiMutexWaitStorageView<'_> {
        self.wait_storage.view()
    }

    /// Returns a borrowed view over this physical lock's complete PI storage.
    #[doc(hidden)]
    pub const fn view(&self) -> PiMutexCoreView<'_> {
        PiMutexCoreView::from_parts(
            &self.owner,
            &self.generation,
            &self.wait_storage.state,
            &self.wait_storage.words,
        )
    }
}

impl fmt::Debug for PiMutexCore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PiMutexCore")
            .field(
                "owner",
                &owner_from_word(self.owner.load(Ordering::Relaxed)),
            )
            .field("generation", &self.generation.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Default for PiMutexCore {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PiMutexCore {
    fn drop(&mut self) {
        if let Some(wait_handle) = self.wait_storage.take_initialized() {
            // SAFETY: mutable destruction makes every safe reference to this
            // core unreachable, and the waiter handle verifies its tree is
            // empty before releasing the inline object.
            unsafe { crate::drop_pi_mutex_wait_handle(wait_handle) };
        }
    }
}

/// Borrowed scheduler capability of one physical PI mutex.
#[derive(Clone, Copy, Debug)]
pub struct PiMutexRef<'lock> {
    core: PiMutexCoreView<'lock>,
    id: PiMutexId,
}

impl<'lock> PiMutexRef<'lock> {
    /// Returns the stable generation-bearing lock identity.
    pub const fn id(self) -> PiMutexId {
        self.id
    }

    /// Returns the borrowed physical core.
    #[doc(hidden)]
    pub const fn core(self) -> PiMutexCoreView<'lock> {
        self.core
    }

    /// Converts the borrow into a token-scoped raw capability.
    #[doc(hidden)]
    pub fn raw(self) -> PiMutexRaw {
        PiMutexRaw {
            owner: NonNull::from(self.core.owner),
            generation: NonNull::from(self.core.generation),
            wait_state: NonNull::from(self.core.wait_storage.state),
            wait_words: NonNull::from(self.core.wait_storage.words),
            id: self.id,
        }
    }
}

/// Raw generation-checked reference retained by a registered waiter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PiMutexRaw {
    owner: NonNull<AtomicU64>,
    generation: NonNull<AtomicU64>,
    wait_state: NonNull<AtomicU8>,
    wait_words: NonNull<UnsafeCell<[MaybeUninit<usize>; PI_MUTEX_WAIT_STORAGE_WORDS]>>,
    id: PiMutexId,
}

impl PiMutexRaw {
    /// Returns the stable lock identity.
    pub const fn id(self) -> PiMutexId {
        self.id
    }

    /// Recovers the physical lock core while its wait token is live.
    ///
    /// # Safety
    ///
    /// The caller must hold the registration whose token retained this raw
    /// capability and must not outlive the physical mutex.
    #[doc(hidden)]
    pub unsafe fn core(self) -> PiMutexCoreView<'static> {
        PiMutexCoreView {
            // SAFETY: the live scheduler registration retains every storage
            // field from the same physical mutex for this complete borrow.
            owner: unsafe { self.owner.as_ref() },
            // SAFETY: identical registration lifetime to `owner` above.
            generation: unsafe { self.generation.as_ref() },
            wait_storage: PiMutexWaitStorageView {
                // SAFETY: identical registration lifetime to `owner` above.
                state: unsafe { self.wait_state.as_ref() },
                // SAFETY: identical registration lifetime to `owner` above.
                words: unsafe { self.wait_words.as_ref() },
            },
        }
    }
}

// SAFETY: provider code may move the raw identity only while a live waiter
// registration keeps the physical mutex borrowed and its generation stable.
unsafe impl Send for PiMutexRaw {}
unsafe impl Sync for PiMutexRaw {}

/// Atomic owner snapshot serialized with a provider's waiter-tree lock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PiMutexOwnerSnapshot {
    word: u64,
    owner: Option<PiTaskId>,
}

impl PiMutexOwnerSnapshot {
    /// Returns the physical owner, if one exists.
    pub const fn owner(self) -> Option<PiTaskId> {
        self.owner
    }

    /// Returns whether the physical mutex is fully unlocked.
    pub const fn is_unlocked(self) -> bool {
        self.word == 0
    }

    /// Returns whether unlock reserved an ownerless waiter handoff.
    pub const fn is_ownerless(self) -> bool {
        self.word == OWNER_HAS_WAITERS
    }

    /// Returns whether the slow path owns waiter metadata.
    pub const fn has_waiters(self) -> bool {
        self.word & OWNER_HAS_WAITERS != 0
    }
}

/// Result of the atomic PI mutex fast acquisition path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PiMutexAcquire {
    /// The caller became the physical owner.
    Acquired,
    /// The caller must register in the task provider's waiter tree.
    Contended,
}

/// Result of an owner-authorized PI mutex release.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PiMutexOwnedRelease {
    /// The uncontended owner word was released atomically.
    Released,
    /// Provider metadata must select and wake the next waiter.
    Contended(PiTaskId),
}

/// Token joining one provider registration to the physical lock lifetime.
#[must_use = "a PI wait token must be granted or explicitly cancelled"]
#[derive(Debug)]
pub struct PiWaitToken {
    thread: PiTaskId,
    initial_owner: Option<PiTaskId>,
    generation: u64,
    lock: PiMutexRaw,
    provider_waiter: NonNull<()>,
}

impl PiWaitToken {
    /// Creates a token after the provider committed both waiter-tree edges.
    ///
    /// # Safety
    ///
    /// `lock`, `thread`, and `generation` must name one live registration, and
    /// that registration must keep the physical mutex alive until cancellation
    /// or handoff claim completes.
    #[doc(hidden)]
    pub const unsafe fn from_registration(
        lock: PiMutexRaw,
        thread: PiTaskId,
        initial_owner: Option<PiTaskId>,
        generation: u64,
        provider_waiter: NonNull<()>,
    ) -> Self {
        Self {
            thread,
            initial_owner,
            generation,
            lock,
            provider_waiter,
        }
    }

    /// Returns the registered task identity.
    pub const fn thread_id(&self) -> PiTaskId {
        self.thread
    }

    /// Returns the owner observed by the registration transaction.
    pub const fn initial_owner(&self) -> Option<PiTaskId> {
        self.initial_owner
    }

    /// Returns the task-local waiter generation.
    #[doc(hidden)]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns the registered physical lock identity.
    #[doc(hidden)]
    pub const fn lock_raw(&self) -> PiMutexRaw {
        self.lock
    }

    /// Returns the provider-owned task-local waiter capability.
    ///
    /// # Safety
    ///
    /// Only the provider that created this token may interpret the pointer,
    /// and only while the waiter registration remains live.
    #[doc(hidden)]
    pub const unsafe fn provider_waiter(&self) -> NonNull<()> {
        self.provider_waiter
    }

    /// Returns whether scheduler handoff completed for this generation.
    pub fn is_granted(&self) -> bool {
        crate::pi_waiter_is_granted(self)
    }

    /// Returns whether this waiter is first and the mutex is ownerless.
    pub fn can_claim(&self) -> bool {
        self.is_top_waiter() && unsafe { self.lock.core() }.owner_snapshot().is_ownerless()
    }

    /// Returns whether this waiter is currently first in the lock tree.
    pub fn is_top_waiter(&self) -> bool {
        crate::pi_waiter_is_top(self)
    }

    /// Returns whether the owner observed at registration still occupies a CPU.
    pub fn initial_owner_is_on_cpu(&self) -> bool {
        super::task_result(
            crate::pi_initial_owner_is_on_cpu(self),
            "observe PI mutex owner execution state",
        )
    }
}

/// Result of entering the PI mutex slow path.
#[must_use = "a registered PI waiter must be blocked, claimed, or cancelled"]
#[derive(Debug)]
pub enum PiMutexLockResult {
    /// A racing fast unlock let this caller acquire the mutex directly.
    Acquired,
    /// The caller is linked in the mutex-owned waiter tree.
    Waiting(PiWaitToken),
}

/// Result of serializing one ownerless PI-mutex claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PiMutexClaimOutcome {
    /// This waiter was still first and became the physical owner.
    Claimed,
    /// The owner or top waiter changed after the optimistic observation.
    Retry,
}

/// Result of trying to cancel one committed PI waiter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PiWaitCancelOutcome {
    /// The waiter and all inherited donations were removed.
    Cancelled,
    /// Unlock already published an ownerless handoff to this waiter.
    HandoffPending,
}

fn owner_from_word(state: u64) -> Option<PiTaskId> {
    PiTaskId::new(state & OWNER_ID_MASK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrowed_core_view_keeps_external_fields_authoritative() {
        let owner = AtomicU64::new(0);
        let generation = AtomicU64::new(0);
        let wait_state = AtomicU8::new(WAIT_STORAGE_UNINITIALIZED);
        let wait_words = UnsafeCell::new([MaybeUninit::uninit(); PI_MUTEX_WAIT_STORAGE_WORDS]);
        let core = PiMutexCoreView::from_parts(&owner, &generation, &wait_state, &wait_words);
        let task = PiTaskId::new(7).unwrap();

        assert_eq!(core.try_acquire(task), Ok(PiMutexAcquire::Acquired));
        assert_eq!(owner.load(Ordering::Relaxed), task.get());
        let lock = core.mutex_ref().unwrap();
        let recovered = unsafe {
            // SAFETY: `raw` remains bounded by all local backing fields.
            lock.raw().core()
        };
        assert_eq!(recovered.mutex_ref().unwrap().id(), lock.id());
        assert!(recovered.is_owned_by(task));
        assert!(
            unsafe {
                // SAFETY: this test established `task` as the physical owner.
                recovered.try_release_for_thread(task)
            }
            .unwrap()
        );
        assert_eq!(owner.load(Ordering::Relaxed), 0);

        let waiter = unsafe {
            // SAFETY: this local storage is used only with `u64`, which fits
            // the published inline size and alignment.
            core.wait_storage().get_or_init(|| 0x5a5a_u64)
        };
        assert_eq!(*waiter, 0x5a5a);
        assert!(core.wait_storage().is_initialized());
    }
}
