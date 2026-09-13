//! Handoff under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    /// Completes selection either by releasing rq locally or by installing the
    /// Linux-style raw rq lock baton into a real non-migrating switch handoff.
    pub(in crate::sched::system::task_system) fn commit_owner_switch_selection(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        transaction: OwnerRqTxn<'_>,
        mut handoff: Option<SwitchHandoff>,
        retain_rq_lock: bool,
    ) {
        if cpu.as_ref().get_ref().switch_handoff().is_some() {
            task_runtime::fatal_invariant(0x5343_1117, cpu.owner().as_u32() as usize);
        }
        let retain_rq_lock = retain_rq_lock && handoff.is_some();
        if handoff
            .as_ref()
            .is_some_and(SwitchHandoff::previous_requires_rq_baton)
            && !retain_rq_lock
        {
            task_runtime::fatal_invariant(0x5343_111c, cpu.owner().as_u32() as usize);
        }
        if retain_rq_lock {
            let baton = transaction.commit_and_handoff_scheduler_work();
            handoff
                .as_mut()
                .expect("rq baton requires a prepared switch handoff")
                .install_rq_baton(baton)
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x5343_111a, cpu.owner().as_u32() as usize)
                });
        } else {
            transaction.commit_and_finish_scheduler_request();
        }
        if let Some(handoff) = handoff {
            cpu.as_mut()
                .install_switch_handoff(handoff)
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x5343_1117, cpu.owner().as_u32() as usize)
                });
        }
    }

    pub(in crate::sched::system::task_system) fn prepare_switch_handoff(
        previous: Option<ThreadId>,
        previous_core: Option<PreviousSwitchOwnership>,
        next: SchedulerThreadRef,
        next_policy: SchedulerPolicyRef,
        previous_disposition: PreviousSwitchDisposition,
        migration: Option<PreparedMigrationDelivery>,
    ) -> Option<SwitchHandoff> {
        match previous {
            Some(previous) if previous != next.as_ref().id() => {
                let previous_core = previous_core.unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_1115, previous.as_u64() as usize)
                });
                if previous_core.as_ref().id() != previous {
                    task_runtime::fatal_invariant(0x5343_1116, previous.as_u64() as usize);
                }
                Some(SwitchHandoff::prepared(
                    previous_core,
                    next,
                    next_policy,
                    previous_disposition,
                    migration,
                ))
            }
            _ if migration.is_none() => None,
            _ => task_runtime::fatal_invariant(0x5343_1118, next.as_ref().id().as_u64() as usize),
        }
    }

    pub(in crate::sched::system::task_system) fn owner_switch_plan(
        previous_endpoint: Option<SwitchEndpoint>,
        next_endpoint: SwitchEndpoint,
        switch_reason: SwitchReason,
        timestamp_ns: u64,
    ) -> ScheduleDecision {
        let runtime_switch_plan = previous_endpoint
            .filter(|previous| previous.thread() != next_endpoint.thread())
            .map(|previous| {
                crate::runtime::switch::RuntimeSwitchPlan::new(
                    previous.binding().context(),
                    previous.binding().address_space(),
                    previous.address_space_identity(),
                    next_endpoint.binding().context(),
                    next_endpoint.binding().address_space(),
                    next_endpoint.address_space_identity(),
                )
                .unwrap_or_else(|| {
                    task_runtime::fatal_invariant(
                        0x5343_1119,
                        next_endpoint.thread().as_u64() as usize,
                    )
                })
            });
        ScheduleDecision {
            previous: previous_endpoint.map(SwitchEndpoint::thread),
            next: next_endpoint.thread(),
            runtime_switch_plan,
            switch_reason,
            timestamp_ns,
        }
    }
}
