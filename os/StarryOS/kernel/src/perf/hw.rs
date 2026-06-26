//! Hardware-PMU `perf` events (M0: ARM PMUv3 cycle counter only).
//!
//! This is the minimal hardware slice of `perf_event_open(2)`: a single
//! `PERF_TYPE_HARDWARE` / `PERF_COUNT_HW_CPU_CYCLES` event backed by the
//! dedicated ARM cycle counter (`PMCCNTR_EL0`). The per-CPU sysreg layer
//! lives in [`ax_cpu::pmu`]; this module only wires it into the file-like
//! `PerfEvent` dispatcher.
//!
//! Scope is deliberately tiny: one event, one CPU, `read_format == 0`. The
//! `ioctl(ENABLE/DISABLE/RESET)` calls drive the counter and `read(perf_fd,
//! &val, 8)` returns the bare `u64` cycle count. Sampling, grouping, time
//! fields, the `exclude_*` bits, and the `HW_CACHE` / `RAW` types are later
//! milestones.

use core::any::Any;

use ax_errno::{AxError, AxResult};
use axpoll::{IoEvents, Pollable};
use kbpf_basic::linux_bpf::perf_event_attr;
#[cfg(target_arch = "aarch64")]
use kbpf_basic::linux_bpf::perf_hw_id;

use super::PerfEventOps;

/// A hardware-PMU perf event. M0 backs exactly one counter — the ARM cycle
/// counter — so no per-event state is needed; all state lives in the global
/// PMU sysregs driven through [`ax_cpu::pmu`]. M1 will add per-counter state.
#[derive(Debug)]
pub struct HwPerfEvent;

impl Pollable for HwPerfEvent {
    fn poll(&self) -> IoEvents {
        // A counter is always readable: `read(perf_fd)` returns the current
        // value without blocking. M0 has no sampling ringbuf, so only `IN`.
        IoEvents::IN
    }

    fn register(&self, _context: &mut core::task::Context<'_>, _events: IoEvents) {
        // The counter never transitions readiness, so there is nothing to
        // wake on; registration is a no-op.
    }
}

impl PerfEventOps for HwPerfEvent {
    fn enable(&mut self) -> AxResult<()> {
        #[cfg(target_arch = "aarch64")]
        ax_cpu::pmu::cycles::enable();
        Ok(())
    }

    fn disable(&mut self) -> AxResult<()> {
        #[cfg(target_arch = "aarch64")]
        ax_cpu::pmu::cycles::disable();
        Ok(())
    }

    fn reset(&mut self) -> AxResult<()> {
        #[cfg(target_arch = "aarch64")]
        ax_cpu::pmu::cycles::reset();
        Ok(())
    }

    fn read_count(&mut self) -> AxResult<u64> {
        #[cfg(target_arch = "aarch64")]
        {
            Ok(ax_cpu::pmu::cycles::read())
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            Err(AxError::Unsupported)
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Open a hardware-PMU perf event from a user `perf_event_attr`.
///
/// M0 supports only `PERF_COUNT_HW_CPU_CYCLES` on a PMUv3-capable CPU. The
/// counter is configured (filter reset to count EL0+EL1, value reset to 0)
/// but left disabled: the attr carries `disabled = 1`, and the caller drives
/// it with `ioctl(PERF_EVENT_IOC_ENABLE)`.
#[cfg(target_arch = "aarch64")]
pub fn perf_event_open_hw(attr: &perf_event_attr) -> AxResult<HwPerfEvent> {
    // No PMUv3 → no hardware events.
    if ax_cpu::pmu::probe().is_none() {
        return Err(AxError::Unsupported);
    }

    // Idempotent per-CPU global enable (`PMCR_EL0.E`).
    ax_cpu::pmu::init_cpu();

    // M0 supports only the CPU cycle counter.
    if attr.config != perf_hw_id::PERF_COUNT_HW_CPU_CYCLES as u64 {
        warn!(
            "perf_event_open: unsupported hardware config {:#x} (M0 supports only \
             PERF_COUNT_HW_CPU_CYCLES)",
            attr.config
        );
        return Err(AxError::Unsupported);
    }

    // M0: count both EL0 and EL1; honouring `exclude_*` is M1. `configure`
    // also resets the counter to 0. Do not enable here — `disabled = 1`.
    ax_cpu::pmu::cycles::configure(false, false);

    Ok(HwPerfEvent)
}

/// Non-aarch64 fallback: no hardware PMU support outside ARM PMUv3 in M0.
#[cfg(not(target_arch = "aarch64"))]
pub fn perf_event_open_hw(_attr: &perf_event_attr) -> AxResult<HwPerfEvent> {
    Err(AxError::Unsupported)
}
