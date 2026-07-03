use page_table_generic::{MemAttributes, PageTableEntry, TableMeta};
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

    /// 创建空页表项
    pub const fn empty() -> Self {
        Self(0)
    }
}

impl PageTableEntry for Entry {
    fn from_config(config: page_table_generic::PteConfig) -> Self {
        let entry = Entry::empty();
        if !config.valid {
            return entry;
        }

        let mut val = PTE::VALID::SET;

        if config.read {
            val += PTE::AF::SET;
        }

        val += PTE::PHYS_ADDR.val((config.paddr.raw() as u64) >> 12);

        // 设置大页标志（NON_BLOCK=0 表示大页）
        if !config.huge {
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
                val += PTE::MAIR.val(1);
                if matches!(config.mem_attr, MemAttributes::PerCpu) {
                    val += PTE::SHAREABLE::NON;
                } else {
                    val += PTE::SHAREABLE::OUTER;
                }
            }
            MemAttributes::Uncached => {
                val += PTE::MAIR.val(2) + PTE::SHAREABLE::OUTER;
            }
        }
        entry.as_typed().write(val);
        entry
    }

    fn to_config(&self, is_dir: bool) -> page_table_generic::PteConfig {
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

        page_table_generic::PteConfig {
            paddr: ((pte.read(PTE::PHYS_ADDR) << 12) as usize).into(),
            valid: pte.is_set(PTE::VALID),
            read: pte.is_set(PTE::AF),
            writable: pte.is_set(PTE::AP_RO),
            executable,
            lower,
            dirty: pte.is_set(PTE::AF),
            global: !pte.is_set(PTE::NG),
            is_dir,
            huge: !pte.is_set(PTE::NON_BLOCK),
            mem_attr: {
                let mut attr = match pte.read(PTE::MAIR) {
                    0 => MemAttributes::Device,
                    1 => MemAttributes::Normal,
                    2 => MemAttributes::Uncached,
                    _ => MemAttributes::Normal,
                };

                match pte.read_as_enum(PTE::SHAREABLE) {
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

impl TableMeta for Generic {
    type P = Entry;

    const PAGE_SIZE: usize = 0x1000;

    const LEVEL_BITS: &'static [usize] = &[9, 9, 9, 9];

    const MAX_BLOCK_LEVEL: usize = 3;

    fn flush(vaddr: Option<page_table_generic::VirtAddr>) {
        super::super::elx::flush_tlb(vaddr);
    }
}
