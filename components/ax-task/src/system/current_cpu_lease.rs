//! Nestable current-thread CPU-placement lease.

use core::marker::PhantomData;

use crate::{CpuId, ThreadHandle, ThreadId};

/// Prevents the owning scheduler thread from migrating until the lease drops.
///
/// The lease is nestable. Every nested acquisition shares one generation, and
/// the thread becomes migratable only after the final lease in that generation
/// is released. Preemption and blocking remain valid; a runnable or woken owner
/// stays attached to the leased CPU.
///
/// A lease belongs to the acquiring execution context and cannot cross a Rust
/// thread boundary:
///
/// ```compile_fail
/// use ax_task::CurrentCpuLease;
///
/// fn require_send<T: Send>() {}
/// require_send::<CurrentCpuLease>();
/// ```
#[derive(Debug)]
#[must_use = "dropping the lease immediately makes the thread migratable"]
pub struct CurrentCpuLease {
    thread: ThreadHandle,
    cpu: CpuId,
    generation: u64,
    _not_send_or_sync: PhantomData<*mut ()>,
}

impl CurrentCpuLease {
    pub(super) const fn new(thread: ThreadHandle, cpu: CpuId, generation: u64) -> Self {
        Self {
            thread,
            cpu,
            generation,
            _not_send_or_sync: PhantomData,
        }
    }

    /// Returns the generation-bearing thread identity protected by this lease.
    pub fn thread_id(&self) -> ThreadId {
        self.thread.id()
    }

    /// Returns the CPU to which this lease constrains placement.
    pub const fn cpu(&self) -> CpuId {
        self.cpu
    }

    /// Returns the nesting generation shared by concurrent leases of the owner.
    pub const fn generation(&self) -> u64 {
        self.generation
    }
}

impl Drop for CurrentCpuLease {
    fn drop(&mut self) {
        self.thread
            .core
            .sched()
            .lock()
            .release_current_cpu_pin(self.cpu, self.generation);
    }
}
