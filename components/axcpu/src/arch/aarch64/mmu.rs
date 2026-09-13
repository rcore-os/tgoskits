// SPDX-License-Identifier: MPL-2.0
// Stage-one setup migrated from someboot/src/arch/aarch64/el1 and el2.
// Original package author: 周睿 <zrufo747@outlook.com>.
//! Explicit AArch64 translation and exception regimes.

/// EL1 stage-one translation and native exception registers.
#[derive(Clone, Copy, Debug, Default)]
pub struct El1;

/// Non-VHE EL2 stage-one translation and native exception registers.
/// Guest stage-two translation is a separate VTTBR/VTCR capability.
#[derive(Clone, Copy, Debug, Default)]
pub struct El2;

impl El1 {
    /// Invalidates an EL1 stage-one translation on this CPU, across all ASIDs.
    pub fn flush_tlb(address: Option<crate::VirtAddr>) {
        // SAFETY: these privileged operations touch only local translation
        // caches; table stores precede invalidation and completion precedes fetch.
        unsafe {
            if let Some(address) = address {
                let operand = (address.as_usize() >> 12) & ((1usize << 44) - 1);
                core::arch::asm!("dsb nshst; tlbi vaae1, {}; dsb nsh; isb", in(reg) operand);
            } else {
                core::arch::asm!("dsb nshst; tlbi vmalle1; dsb nsh; isb");
            }
        }
    }
}

impl El2 {
    /// Invalidates a non-VHE EL2 stage-one translation on this CPU.
    /// This does not invalidate guest stage-two translations.
    pub fn flush_tlb(address: Option<crate::VirtAddr>) {
        // SAFETY: local EL2 translation maintenance has the same publication
        // and completion ordering as EL1 but names its own translation regime.
        unsafe {
            if let Some(address) = address {
                let operand = (address.as_usize() >> 12) & ((1usize << 44) - 1);
                core::arch::asm!("dsb nshst; tlbi vae2, {}; dsb nsh; isb", in(reg) operand);
            } else {
                core::arch::asm!("dsb nshst; tlbi alle2; dsb nsh; isb");
            }
        }
    }
}

impl El1 {
    /// Returns the EL1 kernel's TTBR1 base, excluding its ASID field.
    pub fn read_kernel_page_table() -> crate::PhysAddr {
        use aarch64_cpu::registers::{Readable, TTBR1_EL1};
        crate::PhysAddr::from_usize((TTBR1_EL1.get() & 0x0000_ffff_ffff_f000) as usize)
    }
    /// Replaces the EL1 kernel root without invalidating cached translations.
    ///
    /// # Safety
    /// The caller must retain the new tables and all current code, stack and
    /// data mappings at EL1, and perform required translation synchronization.
    pub unsafe fn write_kernel_page_table(root: crate::PhysAddr) {
        use aarch64_cpu::registers::{TTBR1_EL1, Writeable};
        TTBR1_EL1.set(root.as_usize() as u64);
    }
    /// Returns the configured EL1 ASID capacity.
    pub fn address_space_tag_capacity() -> u32 {
        super::asm::address_space_tag_capacity()
    }
    /// Invalidates all EL1 translations intersecting a local byte range.
    pub fn flush_tlb_range(start: crate::VirtAddr, size: usize) {
        crate::mmu::flush_range_with(start, size, Self::flush_tlb);
    }
}

impl El2 {
    /// Returns the non-VHE EL2 kernel's TTBR0 base.
    pub fn read_kernel_page_table() -> crate::PhysAddr {
        use aarch64_cpu::registers::{Readable, TTBR0_EL2};
        crate::PhysAddr::from_usize((TTBR0_EL2.get() & 0x0000_ffff_ffff_f000) as usize)
    }
    /// Replaces the non-VHE EL2 stage-one root without invalidation.
    ///
    /// # Safety
    /// The caller must retain the tables and all active EL2 code, stack and
    /// data mappings and perform required translation synchronization.
    pub unsafe fn write_kernel_page_table(root: crate::PhysAddr) {
        use aarch64_cpu::registers::{TTBR0_EL2, Writeable};
        TTBR0_EL2.set(root.as_usize() as u64);
    }
    /// Non-VHE EL2 native translations have no userspace ASID allocation.
    pub const fn address_space_tag_capacity() -> u32 {
        1
    }
    /// Invalidates all non-VHE EL2 translations intersecting a local byte range.
    pub fn flush_tlb_range(start: crate::VirtAddr, size: usize) {
        crate::mmu::flush_range_with(start, size, Self::flush_tlb);
    }
}

