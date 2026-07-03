//! CPU 信息获取模块
//!
//! 提供多架构的 CPU 信息解析功能，通过解析 ACPI MADT 表获取 CPU 核心信息。
//!
//! # 支持的架构
//! - **x86_64**: 使用 acpi crate 的 LocalApic/LocalX2Apic 条目
//! - **AArch64**: 使用 acpi crate 的 Gicc 条目
//! - **RISC-V 64**: 手动解析 RINTC 条目 (acpi crate 不支持)
//! - **LoongArch64**: 手动解析 Core PIC 条目 (acpi crate 不支持)

/// MADT 头部大小: SdtHeader(36) + local_apic_address(4) + flags(4)
const MADT_HEADER_SIZE: usize = 44;

/// MADT 标志：CPU 已启用
const ACPI_MADT_ENABLED: u32 = 1;

// 条件导入：用于 acpi crate 支持的架构
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
use acpi::sdt::madt::Madt;

// ============================================================================
// 通用 CPU 信息结构
// ============================================================================

/// CPU 信息
#[derive(Clone, Copy, Debug)]
pub struct CpuInfo {
    /// 物理 CPU ID (架构相关)
    /// - x86_64: APIC ID / X2APIC ID
    /// - AArch64: MPIDR
    /// - RISC-V: hart ID
    /// - LoongArch64: core ID
    pub physical_id: u32,
    /// ACPI 处理器 ID
    #[allow(dead_code)]
    pub processor_id: u32,
    /// 是否启用
    pub enabled: bool,
}

fn non_empty_cpu_info_iter<I>(mut iter: I) -> Option<impl Iterator<Item = CpuInfo>>
where
    I: Iterator<Item = CpuInfo>,
{
    let first = iter.next()?;
    Some(core::iter::once(first).chain(iter))
}

fn non_empty_enabled_cpu_id_iter<I>(cpu_info: I) -> Option<impl Iterator<Item = usize>>
where
    I: Iterator<Item = CpuInfo>,
{
    let mut ids = cpu_info
        .filter(|info| info.enabled)
        .map(|info| info.physical_id as usize);
    let first = ids.next()?;
    Some(core::iter::once(first).chain(ids))
}

// ============================================================================
// x86_64 实现 (使用 acpi crate)
// ============================================================================

#[cfg(target_arch = "x86_64")]
mod x86_64_impl {
    use super::{
        super::tables, ACPI_MADT_ENABLED, CpuInfo, MADT_HEADER_SIZE, Madt, non_empty_cpu_info_iter,
        non_empty_enabled_cpu_id_iter,
    };

    const MADT_TYPE_LOCAL_APIC: u8 = 0;
    const MADT_TYPE_LOCAL_X2APIC: u8 = 9;

    #[derive(Clone, Copy, Debug)]
    #[repr(C, packed)]
    struct LocalApicEntry {
        entry_type: u8,
        length: u8,
        processor_id: u8,
        apic_id: u8,
        flags: u32,
    }

    #[derive(Clone, Copy, Debug)]
    #[repr(C, packed)]
    struct LocalX2ApicEntry {
        entry_type: u8,
        length: u8,
        _reserved: u16,
        x2apic_id: u32,
        flags: u32,
        processor_id: u32,
    }

    const _: () = assert!(core::mem::size_of::<LocalApicEntry>() == 8);
    const _: () = assert!(core::mem::size_of::<LocalX2ApicEntry>() == 16);

    struct X86CpuIter {
        _madt_mapping: acpi::PhysicalMapping<super::super::AcpiHandle, Madt>,
        madt_ptr: *const u8,
        madt_len: usize,
        offset: usize,
    }

    impl Iterator for X86CpuIter {
        type Item = CpuInfo;

