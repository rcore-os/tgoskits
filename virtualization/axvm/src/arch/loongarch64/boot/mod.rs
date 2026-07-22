mod fdt;
mod resources;

#[cfg(any(feature = "fs", feature = "host-fs"))]
use alloc::boxed::Box;
use alloc::{format, vec::Vec};

use axdevice::{FwCfgMemoryConfig, FwCfgRamRegion};
use axvmconfig::{AxVMCrateConfig, VMBootProtocol};
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
    pub serial: Option<SerialDevice>,
    pub pci: PciHost,
    pub interrupt: InterruptTopology,
    pub fw_cfg: MmioRegion,
    pub firmware_devices: FirmwareDevices,
    pub irq_routes: Vec<LoongArchGuestIrqRoute>,
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
    pub fn discover(vm: &AxVMRef) -> AxVmResult<Self> {
        let memory = ram_regions(vm);
        vm.with_config(|config| guest_platform_from_plan(config, memory))
    }
}

pub fn load_firmware_fdt(vm: &AxVMRef, config: &AxVMCrateConfig) -> AxVmResult {
    let platform = GuestPlatform::discover(vm)?;
    let fdt = fdt::guest_firmware_dtb::build(&platform)?;
    debug!(
        "VM[{}] loading LoongArch UEFI firmware FDT: {} bytes at {:#x}",
        config.base.id,
        fdt.len(),
        UEFI_FIRMWARE_FDT_BASE
    );
    vm.with_config_mut(|config| {
        config.update_dtb_load_gpa(Some(GuestPhysAddr::from(UEFI_FIRMWARE_FDT_BASE)));
    });
    load_vm_image_from_memory(
        &fdt,
        GuestPhysAddr::from(UEFI_FIRMWARE_FDT_BASE),
        vm.clone(),
    )?;
    vm.set_guest_device_tree(GuestPhysAddr::from(UEFI_FIRMWARE_FDT_BASE), fdt)
}

pub fn guest_irq_routes(vm: &AxVMRef) -> AxVmResult<Vec<LoongArchGuestIrqRoute>> {
    Ok(GuestPlatform::discover(vm)?.irq_routes)
}

fn guest_platform_from_plan(
    config: &crate::config::AxVMConfig,
    ram_regions: Vec<MemoryRegion>,
) -> AxVmResult<GuestPlatform> {
    let controller = match config.machine_plan().interrupt_controller() {
        Some(crate::machine::InterruptControllerPlan::LoongArch(controller)) => controller,
        Some(_) => {
            return Err(crate::AxVmError::invalid_config(
                "LoongArch machine plan contains another architecture's controller",
            ));
        }
        None => {
            return Err(crate::AxVmError::invalid_config(
                "LoongArch machine plan has no interrupt controller",
            ));
        }
    };
    let machine = loongarch_platform_layout(config)?;
    let routing = controller.routing();
    let acpi = routing.acpi();
    let pci = machine.pci();
    let power = machine.power();
    let firmware = machine.firmware_devices();
    let serial = config
        .machine_plan()
        .virtual_devices()
        .iter()
        .find(|device| device.model_id().as_str() == "ns16550a")
        .map(|device| -> AxVmResult<_> {
            let mmio = device
                .mmio()
                .iter()
                .find(|resource| resource.slot().as_str() == "registers")
                .ok_or_else(|| {
                    crate::AxVmError::invalid_config("planned LoongArch serial has no registers")
                })?
                .range();
            let irq = device
                .interrupts()
                .iter()
                .find(|resource| resource.slot().as_str() == "irq")
                .ok_or_else(|| {
                    crate::AxVmError::invalid_config("planned LoongArch serial has no IRQ")
                })?
                .id();
            Ok(SerialDevice {
                mmio: MmioRegion {
                    base: mmio.base(),
                    size: mmio.size(),
                },
                irq,
                clock_hz: 100_000_000,
                baud: 115_200,
            })
        })
        .transpose()?;
    let ged_base = power.poweroff_register().min(power.reset_register());
    let ged_end = power
        .poweroff_register()
        .max(power.reset_register())
        .checked_add(1)
        .ok_or_else(|| crate::AxVmError::invalid_config("LoongArch GED range overflows"))?;
    let poweroff_offset = u32::try_from(power.poweroff_register() - ged_base)
        .map_err(|_| crate::AxVmError::invalid_config("LoongArch poweroff offset exceeds u32"))?;
    let reboot_offset = u32::try_from(power.reset_register() - ged_base)
        .map_err(|_| crate::AxVmError::invalid_config("LoongArch reset offset exceeds u32"))?;
    let irq_routes = if config.physical_interrupt_policy()
        == axvm_types::PhysicalInterruptPolicy::HardwareForwarded
    {
        config
            .machine_plan()
            .assigned_host_interrupts()
            .iter()
            .map(|interrupt| LoongArchGuestIrqRoute {
                physical_irq: interrupt.input().value(),
                guest_vector: interrupt.input().value(),
            })
            .collect()
    } else {
        Vec::new()
    };
    Ok(GuestPlatform {
        ram_regions,
        serial,
        pci: PciHost {
            ecam: MmioRegion {
                base: pci.ecam().base(),
                size: pci.ecam().size(),
            },
            mmio: MmioRegion {
                base: pci.mmio().base(),
                size: pci.mmio().size(),
            },
            io_base: pci.io().base(),
            io_size: pci.io().size(),
            intx_base: u32::from(pci.intx_base()),
        },
        interrupt: InterruptTopology {
            eiointc_irq: u32::from(routing.eiointc_irq()),
            pch_pic: MmioRegion {
                base: controller.pch_pic().base(),
                size: controller.pch_pic().size(),
            },
            pch_pic_gsi_base: routing.pch_pic_vector_base(),
            pch_msi: MmioRegion {
                base: controller.pch_msi().base(),
                size: controller.pch_msi().size(),
            },
            pch_msi_start: routing.pch_msi_vector_base(),
            pch_msi_count: routing.pch_msi_vector_count(),
            acpi_gsi_base: u32::from(acpi.pch_pic_gsi_base()),
            acpi_msi_start: acpi.pch_msi_start(),
            acpi_msi_count: acpi.pch_msi_count(),
        },
        fw_cfg: MmioRegion {
            base: machine.fw_cfg().base(),
            size: machine.fw_cfg().size(),
        },
        firmware_devices: FirmwareDevices {
            rtc: IrqMmioDevice {
                mmio: MmioRegion {
                    base: firmware.rtc().base(),
                    size: firmware.rtc().size(),
                },
                irq: firmware.rtc_interrupt(),
            },
            flash: FlashDevice {
                banks: firmware.flash_banks().map(|bank| MmioRegion {
                    base: bank.base(),
                    size: bank.size(),
                }),
                bank_width: firmware.flash_bank_width(),
            },
            ged: GedDevice {
                mmio: MmioRegion {
                    base: ged_base,
                    size: ged_end - ged_base,
                },
                poweroff_offset,
                poweroff_value: u32::from(power.poweroff_value()),
                reboot_offset,
                reboot_value: u32::from(power.reset_value()),
            },
        },
        irq_routes,
    })
}

