use axdevice_base::{InterruptControllerId, InterruptSharing, InterruptTrigger};

use super::*;
use crate::{
    boot::{
        acpi::{
            ResolvedAcpiInterrupt, ResolvedAcpiProperty, ResolvedAcpiRegister, ResolvedAcpiSpecial,
            ResolvedAcpiSpecialKind,
        },
        fdt::device::{
            ResolvedFdtInterrupt, ResolvedFdtProperty, ResolvedFdtSpecial, ResolvedFdtSpecialKind,
        },
    },
    vm::prepare::device_plan::ArchitectureVmPlan,
};

#[test]
fn firmware_selection_is_exhaustive_over_vm_boot_policy() {
    use crate::config::GuestBootPolicy;

    let uefi = config_with_boot_policy(GuestBootPolicy::AdjustKernelForBootProtocol {
        protocol: VMBootProtocol::Uefi,
    });
    let direct = config_with_boot_policy(GuestBootPolicy::AdjustKernelForBootProtocol {
        protocol: VMBootProtocol::Direct,
    });
    let keep = config_with_boot_policy(GuestBootPolicy::KeepConfigured);
    let multiboot = config_with_boot_policy(GuestBootPolicy::AdjustKernelForBootProtocol {
        protocol: VMBootProtocol::Multiboot,
    });

    assert_eq!(
        select_guest_firmware(&uefi).unwrap(),
        GuestFirmwareSelection::Uefi
    );
    assert_eq!(
        select_guest_firmware(&direct).unwrap(),
        GuestFirmwareSelection::DirectFdt
    );
    assert_eq!(
        select_guest_firmware(&keep).unwrap(),
        GuestFirmwareSelection::DirectFdt
    );
    let error = select_guest_firmware(&multiboot).unwrap_err();
    assert!(error.to_string().contains("Multiboot"));
}

#[test]
fn uefi_special_resolution_accepts_three_fdt_and_four_acpi_specials() {
    let serial = test_serial();
    let (fdt, mut acpi) = common_specials(serial);
    acpi.push(pci_special(0x3000_0000, 0x0400_0000));

    let special = resolve_special_firmware(&fdt, &acpi, serial).unwrap();

    assert_eq!(special.controller, InterruptControllerId::new(7));
    assert_eq!(special.pch_pic.base, 0x1000_0000);
    assert_eq!(special.fw_cfg.base, 0x1e02_0000);
    assert_eq!(
        special.pci_ecam,
        Some(MmioRegion {
            base: 0x3000_0000,
            size: 0x0400_0000,
        })
    );
}

#[test]
fn direct_fdt_discovery_policy_is_strict_without_probing_malformed_host_mcfg() {
    let config = config_with_boot_policy(
        crate::config::GuestBootPolicy::AdjustKernelForBootProtocol {
            protocol: VMBootProtocol::Direct,
        },
    );
    let firmware = select_guest_firmware(&config).unwrap();
    let plan = crate::arch::loongarch64::vm::plan_devices_for_test(&config).unwrap();
    let graph = plan.devices().graph();
    let serial = resolved_serial_from_graph(graph).unwrap();

    let platform = assemble_guest_platform(graph, firmware, serial, Vec::new(), |builder| {
        builder.build_with_host_pci_profile(Some(Err(AxVmError::invalid_config(
            "malformed host ACPI MCFG",
        ))))
    })
    .unwrap();

    assert!(platform.configured_acpi_devices.is_empty());
    assert_eq!(platform.pci, probe::qemu_guest_pci_profile());
}

#[test]
fn uefi_discovery_policy_rejects_malformed_host_mcfg() {
    let config = config_with_boot_policy(
        crate::config::GuestBootPolicy::AdjustKernelForBootProtocol {
            protocol: VMBootProtocol::Uefi,
        },
    );
    let firmware = select_guest_firmware(&config).unwrap();
    let plan = crate::arch::loongarch64::vm::plan_devices_for_test(&config).unwrap();
    let graph = plan.devices().graph();
    let serial = resolved_serial_from_graph(graph).unwrap();

    let error = assemble_guest_platform(graph, firmware, serial, Vec::new(), |builder| {
        builder.build_with_host_pci_profile(Some(Err(AxVmError::invalid_config(
            "malformed host ACPI MCFG",
        ))))
    })
    .unwrap_err();

    assert!(error.to_string().contains("malformed host ACPI MCFG"));
}

