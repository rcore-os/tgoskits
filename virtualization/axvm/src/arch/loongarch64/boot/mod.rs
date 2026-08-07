mod acpi;
mod fdt;
pub(super) mod probe;
mod resources;

use std::{format, sync::Arc, vec::Vec};

use axdevice::{FwCfgKernelPayload, FwCfgPlatformConfig, FwCfgRamRegion, ResourceSlot};
use axvmconfig::{GuestConfig, VMBootProtocol};
pub use resources::{
    LoongArchGuestIrqRoute, get_guest_irq_routes, prepare_uefi_fdt_config,
    prepare_uefi_runtime_config,
};

use crate::{
    architecture::*,
    boot::{images::*, *},
    *,
};

pub const UEFI_FIRMWARE_FDT_BASE: usize = 0x0010_0000;

pub struct ImageLoader<'a>(ImageLoaderCore<'a>);

impl<'a> ImageLoader<'a> {
    pub fn new(
        main_memory: crate::VMMemoryRegion,
        config: GuestConfig,
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
    pub fn discover(vm: &AxVMRef, _config: &GuestConfig) -> AxVmResult<Self> {
        let fw_cfg = resolved_fw_cfg(vm)?;
        let serial = resolved_serial(vm)?;
        Ok(
            probe::GuestPlatformBuilder::new(ram_regions(vm), Some(fw_cfg))
                .with_serial(serial)
                .apply_host_acpi()
                .build(),
        )
    }

    pub fn fw_cfg_platform_config(&self, cpu_num: u16) -> AxVmResult<FwCfgPlatformConfig> {
        let ram_regions = fw_cfg_ram_regions(&self.ram_regions);
        let acpi = acpi::build(cpu_num, self, &ram_regions).map_err(|error| {
            crate::AxVmError::invalid_config(std::format!(
                "failed to build LoongArch guest ACPI: {error}"
            ))
        })?;
        Ok(FwCfgPlatformConfig {
            ram_regions: ram_regions.clone(),
            srat_regions: ram_regions,
            acpi,
        })
    }
}

fn resolved_serial(vm: &AxVMRef) -> AxVmResult<SerialDevice> {
    vm.with_planned_device_graph(|graph| {
        let serials = crate::machine::resolved_serial_devices(graph)?;
        let serial = serials
            .iter()
            .find(|serial| serial.id() == "console0")
            .ok_or_else(|| AxVmError::invalid_config("LoongArch plan has no console0"))?
            .profile();
        let crate::machine::GuestSerialTransport::Mmio { base, length, .. } = serial.transport
        else {
            return Err(AxVmError::unsupported(
                "build LoongArch guest firmware",
                "LoongArch console0 must use MMIO",
            ));
        };
        Ok(SerialDevice {
            mmio: MmioRegion {
                base: base as u64,
                size: length as u64,
            },
            irq: u32::try_from(serial.irq)
                .map_err(|_| AxVmError::invalid_config("LoongArch console IRQ exceeds u32"))?,
            clock_hz: serial.clock_hz,
            baud: 115_200,
        })
    })
}

pub fn load_firmware_fdt(vm: &AxVMRef, config: &GuestConfig) -> AxVmResult {
    let platform = GuestPlatform::discover(vm, config)?;
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

pub fn fw_cfg_platform_config(
    vm: &AxVMRef,
    config: &GuestConfig,
) -> AxVmResult<FwCfgPlatformConfig> {
    GuestPlatform::discover(vm, config)?.fw_cfg_platform_config(config.base.cpu_num as u16)
}

pub fn guest_irq_routes(
    vm: &AxVMRef,
    config: &GuestConfig,
) -> AxVmResult<Vec<LoongArchGuestIrqRoute>> {
    Ok(GuestPlatform::discover(vm, config)?
        .irq_routes
        .into_iter()
        .map(|route| LoongArchGuestIrqRoute {
            physical_irq: route.physical_irq,
            guest_vector: route.guest_vector,
        })
        .collect())
}

fn resolved_fw_cfg(vm: &AxVMRef) -> AxVmResult<MmioRegion> {
    vm.with_planned_device_graph(|graph| {
        let node = graph
            .nodes()
            .find(|node| node.id().as_str() == "fw-cfg")
            .ok_or_else(|| ax_err_type!(NotFound, "LoongArch UEFI boot requires an fw_cfg node"))?;
        let (base, size) = graph
            .resources_for(node.id())?
            .mmio(&ResourceSlot::new("registers")?)?;
        Ok(MmioRegion { base, size })
    })
}

impl BootImagePlatform for super::LoongArch64Arch {
    fn load_images_from_memory(
        loader: &mut ImageLoaderCore<'_>,
        images: StaticVmImage,
    ) -> AxVmResult {
        ensure_uefi_boot(loader)?;
        load_uefi_firmware_dtb(loader)?;
        add_uefi_fw_cfg(
            loader,
            Arc::from(images.kernel),
            images.ramdisk.map(Arc::from),
        )?;
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
        let kernel = Arc::from(kernel);
        let ramdisk = if let Some(path) = &loader.config.kernel.ramdisk_path {
            let ramdisk = crate::boot::images::fs::read_full_image(path, loader.provider)?;
            Some(Arc::from(ramdisk))
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
    prepare_uefi_runtime_config(&loader.vm, &loader.config)?;
    load_firmware_fdt(&loader.vm, &loader.config)
}

fn add_uefi_fw_cfg(
    loader: &ImageLoaderCore<'_>,
    kernel: Arc<[u8]>,
    ramdisk: Option<Arc<[u8]>>,
) -> AxVmResult {
    let fw_cfg = resolved_fw_cfg(&loader.vm)?;
    loader.vm.add_fw_cfg_device(crate::FwCfgDeviceConfig {
        base: GuestPhysAddr::from(
            usize::try_from(fw_cfg.base)
                .map_err(|_| crate::AxVmError::invalid_config("fw_cfg GPA does not fit usize"))?,
        ),
        size: usize::try_from(fw_cfg.size)
            .map_err(|_| crate::AxVmError::invalid_config("fw_cfg size does not fit usize"))?,
        kernel: FwCfgKernelPayload::unsplit(kernel),
        initrd: ramdisk,
        cmdline: loader.config.kernel.cmdline.clone(),
        cpu_num: loader.config.base.cpu_num as u16,
        platform: fw_cfg_platform_config(&loader.vm, &loader.config)?,
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
        unsafe { std::ptr::write_bytes(region.as_mut_ptr(), byte, region.len()) };
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

fn fw_cfg_ram_regions(regions: &[MemoryRegion]) -> Arc<[FwCfgRamRegion]> {
    let regions = regions
        .iter()
        .map(|region| FwCfgRamRegion {
            base: region.base,
            size: region.size,
        })
        .collect::<Vec<_>>();
    regions.into()
}
