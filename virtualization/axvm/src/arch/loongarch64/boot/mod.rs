mod acpi;
mod fdt;
mod firmware;
pub(super) mod probe;
mod resources;

use std::{format, sync::Arc, vec::Vec};

use axdevice::{FwCfgKernelPayload, FwCfgPlatformConfig, FwCfgRamRegion};
use axdevice_base::InterruptControllerId;
use axvmconfig::{GuestConfig, VMBootProtocol};
pub(crate) use resources::{
    LoongArchGuestIrqRoute, get_guest_irq_routes, prepare_uefi_fdt_config,
    prepare_uefi_runtime_config,
};

use crate::{
    architecture::*,
    boot::{images::*, *},
    *,
};

pub(crate) const UEFI_FIRMWARE_FDT_BASE: usize = 0x0010_0000;
pub(in crate::arch::loongarch64) use firmware::{GuestFirmwareSelection, select_guest_firmware};

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
    pub(crate) configured_fdt_devices: Vec<crate::boot::fdt::device::ResolvedFdtDevice>,
    pub(crate) configured_acpi_devices: Vec<crate::boot::acpi::ResolvedAcpiDevice>,
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
    pub register_shift: u8,
    pub register_width: axdevice_base::AccessWidth,
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
    pub controller: InterruptControllerId,
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
        let serial = resolved_serial(vm)?;
        let firmware = vm.with_config(|config| select_guest_firmware(config))?;
        let ram_regions = ram_regions(vm);
        vm.with_planned_device_graph(|graph| {
            assemble_guest_platform(graph, firmware, serial, ram_regions, |builder| {
                builder.apply_host_acpi().build()
            })
        })
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

fn assemble_guest_platform(
    graph: &axdevice::ResolvedDeviceGraph,
    firmware: GuestFirmwareSelection,
    serial: SerialDevice,
    ram_regions: Vec<MemoryRegion>,
    build_platform: impl FnOnce(probe::GuestPlatformBuilder) -> AxVmResult<GuestPlatform>,
) -> AxVmResult<GuestPlatform> {
    let (fdt_firmware, acpi_firmware) = if firmware.uses_acpi() {
        let acpi = crate::boot::acpi::resolve_acpi_firmware(graph)?;
        let fdt = crate::boot::fdt::device::resolve_available_fdt_firmware(graph)?;
        (fdt, Some(acpi))
    } else {
        (crate::boot::fdt::device::resolve_fdt_firmware(graph)?, None)
    };
    let special = if let Some(acpi) = &acpi_firmware {
        resolve_special_firmware(&fdt_firmware.specials, &acpi.specials, serial)?
    } else {
        resolve_fdt_special_firmware(&fdt_firmware.specials, serial)?
    };
    let builder = probe::GuestPlatformBuilder::new(ram_regions, Some(special.fw_cfg), firmware)
        .with_serial(serial);
    let mut platform = build_platform(builder)?;
    if let Some(ecam) = special.pci_ecam {
        reconcile_pci_ecam(&mut platform.pci, ecam)?;
    }
    platform.interrupt.controller = special.controller;
    platform.interrupt.pch_pic = special.pch_pic;
    platform.configured_fdt_devices = fdt_firmware.devices;
    platform.configured_acpi_devices = acpi_firmware
        .map(|firmware| firmware.devices)
        .unwrap_or_default();
    Ok(platform)
}

struct LoongArchSpecialFirmware {
    controller: InterruptControllerId,
    pch_pic: MmioRegion,
    fw_cfg: MmioRegion,
    pci_ecam: Option<MmioRegion>,
}

fn resolve_special_firmware(
    fdt: &[crate::boot::fdt::device::ResolvedFdtSpecial],
    acpi: &[crate::boot::acpi::ResolvedAcpiSpecial],
    serial: SerialDevice,
) -> AxVmResult<LoongArchSpecialFirmware> {
    if fdt.len() != 3 || acpi.len() != 4 {
        return Err(AxVmError::unsupported(
            "resolve LoongArch firmware topology",
            std::format!(
                "expected interrupt-controller, console, and fw_cfg contributions in FDT and \
                 those contributions plus PCI in ACPI; found {} FDT and {} ACPI",
                fdt.len(),
                acpi.len()
            ),
        ));
    }

    let common = firmware::resolve_fdt_common_firmware(fdt, serial)?;
    firmware::cross_check_acpi_common_firmware(common, acpi, serial)?;
    Ok(LoongArchSpecialFirmware {
        controller: common.controller,
        pch_pic: common.pch_pic,
        fw_cfg: common.fw_cfg,
        pci_ecam: Some(resolve_pci_ecam(acpi)?),
    })
}

