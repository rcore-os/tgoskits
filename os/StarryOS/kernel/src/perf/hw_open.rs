//! Validation and resource construction for ARM PMUv3 `perf_event_open`.

use alloc::sync::Arc;
use core::sync::atomic::AtomicBool;

use ax_errno::{AxError, AxResult};
use axpoll::PollSet;
use kbpf_basic::linux_bpf::{perf_event_attr, perf_hw_id, perf_type_id};

use super::{
    access::AuthorizedPerfTarget,
    cpu_worker,
    hw::{ARMV8_PMUV3_PERF_TYPE, ValidatedHwCounter, ValidatedHwOpen},
    hw_allocation::{
        alloc_preferred_cycle, alloc_programmable, free_counter, set_programmable_counter_count,
    },
    hw_event::{HwPerfEvent, SystemEventInit, TaskEventInit},
    hw_owner::SystemPmuConfigure,
    hw_sampling::{SamplingState, resolve_sampling, start_sampling_notify_worker},
    inheritance::PerfInheritanceFamily,
    output::PerfOutputRoute,
    sampling,
    target::{PerfCpuId, PerfTargetKind},
};
use crate::task::future::IrqNotify;

/// Required instruction-pointer bit in a hardware sampling event.
/// A sampling event with any other `sample_type` is rejected at open.
const PERF_SAMPLE_IP: u64 = 1;

/// Performs the side-effect-free part of ARM PMUv3 event construction.
pub(super) fn validate_perf_event_open_hw(
    attr: &perf_event_attr,
    target_kind: PerfTargetKind,
) -> AxResult<ValidatedHwOpen> {
    let Some(info) = ax_hal::pmu::info() else {
        return Err(AxError::Unsupported);
    };

    // SAFETY: both union arms are `u64` in the copied `repr(C)` attribute.
    let raw = unsafe { attr.__bindgen_anon_1.sample_period };
    let is_freq = attr.freq() != 0;
    let is_sampling = raw > 0;
    let kind = match target_kind {
        PerfTargetKind::Task => "per-task sampling",
        PerfTargetKind::Cpu => "sampling",
    };
    validate_sampling(attr, raw, is_freq, kind)?;
    let (sample_period, target_freq) = resolve_sampling(raw, is_freq);

    let event = if attr.type_ == perf_type_id::PERF_TYPE_HARDWARE as u32 {
        ax_cpu::pmu::hw_event_to_arm(attr.config as u32).ok_or(AxError::Unsupported)?
    } else if attr.type_ == perf_type_id::PERF_TYPE_RAW as u32
        || attr.type_ == ARMV8_PMUV3_PERF_TYPE
    {
        (attr.config & 0xFFFF) as u16
    } else {
        return Err(AxError::Unsupported);
    };

    let cycle_event = ax_cpu::pmu::hw_event_to_arm(perf_hw_id::PERF_COUNT_HW_CPU_CYCLES as u32)
        .ok_or(AxError::Unsupported)?;
    let prefer_cycle = !is_sampling && event == cycle_event;
    let counter = match (target_kind, prefer_cycle) {
        (PerfTargetKind::Cpu, true) => ValidatedHwCounter::SystemPreferredCycle(event),
        (PerfTargetKind::Cpu, false) => ValidatedHwCounter::SystemProgrammable(event),
        (PerfTargetKind::Task, true) => ValidatedHwCounter::TaskPreferredCycle(event),
        (PerfTargetKind::Task, false) if ax_cpu::pmu::event_supported(event) => {
            ValidatedHwCounter::TaskProgrammable(event)
        }
        (PerfTargetKind::Task, false) => return Err(AxError::Unsupported),
    };

    Ok(ValidatedHwOpen {
        num_counters: info.num_counters,
        counter,
        is_sampling,
        is_freq,
        sample_period,
        target_freq,
    })
}