#[test]
fn pci_special_resolves_exactly_one_pci0_mmio_window() {
    assert_eq!(
        resolve_pci_ecam(&[pci_special(0x3000_0000, 0x0400_0000)]).unwrap(),
        MmioRegion {
            base: 0x3000_0000,
            size: 0x0400_0000,
        }
    );
}

#[test]
fn pci_special_rejects_missing_duplicate_and_malformed_contributions() {
    assert!(resolve_pci_ecam(&[]).is_err());
    assert!(
        resolve_pci_ecam(&[
            pci_special(0x2000_0000, 0x0800_0000),
            pci_special(0x3000_0000, 0x0400_0000),
        ])
        .is_err()
    );

    let mut wrong_name = pci_special(0x2000_0000, 0x0800_0000);
    wrong_name.name = "PCIX".into();
    assert!(resolve_pci_ecam(&[wrong_name]).is_err());

    let mut wrong_hid = pci_special(0x2000_0000, 0x0800_0000);
    wrong_hid.hid = Some("PNP0A03".into());
    assert!(resolve_pci_ecam(&[wrong_hid]).is_err());

    let mut wrong_kind = pci_special(0x2000_0000, 0x0800_0000);
    wrong_kind.kind = ResolvedAcpiSpecialKind::FirmwareTransport;
    assert!(resolve_pci_ecam(&[wrong_kind]).is_err());

    let mut multiple_registers = pci_special(0x2000_0000, 0x0800_0000);
    multiple_registers
        .registers
        .push(ResolvedAcpiRegister::Mmio {
            base: 0x3000_0000,
            size: 0x1000,
        });
    assert!(resolve_pci_ecam(&[multiple_registers]).is_err());

    let mut empty_registers = pci_special(0x2000_0000, 0x0800_0000);
    empty_registers.registers.clear();
    assert!(resolve_pci_ecam(&[empty_registers]).is_err());

    let mut pio_register = pci_special(0x2000_0000, 0x0800_0000);
    pio_register.registers = vec![ResolvedAcpiRegister::Pio {
        base: 0xcf8,
        size: 8,
    }];
    assert!(resolve_pci_ecam(&[pio_register]).is_err());

    let mut interrupt = pci_special(0x2000_0000, 0x0800_0000);
    interrupt
        .interrupts
        .push(acpi_interrupt(InterruptControllerId::new(7), 16));
    assert!(resolve_pci_ecam(&[interrupt]).is_err());

    let mut property = pci_special(0x2000_0000, 0x0800_0000);
    property
        .properties
        .push(ResolvedAcpiProperty::Empty("unexpected".into()));
    assert!(resolve_pci_ecam(&[property]).is_err());

    assert!(resolve_pci_ecam(&[pci_special(0x2000_1000, 0x0800_0000)]).is_err());
}

#[test]
fn uefi_orchestration_reconciles_graph_ecam_and_emits_checked_acpi() {
    let config = config_with_boot_policy(
        crate::config::GuestBootPolicy::AdjustKernelForBootProtocol {
            protocol: VMBootProtocol::Uefi,
        },
    );
    let plan = crate::arch::loongarch64::vm::plan_devices_for_test(&config).unwrap();
    let graph = plan.devices().graph();
    let serial = resolved_serial_from_graph(graph).unwrap();
    let profile = pci_profile_matching_graph(graph);

    let platform = assemble_guest_platform(
        graph,
        GuestFirmwareSelection::Uefi,
        serial,
        Vec::new(),
        |builder| builder.build_with_host_pci_profile(Some(Ok(Some(profile)))),
    )
    .unwrap();
    let resolved_ecam = profile.ecam;
    assert_eq!(platform.pci.ecam, resolved_ecam);

    let mut blobs = platform.fw_cfg_platform_config(1).unwrap().acpi;
    let mcfg = blobs
        .tables
        .windows(4)
        .position(|bytes| bytes == b"MCFG")
        .expect("firmware tables must contain MCFG");
    assert_eq!(
        u64::from_le_bytes(blobs.tables[mcfg + 44..mcfg + 52].try_into().unwrap()),
        resolved_ecam.base
    );
    let bus_end = u8::try_from((resolved_ecam.size >> 20) - 1).unwrap();
    assert_eq!(blobs.tables[mcfg + 54], 0);
    assert_eq!(blobs.tables[mcfg + 55], bus_end);

    let dsdt = blobs
        .tables
        .windows(4)
        .position(|bytes| bytes == b"DSDT")
        .expect("firmware tables must contain DSDT");
    let dsdt_len =
        u32::from_le_bytes(blobs.tables[dsdt + 4..dsdt + 8].try_into().unwrap()) as usize;
    let dsdt = &blobs.tables[dsdt..dsdt + dsdt_len];
    let bus = dsdt
        .windows(3)
        .position(|bytes| bytes == [0x88, 0x0d, 0x00])
        .expect("PCI0 _CRS must contain a bus-number descriptor");
    assert_eq!(
        u16::from_le_bytes(dsdt[bus + 8..bus + 10].try_into().unwrap()),
        0
    );
    assert_eq!(
        u16::from_le_bytes(dsdt[bus + 10..bus + 12].try_into().unwrap()),
        u16::from(bus_end)
    );
    assert_eq!(
        u16::from_le_bytes(dsdt[bus + 14..bus + 16].try_into().unwrap()),
        u16::from(bus_end) + 1
    );

    let mcfg_len =
        u32::from_le_bytes(blobs.tables[mcfg + 4..mcfg + 8].try_into().unwrap()) as usize;
    let checksum = loader_checksum_command(&blobs.loader, mcfg as u32)
        .expect("loader plan must checksum MCFG");
    assert_eq!(checksum, ((mcfg + 9) as u32, mcfg_len as u32));
    let sum = blobs.tables[mcfg..mcfg + mcfg_len]
        .iter()
        .fold(0u8, |sum, byte| sum.wrapping_add(*byte));
    blobs.tables[mcfg + 9] = blobs.tables[mcfg + 9].wrapping_sub(sum);
    assert_eq!(
        blobs.tables[mcfg..mcfg + mcfg_len]
            .iter()
            .fold(0u8, |sum, byte| sum.wrapping_add(*byte)),
        0
    );
}

