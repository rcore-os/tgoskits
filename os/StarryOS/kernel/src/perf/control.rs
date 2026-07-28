//! Task-context control capability for perf events.
//!
//! The perf fd keeps its historical non-sleeping event lock for BPF output
//! paths that may run with preemption disabled. Implementations that need to
//! wait for a CPU-owned worker expose this separate capability; file control
//! operations dispatch through it without retaining the non-sleeping lock.

use alloc::sync::Arc;
use core::{any::Any, fmt::Debug};

use ax_errno::{AxError, AxResult};
use ax_memory_addr::PhysAddr;
use axpoll::Pollable;

use super::PerfReadValues;
#[cfg(target_arch = "aarch64")]
use super::output::{PerfOutputScope, PerfRingOutput};

/// Sleepable, task-context operations for one perf fd.
pub(super) trait PerfControl: Pollable + Send + Sync + Debug {
    /// Starts the event and waits until its owner context has committed it.
    fn enable(&self) -> AxResult<()>;

    /// Stops the event and waits for owner-context quiescence.
    fn disable(&self) -> AxResult<()>;

    /// Resets the event count in owner-context order.
    fn reset(&self) -> AxResult<()>;

    /// Takes an owner-consistent counter and timing snapshot.
    fn read_values(&self) -> AxResult<PerfReadValues>;

    /// Allocates and publishes the user-visible mmap backing.
    fn device_mmap(&self, _len: usize) -> AxResult<(PhysAddr, Arc<dyn Any + Send + Sync>)> {
        Err(AxError::Unsupported)
    }

    /// Returns a pinned output ring for `PERF_EVENT_IOC_SET_OUTPUT`.
    #[cfg(target_arch = "aarch64")]
    fn output_ring(&self) -> Option<PerfRingOutput> {
        None
    }

    /// Returns the task or CPU context used for output compatibility checks.
    #[cfg(target_arch = "aarch64")]
    fn output_scope(&self) -> Option<PerfOutputScope> {
        None
    }

    /// Redirects records into a separately pinned output ring.
    #[cfg(target_arch = "aarch64")]
    fn redirect_output(&self, _output: PerfRingOutput) -> AxResult<()> {
        Ok(())
    }

    /// Detaches a previous redirect and restores this event's own mmap ring.
    #[cfg(target_arch = "aarch64")]
    fn detach_output(&self) -> AxResult<()> {
        Ok(())
    }
}
