//! Validation and resource construction for ARM PMUv3 `perf_event_open`.

use alloc::sync::Arc;
use core::sync::atomic::AtomicBool;

use axpoll_set::PollSet;
use kbpf_basic::linux_bpf::{perf_event_attr, perf_hw_id, perf_type_id};

use super::{
    access::AuthorizedPerfTarget,
    cpu_worker,
    hw::{
        ARMV8_CORTEX_A55_PERF_TYPE, ARMV8_CORTEX_A76_PERF_TYPE, ARMV8_PMUV3_PERF_TYPE,
        ValidatedHwCounter, ValidatedHwOpen,
    },
    hw_allocation::{
        alloc_cycle_counter, alloc_system, alloc_system_cycle, free_counter, free_system,
    },
    hw_event::{HwPerfEvent, SystemEventInit, TaskEventInit},
    hw_owner::SystemPmuConfigure,
    hw_sampling::{
        SamplingReadState, SamplingState, resolve_sampling, start_sampling_notify_worker,
    },
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
    cpu_constraint: Option<PerfCpuId>,
) -> crate::StarryResult<ValidatedHwOpen> {
    let required_cluster = match attr.type_ {
        ARMV8_CORTEX_A55_PERF_TYPE => Some(crate::perf::event_map::ClusterId::CortexA55),
        ARMV8_CORTEX_A76_PERF_TYPE => Some(crate::perf::event_map::ClusterId::CortexA76),
        _ => None,
    };
    let target_cpu = cpu_constraint.map(PerfCpuId::as_usize);
    let num_counters = super::percpu::counter_count_for_target(target_cpu, required_cluster)
        .ok_or(crate::StarryError::NotFound)?;

    // SAFETY: both union arms are `u64` in the copied `repr(C)` attribute.
    let raw = unsafe { attr.__bindgen_anon_1.sample_period };
    let is_freq = attr.freq() != 0;
    let is_sampling = raw > 0;
    let kind = match target_kind {
        PerfTargetKind::Task => "per-task sampling",
        PerfTargetKind::Cpu => "sampling",
    };
    validate_sampling(attr, raw, is_freq, kind)?;
    if attr.inherit() != 0
        && attr.sample_type & sampling::PERF_SAMPLE_READ != 0
        && attr.sample_type & sampling::PERF_SAMPLE_TID == 0
    {
        return Err(crate::StarryError::InvalidInput);
    }
    let (sample_period, target_freq) = resolve_sampling(raw, is_freq);

    let is_generic_hw = attr.type_ == perf_type_id::PERF_TYPE_HARDWARE as u32;
    let is_hw_cache = attr.type_ == perf_type_id::PERF_TYPE_HW_CACHE as u32;
    let is_named_pmu = matches!(
        attr.type_,
        ARMV8_PMUV3_PERF_TYPE | ARMV8_CORTEX_A55_PERF_TYPE | ARMV8_CORTEX_A76_PERF_TYPE
    );
    let event = if is_generic_hw {
        super::percpu::generic_event_for_target(target_cpu, required_cluster, attr.config as u32)
            .ok_or(crate::StarryError::NotFound)?
    } else if is_hw_cache {
        crate::perf::event_map::hw_cache_to_arm(attr.config).map_err(|error| match error {
            crate::perf::event_map::CacheEventError::Invalid => crate::StarryError::InvalidInput,
            crate::perf::event_map::CacheEventError::Unsupported => crate::StarryError::NotFound,
        })?
    } else if attr.type_ == perf_type_id::PERF_TYPE_RAW as u32 || is_named_pmu {
        (attr.config & 0xFFFF) as u16
    } else {
        return Err(crate::StarryError::Unsupported);
    };

    // RAW must satisfy the same capability contract as Pmu::configure before
    // publication. Implementation-defined encodings remain accepted; only
    // explicitly absent common events are rejected, including on task targets.
    if !super::percpu::event_supported_for_target(target_cpu, required_cluster, event) {
        return Err(crate::StarryError::NotFound);
    }
    let prefer_cycle = !is_sampling
        && (is_generic_hw || is_named_pmu)
        && super::percpu::generic_event_for_target(
            target_cpu,
            required_cluster,
            perf_hw_id::PERF_COUNT_HW_CPU_CYCLES as u32,
        ) == Some(event);
    let counter = match (target_kind, prefer_cycle) {
        (PerfTargetKind::Cpu, true) => ValidatedHwCounter::SystemPreferredCycle(event),
        (PerfTargetKind::Cpu, false) => ValidatedHwCounter::SystemProgrammable(event),
        (PerfTargetKind::Task, true) => ValidatedHwCounter::TaskPreferredCycle(event),
        (PerfTargetKind::Task, false) => ValidatedHwCounter::TaskProgrammable(event),
    };

    Ok(ValidatedHwOpen {
        num_counters,
        counter,
        is_sampling,
        is_freq,
        sample_period,
        target_freq,
        required_cluster,
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
) -> crate::StarryResult<HwPerfEvent> {
    let owner_cpu = match target {
        AuthorizedPerfTarget::Task { task, cpu } => {
            return perf_event_open_hw_per_task(attr, task, cpu, validated);
        }
        AuthorizedPerfTarget::Cpu(cpu) => cpu,
    };
    let exclude_user = attr.exclude_user() != 0;
    let exclude_kernel = attr.exclude_kernel() != 0;

    let (counter, event) = match validated.counter {
        ValidatedHwCounter::SystemPreferredCycle(event) => (alloc_system_cycle(owner_cpu), event),
        ValidatedHwCounter::SystemProgrammable(event) if !validated.is_sampling => (None, event),
        ValidatedHwCounter::SystemProgrammable(event) => (
            Some(alloc_system(
                owner_cpu,
                event,
                false,
                validated.num_counters,
            )?),
            event,
        ),
        ValidatedHwCounter::TaskPreferredCycle(_) | ValidatedHwCounter::TaskProgrammable(_) => {
            return Err(crate::StarryError::BadState);
        }
    };
    // All programmable counting events are logical, including cycle fallbacks.
    // Check before constructing a worker; only a native 64-bit counter can run
    // without overflow delivery. Sampling's fixed reservation rolls back here.
    if counter.is_none_or(|counter| counter.programmable_index().is_some())
        && sampling::ensure_pmu_irq_registered().is_err()
    {
        if let Some(counter) = counter {
            free_system(owner_cpu, counter);
        }
        return Err(crate::StarryError::NoSuchDevice);
    }
    let flexible = counter.is_none().then(|| {
        super::system_flex::SystemFlexCounter::new(owner_cpu, event, exclude_user, exclude_kernel)
    });
    let counter = counter.unwrap_or(super::hw_owner::Counter::Programmable(0));
    let event = counter.programmable_index().map(|_| event);
    if flexible.is_none()
        && let Err(error) = cpu_worker::configure_system(
            owner_cpu,
            SystemPmuConfigure {
                counter,
                event,
                exclude_user,
                exclude_kernel,
            },
        )
    {
        free_system(owner_cpu, counter);
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
            sample_id_all: attr.sample_id_all() != 0,
            sample_user_lr: attr.sample_regs_user == super::uapi::PERF_REG_ARM64_LR_MASK,
            observer: crate::task::current_user_task()
                .as_thread()
                .active_pid_namespace()
                .id(),
            poll_ready,
            notify,
            poll_alive,
            output: PerfOutputRoute::new(),
            read: Arc::new(SamplingReadState {
                loss: Arc::new(sampling::LossState::new()),
                sample_count: Arc::new(sampling::SamplingCount::new()),
                enabled_at_ns: core::sync::atomic::AtomicU64::new(0),
                time_enabled_ns: core::sync::atomic::AtomicU64::new(0),
                time_running_ns: core::sync::atomic::AtomicU64::new(0),
            }),
        }
    });

    Ok(HwPerfEvent::new_system(SystemEventInit {
        counter,
        owner: owner_cpu,
        read_format: attr.read_format,
        sampling,
        flexible,
        enable_at_open: attr.disabled() == 0,
    }))
}

