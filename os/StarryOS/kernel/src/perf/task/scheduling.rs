use super::*;

/// Scheduler hook: the given thread is about to start running on this CPU.
///
/// Programs every enabled, not-yet-running, live per-task counter onto HW and
/// starts it. `configure` resets the counter to 0, so the slice delta will equal
/// `counter::read(n)` at the matching [`perf_sched_out`].
///
/// For a *sampling* counter (`is_sampling`) whose ring is mapped, it instead arms
/// the M2 overflow-IRQ path for this slice: `configure`, `preload` to overflow
/// after `sample_period` events, register a [`SampleSlot`] pointing at the ptc's
/// ring + notify, `enable_irq`, then `enable`. So overflows fire `PERF_RECORD_SAMPLE`
/// into the task's ring only while the task runs. (If the ring is not mapped yet,
/// the slice is skipped — `perf` always mmaps before enable, so this is a rare race.)
///
/// Runs with IRQs disabled inside `switch_to` and uses only
/// [`SpinNoIrq`](ax_sync::spin::SpinNoIrq), atomics, and sysreg writes; it does
/// not allocate. `sampling::register` nests a further local-IRQ-off section.
pub fn perf_sched_in(thr: &Thread) {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    thr.perf_context().with_counters(perf_sched_in_counters);
}

fn perf_sched_in_counters(counters: &[Arc<PerTaskCounter>]) {
    if counters.is_empty() {
        return;
    }
    let now = now_ns();
    let current_cpu = PerfCpuId::new(ax_hal::percpu::this_cpu_id());
    for ptc in counters.iter() {
        if !ptc.enabled.load(Ordering::Acquire) {
            continue;
        }
        if ptc.cpu_filter.is_some_and(|cpu| cpu != current_cpu) {
            continue;
        }
        let sample_output = if ptc.is_sampling {
            let Some(output) = ptc.sample_output() else {
                continue;
            };
            Some(output)
        } else {
            None
        };
        let mut run_state = ptc.run_state.lock();
        let Some(ticket) = run_state.begin_arm(current_cpu) else {
            continue;
        };
        if let Some(output) = sample_output {
            let n = ptc.programmable_index();
            if let Err(error) = sampling::enable_local_pmu_irq() {
                run_state.cancel_arm(ticket);
                warn!(
                    "perf: failed to enable the PMU IRQ on CPU {}: {error:?}",
                    current_cpu.as_usize()
                );
                continue;
            }
            // configure() programs event + EL filter AND resets the counter to 0.
            ptc.counter
                .configure(ptc.programmed_event(), ptc.exclude_user, ptc.exclude_kernel)
                .expect("validated task PMU counter/event pairing");
            // Overflow after `sample_period` events.
            ax_cpu::pmu::counter::preload(n, ptc.sample_period);
            let registration = match sampling::register(
                n,
                SampleSlot::new(
                    output,
                    SampleSlotConfig {
                        period: ptc.sample_period,
                        sample_type: ptc.sample_type,
                        id: ptc.sample_id.load(Ordering::Relaxed),
                        // Frequency mode adapts the period within each slice; the
                        // slot starts at the initial estimate with no timestamp.
                        freq: ptc.freq,
                        target_freq: ptc.freq_target,
                        last_time: 0,
                    },
                ),
            ) {
                Ok(registration) => registration,
                Err(error) => {
                    run_state.cancel_arm(ticket);
                    warn!(
                        "perf: failed to register counter {} on CPU {}: {error:?}",
                        n,
                        current_cpu.as_usize()
                    );
                    continue;
                }
            };
            run_state.publish_registration(ticket, registration);
            // Arm the per-counter overflow interrupt, then start counting.
            ax_cpu::pmu::overflow::enable_irq(n);
            ax_cpu::pmu::counter::enable(n);
        } else {
            // Counting: configure() programs event + EL filter AND resets to 0.
            ptc.counter
                .configure(ptc.programmed_event(), ptc.exclude_user, ptc.exclude_kernel)
                .expect("validated task PMU counter/event pairing");
            ptc.counter.enable();
        }
        ptc.last_in_ns.store(now, Ordering::Release);
        run_state.finish_arm(ticket);
        // Publish while the generation transition is still serialized by
        // `run_state`. Otherwise a concurrent disable can publish inactive and
        // then be overwritten by this delayed active publication.
        ptc.publish_rdpmc_active();
        drop(run_state);
    }
}