fn resolve_fdt_special_firmware(
    fdt: &[crate::boot::fdt::device::ResolvedFdtSpecial],
    serial: SerialDevice,
) -> AxVmResult<LoongArchSpecialFirmware> {
    let common = firmware::resolve_fdt_common_firmware(fdt, serial)?;
    Ok(LoongArchSpecialFirmware {
        controller: common.controller,
        pch_pic: common.pch_pic,
        fw_cfg: common.fw_cfg,
        pci_ecam: None,
    })
}

fn resolve_pci_ecam(acpi: &[crate::boot::acpi::ResolvedAcpiSpecial]) -> AxVmResult<MmioRegion> {
    use crate::boot::acpi::{ResolvedAcpiRegister, ResolvedAcpiSpecialKind};

    let pci = single_acpi_special(
        acpi,
        |kind| kind == ResolvedAcpiSpecialKind::PciHostBridge,
        "PCI host bridge",
    )?;
    let [ResolvedAcpiRegister::Mmio { base, size }] = pci.registers.as_slice() else {
        return Err(AxVmError::invalid_config(
            "LoongArch ACPI PCI0 contribution must resolve exactly one MMIO window",
        ));
    };
    if pci.name != "PCI0"
        || pci.hid.as_deref() != Some("PNP0A08")
        || !pci.interrupts.is_empty()
        || !pci.properties.is_empty()
    {
        return Err(AxVmError::invalid_config(
            "LoongArch ACPI PCI host contribution must be PCI0/PNP0A08 without interrupts or \
             properties",
        ));
    }
    axdevice::PciEcamDevice::new(*base, *size).map_err(|error| {
        AxVmError::invalid_config(std::format!(
            "invalid resolved LoongArch PCI ECAM at base {base:#x}, size {size:#x}: {error}"
        ))
    })?;
    Ok(MmioRegion {
        base: *base,
        size: *size,
    })
}

fn reconcile_pci_ecam(pci: &mut PciHost, resolved: MmioRegion) -> AxVmResult {
    let profile = pci.ecam;
    if profile != resolved {
        return Err(AxVmError::invalid_config(std::format!(
            "normalized LoongArch PCI ECAM range {:#x}..{:#x} differs from graph-resolved range \
             {:#x}..{:#x}",
            profile.base,
            profile.base.saturating_add(profile.size),
            resolved.base,
            resolved.base.saturating_add(resolved.size),
        )));
    }
    pci.ecam = resolved;
    Ok(())
}

fn single_acpi_special<'a>(
    specials: &'a [crate::boot::acpi::ResolvedAcpiSpecial],
    predicate: impl Fn(crate::boot::acpi::ResolvedAcpiSpecialKind) -> bool,
    name: &'static str,
) -> AxVmResult<&'a crate::boot::acpi::ResolvedAcpiSpecial> {
    let mut matches = specials.iter().filter(|special| predicate(special.kind));
    let special = matches.next().ok_or_else(|| {
        AxVmError::invalid_config(std::format!("LoongArch ACPI has no {name} contribution"))
    })?;
    if matches.next().is_some() {
        return Err(AxVmError::unsupported(
            "resolve LoongArch ACPI topology",
            std::format!("multiple {name} contributions are not supported"),
        ));
    }
    Ok(special)
}

fn resolved_serial(vm: &AxVMRef) -> AxVmResult<SerialDevice> {
    vm.with_planned_device_graph(resolved_serial_from_graph)
}

fn resolved_serial_from_graph(graph: &axdevice::ResolvedDeviceGraph) -> AxVmResult<SerialDevice> {
    let serials = crate::machine::resolved_serial_devices(graph)?;
    let serial = serials
        .iter()
        .find(|serial| serial.id() == "console0")
        .ok_or_else(|| AxVmError::invalid_config("LoongArch plan has no console0"))?
        .profile();
    let crate::machine::GuestSerialTransport::Mmio {
        base,
        length,
        register_shift,
        register_width,
    } = serial.transport
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
        register_shift,
        register_width,
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

impl BootImagePlatform for super::LoongArch64Arch {
    fn make_guest_memory_visible(addr: ax_memory_addr::VirtAddr, size: usize) {
        super::make_guest_memory_visible(addr, size);
    }

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
    let platform = GuestPlatform::discover(&loader.vm, &loader.config)?;
    let fw_cfg = platform.fw_cfg;
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
        platform: platform.fw_cfg_platform_config(loader.config.base.cpu_num as u16)?,
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
        crate::arch::current::make_guest_memory_visible(
            (region.as_ptr() as usize).into(),
            region.len(),
        );
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

#[cfg(test)]
mod tests;
