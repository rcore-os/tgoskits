use core::ptr::NonNull;

use crate::{CpuLocalAnchor, CpuPin};

/// Installs the current CPU's architecture-local anchor.
///
/// Architectures intentionally choose their own hardware encoding. For
/// example, RISC-V keeps the directly addressable runtime header in its scratch
/// CSR, while architectures with relocation addressing keep the relocation.
///
/// # Safety
///
/// `anchor.area_base()` must address a fully initialized, writable
/// [`CpuAreaHeader`](crate::CpuAreaHeader) that remains mapped for the lifetime
/// of this CPU. Local interrupts must be disabled, the caller must hold a
/// [`CpuPin`]-equivalent boot invariant, and no trap may use the anchor until
/// this function returns.
#[inline(always)]
pub unsafe fn install_current(anchor: CpuLocalAnchor) {
    // SAFETY: the architecture implementation inherits this function's full
    // installation contract.
    unsafe { imp::install_current(anchor) }
}

/// Reads the unverified architecture-owned current-area address.
///
/// This primitive performs no memory access through the observed address. It
/// exists so `ax-percpu` can first prove that the value names exactly one area
/// in its installed layout and only then inspect that area's header.
///
/// # Safety
///
/// `pin` must cover this read. The returned integer is untrusted: it may be
/// null, stale, or name an early-boot record rather than a CPU-area header. It
/// must not be dereferenced until the caller validates it against an owned
/// mapped region.
#[doc(hidden)]
#[inline(always)]
pub unsafe fn current_area_base_raw(_pin: &CpuPin) -> usize {
    // SAFETY: the caller owns the pinning contract. Architecture backends only
    // read the CPU-owned register here and never dereference the observed base.
    unsafe { imp::read_current_area_base() }
}

/// Returns the current CPU-area base under a caller-provided pinning invariant.
///
/// # Safety
///
/// A non-null CPU-area binding must already be installed, and the current
/// execution context must be unable to migrate for the complete operation that
/// consumes the returned pointer. The area must remain mapped for the CPU
/// lifetime.
#[inline(always)]
pub unsafe fn current_area_base_unchecked() -> NonNull<u8> {
    // SAFETY: the caller guarantees both pinning and a live non-null binding.
    let area_base = unsafe { imp::read_current_area_base() };
    // SAFETY: non-nullness is part of the forwarded caller contract.
    unsafe { NonNull::new_unchecked(area_base as *mut u8) }
}

#[cfg(feature = "host-test")]
mod imp {
    use core::cell::Cell;
    use std::sync::OnceLock;

    use super::*;

    std::thread_local! {
        static CURRENT_ANCHOR: Cell<Option<CpuLocalAnchor>> = const { Cell::new(None) };
    }

    // Linux host tests historically installed the bootstrap GS anchor before
    // libtest created its worker threads, so those workers inherited CPU 0.
    // Preserve that explicit fixture behavior without exposing a process-wide
    // mutable current CPU: a thread-local installation always overrides this
    // immutable bootstrap fallback.
    static BOOTSTRAP_ANCHOR: OnceLock<CpuLocalAnchor> = OnceLock::new();

    #[inline(always)]
    pub unsafe fn install_current(anchor: CpuLocalAnchor) {
        let _ = BOOTSTRAP_ANCHOR.set(anchor);
        CURRENT_ANCHOR.set(Some(anchor));
    }

    #[inline(always)]
    pub unsafe fn read_current_area_base() -> usize {
        CURRENT_ANCHOR
            .get()
            .or_else(|| BOOTSTRAP_ANCHOR.get().copied())
            .map_or(0, CpuLocalAnchor::area_base)
    }
}

#[cfg(all(not(feature = "host-test"), target_arch = "x86_64"))]
mod imp {
    use super::*;

    const IA32_GS_BASE: u32 = 0xc000_0101;

    #[inline(always)]
    pub unsafe fn install_current(anchor: CpuLocalAnchor) {
        let area_base = anchor.area_base() as u64;
        // SAFETY: the caller guarantees ring-0 execution and a mapped header.
        unsafe {
            core::arch::asm!(
                "wrmsr",
                in("ecx") IA32_GS_BASE,
                in("eax") area_base as u32,
                in("edx") (area_base >> 32) as u32,
                options(nostack, preserves_flags),
            );
        }
    }