fn loongarch_platform_layout(
    config: &crate::config::AxVMConfig,
) -> AxVmResult<&crate::machine::LoongArchPlatformPlan> {
    config.machine_plan().loongarch_platform().ok_or_else(|| {
        crate::AxVmError::invalid_config(
            "LoongArch VM machine plan has no firmware-facing platform resources",
        )
    })
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
    prepare_uefi_runtime_config(&loader.vm)?;
    load_firmware_fdt(&loader.vm, &loader.config)
}

fn add_uefi_fw_cfg(
    loader: &ImageLoaderCore<'_>,
    kernel: &'static [u8],
    ramdisk: Option<&'static [u8]>,
) -> AxVmResult {
    let fw_cfg = loader
        .vm
        .with_config(|config| loongarch_platform_layout(config).cloned())?;
    let acpi = loader
        .vm
        .with_config(|config| config.machine_plan().fw_cfg_acpi_firmware().cloned())
        .ok_or_else(|| {
            ax_err_type!(
                InvalidInput,
                "LoongArch machine plan has no generated fw_cfg ACPI files"
            )
        })?;
    loader.vm.add_fw_cfg_device(crate::FwCfgDeviceConfig {
        base: GuestPhysAddr::from(
            usize::try_from(fw_cfg.fw_cfg().base())
                .map_err(|_| ax_err_type!(InvalidInput, "fw_cfg base exceeds usize"))?,
        ),
        size: usize::try_from(fw_cfg.fw_cfg().size())
            .map_err(|_| ax_err_type!(InvalidInput, "fw_cfg size exceeds usize"))?,
        kernel,
        initrd: ramdisk,
        cmdline: loader.config.kernel.cmdline.clone(),
        cpu_num: loader.config.base.cpu_num as u16,
        memory: FwCfgMemoryConfig {
            ram_regions: ram_regions(&loader.vm)
                .into_iter()
                .map(|region| FwCfgRamRegion {
                    base: region.base,
                    size: region.size,
                })
                .collect(),
        },
        acpi,
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
        .memory
        .regions
        .iter()
        .find(|region| region.guest_base == load_gpa.as_usize() as u64)
        .map(|region| usize::try_from(region.size))
        .transpose()
        .map_err(|_| ax_err_type!(InvalidInput, "firmware memory region size exceeds usize"))?
        .unwrap_or(firmware.len());
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
