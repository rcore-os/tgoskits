/// Returns the link-time address of the fixed CPU-area prefix.
///
/// This special materialization remains private to the architecture leaf. A
/// normal per-CPU variable uses raw-pointer-to-integer arithmetic in
/// `ax-percpu`, so no architecture feature or instruction leaks downstream.
#[inline(always)]
pub fn cpu_area_header_link_address() -> usize {
    imp::cpu_area_header_link_address()
}

/// Returns the exact initialized CPU-area template size.
///
/// The final sentinel is one byte wide. Checked arithmetic turns a missing or
/// misordered linker boundary into an explicit failure instead of wrapping it
/// into a plausible layout size.
#[inline(always)]
pub fn cpu_area_template_size() -> Option<usize> {
    imp::cpu_area_template_end_link_address()
        .checked_add(core::mem::size_of::<u8>())?
        .checked_sub(cpu_area_header_link_address())
}

#[cfg(any(feature = "host-test", not(target_os = "none")))]
mod imp {
    #[inline(always)]
    pub fn cpu_area_header_link_address() -> usize {
        core::ptr::addr_of!(crate::__AX_CPU_AREA_PREFIX) as usize
    }

    #[inline(always)]
    pub fn cpu_area_template_end_link_address() -> usize {
        core::ptr::addr_of!(crate::__AX_CPU_AREA_TEMPLATE_END) as usize
    }
}

#[cfg(all(not(feature = "host-test"), target_os = "none", target_arch = "x86_64"))]
mod imp {
    #[inline(always)]
    pub fn cpu_area_header_link_address() -> usize {
        let address: usize;
        // SAFETY: this materializes only the link-time integer value and never
        // creates a Rust reference to the possibly-zero symbol.
        unsafe {
            core::arch::asm!(
                "mov {address}, offset {prefix}",
                address = out(reg) address,
                prefix = sym crate::__AX_CPU_AREA_PREFIX,
                options(nostack, preserves_flags),
            );
        }
        address
    }

    #[inline(always)]
    pub fn cpu_area_template_end_link_address() -> usize {
        let address: usize;
        // SAFETY: this materializes only the link-time integer value and never
        // dereferences the sentinel.
        unsafe {
            core::arch::asm!(
                "mov {address}, offset {end}",
                address = out(reg) address,
                end = sym crate::__AX_CPU_AREA_TEMPLATE_END,
                options(nostack, preserves_flags),
            );
        }
        address
    }
}

#[cfg(all(
    not(feature = "host-test"),
    target_os = "none",
    target_arch = "aarch64"
))]
mod imp {
    #[inline(always)]
    pub fn cpu_area_header_link_address() -> usize {
        let address: usize;
        // SAFETY: the fixed prefix lies in the low per-CPU link-time range.
        unsafe {
            core::arch::asm!(
                "movz {address}, #:abs_g0_nc:{prefix}",
                "movk {address}, #:abs_g1_nc:{prefix}",
                "movk {address}, #:abs_g2_nc:{prefix}",
                "movk {address}, #:abs_g3:{prefix}",
                address = out(reg) address,
                prefix = sym crate::__AX_CPU_AREA_PREFIX,
                options(nostack, preserves_flags),
            );
        }
        address
    }

    #[inline(always)]
    pub fn cpu_area_template_end_link_address() -> usize {
        let address: usize;
        // SAFETY: these absolute relocations materialize only a linker integer.
        unsafe {
            core::arch::asm!(
                "movz {address}, #:abs_g0_nc:{end}",
                "movk {address}, #:abs_g1_nc:{end}",
                "movk {address}, #:abs_g2_nc:{end}",
                "movk {address}, #:abs_g3:{end}",
                address = out(reg) address,
                end = sym crate::__AX_CPU_AREA_TEMPLATE_END,
                options(nostack, preserves_flags),
            );
        }
        address
    }
}