        fn next(&mut self) -> Option<Self::Item> {
            while self.offset + 2 <= self.madt_len {
                unsafe {
                    let entry_type = *self.madt_ptr.add(self.offset);
                    let entry_len = *self.madt_ptr.add(self.offset + 1) as usize;

                    if entry_len < 2 || self.offset + entry_len > self.madt_len {
                        return None;
                    }

                    let mut cpu_info = None;

                    if entry_type == MADT_TYPE_LOCAL_APIC
                        && entry_len >= core::mem::size_of::<LocalApicEntry>()
                    {
                        let entry = core::ptr::read_unaligned(
                            self.madt_ptr.add(self.offset) as *const LocalApicEntry
                        );
                        cpu_info = Some(CpuInfo {
                            physical_id: entry.apic_id as u32,
                            processor_id: entry.processor_id as u32,
                            enabled: (entry.flags & ACPI_MADT_ENABLED) != 0,
                        });
                    } else if entry_type == MADT_TYPE_LOCAL_X2APIC
                        && entry_len >= core::mem::size_of::<LocalX2ApicEntry>()
                    {
                        let entry = core::ptr::read_unaligned(
                            self.madt_ptr.add(self.offset) as *const LocalX2ApicEntry
                        );
                        cpu_info = Some(CpuInfo {
                            physical_id: entry.x2apic_id,
                            processor_id: entry.processor_id,
                            enabled: (entry.flags & ACPI_MADT_ENABLED) != 0,
                        });
                    }

                    self.offset += entry_len;

                    if let Some(info) = cpu_info {
                        return Some(info);
                    }
                }
            }

            None
        }
    }

    /// 获取 x86_64 CPU 信息列表
    ///
    /// 通过解析 MADT 表中的 LocalApic/LocalX2Apic 条目获取所有 CPU 核心信息。
    pub fn x86_64_cpu_info() -> Option<impl Iterator<Item = CpuInfo>> {
        let tables = tables().ok()?;
        let madt_mapping = tables.find_table::<Madt>()?;
        let madt_ptr = madt_mapping.virtual_start.as_ptr() as *const u8;
        let madt_len = madt_mapping.region_length;

        non_empty_cpu_info_iter(X86CpuIter {
            _madt_mapping: madt_mapping,
            madt_ptr,
            madt_len,
            offset: MADT_HEADER_SIZE,
        })
    }

    /// 获取 x86_64 CPU ID 列表（仅返回已启用的）
    pub fn x86_64_cpu_id_list() -> Option<impl Iterator<Item = usize>> {
        non_empty_enabled_cpu_id_iter(x86_64_cpu_info()?)
    }
}

#[cfg(target_arch = "x86_64")]
pub use x86_64_impl::*;

// ============================================================================
// AArch64 实现 (使用 acpi crate)
// ============================================================================

#[cfg(target_arch = "aarch64")]
mod aarch64_impl {
    use super::{
        super::tables, ACPI_MADT_ENABLED, CpuInfo, MADT_HEADER_SIZE, Madt, non_empty_cpu_info_iter,
        non_empty_enabled_cpu_id_iter,
    };

    const MADT_TYPE_GICC: u8 = 11;

    #[derive(Clone, Copy, Debug)]
    #[repr(C, packed)]
    struct GiccHead {
        entry_type: u8,
        length: u8,
        _reserved: u16,
        cpu_interface_number: u32,
        uid: u32,
        flags: u32,
    }

    const _: () = assert!(core::mem::size_of::<GiccHead>() == 16);

    struct Aarch64CpuIter {
        _madt_mapping: acpi::PhysicalMapping<super::super::AcpiHandle, Madt>,
        madt_ptr: *const u8,
        madt_len: usize,
        offset: usize,
    }

    impl Iterator for Aarch64CpuIter {
        type Item = CpuInfo;

        fn next(&mut self) -> Option<Self::Item> {
            while self.offset + 2 <= self.madt_len {
                unsafe {
                    let entry_type = *self.madt_ptr.add(self.offset);
                    let entry_len = *self.madt_ptr.add(self.offset + 1) as usize;

                    if entry_len < 2 || self.offset + entry_len > self.madt_len {
                        return None;
                    }

                    let mut cpu_info = None;

                    if entry_type == MADT_TYPE_GICC && entry_len >= core::mem::size_of::<GiccHead>()
                    {
                        let entry = core::ptr::read_unaligned(
                            self.madt_ptr.add(self.offset) as *const GiccHead
                        );
                        cpu_info = Some(CpuInfo {
                            physical_id: entry.cpu_interface_number,
                            processor_id: entry.uid,
                            enabled: (entry.flags & ACPI_MADT_ENABLED) != 0,
                        });
                    }

                    self.offset += entry_len;

                    if let Some(info) = cpu_info {
                        return Some(info);
                    }
                }
            }

            None
        }
    }

