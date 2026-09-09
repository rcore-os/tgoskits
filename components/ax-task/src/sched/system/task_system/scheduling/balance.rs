//! Balance under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    /// Requests one owner-mediated pull from the busiest remote CPU.
    ///
    /// The target never locks or mutates the source runqueue. Its pinned request
    /// node is published to the source owner-control inbox and the source owner
    /// selects and hands off one affinity-compatible thread at a safe point.
    pub fn request_idle_pull(&self, mut cpu: Pin<&mut CpuLocal>) -> Result<bool, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if task_runtime::in_hard_irq() {
            return Ok(false);
        }
        self.ensure_owner_cpu_online(&cpu)?;
        if !self.root_domain.has_idle_pull_source() {
            cpu.as_mut().reset_idle_pull_scan();
            return Ok(false);
        }
        if !cpu.idle_pull_eligible() || cpu.has_remote_work() {
            cpu.as_mut().reset_idle_pull_scan();
            return Ok(false);
        }
        let target_remote = Arc::clone(cpu.remote());
        let reservation = match target_remote.begin_idle_pull() {
            IdlePullReservation::Started(reservation) => reservation,
            IdlePullReservation::AlreadyPending => return Ok(true),
            IdlePullReservation::Busy => return Ok(false),
        };
        if !cpu.idle_pull_eligible() || cpu.has_remote_work() {
            target_remote.cancel_idle_pull(reservation);
            cpu.as_mut().reset_idle_pull_scan();
            return Ok(false);
        }
        let target = cpu.owner();
        let source = self
            .root_domain
            .find_idle_pull_source(target, cpu.idle_pull_visited());
        let Some((source, class)) = source else {
            target_remote.cancel_idle_pull(reservation);
            cpu.as_mut().reset_idle_pull_scan();
            return Ok(false);
        };
        cpu.as_mut().mark_idle_pull_source(source);
        let Some(source_local) = self.cpu_remote(source) else {
            target_remote.cancel_idle_pull(reservation);
            cpu.request_scheduler_work();
            return Ok(true);
        };
        let message = InboxMessage::balance_request(source, target, reservation, class);
        let result = source_local.publish_owner_control(cpu.balance_request_node(), message);
        match result {
            PublishResult::Published => Ok(true),
            PublishResult::AlreadyPending => {
                target_remote.cancel_idle_pull(reservation);
                cpu.request_scheduler_work();
                Ok(true)
            }
            PublishResult::WrongKind => {
                target_remote.cancel_idle_pull(reservation);
                cpu.request_scheduler_work();
                Ok(true)
            }
        }
    }

    /// Lets the selected Linux-style ILB coordinate one Fair pull for every
    /// CPU still published in the root-domain idle mask.
    ///
    /// The coordinator only reserves target-owned request nodes and publishes
    /// them to source owners. Source and target runqueues remain private to
    /// their owners; a failed or stale request ends this NOHZ pass instead of
    /// kicking the target into an immediate retry loop.
    pub(in crate::sched::system::task_system) fn request_fair_nohz_idle_pulls(&self) -> bool {
        let mut requested = false;
        for (index, target_remote) in self.cpu_remotes.iter().enumerate() {
            let target = CpuId::new(index as u32);
            if !self.root_domain.fair_nohz_idle_target(target)
                || !target_remote.accepts_placement()
                || !target_remote.is_scheduler_ready()
            {
                continue;
            }
            requested |= self.request_fair_nohz_idle_pull(target, target_remote);
        }
        requested
    }

    pub(super) fn request_fair_nohz_idle_pull(
        &self,
        target: CpuId,
        target_remote: &CpuRemote,
    ) -> bool {
        let reservation = match target_remote.begin_idle_pull() {
            IdlePullReservation::Started(reservation) => reservation,
            IdlePullReservation::AlreadyPending => return true,
            IdlePullReservation::Busy => return false,
        };
        if !self.root_domain.fair_nohz_idle_target(target)
            || !target_remote.accepts_placement()
            || !target_remote.is_scheduler_ready()
        {
            target_remote.cancel_idle_pull(reservation);
            return false;
        }
        let Some(source) = self
            .root_domain
            .find_unvisited_fair_idle_pull_source(target)
        else {
            target_remote.cancel_idle_pull(reservation);
            return false;
        };
        let Some(source_remote) = self.cpu_remote(source) else {
            target_remote.cancel_idle_pull(reservation);
            return false;
        };
        let message =
            InboxMessage::balance_request(source, target, reservation, SchedulingClass::Fair);
        match source_remote.publish_owner_control(target_remote.balance_request_node(), message) {
            PublishResult::Published => true,
            PublishResult::AlreadyPending | PublishResult::WrongKind => {
                target_remote.cancel_idle_pull(reservation);
                false
            }
        }
    }

    /// Pushes one queued thread from an overloaded owner to the least loaded CPU.
    ///
    /// Selection and dequeue happen only on `cpu`; the target receives an
    /// intrusive handoff and enqueues it in its own safe-point drain.
    pub fn push_rt_deadline(&self, cpu: Pin<&mut CpuLocal>) -> Result<Option<ThreadId>, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if task_runtime::in_hard_irq() {
            return Ok(None);
        }
        self.ensure_owner_cpu_online(&cpu)?;
        self.push_rt_deadline_from_root_domain(cpu, None)
    }

    /// Pushes from the coherent owner snapshot published by the immediately
    /// preceding runqueue transaction.
    ///
    /// Scheduler selection publishes after installing its next dispatch, so
    /// its common tail can reuse that snapshot just as Linux keeps balancing
    /// decisions under one owner-rq transaction. Callers must not mutate the
    /// local runqueue or current dispatch between publication and this call.
    pub(in crate::sched::system::task_system) fn push_rt_deadline_from_root_domain(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        class: Option<SchedulingClass>,
    ) -> Result<Option<ThreadId>, TaskError> {
        if !class.map_or_else(
            || self.root_domain.cpu_has_rt_deadline_overload(cpu.owner()),
            |class| self.root_domain.cpu_has_overload(cpu.owner(), class),
        ) {
            return Ok(None);
        }
        let Some(selection) =
            self.select_rt_deadline_balance_transfer(cpu.as_ref().get_ref(), class)
        else {
            return Ok(None);
        };
        let target = selection.target();
        let outcome = self.commit_owner_balance_transfer(cpu.as_mut(), selection)?;
        if outcome == BalanceTransferOutcome::Retry
            && let Some(target_remote) = self.cpu_remote(target)
        {
            // Ask the idle destination to issue a fresh owner-mediated pull.
            // This keeps retry asynchronous instead of spinning the source
            // scheduler tail on a transient affinity/publication race.
            target_remote.kick_scheduler_work();
        }
        Ok(outcome.migrated())
    }
}
