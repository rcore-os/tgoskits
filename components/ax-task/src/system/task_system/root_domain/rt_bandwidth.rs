//! Root-domain RT period callback and online-rq replenishment.

use super::*;

impl RootDomain {
    pub(in crate::system::task_system) fn rt_bandwidth_enabled(&self) -> bool {
        self.rt_bandwidth.enabled()
    }

    pub(in crate::system::task_system) fn activate_rt_period(
        &self,
        cpu: CpuId,
        sample_now: impl FnOnce() -> MonotonicInstant,
    ) -> bool {
        #[cfg(feature = "task-test-hooks")]
        if !self.rt_bandwidth.enabled() {
            crate::task_test_hooks::record_disabled_rt_bandwidth_activation_entry(cpu);
        }
        self.rt_bandwidth.activate(cpu, sample_now)
    }

    /// Runs the Linux `do_sched_rt_period_timer()` transaction.
    pub(super) fn service_rt_period(
        &self,
        system: &TaskSystem,
        cpu: CpuId,
        now: MonotonicInstant,
    ) -> bool {
        let Some(firing) = self.rt_bandwidth.begin_period(cpu, now) else {
            return false;
        };
        let overruns = firing.overruns();
        let mut keep_active = false;
        let mut rescheduled = false;

        for remote in &self.runqueues {
            if !remote.is_online() {
                continue;
            }
            let snapshot = *remote.lock_rt_bandwidth();
            let run_queue = remote.lock_run_queue(RunQueueGuardSource::RtAccounting);
            let runnable = run_queue.has_runnable_rt();
            let throttled = run_queue.rt_is_throttled();
            drop(run_queue);
            // The ledger snapshot is optimistic and precedes the rq read. A
            // concurrent owner may publish throttling between them, so the rq
            // fact must veto the empty-ledger fast path.
            if snapshot.time_ns() == 0 && !runnable && !throttled {
                continue;
            }
            let mut transaction = OwnerRqTxn::begin(system, remote);
            let charged_runtime_active = remote.lock_rt_bandwidth().time_ns() != 0;
            if !charged_runtime_active {
                let runtime_active = transaction.rt_is_throttled();
                let runnable = transaction.has_runnable_rt();
                keep_active |= runtime_active || runnable;
                transaction.commit();
                continue;
            }
            if transaction.rt_is_throttled() {
                // Linux balances a charged, throttled rq before subtracting
                // elapsed periods. A zero rt_time never enters balance_runtime
                // and therefore cannot borrow quota or clear throttling here.
                self.balance_rt_runtime(remote.owner());
            }
            let mut bandwidth = remote.lock_rt_bandwidth();
            let below_runtime = bandwidth.replenish(overruns);
            let charged_runtime_active = bandwidth.time_ns() != 0;
            drop(bandwidth);
            let unthrottled = transaction.rt_is_throttled() && below_runtime;
            if unthrottled {
                transaction.set_rt_throttled(false);
            }
            let runtime_active = charged_runtime_active || transaction.rt_is_throttled();
            let runnable = transaction.has_runnable_rt();
            keep_active |= runtime_active || runnable;
            transaction.commit();
            if !unthrottled || !runnable {
                continue;
            }
            rescheduled = true;
            if remote.owner() == cpu {
                remote.request_reschedule(RescheduleKind::Immediate);
            } else {
                remote.request_remote_reschedule(RescheduleKind::Immediate);
            }
        }

        self.rt_bandwidth.finish_period(firing, keep_active);
        rescheduled
    }

    /// Linux `sched_rt_runtime_exceeded()` plus `do_balance_runtime()`.
    /// The rq caller owns local execution accounting; the root lock is entered
    /// only on the quota edge and serializes transfers among independent
    /// per-rq runtime locks.
    pub(super) fn charge_rt_runtime(
        &self,
        cpu: CpuId,
        runtime_ns: u64,
        already_throttled: bool,
    ) -> bool {
        #[cfg(feature = "task-test-hooks")]
        if !self.rt_bandwidth.enabled() {
            crate::task_test_hooks::record_disabled_rt_bandwidth_charge_entry(cpu);
        }
        if !self.rt_bandwidth.enabled() {
            return false;
        }
        let remote = &self.runqueues[cpu.as_usize()];
        if !remote.lock_rt_bandwidth().account(runtime_ns) {
            return false;
        }
        // Linux `sched_rt_runtime_exceeded()` keeps accounting PI-boosted
        // execution on a throttled rq, but returns before `balance_runtime()`.
        // A sticky throttle therefore cannot repeatedly borrow runtime or scan
        // the root-domain span on every later charge.
        if already_throttled {
            return true;
        }
        self.balance_rt_runtime(cpu);
        remote.lock_rt_bandwidth().should_throttle()
    }

