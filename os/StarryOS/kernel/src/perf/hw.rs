//! Hardware-PMU `perf` events (M1: ARM PMUv3 counting via `perf stat`).
//!
//! This is the counting slice of `perf_event_open(2)`: one or more concurrent
//! `PERF_TYPE_HARDWARE` / `PERF_TYPE_RAW` events, each backed by either the
//! dedicated 64-bit cycle counter (`PMCCNTR_EL0`) or one of the programmable
//! 32-bit event counters (`PMEVCNTRn_EL0`). The per-CPU sysreg layer lives in
//! [`ax_cpu::pmu`]; this module allocates counters, configures the requested
//! event, drives `ioctl(ENABLE/DISABLE/RESET)`, and serves `read(perf_fd)`
//! with the timing fields `perf stat` expects.
//!
//! Scope: single CPU (the current one), no sampling, no overflow IRQ, no
//! multiplexing. Because there is no multiplexing, `time_running` always
//! equals `time_enabled`. Programmable counters are 32-bit and can wrap
//! within an enabled window; the overflow IRQ that fixes this is a later
//! milestone (M2).

use core::any::Any;

use ax_errno::{AxError, AxResult};
use axpoll::{IoEvents, Pollable};
use kbpf_basic::linux_bpf::perf_event_attr;
#[cfg(target_arch = "aarch64")]
use kbpf_basic::linux_bpf::{perf_hw_id, perf_type_id};

use super::PerfEventOps;
#[cfg(target_arch = "aarch64")]
use super::PerfReadValues;

/// The hardware counter a [`HwPerfEvent`] is bound to.
///
/// `Cycle` is the dedicated 64-bit cycle counter (`PMCCNTR_EL0`);
/// `Programmable(n)` is one of the 32-bit event counters at logical index
/// `n` in `0..num_counters`.
#[cfg(target_arch = "aarch64")]
#[derive(Debug, Clone, Copy)]
enum Counter {
    Cycle,
    Programmable(usize),
}

/// Per-CPU counter allocator. M1 is single-core, so a single global allocator
/// (mirroring the cycle-only PMU state already living in sysregs) tracks which
/// physical counters are in use. `used` is a bitmask over programmable counter
/// indices `0..num_counters`; `cycle_used` guards the dedicated cycle counter.
#[cfg(target_arch = "aarch64")]
struct HwAlloc {
    /// Number of programmable counters (`PMCR_EL0.N`), from [`ax_cpu::pmu::probe`].
    num_counters: usize,
    /// Bitmask of allocated programmable counters (bit `n` ⇒ index `n` in use).
    used: u32,
    /// Whether the dedicated cycle counter is allocated.
    cycle_used: bool,
}

#[cfg(target_arch = "aarch64")]
impl HwAlloc {
    const fn new() -> Self {
        HwAlloc {
            num_counters: 0,
            used: 0,
            cycle_used: false,
        }
    }

    /// Allocate the dedicated cycle counter, if free.
    fn alloc_cycle(&mut self) -> Option<Counter> {
        if self.cycle_used {
            return None;
        }
        self.cycle_used = true;
        Some(Counter::Cycle)
    }

    /// Allocate the lowest free programmable counter, if any.
    fn alloc_counter(&mut self) -> Option<Counter> {
        for n in 0..self.num_counters.min(32) {
            if self.used & (1 << n) == 0 {
                self.used |= 1 << n;
                return Some(Counter::Programmable(n));
            }
        }
        None
    }

    /// Release a previously allocated counter.
    fn free(&mut self, counter: Counter) {
        match counter {
            Counter::Cycle => self.cycle_used = false,
            Counter::Programmable(n) => {
                if n < 32 {
                    self.used &= !(1 << n);
                }
            }
        }
    }
}

#[cfg(target_arch = "aarch64")]
static ALLOC: ax_kspin::SpinNoPreempt<HwAlloc> = ax_kspin::SpinNoPreempt::new(HwAlloc::new());

/// A hardware-PMU perf event: one allocated counter plus the timing
/// accumulators `perf stat` reads back through `read_format`.
///
/// Timing follows Linux semantics: `time_enabled` accumulates wall time the
/// event has spent enabled and `time_running` the time it was actually
/// scheduled onto hardware. With no multiplexing in M1 the two are equal.
#[cfg(target_arch = "aarch64")]
#[derive(Debug)]
pub struct HwPerfEvent {
    /// The physical counter backing this event.
    counter: Counter,
    /// `attr.read_format`, controlling which fields `read(perf_fd)` emits.
    read_format: u64,
    /// Monotonic ns timestamp of the last `enable`, or `None` while disabled.
    enabled_since: Option<u64>,
    /// Accumulated enabled time across past enabled windows (ns).
    time_enabled: u64,
    /// Accumulated running time across past enabled windows (ns). Equal to
    /// `time_enabled` in M1 (no multiplexing).
    time_running: u64,
}

