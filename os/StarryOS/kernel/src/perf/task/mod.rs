//! Per-task hardware-PMU `perf` counting (`perf stat -- cmd`).
//!
//! Where [`super::hw`] can bind a system-wide event to an explicit CPU context,
//! this module counts a *specific task*: the counter is
//! programmed onto hardware only while the target task is the running task, and
//! its per-slice deltas are accumulated across context switches. That is what
//! makes `perf stat -- /bin/true` attribute events to the workload rather than
//! to whatever happened to run on the CPU.
//!
//! ## Ownership and lifetime
//!
//! A [`PerTaskCounter`] is shared (`Arc`) between two places:
//!
//! * the target [`Thread`]'s perf context, walked by the scheduler
//!   hooks ([`perf_sched_in`] / [`perf_sched_out`]) and the exec/exit hooks, and
//! * the [`super::hw::HwPerfEvent`] behind the perf fd, which serves
//!   `read(perf_fd)` / `ioctl(ENABLE/DISABLE/RESET)` and frees the HW counter on
//!   `Drop`.
//!
//! Both can outlive the other (the fd can be `close`d while the task runs, or
//! the task can exit while the fd is still open), so the HW counter is freed via
//! the idempotent [`free_hw`] from whichever side reaches end-of-life first
//! ([`HwPerfEvent::drop`] or [`on_task_exit`]).
//!
//! ## Hot-path cost
//!
//! The scheduler hooks run inside `switch_to` with IRQs disabled and preemption
//! off: no allocation, no sleeping locks. They early-return on a single relaxed
//! load of [`PERF_TASK_ACTIVE`] when no per-task counter exists anywhere, so the
//! common (perf-unused) case is one atomic load per switch.
//!
//! ## Per-task sampling (`perf record -- cmd`, M3-pt-rec)
//!
//! A task-bound event opened with a nonzero sampling period and a supported
//! scalar `sample_type` behaves like an [M2 sampling
//! event](super::sampling) *while the attached task is running*, and fires no
//! samples while it is not — so the samples are attributed to the task.
//!
//! This reuses the M2 IRQ backend wholesale. The mechanism is:
//!
//! * `mmap(perf_fd)` allocates the ring (in [`super::hw::HwPerfEvent::device_mmap`])
//!   and publishes the ring plus page/notify anchors through the fd-owned
//!   [`PerfInheritanceFamily`]. Existing and future descendants receive the same
//!   output.
//! * [`perf_sched_in`] arms the slice: `preload` the counter to overflow after
//!   `sample_period` events, `register` a [`SampleSlot`](super::sampling::SampleSlot)
//!   pointing at the ptc's ring + notify, and `enable_irq` the overflow line.
//! * [`perf_sched_out`] disarms the slice: stop the counter, `disable_irq`, and
//!   `unregister` the slot — so the next time some *other* task runs, an overflow
//!   on this counter cannot fire a sample into our ring.
//!
//! The IRQ-half (the overflow handler writing `PERF_RECORD_SAMPLE` and re-arming)
//! is exactly the M2 [`super::sampling::pmu_overflow_handler`] — nothing here
//! runs in IRQ context except via the registered slot.
//!
//! ## Scope / deferrals
//!
//! There is no counter multiplexing (so `time_running == time_enabled`).
//! Generation-bearing owner leases follow task migration across CPUs, and an
//! optional CPU filter limits eligibility. Sampling supports fixed-period
//! (`-c <period>`) and frequency mode (`-F`, `sample_freq`); inherited child
//! events share the root output through the same owned redirect boundary.

use alloc::sync::Arc;
use core::{
    any::Any,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use ax_memory_addr::PhysAddr;

pub use super::inheritance::on_clone_inherit;
pub(super) use super::task_context::PERF_TASK_ACTIVE;
pub(crate) use super::task_sideband::{on_clone_sideband, on_exec_sideband, on_mmap_sideband};
use super::{
    cpu_worker,
    hw_owner::Counter,
    inheritance::{PerfInheritanceFamily, PerfInheritanceFamilyWeak},
    output::{PerfOutputRoute, PerfRingOutput},
    rdpmc::{RdpmcMapping, RdpmcSnapshot, mapping_result},
    resource_lifecycle::{PmuResourceClaim, PmuResourceRelease},
    sampling::{self, SampleOutput, SampleSlot, SampleSlotConfig},
    sampling_lifecycle::{PmuCloseAction, PmuRunLease, PmuRunState, PmuStopClaim},
    sideband::{self, SidebandTarget},
    target::PerfCpuId,
};
use crate::task::{Thread, future::IrqNotify};

mod attachment;
mod control;
mod lifecycle;
mod model;
mod read;
mod scheduling;

pub use attachment::attach;
pub(super) use attachment::{detach_unpublished, now_ns};
pub(crate) use control::{disable_counter, reset_counter};
pub(super) use lifecycle::sideband_target;
pub(crate) use lifecycle::{free_hw, on_scheduler_task_exit};
pub use lifecycle::{on_exec, on_task_exit};
pub(super) use model::PerTaskConfig;
pub use model::PerTaskCounter;
pub(crate) use model::SamplingAnchors;
pub(crate) use read::{read_counter, read_task_on_owner};
pub(crate) use scheduling::stop_requested_on_owner;
pub use scheduling::{perf_sched_in, perf_sched_out};