    /// 获取 AArch64 CPU 信息列表
    ///
    /// 通过解析 MADT 表中的 GICC 条目获取所有 CPU 核心信息。
    /// MPIDR 是 PSCI/启动次核常用的硬件 ID。
    pub fn aarch64_cpu_info() -> Option<impl Iterator<Item = CpuInfo>> {
        let tables = tables().ok()?;
        let madt_mapping = tables.find_table::<Madt>()?;
        let madt_ptr = madt_mapping.virtual_start.as_ptr() as *const u8;
        let madt_len = madt_mapping.region_length;

        non_empty_cpu_info_iter(Aarch64CpuIter {
            _madt_mapping: madt_mapping,
            madt_ptr,
            madt_len,
            offset: MADT_HEADER_SIZE,
        })
    }

    /// 获取 AArch64 CPU ID 列表（仅返回已启用的）
    pub fn aarch64_cpu_id_list() -> Option<impl Iterator<Item = usize>> {
        non_empty_enabled_cpu_id_iter(aarch64_cpu_info()?)
    }
}

#[cfg(target_arch = "aarch64")]
pub use aarch64_impl::*;

// ============================================================================
// RISC-V 64 实现 (手动解析 RINTC)
// ============================================================================

#[cfg(target_arch = "riscv64")]
mod riscv64_impl {
    use acpi::sdt::madt::Madt;

    use super::{
        super::tables, ACPI_MADT_ENABLED, CpuInfo, MADT_HEADER_SIZE, non_empty_cpu_info_iter,
        non_empty_enabled_cpu_id_iter,
    };

    /// MADT RINTC 条目类型 (RISC-V)
    /// 参考 Linux: include/acpi/actbl2.h - ACPI_MADT_TYPE_RINTC
    const MADT_TYPE_RINTC: u8 = 0x18;

    /// RISC-V MADT RINTC 结构
    ///
    /// 参考 Linux: struct acpi_madt_rintc (include/acpi/actbl2.h)
    ///
    /// # Layout
    /// ```text
    /// | Offset | Size | Field         |
    /// |--------|------|---------------|
    /// | 0      | 1    | entry_type    | (0x18)
    /// | 1      | 1    | length        |
    /// | 2      | 1    | version       |
    /// | 3      | 1    | reserved      |
    /// | 4      | 4    | flags         |
    /// | 8      | 8    | hart_id       |
    /// | 16     | 4    | uid           |
    /// | 20     | 4    | ext_intc_id   |
    /// | 24     | 8    | imsic_addr    |
    /// | 32     | 4    | imsic_size    |
    /// ```
    #[derive(Clone, Copy, Debug)]
    #[repr(C, packed)]
    pub struct RintcEntry {
        /// 条目类型，应为 0x18 (MADT_TYPE_RINTC)
        pub entry_type: u8,
        /// 条目长度
        pub length: u8,
        /// 版本号
        pub version: u8,
        /// 保留字段
        _reserved1: u8,
        /// 标志位 (bit 0: enabled)
        pub flags: u32,
        /// RISC-V Hart ID
        pub hart_id: u64,
        /// ACPI 处理器 UID
        pub uid: u32,
        /// 外部中断控制器 ID
        pub ext_intc_id: u32,
        /// IMSIC 基地址
        pub imsic_addr: u64,
        /// IMSIC 大小
        pub imsic_size: u32,
    }

    impl RintcEntry {
        /// 检查 CPU 是否已启用
        #[inline]
        pub fn is_enabled(&self) -> bool {
            (self.flags & ACPI_MADT_ENABLED) != 0
        }
    }

    // 确保结构体大小正确 (36 字节)
    const _: () = assert!(core::mem::size_of::<RintcEntry>() == 36);

    struct Riscv64CpuIter {
        _madt_mapping: acpi::PhysicalMapping<super::super::AcpiHandle, Madt>,
        madt_ptr: *const u8,
        madt_len: usize,
        offset: usize,
    }