/// Opens a task-bound hardware-PMU event (`perf_event_open` with `pid >= 0`).
fn perf_event_open_hw_per_task(
    attr: &perf_event_attr,
    task: crate::task::UserTaskRef,
    cpu_filter: Option<PerfCpuId>,
    validated: ValidatedHwOpen,
) -> crate::StarryResult<HwPerfEvent> {
    let thread = task.as_thread();
    let scheduler_id = thread.scheduler_id().ok_or(crate::StarryError::BadState)?;

    let exclude_user = attr.exclude_user() != 0;
    let exclude_kernel = attr.exclude_kernel() != 0;

    let (counter, event, flexible) = match validated.counter {
        ValidatedHwCounter::TaskPreferredCycle(event) => {
            let counter = alloc_cycle_counter();
            (
                counter.unwrap_or(super::hw_owner::Counter::Programmable(0)),
                event,
                counter.is_none(),
            )
        }
        ValidatedHwCounter::TaskProgrammable(event) => {
            // Flexible events are logical until a scheduler slice acquires one
            // of the executing CPU's physical programmable slots.
            (super::hw_owner::Counter::Programmable(0), event, true)
        }
        ValidatedHwCounter::SystemPreferredCycle(_) | ValidatedHwCounter::SystemProgrammable(_) => {
            return Err(crate::StarryError::BadState);
        }
    };

    // Logical flexible events own no slot yet; fixed fallbacks must release
    // their reservation if the overflow IRQ cannot be installed.
    // Inherited copies are flexible even when this root owns the cycle counter.
    if (counter.programmable_index().is_some() || attr.inherit() != 0)
        && sampling::ensure_pmu_irq_registered().is_err()
    {
        if !flexible {
            free_counter(counter);
        }
        return Err(crate::StarryError::NoSuchDevice);
    }

    let enabled = attr.disabled() == 0;
    let observer = crate::task::current_user_task()
        .as_thread()
        .active_pid_namespace()
        .id();
    let owner_ids = thread
        .proc_data
        .identity()
        .visible_number_in(observer)
        .map(crate::task::TgidNumber::from)
        .zip(
            thread
                .pid_identity()
                .visible_number_in(observer)
                .map(crate::task::TidNumber::from),
        );
    let per_task_counter = Arc::new(super::task::PerTaskCounter::new(
        super::task::PerTaskConfig {
            loss: Arc::new(sampling::LossState::new()),
            scheduler_id,
            counter,
            flexible,
            scheduler_tick_lease: flexible.then(|| thread.proc_data.acquire_perf_scheduler_tick()),
            event,
            exclude_user,
            exclude_kernel,
            read_format: attr.read_format,
            enabled,
            enable_on_exec: attr.enable_on_exec() != 0,
            cpu_filter,
            required_cluster: validated.required_cluster,
            sample_period: validated.sample_period,
            sample_type: attr.sample_type,
            sample_user_lr: attr.sample_regs_user == super::uapi::PERF_REG_ARM64_LR_MASK,
            freq: validated.is_freq,
            target_freq: validated.target_freq,
            want_comm: attr.comm() != 0,
            want_mmap2: attr.mmap2() != 0,
            want_task: attr.task() != 0,
            sample_id_all: attr.sample_id_all() != 0,
            inherit: attr.inherit() != 0,
            observer,
            owner_ids,
        },
    ));
    let family = PerfInheritanceFamily::new(Arc::clone(&per_task_counter), enabled);
    if let Err(error) = super::task::attach(thread, per_task_counter) {
        super::task::free_hw(&family.root())
            .expect("an unpublished task event must roll back without owner-CPU work");
        return Err(error);
    }
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

fn validate_sampling(
    attr: &perf_event_attr,
    raw: u64,
    is_freq: bool,
    kind: &str,
) -> crate::StarryResult<()> {
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
        return Err(crate::StarryError::Unsupported);
    }
    if !is_freq && raw > u32::MAX as u64 {
        warn!("perf_event_open: {kind} period {raw} exceeds 32-bit counter");
        return Err(crate::StarryError::InvalidInput);
    }
    Ok(())
}
