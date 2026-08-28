mod acpi;
mod fdt;
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
        let (fdt_firmware, acpi_firmware) = vm.with_planned_device_graph(|graph| {
            Ok((
                crate::boot::fdt::device::resolve_fdt_firmware(graph)?,
                crate::boot::acpi::resolve_acpi_firmware(graph)?,
            ))
        })?;
        let special =
            resolve_special_firmware(&fdt_firmware.specials, &acpi_firmware.specials, serial)?;
        let mut platform = probe::GuestPlatformBuilder::new(ram_regions(vm), Some(special.fw_cfg))
            .with_serial(serial)
            .apply_host_acpi()
            .build();
        platform.interrupt.controller = special.controller;
        platform.interrupt.pch_pic = special.pch_pic;
        platform.configured_fdt_devices = fdt_firmware.devices;
        platform.configured_acpi_devices = acpi_firmware.devices;
        Ok(platform)
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

struct LoongArchSpecialFirmware {
    controller: InterruptControllerId,
    pch_pic: MmioRegion,
    fw_cfg: MmioRegion,
}

fn resolve_special_firmware(
    fdt: &[crate::boot::fdt::device::ResolvedFdtSpecial],
    acpi: &[crate::boot::acpi::ResolvedAcpiSpecial],
    serial: SerialDevice,
) -> AxVmResult<LoongArchSpecialFirmware> {
    use crate::boot::{
        acpi::{ResolvedAcpiProperty, ResolvedAcpiRegister, ResolvedAcpiSpecialKind},
        fdt::device::{ResolvedFdtProperty, ResolvedFdtSpecialKind},
    };

    if fdt.len() != 3 || acpi.len() != 3 {
        return Err(AxVmError::unsupported(
            "resolve LoongArch firmware topology",
            std::format!(
                "expected interrupt-controller, console, and fw_cfg contributions in both FDT and \
                 ACPI; found {} FDT and {} ACPI",
                fdt.len(),
                acpi.len()
            ),
        ));
    }

    let fdt_controller = single_fdt_special(
        fdt,
        |kind| matches!(kind, ResolvedFdtSpecialKind::InterruptController(_)),
        "interrupt controller",
    )?;
    let ResolvedFdtSpecialKind::InterruptController(controller) = fdt_controller.kind else {
        unreachable!("the special selector checked the contribution kind")
    };
    let acpi_controller = single_acpi_special(
        acpi,
        |kind| matches!(kind, ResolvedAcpiSpecialKind::InterruptController(_)),
        "interrupt controller",
    )?;
    if acpi_controller.kind != ResolvedAcpiSpecialKind::InterruptController(controller) {
        return Err(AxVmError::invalid_config(
            "LoongArch FDT and ACPI interrupt-controller identities differ",
        ));
    }
    let [pch_pic] = fdt_controller.registers.as_slice() else {
        return Err(AxVmError::invalid_config(
            "LoongArch FDT PCH-PIC contribution must resolve one MMIO window",
        ));
    };
    let [
        ResolvedAcpiRegister::Mmio {
            base: acpi_pic_base,
            size: acpi_pic_size,
        },
    ] = acpi_controller.registers.as_slice()
    else {
        return Err(AxVmError::invalid_config(
            "LoongArch ACPI PCH-PIC contribution must resolve one MMIO window",
        ));
    };
    if *pch_pic != (*acpi_pic_base, *acpi_pic_size)
        || fdt_controller.node_name != "interrupt-controller"
        || fdt_controller.compatible.len() != 1
        || fdt_controller
            .compatible
            .first()
            .is_none_or(|compatible| compatible != "loongson,pch-pic-1.0")
        || !fdt_controller.interrupts.is_empty()
        || !fdt_controller.properties.is_empty()
        || acpi_controller.name != "PCH0"
        || acpi_controller.hid.is_some()
        || !acpi_controller.interrupts.is_empty()
        || !acpi_controller.properties.is_empty()
    {
        return Err(AxVmError::invalid_config(
            "LoongArch PCH-PIC FDT and ACPI contributions disagree",
        ));
    }

    let fdt_fw_cfg = single_fdt_special(
        fdt,
        |kind| kind == ResolvedFdtSpecialKind::FirmwareTransport,
        "firmware transport",
    )?;
    let acpi_fw_cfg = single_acpi_special(
        acpi,
        |kind| kind == ResolvedAcpiSpecialKind::FirmwareTransport,
        "firmware transport",
    )?;
    let [fw_cfg] = fdt_fw_cfg.registers.as_slice() else {
        return Err(AxVmError::invalid_config(
            "LoongArch FDT fw_cfg contribution must resolve one MMIO window",
        ));
    };
    let [
        ResolvedAcpiRegister::Mmio {
            base: acpi_fw_cfg_base,
            size: acpi_fw_cfg_size,
        },
    ] = acpi_fw_cfg.registers.as_slice()
    else {
        return Err(AxVmError::invalid_config(
            "LoongArch ACPI fw_cfg contribution must resolve one MMIO window",
        ));
    };
    if *fw_cfg != (*acpi_fw_cfg_base, *acpi_fw_cfg_size)
        || fdt_fw_cfg.node_name != "fw_cfg"
        || fdt_fw_cfg.compatible.len() != 1
        || fdt_fw_cfg
            .compatible
            .first()
            .is_none_or(|compatible| compatible != "qemu,fw-cfg-mmio")
        || !fdt_fw_cfg.interrupts.is_empty()
        || !matches!(
            fdt_fw_cfg.properties.as_slice(),
            [ResolvedFdtProperty::Empty(name)] if name == "dma-coherent"
        )
        || acpi_fw_cfg.name != "FWCF"
        || acpi_fw_cfg.hid.as_deref() != Some("QEMU0002")
        || !acpi_fw_cfg.interrupts.is_empty()
        || !acpi_fw_cfg.properties.is_empty()
    {
        return Err(AxVmError::invalid_config(
            "LoongArch fw_cfg FDT and ACPI contributions disagree",
        ));
    }

    let fdt_console = single_fdt_special(
        fdt,
        |kind| kind == ResolvedFdtSpecialKind::Console,
        "console",
    )?;
    let acpi_console = single_acpi_special(
        acpi,
        |kind| kind == ResolvedAcpiSpecialKind::Console,
        "console",
    )?;
    let expected_serial = (serial.mmio.base, serial.mmio.size);
    let [fdt_serial] = fdt_console.registers.as_slice() else {
        return Err(AxVmError::invalid_config(
            "LoongArch FDT console contribution must resolve one MMIO window",
        ));
    };
    let [
        ResolvedAcpiRegister::Mmio {
            base: acpi_serial_base,
            size: acpi_serial_size,
        },
    ] = acpi_console.registers.as_slice()
    else {
        return Err(AxVmError::invalid_config(
            "LoongArch ACPI console contribution must resolve one MMIO window",
        ));
    };
    let [fdt_console_irq] = fdt_console.interrupts.as_slice() else {
        return Err(AxVmError::invalid_config(
            "LoongArch FDT console contribution must resolve one interrupt",
        ));
    };
    let [acpi_console_irq] = acpi_console.interrupts.as_slice() else {
        return Err(AxVmError::invalid_config(
            "LoongArch ACPI console contribution must resolve one interrupt",
        ));
    };
    if *fdt_serial != expected_serial
        || (*acpi_serial_base, *acpi_serial_size) != expected_serial
        || fdt_console.node_name != "serial"
        || fdt_console.compatible.len() != 1
        || fdt_console
            .compatible
            .first()
            .is_none_or(|compatible| compatible != "ns16550a")
        || fdt_console_irq.controller != controller
        || acpi_console_irq.controller != controller
        || fdt_console_irq.input != serial.irq
        || acpi_console_irq.input != serial.irq
        || !matches!(
            fdt_console.properties.as_slice(),
            [
                ResolvedFdtProperty::U32(clock_name, clock_hz),
                ResolvedFdtProperty::U32(shift_name, register_shift),
                ResolvedFdtProperty::U32(width_name, register_width),
            ] if clock_name == "clock-frequency"
                && *clock_hz == serial.clock_hz
                && shift_name == "reg-shift"
                && *register_shift == u32::from(serial.register_shift)
                && width_name == "reg-io-width"
                && *register_width == u32::try_from(serial.register_width.size())
                    .expect("a serial access width is at most eight bytes")
        )
        || acpi_console.name != "COM0"
        || acpi_console.hid.as_deref() != Some("PNP0501")
        || !matches!(
            acpi_console.properties.as_slice(),
            [ResolvedAcpiProperty::U32(name, clock_hz)]
                if name == "clock-frequency" && *clock_hz == serial.clock_hz
        )
    {
        return Err(AxVmError::invalid_config(
            "LoongArch console FDT, ACPI, and runtime resources disagree",
        ));
    }

    Ok(LoongArchSpecialFirmware {
        controller,
        pch_pic: MmioRegion {
            base: pch_pic.0,
            size: pch_pic.1,
        },
        fw_cfg: MmioRegion {
            base: fw_cfg.0,
            size: fw_cfg.1,
        },
    })
}

fn single_fdt_special<'a>(
    specials: &'a [crate::boot::fdt::device::ResolvedFdtSpecial],
    predicate: impl Fn(crate::boot::fdt::device::ResolvedFdtSpecialKind) -> bool,
    name: &'static str,
) -> AxVmResult<&'a crate::boot::fdt::device::ResolvedFdtSpecial> {
    let mut matches = specials.iter().filter(|special| predicate(special.kind));
    let special = matches.next().ok_or_else(|| {
        AxVmError::invalid_config(std::format!("LoongArch FDT has no {name} contribution"))
    })?;
    if matches.next().is_some() {
        return Err(AxVmError::unsupported(
            "resolve LoongArch FDT topology",
            std::format!("multiple {name} contributions are not supported"),
        ));
    }
    Ok(special)
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
    vm.with_planned_device_graph(|graph| {
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