    impl Iterator for Riscv64CpuIter {
        type Item = CpuInfo;

        fn next(&mut self) -> Option<Self::Item> {
            while self.offset + 2 <= self.madt_len {
                unsafe {
                    let entry_type = *self.madt_ptr.add(self.offset);
                    let entry_len = *self.madt_ptr.add(self.offset + 1) as usize;

                    if entry_len < 2 || self.offset + entry_len > self.madt_len {
                        return None;
                    }

                    let mut cpu_info = None;

                    if entry_type == MADT_TYPE_RINTC
                        && entry_len >= core::mem::size_of::<RintcEntry>()
                    {
                        let rintc = core::ptr::read_unaligned(
                            self.madt_ptr.add(self.offset) as *const RintcEntry
                        );

                        cpu_info = Some(CpuInfo {
                            physical_id: rintc.hart_id as u32,
                            processor_id: rintc.uid,
                            enabled: rintc.is_enabled(),
                        });
                    }

                    self.offset += entry_len;

                    if let Some(info) = cpu_info {
                        return Some(info);
                    }
                }
            }

            None
        }
    }

    /// 获取 RISC-V CPU 信息列表
    ///
    /// 通过解析 MADT 表中的 RINTC 条目获取所有 CPU 核心信息。
    pub fn riscv64_cpu_info() -> Option<impl Iterator<Item = CpuInfo>> {
        let tables = tables().ok()?;
        let madt_mapping = tables.find_table::<Madt>()?;
        let madt_ptr = madt_mapping.virtual_start.as_ptr() as *const u8;
        let madt_len = madt_mapping.region_length;

        non_empty_cpu_info_iter(Riscv64CpuIter {
            _madt_mapping: madt_mapping,
            madt_ptr,
            madt_len,
            offset: MADT_HEADER_SIZE,
        })
    }

    /// 获取 RISC-V CPU ID 列表（仅返回已启用的）
    pub fn riscv64_cpu_id_list() -> Option<impl Iterator<Item = usize>> {
        non_empty_enabled_cpu_id_iter(riscv64_cpu_info()?)
    }
}

#[cfg(target_arch = "riscv64")]
pub use riscv64_impl::*;

// ============================================================================
// LoongArch64 实现 (手动解析 Core PIC)
// ============================================================================

#[cfg(target_arch = "loongarch64")]
mod loongarch64_impl {
    use acpi::sdt::madt::Madt;

    use super::{
        super::tables, ACPI_MADT_ENABLED, CpuInfo, MADT_HEADER_SIZE, non_empty_cpu_info_iter,
        non_empty_enabled_cpu_id_iter,
    };

    /// MADT Core PIC 条目类型 (LoongArch64)
    /// 参考 Linux: include/acpi/actbl2.h - ACPI_MADT_TYPE_CORE_PIC
    const MADT_TYPE_CORE_PIC: u8 = 0x11;

    /// LoongArch64 MADT Core PIC 结构
    ///
    /// 参考 Linux: struct acpi_madt_core_pic (include/acpi/actbl2.h)
    #[derive(Clone, Copy, Debug)]
    #[repr(C, packed)]
    pub struct CorePicEntry {
        /// 条目类型，应为 0x11 (MADT_TYPE_CORE_PIC)
        pub entry_type: u8,
        /// 条目长度
        pub length: u8,
        /// 版本号
        pub version: u8,
        /// ACPI 处理器 ID
        pub processor_id: u32,
        /// 物理 CPU ID (核心 ID)
        pub core_id: u32,
        /// 标志位 (bit 0: enabled)
        pub flags: u32,
    }

    impl CorePicEntry {
        /// 检查 CPU 是否已启用
        #[inline]
        pub fn is_enabled(&self) -> bool {
            (self.flags & ACPI_MADT_ENABLED) != 0
        }
    }

    // 确保结构体大小正确 (15 字节)
    const _: () = assert!(core::mem::size_of::<CorePicEntry>() == 15);

    struct LoongArch64CpuIter {
        _madt_mapping: acpi::PhysicalMapping<super::super::AcpiHandle, Madt>,
        madt_ptr: *const u8,
        madt_len: usize,
        offset: usize,
    }