/// Scheduler hook: the given thread is about to stop running on this CPU.
///
/// For a counting counter, reads the current slice delta, folds it into the
/// accumulator, stops the counter, and accrues the slice's wall time.
///
/// For a *sampling* counter, disarms the M2 overflow-IRQ path for this slice:
/// stop the counter (it can no longer overflow), `disable_irq`, then `unregister`
/// the [`SampleSlot`]. After this, an overflow on counter `n` while some *other*
/// task runs cannot fire a sample into this task's ring — that is what attributes
/// samples to the task. (Sampling events carry no read-back value, so no delta is
/// accumulated; only wall time is accrued.)
///
/// Same hot-path constraints as [`perf_sched_in`].
pub fn perf_sched_out(thr: &Thread) {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    thr.perf_context().with_counters(perf_sched_out_counters);
}

fn perf_sched_out_counters(counters: &[Arc<PerTaskCounter>]) {
    if counters.is_empty() {
        return;
    }
    for ptc in counters.iter() {
        let Some(lease) = ptc.run_state.lock().claim_schedule_out() else {
            continue;
        };
        stop_hardware_on_owner(ptc, lease)
            .unwrap_or_else(|error| panic!("scheduler PMU stop failed: {error}"));
        ptc.run_state.lock().finish_owner_stop(lease);
    }
}

/// Stops one exact PMU generation on its owner CPU.
///
/// The sampling order is mask → stop → clear pending overflow → generation
/// unregister. Local IRQ exclusion in the registry removal is the grace period
/// before its owned ring/notification references can be released.
fn stop_hardware_on_owner(ptc: &PerTaskCounter, lease: PmuRunLease) -> AxResult<()> {
    if lease.owner().as_usize() != ax_hal::percpu::this_cpu_id() {
        return Err(AxError::BadState);
    }
    if let Some(registration) = lease.registration() {
        let n = ptc.programmable_index();
        if registration.counter() != n {
            return Err(AxError::BadState);
        }
        ax_cpu::pmu::overflow::disable_irq(n);
        ax_cpu::pmu::counter::disable(n);
        ax_cpu::pmu::overflow::clear(1 << n);
        sampling::unregister(registration).map_err(|_| AxError::BadState)?;
    } else {
        // Freeze the physical slice before sampling its terminal value. Reading
        // first would lose the events retired between the read and disable.
        ptc.counter.disable();
        let delta = ptc.counter.read();
        ptc.accumulated.fetch_add(delta, Ordering::AcqRel);
    }

    let dt = now_ns().saturating_sub(ptc.last_in_ns.load(Ordering::Acquire));
    ptc.time_enabled_ns.fetch_add(dt, Ordering::AcqRel);
    ptc.time_running_ns.fetch_add(dt, Ordering::AcqRel);
    ptc.publish_rdpmc_inactive();
    Ok(())
}

/// Completes one disable/close request on the CPU that owns `lease`.
///
/// The scheduler switch-out path may have won the same generation before the
/// affine worker gets CPU time. Generation state makes that case a successful
/// fence instead of a duplicate hardware unregister.
pub(crate) fn stop_requested_on_owner(ptc: &PerTaskCounter, lease: PmuRunLease) -> AxResult<()> {
    // The run-state guard must end before the hardware transaction and before
    // the completion path takes it again. A lock expression used directly as a
    // `match` scrutinee lives through the whole match and self-deadlocks in the
    // `Claimed` arm.
    let claim = ptc.run_state.lock().claim_requested_stop(lease);
    match claim {
        PmuStopClaim::Claimed(claimed) => {
            if let Err(error) = stop_hardware_on_owner(ptc, claimed) {
                ptc.run_state.lock().abort_owner_stop(claimed);
                return Err(error);
            }
            ptc.run_state.lock().finish_owner_stop(claimed);
            Ok(())
        }
        PmuStopClaim::AlreadyComplete => Ok(()),
        PmuStopClaim::InProgress => Err(AxError::ResourceBusy),
        PmuStopClaim::Stale => Err(AxError::BadState),
    }
}
