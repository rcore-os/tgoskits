//! Accounting under the owning scheduler transaction.

use super::*;

impl<'a> OwnerRqTxn<'a> {
    #[inline(always)]
    pub(crate) fn charge_current(&mut self, runtime_ns: u64, reclaimed_ns: u64) -> DispatchCharge {
        let current_policy = self
            .current()
            .unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5251_1001, self.remote.owner().as_u32() as usize)
            })
            .schedule_policy();
        self.charge_current_for_policy(runtime_ns, reclaimed_ns, current_policy)
    }

    #[inline(always)]
    pub(super) fn charge_current_for_policy(
        &mut self,
        runtime_ns: u64,
        reclaimed_ns: u64,
        current_policy: SchedulePolicy,
    ) -> DispatchCharge {
        if matches!(
            current_policy,
            SchedulePolicy::Fifo { .. } | SchedulePolicy::RoundRobin { .. }
        ) {
            let now_ns = self.clock.task().as_nanos();
            let (charge, rt_quota_exempt) = self
                .scheduler_queue_mut()
                .charge_fixed_realtime_current(now_ns);
            return self.apply_current_update(
                runtime_ns,
                RqCurrentUpdate::Task {
                    charge,
                    reschedule: None,
                    realtime: true,
                    rt_quota_exempt,
                },
            );
        }
        let deadline_extra_bw_scaled = if matches!(current_policy, SchedulePolicy::Deadline(_)) {
            self.remote.deadline_extra_bw_scaled()
        } else {
            0
        };
        let update = self
            .run_queue_mut()
            .update_current(runtime_ns, reclaimed_ns, deadline_extra_bw_scaled)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_1001, self.remote.owner().as_u32() as usize)
            });
        self.apply_current_update(runtime_ns, update)
    }

    pub(crate) fn task_tick_current(
        &mut self,
        runtime_ns: u64,
        reclaimed_ns: u64,
        tick_ns: u64,
    ) -> DispatchCharge {
        let deadline_extra_bw_scaled = self.remote.deadline_extra_bw_scaled();
        let update = self
            .run_queue_mut()
            .task_tick_current(runtime_ns, reclaimed_ns, deadline_extra_bw_scaled, tick_ns)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_1001, self.remote.owner().as_u32() as usize)
            });
        self.apply_current_update(runtime_ns, update)
    }

    pub(crate) fn clock_event_current(
        &mut self,
        runtime_ns: u64,
        reclaimed_ns: u64,
    ) -> DispatchCharge {
        let deadline_extra_bw_scaled = self.remote.deadline_extra_bw_scaled();
        let update = self
            .run_queue_mut()
            .clock_event_current(runtime_ns, reclaimed_ns, deadline_extra_bw_scaled)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_1001, self.remote.owner().as_u32() as usize)
            });
        self.apply_current_update(runtime_ns, update)
    }

    pub(crate) fn task_tick_and_clock_event_current(
        &mut self,
        runtime_ns: u64,
        reclaimed_ns: u64,
        tick_ns: u64,
    ) -> DispatchCharge {
        let deadline_extra_bw_scaled = self.remote.deadline_extra_bw_scaled();
        let update = self
            .run_queue_mut()
            .task_tick_and_clock_event_current(
                runtime_ns,
                reclaimed_ns,
                deadline_extra_bw_scaled,
                tick_ns,
            )
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_1001, self.remote.owner().as_u32() as usize)
            });
        self.apply_current_update(runtime_ns, update)
    }

    #[inline(always)]
    pub(super) fn apply_current_update(
        &mut self,
        runtime_ns: u64,
        update: RqCurrentUpdate,
    ) -> DispatchCharge {
        match update {
            RqCurrentUpdate::DedicatedIdle => DispatchCharge::default(),
            RqCurrentUpdate::Task {
                charge,
                reschedule,
                realtime,
                rt_quota_exempt,
            } => {
                self.current()
                    .unwrap_or_else(|| {
                        task_runtime::fatal_invariant(
                            0x5251_1001,
                            self.remote.owner().as_u32() as usize,
                        )
                    })
                    .runtime_core()
                    .commit_runtime_interval(runtime_ns);
                let already_throttled = self.run_queue().rt_is_throttled();
                let rt_throttled = realtime
                    && self.system.rt_bandwidth_enabled()
                    && self.system.charge_rt_runtime(
                        self.remote.owner(),
                        runtime_ns,
                        already_throttled,
                    );
                if rt_throttled {
                    self.run_queue_mut().set_rt_throttled(true);
                }
                self.remote.charge_busy_runtime(runtime_ns);
                if rt_throttled && !rt_quota_exempt {
                    self.remote.request_reschedule(RescheduleKind::Immediate);
                } else if let Some(kind) = reschedule {
                    self.remote.request_reschedule(kind);
                }
                charge
            }
        }
    }

    pub(crate) fn rt_is_effectively_throttled(&self) -> bool {
        self.run_queue().rt_is_throttled() && !self.run_queue().has_exempt_rt()
    }

    pub(crate) fn rt_is_throttled(&self) -> bool {
        self.run_queue().rt_is_throttled()
    }

    pub(crate) fn set_rt_throttled(&mut self, throttled: bool) -> bool {
        self.run_queue_mut().set_rt_throttled(throttled)
    }

    #[inline(always)]
    pub(crate) fn settle_current(&mut self, reclaimed_ns: u64) -> DispatchCharge {
        let now_ns = self.clock.task().as_nanos();
        let (runtime_ns, current_policy) = {
            let current = self.current().unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5251_1002, self.remote.owner().as_u32() as usize)
            });
            (
                current.unaccounted_runtime(now_ns),
                current.schedule_policy(),
            )
        };
        self.charge_current_for_policy(runtime_ns, reclaimed_ns, current_policy)
    }

    /// Linux `update_curr_rt()` for a current already proved to be FIFO/RR.
    #[inline(always)]
    pub(crate) fn settle_fixed_realtime_current(&mut self) {
        let now_ns = self.clock.task().as_nanos();
        let runtime_ns = self
            .current()
            .unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5251_1002, self.remote.owner().as_u32() as usize)
            })
            .unaccounted_runtime(now_ns);
        let (charge, rt_quota_exempt) = self
            .scheduler_queue_mut()
            .charge_fixed_realtime_current(now_ns);
        debug_assert_eq!(charge, DispatchCharge::default());
        let _ = self.apply_current_update(
            runtime_ns,
            RqCurrentUpdate::Task {
                charge,
                reschedule: None,
                realtime: true,
                rt_quota_exempt,
            },
        );
    }

    pub(crate) fn task_tick_current_until(
        &mut self,
        reclaimed_ns: u64,
        tick_ns: u64,
    ) -> DispatchCharge {
        let now_ns = self.clock.task().as_nanos();
        let runtime_ns = self
            .current()
            .unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5251_1002, self.remote.owner().as_u32() as usize)
            })
            .unaccounted_runtime(now_ns);
        self.task_tick_current(runtime_ns, reclaimed_ns, tick_ns)
    }

    pub(crate) fn clock_event_current_until(&mut self, reclaimed_ns: u64) -> DispatchCharge {
        let now_ns = self.clock.task().as_nanos();
        let runtime_ns = self
            .current()
            .unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5251_1002, self.remote.owner().as_u32() as usize)
            })
            .unaccounted_runtime(now_ns);
        self.clock_event_current(runtime_ns, reclaimed_ns)
    }

    pub(crate) fn task_tick_and_clock_event_current_until(
        &mut self,
        reclaimed_ns: u64,
        tick_ns: u64,
    ) -> DispatchCharge {
        let now_ns = self.clock.task().as_nanos();
        let runtime_ns = self
            .current()
            .unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5251_1002, self.remote.owner().as_u32() as usize)
            })
            .unaccounted_runtime(now_ns);
        self.task_tick_and_clock_event_current(runtime_ns, reclaimed_ns, tick_ns)
    }
}
