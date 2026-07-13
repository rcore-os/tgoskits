mod fdt;
mod probe;
mod resources;

use alloc::{boxed::Box, format, vec::Vec};

use axdevice::{
    FwCfgInterruptConfig, FwCfgPciConfig, FwCfgPlatformConfig, FwCfgRamRegion, FwCfgSerialConfig,
};
use axvmconfig::{AxVMCrateConfig, EmulatedDeviceType, VMBootProtocol};
pub use resources::{
    LoongArchGuestIrqRoute, get_guest_irq_routes, prepare_uefi_fdt_config,
    prepare_uefi_runtime_config,
};

use crate::{
    AxVMRef, AxVmResult, GuestPhysAddr,
    architecture::BootImagePlatform,
    ax_err, ax_err_type,
    boot::{
        BootImageProvider, StaticVmImage,
        images::{ImageLoaderCore, load_vm_image_from_memory},
    },
};

pub const UEFI_FIRMWARE_FDT_BASE: usize = 0x0010_0000;

pub struct ImageLoader<'a>(ImageLoaderCore<'a>);

impl<'a> ImageLoader<'a> {
    pub fn new(
        main_memory: crate::VMMemoryRegion,
        config: AxVMCrateConfig,
        vm: AxVMRef,
        provider: &'a dyn BootImageProvider,
    ) -> Self {
        Self(ImageLoaderCore::new(
            main_memory,
            config,
            vm,
            provider,
            None,
        ))
    }

    pub fn load(&mut self) -> AxVmResult {
        self.0.load()
    }
}

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

pub fn load_firmware_fdt(vm: &AxVMRef, config: &AxVMCrateConfig) -> AxVmResult {
    let platform = GuestPlatform::discover(vm, config);
    let fdt = fdt::guest_firmware_dtb::build(&platform)?;
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
    )?;
    vm.set_guest_device_tree(GuestPhysAddr::from(UEFI_FIRMWARE_FDT_BASE), fdt)
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

pub fn emulated_fw_cfg(config: &AxVMCrateConfig) -> AxVmResult<&axvmconfig::EmulatedDeviceConfig> {
    config
        .devices
        .emu_devices
        .iter()
        .find(|device| device.emu_type == EmulatedDeviceType::FwCfg)
        .ok_or_else(|| ax_err_type!(NotFound, "LoongArch UEFI boot requires a fw_cfg device"))
}

impl BootImagePlatform for super::LoongArch64Arch {
    fn load_images_from_memory(
        loader: &mut ImageLoaderCore<'_>,
        images: StaticVmImage,
    ) -> AxVmResult {
        ensure_uefi_boot(loader)?;
        load_uefi_firmware_dtb(loader)?;
        add_uefi_fw_cfg(loader, images.kernel, images.ramdisk)?;
        let firmware = images
            .bios
            .or_else(|| provider_firmware_image(loader))
            .ok_or_else(|| {
                ax_err_type!(
                    NotFound,
                    "LoongArch UEFI boot requires a build-time firmware image"
                )
            })?;
        load_uefi_firmware_image(loader, firmware)
    }

    #[cfg(any(feature = "fs", feature = "host-fs"))]
    fn load_images_from_filesystem(loader: &mut ImageLoaderCore<'_>) -> AxVmResult {
        ensure_uefi_boot(loader)?;
        load_uefi_firmware_dtb(loader)?;

        let kernel = crate::boot::images::fs::read_full_image(
            &loader.config.kernel.kernel_path,
            loader.provider,
        )?;
        let kernel: &'static [u8] = Box::leak(kernel.into_boxed_slice());
        let ramdisk = if let Some(path) = &loader.config.kernel.ramdisk_path {
            let ramdisk = crate::boot::images::fs::read_full_image(path, loader.provider)?;
            Some(Box::leak(ramdisk.into_boxed_slice()) as &'static [u8])
        } else {
            None
        };
        add_uefi_fw_cfg(loader, kernel, ramdisk)?;

        let firmware = provider_firmware_image(loader).ok_or_else(|| {
            ax_err_type!(
                NotFound,
                "LoongArch UEFI boot requires a build-time firmware image"
            )
        })?;
        load_uefi_firmware_image(loader, firmware)
    }
}

fn ensure_uefi_boot(loader: &ImageLoaderCore<'_>) -> AxVmResult {
    if loader.config.kernel.effective_boot_protocol() == VMBootProtocol::Uefi {
        Ok(())
    } else {
        ax_err!(Unsupported, "LoongArch guests require UEFI boot")
    }
}

fn load_uefi_firmware_dtb(loader: &ImageLoaderCore<'_>) -> AxVmResult {
    prepare_uefi_runtime_config(&loader.vm, &loader.config);
    load_firmware_fdt(&loader.vm, &loader.config)
}

fn add_uefi_fw_cfg(
    loader: &ImageLoaderCore<'_>,
    kernel: &'static [u8],
    ramdisk: Option<&'static [u8]>,
) -> AxVmResult {
    let fw_cfg = emulated_fw_cfg(&loader.config)?;
    loader.vm.add_fw_cfg_device(crate::FwCfgDeviceConfig {
        base: GuestPhysAddr::from(fw_cfg.base_gpa),
        size: fw_cfg.length,
        kernel,
        initrd: ramdisk,
        cmdline: loader.config.kernel.cmdline.clone(),
        cpu_num: loader.config.base.cpu_num as u16,
        platform: fw_cfg_platform_config(&loader.vm, &loader.config),
    })
}

fn provider_firmware_image(loader: &ImageLoaderCore<'_>) -> Option<&'static [u8]> {
    loader
        .provider
        .static_firmware_images()
        .iter()
        .find(|image| image.id == loader.config.base.id)
        .and_then(|image| image.bios)
}

fn load_uefi_firmware_image(loader: &ImageLoaderCore<'_>, firmware: &[u8]) -> AxVmResult {
    let load_gpa = loader
        .bios_load_gpa
        .ok_or_else(|| ax_err_type!(NotFound, "LoongArch UEFI firmware load addr is missed"))?;
    let flash_len = loader
        .config
        .kernel
        .memory_regions
        .iter()
        .find(|region| region.gpa == load_gpa.as_usize())
        .map_or(firmware.len(), |region| region.size);
    fill_vm_region(load_gpa, flash_len, 0xff, loader.vm.clone())?;
    load_vm_image_from_memory(firmware, load_gpa, loader.vm.clone())
}

fn fill_vm_region(load_addr: GuestPhysAddr, size: usize, byte: u8, vm: AxVMRef) -> AxVmResult {
    let regions = vm.get_image_load_region(load_addr, size)?;
    let mut filled_size = 0;
    for region in regions {
        // SAFETY: AxVM returned this writable guest-memory region and the fill
        // is bounded by its length.
        unsafe { core::ptr::write_bytes(region.as_mut_ptr(), byte, region.len()) };
        crate::clean_dcache_range((region.as_ptr() as usize).into(), region.len());
        filled_size += region.len();
    }
    if filled_size == size {
        Ok(())
    } else {
        ax_err!(
            InvalidData,
            format!("VM memory was only partially filled: {filled_size}/{size} bytes")
        )
    }
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
