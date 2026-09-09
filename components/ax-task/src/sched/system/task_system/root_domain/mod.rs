//! Linux-style root-domain topology, priority indexes, and Deadline bandwidth ownership.

mod fair_nohz;
mod rt_bandwidth;

use core::{
    cmp::Reverse,
    ops::Deref,
    sync::atomic::{AtomicUsize, Ordering},
};

use fair_nohz::RootDomainFairNoHz;
pub(in crate::sched::system::task_system) use fair_nohz::RootDomainFairNoHzClaim;

use super::*;
use crate::{
    runtime::lock::PreemptTicketGuard,
    sched::{
        RtPriority,
        algorithm::{DEADLINE_UTILIZATION_SCALE, RootRtBandwidth},
    },
};

/// The scheduler-wide owner corresponding to Linux `struct root_domain`.
///
/// Runqueues remain the physical owner of runnable entities and local
/// `this_bw`/`running_bw`. This object owns facts shared by those runqueues:
/// online topology, Deadline admission, and cpupri/cpudl indexes. Every
/// runqueue stores its own published
/// `extra_bw`, matching Linux `dl_rq`, while this object owns the root-domain
/// total used to derive those values.
#[derive(Debug)]
pub(super) struct RootDomain {
    state: PreemptTicketLock<RootDomainState>,
    online_count: AtomicUsize,
    priority: RootDomainPriorityIndex,
    overload: RootDomainOverloadIndex,
    realtime_push: RootDomainPushIterator,
    deadline_push: RootDomainPushIterator,
    fair_nohz: RootDomainFairNoHz,
    runqueues: Vec<Arc<CpuRemote>>,
    rt_bandwidth: Arc<RootRtBandwidth>,
    deadline_max_bw_scaled: u64,
}

/// Linux `rto_mask`/`dlo_mask` and their publication counts.
///
/// Each bit is published while its CPU owns the corresponding rq lock. A set
/// transition publishes the mask before the count; a clear transition removes
/// the count before the mask. Readers may therefore use the count as the fast
/// path and then scan a mask without observing an increment whose bit is still
/// absent.
#[derive(Debug)]
struct RootDomainOverloadIndex {
    realtime: RootDomainOverloadMask,
    deadline: RootDomainOverloadMask,
}

#[derive(Debug)]
struct RootDomainOverloadMask {
    count: AtomicUsize,
    words: Vec<AtomicUsize>,
}

/// The single root-domain push iterator corresponding to Linux
/// `rto_push_work`.
///
/// A priority drop starts one serialized scan instead of broadcasting an IPI
/// to every overloaded rq. The target owner claims the published generation,
/// performs its rq-local push callback, then hands the scan to the next owner.
#[derive(Debug)]
struct RootDomainPushIterator {
    state: PreemptTicketLock<RootDomainPushState>,
    /// Zero means no published target; otherwise this is `CpuId + 1`.
    ///
    /// The serialized state remains authoritative for claim and completion.
    /// This atomic mirrors only the Linux `irq_work`-style fact needed by the
    /// ordinary switch path, so an unrelated CPU never waits on `rto_lock` to
    /// discover that it has no push callback to run.
    published_target: AtomicUsize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RootDomainPushClass {
    Realtime,
    Deadline,
}

impl RootDomainPushClass {
    pub(super) const fn scheduling_class(self) -> SchedulingClass {
        match self {
            Self::Realtime => SchedulingClass::Realtime,
            Self::Deadline => SchedulingClass::Deadline,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RootDomainPushPhase {
    Idle,
    Published(CpuId),
    Claimed(CpuId),
}

#[derive(Debug)]
struct RootDomainPushState {
    requested_generation: u64,
    scan_generation: u64,
    cursor: Option<CpuId>,
    phase: RootDomainPushPhase,
}

#[derive(Debug)]
pub(super) struct RootDomainPushClaim {
    source: CpuId,
    generation: u64,
    class: RootDomainPushClass,
}

impl RootDomainPushClaim {
    pub(super) const fn class(&self) -> RootDomainPushClass {
        self.class
    }
}

#[derive(Debug)]
pub(super) struct RootDomainState {
    pub(super) online: CpuSet,
    deadline_admission: DeadlineAdmission,
}

pub(super) struct RootDomainGuard<'domain> {
    owner: &'domain RootDomain,
    state: PreemptTicketGuard<'domain, RootDomainState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct DeadlineBandwidthRebuild {
    pub(super) online_cpus: u32,
    pub(super) reserved_scaled: u64,
    pub(super) distributed_scaled: u64,
}

impl RootDomain {
    pub(super) fn new(config: TaskSystemConfig, runqueues: Vec<Arc<CpuRemote>>) -> Self {
        debug_assert_eq!(config.cpu_count(), runqueues.len());
        let deadline_max_bw_scaled =
            u64::from(config.deadline_cap_percent()) * DEADLINE_UTILIZATION_SCALE / 100;
        Self {
            state: PreemptTicketLock::new(RootDomainState {
                online: CpuSet::empty(config.cpu_count()),
                deadline_admission: DeadlineAdmission::new(config.deadline_cap_percent()),
            }),
            online_count: AtomicUsize::new(0),
            priority: RootDomainPriorityIndex::new(config.cpu_count()),
            overload: RootDomainOverloadIndex::new(config.cpu_count()),
            realtime_push: RootDomainPushIterator::new(),
            deadline_push: RootDomainPushIterator::new(),
            fair_nohz: RootDomainFairNoHz::new(config.cpu_count()),
            runqueues,
            rt_bandwidth: Arc::new(RootRtBandwidth::new(config)),
            deadline_max_bw_scaled,
        }
    }

    pub(super) fn rt_bandwidth(&self) -> &Arc<RootRtBandwidth> {
        &self.rt_bandwidth
    }

    pub(super) fn lock(&self) -> RootDomainGuard<'_> {
        RootDomainGuard {
            owner: self,
            state: self.state.lock(),
        }
    }

    /// Returns Linux `sd_llc_size` for the current flat root-domain model.
    ///
    /// Cache topology is not published yet, so the highest shared-cache
    /// domain is represented by all online CPUs. Before topology publication,
    /// Linux likewise falls back to a factor of one.
    pub(super) fn fair_wake_domain_size(&self) -> u32 {
        self.online_count.load(Ordering::Acquire).max(1) as u32
    }
}

impl RootDomainOverloadIndex {
    fn new(cpu_count: usize) -> Self {
        Self {
            realtime: RootDomainOverloadMask::new(cpu_count),
            deadline: RootDomainOverloadMask::new(cpu_count),
        }
    }

