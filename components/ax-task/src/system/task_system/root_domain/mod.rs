//! Linux-style root-domain topology, priority indexes, and Deadline bandwidth ownership.

mod rt_bandwidth;

use core::{
    ops::Deref,
    sync::atomic::{AtomicUsize, Ordering},
};

use super::*;
use crate::{DEADLINE_UTILIZATION_SCALE, RootRtBandwidth, RtPriority, lock::PreemptTicketGuard};

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
    priority: RootDomainPriorityIndex,
    overload: RootDomainOverloadIndex,
    realtime_push: RootDomainPushIterator,
    deadline_push: RootDomainPushIterator,
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

#[cfg(any(test, all(axtest, feature = "axtest")))]
static ROOT_PUSH_STATE_LOCK_ACQUISITIONS: AtomicUsize = AtomicUsize::new(0);

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
            priority: RootDomainPriorityIndex::new(config.cpu_count()),
            overload: RootDomainOverloadIndex::new(config.cpu_count()),
            realtime_push: RootDomainPushIterator::new(),
            deadline_push: RootDomainPushIterator::new(),
            runqueues,
            rt_bandwidth: Arc::new(RootRtBandwidth::new(config)),
            deadline_max_bw_scaled,
        }
    }

    pub(super) fn rt_bandwidth(&self) -> &Arc<RootRtBandwidth> {
        &self.rt_bandwidth
    }

    pub(super) fn has_multiple_online_priority_cpus(&self) -> bool {
        self.priority.has_multiple_online_cpus()
    }

    pub(super) fn lock(&self) -> RootDomainGuard<'_> {
        RootDomainGuard {
            owner: self,
            state: self.state.lock(),
        }
    }

    pub(super) fn publish_run_queue(&self, cpu: CpuId, run_queue: &CpuRunQueueState, online: bool) {
        let rt_throttled = run_queue.rt_is_throttled();
        let rt_effectively_throttled = rt_throttled && !run_queue.has_exempt_rt();
        let highest_rt = if rt_effectively_throttled {
            None
        } else {
            run_queue.highest_rt_priority_including_current()
        };
        self.priority.publish_run_queue(
            cpu,
            highest_rt,
            run_queue.earliest_deadline_including_current(),
            online,
        );
        self.overload.publish(
            cpu,
            online && !rt_effectively_throttled && run_queue.has_pushable_realtime(),
            online && run_queue.has_pushable_deadline(),
        );
    }

    pub(super) fn publish_offline(&self, cpu: CpuId) {
        self.priority.publish_offline(cpu);
        self.overload.publish(cpu, false, false);
    }

    pub(super) fn cpu_has_overload(&self, cpu: CpuId, class: SchedulingClass) -> bool {
        self.overload.contains(cpu, class)
    }

    pub(super) fn cpu_has_rt_deadline_overload(&self, cpu: CpuId) -> bool {
        self.overload.contains_any(cpu)
    }

    pub(super) fn has_idle_pull_source(&self) -> bool {
        self.overload.any_class(RootDomainPushClass::Deadline)
            || self.overload.any_class(RootDomainPushClass::Realtime)
    }

    /// Selects an overloaded rq for an idle target from the class-specific
    /// root-domain masks. Deadline is considered before fixed-priority RT,
    /// matching Linux's class order; Fair load balancing remains a separate
    /// sched-domain decision.
    pub(super) fn find_idle_pull_source(
        &self,
        target: CpuId,
        visited: &CpuSet,
    ) -> Option<(CpuId, SchedulingClass)> {
        for class in [RootDomainPushClass::Deadline, RootDomainPushClass::Realtime] {
            let source = self.overload.find_next_class(class, None, target, |cpu| {
                self.runqueues
                    .get(cpu.as_usize())
                    .is_some_and(|remote| !visited.contains(cpu) && remote.is_scheduler_ready())
            });
            if let Some(source) = source {
                return Some((source, class.scheduling_class()));
            }
        }
        None
    }

    fn push_iterator(&self, class: RootDomainPushClass) -> &RootDomainPushIterator {
        match class {
            RootDomainPushClass::Realtime => &self.realtime_push,
            RootDomainPushClass::Deadline => &self.deadline_push,
        }
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(super) fn push_requested_generation_for_test(&self, class: RootDomainPushClass) -> u64 {
        self.push_iterator(class).lock_state().requested_generation
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(super) fn push_target_pending_lock_acquisitions_for_test(
        &self,
        source: CpuId,
    ) -> (bool, usize) {
        ROOT_PUSH_STATE_LOCK_ACQUISITIONS.store(0, Ordering::Relaxed);
        let pending = self.push_target_pending(source);
        let acquisitions = ROOT_PUSH_STATE_LOCK_ACQUISITIONS.load(Ordering::Relaxed);
        (pending, acquisitions)
    }

    pub(super) fn request_rt_deadline_push(&self, class: RootDomainPushClass, requester: CpuId) {
        // Linux gates `pull_rt_task()`/`pull_dl_task()` on the root-domain
        // overload count before starting or extending the serialized push
        // iterator. A priority drop with no pushable source is not work and
        // must not contend on the iterator or create a phantom generation.
        if !self.overload.any_class(class) {
            return;
        }
        let push = self.push_iterator(class);
        let target = {
            let mut state = push.lock_state();
            state.requested_generation = state
                .requested_generation
                .checked_add(1)
                .expect("root-domain push generation exhausted");
            if state.phase != RootDomainPushPhase::Idle {
                None
            } else {
                state.scan_generation = state.requested_generation;
                state.cursor = None;
                self.publish_next_push_target(class, &mut state, requester)
            }
        };
        self.deliver_push_target(class, target);
    }

    pub(super) fn push_target_pending(&self, source: CpuId) -> bool {
        [RootDomainPushClass::Deadline, RootDomainPushClass::Realtime]
            .into_iter()
            .any(|class| self.push_iterator(class).has_published_target(source))
    }

    pub(super) fn claim_rt_deadline_push(&self, source: CpuId) -> Option<RootDomainPushClaim> {
        for class in [RootDomainPushClass::Deadline, RootDomainPushClass::Realtime] {
            let push = self.push_iterator(class);
            let mut state = push.lock_state();
            if state.phase != RootDomainPushPhase::Published(source) {
                continue;
            }
            state.phase = RootDomainPushPhase::Claimed(source);
            push.clear_published_target();
            return Some(RootDomainPushClaim {
                source,
                generation: state.scan_generation,
                class,
            });
        }
        None
    }

    pub(super) fn finish_rt_deadline_push(&self, claim: RootDomainPushClaim, made_progress: bool) {
        let target = {
            let push = self.push_iterator(claim.class);
            let mut state = push.lock_state();
            assert_eq!(
                state.phase,
                RootDomainPushPhase::Claimed(claim.source),
                "root-domain push completion must match the claimed owner"
            );
            assert_eq!(
                state.scan_generation, claim.generation,
                "root-domain push completion must match the claimed scan generation"
            );
            if made_progress
                && self.overload.contains_class(claim.source, claim.class)
                && self
                    .runqueues
                    .get(claim.source.as_usize())
                    .is_some_and(|remote| remote.is_online())
            {
                push.publish_target(&mut state, Some(claim.source));
                Some(claim.source)
            } else {
                state.cursor = Some(claim.source);
                self.advance_push_scan(claim.class, &mut state, claim.source)
            }
        };
        self.deliver_push_target(claim.class, target);
    }

    pub(super) fn find_lowest_rt_cpu(
        &self,
        priority: RtPriority,
        affinity: &CpuSet,
        preferred: Option<CpuId>,
        accepts: impl FnMut(CpuId) -> bool,
    ) -> Option<CpuId> {
        self.priority
            .find_lowest_rt_cpu(priority, affinity, preferred, accepts)
    }

    pub(super) fn find_later_deadline_cpu(
        &self,
        absolute_deadline_ns: u64,
        affinity: &CpuSet,
        preferred: Option<CpuId>,
        accepts: impl FnMut(CpuId) -> bool,
    ) -> Option<CpuId> {
        self.priority
            .find_later_deadline_cpu(absolute_deadline_ns, affinity, preferred, accepts)
    }

    fn rebuild_deadline_bandwidth(
        &self,
        state: &mut RootDomainState,
        rebuild: DeadlineBandwidthRebuild,
    ) {
        assert_eq!(
            state.online.count(),
            rebuild.online_cpus as usize,
            "Deadline rebuild topology must match the root-domain mask"
        );
        assert_eq!(
            state.deadline_admission.reserved_scaled(),
            rebuild.reserved_scaled,
            "Deadline rebuild must account every admitted reservation"
        );
        state
            .deadline_admission
            .set_online_cpus(rebuild.online_cpus);
        assert!(
            rebuild.distributed_scaled <= self.deadline_max_bw_scaled,
            "admission must reject root-domain Deadline overcommit before publication"
        );
        let extra = self.deadline_max_bw_scaled - rebuild.distributed_scaled;
        for remote in &self.runqueues {
            let published = if state.online.contains(remote.owner()) {
                extra
            } else {
                self.deadline_max_bw_scaled
            };
            remote.publish_deadline_extra_bw(published);
        }
    }

    fn replace_deadline_bandwidth(
        &self,
        state: &RootDomainState,
        old_utilization: u64,
        new_utilization: u64,
    ) {
        let online_cpus = u64::try_from(state.online.count())
            .expect("validated root-domain topology must fit CpuId");
        assert_ne!(
            online_cpus, 0,
            "Deadline admission requires an online root-domain CPU"
        );
        let old_per_cpu = old_utilization / online_cpus;
        let new_per_cpu = new_utilization / online_cpus;
        for remote in &self.runqueues {
            if state.online.contains(remote.owner()) {
                let extra = remote
                    .deadline_extra_bw_scaled()
                    .checked_add(old_per_cpu)
                    .expect("dl_rq extra bandwidth must fit its fixed-point ledger")
                    .checked_sub(new_per_cpu)
                    .expect("admission must not consume unavailable dl_rq extra bandwidth");
                assert!(
                    extra <= self.deadline_max_bw_scaled,
                    "Deadline replacement must match a previously published reservation"
                );
                remote.publish_deadline_extra_bw(extra);
            }
        }
    }

    fn advance_push_scan(
        &self,
        class: RootDomainPushClass,
        state: &mut RootDomainPushState,
        current: CpuId,
    ) -> Option<CpuId> {
        if let Some(target) = self.publish_next_push_target(class, state, current) {
            return Some(target);
        }
        if state.scan_generation != state.requested_generation {
            state.scan_generation = state.requested_generation;
            state.cursor = None;
            return self.publish_next_push_target(class, state, current);
        }
        self.push_iterator(class).publish_target(state, None);
        None
    }

    fn publish_next_push_target(
        &self,
        class: RootDomainPushClass,
        state: &mut RootDomainPushState,
        excluded: CpuId,
    ) -> Option<CpuId> {
        if !self.overload.any_class(class) {
            self.push_iterator(class).publish_target(state, None);
            return None;
        }
        let target = self
            .overload
            .find_next_class(class, state.cursor, excluded, |cpu| {
                self.runqueues[cpu.as_usize()].is_online()
            });
        self.push_iterator(class).publish_target(state, target);
        target
    }

    fn deliver_push_target(&self, class: RootDomainPushClass, mut target: Option<CpuId>) {
        while let Some(source) = target {
            let Some(remote) = self.runqueues.get(source.as_usize()) else {
                return;
            };
            if remote.kick_scheduler_work() {
                return;
            }
            target = {
                let mut state = self.push_iterator(class).lock_state();
                if state.phase != RootDomainPushPhase::Published(source) {
                    return;
                }
                state.cursor = Some(source);
                self.advance_push_scan(class, &mut state, source)
            };
        }
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
            SchedulingClass::Stop | SchedulingClass::Fair | SchedulingClass::Idle => false,
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

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    fn for_each_source(&self, excluded: CpuId, mut visit: impl FnMut(CpuId)) {
        self.deadline.for_each(|cpu| {
            if cpu != excluded {
                visit(cpu);
            }
        });
        self.realtime.for_each(|cpu| {
            if cpu != excluded && !self.deadline.contains(cpu) {
                visit(cpu);
            }
        });
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
        #[cfg(any(test, all(axtest, feature = "axtest")))]
        ROOT_PUSH_STATE_LOCK_ACQUISITIONS.fetch_add(1, Ordering::Relaxed);
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

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    fn for_each(&self, mut visit: impl FnMut(CpuId)) {
        if self.count.load(Ordering::Acquire) == 0 {
            return;
        }
        for (word_index, word) in self.words.iter().enumerate() {
            let mut members = word.load(Ordering::Acquire);
            while members != 0 {
                let bit = members.trailing_zeros() as usize;
                members &= members - 1;
                let index = word_index
                    .saturating_mul(usize::BITS as usize)
                    .saturating_add(bit);
                visit(CpuId::new(index as u32));
            }
        }
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
        true
    }

    pub(super) fn remove_online(&mut self, cpu: CpuId, rebuild: DeadlineBandwidthRebuild) -> bool {
        if !self.state.online.remove(cpu) {
            return false;
        }
        self.owner
            .rebuild_deadline_bandwidth(&mut self.state, rebuild);
        true
    }
}

impl Deref for RootDomainGuard<'_> {
    type Target = RootDomainState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}