#[test]
fn uefi_orchestration_rejects_profile_graph_ecam_mismatch() {
    let config = config_with_boot_policy(
        crate::config::GuestBootPolicy::AdjustKernelForBootProtocol {
            protocol: VMBootProtocol::Uefi,
        },
    );
    let plan = crate::arch::loongarch64::vm::plan_devices_for_test(&config).unwrap();
    let graph = plan.devices().graph();
    let serial = resolved_serial_from_graph(graph).unwrap();
    let mut profile = pci_profile_matching_graph(graph);
    let graph_ecam = profile.ecam;
    profile.ecam.base += 0x1000_0000;

    let error = assemble_guest_platform(
        graph,
        GuestFirmwareSelection::Uefi,
        serial,
        Vec::new(),
        |builder| builder.build_with_host_pci_profile(Some(Ok(Some(profile)))),
    )
    .unwrap_err();
    let diagnostic = error.to_string();
    assert!(diagnostic.contains(&std::format!(
        "{:#x}..{:#x}",
        profile.ecam.base,
        profile.ecam.base + profile.ecam.size
    )));
    assert!(diagnostic.contains(&std::format!(
        "{:#x}..{:#x}",
        graph_ecam.base,
        graph_ecam.base + graph_ecam.size
    )));
}

fn pci_profile_matching_graph(graph: &axdevice::ResolvedDeviceGraph) -> PciHost {
    let firmware = crate::boot::acpi::resolve_acpi_firmware(graph).unwrap();
    let mut profile = probe::qemu_guest_pci_profile();
    profile.ecam = resolve_pci_ecam(&firmware.specials).unwrap();
    profile
}

fn loader_checksum_command(loader: &[u8], start: u32) -> Option<(u32, u32)> {
    loader.chunks_exact(128).find_map(|command| {
        let kind = u32::from_le_bytes(command[0..4].try_into().unwrap());
        let command_start = u32::from_le_bytes(command[64..68].try_into().unwrap());
        let file_end = command[4..60]
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(56);
        if kind != 3 || &command[4..4 + file_end] != b"etc/acpi/tables" || command_start != start {
            return None;
        }
        Some((
            u32::from_le_bytes(command[60..64].try_into().unwrap()),
            u32::from_le_bytes(command[68..72].try_into().unwrap()),
        ))
    })
}

fn test_serial() -> SerialDevice {
    SerialDevice {
        mmio: MmioRegion {
            base: 0x1fe0_01e0,
            size: 0x100,
        },
        irq: 2,
        clock_hz: 100_000_000,
        baud: 115_200,
        register_shift: 0,
        register_width: axdevice_base::AccessWidth::Byte,
    }
}