    fn publish(&self, cpu: CpuId, realtime: bool, deadline: bool) {
        self.realtime.publish(cpu, realtime);
        self.deadline.publish(cpu, deadline);
    }

    fn contains_any(&self, cpu: CpuId) -> bool {
        self.deadline.contains(cpu) || self.realtime.contains(cpu)
    }

    fn contains(&self, cpu: CpuId, class: SchedulingClass) -> bool {
        match class {
            SchedulingClass::Realtime => self.realtime.contains(cpu),
            SchedulingClass::Deadline => self.deadline.contains(cpu),
            SchedulingClass::Stop | SchedulingClass::Fair => false,
        }
    }

    fn contains_class(&self, cpu: CpuId, class: RootDomainPushClass) -> bool {
        match class {
            RootDomainPushClass::Realtime => self.realtime.contains(cpu),
            RootDomainPushClass::Deadline => self.deadline.contains(cpu),
        }
    }

    fn any_class(&self, class: RootDomainPushClass) -> bool {
        match class {
            RootDomainPushClass::Realtime => self.realtime.count.load(Ordering::Acquire) != 0,
            RootDomainPushClass::Deadline => self.deadline.count.load(Ordering::Acquire) != 0,
        }
    }

    fn find_next_class(
        &self,
        class: RootDomainPushClass,
        cursor: Option<CpuId>,
        excluded: CpuId,
        accepts: impl FnMut(CpuId) -> bool,
    ) -> Option<CpuId> {
        match class {
            RootDomainPushClass::Realtime => {
                self.realtime.find_next_after(cursor, excluded, accepts)
            }
            RootDomainPushClass::Deadline => {
                self.deadline.find_next_after(cursor, excluded, accepts)
            }
        }
    }
}

impl RootDomainPushIterator {
    const fn new() -> Self {
        Self {
            state: PreemptTicketLock::new(RootDomainPushState {
                requested_generation: 0,
                scan_generation: 0,
                cursor: None,
                phase: RootDomainPushPhase::Idle,
            }),
            published_target: AtomicUsize::new(0),
        }
    }

    fn lock_state(&self) -> PreemptTicketGuard<'_, RootDomainPushState> {
        self.state.lock()
    }

    fn target_token(source: CpuId) -> usize {
        source
            .as_usize()
            .checked_add(1)
            .expect("a root-domain push target must fit the configured CPU topology")
    }

    fn publish_target(&self, state: &mut RootDomainPushState, target: Option<CpuId>) {
        state.phase = target.map_or(RootDomainPushPhase::Idle, RootDomainPushPhase::Published);
        let token = target.map_or(0, Self::target_token);
        self.published_target.store(token, Ordering::Release);
    }

    fn clear_published_target(&self) {
        self.published_target.store(0, Ordering::Release);
    }

    fn has_published_target(&self, source: CpuId) -> bool {
        self.published_target.load(Ordering::Acquire) == Self::target_token(source)
    }
}

impl RootDomainOverloadMask {
    fn new(cpu_count: usize) -> Self {
        let word_count = cpu_count.div_ceil(usize::BITS as usize);
        Self {
            count: AtomicUsize::new(0),
            words: (0..word_count).map(|_| AtomicUsize::new(0)).collect(),
        }
    }

    fn publish(&self, cpu: CpuId, present: bool) {
        let word_index = cpu.as_usize() / usize::BITS as usize;
        let bit = 1usize << (cpu.as_usize() % usize::BITS as usize);
        let Some(word) = self.words.get(word_index) else {
            return;
        };
        let already_present = word.load(Ordering::Acquire) & bit != 0;
        if already_present == present {
            return;
        }
        if present {
            word.fetch_or(bit, Ordering::Release);
            self.count.fetch_add(1, Ordering::Release);
        } else {
            let previous = self.count.fetch_sub(1, Ordering::AcqRel);
            assert_ne!(previous, 0, "root-domain overload count underflowed");
            word.fetch_and(!bit, Ordering::Release);
        }
    }

