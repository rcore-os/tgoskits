use super::*;

#[derive(Debug)]
pub(super) struct OwnerState {
    claimed: AtomicBool,
    idle_thread: AtomicU64,
    busy_runtime_ns: AtomicU64,
    #[cfg(feature = "qperf-metrics")]
    owner_claims: AtomicU64,
}

impl OwnerState {
    pub(super) const fn new() -> Self {
        Self {
            claimed: AtomicBool::new(false),
            idle_thread: AtomicU64::new(0),
            busy_runtime_ns: AtomicU64::new(0),
            #[cfg(feature = "qperf-metrics")]
            owner_claims: AtomicU64::new(0),
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
        #[cfg(feature = "qperf-metrics")]
        {
            self.owner_state
                .owner_claims
                .fetch_add(1, Ordering::Relaxed);
            crate::metrics::record_runtime_cpu_owner_claim();
        }
        Ok(CpuLocalOwnerBorrow {
            remote: self,
            cpu,
            release_claim: true,
            _not_send_or_sync: PhantomData,
        })
    }

    /// Borrows the owner-only scheduler state under an existing scheduler baton.
    ///
    /// # Safety
    ///
    /// `cpu` must identify this endpoint's pinned [`CpuLocal`]. The caller must
    /// own the CPU's IRQ-off scheduler frame for the complete returned borrow,
    /// and no dynamically claimed owner borrow may overlap it.
    pub unsafe fn borrow_local_in_scheduler_frame(
        &self,
        cpu: *mut CpuLocal,
    ) -> Result<CpuLocalOwnerBorrow<'_>, TaskError> {
        let cpu = NonNull::new(cpu).ok_or(TaskError::InvalidRuntimeHandle)?;
        // SAFETY: the caller's scheduler baton pins this exact CpuLocal and
        // excludes every other owner-side entry until the borrow is dropped.
        let actual = unsafe { cpu.as_ref() }.owner();
        if actual != self.owner {
            return Err(TaskError::CpuOwnerMismatch {
                expected: self.owner.as_u32(),
                actual: actual.as_u32(),
            });
        }
        Ok(CpuLocalOwnerBorrow {
            remote: self,
            cpu,
            release_claim: false,
            _not_send_or_sync: PhantomData,
        })
    }

    /// Returns `rq->curr` under the authoritative runqueue lock.
    pub fn current_thread(&self) -> Option<ThreadId> {
        self.lock_run_queue(RunQueueGuardSource::OwnerCurrentThreadObservation)
            .current_thread()
    }

    /// Returns the configured idle-thread snapshot.
    pub fn idle_thread(&self) -> Option<ThreadId> {
        decode_thread_id(self.owner_state.idle_thread.load(Ordering::Acquire))
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

    #[cfg(feature = "qperf-metrics")]
    pub(crate) fn qperf_owner_claims(&self) -> u64 {
        self.owner_state.owner_claims.load(Ordering::Relaxed)
    }

    pub(in crate::system::cpu) fn charge_busy_runtime(&self, runtime_ns: u64) {
        self.owner_state
            .busy_runtime_ns
            .fetch_add(runtime_ns, Ordering::Relaxed);
    }
}

/// Exclusive owner borrow of one pinned [`CpuLocal`].
///
/// Ordinary callers acquire the dynamic gate in the separately allocated
/// [`CpuRemote`] endpoint. A live IRQ-off scheduler frame may instead lend its
/// stronger CPU-owner baton to the same borrow type without another atomic
/// ownership transaction.
pub struct CpuLocalOwnerBorrow<'remote> {
    remote: &'remote CpuRemote,
    cpu: NonNull<CpuLocal>,
    release_claim: bool,
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
        if self.release_claim {
            self.remote
                .owner_state
                .claimed
                .store(false, Ordering::Release);
        }
    }
}

fn decode_thread_id(raw: u64) -> Option<ThreadId> {
    (raw != 0).then(|| ThreadId::from_parts(raw as u32, (raw >> 32) as u32))
}
