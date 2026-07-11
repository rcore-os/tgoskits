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

/// Reads a machine word at a *kernel* virtual address for the FP walk.
///
/// Routed through the IRQ-safe no-fault reader ([`super::nofault`]) rather than a
/// raw dereference: a corrupt or stale frame pointer that slips past the
/// range / alignment / monotonic guards and lands on an unmapped kernel VA
/// returns `None` (ending the walk) instead of taking an unrecoverable data abort
/// in hard-IRQ context (there is no kernel fault-fixup table). A legitimate
/// kernel-stack frame is mapped, so it resolves and is read through the direct
/// map.
#[inline]
fn read_kernel_word(va: usize) -> Option<usize> {
    super::nofault::read_kernel_word_nofault(va).map(|w| w as usize)
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

/// Walks the *user* frame-pointer chain from (`pc`, `fp`) into `out`, returning
/// the number of `u64` entries written (`out[0] == pc`, so always `>= 1` when
/// `out` is non-empty).
///
/// Reads user memory through the IRQ-safe no-fault reader ([`super::nofault`]),
/// which returns `None` for any unmapped or non-RAM address — so a wild user `fp`
/// can never fault the kernel or touch device MMIO. The frame-pointer range is
/// bounded to a generous window above the interrupted user SP (`sp`): the stack
/// grows down, so every frame record sits at an address `>= sp`, and a wild `fp`
/// outside the window is rejected before any page-table walk. The IP range stays
/// permissive (any user VA); the no-fault reader validates each read. Together
/// with `walk_fp`'s alignment / monotonic / 8 MiB-gap / depth / total-step guards
/// this keeps a crafted user chain from stalling the handler. Yields a deep chain
/// only when the sampled user binary keeps frame pointers.
pub fn user_callchain(pc: usize, fp: usize, sp: usize, out: &mut [u64]) -> usize {
    /// Cap on how far above the interrupted SP a user frame record may sit — a
    /// typical maximum user stack, so a corrupt `fp` can't range over the whole
    /// address space.
    const USER_STACK_WINDOW: usize = 8 * 1024 * 1024;
    const USER_VA_END: usize = 1 << 48;
    let lo = sp & !0xfff;
    let hi = lo.saturating_add(USER_STACK_WINDOW).min(USER_VA_END);
    let fp_range = lo..hi;
    let ip_range = 1..USER_VA_END;
    axbacktrace::walk_fp(
        pc,
        fp,
        &ip_range,
        &fp_range,
        |va| super::nofault::read_user_word_nofault(va).map(|w| w as usize),
        out,
    )
}