    fn balance_rt_runtime(&self, receiver: CpuId) {
        let period_ns = self.rt_bandwidth.period_ns();
        let _root = self.rt_bandwidth.lock_runtime();
        let span_weight = self
            .runqueues
            .iter()
            .filter(|remote| remote.is_online())
            .count();
        if span_weight == 0 {
            return;
        }
        for donor in &self.runqueues {
            if donor.owner() == receiver || !donor.is_online() {
                continue;
            }
            let receiver_runtime = self.runqueues[receiver.as_usize()]
                .lock_rt_bandwidth()
                .runtime_ns();
            let room = period_ns.saturating_sub(receiver_runtime);
            if room == 0 {
                break;
            }
            let amount = {
                let mut runtime = donor.lock_rt_bandwidth();
                if !runtime.enabled() {
                    continue;
                }
                let amount = (runtime.spare_runtime_ns() / span_weight as u64).min(room);
                if amount != 0 {
                    runtime.lend_runtime(amount);
                }
                amount
            };
            if amount != 0 {
                self.runqueues[receiver.as_usize()]
                    .lock_rt_bandwidth()
                    .borrow_runtime(amount, period_ns);
            }
            if self.runqueues[receiver.as_usize()]
                .lock_rt_bandwidth()
                .runtime_ns()
                == period_ns
            {
                break;
            }
        }
    }

    pub(in crate::system::task_system) fn enable_rt_runtime(&self, cpu: CpuId) {
        let remote = &self.runqueues[cpu.as_usize()];
        let mut run_queue = remote.lock_run_queue(RunQueueGuardSource::RtAccounting);
        remote.lock_rt_bandwidth().enable(
            self.rt_bandwidth.period_ns(),
            self.rt_bandwidth.runtime_ns(),
        );
        run_queue.set_rt_throttled(false);
    }

    pub(in crate::system::task_system) fn disable_rt_runtime(&self, cpu: CpuId) {
        let root_guard = self.rt_bandwidth.lock_runtime();
        let base = self.rt_bandwidth.runtime_ns();
        let current = self.runqueues[cpu.as_usize()]
            .lock_rt_bandwidth()
            .runtime_ns();
        let mut want = i128::from(base) - i128::from(current);
        for remote in &self.runqueues {
            if remote.owner() == cpu || !remote.is_online() || want == 0 {
                continue;
            }
            let mut runtime = remote.lock_rt_bandwidth();
            if !runtime.enabled() {
                continue;
            }
            if want > 0 {
                let reclaim = u64::try_from(want)
                    .expect("positive RT reclaim must fit u64")
                    .min(runtime.runtime_ns());
                runtime.adjust_runtime(-i128::from(reclaim));
                want -= i128::from(reclaim);
            } else {
                let returned = u64::try_from(-want).expect("negative RT reclaim must fit u64");
                runtime.adjust_runtime(i128::from(returned));
                want = 0;
            }
        }
        assert_eq!(want, 0, "root-domain RT runtime loan leaked across hotplug");
        self.runqueues[cpu.as_usize()].lock_rt_bandwidth().disable();
        drop(root_guard);
        self.runqueues[cpu.as_usize()]
            .lock_run_queue(RunQueueGuardSource::RtAccounting)
            .set_rt_throttled(false);
    }
}

impl TaskSystem {
    pub(crate) fn rt_bandwidth_enabled(&self) -> bool {
        self.root_domain.rt_bandwidth_enabled()
    }

    pub(crate) fn service_rt_period(&self, cpu: &CpuLocal, now: MonotonicInstant) -> bool {
        self.root_domain.service_rt_period(self, cpu.owner(), now)
    }