#[cfg(target_arch = "aarch64")]
impl HwPerfEvent {
    /// Reads the current raw counter value (cycle ⇒ 64-bit, programmable ⇒
    /// 32-bit zero-extended).
    fn raw_value(&self) -> u64 {
        match self.counter {
            Counter::Cycle => ax_cpu::pmu::cycles::read(),
            Counter::Programmable(n) => ax_cpu::pmu::counter::read(n),
        }
    }
}

#[cfg(target_arch = "aarch64")]
impl Drop for HwPerfEvent {
    fn drop(&mut self) {
        // Stop the counter so a freed slot does not keep ticking, then release
        // it back to the allocator for reuse.
        match self.counter {
            Counter::Cycle => ax_cpu::pmu::cycles::disable(),
            Counter::Programmable(n) => ax_cpu::pmu::counter::disable(n),
        }
        ALLOC.lock().free(self.counter);
    }
}

#[cfg(target_arch = "aarch64")]
impl Pollable for HwPerfEvent {
    fn poll(&self) -> IoEvents {
        // A counter is always readable: `read(perf_fd)` returns the current
        // value without blocking. M1 has no sampling ringbuf, so only `IN`.
        IoEvents::IN
    }

    fn register(&self, _context: &mut core::task::Context<'_>, _events: IoEvents) {
        // The counter never transitions readiness, so there is nothing to wake
        // on; registration is a no-op.
    }
}

#[cfg(target_arch = "aarch64")]
impl PerfEventOps for HwPerfEvent {
    fn enable(&mut self) -> AxResult<()> {
        if self.enabled_since.is_none() {
            self.enabled_since = Some(ax_runtime::hal::time::monotonic_time_nanos());
        }
        match self.counter {
            Counter::Cycle => ax_cpu::pmu::cycles::enable(),
            Counter::Programmable(n) => ax_cpu::pmu::counter::enable(n),
        }
        Ok(())
    }

    fn disable(&mut self) -> AxResult<()> {
        match self.counter {
            Counter::Cycle => ax_cpu::pmu::cycles::disable(),
            Counter::Programmable(n) => ax_cpu::pmu::counter::disable(n),
        }
        if let Some(since) = self.enabled_since.take() {
            let now = ax_runtime::hal::time::monotonic_time_nanos();
            let elapsed = now.saturating_sub(since);
            self.time_enabled += elapsed;
            self.time_running += elapsed;
        }
        Ok(())
    }

    fn reset(&mut self) -> AxResult<()> {
        match self.counter {
            Counter::Cycle => ax_cpu::pmu::cycles::reset(),
            Counter::Programmable(n) => ax_cpu::pmu::counter::reset(n),
        }
        Ok(())
    }

