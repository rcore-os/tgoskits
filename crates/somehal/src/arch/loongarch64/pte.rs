//! LoongArch64 页表项 (Page Table Entry)
//!
//! 使用 tock-registers 风格定义页表项，提供类型安全的寄存器访问
//! 参考: LoongArch64 参考手册 Vol. 1 - 5.4.2 节

use core::fmt::Debug;

use loongArch64::register::asid;
use page_table_generic::{MemAttributes, PageTableEntry};
use tock_registers::interfaces::*;
use tock_registers::register_bitfields;
use tock_registers::registers::*;

// LoongArch64 页表项寄存器位域定义
register_bitfields![u64,
    /// LoongArch64 单页页表项 (Page Table Entry)
    ///
    /// 布局参考 LoongArch64 参考手册 5.4.2 节
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

        /// H/G - 共享位（bit 6）
        /// 在目录项中：H=1 表示大页映射
        /// 在页表项中：G=1 表示全局映射（此时 H 必须为 0）
        /// 注意：根据上下文区分是 H 位还是 G 位，不能同时为 1
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

        /// H/G - 共享位（bit 6）
        /// 在目录项中：H=1 表示大页映射
        /// 在页表项中：G=1 表示全局映射（此时 H 必须为 0）
        /// 注意：根据上下文区分是 H 位还是 G 位，不能同时为 1
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
}

#[derive(Clone, Copy)]
pub(crate) struct EntryDebug(Entry, bool);

impl Debug for EntryDebug {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.debug(self.1, f)
    }
}

impl PageTableEntry for Entry {
    fn from_config(config: page_table_generic::PteConfig) -> Self {
        let entry = Self::empty();

        // 设置有效位和存在位
        if config.valid {
            entry.as_base().modify(PTE::VALID::SET + PTE::PRESENT::SET);
        } else {
            entry
                .as_base()
                .modify(PTE::VALID::CLEAR + PTE::PRESENT::CLEAR);
        }

        if config.read {
            entry.as_base().modify(PTE::NO_READ::CLEAR);
        } else {
            entry.as_base().modify(PTE::NO_READ::SET);
        }

        // 设置可写标志和脏位
        if config.writable {
            entry.as_base().modify(PTE::WRITE::SET + PTE::DIRTY::SET);
        } else {
            entry
                .as_base()
                .modify(PTE::WRITE::CLEAR + PTE::DIRTY::CLEAR);
        }

        // 设置可执行标志
        if config.executable {
            entry.as_base().modify(PTE::NO_EXEC::CLEAR);
        } else {
            entry.as_base().modify(PTE::NO_EXEC::SET);
        }

        // 设置用户访问标志（PLV3 表示用户态）
        if config.lower {
            entry.as_base().modify(PTE::PLV::PLV3);
        } else {
            entry.as_base().modify(PTE::PLV::PLV0);
        }

        // 设置脏位
        if config.dirty {
            entry.as_base().modify(PTE::DIRTY::SET);
        } else {
            entry.as_base().modify(PTE::DIRTY::CLEAR);
        }

        // 设置物理地址（关键：根据 is_dir 选择不同的布局）
        if config.is_dir {
            // 目录项：使用 PTE_DIR 格式，bits [51:13]
            let ppn = (config.paddr.raw() as u64) >> 12;
            entry.as_dir().modify(PTE_DIR::PHYS_ADDR.val(ppn));

            // 设置全局标志（目录项使用 G 位，bit 12）
            if config.global {
                entry.as_dir().modify(PTE_DIR::G::SET);
            }

            // 设置大页标志（仅目录项，H 位 bit 6）
            if config.huge {
                entry.as_dir().modify(PTE_DIR::H::SET);
            }
        } else {
            // 页表项：使用 PTE 格式，bits [51:12]
            let ppn = (config.paddr.raw() as u64) >> 12;
            entry.as_base().modify(PTE::PHYS_ADDR.val(ppn));

            // 设置全局标志（页表项使用 G 位，bit 6）
            if config.global {
                entry.as_base().modify(PTE::G::SET);
            }

            // 页表项不能是大页，huge 标志被忽略
        }

        // 设置内存属性
        let cache = match config.mem_attr {
            MemAttributes::Device => 0b00,                         // SUC
            MemAttributes::Normal | MemAttributes::PerCpu => 0b01, // CC
            MemAttributes::Uncached => 0b10,                       // WUC
        };
        entry.as_base().modify(PTE::CACHE.val(cache));

        entry
    }

