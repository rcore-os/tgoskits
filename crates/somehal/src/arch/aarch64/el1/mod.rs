use aarch64_cpu::{
    asm::{barrier::*, *},
    registers::*,
};
use aarch64_cpu_ext::asm::tlb::*;
use page_table_generic::{PageTableEntry, TableGeneric, VirtAddr};

use crate::arch::entry::el_entry;

pub fn switch_to_elx() {
    unsafe extern "C" {
        fn __cpu0_stack_top();
    }

    SPSel.write(SPSel::SP::ELx);
    SP_EL0.set(0);
    let current_el = CurrentEL.read(CurrentEL::EL);
    if current_el >= 2 {
        let el_entry = sym_addr!(el_entry);
        let sp = sym_addr!(__cpu0_stack_top);

        if current_el == 3 {
            // Set EL2 to 64bit and enable the HVC instruction.
            SCR_EL3.write(
                SCR_EL3::NS::NonSecure + SCR_EL3::HCE::HvcEnabled + SCR_EL3::RW::NextELIsAarch64,
            );
            // Set the return address and exception level.
            SPSR_EL3.write(
                SPSR_EL3::M::EL1h
                    + SPSR_EL3::D::Masked
                    + SPSR_EL3::A::Masked
                    + SPSR_EL3::I::Masked
                    + SPSR_EL3::F::Masked,
            );
            let switch = sym_addr!(switch_to_elx);

            ELR_EL3.set(switch as _);
            SP_EL2.set(sp as _);
            barrier::isb(barrier::SY);
            eret();
        }
        // Disable EL1 timer traps and the timer offset.
        CNTHCTL_EL2.modify(CNTHCTL_EL2::EL1PCEN::SET + CNTHCTL_EL2::EL1PCTEN::SET);
        CNTVOFF_EL2.set(0);
        // Set EL1 to 64bit.
        HCR_EL2.write(HCR_EL2::RW::EL1IsAarch64);
        // Set the return address and exception level.
        SPSR_EL2.write(
            SPSR_EL2::M::EL1h
                + SPSR_EL2::D::Masked
                + SPSR_EL2::A::Masked
                + SPSR_EL2::I::Masked
                + SPSR_EL2::F::Masked,
        );

        ELR_EL2.set(el_entry as _);
        SP_EL1.set(sp as _);
        barrier::isb(barrier::SY);
        eret();
    }

    el_entry();
}

bitflags::bitflags! {
    #[repr(transparent)]
    /// Memory attribute fields in the VMSAv8-64 translation table format descriptors.
    #[derive(Clone, Copy)]
    pub struct PteFlags: usize {
        // Attribute fields in stage 1 VMSAv8-64 Block and Page descriptors:

        /// Whether the descriptor is valid.
        const VALID =       1 << 0;
        /// The descriptor gives the address of the next level of translation table or 4KB page.
        /// (not a 2M, 1G block)
        const NON_BLOCK =   1 << 1;

        /// Non-secure bit. For memory accesses from Secure state, specifies whether the output
        /// address is in Secure or Non-secure memory.
        const NS =          1 << 5;
        /// Access permission: accessable at EL0.
        const AP_EL0 =      1 << 6;
        /// Access permission: read-only.
        const AP_RO =       1 << 7;
        /// Shareability: Inner Shareable (otherwise Outer Shareable).
        const INNER =       1 << 8;
        /// Shareability: Inner or Outer Shareable (otherwise Non-shareable).
        const SHAREABLE =   1 << 9;
        /// The Access flag.
        const AF =          1 << 10;
        /// The not global bit.
        const NG =          1 << 11;
        /// Indicates that 16 adjacent translation table entries point to contiguous memory regions.
        const CONTIGUOUS =  1 <<  52;
        /// The Privileged execute-never field.
        const PXN =         1 <<  53;
        /// The Execute-never or Unprivileged execute-never field.
        const UXN =         1 <<  54;

        // Next-level attributes in stage 1 VMSAv8-64 Table descriptors:

        /// PXN limit for subsequent levels of lookup.
        const PXN_TABLE =           1 << 59;
        /// XN limit for subsequent levels of lookup.
        const XN_TABLE =            1 << 60;
        /// Access permissions limit for subsequent levels of lookup: access at EL0 not permitted.
        const AP_NO_EL0_TABLE =     1 << 61;
        /// Access permissions limit for subsequent levels of lookup: write access not permitted.
        const AP_NO_WRITE_TABLE =   1 << 62;
        /// For memory accesses from Secure state, specifies the Security state for subsequent
        /// levels of lookup.
        const NS_TABLE =            1 << 63;
    }
}

#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct Pte(usize);

impl Pte {
    const PHYS_ADDR_MASK: usize = 0x0000_ffff_ffff_f000; // bits 12..48
    const MAIR_MASK: usize = 0b111 << 2;

    #[inline(always)]
    pub fn as_flags(&self) -> PteFlags {
        PteFlags::from_bits_truncate(self.0)
    }

    #[inline(always)]
    pub fn set_mair_idx(&mut self, idx: usize) {
        self.0 &= !Self::MAIR_MASK;
        self.0 |= idx << 2;
    }

