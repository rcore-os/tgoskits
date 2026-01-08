use page_table_generic::{MemAttributes, PageTableEntry, TableGeneric};

use tock_registers::{interfaces::*, register_bitfields, registers::ReadWrite};

register_bitfields![u64,
    /// 4k 48-bit
    PTE [
        VALID OFFSET(0) NUMBITS(1) [],
        NON_BLOCK OFFSET(1) NUMBITS(1) [],
        MAIR OFFSET(2) NUMBITS(3) [],
        NS OFFSET(5) NUMBITS(1) [],
        AP_EL0 OFFSET(6) NUMBITS(1) [],
        AP_RO OFFSET(7) NUMBITS(1) [],
        SHAREABLE OFFSET(8) NUMBITS(2) [
            NON = 0b00,
            INNER = 0b01,
            OUTER = 0b10,
            RESERVED = 0b11
        ],
        AF OFFSET(10) NUMBITS(1) [],
        NG OFFSET(11) NUMBITS(1) [],
        PHYS_ADDR OFFSET(12) NUMBITS(36) [],
        CONTIGUOUS OFFSET(52) NUMBITS(1) [],
        PXN OFFSET(53) NUMBITS(1) [],
        UXN OFFSET(54) NUMBITS(1) [],
        PXN_TABLE OFFSET(59) NUMBITS(1) [],
        XN_TABLE OFFSET(60) NUMBITS(1) [],
        AP_NO_EL0_TABLE OFFSET(61) NUMBITS(1) [],
        AP_NO_WRITE_TABLE OFFSET(62) NUMBITS(1) [],
        NS_TABLE OFFSET(63) NUMBITS(1) [],
    ],
];

#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct Entry(u64);

impl Entry {
    fn as_typed(&self) -> &ReadWrite<u64, PTE::Register> {
        unsafe { &*(self as *const Self as *const ReadWrite<u64, PTE::Register>) }
    }

    #[inline(always)]
    pub fn set_mair_idx(&mut self, idx: usize) {
        self.as_typed().modify(PTE::MAIR.val(idx as u64));
    }

    /// 创建空页表项
    pub const fn empty() -> Self {
        Self(0)
    }
}

impl PageTableEntry for Entry {
    fn valid(&self) -> bool {
        self.as_typed().is_set(PTE::VALID)
    }

    fn paddr(&self) -> page_table_generic::PhysAddr {
        (self.as_typed().read(PTE::PHYS_ADDR) << 12).into()
    }

    fn set_paddr(&mut self, paddr: page_table_generic::PhysAddr) {
        self.as_typed()
            .modify(PTE::PHYS_ADDR.val(paddr.raw() as u64 >> 12));
    }

    fn set_valid(&mut self, valid: bool) {
        self.as_typed().modify(if valid {
            PTE::VALID::SET
        } else {
            PTE::VALID::CLEAR
        });
    }

    fn is_huge(&self) -> bool {
        !self.as_typed().is_set(PTE::NON_BLOCK)
    }

    fn set_is_huge(&mut self, b: bool) {
        self.as_typed().modify(if b {
            PTE::NON_BLOCK::CLEAR
        } else {
            PTE::NON_BLOCK::SET
        });
    }

    fn new_valid() -> Self {
        let entry = Self::empty();
        entry
            .as_typed()
            .write(PTE::AF::SET + PTE::VALID::SET + PTE::NON_BLOCK::SET + PTE::UXN::SET);
        entry
    }

    fn is_writable(&self) -> bool {
        self.valid() && !self.as_typed().is_set(PTE::AP_RO)
    }

    fn set_writable(&mut self, b: bool) {
        self.as_typed().modify(if b {
            PTE::AP_RO::CLEAR
        } else {
            PTE::AP_RO::SET
        });
    }

    fn is_executable(&self) -> bool {
        self.valid() && !self.as_typed().is_set(PTE::PXN)
    }

    fn set_executable(&mut self, b: bool) {
        if b {
            self.as_typed().modify(PTE::PXN::CLEAR);
        } else {
            self.as_typed().modify(PTE::PXN::SET);
        }
    }

    fn is_lower_access(&self) -> bool {
        self.valid() && self.as_typed().is_set(PTE::AP_EL0)
    }

    fn set_lower_access(&mut self, b: bool) {
        self.as_typed().modify(if b {
            PTE::AP_EL0::SET
        } else {
            PTE::AP_EL0::CLEAR
        });
    }

    fn is_global(&self) -> bool {
        !self.as_typed().is_set(PTE::NG)
    }

    fn set_global(&mut self, b: bool) {
        self.as_typed()
            .modify(if b { PTE::NG::CLEAR } else { PTE::NG::SET });
    }

    fn is_accessed(&self) -> bool {
        self.as_typed().is_set(PTE::AF)
    }

    fn set_accessed(&mut self, b: bool) {
        self.as_typed()
            .modify(if b { PTE::AF::SET } else { PTE::AF::CLEAR });
    }

    fn is_dirty(&self) -> bool {
        self.as_typed().is_set(PTE::AF)
    }

    fn set_dirty(&mut self, b: bool) {
        self.as_typed()
            .modify(if b { PTE::AF::SET } else { PTE::AF::CLEAR });
    }

    fn mem_attr(&self) -> page_table_generic::MemAttributes {
        let mut attr = match self.as_typed().read(PTE::MAIR) {
            0 => MemAttributes::Normal,
            1 => MemAttributes::Device,
            2 => MemAttributes::Uncached,
            _ => MemAttributes::Normal,
        };

        match self.as_typed().read_as_enum(PTE::SHAREABLE) {
            Some(PTE::SHAREABLE::Value::OUTER) => {}
            _ => attr = MemAttributes::PerCpu,
        }

        attr
    }

    fn set_mem_attr(&mut self, attr: page_table_generic::MemAttributes) {
        let idx = match attr {
            MemAttributes::Normal | MemAttributes::PerCpu => 0,
            MemAttributes::Device => 1,
            MemAttributes::Uncached => 2,
        };
        self.set_mair_idx(idx);
        if matches!(attr, MemAttributes::PerCpu) {
            self.as_typed().modify(PTE::SHAREABLE::NON);
        } else {
            self.as_typed().modify(PTE::SHAREABLE::OUTER);
        }
    }
}

impl core::fmt::Debug for Entry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "PTE {:?}", self.paddr())
    }
}

#[cfg(page_size_4k)]
#[derive(Clone, Copy)]
pub struct Generic;

impl TableGeneric for Generic {
    type P = Entry;

    const PAGE_SIZE: usize = 0x1000;

    const LEVEL_BITS: &'static [usize] = &[9, 9, 9, 9];

    const MAX_BLOCK_LEVEL: usize = 3;

    fn flush(vaddr: Option<page_table_generic::VirtAddr>) {
        super::super::elx::flush_tlb(vaddr);
    }
}
