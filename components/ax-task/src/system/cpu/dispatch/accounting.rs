//! `rq->clock_task` dispatch accounting and GRUB charge calculation.

use alloc::sync::Arc;

use super::{CurrentClassState, CurrentDispatch};
use crate::{SchedulerTimestamp, SchedulingEntity};

struct DispatchChargeState<'a> {
    accumulated_deadline_overrun: &'a mut bool,
    accounted_until_ns: &'a mut u64,
    charged_runtime_ns: &'a mut u64,
    runtime_core: &'a crate::thread::ThreadCore,
}

impl CurrentDispatch {
    pub(crate) fn charge(
        &mut self,
        runtime_ns: u64,
        now_ns: u64,
        reclaimed_ns: u64,
    ) -> DispatchCharge {
        let runtime_core = Arc::clone(&self.task.runtime_core);
        let Some(CurrentClassState::Owned(active)) = &mut self.class.schedule else {
            crate::runtime::task_runtime::fatal_invariant(
                0x4355_0001,
                self.thread().as_u64() as usize,
            )
        };
        Self::charge_entity(
            active.entity_mut(),
            DispatchChargeState {
                accumulated_deadline_overrun: &mut self.class.deadline_overrun,
                accounted_until_ns: &mut self.accounting.accounted_until_ns,
                charged_runtime_ns: &mut self.accounting.charged_runtime_ns,
                runtime_core: &runtime_core,
            },
            runtime_ns,
            now_ns,
            reclaimed_ns,
        )
    }

    pub(crate) fn charge_linked(
        &mut self,
        entity: &mut SchedulingEntity,
        runtime_ns: u64,
        now_ns: u64,
        reclaimed_ns: u64,
    ) -> DispatchCharge {
        let runtime_core = Arc::clone(&self.task.runtime_core);
        let Some(CurrentClassState::Linked { policy: _ }) = self.class.schedule else {
            crate::runtime::task_runtime::fatal_invariant(
                0x4355_0002,
                self.thread().as_u64() as usize,
            )
        };
        Self::charge_entity(
            entity,
            DispatchChargeState {
                accumulated_deadline_overrun: &mut self.class.deadline_overrun,
                accounted_until_ns: &mut self.accounting.accounted_until_ns,
                charged_runtime_ns: &mut self.accounting.charged_runtime_ns,
                runtime_core: &runtime_core,
            },
            runtime_ns,
            now_ns,
            reclaimed_ns,
        )
    }

    fn charge_entity(
        entity: &mut SchedulingEntity,
        state: DispatchChargeState<'_>,
        runtime_ns: u64,
        now_ns: u64,
        reclaimed_ns: u64,
    ) -> DispatchCharge {
        *state.charged_runtime_ns = state.charged_runtime_ns.saturating_add(runtime_ns);
        *state.accounted_until_ns = now_ns;
        state.runtime_core.charge_runtime(runtime_ns, now_ns);
        let (slice_expired, deadline_overrun, deadline_replenished) = {
            let mut slice_expired = entity.charge(runtime_ns, 0, reclaimed_ns);
            let deadline_overrun = slice_expired
                && entity
                    .deadline_owner_flags()
                    .contains(crate::DeadlineFlags::DL_OVERRUN);
            let deadline_replenished = if let SchedulingEntity::Deadline(deadline) = entity {
                if slice_expired && deadline.is_pi_boosted() {
                    // Linux PREEMPT_RT immediately re-enqueues a boosted DL
                    // entity with ENQUEUE_REPLENISH instead of waiting for its
                    // ordinary CBS timer.
                    deadline.replenish_for_pi(now_ns);
                    slice_expired = false;
                    true
                } else {
                    false
                }
            } else {
                false
            };
            (slice_expired, deadline_overrun, deadline_replenished)
        };
        *state.accumulated_deadline_overrun |= deadline_overrun;
        DispatchCharge {
            slice_expired,
            deadline_overrun,
            deadline_replenished,
        }
    }

