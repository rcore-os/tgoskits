use std::{collections::BTreeMap, vec::Vec};

use super::*;

#[derive(Default)]
struct FakeIo {
    ports: BTreeMap<u16, u32>,
    gcs: BTreeMap<u64, u32>,
    writes: Vec<(u8, u64, u32)>,
    ignore_gcs_writes: bool,
    ignore_smi_enable_writes: bool,
}

impl RegisterIo for FakeIo {
    fn read_port16(&mut self, port: u16) -> u16 {
        self.ports.get(&port).copied().unwrap_or_default() as u16
    }

    fn write_port16(&mut self, port: u16, value: u16) {
        self.writes.push((2, u64::from(port), u32::from(value)));
        let old = self.ports.get(&port).copied().unwrap_or_default();
        // The two status registers are write-one-to-clear.
        let value = if matches!(port & 0x1f, 0x04 | 0x06) {
            old & !u32::from(value)
        } else {
            u32::from(value)
        };
        self.ports.insert(port, value);
    }

    fn read_port32(&mut self, port: u16) -> u32 {
        self.ports.get(&port).copied().unwrap_or_default()
    }

    fn write_port32(&mut self, port: u16, value: u32) {
        self.writes.push((4, u64::from(port), value));
        if !self.ignore_smi_enable_writes {
            self.ports.insert(port, value);
        }
    }

    fn read_gcs32(&mut self, address: u64) -> u32 {
        self.gcs.get(&address).copied().unwrap_or_default()
    }

    fn write_gcs32(&mut self, address: u64, value: u32) {
        self.writes.push((5, address, value));
        if !self.ignore_gcs_writes {
            self.gcs.insert(address, value);
        }
    }
}

fn ich9() -> (SupportedDevice, Resources) {
    let device = identify(0x8086, 0x2918).unwrap();
    let resources = Resources::from_chipset_config(device, 0x0400, 0xfed1_0001).unwrap();
    (device, resources)
}

#[test]
fn admission_and_resource_decoding_reject_unknown_or_disabled_devices() {
    assert!(identify(0x1234, 0x2918).is_none());
    assert!(identify(0x8086, 0xffff).is_none());

    let ich9 = identify(0x8086, 0x2918).unwrap();
    assert!(matches!(
        Resources::from_chipset_config(ich9, 0x0400, 0xfed1_0000),
        Err(Error::InvalidResources)
    ));
    let v6 = identify(0x8086, 0x54a3).unwrap();
    let v6_resources = Resources::from_chipset_config(v6, 0x0500, 0x100).unwrap();
    assert!(matches!(
        Tco::new(FakeIo::default(), ich9, v6_resources),
        Err(Error::InvalidResources)
    ));
    assert!(matches!(
        Resources::from_chipset_config(v6, 0x0501, 0),
        Err(Error::InvalidResources)
    ));
}

#[test]
fn timeout_conversion_covers_hardware_limits_without_overflow() {
    assert_eq!(timeout_to_ticks(3), Ok(5));
    assert_eq!(timeout_to_ticks(614), Ok(1023));
    assert_eq!(timeout_to_ticks(2), Err(Error::InvalidTimeout));
    assert_eq!(timeout_to_ticks(615), Err(Error::InvalidTimeout));
    assert_eq!(timeout_to_ticks(u32::MAX), Err(Error::InvalidTimeout));
}

#[test]
fn start_and_stop_are_explicit_and_verify_effective_register_state() {
    let (device, resources) = ich9();
    let gcs = resources.no_reboot_gcs().unwrap();
    let mut io = FakeIo::default();
    io.ports
        .insert(resources.tco_port() + 0x08, u32::from(regs::HALT));
    io.ports.insert(resources.tco_port() + 0x06, 4);
    io.ports
        .insert(resources.smi_enable_port().unwrap(), 1 << 13);
    io.gcs.insert(gcs, 0);
    let mut tco = Tco::new(io, device, resources).unwrap();

    assert!(!tco.is_running());
    assert!(!tco.boot_status());
    assert!(tco.start(300).is_ok());
    assert!(tco.is_running());
    assert!(tco.boot_status());
    assert_eq!(tco.timeout_seconds(), Some(300));

    let io = tco.into_io();
    assert_eq!(
        io.ports.get(&(resources.tco_port() + 0x12)).copied(),
        Some(500)
    );
    assert_eq!(
        io.ports.get(&resources.smi_enable_port().unwrap()).copied(),
        Some(0)
    );
    assert_eq!(
        io.ports
            .get(&(resources.tco_port() + 0x08))
            .copied()
            .unwrap() as u16
            & regs::HALT,
        0
    );
    assert_eq!(io.gcs.get(&gcs).copied().unwrap() & regs::NO_REBOOT_V2, 0);

    let mut tco = Tco::new(io, device, resources).unwrap();
    // The fixture reflects the running hardware. Takeover is explicit.
    assert_eq!(tco.start(30), Err(Error::AlreadyRunning));
    assert_eq!(tco.adopt_running(30), Ok(()));
    assert_eq!(tco.stop(), Ok(()));
    assert!(!tco.is_running());
    let io = tco.into_io();
    assert_ne!(
        io.ports
            .get(&(resources.tco_port() + 0x08))
            .copied()
            .unwrap() as u16
            & regs::HALT,
        0
    );
    assert_ne!(io.gcs.get(&gcs).copied().unwrap() & regs::NO_REBOOT_V2, 0);
}