    fn contains(&self, cpu: CpuId) -> bool {
        let word_index = cpu.as_usize() / usize::BITS as usize;
        let bit = 1usize << (cpu.as_usize() % usize::BITS as usize);
        self.words
            .get(word_index)
            .is_some_and(|word| word.load(Ordering::Acquire) & bit != 0)
    }

    fn find_next_after(
        &self,
        cursor: Option<CpuId>,
        excluded: CpuId,
        mut accepts: impl FnMut(CpuId) -> bool,
    ) -> Option<CpuId> {
        if self.count.load(Ordering::Acquire) == 0 {
            return None;
        }
        let first = cursor.map_or(0, |cpu| cpu.as_usize().saturating_add(1));
        let first_word = first / usize::BITS as usize;
        let first_bit = first % usize::BITS as usize;
        for (word_index, word) in self.words.iter().enumerate().skip(first_word) {
            let mut members = word.load(Ordering::Acquire);
            if word_index == first_word {
                members &= usize::MAX << first_bit;
            }
            if excluded.as_usize() / usize::BITS as usize == word_index {
                members &= !(1usize << (excluded.as_usize() % usize::BITS as usize));
            }
            while members != 0 {
                let bit = members.trailing_zeros() as usize;
                members &= members - 1;
                let index = word_index
                    .saturating_mul(usize::BITS as usize)
                    .saturating_add(bit);
                let cpu = CpuId::new(index as u32);
                if accepts(cpu) {
                    return Some(cpu);
                }
            }
        }
        None
    }
}

impl RootDomainGuard<'_> {
    pub(super) fn reserve_deadline(
        &mut self,
        policy: SchedulePolicy,
        affinity: &CpuSet,
    ) -> Result<u64, TaskError> {
        let reservation = self.deadline_reservation_for(policy, affinity)?;
        if reservation != 0 {
            self.state
                .deadline_admission
                .reserve_utilization(reservation)?;
            self.owner
                .replace_deadline_bandwidth(&self.state, 0, reservation);
        }
        Ok(reservation)
    }

    pub(super) fn deadline_reservation_for(
        &self,
        policy: SchedulePolicy,
        affinity: &CpuSet,
    ) -> Result<u64, TaskError> {
        match policy {
            SchedulePolicy::Deadline(deadline) => {
                if !affinity.covers(&self.state.online) {
                    return Err(TaskError::DeadlineAffinity);
                }
                Ok(DeadlineAdmission::utilization(deadline))
            }
            _ => Ok(0),
        }
    }

    pub(super) fn replace_deadline_utilization(
        &mut self,
        old_utilization: u64,
        new_utilization: u64,
    ) -> Result<(), TaskError> {
        if old_utilization == new_utilization {
            return Ok(());
        }
        self.state
            .deadline_admission
            .replace_utilization(old_utilization, new_utilization)?;
        self.owner
            .replace_deadline_bandwidth(&self.state, old_utilization, new_utilization);
        Ok(())
    }

    pub(super) fn release_deadline(&mut self, utilization: u64) {
        if utilization == 0 {
            return;
        }
        self.replace_deadline_utilization(utilization, 0)
            .expect("root-domain Deadline release must match an admitted reservation");
    }

    pub(super) fn admission_overcommitted(&self) -> bool {
        self.state.deadline_admission.reserved_scaled()
            > self.state.deadline_admission.capacity_scaled()
    }

    pub(super) fn can_deactivate_cpu(&self, cpu: CpuId) -> bool {
        if !self.state.online.contains(cpu) {
            return false;
        }
        let remaining = self.state.online.count() - 1;
        let remaining =
            u64::try_from(remaining).expect("validated root-domain topology must fit CpuId");
        let capacity = remaining * self.owner.deadline_max_bw_scaled;
        self.state.deadline_admission.reserved_scaled() <= capacity
    }

    pub(super) fn insert_online(&mut self, cpu: CpuId, rebuild: DeadlineBandwidthRebuild) -> bool {
        if !self.state.online.insert(cpu) {
            return false;
        }
        self.owner
            .rebuild_deadline_bandwidth(&mut self.state, rebuild);
        self.owner
            .online_count
            .store(self.state.online.count(), Ordering::Release);
        true
    }

    pub(super) fn remove_online(&mut self, cpu: CpuId, rebuild: DeadlineBandwidthRebuild) -> bool {
        if !self.state.online.remove(cpu) {
            return false;
        }
        self.owner
            .rebuild_deadline_bandwidth(&mut self.state, rebuild);
        self.owner
            .online_count
            .store(self.state.online.count(), Ordering::Release);
        true
    }
}

impl Deref for RootDomainGuard<'_> {
    type Target = RootDomainState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

mod publication;

mod fair_balance;

mod push;

mod admission;
