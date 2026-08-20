//! Lock-free scheduling-policy publication and immutable core accessors.

use super::*;

impl ThreadCore {
    pub(crate) fn publish_base_policy(&self, policy: SchedulePolicy) {
        self.base_policy.store(policy);
    }

    pub(crate) fn publish_effective_schedule(
        &self,
        policy: SchedulePolicy,
        entity: &crate::SchedulingEntity,
    ) {
        self.effective_key_sequence.fetch_add(1, Ordering::AcqRel);
        self.effective_policy.store(policy);
        let absolute_deadline_ns = entity
            .deadline()
            .and_then(crate::DeadlineEntity::absolute_deadline_ns);
        self.effective_deadline_active
            .store(absolute_deadline_ns.is_some(), Ordering::Relaxed);
        self.effective_deadline_ns
            .store(absolute_deadline_ns.unwrap_or(0), Ordering::Relaxed);
        self.effective_key_sequence.fetch_add(1, Ordering::Release);
    }

    pub(crate) fn effective_placement_demand(&self) -> u64 {
        // Demand depends on the policy atom alone, unlike the scheduling key
        // which also needs a coherent Deadline timestamp. Avoid waiting on a
        // preempted sequence writer from migration publication context.
        self.effective_policy.load().placement_demand()
    }

    pub(crate) fn effective_policy_snapshot(&self) -> SchedulePolicy {
        self.effective_policy.load()
    }

    fn effective_scheduling_key(&self) -> SchedulingKey {
        loop {
            let sequence = self.effective_key_sequence.load(Ordering::Acquire);
            if sequence & 1 != 0 {
                core::hint::spin_loop();
                continue;
            }
            let policy = self.effective_policy.load();
            let deadline_active = self.effective_deadline_active.load(Ordering::Relaxed);
            let absolute_deadline_ns = self.effective_deadline_ns.load(Ordering::Relaxed);
            if self.effective_key_sequence.load(Ordering::Acquire) != sequence {
                continue;
            }
            return match policy {
                SchedulePolicy::Deadline(_) if deadline_active => {
                    SchedulingKey::new(policy.class_rank(), absolute_deadline_ns, self.id.as_u64())
                }
                SchedulePolicy::Deadline(_) => {
                    panic!("an inactive Deadline entity has no effective scheduler key")
                }
                _ => policy.scheduling_key(self.id.as_u64()),
            };
        }
    }

    pub(crate) fn effective_scheduling_urgency(&self) -> SchedulingUrgency {
        let key = self.effective_scheduling_key();
        SchedulingUrgency::new(key.class_rank(), key.primary())
    }

    /// Returns the rtmutex waiter ordering key.
    ///
    /// Linux maps every non-RT/non-Deadline task to `DEFAULT_PRIO` in
    /// `__waiter_prio()`. Nice and idle weight affect the fair rq, but never
    /// lock handoff order or PI donation.
    pub(crate) fn effective_pi_wait_urgency(&self) -> SchedulingUrgency {
        let urgency = self.effective_scheduling_urgency();
        if urgency.class_rank() >= 3 {
            SchedulingUrgency::new(3, 0)
        } else {
            urgency
        }
    }

    pub(crate) fn set_wake_cpu_hint(&self, cpu: CpuId) {
        self.wake_cpu_hint.store(cpu.as_u32(), Ordering::Release);
    }

    pub(crate) fn base_policy(&self) -> SchedulePolicy {
        self.base_policy.load()
    }

    pub(crate) fn wake_cpu_hint(&self) -> Option<CpuId> {
        let cpu = self.wake_cpu_hint.load(Ordering::Acquire);
        (cpu != u32::MAX).then(|| CpuId::new(cpu))
    }

    pub(super) fn assigned_cpu(&self) -> Option<CpuId> {
        self.sched.assigned_cpu()
    }

    pub(crate) const fn id(&self) -> ThreadId {
        self.id
    }

    pub(crate) const fn extension_view(&self) -> Option<ThreadExtensionView> {
        self.extension
    }

    pub(crate) fn sched(&self) -> &Arc<ThreadSchedCell> {
        &self.sched
    }

    pub(crate) const fn pi_wait_state(&self) -> &PiWaitState {
        &self.pi_wait_state
    }

    pub(crate) const fn affinity_update_node(&self) -> &InboxNode {
        &self.affinity_update_node
    }

    pub(crate) const fn deadline_cbs_timer(&self) -> &TaskDeadlineNode {
        &self.deadline_cbs_timer
    }

    pub(crate) const fn deadline_zero_lag_timer(&self) -> &TaskDeadlineNode {
        &self.deadline_zero_lag_timer
    }

