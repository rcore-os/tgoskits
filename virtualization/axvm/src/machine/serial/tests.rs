use ax_driver::probe::acpi::*;

use super::*;

#[test]
fn host_spcr_serial_becomes_owned_guest_snapshot() {
    let serial = AcpiSerialConsole {
        interface: AcpiSerialInterface::Pl011,
        address_space: AcpiSerialAddressSpace::Memory,
        registers: AcpiResourceRange {
            base: 0x900_0000,
            size: 0x1000,
        },
        access_size: 3,
        irq: Some(33),
        baud_rate: Some(115_200),
        clock_hz: Some(48_000_000),
        namespace_path: Some("\\_SB_.COM0".into()),
    };
    let snapshot = host_serial_from_acpi(serial.clone(), fallback()).unwrap();

    assert_eq!(snapshot.profile.model, GuestSerialModel::Pl011);
    assert_eq!(
        snapshot.profile.transport,
        GuestSerialTransport::Mmio {
            base: 0x900_0000,
            length: 0x1000,
            register_shift: 0,
            register_width: AccessWidth::Dword,
        }
    );
    assert_eq!(snapshot.profile.irq, 33);
    assert_eq!(snapshot.profile.clock_hz, 48_000_000);
    assert_eq!(
        snapshot.identity,
        GuestSerialFirmwareIdentity::Acpi(GuestSerialAcpiIdentity {
            namespace_path: Some("\\_SB_.COM0".into()),
            source_table: *b"SPCR",
        })
    );

    assert!(
        host_serial_from_acpi(
            AcpiSerialConsole {
                irq: None,
                ..serial
            },
            fallback(),
        )
        .is_err()
    );
}

const fn fallback() -> GuestSerialProfile {
    GuestSerialProfile {
        model: GuestSerialModel::Uart16550,
        transport: GuestSerialTransport::Port {
            base: 0x3f8,
            length: 8,
        },
        irq: 4,
        clock_hz: 1_843_200,
    }
}
