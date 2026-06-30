mod probe;
mod resources;

use alloc::{boxed::Box, vec::Vec};

use ax_errno::{AxResult, ax_err_type};
use axdevice::{
    FwCfgInterruptConfig, FwCfgPciConfig, FwCfgPlatformConfig, FwCfgRamRegion, FwCfgSerialConfig,
};
use axvm::{AxVMRef, GuestPhysAddr};
use axvmconfig::{AxVMCrateConfig, EmulatedDeviceType};

pub use resources::{
    LoongArchGuestIrqRoute, get_guest_irq_routes, prepare_uefi_fdt_config,
    prepare_uefi_runtime_config,
};

use crate::images::load_vm_image_from_memory;

pub const UEFI_FIRMWARE_FDT_BASE: usize = 0x0010_0000;

pub fn init() {
    resources::init();
}

#[derive(Clone, Debug)]
pub struct GuestPlatform {
    pub ram_regions: Vec<MemoryRegion>,
    pub serial: SerialDevice,
    pub pci: PciHost,
    pub interrupt: InterruptTopology,
    pub fw_cfg: MmioRegion,
    pub firmware_devices: FirmwareDevices,
    pub irq_routes: Vec<probe::GuestIrqRoute>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryRegion {
    pub base: u64,
    pub size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MmioRegion {
    pub base: u64,
    pub size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SerialDevice {
    pub mmio: MmioRegion,
    pub irq: u32,
    pub clock_hz: u32,
    pub baud: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciHost {
    pub ecam: MmioRegion,
    pub mmio: MmioRegion,
    pub io_base: u64,
    pub io_size: u64,
    pub intx_base: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterruptTopology {
    pub eiointc_irq: u32,
    pub pch_pic: MmioRegion,
    pub pch_pic_gsi_base: u32,
    pub pch_msi: MmioRegion,
    pub pch_msi_start: u32,
    pub pch_msi_count: u32,
    pub acpi_gsi_base: u32,
    pub acpi_msi_start: u32,
    pub acpi_msi_count: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FirmwareDevices {
    pub rtc: IrqMmioDevice,
    pub flash: FlashDevice,
    pub ged: GedDevice,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IrqMmioDevice {
    pub mmio: MmioRegion,
    pub irq: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlashDevice {
    pub banks: [MmioRegion; 2],
    pub bank_width: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GedDevice {
    pub mmio: MmioRegion,
    pub poweroff_offset: u32,
    pub poweroff_value: u32,
    pub reboot_offset: u32,
    pub reboot_value: u32,
}

impl GuestPlatform {
    pub fn discover(vm: &AxVMRef, config: &AxVMCrateConfig) -> Self {
        probe::GuestPlatformBuilder::new(ram_regions(vm), config)
            .apply_host_acpi()
            .build()
    }

    pub fn fw_cfg_platform_config(&self) -> FwCfgPlatformConfig {
        let ram_regions = leak_fw_cfg_ram_regions(&self.ram_regions);
        FwCfgPlatformConfig {
            ram_regions,
            srat_regions: ram_regions,
            serial: FwCfgSerialConfig {
                base: self.serial.mmio.base,
                size: self.serial.mmio.size,
                irq: (self.interrupt.acpi_gsi_base + self.serial.irq) as u8,
                clock_hz: self.serial.clock_hz,
                baud: self.serial.baud,
            },
            pci: FwCfgPciConfig {
                ecam_base: self.pci.ecam.base,
                ecam_size: self.pci.ecam.size,
                mmio_base: self.pci.mmio.base,
                mmio_size: self.pci.mmio.size,
                io_base: self.pci.io_base,
                io_size: self.pci.io_size as u32,
                intx_base: (self.interrupt.acpi_gsi_base + self.pci.intx_base) as u8,
            },
            interrupt: FwCfgInterruptConfig {
                eiointc_irq: self.interrupt.eiointc_irq as u8,
                pch_msi_base: self.interrupt.pch_msi.base,
                pch_msi_start: self.interrupt.acpi_msi_start,
                pch_msi_count: self.interrupt.acpi_msi_count,
                pch_pic_base: self.interrupt.pch_pic.base,
                pch_pic_size: self.interrupt.pch_pic.size as u16,
                pch_pic_gsi_base: self.interrupt.acpi_gsi_base as u16,
            },
        }
    }
}

pub fn load_firmware_fdt(vm: &AxVMRef, config: &AxVMCrateConfig) -> AxResult {
    let platform = GuestPlatform::discover(vm, config);
    let fdt = crate::fdt::loongarch64::guest_firmware_dtb::build(&platform)?;
    debug!(
        "VM[{}] loading LoongArch UEFI firmware FDT: {} bytes at {:#x}",
        config.base.id,
        fdt.len(),
        UEFI_FIRMWARE_FDT_BASE
    );
    vm.with_config(|config| {
        config.set_dtb_load_gpa(GuestPhysAddr::from(UEFI_FIRMWARE_FDT_BASE));
    });
    load_vm_image_from_memory(
        &fdt,
        GuestPhysAddr::from(UEFI_FIRMWARE_FDT_BASE),
        vm.clone(),
    )
}

pub fn fw_cfg_platform_config(vm: &AxVMRef, config: &AxVMCrateConfig) -> FwCfgPlatformConfig {
    GuestPlatform::discover(vm, config).fw_cfg_platform_config()
}

pub fn guest_irq_routes(vm: &AxVMRef, config: &AxVMCrateConfig) -> Vec<LoongArchGuestIrqRoute> {
    GuestPlatform::discover(vm, config)
        .irq_routes
        .into_iter()
        .map(|route| LoongArchGuestIrqRoute {
            physical_irq: route.physical_irq,
            guest_vector: route.guest_vector,
        })
        .collect()
}

pub fn emulated_fw_cfg(config: &AxVMCrateConfig) -> AxResult<&axvmconfig::EmulatedDeviceConfig> {
    config
        .devices
        .emu_devices
        .iter()
        .find(|device| device.emu_type == EmulatedDeviceType::FwCfg)
        .ok_or_else(|| ax_err_type!(NotFound, "LoongArch UEFI boot requires a fw_cfg device"))
}

fn ram_regions(vm: &AxVMRef) -> Vec<MemoryRegion> {
    let mut regions = vm
        .memory_regions()
        .into_iter()
        .filter(|region| {
            region.gpa.as_usize() < 0x1000_0000 || region.gpa.as_usize() >= 0x8000_0000
        })
        .map(|region| MemoryRegion {
            base: region.gpa.as_usize() as u64,
            size: region.size() as u64,
        })
        .filter(|region| region.size != 0)
        .collect::<Vec<_>>();
    regions.sort_by_key(|region| region.base);
    if regions.is_empty() {
        regions.extend_from_slice(&[
            MemoryRegion {
                base: 0,
                size: 0x1000_0000,
            },
            MemoryRegion {
                base: 0x8000_0000,
                size: 0x2400_0000,
            },
        ]);
    }
    regions
}

fn leak_fw_cfg_ram_regions(regions: &[MemoryRegion]) -> &'static [FwCfgRamRegion] {
    let regions = regions
        .iter()
        .map(|region| FwCfgRamRegion {
            base: region.base,
            size: region.size,
        })
        .collect::<Vec<_>>();
    Box::leak(regions.into_boxed_slice())
}