fn common_specials(serial: SerialDevice) -> (Vec<ResolvedFdtSpecial>, Vec<ResolvedAcpiSpecial>) {
    let controller = InterruptControllerId::new(7);
    let fdt_interrupt = ResolvedFdtInterrupt {
        controller,
        input: serial.irq,
        trigger: InterruptTrigger::LevelTriggered,
    };
    let acpi_interrupt = acpi_interrupt(controller, serial.irq);
    (
        vec![
            ResolvedFdtSpecial {
                id: "pch-pic".into(),
                kind: ResolvedFdtSpecialKind::InterruptController(controller),
                node_name: "interrupt-controller".into(),
                compatible: vec!["loongson,pch-pic-1.0".into()],
                registers: vec![(0x1000_0000, 0x1000)],
                interrupts: Vec::new(),
                properties: Vec::new(),
            },
            ResolvedFdtSpecial {
                id: "fw-cfg".into(),
                kind: ResolvedFdtSpecialKind::FirmwareTransport,
                node_name: "fw_cfg".into(),
                compatible: vec!["qemu,fw-cfg-mmio".into()],
                registers: vec![(0x1e02_0000, 0x18)],
                interrupts: Vec::new(),
                properties: vec![ResolvedFdtProperty::Empty("dma-coherent".into())],
            },
            ResolvedFdtSpecial {
                id: "console0".into(),
                kind: ResolvedFdtSpecialKind::Console,
                node_name: "serial".into(),
                compatible: vec!["ns16550a".into()],
                registers: vec![(serial.mmio.base, serial.mmio.size)],
                interrupts: vec![fdt_interrupt],
                properties: vec![
                    ResolvedFdtProperty::U32("clock-frequency".into(), serial.clock_hz),
                    ResolvedFdtProperty::U32("reg-shift".into(), u32::from(serial.register_shift)),
                    ResolvedFdtProperty::U32("reg-io-width".into(), 1),
                ],
            },
        ],
        vec![
            ResolvedAcpiSpecial {
                id: "pch-pic".into(),
                kind: ResolvedAcpiSpecialKind::InterruptController(controller),
                name: "PCH0".into(),
                hid: None,
                registers: vec![ResolvedAcpiRegister::Mmio {
                    base: 0x1000_0000,
                    size: 0x1000,
                }],
                interrupts: Vec::new(),
                properties: Vec::new(),
            },
            ResolvedAcpiSpecial {
                id: "fw-cfg".into(),
                kind: ResolvedAcpiSpecialKind::FirmwareTransport,
                name: "FWCF".into(),
                hid: Some("QEMU0002".into()),
                registers: vec![ResolvedAcpiRegister::Mmio {
                    base: 0x1e02_0000,
                    size: 0x18,
                }],
                interrupts: Vec::new(),
                properties: Vec::new(),
            },
            ResolvedAcpiSpecial {
                id: "console0".into(),
                kind: ResolvedAcpiSpecialKind::Console,
                name: "COM0".into(),
                hid: Some("PNP0501".into()),
                registers: vec![ResolvedAcpiRegister::Mmio {
                    base: serial.mmio.base,
                    size: serial.mmio.size,
                }],
                interrupts: vec![acpi_interrupt],
                properties: vec![ResolvedAcpiProperty::U32(
                    "clock-frequency".into(),
                    serial.clock_hz,
                )],
            },
        ],
    )
}

fn acpi_interrupt(controller: InterruptControllerId, input: u32) -> ResolvedAcpiInterrupt {
    ResolvedAcpiInterrupt {
        controller,
        input,
        trigger: InterruptTrigger::LevelTriggered,
        sharing: InterruptSharing::Exclusive,
    }
}

fn config_with_boot_policy(policy: crate::config::GuestBootPolicy) -> crate::config::AxVMConfig {
    let mut catalog = crate::ConfiguredDeviceCatalog::new();
    crate::machine::register_devices(&mut catalog).unwrap();
    crate::config::AxVMConfig::new(crate::config::AxVMConfigParams {
        id: 1,
        name: "loongarch-firmware-selection-test".into(),
        phys_cpu_ls: crate::config::PhysCpuList::new(1, None, None),
        boot_policy: policy,
        virtual_device_catalog: std::sync::Arc::new(catalog),
        ..Default::default()
    })
}

fn pci_special(base: u64, size: u64) -> ResolvedAcpiSpecial {
    ResolvedAcpiSpecial {
        id: "pci-ecam".into(),
        kind: ResolvedAcpiSpecialKind::PciHostBridge,
        name: "PCI0".into(),
        hid: Some("PNP0A08".into()),
        registers: vec![ResolvedAcpiRegister::Mmio { base, size }],
        interrupts: Vec::new(),
        properties: Vec::new(),
    }
}
