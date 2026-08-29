use super::*;

const CPU_LIFECYCLE_OFFLINE: usize = 1 << (usize::BITS - 1);
const CPU_LIFECYCLE_INACTIVE: usize = 1 << (usize::BITS - 2);
const CPU_LIFECYCLE_DRAINING: usize = CPU_LIFECYCLE_OFFLINE | CPU_LIFECYCLE_INACTIVE;
const CPU_LIFECYCLE_MASK: usize = CPU_LIFECYCLE_DRAINING;
const CPU_PUBLICATION_COUNT_MASK: usize = !CPU_LIFECYCLE_MASK;
const CPU_PUBLICATION_OVERFLOW_INVARIANT: u32 = 0x4350_5542;
const CPU_PUBLICATION_RELEASE_INVARIANT: u32 = 0x4350_5544;

pub(super) const INITIAL_CPU_LIFECYCLE_STATE: usize = CPU_LIFECYCLE_OFFLINE;

#[derive(Clone, Copy)]
enum CpuPublicationClass {
    Placement,
    OwnerControl,
}

impl CpuPublicationClass {
    const fn accepts(self, state: usize) -> bool {
        match self {
            Self::Placement => state & CPU_LIFECYCLE_MASK == 0,
            Self::OwnerControl => state & CPU_LIFECYCLE_OFFLINE == 0,
        }
    }
}

#[derive(Debug)]
pub(super) struct CpuPublicationState {
    state: AtomicUsize,
}

impl CpuPublicationState {
    pub(super) const fn new() -> Self {
        Self {
            state: AtomicUsize::new(INITIAL_CPU_LIFECYCLE_STATE),
        }
    }
}

/// Placement and remote-publication state of one logical CPU.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuLifecycleState {
    /// The CPU accepts placement and remote scheduler publications.
    Online,
    /// New placement is closed while owner-directed control delivery may finish.
    Inactive,
    /// Every remote publication is closed while the owner proves work is gone.
    Draining,
    /// The CPU owns no schedulable work and is absent from the root domain.
    Offline,
}

impl CpuRemote {
    /// Returns the CPU's placement and publication lifecycle.
    pub fn lifecycle_state(&self) -> CpuLifecycleState {
        match self.publication.state.load(Ordering::Acquire) & CPU_LIFECYCLE_MASK {
            0 => CpuLifecycleState::Online,
            CPU_LIFECYCLE_INACTIVE => CpuLifecycleState::Inactive,
            CPU_LIFECYCLE_DRAINING => CpuLifecycleState::Draining,
            CPU_LIFECYCLE_OFFLINE => CpuLifecycleState::Offline,
            _ => unreachable!("CPU lifecycle mask has four encoded states"),
        }
    }

    /// Returns whether owner initialization and online publication completed.
    pub fn is_online(&self) -> bool {
        matches!(
            self.lifecycle_state(),
            CpuLifecycleState::Online | CpuLifecycleState::Inactive
        )
    }

    /// Returns whether new runnable placement may target this CPU.
    pub(crate) fn accepts_placement(&self) -> bool {
        self.lifecycle_state() == CpuLifecycleState::Online
    }

