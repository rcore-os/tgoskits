// SPDX-License-Identifier: Apache-2.0 AND MPL-2.0
// Boot root installation and jumps migrated from someboot (周睿).
//! Helper functions to initialize the CPU states on systems bootstrapping.

use ax_memory_addr::PhysAddr;

use super::paging::{PWCH_VALUE, PWCL_VALUE};

/// Initializes trap handling on the current CPU.
///
/// In detail, it initializes the exception vector on LoongArch64 platforms.
pub fn init_trap() {
    unsafe {
        unsafe extern "C" {
            fn exception_entry_base();
        }
        core::arch::asm!(include_asm_macros!(), "csrwr $r0, KSAVE_KSP");
        crate::asm::write_exception_entry_base(exception_entry_base as *const () as usize);
    }
}

/// Returns the running address of the CPU-owned early exception vectors.
pub fn boot_vector() -> usize {
    super::entry::boot::vector()
}

/// Installs early exception and refill vectors with 512-byte vector spacing.
///
/// # Safety
/// Execute at PLV0 with IRQs masked. Both entries must remain executable and
/// correctly mapped; the refill address is physical. The boot stack and
/// BootTrapHandler provider must remain available before runtime initialization.
pub unsafe fn install_boot_vectors(vector: usize, refill: PhysAddr) {
    // SAFETY: the caller owns the local boot exception register bank.
    unsafe {
        core::arch::asm!(
            "csrrd {ecfg}, 0x4", "bstrins.d {ecfg}, {spacing}, 18, 16",
            "csrwr {ecfg}, 0x4", "csrwr {vector}, 0xc", "csrwr {refill}, 0x88",
            ecfg = out(reg) _, spacing = in(reg) 7usize,
            vector = in(reg) vector, refill = in(reg) refill.as_usize(), options(nostack),
        );
    }
}

/// Installs a shared lower/upper boot root with four-level 4-KiB geometry.
///
/// # Safety
/// Execute at PLV0 with IRQs masked. The aligned root and walker tables must
/// remain mapped and cover the current instruction, stack and handoff data.
/// This does not enable paged translation or provide cross-CPU invalidation.
#[inline(always)]
pub unsafe fn install_boot_page_table(root: PhysAddr) {
    // SAFETY: this assembly-only setup is valid before final image relocation.
    unsafe {
        core::arch::asm!(
            "csrrd {stlbps}, 0x1e", "bstrins.d {stlbps}, {ps}, 5, 0",
            "csrwr {root}, 0x1a", "csrwr {root}, 0x19", "dbar 0",
            "csrwr {stlbps}, 0x1e", "csrwr {pwcl}, 0x1c", "csrwr {pwch}, 0x1d",
            "invtlb 0x0, $r0, $r0",
            stlbps = out(reg) _, ps = in(reg) 12usize, root = in(reg) root.as_usize(),
            pwcl = in(reg) PWCL_VALUE as usize, pwch = in(reg) PWCH_VALUE as usize,
            options(nostack),
        );
    }
}

/// Enables coherent paged translation in one CRMD update.
///
/// # Safety
/// The owner must install valid roots, walker geometry, vectors and mappings
/// retaining every active instruction, stack and handoff byte across this write.
#[inline(always)]
pub unsafe fn enable_paged_translation() {
    let mut crmd: usize;
    // SAFETY: one local transition updates DA/PG and both memory types atomically.
    unsafe {
        core::arch::asm!("csrrd {}, 0", out(reg) crmd, options(nostack));
        crmd = (crmd & !((1 << 3) | (3 << 5) | (3 << 7))) | (1 << 4) | (1 << 5) | (1 << 7);
        core::arch::asm!("csrwr {}, 0", in(reg) crmd, options(nostack));
    }
}

/// Transfers to boot code on a new stack, placing its handoff argument in a0.
///
/// # Safety
/// Entry must be executable at the current translation regime, the aligned
/// stack must be exclusively owned and writable, and the argument must satisfy
/// the entry's contract. No resource on the abandoned stack may need destruction.
#[unsafe(naked)]
pub unsafe extern "C" fn jump_to(
    _argument: usize,
    _stack: crate::VirtAddr,
    _entry: crate::VirtAddr,
) -> ! {
    core::arch::naked_asm!("ibar 0", "dbar 0", "move $sp, $a1", "jr $a2");
}
