use super::*;

#[derive(Debug)]
pub(super) struct OwnerState {
    claimed: AtomicBool,
    current_thread: AtomicU64,
    idle_thread: AtomicU64,
    busy_runtime_ns: AtomicU64,
}

impl OwnerState {
    pub(super) const fn new() -> Self {
        Self {
            claimed: AtomicBool::new(false),
            current_thread: AtomicU64::new(0),
            idle_thread: AtomicU64::new(0),
            busy_runtime_ns: AtomicU64::new(0),
        }
    }
}

impl CpuRemote {
    /// Returns the CPU that owns the corresponding runqueue.
    pub const fn owner(&self) -> CpuId {
        self.owner
    }

    /// Claims exclusive access to the corresponding owner-only scheduler object.
    ///
    /// # Safety
    ///
    /// `cpu` must identify the pinned, live [`CpuLocal`] associated with this
    /// endpoint. After runtime publication, every access that can overlap this
    /// claim must use the same endpoint rather than retaining an ungated borrow.
    pub unsafe fn claim_local(
        &self,
        cpu: *mut CpuLocal,
    ) -> Result<CpuLocalOwnerBorrow<'_>, TaskError> {
        let cpu = NonNull::new(cpu).ok_or(TaskError::InvalidRuntimeHandle)?;
        self.owner_state
            .claimed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .map_err(|_| TaskError::CpuOwnerBorrowed)?;

        // SAFETY: the caller guarantees that this is the live pinned CpuLocal
        // paired with this endpoint. The successful gate claim excludes every
        // other runtime-derived reference while the identity is checked.
        let actual = unsafe { cpu.as_ref() }.owner();
        if actual != self.owner {
            self.owner_state.claimed.store(false, Ordering::Release);
            return Err(TaskError::CpuOwnerMismatch {
                expected: self.owner.as_u32(),
                actual: actual.as_u32(),
            });
        }
        Ok(CpuLocalOwnerBorrow {
            remote: self,
            cpu,
            _not_send_or_sync: PhantomData,
        })
    }

    /// Returns the generation-bearing current-thread snapshot.
    pub fn current_thread(&self) -> Option<ThreadId> {
        decode_thread_id(self.owner_state.current_thread.load(Ordering::Acquire))
    }

    /// Returns the configured idle-thread snapshot.
    pub fn idle_thread(&self) -> Option<ThreadId> {
        decode_thread_id(self.owner_state.idle_thread.load(Ordering::Acquire))
    }

    pub(crate) fn publish_current_thread(&self, current: Option<ThreadId>) {
        self.owner_state
            .current_thread
            .store(current.map_or(0, ThreadId::as_u64), Ordering::Release);
    }

    pub(in crate::system::cpu) fn publish_idle_thread(&self, idle: ThreadId) {
        self.owner_state
            .idle_thread
            .store(idle.as_u64(), Ordering::Release);
    }

    /// Returns cumulative time this CPU has executed non-idle scheduler threads.
    pub fn busy_runtime_ns(&self) -> u64 {
        self.owner_state.busy_runtime_ns.load(Ordering::Relaxed)
    }

    pub(in crate::system::cpu) fn charge_busy_runtime(&self, runtime_ns: u64) {
        self.owner_state
            .busy_runtime_ns
            .fetch_add(runtime_ns, Ordering::Relaxed);
    }
}

/// Dynamically checked owner borrow of one pinned [`CpuLocal`].
///
/// The borrow gate resides in the separately allocated [`CpuRemote`] endpoint,
/// so a reentrant claim can fail without touching memory covered by the active
/// mutable `CpuLocal` reference.
pub struct CpuLocalOwnerBorrow<'remote> {
    remote: &'remote CpuRemote,
    cpu: NonNull<CpuLocal>,
    _not_send_or_sync: PhantomData<*mut ()>,
}

impl CpuLocalOwnerBorrow<'_> {
    /// Borrows the pinned owner state mutably for one audited call scope.
    pub fn as_pin_mut(&mut self) -> Pin<&mut CpuLocal> {
        // SAFETY: construction claimed the unique runtime owner gate, the
        // pointer remains pinned, and the returned lifetime is bounded by the
        // mutable borrow of this gate-owning wrapper.
        unsafe { Pin::new_unchecked(self.cpu.as_mut()) }
    }
}

impl Deref for CpuLocalOwnerBorrow<'_> {
    type Target = CpuLocal;

    fn deref(&self) -> &Self::Target {
        // SAFETY: the wrapper owns the endpoint's exclusive claim and its
        // lifetime is bounded by that claim.
        unsafe { self.cpu.as_ref() }
    }
}

impl Drop for CpuLocalOwnerBorrow<'_> {
    fn drop(&mut self) {
        self.remote
            .owner_state
            .claimed
            .store(false, Ordering::Release);
    }
}

fn decode_thread_id(raw: u64) -> Option<ThreadId> {
    (raw != 0).then(|| ThreadId::from_parts(raw as u32, (raw >> 32) as u32))
}