    pub(crate) fn mark_online(&self) -> bool {
        self.publication
            .state
            .compare_exchange(
                CPU_LIFECYCLE_OFFLINE,
                0,
                Ordering::Release,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(crate) fn try_deactivate(&self) -> bool {
        // Matching the exact zero-valued Online state proves that no target-rq
        // placement transaction spans the transition. Owner-directed control
        // delivery remains allowed until final draining.
        let inactive = self
            .publication
            .state
            .compare_exchange(
                0,
                CPU_LIFECYCLE_INACTIVE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        if inactive {
            self.cancel_idle_pull_if_uncommitted();
        }
        inactive
    }

    pub(crate) fn cancel_deactivation(&self) {
        let mut current = self.publication.state.load(Ordering::Acquire);
        loop {
            if current & CPU_LIFECYCLE_MASK != CPU_LIFECYCLE_INACTIVE {
                task_runtime::fatal_invariant(
                    CPU_PUBLICATION_RELEASE_INVARIANT,
                    self.owner.as_u32() as usize,
                );
            }
            let online = current & !CPU_LIFECYCLE_INACTIVE;
            match self.publication.state.compare_exchange_weak(
                current,
                online,
                Ordering::Release,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    pub(crate) fn try_begin_draining(&self) -> bool {
        // An exact inactive state also proves that every owner-directed control
        // delivery has completed its publication and doorbell transaction.
        self.publication
            .state
            .compare_exchange(
                CPU_LIFECYCLE_INACTIVE,
                CPU_LIFECYCLE_DRAINING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(crate) fn cancel_draining(&self) {
        if self
            .publication
            .state
            .compare_exchange(
                CPU_LIFECYCLE_DRAINING,
                0,
                Ordering::Release,
                Ordering::Acquire,
            )
            .is_err()
        {
            task_runtime::fatal_invariant(
                CPU_PUBLICATION_RELEASE_INVARIANT,
                self.owner.as_u32() as usize,
            );
        }
    }

    pub(crate) fn finish_offline(&self) {
        self.reset_scheduler_for_offline();
        if self
            .publication
            .state
            .compare_exchange(
                CPU_LIFECYCLE_DRAINING,
                CPU_LIFECYCLE_OFFLINE,
                Ordering::Release,
                Ordering::Acquire,
            )
            .is_err()
        {
            task_runtime::fatal_invariant(
                CPU_PUBLICATION_RELEASE_INVARIANT,
                self.owner.as_u32() as usize,
            );
        }
    }

    pub(crate) fn begin_publication(&self) -> Option<CpuRemotePublication<'_>> {
        self.try_acquire_publication(CpuPublicationClass::Placement)
            .then(|| CpuRemotePublication { remote: self })
    }

    fn try_acquire_publication(&self, class: CpuPublicationClass) -> bool {
        let mut current = self.publication.state.load(Ordering::Acquire);
        loop {
            if !class.accepts(current) {
                return false;
            }
            let count = current & CPU_PUBLICATION_COUNT_MASK;
            if count == CPU_PUBLICATION_COUNT_MASK {
                task_runtime::fatal_invariant(
                    CPU_PUBLICATION_OVERFLOW_INVARIANT,
                    self.owner.as_u32() as usize,
                );
            }
            match self.publication.state.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    #[cfg(feature = "qperf-metrics")]
                    match class {
                        CpuPublicationClass::Placement => {
                            crate::metrics::record_cpu_placement_publication_acquire();
                        }
                        CpuPublicationClass::OwnerControl => {
                            crate::metrics::record_cpu_owner_control_publication_acquire();
                        }
                    }
                    return true;
                }
                Err(actual) => current = actual,
            }
        }
    }

    /// Pins an online placement target across an owner context switch.
    ///
    /// Linux holds the CPU-hotplug/read-side ownership that makes the selected
    /// destination runqueue stable before it commits `TASK_ON_RQ_MIGRATING`.
    /// The owned form carries the same lifetime proof through switch tail,
    /// where a borrowed runqueue guard cannot survive the architecture switch.
    pub(crate) fn begin_owned_publication(self: &Arc<Self>) -> Option<OwnedCpuRemotePublication> {
        self.try_acquire_publication(CpuPublicationClass::Placement)
            .then(|| OwnedCpuRemotePublication {
                remote: Arc::clone(self),
            })
    }

    pub(crate) fn begin_owner_delivery(&self) -> Option<CpuRemotePublication<'_>> {
        self.try_acquire_publication(CpuPublicationClass::OwnerControl)
            .then(|| CpuRemotePublication { remote: self })
    }

    pub(crate) fn is_quiescent_for_offline(&self) -> bool {
        self.publication.state.load(Ordering::Acquire) == CPU_LIFECYCLE_DRAINING
            && self.ktimer_is_quiescent_for_offline()
            && self.deadline_is_quiescent_for_offline()
            && !self.needs_reschedule()
            && !self.has_remote_work()
            && !self.is_idle_polling()
            && self.idle_pull_is_quiescent()
    }
}

pub(crate) struct CpuRemotePublication<'remote> {
    remote: &'remote CpuRemote,
}

#[derive(Debug)]
pub(crate) struct OwnedCpuRemotePublication {
    remote: Arc<CpuRemote>,
}

impl OwnedCpuRemotePublication {
    pub(crate) fn publish_owner_control(
        self,
        node: Pin<&'static InboxNode>,
        message: InboxMessage,
    ) -> PublishResult {
        self.remote.publish_owner_control_owned(node, message)
    }
}

impl CpuRemotePublication<'_> {
    pub(super) fn reserves(&self, remote: &CpuRemote) -> bool {
        core::ptr::eq(self.remote, remote)
    }

    pub(crate) fn publish_owner_control(
        self,
        node: Pin<&'static InboxNode>,
        message: InboxMessage,
    ) -> PublishResult {
        self.remote.publish_owner_control_owned(node, message)
    }
}

impl Drop for CpuRemotePublication<'_> {
    fn drop(&mut self) {
        release_publication(self.remote);
    }
}

impl Drop for OwnedCpuRemotePublication {
    fn drop(&mut self) {
        release_publication(&self.remote);
    }
}

fn release_publication(remote: &CpuRemote) {
    let mut current = remote.publication.state.load(Ordering::Acquire);
    loop {
        if current & CPU_LIFECYCLE_OFFLINE != 0 || current & CPU_PUBLICATION_COUNT_MASK == 0 {
            task_runtime::fatal_invariant(
                CPU_PUBLICATION_RELEASE_INVARIANT,
                remote.owner.as_u32() as usize,
            );
        }
        match remote.publication.state.compare_exchange_weak(
            current,
            current - 1,
            Ordering::Release,
            Ordering::Acquire,
        ) {
            Ok(_) => return,
            Err(actual) => current = actual,
        }
    }
}
