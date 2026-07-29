//! Validation and resource construction for ARM PMUv3 `perf_event_open`.

use alloc::sync::Arc;
use core::sync::atomic::AtomicBool;

use ax_errno::{AxError, AxResult};
use axpoll::PollSet;
use kbpf_basic::linux_bpf::{perf_event_attr, perf_hw_id, perf_type_id};

use super::{
    cpu_worker,
    hw::ARMV8_PMUV3_PERF_TYPE,
    hw_allocation::{
        alloc_cycle_counter, alloc_programmable, alloc_programmable_counter, free_counter,
        set_programmable_counter_count,
    },
    hw_event::{HwPerfEvent, SystemEventInit, TaskEventInit},
    hw_owner::SystemPmuConfigure,
    hw_sampling::{SamplingState, resolve_sampling, start_sampling_notify_worker},
    inheritance::PerfInheritanceFamily,
    output::PerfOutputRoute,
    sampling,
    target::{PerfCpuId, PerfTarget, PerfTaskTarget},
};
use crate::task::future::IrqNotify;

/// Required instruction-pointer bit in a hardware sampling event.
/// A sampling event with any other `sample_type` is rejected at open.
const PERF_SAMPLE_IP: u64 = 1;

/// Opens a hardware-PMU perf event from a user `perf_event_attr`.
///
/// Supports `PERF_TYPE_HARDWARE` (cycles via the dedicated counter, every
/// other mapped `perf_hw_id` via a programmable counter) and `PERF_TYPE_RAW`
/// (the low 16 bits of `config` as the raw ARM event number on a programmable
/// counter).
pub(super) fn perf_event_open_hw(
    attr: &perf_event_attr,
    target: PerfTarget,
) -> AxResult<HwPerfEvent> {
    let Some(info) = ax_hal::pmu::info() else {
        return Err(AxError::Unsupported);
    };

    set_programmable_counter_count(info.num_counters);

    let owner_cpu = match target {
        PerfTarget::Task { task, cpu } => {
            return perf_event_open_hw_per_task(attr, task, cpu);
        }
        PerfTarget::Cpu(cpu) => cpu,
    };
    let exclude_user = attr.exclude_user() != 0;
    let exclude_kernel = attr.exclude_kernel() != 0;

    // SAFETY: both union arms are `u64` in the copied `repr(C)` attribute.
    let raw = unsafe { attr.__bindgen_anon_1.sample_period };
    let is_freq = attr.freq() != 0;
    let is_sampling = raw > 0;
    validate_sampling(attr, raw, is_freq, "sampling")?;
    let (sample_period, target_freq) = resolve_sampling(raw, is_freq);
    if is_sampling {
        sampling::ensure_pmu_irq_registered().map_err(|_| AxError::NoSuchDevice)?;
    }

    let (counter, event) = if attr.type_ == perf_type_id::PERF_TYPE_HARDWARE as u32 {
        if attr.config == perf_hw_id::PERF_COUNT_HW_CPU_CYCLES as u64 && !is_sampling {
            let Some(counter) = alloc_cycle_counter() else {
                return Err(AxError::NoMemory);
            };
            (counter, None)
        } else {
            let Some(event) = ax_cpu::pmu::hw_event_to_arm(attr.config as u32) else {
                warn!(
                    "perf_event_open: unsupported hardware config {:#x}",
                    attr.config
                );
                return Err(AxError::Unsupported);
            };
            (alloc_programmable(event)?, Some(event))
        }
    } else if attr.type_ == perf_type_id::PERF_TYPE_RAW as u32
        || attr.type_ == ARMV8_PMUV3_PERF_TYPE
    {
        let event = (attr.config & 0xFFFF) as u16;
        (alloc_programmable(event)?, Some(event))
    } else {
        warn!(
            "perf_event_open: unsupported hardware type {:#x}",
            attr.type_
        );
        return Err(AxError::Unsupported);
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

    let sampling = is_sampling.then(|| {
        let poll_ready = Arc::new(PollSet::new());
        let notify = Arc::new(IrqNotify::new());
        let poll_alive = Arc::new(AtomicBool::new(true));
        start_sampling_notify_worker(
            Arc::clone(&poll_ready),
            Arc::clone(&notify),
            Arc::clone(&poll_alive),
        );
        SamplingState {
            period: sample_period,
            freq: is_freq,
            target_freq,
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
    target: PerfTaskTarget,
    cpu_filter: Option<PerfCpuId>,
) -> AxResult<HwPerfEvent> {
    let task = match target {
        PerfTaskTarget::Current => crate::task::current_user_task(),
        PerfTaskTarget::Tid(tid) => crate::task::get_task(tid)?,
    };
    let thread = task.as_thread();
    let scheduler_id = thread.scheduler_id().ok_or(AxError::BadState)?;

    let exclude_user = attr.exclude_user() != 0;
    let exclude_kernel = attr.exclude_kernel() != 0;

    // SAFETY: both union arms are `u64` in the copied `repr(C)` attribute.
    let raw = unsafe { attr.__bindgen_anon_1.sample_period };
    let is_freq = attr.freq() != 0;
    let is_sampling = raw > 0;
    validate_sampling(attr, raw, is_freq, "per-task sampling")?;
    let (sample_period, target_freq) = resolve_sampling(raw, is_freq);
    if is_sampling {
        sampling::ensure_pmu_irq_registered().map_err(|_| AxError::NoSuchDevice)?;
    }

    let event = if attr.type_ == perf_type_id::PERF_TYPE_HARDWARE as u32 {
        match ax_cpu::pmu::hw_event_to_arm(attr.config as u32) {
            Some(event) => event,
            None => {
                warn!(
                    "perf_event_open: unsupported per-task hardware config {:#x}",
                    attr.config
                );
                return Err(AxError::Unsupported);
            }
        }
    } else if attr.type_ == perf_type_id::PERF_TYPE_RAW as u32
        || attr.type_ == ARMV8_PMUV3_PERF_TYPE
    {
        (attr.config & 0xFFFF) as u16
    } else {
        warn!(
            "perf_event_open: unsupported per-task hardware type {:#x}",
            attr.type_
        );
        return Err(AxError::Unsupported);
    };

    if !ax_cpu::pmu::event_supported(event) {
        warn!(
            "perf_event_open: per-task ARM event {:#x} not implemented on this CPU",
            event
        );
        return Err(AxError::Unsupported);
    }

    let Some(counter) = alloc_programmable_counter() else {
        return Err(AxError::NoMemory);
    };

    let enabled = attr.disabled() == 0;
    let per_task_counter = Arc::new(super::task::PerTaskCounter::new(
        super::task::PerTaskConfig {
            n: counter,
            event,
            exclude_user,
            exclude_kernel,
            read_format: attr.read_format,
            enabled,
            enable_on_exec: attr.enable_on_exec() != 0,
            cpu_filter,
            sample_period,
            sample_type: attr.sample_type,
            freq: is_freq,
            target_freq,
            want_comm: attr.comm() != 0,
            want_mmap2: attr.mmap2() != 0,
            want_task: attr.task() != 0,
            sample_id_all: attr.sample_id_all() != 0,
            inherit: attr.inherit() != 0,
        },
    ));
    let family = PerfInheritanceFamily::new(Arc::clone(&per_task_counter), enabled);
    super::task::attach(thread, per_task_counter);

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