    pub(crate) const fn deadline_refresh_node(&self) -> &InboxNode {
        &self.deadline_refresh_node
    }

    pub(crate) const fn migration_node(&self) -> &InboxNode {
        &self.migration_node
    }

    pub(crate) const fn scheduler_tick_work_node(&self) -> &InboxNode {
        &self.scheduler_tick_work_node
    }

    pub(crate) const fn deadline_callback_node(&self) -> &InboxNode {
        &self.deadline_callback_node
    }
}

#[derive(Debug)]
pub(super) struct AtomicPolicy {
    sequence: AtomicUsize,
    kind: AtomicU8,
    first: AtomicU64,
    second: AtomicU64,
    third: AtomicU64,
    flags: AtomicU32,
}

impl AtomicPolicy {
    pub(super) fn new(policy: SchedulePolicy) -> Self {
        let (kind, first, second, third, flags) = encode_policy(policy);
        Self {
            sequence: AtomicUsize::new(0),
            kind: AtomicU8::new(kind),
            first: AtomicU64::new(first),
            second: AtomicU64::new(second),
            third: AtomicU64::new(third),
            flags: AtomicU32::new(flags),
        }
    }

    pub(super) fn load(&self) -> SchedulePolicy {
        loop {
            let start = self.sequence.load(Ordering::Acquire);
            if start & 1 != 0 {
                core::hint::spin_loop();
                continue;
            }
            let encoded = (
                self.kind.load(Ordering::Relaxed),
                self.first.load(Ordering::Relaxed),
                self.second.load(Ordering::Relaxed),
                self.third.load(Ordering::Relaxed),
                self.flags.load(Ordering::Relaxed),
            );
            if self.sequence.load(Ordering::Acquire) == start {
                return decode_policy(encoded);
            }
        }
    }

    fn store(&self, policy: SchedulePolicy) {
        let (kind, first, second, third, flags) = encode_policy(policy);
        self.sequence.fetch_add(1, Ordering::AcqRel);
        self.kind.store(kind, Ordering::Relaxed);
        self.first.store(first, Ordering::Relaxed);
        self.second.store(second, Ordering::Relaxed);
        self.third.store(third, Ordering::Relaxed);
        self.flags.store(flags, Ordering::Relaxed);
        self.sequence.fetch_add(1, Ordering::Release);
    }
}

fn encode_policy(policy: SchedulePolicy) -> (u8, u64, u64, u64, u32) {
    match policy {
        SchedulePolicy::KernelStop => (6, 0, 0, 0, 0),
        SchedulePolicy::Fair { nice, mode } => {
            let kind = match mode {
                FairMode::Normal => 0,
                FairMode::Batch => 1,
                FairMode::Idle => 2,
            };
            (kind, nice.get() as i64 as u64, 0, 0, 0)
        }
        SchedulePolicy::Fifo { priority } => (3, priority.get() as u64, 0, 0, 0),
        SchedulePolicy::RoundRobin {
            priority,
            quantum_ns,
        } => (4, priority.get() as u64, quantum_ns, 0, 0),
        SchedulePolicy::Deadline(policy) => (
            5,
            policy.runtime_ns(),
            policy.deadline_ns(),
            policy.period_ns(),
            policy.flags().bits(),
        ),
    }
}

fn decode_policy(encoded: (u8, u64, u64, u64, u32)) -> SchedulePolicy {
    let (kind, first, second, third, flags) = encoded;
    match kind {
        0..=2 => {
            let mode = match kind {
                0 => FairMode::Normal,
                1 => FairMode::Batch,
                _ => FairMode::Idle,
            };
            SchedulePolicy::fair(Nice::new(first as i64 as i8).unwrap_or(Nice::ZERO), mode)
        }
        3 => SchedulePolicy::fifo(
            RtPriority::new(first as u8)
                .unwrap_or_else(|_| RtPriority::new(1).expect("constant RT priority is valid")),
        ),
        4 => SchedulePolicy::round_robin_with_quantum(
            RtPriority::new(first as u8)
                .unwrap_or_else(|_| RtPriority::new(1).expect("constant RT priority is valid")),
            second,
        )
        .unwrap_or_default(),
        5 => {
            let flags = DeadlineFlags::from_bits(flags).unwrap_or(DeadlineFlags::NONE);
            DeadlinePolicy::new(first, second, third, flags)
                .map(SchedulePolicy::deadline)
                .unwrap_or_default()
        }
        6 => SchedulePolicy::kernel_stop(),
        _ => SchedulePolicy::default(),
    }
}