    fn to_config(&self, is_dir: bool) -> page_table_generic::PteConfig {
        let valid = self.as_base().is_set(PTE::VALID);

        // 获取物理地址（关键：根据 is_dir 选择不同的布局）
        let paddr = if is_dir {
            // 目录项：使用 PTE_DIR 格式，bits [51:13]
            let raw_val = self.as_dir().read(PTE_DIR::PHYS_ADDR);
            (raw_val << 12) as usize
        } else {
            // 页表项：使用 PTE 格式，bits [51:12]
            let raw_val = self.as_base().read(PTE::PHYS_ADDR);
            (raw_val << 12) as usize
        };

        // 检查是否为大页（仅目录项）
        let huge = if is_dir {
            self.as_dir().is_set(PTE_DIR::H)
        } else {
            false
        };

        // 检查全局标志（根据 is_dir 选择不同的位）
        let global = if is_dir {
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

/// 页表遍历结果
#[derive(Debug, Clone, Copy)]
pub struct WalkResult {
    /// 虚拟地址
    pub vaddr: usize,
    /// 最终物理地址
    pub paddr: usize,
    /// 是否是大页映射
    pub is_huge: bool,
    /// 大页级别 (0=PTE 4KB, 1=PMD 2MB, 2=PUD, 3=PGD 1GB)
    pub huge_level: usize,
}

/// 软件页表遍历 - 按照 LoongArch64 手册伪代码实现
/// 参考: LoongArch64 参考手册 Vol. 1 - 5.4.5 节
pub fn find_stlb(vaddr: usize) -> WalkResult {
    use super::addrspace::PAGE_OFFSET;
    use super::paging::{read_csr_pgdh, read_csr_pgdl, read_csr_pwctl0, read_csr_pwctl1};

    const VALEN: usize = 48;
    const PAGE_SHIFT: usize = 12;
    const PAGE_MASK: usize = (1 << PAGE_SHIFT) - 1;

    println!("\n========== 硬件页表遍历模拟 ==========");
    println!("虚拟地址: {:#018x}", vaddr);

    // 读取 CSR 寄存器配置
    let pwctl0 = read_csr_pwctl0();
    let pwctl1 = read_csr_pwctl1();
    let asid = asid::read();
    println!("ASID: {:#x}, width: {}", asid.asid(), asid.asid_width());

    // 解析 PWCTL0: PTBase, PTWidth, Dir0Base, Dir0Width, Dir1Base, Dir1Width
    let pt_base = (pwctl0 & 0x1f) as usize; // bits [4:0]
    let pt_width = ((pwctl0 >> 5) & 0x1f) as usize; // bits [9:5]
    let dir0_base = ((pwctl0 >> 10) & 0x1f) as usize; // bits [14:10]
    let dir0_width = ((pwctl0 >> 15) & 0x1f) as usize; // bits [19:15]
    let dir1_base = ((pwctl0 >> 20) & 0x1f) as usize; // bits [24:20]
    let dir1_width = ((pwctl0 >> 25) & 0x1f) as usize; // bits [29:25]

    // 解析 PWCTL1: Dir2Base, Dir2Width, Dir3Base, Dir3Width
    let dir2_base = (pwctl1 & 0x3f) as usize; // bits [5:0]
    let dir2_width = ((pwctl1 >> 6) & 0x3f) as usize; // bits [11:6]
    let dir3_base = ((pwctl1 >> 12) & 0x3f) as usize; // bits [17:12]
    let dir3_width = ((pwctl1 >> 18) & 0x3f) as usize; // bits [23:18]

    println!("PWCTL 配置:");
    println!("  PT: base={}, width={}", pt_base, pt_width);
    println!("  Dir0(PMD): base={}, width={}", dir0_base, dir0_width);
    println!("  Dir1(PUD): base={}, width={}", dir1_base, dir1_width);
    println!("  Dir2(PGD): base={}, width={}", dir2_base, dir2_width);
    println!("  Dir3: base={}, width={}", dir3_base, dir3_width);

    // 根据 VA[VALEN-1] 选择 PGDL 或 PGDH
    let use_high_half = (vaddr >> (VALEN - 1)) & 1 == 1;
    let mut table_paddr = if use_high_half {
        println!("使用高地址空间页表 (PGDH)");
        read_csr_pgdh() as usize
    } else {
        println!("使用低地址空间页表 (PGDL)");
        read_csr_pgdl() as usize
    };

    if table_paddr == 0 {
        panic!("页表基地址为空");
    }
    println!("PGD 基址: {:#018x}", table_paddr);

    // 定义页表遍历的各级配置 (从高到低: Dir3 -> Dir2 -> Dir1 -> Dir0 -> PT)
    // 根据 PWCTL 寄存器，只有 width > 0 的级别才存在
    let levels = [
        ("Dir3(保留)", dir3_base, dir3_width),
        ("Dir2(PGD)", dir2_base, dir2_width),
        ("Dir1(PUD)", dir1_base, dir1_width),
        ("Dir0(PMD)", dir0_base, dir0_width),
        ("PT(PTE)", pt_base, pt_width),
    ];

    // 循环遍历各级页表（从高到低）
    for (level_idx, (level_name, base, width)) in levels.iter().enumerate() {
        if *width == 0 {
            println!("跳过 {} (width=0)", level_name);
            continue;
        }

        // 计算当前级别的索引
        let index = (vaddr >> base) & ((1 << width) - 1);

        println!("\n--- {} 级别 ---", level_name);
        println!("  Base: {}, Width: {}, Index: {}", base, width, index);

        // 读取页表项
        let table_vaddr = table_paddr + PAGE_OFFSET;
        let entry_ptr = (table_vaddr + index * 8) as *const u64;

        unsafe { core::arch::asm!("dbar 0", options(nostack, nomem)) };
        let entry_val = unsafe { core::ptr::read_volatile(entry_ptr) };
        let entry = Entry(entry_val);

        println!("  表虚拟地址: {:#018x}", table_vaddr);
        println!("  条目[{}] 地址: {:#018x}", index, entry_ptr as usize);
        println!("  条目[{}] 原始值: {:#018x}", index, entry_val);

        // 检查是否是大页 (H bit 6)
        // 只检查目录项（Dir3/Dir2/Dir1/Dir0），页表项（PT）不可能是大页
        let is_dir = !level_name.contains("PT");

        // 检查有效性 (V bit 0)
        let entry_config = entry.to_config(is_dir);
        if !entry_config.valid {
            panic!(
                "{} 条目无效 (V=0): vaddr={:#018x}, index={}",
                level_name, vaddr, index
            );
        }

        // 检查存在位 (P bit 7) - 用于页表遍历
        if (entry_val & (1 << 7)) == 0 {
            panic!(
                "{} 条目不存在 (P=0): vaddr={:#018x}, index={}",
                level_name, vaddr, index
            );
        }

        println!("  条目[{}] 详情: {:#x?}", index, EntryDebug(entry, is_dir));

        if entry_config.huge {
            println!("  -> 检测到大页！");
            let phys_base = entry_config.paddr.raw() & !PAGE_MASK;
            let offset_mask = (1 << base) - 1;
            let offset = vaddr & offset_mask;
            let final_paddr = phys_base + offset;

            println!("✓ {} 大页映射", level_name);
            println!("  物理页基址: {:#018x}", phys_base);
            println!("  页内偏移:   {:#018x}", offset);
            println!("  最终物理地址: {:#018x}", final_paddr);
            println!("==========================================\n");

            return WalkResult {
                vaddr,
                paddr: final_paddr,
                is_huge: true,
                huge_level: level_idx,
            };
        }

        // 获取下一级页表的物理地址
        table_paddr = entry_config.paddr.raw() & !PAGE_MASK;
        println!("  -> 下一级页表物理地址: {:#018x}", table_paddr);
    }

    // 所有级别都遍历完，计算最终物理地址
    let offset_in_page = vaddr & PAGE_MASK;
    let final_paddr = table_paddr + offset_in_page;

    println!("\n✓ 基本页映射");
    println!("  物理页基址: {:#018x}", table_paddr);
    println!("  页内偏移:   {:#018x}", offset_in_page);
    println!("  最终物理地址: {:#018x}", final_paddr);
    println!("==========================================\n");

    WalkResult {
        vaddr,
        paddr: final_paddr,
        is_huge: false,
        huge_level: 0,
    }
}