impl El1 {
    /// Programs the existing four-level, 4-KiB stage-one geometry and MAIR.
    ///
    /// # Safety
    /// The caller must own this CPU with IRQs masked before enabling this
    /// translation regime. Existing translations must not depend on the old
    /// configuration. MAIR slots must agree with every installed descriptor.
    pub unsafe fn configure_stage1(mair: u64) {
        use aarch64_cpu::{asm::barrier, registers::*};
        MAIR_EL1.set(mair);
        // Enable 4-KiB, 48-bit virtual geometry with a supported physical range.
        const VADDR_SIZE: u64 = 48;
        const T0SZ: u64 = 64 - VADDR_SIZE;

        let tcr_flags0 = TCR_EL1::EPD0::EnableTTBR0Walks
            + TCR_EL1::TG0::KiB_4
            + TCR_EL1::SH0::Inner
            + TCR_EL1::ORGN0::WriteBack_ReadAlloc_WriteAlloc_Cacheable
            + TCR_EL1::IRGN0::WriteBack_ReadAlloc_WriteAlloc_Cacheable
            + TCR_EL1::T0SZ.val(T0SZ);
        let tcr_flags1 = TCR_EL1::EPD1::EnableTTBR1Walks
            + TCR_EL1::TG1::KiB_4
            + TCR_EL1::SH1::Inner
            + TCR_EL1::ORGN1::WriteBack_ReadAlloc_WriteAlloc_Cacheable
            + TCR_EL1::IRGN1::WriteBack_ReadAlloc_WriteAlloc_Cacheable
            + TCR_EL1::T1SZ.val(T0SZ);
        // Configure the widest ASID mode implemented by this CPU. Runtime tag
        // allocation observes this TCR field instead of assuming that a hardware
        // capability has already been enabled by the boot path.
        let asid_size = if ID_AA64MMFR0_EL1.read(ID_AA64MMFR0_EL1::ASIDBits) == 2 {
            TCR_EL1::AS::ASID16Bits
        } else {
            TCR_EL1::AS::ASID8Bits
        };
        // The descriptors implemented here encode at most 48 physical bits.
        // Do not request a larger output size than this CPU implements.
        let physical_range = (ID_AA64MMFR0_EL1.get() & 15).min(5);
        TCR_EL1.write(TCR_EL1::IPS.val(physical_range) + asid_size + tcr_flags0 + tcr_flags1);

        Self::flush_tlb(None);
        barrier::dsb(barrier::SY);
        barrier::isb(barrier::SY);
    }
}

impl El1 {
    /// Returns whether this translation regime's MMU is enabled.
    pub fn is_mmu_enabled() -> bool {
        use aarch64_cpu::registers::*;
        SCTLR_EL1.is_set(SCTLR_EL1::M)
    }
    /// Enables translation and native cache access for this regime.
    ///
    /// # Safety
    /// Valid roots, MAIR and geometry must be installed. The new mappings
    /// must retain current code, stack and data until the owner's next handoff.
    pub unsafe fn enable_mmu_and_caches() {
        use aarch64_cpu::{asm::barrier, registers::*};

        SCTLR_EL1.modify(
            SCTLR_EL1::M::Enable
                + SCTLR_EL1::C::Cacheable
                + SCTLR_EL1::I::Cacheable
                + SCTLR_EL1::UCT::DontTrap
                + SCTLR_EL1::DZE::DontTrap
                + SCTLR_EL1::UCI::DontTrap,
        );
        SCTLR_EL1.set(SCTLR_EL1.get() | (1 << 23));
        Self::flush_tlb(None);
        barrier::dsb(barrier::SY);
        barrier::isb(barrier::SY);
    }
}

impl El2 {
    /// Programs the existing four-level, 4-KiB stage-one geometry and MAIR.
    ///
    /// # Safety
    /// The caller must own this CPU with IRQs masked before enabling this
    /// translation regime. Existing translations must not depend on the old
    /// configuration. MAIR slots must agree with every installed descriptor.
    pub unsafe fn configure_stage1(mair: u64) {
        use aarch64_cpu::{asm::barrier, registers::*};
        MAIR_EL2.set(mair);
        // Enable 4-KiB, 48-bit virtual geometry with a supported physical range.
        const VADDR_SIZE: u64 = 48;
        const T0SZ: u64 = 64 - VADDR_SIZE;

        // Note: TCR_EL2 only has one set of translation controls (T0SZ, TG0)
        // TTBR1_EL2 does not exist in ARMv8 architecture
        let tcr_flags0 = TCR_EL2::T0SZ.val(T0SZ)
            + TCR_EL2::TG0::KiB_4
            + TCR_EL2::SH0::Inner
            + TCR_EL2::ORGN0::WriteBack_ReadAlloc_WriteAlloc_Cacheable
            + TCR_EL2::IRGN0::WriteBack_ReadAlloc_WriteAlloc_Cacheable;

        let physical_range = (ID_AA64MMFR0_EL1.get() & 15).min(5);
        TCR_EL2.write(TCR_EL2::PS.val(physical_range) + tcr_flags0);

        Self::flush_tlb_inner_shareable(None);
        barrier::dsb(barrier::SY);
        barrier::isb(barrier::SY);
    }
}

