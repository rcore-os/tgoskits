//! Private non-sleeping synchronization used by scheduler internals.

mod irq;
mod irq_ticket;
mod preempt;
mod raw;

use core::sync::atomic::Ordering;
#[cfg(any(target_arch = "x86_64", target_arch = "loongarch64"))]
use core::sync::atomic::compiler_fence;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use core::sync::atomic::fence;

pub(crate) use irq::*;
pub(crate) use irq_ticket::*;
pub(crate) use preempt::*;
pub(crate) use raw::*;

/// Provides Linux `smp_mb__after_spinlock()` ordering after a scheduler lock.
///
/// TSO architectures obtain the required RCsc ordering from the atomic lock
/// acquisition itself. Weakly ordered architectures need an explicit full
/// barrier after acquire, matching their Linux architecture definitions.
#[inline(always)]
pub(crate) fn smp_mb_after_spinlock() {
    #[cfg(any(target_arch = "x86_64", target_arch = "loongarch64"))]
    compiler_fence(Ordering::SeqCst);
    #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
    fence(Ordering::SeqCst);
}
