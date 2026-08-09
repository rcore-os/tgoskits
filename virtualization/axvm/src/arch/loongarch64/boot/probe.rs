use std::vec::Vec;

use ax_std::os::arceos::driver as ax_driver;

use super::{
    FirmwareDevices, FlashDevice, GedDevice, GuestPlatform, InterruptTopology, IrqMmioDevice,
    MemoryRegion, MmioRegion, PciHost, SerialDevice,
    acpi::{LoongArchFwCfgInterruptConfig, LoongArchFwCfgPciConfig, LoongArchFwCfgSerialConfig},
};

pub struct GuestPlatformBuilder {
    ram_regions: Vec<MemoryRegion>,
    fw_cfg: Option<MmioRegion>,
    serial: Option<SerialDevice>,
    pci: Option<PciHost>,
    interrupt: Option<InterruptTopology>,
    firmware_devices: Option<FirmwareDevices>,
    irq_routes: Vec<GuestIrqRoute>,
}

pub(crate) fn apply_host_serial(config: &mut crate::config::AxVMConfig) -> crate::AxVmResult {
    let Some(serial) = ax_driver::probe::acpi::with_acpi(|acpi| acpi.serial_console()) else {
        return Ok(());
    };
    let Some(serial) = serial.map_err(|error| {
        crate::AxVmError::invalid_config(std::format!(
            "failed to parse host ACPI serial console: {error}"
        ))
    })?
    else {
        return Ok(());
    };
    let snapshot = crate::machine::host_serial_from_acpi(serial, config.serial_profile())?;
    if matches!(
        snapshot.profile.transport,
        crate::machine::GuestSerialTransport::Port { .. }
    ) {
        return Err(crate::AxVmError::unsupported(
            "replace LoongArch host serial",
            "LoongArch guests require an MMIO serial console",
        ));
    }
    config.replace_machine_serial(snapshot.profile, Some(snapshot.identity))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GuestIrqRoute {
    pub physical_irq: usize,
    pub guest_vector: usize,
}

impl GuestPlatformBuilder {
    pub fn new(ram_regions: Vec<MemoryRegion>, fw_cfg: Option<MmioRegion>) -> Self {
        Self {
            ram_regions,
            fw_cfg,
            serial: None,
            pci: None,
            interrupt: None,
            firmware_devices: None,
            irq_routes: Vec::new(),
        }
    }

    pub fn with_serial(mut self, serial: SerialDevice) -> Self {
        self.serial = Some(serial);
        self
    }

    pub fn apply_host_acpi(mut self) -> Self {
        if let Some(result) = ax_driver::probe::acpi::with_acpi(host_acpi_resources) {
            match result {
                Ok(resources) => self.apply_host_resources(resources),
                Err(err) => warn!("failed to collect LoongArch host ACPI resources: {err:?}"),
            }
        }
        self
    }

    pub fn build(self) -> GuestPlatform {
        let defaults = QemuVirtDefaults::new();
        let serial = self.serial.unwrap_or(defaults.serial);
        let pci = self.pci.unwrap_or(defaults.pci);
        let interrupt = self.interrupt.unwrap_or(defaults.interrupt);

        let irq_routes = if self.irq_routes.is_empty() {
            guest_irq_routes(&interrupt, &Some(serial), &Some(pci))
        } else {
            self.irq_routes
        };
        GuestPlatform {
            ram_regions: self.ram_regions,
            serial,
            pci,
            interrupt,
            fw_cfg: self.fw_cfg.unwrap_or(defaults.fw_cfg),
            firmware_devices: self.firmware_devices.unwrap_or(defaults.firmware_devices),
            irq_routes,
        }
    }

    fn apply_host_resources(&mut self, resources: HostResources) {
        if let Some(pci) = resources.pci {
            self.pci = Some(pci);
        }
        if let Some(interrupt) = resources.interrupt {
            self.interrupt = Some(interrupt);
        }
        if let Some(firmware_devices) = resources.firmware_devices {
            self.firmware_devices = Some(firmware_devices);
        }
        self.irq_routes = resources.irq_routes;
    }
}

struct HostResources {
    pci: Option<PciHost>,
    interrupt: Option<InterruptTopology>,
    firmware_devices: Option<FirmwareDevices>,
    irq_routes: Vec<GuestIrqRoute>,
}

fn host_acpi_resources(
    acpi: &ax_driver::probe::acpi::System,
) -> axvm_types::VmBackendResult<HostResources> {
    let defaults = QemuVirtDefaults::new();
    let interrupt = acpi
        .routing()
        .pch_pics()
        .first()
        .map(|pch_pic| InterruptTopology {
            eiointc_irq: defaults.interrupt.eiointc_irq,
            pch_pic: MmioRegion {
                base: pch_pic.address,
                size: effective_pch_pic_size(pch_pic.mmio_size),
            },
            pch_pic_gsi_base: 0,
            pch_msi: defaults.interrupt.pch_msi,
            pch_msi_start: defaults.interrupt.pch_msi_start,
            pch_msi_count: defaults.interrupt.pch_msi_count,
            acpi_gsi_base: pch_pic.gsi_base,
            acpi_msi_start: pch_pic.gsi_base,
            acpi_msi_count: defaults.interrupt.acpi_msi_count,
        });

    let pci = acpi.pci_ecam_regions().first().map(|ecam| PciHost {
        ecam: MmioRegion {
            base: ecam.base_address,
            size: ecam.size() as u64,
        },
        mmio: defaults.pci.mmio,
        io_base: defaults.pci.io_base,
        io_size: defaults.pci.io_size,
        intx_base: defaults.pci.intx_base,
    });

    let firmware_devices = Some(find_firmware_devices(acpi, defaults.firmware_devices));

    Ok(HostResources {
        pci,
        interrupt,
        firmware_devices,
        irq_routes: Vec::new(),
    })
}

fn effective_pch_pic_size(size: u16) -> u64 {
    if size == 0 { 0x1000 } else { u64::from(size) }
}

fn find_firmware_devices(
    acpi: &ax_driver::probe::acpi::System,
    mut devices: FirmwareDevices,
) -> FirmwareDevices {
    if let Some(rtc) = find_rtc(acpi) {
        devices.rtc = rtc;
    }
    devices
}

fn find_rtc(acpi: &ax_driver::probe::acpi::System) -> Option<IrqMmioDevice> {
    let devices = acpi.resource_devices().ok()?;
    devices.into_iter().find_map(|device| {
        let is_rtc = device.hid.as_deref() == Some("LOON0001")
            || device.cids.iter().any(|cid| cid == "LOON0001")
            || device.path.contains("RTC");
        if !is_rtc {
            return None;
        }
        let range = device.memory_ranges.first()?;
        let irq = device
            .irq_routes
            .first()
            .map(|route| u32::from(route.controller_input))
            .unwrap_or(defaults_rtc_irq());
        Some(IrqMmioDevice {
            mmio: MmioRegion {
                base: range.base,
                size: range.size,
            },
            irq,
        })
    })
}

fn defaults_rtc_irq() -> u32 {
    6
}

fn guest_irq_routes(
    interrupt: &InterruptTopology,
    serial: &Option<SerialDevice>,
    pci: &Option<PciHost>,
) -> Vec<GuestIrqRoute> {
    let defaults = QemuVirtDefaults::new();
    let serial = serial.unwrap_or(defaults.serial);
    let pci = pci.unwrap_or(defaults.pci);

    let mut routes = Vec::from([GuestIrqRoute {
        physical_irq: serial.irq as usize,
        guest_vector: serial.irq as usize,
    }]);
    routes.extend((0..4).map(|idx| GuestIrqRoute {
        physical_irq: pci.intx_base as usize + idx,
        guest_vector: pci.intx_base as usize + idx,
    }));

    let _ = interrupt;
    routes
}

struct QemuVirtDefaults {
    serial: SerialDevice,
    pci: PciHost,
    interrupt: InterruptTopology,
    fw_cfg: MmioRegion,
    firmware_devices: FirmwareDevices,
}

impl QemuVirtDefaults {
    fn new() -> Self {
        let serial = SerialDevice {
            mmio: MmioRegion {
                base: LoongArchFwCfgSerialConfig::default().base,
                size: LoongArchFwCfgSerialConfig::default().size,
            },
            irq: 2,
            clock_hz: LoongArchFwCfgSerialConfig::default().clock_hz,
            baud: LoongArchFwCfgSerialConfig::default().baud,
        };
        let pci = PciHost {
            ecam: MmioRegion {
                base: LoongArchFwCfgPciConfig::default().ecam_base,
                size: LoongArchFwCfgPciConfig::default().ecam_size,
            },
            mmio: MmioRegion {
                base: LoongArchFwCfgPciConfig::default().mmio_base,
                size: LoongArchFwCfgPciConfig::default().mmio_size,
            },
            io_base: LoongArchFwCfgPciConfig::default().io_base,
            io_size: u64::from(LoongArchFwCfgPciConfig::default().io_size),
            intx_base: 16,
        };
        let interrupt = InterruptTopology {
            eiointc_irq: LoongArchFwCfgInterruptConfig::default().eiointc_irq as u32,
            pch_pic: MmioRegion {
                base: LoongArchFwCfgInterruptConfig::default().pch_pic_base,
                size: u64::from(LoongArchFwCfgInterruptConfig::default().pch_pic_size),
            },
            pch_pic_gsi_base: 0,
            pch_msi: MmioRegion {
                base: LoongArchFwCfgInterruptConfig::default().pch_msi_base,
                size: 0x8,
            },
            pch_msi_start: 0x20,
            pch_msi_count: 0xe0,
            acpi_gsi_base: u32::from(LoongArchFwCfgInterruptConfig::default().pch_pic_gsi_base),
            acpi_msi_start: LoongArchFwCfgInterruptConfig::default().pch_msi_start,
            acpi_msi_count: LoongArchFwCfgInterruptConfig::default().pch_msi_count,
        };
        Self {
            serial,
            pci,
            interrupt,
            fw_cfg: MmioRegion {
                base: 0x1e02_0000,
                size: 0x18,
            },
            firmware_devices: FirmwareDevices {
                rtc: IrqMmioDevice {
                    mmio: MmioRegion {
                        base: 0x100d_0100,
                        size: 0x100,
                    },
                    irq: defaults_rtc_irq(),
                },
                flash: FlashDevice {
                    banks: [
                        MmioRegion {
                            base: 0x1c00_0000,
                            size: 0x0100_0000,
                        },
                        MmioRegion {
                            base: 0x1d00_0000,
                            size: 0x0100_0000,
                        },
                    ],
                    bank_width: 4,
                },
                ged: GedDevice {
                    mmio: MmioRegion {
                        base: 0x100e_001c,
                        size: 3,
                    },
                    poweroff_offset: 0,
                    poweroff_value: 0x34,
                    reboot_offset: 2,
                    reboot_value: 0x42,
                },
            },
        }
    }
}