impl El2 {
    /// Returns whether this translation regime's MMU is enabled.
    pub fn is_mmu_enabled() -> bool {
        use aarch64_cpu::registers::*;
        SCTLR_EL2.is_set(SCTLR_EL2::M)
    }
    /// Enables translation and native cache access for this regime.
    ///
    /// # Safety
    /// Valid roots, MAIR and geometry must be installed. The new mappings
    /// must retain current code, stack and data until the owner's next handoff.
    pub unsafe fn enable_mmu_and_caches() {
        use aarch64_cpu::{asm::barrier, registers::*};

        SCTLR_EL2.modify(SCTLR_EL2::M::Enable + SCTLR_EL2::C::Cacheable + SCTLR_EL2::I::Cacheable);
        Self::flush_tlb(None);
        barrier::dsb(barrier::SY);
        barrier::isb(barrier::SY);
    }
}

impl El1 {
    /// Invalidates stage-one translations throughout the inner-shareable domain.
    pub fn flush_tlb_inner_shareable(address: Option<crate::VirtAddr>) {
        // SAFETY: publish table stores before the broadcast operation and wait
        // for all affected walks before permitting subsequent instruction fetch.
        unsafe {
            if let Some(address) = address {
                let operand = (address.as_usize() >> 12) & ((1usize << 44) - 1);
                core::arch::asm!("dsb ishst; tlbi vaae1is, {}; dsb ish; isb", in(reg) operand);
            } else {
                core::arch::asm!("dsb ishst; tlbi vmalle1is; dsb ish; isb");
            }
        }
    }
}

impl El2 {
    /// Invalidates stage-one translations throughout the inner-shareable domain.
    pub fn flush_tlb_inner_shareable(address: Option<crate::VirtAddr>) {
        // SAFETY: publish table stores before the broadcast operation and wait
        // for all affected walks before permitting subsequent instruction fetch.
        unsafe {
            if let Some(address) = address {
                let operand = (address.as_usize() >> 12) & ((1usize << 44) - 1);
                core::arch::asm!("dsb ishst; tlbi vae2is, {}; dsb ish; isb", in(reg) operand);
            } else {
                core::arch::asm!("dsb ishst; tlbi alle2is; dsb ish; isb");
            }
        }
    }
}

fn decode_ttbr(value: u64) -> crate::mmu::HardwareAddressSpace {
    crate::mmu::HardwareAddressSpace::new(
        crate::PhysAddr::from_usize((value & 0x0000_ffff_ffff_f000) as usize),
        (value >> 48) as u16,
    )
}

fn encode_ttbr(space: crate::mmu::HardwareAddressSpace) -> u64 {
    space.root().as_usize() as u64 | (u64::from(space.hardware_tag()) << 48)
}

impl El1 {
    /// Reads the kernel TTBR1 base and ASID together.
    pub fn read_kernel_address_space() -> crate::mmu::HardwareAddressSpace {
        use aarch64_cpu::registers::*;
        decode_ttbr(TTBR1_EL1.get())
    }
    /// Reads the lower-address TTBR0 base and ASID together.
    pub fn read_user_address_space() -> crate::mmu::HardwareAddressSpace {
        use aarch64_cpu::registers::*;
        decode_ttbr(TTBR0_EL1.get())
    }
    /// Installs a kernel TTBR1 root and ASID without implicit invalidation.
    ///
    /// # Safety
    /// The aligned root and supported ASID must remain owned while active.
    /// Current mappings must survive the write; the owner arranges TLB ordering.
    pub unsafe fn write_kernel_address_space(space: crate::mmu::HardwareAddressSpace) {
        use aarch64_cpu::registers::*;
        TTBR1_EL1.set(encode_ttbr(space));
    }
    /// Installs a lower-address TTBR0 root and ASID without implicit invalidation.
    ///
    /// # Safety
    /// The aligned root and supported ASID must remain owned while active.
    /// No in-flight lower-address access may use the retired mapping.
    pub unsafe fn write_user_address_space(space: crate::mmu::HardwareAddressSpace) {
        use aarch64_cpu::registers::*;
        TTBR0_EL1.set(encode_ttbr(space));
    }
}
