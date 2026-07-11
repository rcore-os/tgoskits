//! Kernel-side integration for PMU-sampling call-graph capture
//! (`PERF_SAMPLE_CALLCHAIN`).
//!
//! The allocation-free frame-pointer walk engine itself lives in `axbacktrace`
//! ([`axbacktrace::walk_fp`]); this module supplies the kernel address ranges and
//! the memory-read strategy used from the PMU overflow IRQ handler:
//!
//!   * **Kernel frames** are read by a direct dereference — kernel stacks are
//!     always mapped, and the IP/FP ranges reuse the ones `axbacktrace::init`
//!     already records at boot (`[_stext, _etext)` / the kernel address space).
//!   * **User frames** are read by the IRQ-safe no-fault page-table walk in
//!     [`super::nofault`], seeded from the interrupted `SP_EL0`/`x29` (Task M4b).
//!
//! Everything here is called from hard-IRQ context, so it must be allocation-free
//! and take no sleeping locks.

use core::ops::Range;

/// The instruction (text) and frame-pointer ranges used to validate a *kernel*
/// frame-pointer walk.
///
/// Reuses the ranges [`axbacktrace::init`] recorded at boot: `ip` = `[_stext,
/// _etext)`, `fp` = the kernel address space `[kernel_space_start, _end)`. Both
/// getters are lock-free (`spin::Once::get`), so this is safe from the overflow
/// handler. Returns `None` only before backtrace init (never on a running
/// system).
pub fn kernel_ranges() -> Option<(Range<usize>, Range<usize>)> {
    Some((axbacktrace::ip_range()?, axbacktrace::fp_range()?))
}

/// Reads a machine word at a *kernel* virtual address by direct dereference.
///
/// Valid only for kernel VAs: [`kernel_callchain`] validates every `fp` against
/// the kernel `fp_range` before the read, and kernel stacks are always mapped, so
/// in practice this cannot fault. There is no kernel fault-fixup table, so a
/// wildly corrupt frame pointer that slips past the range/alignment/monotonic
/// guards and lands on an unmapped kernel VA would fault — the M4b no-fault
/// reader ([`super::nofault`]) is the hardening path if that ever proves
/// reachable. Never use this for user addresses.
#[inline]
fn read_kernel_word(va: usize) -> Option<usize> {
    // SAFETY: `va` is a kernel VA inside the validated kernel `fp_range`; kernel
    // memory is always mapped, so the read cannot fault.
    Some(unsafe { *(va as *const usize) })
}

/// Walks the kernel frame-pointer chain from (`pc`, `fp`) into `out`, returning
/// the number of `u64` entries written (`out[0] == pc`, so always `>= 1` when
/// `out` is non-empty).
///
/// Allocation-free and safe from the PMU overflow handler. If the backtrace
/// ranges are not yet initialized, emits just the leaf `pc`.
pub fn kernel_callchain(pc: usize, fp: usize, out: &mut [u64]) -> usize {
    let Some((ip_range, fp_range)) = kernel_ranges() else {
        if out.is_empty() {
            return 0;
        }
        out[0] = pc as u64;
        return 1;
    };
    axbacktrace::walk_fp(pc, fp, &ip_range, &fp_range, read_kernel_word, out)
}