    #[inline(always)]
    pub unsafe fn read_current_area_base() -> usize {
        let low: u32;
        let high: u32;
        // Read the register value itself. A GS-relative load would dereference
        // an unverified early-boot value before ax-percpu can range-check it.
        unsafe {
            core::arch::asm!(
                "rdmsr",
                in("ecx") IA32_GS_BASE,
                out("eax") low,
                out("edx") high,
                options(nostack, preserves_flags),
            );
        }
        ((high as usize) << 32) | low as usize
    }
}

#[cfg(all(not(feature = "host-test"), target_arch = "aarch64"))]
mod imp {
    use super::*;

    #[inline(always)]
    pub unsafe fn install_current(anchor: CpuLocalAnchor) {
        let area_base = anchor.area_base();
        #[cfg(not(feature = "arm-el2"))]
        unsafe {
            core::arch::asm!("msr TPIDR_EL1, {area_base}", area_base = in(reg) area_base)
        }
        #[cfg(feature = "arm-el2")]
        unsafe {
            core::arch::asm!("msr TPIDR_EL2, {area_base}", area_base = in(reg) area_base)
        }
    }

    #[inline(always)]
    pub unsafe fn read_current_area_base() -> usize {
        let area_base: usize;
        #[cfg(not(feature = "arm-el2"))]
        unsafe {
            core::arch::asm!("mrs {area_base}, TPIDR_EL1", area_base = out(reg) area_base)
        }
        #[cfg(feature = "arm-el2")]
        unsafe {
            core::arch::asm!("mrs {area_base}, TPIDR_EL2", area_base = out(reg) area_base)
        }
        area_base
    }
}

#[cfg(all(
    not(feature = "host-test"),
    any(target_arch = "riscv32", target_arch = "riscv64")
))]
mod imp {
    use super::*;

    #[inline(always)]
    pub unsafe fn install_current(anchor: CpuLocalAnchor) {
        // RISC-V trap entry needs a directly addressable fixed header before it
        // has a spare general-purpose register for linker arithmetic.
        unsafe { core::arch::asm!("csrw sscratch, {base}", base = in(reg) anchor.area_base()) }
    }

    #[inline(always)]
    pub unsafe fn read_current_area_base() -> usize {
        let area_base: usize;
        unsafe { core::arch::asm!("csrr {base}, sscratch", base = out(reg) area_base) };
        area_base
    }
}

#[cfg(all(not(feature = "host-test"), target_arch = "loongarch64"))]
mod imp {
    use super::*;

    #[inline(always)]
    pub unsafe fn install_current(anchor: CpuLocalAnchor) {
        let relocation = anchor.relocation().raw();
        let shadow = relocation;
        // `csrwr` replaces its register operand with the previous CSR value, so
        // publish the shadow before installing the live r21 relocation.
        unsafe {
            core::arch::asm!(
                "csrwr {shadow}, 0x33",
                shadow = inout(reg) shadow => _,
                options(nostack),
            );
            core::arch::asm!(
                "move $r21, {relocation}",
                relocation = in(reg) relocation,
                options(nostack),
            );
        }
    }

    #[inline(always)]
    pub unsafe fn read_current_area_base() -> usize {
        let relocation: usize;
        let shadow: usize;
        unsafe {
            core::arch::asm!(
                "move {relocation}, $r21",
                "csrrd {shadow}, 0x33",
                relocation = out(reg) relocation,
                shadow = out(reg) shadow,
                options(nostack),
            )
        };
        assert_eq!(
            relocation, shadow,
            "LoongArch CPU-local r21 differs from its KS3 relocation mirror"
        );
        crate::PerCpuRelocation::from_raw(relocation)
            .relocate(crate::symbol::cpu_area_header_link_address())
    }
}

#[cfg(all(not(feature = "host-test"), target_arch = "arm"))]
mod imp {
    use super::*;

    #[inline(always)]
    pub unsafe fn install_current(anchor: CpuLocalAnchor) {
        let area_base = anchor.area_base();
        unsafe {
            core::arch::asm!(
                "mcr p15, 0, {area_base}, c13, c0, 3",
                area_base = in(reg) area_base,
            )
        }
    }

    #[inline(always)]
    pub unsafe fn read_current_area_base() -> usize {
        let area_base: usize;
        unsafe {
            core::arch::asm!(
                "mrc p15, 0, {area_base}, c13, c0, 3",
                area_base = out(reg) area_base,
            )
        };
        area_base
    }
}