    /// Advances the owner clock for the per-CPU idle dispatch.
    pub(crate) fn account_dedicated_idle_until(&mut self, now_ns: u64) {
        self.accounting.accounted_until_ns = now_ns;
    }

    pub(crate) fn finish_runtime_accounting(&self, now_ns: u64) {
        self.runtime_core().finish_runtime_accounting(now_ns);
    }

    pub(crate) fn take_charged_runtime_ns(&mut self) -> u64 {
        core::mem::take(&mut self.accounting.charged_runtime_ns)
    }

    pub(crate) fn take_deadline_overrun(&mut self) -> bool {
        core::mem::take(&mut self.class.deadline_overrun)
    }

    pub(crate) fn unaccounted_runtime(&self, now_ns: u64) -> u64 {
        dispatch_runtime_delta(now_ns, self.accounting.accounted_until_ns)
    }

    pub(crate) fn grub_reclaimed_ns(
        &self,
        entity: &SchedulingEntity,
        runtime_ns: u64,
        inactive_bw_scaled: u64,
        extra_bw_scaled: u64,
        max_bw_scaled: u64,
    ) -> u64 {
        if !entity
            .deadline_owner_flags()
            .contains(crate::DeadlineFlags::RECLAIM)
            || self.metadata().deadline_bandwidth_scaled == 0
            || max_bw_scaled == 0
        {
            return 0;
        }
        let own_bw_scaled = self.metadata().deadline_bandwidth_scaled;
        if own_bw_scaled > max_bw_scaled {
            crate::runtime::task_runtime::fatal_invariant(
                0x444c_1011,
                self.thread().as_u64() as usize,
            );
        }
        let charged_ns = grub_charge_ns(
            runtime_ns,
            own_bw_scaled,
            inactive_bw_scaled,
            extra_bw_scaled,
            max_bw_scaled,
        );
        runtime_ns - charged_ns
    }
}

fn dispatch_runtime_delta(now_ns: u64, accounted_until_ns: u64) -> u64 {
    SchedulerTimestamp::from_nanos(now_ns).since(SchedulerTimestamp::from_nanos(accounted_until_ns))
}

fn grub_charge_ns(
    runtime_ns: u64,
    own_bw_scaled: u64,
    inactive_bw_scaled: u64,
    extra_bw_scaled: u64,
    max_bw_scaled: u64,
) -> u64 {
    assert!(max_bw_scaled > 0);
    assert!(own_bw_scaled <= max_bw_scaled);

    // Linux compares Uinact + Uextra against Umax - u instead of
    // subtracting first: the reclaimable sum may legitimately exceed Umax.
    let reclaimable_bw_scaled = inactive_bw_scaled as u128 + extra_bw_scaled as u128;
    let charge_rate_scaled = if reclaimable_bw_scaled > (max_bw_scaled - own_bw_scaled) as u128 {
        own_bw_scaled
    } else {
        max_bw_scaled - reclaimable_bw_scaled as u64
    };
    let charged_ns = runtime_ns as u128 * charge_rate_scaled as u128 / max_bw_scaled as u128;
    u64::try_from(charged_ns).expect("GRUB charge cannot exceed the supplied runtime")
}

/// Result of one allocation-free local dispatch charge.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct DispatchCharge {
    pub(crate) slice_expired: bool,
    pub(crate) deadline_overrun: bool,
    pub(crate) deadline_replenished: bool,
}

#[cfg(any(test, all(axtest, feature = "axtest")))]
mod tests {
    use super::{dispatch_runtime_delta, grub_charge_ns};

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn dispatch_runtime_survives_scheduler_clock_wrap() {
        assert_eq!(dispatch_runtime_delta(2, u64::MAX - 2), 5);
    }

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn grub_charge_uses_linux_fixed_point_truncation() {
        assert_eq!(grub_charge_ns(1, 1, 1, 0, 2), 0);
    }
}
