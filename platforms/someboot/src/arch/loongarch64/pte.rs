//! LoongArch64 页表项 (Page Table Entry)
//!
//! 使用 tock-registers 风格定义页表项，提供类型安全的寄存器访问
//! 参考: LoongArch64 参考手册 Vol. 1 - 5.4.2 节

use core::fmt::Debug;

use page_table_generic::{MemAttributes, PageTableEntry};
use tock_registers::{interfaces::*, register_bitfields, registers::*};

// LoongArch64 页表项寄存器位域定义

register_bitfields![u64,
    /// LoongArch64 单页页表项 (Page Table Entry)
    ///
    /// 布局参考 LoongArch64 参考手册 5.4.2 节
    /// 注意: 目录项非大页，除物理地址外，其他位都必须为 0
    PTE_DIR [
        /// V - 有效位 (bit 0)
        VALID OFFSET(0) NUMBITS(1) [],

        /// D - 脏位 (bit 1)
        DIRTY OFFSET(1) NUMBITS(1) [],

        /// PLV - 特权级 (bits 2-3)
        PLV OFFSET(2) NUMBITS(2) [
            PLV0 = 0b00,  // 内核态
            PLV1 = 0b01,  // 特权级1
            PLV2 = 0b10,  // 特权级2
            PLV3 = 0b11   // 用户态
        ],

        /// 缓存属性 (bits 4-5)
        CACHE OFFSET(4) NUMBITS(2) [
            SUC = 0b00,  // 强序非缓存 (Strongly-ordered UnCached)
            CC  = 0b01,  // 一致性缓存 (Coherent Cached)
            WUC = 0b10   // 弱序非缓存 (Weakly-ordered UnCached)
        ],

        /// H - 大页位 (bit 6)
        H OFFSET(6) NUMBITS(1) [],

        /// P - 存在位 (bit 7)
        PRESENT OFFSET(7) NUMBITS(1) [],

        /// W - 写位 (bit 8)
        WRITE OFFSET(8) NUMBITS(1) [],

        G OFFSET(12) NUMBITS(1) [],

        PHYS_ADDR OFFSET(12) NUMBITS(40) [],

        /// NR - 禁止读位 (bit 61)
        NO_READ OFFSET(61) NUMBITS(1) [],

        /// NX - 禁止执行位 (bit 62)
        NO_EXEC OFFSET(62) NUMBITS(1) [],

        /// RPLV (bit 63)
        RPLV OFFSET(63) NUMBITS(1) [],
    ],
    /// LoongArch64 单页页表项 (Page Table Entry)
    ///
    /// 布局参考 LoongArch64 参考手册 5.4.2 节
    PTE [
        /// V - 有效位 (bit 0)
        VALID OFFSET(0) NUMBITS(1) [],

        /// D - 脏位 (bit 1)
        DIRTY OFFSET(1) NUMBITS(1) [],

        /// PLV - 特权级 (bits 2-3)
        PLV OFFSET(2) NUMBITS(2) [
            PLV0 = 0b00,  // 内核态
            PLV1 = 0b01,  // 特权级1
            PLV2 = 0b10,  // 特权级2
            PLV3 = 0b11   // 用户态
        ],

        /// 缓存属性 (bits 4-5)
        CACHE OFFSET(4) NUMBITS(2) [
            SUC = 0b00,  // 强序非缓存 (Strongly-ordered UnCached)
            CC  = 0b01,  // 一致性缓存 (Coherent Cached)
            WUC = 0b10   // 弱序非缓存 (Weakly-ordered UnCached)
        ],

        /// G - 全局位 (bit 6)
        G OFFSET(6) NUMBITS(1) [],

        /// P - 存在位 (bit 7)
        PRESENT OFFSET(7) NUMBITS(1) [],

        /// W - 写位 (bit 8)
        WRITE OFFSET(8) NUMBITS(1) [],

        /// 物理页帧号 (bits 12-51)
        /// 注意: 根据 PDF, PPN 占据 bits [51:12]
        PHYS_ADDR OFFSET(12) NUMBITS(40) [],

        /// NR - 禁止读位 (bit 61)
        NO_READ OFFSET(61) NUMBITS(1) [],

        /// NX - 禁止执行位 (bit 62)
        NO_EXEC OFFSET(62) NUMBITS(1) [],

        /// RPLV (bit 63)
        RPLV OFFSET(63) NUMBITS(1) [],
    ],
];

/// 页表项寄存器类型别名
type PteRegister = ReadWrite<u64, PTE::Register>;

/// LoongArch64 页表项
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct Entry(u64);

impl Entry {
    #[inline(always)]
    fn as_base(&self) -> &PteRegister {
        unsafe { &*(self as *const Self as *const PteRegister) }
    }

    #[inline(always)]
    fn as_dir(&self) -> &ReadWrite<u64, PTE_DIR::Register> {
        unsafe { &*(self as *const Self as *const _) }
    }

    /// 创建空页表项
    pub const fn empty() -> Self {
        Self(0)
    }

    #[allow(dead_code)]
    pub(crate) fn debug(
        &self,
        is_dir: bool,
        f: &mut core::fmt::Formatter<'_>,
    ) -> core::fmt::Result {
        if is_dir {
            self.as_dir().debug().fmt(f)
        } else {
            self.as_base().debug().fmt(f)
        }
    }