/// Opens a hardware-PMU perf event from a user `perf_event_attr`.
///
/// Supports `PERF_TYPE_HARDWARE` (cycles via the dedicated counter, every
/// other mapped `perf_hw_id` via a programmable counter) and `PERF_TYPE_RAW`
/// (the low 16 bits of `config` as the raw ARM event number on a programmable
/// counter).
pub(super) fn perf_event_open_hw(
    attr: &perf_event_attr,
    target: AuthorizedPerfTarget,
    validated: ValidatedHwOpen,
) -> AxResult<HwPerfEvent> {
    set_programmable_counter_count(validated.num_counters);

    let owner_cpu = match target {
        AuthorizedPerfTarget::Task { task, cpu } => {
            return perf_event_open_hw_per_task(attr, task, cpu, validated);
        }
        AuthorizedPerfTarget::Cpu(cpu) => cpu,
    };
    let exclude_user = attr.exclude_user() != 0;
    let exclude_kernel = attr.exclude_kernel() != 0;

    if validated.is_sampling {
        sampling::ensure_pmu_irq_registered().map_err(|_| AxError::NoSuchDevice)?;
    }

    let (counter, event) = match validated.counter {
        ValidatedHwCounter::SystemPreferredCycle(event) => {
            let counter = alloc_preferred_cycle(event)?;
            let programmed_event = counter.programmable_index().map(|_| event);
            (counter, programmed_event)
        }
        ValidatedHwCounter::SystemProgrammable(event) => (alloc_programmable(event)?, Some(event)),
        ValidatedHwCounter::TaskPreferredCycle(_) | ValidatedHwCounter::TaskProgrammable(_) => {
            return Err(AxError::BadState);
        }
    };
    if let Err(error) = cpu_worker::configure_system(
        owner_cpu,
        SystemPmuConfigure {
            counter,
            event,
            exclude_user,
            exclude_kernel,
        },
    ) {
        free_counter(counter);
        return Err(error);
    }

    let sampling = validated.is_sampling.then(|| {
        let poll_ready = Arc::new(PollSet::new());
        let notify = Arc::new(IrqNotify::new());
        let poll_alive = Arc::new(AtomicBool::new(true));
        start_sampling_notify_worker(
            Arc::clone(&poll_ready),
            Arc::clone(&notify),
            Arc::clone(&poll_alive),
        );
        SamplingState {
            period: validated.sample_period,
            freq: validated.is_freq,
            target_freq: validated.target_freq,
            sample_type: attr.sample_type,
            poll_ready,
            notify,
            poll_alive,
            output: PerfOutputRoute::new(),
        }
    });

    Ok(HwPerfEvent::new_system(SystemEventInit {
        counter,
        owner: owner_cpu,
        read_format: attr.read_format,
        sampling,
        enable_at_open: attr.disabled() == 0,
    }))
}

/// Opens a task-bound hardware-PMU event (`perf_event_open` with `pid >= 0`).
fn perf_event_open_hw_per_task(
    attr: &perf_event_attr,
    task: crate::task::UserTaskRef,
    cpu_filter: Option<PerfCpuId>,
    validated: ValidatedHwOpen,
) -> AxResult<HwPerfEvent> {
    let thread = task.as_thread();
    let scheduler_id = thread.scheduler_id().ok_or(AxError::BadState)?;

    let exclude_user = attr.exclude_user() != 0;
    let exclude_kernel = attr.exclude_kernel() != 0;

    if validated.is_sampling {
        sampling::ensure_pmu_irq_registered().map_err(|_| AxError::NoSuchDevice)?;
    }

    let (counter, event) = match validated.counter {
        ValidatedHwCounter::TaskPreferredCycle(event) => (alloc_preferred_cycle(event)?, event),
        ValidatedHwCounter::TaskProgrammable(event) => (alloc_programmable(event)?, event),
        ValidatedHwCounter::SystemPreferredCycle(_) | ValidatedHwCounter::SystemProgrammable(_) => {
            return Err(AxError::BadState);
        }
    };

    let enabled = attr.disabled() == 0;
    let per_task_counter = Arc::new(super::task::PerTaskCounter::new(
        super::task::PerTaskConfig {
            scheduler_id,
            counter,
            event,
            exclude_user,
            exclude_kernel,
            read_format: attr.read_format,
            enabled,
            enable_on_exec: attr.enable_on_exec() != 0,
            cpu_filter,
            sample_period: validated.sample_period,
            sample_type: attr.sample_type,
            freq: validated.is_freq,
            target_freq: validated.target_freq,
            want_comm: attr.comm() != 0,
            want_mmap2: attr.mmap2() != 0,
            want_task: attr.task() != 0,
            sample_id_all: attr.sample_id_all() != 0,
            inherit: attr.inherit() != 0,
        },
    ));
    let family = PerfInheritanceFamily::new(Arc::clone(&per_task_counter), enabled);
    super::task::attach(thread, per_task_counter);
    if let Err(error) = family.root().synchronize_context() {
        let root = family.root();
        // The scheduler publication must remain reachable until its exact PMU
        // generation is quiescent. Withdrawing the list entry first would leave
        // a failed owner-CPU fence with no future sched-out owner.
        if let Err(release_error) = super::task::free_hw(&root) {
            warn!(
                "perf_event_open: failed to quiesce task event after context-sync error \
                 ({error}); retaining its ownership graph: {release_error}"
            );
            core::mem::forget(family);
            return Err(release_error);
        }
        super::task::detach_unpublished(thread, &root);
        return Err(error);
    }

    Ok(HwPerfEvent::new_task(TaskEventInit {
        counter,
        scheduler_id: scheduler_id.as_u64(),
        read_format: attr.read_format,
        family,
    }))
}

fn validate_sampling(attr: &perf_event_attr, raw: u64, is_freq: bool, kind: &str) -> AxResult<()> {
    if raw == 0 {
        return Ok(());
    }
    if attr.sample_type & PERF_SAMPLE_IP == 0
        || attr.sample_type & !sampling::SUPPORTED_SAMPLE_TYPE != 0
    {
        warn!(
            "perf_event_open: {kind} sample_type {:#x} unsupported (need PERF_SAMPLE_IP and only \
             scalar fields)",
            attr.sample_type
        );
        return Err(AxError::Unsupported);
    }
    if !is_freq && raw > u32::MAX as u64 {
        warn!("perf_event_open: {kind} period {raw} exceeds 32-bit counter");
        return Err(AxError::InvalidInput);
    }
    Ok(())
}