    pub fn new_valid() -> Self {
        let flags = PteFlags::empty()
            | PteFlags::AF
            | PteFlags::VALID
            | PteFlags::NON_BLOCK
            | PteFlags::UXN;

        Self(flags.bits())
    }

    #[allow(unused)]
    pub fn update_flags<F>(&mut self, f: F)
    where
        F: FnOnce(&mut PteFlags),
    {
        let mut flags = self.as_flags();
        f(&mut flags);
        // 保留物理地址和 MAIR 索引，只更新标志位
        let preserved = self.0 & (Self::PHYS_ADDR_MASK | Self::MAIR_MASK);
        self.0 = preserved | flags.bits();
    }

    // pub fn new(cache: CacheKind) -> Self {
    //     let mut flags = PteFlags::empty()
    //         | PteFlags::AF
    //         | PteFlags::VALID
    //         | PteFlags::NON_BLOCK
    //         | PteFlags::UXN;

    //     let idx = match cache {
    //         CacheKind::Device => 0,
    //         CacheKind::Normal => {
    //             flags |= PteFlags::INNER | PteFlags::SHAREABLE;
    //             1
    //         }
    //         CacheKind::NoCache => {
    //             flags |= PteFlags::SHAREABLE;
    //             2
    //         }
    //     };

    //     let mut s = Self(flags.bits());
    //     s.set_mair_idx(idx);
    //     s
    // }
}

impl PageTableEntry for Pte {
    fn valid(&self) -> bool {
        self.as_flags().contains(PteFlags::VALID)
    }

    fn paddr(&self) -> page_table_generic::PhysAddr {
        (self.0 & Self::PHYS_ADDR_MASK).into()
    }

    fn set_paddr(&mut self, paddr: page_table_generic::PhysAddr) {
        self.0 &= !Self::PHYS_ADDR_MASK;
        self.0 |= paddr.raw() & Self::PHYS_ADDR_MASK;
    }

    fn set_valid(&mut self, valid: bool) {
        if valid {
            self.0 |= (PteFlags::empty() | PteFlags::VALID).bits();
        } else {
            self.0 &= !(PteFlags::empty() | PteFlags::VALID).bits();
        }
    }

    fn is_huge(&self) -> bool {
        !self.as_flags().contains(PteFlags::NON_BLOCK)
    }

    fn set_is_huge(&mut self, b: bool) {
        let bits = (PteFlags::empty() | PteFlags::NON_BLOCK).bits();
        if b {
            self.0 &= !bits;
        } else {
            self.0 |= bits;
        }
    }
}

impl core::fmt::Debug for Pte {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "PTE {:?}", self.paddr())
    }
}

#[derive(Clone, Copy)]
pub struct Table;

impl TableGeneric for Table {
    type P = Pte;

    const PAGE_SIZE: usize = 0x1000;

    const LEVEL_BITS: &'static [usize] = &[9, 9, 9, 9];

    const MAX_BLOCK_LEVEL: usize = 3;

    fn flush(_vaddr: Option<page_table_generic::VirtAddr>) {
        dsb(SY);
        isb(SY);
    }
}

#[inline(always)]
fn flush_tlb(vaddr: Option<VirtAddr>) {
    match vaddr {
        Some(addr) => {
            tlbi(VAAE1IS::new(addr.raw()));
        }
        None => {
            tlbi(VMALLE1);
        }
    }
    dsb(SY);
    isb(SY);
}

#[inline(always)]
pub fn setup_table_regs() {
    // Device-nGnRE
    let attr0 = MAIR_EL1::Attr0_Device::nonGathering_nonReordering_EarlyWriteAck;
    // Normal
    let attr1 = MAIR_EL1::Attr1_Normal_Inner::WriteBack_NonTransient_ReadWriteAlloc
        + MAIR_EL1::Attr1_Normal_Outer::WriteBack_NonTransient_ReadWriteAlloc;
    // No cache
    let attr2 =
        MAIR_EL1::Attr2_Normal_Inner::NonCacheable + MAIR_EL1::Attr2_Normal_Outer::NonCacheable;
    // WriteThrough
    let attr3 = MAIR_EL1::Attr3_Normal_Inner::WriteThrough_Transient_WriteAlloc
        + MAIR_EL1::Attr3_Normal_Outer::WriteThrough_Transient_WriteAlloc;

    MAIR_EL1.write(attr0 + attr1 + attr2 + attr3);

    // Enable TTBR0 and TTBR1 walks, page size = 4K, vaddr size = 48 bits, paddr size = 40 bits.
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
    TCR_EL1.write(TCR_EL1::IPS::Bits_48 + tcr_flags0 + tcr_flags1);

    tlbi(VMALLE1);
    barrier::dsb(barrier::SY);
    barrier::isb(barrier::SY);
}

#[inline(always)]
pub fn set_table(addr: usize) {
    TTBR1_EL1.set_baddr(addr as _);
    TTBR0_EL1.set_baddr(addr as _);
    barrier::dsb(barrier::SY);
    barrier::isb(barrier::SY);
}

#[inline(always)]
pub fn setup_sctlr() {
    SCTLR_EL1.modify(SCTLR_EL1::M::Enable + SCTLR_EL1::C::Cacheable + SCTLR_EL1::I::Cacheable);
    flush_tlb(None);
    barrier::dsb(barrier::SY);
    barrier::isb(barrier::SY);
}