    pub(crate) fn charge_rt_runtime(
        &self,
        cpu: CpuId,
        runtime_ns: u64,
        already_throttled: bool,
    ) -> bool {
        self.root_domain
            .charge_rt_runtime(cpu, runtime_ns, already_throttled)
    }

    #[cfg(feature = "task-test-hooks")]
    pub(crate) fn already_throttled_rt_charge_preserves_runtime_loans() -> bool {
        const PERIOD_NS: u64 = 1_000;
        const RUNTIME_NS: u64 = 500;

        let config = TaskSystemConfig::new(2).with_rt_bandwidth(PERIOD_NS, RUNTIME_NS);
        let runqueues = (0..config.cpu_count())
            .map(|index| CpuRemote::create(CpuId::new(index as u32), config))
            .collect::<Vec<_>>();
        let root_domain = RootDomain::new(config, runqueues.clone());
        for remote in &runqueues {
            root_domain.enable_rt_runtime(remote.owner());
            assert!(
                remote.mark_online(),
                "test runqueue must become online once"
            );
        }

        let receiver = CpuId::new(0);
        let donor = CpuId::new(1);
        runqueues[receiver.as_usize()]
            .lock_run_queue(RunQueueGuardSource::RtAccounting)
            .set_rt_throttled(true);
        let receiver_runtime = runqueues[receiver.as_usize()]
            .lock_rt_bandwidth()
            .runtime_ns();
        let donor_runtime = runqueues[donor.as_usize()].lock_rt_bandwidth().runtime_ns();

        let throttled = root_domain.charge_rt_runtime(receiver, RUNTIME_NS + 1, true);
        let receiver_bandwidth = *runqueues[receiver.as_usize()].lock_rt_bandwidth();
        let donor_bandwidth = *runqueues[donor.as_usize()].lock_rt_bandwidth();

        throttled
            && receiver_bandwidth.time_ns() == RUNTIME_NS + 1
            && receiver_bandwidth.runtime_ns() == receiver_runtime
            && donor_bandwidth.runtime_ns() == donor_runtime
    }

    #[cfg(feature = "task-test-hooks")]
    pub(crate) fn zero_rt_time_period_preserves_throttle_and_runtime_loans() -> bool {
        const PERIOD_NS: u64 = 1_000;
        const RUNTIME_NS: u64 = 500;

        let config = TaskSystemConfig::new(2).with_rt_bandwidth(PERIOD_NS, RUNTIME_NS);
        let system = TaskSystem::new(config).expect("test task system must be valid");
        let receiver = CpuId::new(0);
        let donor = CpuId::new(1);
        let receiver_cpu = system
            .create_cpu_local(receiver)
            .expect("test receiver CPU-local scheduler must be created");
        for remote in &system.cpu_remotes {
            system.root_domain.enable_rt_runtime(remote.owner());
            assert!(
                remote.mark_online(),
                "test runqueue must become online once"
            );
        }

        system.cpu_remotes[receiver.as_usize()]
            .lock_run_queue(RunQueueGuardSource::RtAccounting)
            .set_rt_throttled(true);
        let receiver_runtime = system.cpu_remotes[receiver.as_usize()]
            .lock_rt_bandwidth()
            .runtime_ns();
        let donor_runtime = system.cpu_remotes[donor.as_usize()]
            .lock_rt_bandwidth()
            .runtime_ns();
        let origin =
            MonotonicInstant::from_nanos(0).expect("the monotonic origin must be representable");
        assert!(system.root_domain.activate_rt_period(receiver, || origin));

        let rescheduled = system.service_rt_period(receiver_cpu.as_ref().get_ref(), origin);
        let receiver_throttled = system.cpu_remotes[receiver.as_usize()]
            .lock_run_queue(RunQueueGuardSource::RtAccounting)
            .rt_is_throttled();
        let receiver_bandwidth = *system.cpu_remotes[receiver.as_usize()].lock_rt_bandwidth();
        let donor_bandwidth = *system.cpu_remotes[donor.as_usize()].lock_rt_bandwidth();

        !rescheduled
            && receiver_throttled
            && receiver_bandwidth.time_ns() == 0
            && receiver_bandwidth.runtime_ns() == receiver_runtime
            && donor_bandwidth.runtime_ns() == donor_runtime
    }
}
