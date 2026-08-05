//! Normalized LoongArch firmware inputs.

use super::super::GuestPlatform;

const VIRT_PCI_CFG_BASE: u64 = 0x2000_0000;
const VIRT_PCI_CFG_SIZE: u64 = 0x0800_0000;

#[derive(Clone, Copy, Debug)]
pub(in crate::arch::loongarch64::boot) struct LoongArchFwCfgSerialConfig {
    pub(in crate::arch::loongarch64::boot) base: u64,
    pub(in crate::arch::loongarch64::boot) size: u64,
    pub(in crate::arch::loongarch64::boot) irq: u8,
    pub(in crate::arch::loongarch64::boot) clock_hz: u32,
    pub(in crate::arch::loongarch64::boot) baud: u32,
}

impl Default for LoongArchFwCfgSerialConfig {
    fn default() -> Self {
        Self {
            base: 0x1fe0_01e0,
            size: 0x100,
            irq: 66,
            clock_hz: 100_000_000,
            baud: 115_200,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(in crate::arch::loongarch64::boot) struct LoongArchFwCfgPciConfig {
    pub(in crate::arch::loongarch64::boot) ecam_base: u64,
    pub(in crate::arch::loongarch64::boot) ecam_size: u64,
    pub(in crate::arch::loongarch64::boot) mmio_base: u64,
    pub(in crate::arch::loongarch64::boot) mmio_size: u64,
    pub(in crate::arch::loongarch64::boot) io_base: u64,
    pub(in crate::arch::loongarch64::boot) io_size: u32,
    pub(in crate::arch::loongarch64::boot) intx_base: u8,
}

impl Default for LoongArchFwCfgPciConfig {
    fn default() -> Self {
        Self {
            ecam_base: VIRT_PCI_CFG_BASE,
            ecam_size: VIRT_PCI_CFG_SIZE,
            mmio_base: 0x4000_0000,
            mmio_size: 0x4000_0000,
            io_base: 0x1800_0000,
            io_size: 0x0001_0000,
            intx_base: 80,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(in crate::arch::loongarch64::boot) struct LoongArchFwCfgInterruptConfig {
    pub(in crate::arch::loongarch64::boot) eiointc_irq: u8,
    pub(in crate::arch::loongarch64::boot) pch_msi_base: u64,
    pub(in crate::arch::loongarch64::boot) pch_msi_start: u32,
    pub(in crate::arch::loongarch64::boot) pch_msi_count: u32,
    pub(in crate::arch::loongarch64::boot) pch_pic_base: u64,
    pub(in crate::arch::loongarch64::boot) pch_pic_size: u16,
    pub(in crate::arch::loongarch64::boot) pch_pic_gsi_base: u16,
}

impl Default for LoongArchFwCfgInterruptConfig {
    fn default() -> Self {
        Self {
            eiointc_irq: 3,
            pch_msi_base: 0x2ff0_0000,
            pch_msi_start: 0x40,
            pch_msi_count: 0xc0,
            pch_pic_base: 0x1000_0000,
            pch_pic_size: 0x1000,
            pch_pic_gsi_base: 0x40,
        }
    }
}

pub(in crate::arch::loongarch64::boot) fn serial_config(
    platform: &GuestPlatform,
) -> LoongArchFwCfgSerialConfig {
    LoongArchFwCfgSerialConfig {
        base: platform.serial.mmio.base,
        size: platform.serial.mmio.size,
        irq: (platform.interrupt.acpi_gsi_base + platform.serial.irq) as u8,
        clock_hz: platform.serial.clock_hz,
        baud: platform.serial.baud,
    }
}

pub(in crate::arch::loongarch64::boot) fn pci_config(
    platform: &GuestPlatform,
) -> LoongArchFwCfgPciConfig {
    LoongArchFwCfgPciConfig {
        ecam_base: platform.pci.ecam.base,
        ecam_size: platform.pci.ecam.size,
        mmio_base: platform.pci.mmio.base,
        mmio_size: platform.pci.mmio.size,
        io_base: platform.pci.io_base,
        io_size: platform.pci.io_size as u32,
        intx_base: (platform.interrupt.acpi_gsi_base + platform.pci.intx_base) as u8,
    }
}

pub(in crate::arch::loongarch64::boot) fn interrupt_config(
    platform: &GuestPlatform,
) -> LoongArchFwCfgInterruptConfig {
    LoongArchFwCfgInterruptConfig {
        eiointc_irq: platform.interrupt.eiointc_irq as u8,
        pch_msi_base: platform.interrupt.pch_msi.base,
        pch_msi_start: platform.interrupt.acpi_msi_start,
        pch_msi_count: platform.interrupt.acpi_msi_count,
        pch_pic_base: platform.interrupt.pch_pic.base,
        pch_pic_size: platform.interrupt.pch_pic.size as u16,
        pch_pic_gsi_base: platform.interrupt.acpi_gsi_base as u16,
    }
}