#[test]
fn locked_no_reboot_readback_does_not_claim_watchdog_started() {
    let (device, resources) = ich9();
    let mut io = FakeIo {
        ignore_gcs_writes: true,
        ..FakeIo::default()
    };
    io.ports
        .insert(resources.tco_port() + 0x08, u32::from(regs::HALT));
    io.gcs
        .insert(resources.no_reboot_gcs().unwrap(), regs::NO_REBOOT_V2);
    let mut tco = Tco::new(io, device, resources).unwrap();
    assert_eq!(tco.start(30), Err(Error::Locked));
    assert!(!tco.is_running());
}

#[test]
fn stop_cannot_disarm_a_watchdog_without_explicit_ownership() {
    let (device, resources) = ich9();
    let mut io = FakeIo::default();
    io.ports.insert(resources.tco_port() + regs::CONTROL1, 0);
    let mut tco = Tco::new(io, device, resources).unwrap();

    assert_eq!(tco.stop(), Err(Error::BadState));
    let io = tco.into_io();
    assert!(io.writes.is_empty());
    assert_eq!(
        io.ports.get(&(resources.tco_port() + regs::CONTROL1)),
        Some(&0)
    );
}

#[test]
fn v6_does_not_report_legacy_boot_status_without_defined_semantics() {
    let device = identify(0x8086, 0x54a3).unwrap();
    let resources = Resources::from_chipset_config(device, 0x0500, 0x0100).unwrap();
    let mut io = FakeIo::default();
    io.ports
        .insert(resources.tco_port() + regs::CONTROL1, u32::from(regs::HALT));
    io.ports.insert(
        resources.tco_port() + regs::STATUS2,
        u32::from(regs::BOOT_STATUS),
    );
    let mut tco = Tco::new(io, device, resources).unwrap();

    assert_eq!(tco.start(30), Ok(()));
    assert!(!tco.boot_status());
}

#[test]
fn start_does_not_claim_running_when_smi_control_write_is_ignored() {
    let (device, resources) = ich9();
    let smi_port = resources.smi_enable_port().unwrap();
    let mut io = FakeIo {
        ignore_smi_enable_writes: true,
        ..FakeIo::default()
    };
    io.ports
        .insert(resources.tco_port() + regs::CONTROL1, u32::from(regs::HALT));
    io.ports.insert(smi_port, 1 << regs::SMI_WDT_CLEAR_BIT);
    let mut tco = Tco::new(io, device, resources).unwrap();

    assert_eq!(tco.start(30), Err(Error::Locked));
    assert!(!tco.is_running());
    let io = tco.into_io();
    assert_ne!(
        io.ports.get(&smi_port).copied().unwrap_or_default() & (1 << regs::SMI_WDT_CLEAR_BIT),
        0
    );
    assert!(
        !io.writes
            .iter()
            .any(|(_, address, _)| *address == u64::from(resources.tco_port() + regs::TIMER))
    );
}

#[test]
fn failed_adoption_keeps_the_already_running_watchdog_manageable() {
    let (device, resources) = ich9();
    let smi_port = resources.smi_enable_port().unwrap();
    let mut io = FakeIo {
        ignore_smi_enable_writes: true,
        ..FakeIo::default()
    };
    io.ports.insert(resources.tco_port() + regs::CONTROL1, 0);
    io.ports.insert(smi_port, 1 << regs::SMI_WDT_CLEAR_BIT);
    let mut tco = Tco::new(io, device, resources).unwrap();

    assert_eq!(tco.adopt_running(30), Err(Error::Locked));
    assert!(tco.is_running());
    assert_eq!(tco.timeout_seconds(), None);
    assert_eq!(tco.ping(), Ok(()));
    assert_eq!(tco.stop(), Ok(()));
    assert!(!tco.is_running());
}