    fn read_values(&mut self) -> AxResult<PerfReadValues> {
        // Current timing = accumulated past windows + the live window, if any.
        let (mut time_enabled, mut time_running) = (self.time_enabled, self.time_running);
        if let Some(since) = self.enabled_since {
            let now = ax_runtime::hal::time::monotonic_time_nanos();
            let elapsed = now.saturating_sub(since);
            time_enabled += elapsed;
            time_running += elapsed;
        }
        Ok(PerfReadValues {
            value: self.raw_value(),
            time_enabled,
            time_running,
            // M1 does not assign per-event ids (no event groups); report 0.
            id: 0,
            read_format: self.read_format,
        })
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Open a hardware-PMU perf event from a user `perf_event_attr`.
///
/// Supports `PERF_TYPE_HARDWARE` (cycles via the dedicated counter, every
/// other mapped `perf_hw_id` via a programmable counter) and `PERF_TYPE_RAW`
/// (the low 16 bits of `config` as the raw ARM event number on a programmable
/// counter). The counter is configured (event programmed, `exclude_*` applied,
/// value reset to 0) but left disabled: the attr carries `disabled = 1`, and
/// the caller drives it with `ioctl(PERF_EVENT_IOC_ENABLE)`.
#[cfg(target_arch = "aarch64")]
pub fn perf_event_open_hw(attr: &perf_event_attr) -> AxResult<HwPerfEvent> {
    // No PMUv3 → no hardware events.
    let Some(info) = ax_cpu::pmu::probe() else {
        return Err(AxError::Unsupported);
    };

    // Idempotent per-CPU global enable (`PMCR_EL0.E`).
    ax_cpu::pmu::init_cpu();

    // Refresh the counter count the allocator sizes its bitmask against. Safe
    // to set every open: M1 is single-core so `num_counters` is invariant.
    ALLOC.lock().num_counters = info.num_counters;

    let exclude_user = attr.exclude_user() != 0;
    let exclude_kernel = attr.exclude_kernel() != 0;

    let counter = if attr.type_ == perf_type_id::PERF_TYPE_HARDWARE as u32 {
        if attr.config == perf_hw_id::PERF_COUNT_HW_CPU_CYCLES as u64 {
            // The dedicated 64-bit cycle counter.
            let Some(counter) = ALLOC.lock().alloc_cycle() else {
                return Err(AxError::NoMemory);
            };
            // `exclude_*` map onto the cycle filter; `configure` also resets.
            ax_cpu::pmu::cycles::configure(exclude_user, exclude_kernel);
            counter
        } else {
            // Map the generic hardware event to an ARM PMUv3 event number.
            let Some(event) = ax_cpu::pmu::hw_event_to_arm(attr.config as u32) else {
                warn!(
                    "perf_event_open: unsupported hardware config {:#x}",
                    attr.config
                );
                return Err(AxError::Unsupported);
            };
            alloc_programmable(event, exclude_user, exclude_kernel)?
        }
    } else if attr.type_ == perf_type_id::PERF_TYPE_RAW as u32 {
        // Raw events: low 16 bits of `config` are the ARM event number.
        let event = (attr.config & 0xFFFF) as u16;
        alloc_programmable(event, exclude_user, exclude_kernel)?
    } else {
        // HW_CACHE / BREAKPOINT and anything else are not supported in M1.
        warn!(
            "perf_event_open: unsupported hardware type {:#x}",
            attr.type_
        );
        return Err(AxError::Unsupported);
    };

    Ok(HwPerfEvent {
        counter,
        read_format: attr.read_format,
        // `disabled = 1`: do not enable; timing accumulators start empty.
        enabled_since: None,
        time_enabled: 0,
        time_running: 0,
    })
}

/// Allocate a programmable counter, validate the event, and program it.
///
/// Common events (`< 0x40`) are gated through [`ax_cpu::pmu::event_supported`];
/// IMPLEMENTATION DEFINED events (`>= 0x40`) cannot be validated and are let
/// through. The counter is configured but left disabled.
#[cfg(target_arch = "aarch64")]
fn alloc_programmable(event: u16, exclude_user: bool, exclude_kernel: bool) -> AxResult<Counter> {
    if !ax_cpu::pmu::event_supported(event) {
        warn!(
            "perf_event_open: ARM event {:#x} not implemented on this CPU",
            event
        );
        return Err(AxError::Unsupported);
    }
    let Some(Counter::Programmable(n)) = ALLOC.lock().alloc_counter() else {
        return Err(AxError::NoMemory);
    };
    // `configure` applies the event + filter and resets the counter to 0.
    ax_cpu::pmu::counter::configure(n, event, exclude_user, exclude_kernel);
    Ok(Counter::Programmable(n))
}

/// Non-aarch64 fallback: no hardware PMU support outside ARM PMUv3.
///
/// A pub unit struct keeps the dispatcher in `mod.rs` arch-agnostic; the
/// `PerfEventOps` methods all report `Unsupported`, and `perf_event_open_hw`
/// rejects the open before one is ever constructed.
#[cfg(not(target_arch = "aarch64"))]
#[derive(Debug)]
pub struct HwPerfEvent;

#[cfg(not(target_arch = "aarch64"))]
impl Pollable for HwPerfEvent {
    fn poll(&self) -> IoEvents {
        IoEvents::IN
    }

    fn register(&self, _context: &mut core::task::Context<'_>, _events: IoEvents) {}
}

#[cfg(not(target_arch = "aarch64"))]
impl PerfEventOps for HwPerfEvent {
    fn enable(&mut self) -> AxResult<()> {
        Err(AxError::Unsupported)
    }

    fn disable(&mut self) -> AxResult<()> {
        Err(AxError::Unsupported)
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Non-aarch64 fallback: no hardware PMU support outside ARM PMUv3.
#[cfg(not(target_arch = "aarch64"))]
pub fn perf_event_open_hw(_attr: &perf_event_attr) -> AxResult<HwPerfEvent> {
    Err(AxError::Unsupported)
}
