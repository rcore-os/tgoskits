use page_table_generic::{PageTableEntry, TableMeta};
use tock_registers::{interfaces::*, register_bitfields, registers::ReadWrite};

use crate::mem::{MemAttributes, PteConfig};

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
            RESERVED = 0b01,
            OUTER = 0b10,
            INNER = 0b11
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

    /// 创建空页表项
    pub const fn empty() -> Self {
        Self(0)
    }
}

impl PageTableEntry for Entry {
    type PteConfig = PteConfig;

    fn new_page(
        paddr: page_table_generic::PhysAddr,
        config: Self::PteConfig,
        is_huge: bool,
    ) -> Self {
        let entry = Entry::empty();
        let mut val = PTE::VALID::SET;

        if config.read {
            val += PTE::AF::SET;
        }

        val += PTE::PHYS_ADDR.val((paddr.as_usize() as u64) >> 12);

        // 设置大页标志（NON_BLOCK=0 表示大页）
        if !is_huge {
            val += PTE::NON_BLOCK::SET;
        }

        if !config.writable {
            val += PTE::AP_RO::SET;
        }

        #[cfg(not(feature = "hv"))]
        {
            if config.lower {
                val += PTE::AP_EL0::SET + PTE::PXN::SET;
                if !config.executable {
                    val += PTE::UXN::SET;
                }
            } else {
                val += PTE::UXN::SET;
                if !config.executable {
                    val += PTE::PXN::SET;
                }
            }
        }
        #[cfg(feature = "hv")]
        {
            // 在虚拟化环境下，内核页表项对 EL2 可执行
            if !config.executable {
                val += PTE::PXN::SET;
            }
        }

        // 设置可执行标志（PXN=0 表示可执行）

        // 设置全局标志（NG=0 表示全局）
        if !config.global {
            val += PTE::NG::SET;
        }

        // 设置脏位（复用 AF 位）
        if config.dirty {
            val += PTE::AF::SET;
        }

        // 设置内存属性
        match config.mem_attr {
            MemAttributes::Device => {
                val += PTE::MAIR.val(0) + PTE::SHAREABLE::OUTER;
            }
            MemAttributes::Normal | MemAttributes::PerCpu => {
                // CPU-local areas have a second virtual alias, but they remain
                // ordinary coherent RAM: remote wake, migration, allocator,
                // and diagnostic paths access another CPU's area directly.
                // Both aliases therefore need the exact same cacheability and
                // shareability attributes.
                val += PTE::MAIR.val(1) + PTE::SHAREABLE::INNER;
            }
            MemAttributes::Uncached => {
                val += PTE::MAIR.val(2) + PTE::SHAREABLE::OUTER;
            }
        }
        entry.as_typed().write(val);
        entry
    }

    fn new_table(paddr: page_table_generic::PhysAddr) -> Self {
        let entry = Entry::empty();
        entry.as_typed().write(
            PTE::VALID::SET
                + PTE::NON_BLOCK::SET
                + PTE::PHYS_ADDR.val((paddr.as_usize() as u64) >> 12),
        );
        entry
    }

    fn paddr(&self, _is_dir: bool) -> page_table_generic::PhysAddr {
        ((self.as_typed().read(PTE::PHYS_ADDR) << 12) as usize).into()
    }

    fn config(&self, _is_dir: bool) -> Self::PteConfig {
        let pte = self.as_typed();
        let lower;
        let executable;
        #[cfg(not(feature = "hv"))]
        {
            lower = pte.is_set(PTE::AP_EL0);
            if lower {
                executable = !pte.is_set(PTE::UXN);
            } else {
                executable = !pte.is_set(PTE::PXN);
            }
        }
        #[cfg(feature = "hv")]
        {
            lower = pte.is_set(PTE::AP_EL0);
            executable = !pte.is_set(PTE::PXN);
        }

        PteConfig {
            read: pte.is_set(PTE::AF),
            writable: pte.is_set(PTE::AP_RO),
            executable,
            lower,
            dirty: pte.is_set(PTE::AF),
            global: !pte.is_set(PTE::NG),
            mem_attr: {
                match pte.read(PTE::MAIR) {
                    0 => MemAttributes::Device,
                    1 => MemAttributes::Normal,
                    2 => MemAttributes::Uncached,
                    _ => MemAttributes::Normal,
                }
            },
        }
    }

    fn present(&self) -> bool {
        self.as_typed().is_set(PTE::VALID)
    }

    fn huge(&self, is_dir: bool) -> bool {
        is_dir && !self.as_typed().is_set(PTE::NON_BLOCK)
    }

    fn unused(&self) -> bool {
        self.0 == 0
    }

    fn clear(&mut self) {
        self.0 = 0;
    }
}

impl core::fmt::Debug for Entry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Debug 输出默认使用页表项格式（is_dir=false）
        write!(f, "PTE {:?}", PageTableEntry::paddr(self, false))
    }
}

#[cfg(page_size_4k)]
#[derive(Clone, Copy)]
pub struct Generic;

impl TableMeta for Generic {
    type P = Entry;

    const PAGE_SIZE: usize = 0x1000;

    const LEVEL_BITS: &'static [usize] = &[9, 9, 9, 9];

    const MAX_BLOCK_LEVEL: usize = 3;

    fn flush(vaddr: Option<page_table_generic::VirtAddr>) {
        super::super::elx::flush_tlb(vaddr);
    }
}