    impl Iterator for LoongArch64CpuIter {
        type Item = CpuInfo;

        fn next(&mut self) -> Option<Self::Item> {
            while self.offset + 2 <= self.madt_len {
                unsafe {
                    let entry_type = *self.madt_ptr.add(self.offset);
                    let entry_len = *self.madt_ptr.add(self.offset + 1) as usize;

                    if entry_len < 2 || self.offset + entry_len > self.madt_len {
                        return None;
                    }

                    let mut cpu_info = None;

                    if entry_type == MADT_TYPE_CORE_PIC
                        && entry_len >= core::mem::size_of::<CorePicEntry>()
                    {
                        let core_pic = core::ptr::read_unaligned(
                            self.madt_ptr.add(self.offset) as *const CorePicEntry
                        );

                        cpu_info = Some(CpuInfo {
                            physical_id: core_pic.core_id,
                            processor_id: core_pic.processor_id,
                            enabled: core_pic.is_enabled(),
                        });
                    }

                    self.offset += entry_len;

                    if let Some(info) = cpu_info {
                        return Some(info);
                    }
                }
            }

            None
        }
    }

    /// 获取 LoongArch64 CPU 信息列表
    ///
    /// 通过解析 MADT 表中的 Core PIC 条目获取所有 CPU 核心信息。
    pub fn loongarch64_cpu_info() -> Option<impl Iterator<Item = CpuInfo>> {
        let tables = tables().ok()?;
        let madt_mapping = tables.find_table::<Madt>()?;
        let madt_ptr = madt_mapping.virtual_start.as_ptr() as *const u8;
        let madt_len = madt_mapping.region_length;

        non_empty_cpu_info_iter(LoongArch64CpuIter {
            _madt_mapping: madt_mapping,
            madt_ptr,
            madt_len,
            offset: MADT_HEADER_SIZE,
        })
    }

    /// 获取 LoongArch64 CPU ID 列表（仅返回已启用的）
    pub fn loongarch64_cpu_id_list() -> Option<impl Iterator<Item = usize>> {
        non_empty_enabled_cpu_id_iter(loongarch64_cpu_info()?)
    }
}

#[cfg(target_arch = "loongarch64")]
pub use loongarch64_impl::*;

// ============================================================================
// 架构无关的公共 API
// ============================================================================

/// 获取当前架构的 CPU 信息列表
///
/// 根据目标架构自动选择正确的解析方式：
/// - x86_64: LocalApic/LocalX2Apic
/// - AArch64: Gicc
/// - RISC-V 64: RINTC
/// - LoongArch64: Core PIC
#[cfg(target_arch = "x86_64")]
#[allow(dead_code)]
pub fn cpu_info() -> Option<impl Iterator<Item = CpuInfo>> {
    x86_64_cpu_info()
}

#[cfg(target_arch = "aarch64")]
#[allow(dead_code)]
pub fn cpu_info() -> Option<impl Iterator<Item = CpuInfo>> {
    aarch64_cpu_info()
}

#[cfg(target_arch = "riscv64")]
#[allow(dead_code)]
pub fn cpu_info() -> Option<impl Iterator<Item = CpuInfo>> {
    riscv64_cpu_info()
}

#[cfg(target_arch = "loongarch64")]
#[allow(dead_code)]
pub fn cpu_info() -> Option<impl Iterator<Item = CpuInfo>> {
    loongarch64_cpu_info()
}

/// 获取当前架构的 CPU ID 列表（仅返回已启用的）
#[cfg(target_arch = "x86_64")]
pub fn cpu_id_list() -> Option<impl Iterator<Item = usize>> {
    x86_64_cpu_id_list()
}

#[cfg(target_arch = "aarch64")]
pub fn cpu_id_list() -> Option<impl Iterator<Item = usize>> {
    aarch64_cpu_id_list()
}

#[cfg(target_arch = "riscv64")]
pub fn cpu_id_list() -> Option<impl Iterator<Item = usize>> {
    riscv64_cpu_id_list()
}

#[cfg(target_arch = "loongarch64")]
pub fn cpu_id_list() -> Option<impl Iterator<Item = usize>> {
    loongarch64_cpu_id_list()
}
