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
    fn from_config(config: page_table_generic::PteConfig) -> Self {
        let mut entry = Self::empty();

        // 设置有效位
        if config.valid {
            entry.as_typed().modify(PTE::VALID::SET);
        } else {
            entry.as_typed().modify(PTE::VALID::CLEAR);
        }

        // 设置访问标志位（对应 read 标志）
        if config.read {
            entry.as_typed().modify(PTE::AF::SET);
        } else {
            entry.as_typed().modify(PTE::AF::CLEAR);
        }

        // 设置物理地址（AArch64 目录项和页表项地址布局相同）
        entry
            .as_typed()
            .modify(PTE::PHYS_ADDR.val(config.paddr.raw() as u64 >> 12));

        // 设置大页标志（NON_BLOCK=0 表示大页）
        if config.huge && config.is_dir {
            entry.as_typed().modify(PTE::NON_BLOCK::CLEAR);
        } else {
            entry.as_typed().modify(PTE::NON_BLOCK::SET);
        }

        // 设置可写标志（AP_RO=0 表示可写）
        if config.writable {
            entry.as_typed().modify(PTE::AP_RO::CLEAR);
        } else {
            entry.as_typed().modify(PTE::AP_RO::SET);
        }

        // 设置可执行标志（PXN=0 表示可执行）
        if config.executable {
            entry.as_typed().modify(PTE::PXN::CLEAR + PTE::UXN::CLEAR);
        } else {
            entry.as_typed().modify(PTE::PXN::SET + PTE::UXN::SET);
        }

        // 设置用户访问标志（AP_EL0=1 表示用户可访问）
        if config.lower {
            entry.as_typed().modify(PTE::AP_EL0::SET);
        } else {
            entry.as_typed().modify(PTE::AP_EL0::CLEAR);
        }

        // 设置全局标志（NG=0 表示全局）
        if config.global {
            entry.as_typed().modify(PTE::NG::CLEAR);
        } else {
            entry.as_typed().modify(PTE::NG::SET);
        }

        // 设置脏位（复用 AF 位）
        if config.dirty {
            entry.as_typed().modify(PTE::AF::SET);
        } else {
            entry.as_typed().modify(PTE::AF::CLEAR);
        }

        // 设置内存属性
        match config.mem_attr {
            MemAttributes::Device => {
                entry.set_mair_idx(1);
                entry.as_typed().modify(PTE::SHAREABLE::OUTER);
            }
            MemAttributes::Normal | MemAttributes::PerCpu => {
                entry.set_mair_idx(0);
                if matches!(config.mem_attr, MemAttributes::PerCpu) {
                    entry.as_typed().modify(PTE::SHAREABLE::NON);
                } else {
                    entry.as_typed().modify(PTE::SHAREABLE::OUTER);
                }
            }
            MemAttributes::Uncached => {
                entry.set_mair_idx(2);
                entry.as_typed().modify(PTE::SHAREABLE::OUTER);
            }
        }

        entry
    }

    fn to_config(&self, is_dir: bool) -> page_table_generic::PteConfig {
        page_table_generic::PteConfig {
            paddr: ((self.as_typed().read(PTE::PHYS_ADDR) << 12) as usize).into(),
            valid: self.as_typed().is_set(PTE::VALID),
            read: self.as_typed().is_set(PTE::AF),
            writable: !self.as_typed().is_set(PTE::AP_RO),
            executable: !self.as_typed().is_set(PTE::PXN),
            lower: self.as_typed().is_set(PTE::AP_EL0),
            dirty: self.as_typed().is_set(PTE::AF),
            global: !self.as_typed().is_set(PTE::NG),
            is_dir,
            huge: !self.as_typed().is_set(PTE::NON_BLOCK),
            mem_attr: {
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
            },
        }
    }

    fn valid(&self) -> bool {
        self.as_typed().is_set(PTE::VALID)
    }
}

impl core::fmt::Debug for Entry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Debug 输出默认使用页表项格式（is_dir=false）
        let config = self.to_config(false);
        write!(f, "PTE {:?}", config.paddr)
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