#[cfg(all(not(feature = "host-test"), target_os = "none", target_arch = "arm"))]
mod imp {
    #[inline(always)]
    pub fn cpu_area_header_link_address() -> usize {
        let address: u32;
        // SAFETY: these relocations only materialize a linker integer.
        unsafe {
            core::arch::asm!(
                "movw {address}, #:lower16:{prefix}",
                "movt {address}, #:upper16:{prefix}",
                address = out(reg) address,
                prefix = sym crate::__AX_CPU_AREA_PREFIX,
                options(nostack, preserves_flags),
            );
        }
        address as usize
    }

    #[inline(always)]
    pub fn cpu_area_template_end_link_address() -> usize {
        let address: u32;
        // SAFETY: these relocations only materialize a linker integer.
        unsafe {
            core::arch::asm!(
                "movw {address}, #:lower16:{end}",
                "movt {address}, #:upper16:{end}",
                address = out(reg) address,
                end = sym crate::__AX_CPU_AREA_TEMPLATE_END,
                options(nostack, preserves_flags),
            );
        }
        address as usize
    }
}

#[cfg(all(
    not(feature = "host-test"),
    target_os = "none",
    any(target_arch = "riscv32", target_arch = "riscv64")
))]
mod imp {
    #[inline(always)]
    pub fn cpu_area_header_link_address() -> usize {
        let address: usize;
        // SAFETY: this absolute sequence does not use the global-pointer
        // register and does not dereference the linker integer.
        unsafe {
            core::arch::asm!(
                "lui {address}, %highest({prefix})",
                "addi {address}, {address}, %higher({prefix})",
                "slli {address}, {address}, 12",
                "addi {address}, {address}, %hi({prefix})",
                "slli {address}, {address}, 12",
                "addi {address}, {address}, %lo({prefix})",
                address = out(reg) address,
                prefix = sym crate::__AX_CPU_AREA_PREFIX,
                options(nostack),
            );
        }
        address
    }

    #[inline(always)]
    pub fn cpu_area_template_end_link_address() -> usize {
        let address: usize;
        // SAFETY: this absolute sequence does not use the global-pointer
        // register and does not dereference the linker integer.
        unsafe {
            core::arch::asm!(
                "lui {address}, %highest({end})",
                "addi {address}, {address}, %higher({end})",
                "slli {address}, {address}, 12",
                "addi {address}, {address}, %hi({end})",
                "slli {address}, {address}, 12",
                "addi {address}, {address}, %lo({end})",
                address = out(reg) address,
                end = sym crate::__AX_CPU_AREA_TEMPLATE_END,
                options(nostack),
            );
        }
        address
    }
}

#[cfg(all(
    not(feature = "host-test"),
    target_os = "none",
    target_arch = "loongarch64"
))]
mod imp {
    #[inline(always)]
    pub fn cpu_area_header_link_address() -> usize {
        let address: usize;
        // SAFETY: this absolute sequence does not access memory.
        unsafe {
            core::arch::asm!(
                "lu12i.w {address}, %abs_hi20({prefix})",
                "ori {address}, {address}, %abs_lo12({prefix})",
                "lu32i.d {address}, %abs64_lo20({prefix})",
                "lu52i.d {address}, {address}, %abs64_hi12({prefix})",
                address = out(reg) address,
                prefix = sym crate::__AX_CPU_AREA_PREFIX,
                options(nostack),
            );
        }
        address
    }

    #[inline(always)]
    pub fn cpu_area_template_end_link_address() -> usize {
        let address: usize;
        // SAFETY: this absolute sequence does not access memory.
        unsafe {
            core::arch::asm!(
                "lu12i.w {address}, %abs_hi20({end})",
                "ori {address}, {address}, %abs_lo12({end})",
                "lu32i.d {address}, %abs64_lo20({end})",
                "lu52i.d {address}, {address}, %abs64_hi12({end})",
                address = out(reg) address,
                end = sym crate::__AX_CPU_AREA_TEMPLATE_END,
                options(nostack),
            );
        }
        address
    }
}