    fn from_huge(config: page_table_generic::PteConfig) -> u64 {
        let mut val = PTE_DIR::H::SET;
        if config.valid {
            val = val + PTE_DIR::VALID::SET + PTE_DIR::PRESENT::SET;
        }

        if !config.read {
            val += PTE_DIR::NO_READ::SET;
        }

        // 设置可写标志和脏位
        if config.writable {
            val += PTE_DIR::WRITE::SET + PTE_DIR::DIRTY::SET;
        }

        // 设置可执行标志
        if !config.executable {
            val += PTE_DIR::NO_EXEC::SET;
        }

        // 设置用户访问标志（PLV3 表示用户态）
        val += if config.lower {
            PTE_DIR::PLV::PLV3
        } else {
            PTE_DIR::PLV::PLV0
        };

        // 设置物理地址
        let ppn = (config.paddr.raw() as u64) >> 12;
        val += PTE_DIR::PHYS_ADDR.val(ppn);

        if config.global {
            val += PTE_DIR::G::SET;
        }

        // 设置内存属性
        val += match config.mem_attr {
            MemAttributes::Device => PTE_DIR::CACHE::SUC, // SUC
            MemAttributes::Normal | MemAttributes::PerCpu => PTE_DIR::CACHE::CC, // CC
            MemAttributes::Uncached => PTE_DIR::CACHE::WUC, // WUC
        };

        val.value
    }

    fn from_dir(config: page_table_generic::PteConfig) -> u64 {
        let paddr = config.paddr.raw();
        PTE_DIR::PHYS_ADDR.val((paddr >> 12) as u64).value
    }

    fn from_base(config: page_table_generic::PteConfig) -> u64 {
        let mut val = PTE::VALID::CLEAR;

        // 设置有效位和存在位
        if config.valid {
            val = PTE::VALID::SET + PTE::PRESENT::SET;
        }
        if !config.read {
            val += PTE::NO_READ::SET;
        }

        // 设置可写标志和脏位
        if config.writable {
            val += PTE::WRITE::SET + PTE::DIRTY::SET;
        }

        // 设置可执行标志
        if !config.executable {
            val += PTE::NO_EXEC::SET;
        }

        // 设置用户访问标志（PLV3 表示用户态）
        val += if config.lower {
            PTE::PLV::PLV3
        } else {
            PTE::PLV::PLV0
        };

        // 设置物理地址
        let ppn = (config.paddr.raw() as u64) >> 12;
        val += PTE::PHYS_ADDR.val(ppn);

        // 设置全局标志（页表项使用 G 位，bit 6）
        if config.global {
            val += PTE::G::SET;
        }

        // 设置内存属性
        val += match config.mem_attr {
            MemAttributes::Device => PTE::CACHE::SUC, // SUC
            MemAttributes::Normal | MemAttributes::PerCpu => PTE::CACHE::CC, // CC
            MemAttributes::Uncached => PTE::CACHE::WUC, // WUC
        };

        val.value
    }
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
pub(crate) struct EntryDebug(Entry, bool);

impl Debug for EntryDebug {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.debug(self.1, f)
    }
}

impl PageTableEntry for Entry {
    fn from_config(config: page_table_generic::PteConfig) -> Self {
        let val = if config.is_dir {
            if config.huge {
                Self::from_huge(config)
            } else {
                Self::from_dir(config)
            }
        } else {
            Self::from_base(config)
        };
        Self(val)
    }

    fn to_config(&self, is_dir: bool) -> page_table_generic::PteConfig {
        let valid = self.as_base().is_set(PTE::VALID);
        let mut paddr = self.as_base().read(PTE::PHYS_ADDR) << 12;

        // 检查是否为大页（仅目录项）
        let huge = if is_dir {
            self.as_dir().is_set(PTE_DIR::H)
        } else {
            false
        };

        if huge {
            paddr &= !0x1FFF;
        }

        let global = if huge {
            self.as_dir().is_set(PTE_DIR::G)
        } else {
            self.as_base().is_set(PTE::G)
        };

        // 内存属性
        let mem_attr = match self.as_base().read_as_enum(PTE::CACHE) {
            Some(PTE::CACHE::Value::SUC) => MemAttributes::Device,
            Some(PTE::CACHE::Value::CC) => MemAttributes::Normal,
            Some(PTE::CACHE::Value::WUC) => MemAttributes::Uncached,
            _ => MemAttributes::Normal,
        };

        page_table_generic::PteConfig {
            paddr: paddr.into(),
            valid,
            read: valid, // LoongArch64: 假设有效项可读
            writable: self.as_base().is_set(PTE::WRITE),
            executable: !self.as_base().is_set(PTE::NO_EXEC),
            lower: matches!(
                self.as_base().read_as_enum(PTE::PLV),
                Some(PTE::PLV::Value::PLV3)
            ),
            dirty: self.as_base().is_set(PTE::DIRTY),
            global,
            is_dir,
            huge,
            mem_attr,
        }
    }

    fn valid(&self) -> bool {
        self.as_base().is_set(PTE::VALID)
    }
}

impl core::fmt::Debug for Entry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let d = self.as_base().debug();
        d.fmt(f)
    }
}
