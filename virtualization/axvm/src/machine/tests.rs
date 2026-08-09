use super::*;

#[test]
fn only_device_discovery_machines_emit_a_default_root_selector() {
    assert_eq!(x86_64_profile().default_passthrough_device_path, None);
    assert_eq!(
        aarch64_profile(1).default_passthrough_device_path,
        Some("/")
    );
    assert_eq!(
        riscv64_profile(2).default_passthrough_device_path,
        Some("/")
    );
    assert_eq!(
        loongarch64_profile().default_passthrough_device_path,
        Some("/")
    );
}

#[test]
fn machine_serial_resources_match_guest_platform_contract() {
    assert_eq!(
        x86_64_profile().serial,
        GuestSerialProfile {
            model: GuestSerialModel::Uart16550,
            transport: GuestSerialTransport::Port {
                base: 0x3f8,
                length: 8,
            },
            irq: 4,
            clock_hz: 1_843_200,
        }
    );
    assert_eq!(
        aarch64_profile(1).serial,
        GuestSerialProfile {
            model: GuestSerialModel::Pl011,
            transport: GuestSerialTransport::Mmio {
                base: 0x0900_0000,
                length: 0x1000,
                register_shift: 0,
                register_width: AccessWidth::Dword,
            },
            irq: 33,
            clock_hz: 24_000_000,
        }
    );
    assert_eq!(
        riscv64_profile(2).serial,
        GuestSerialProfile {
            model: GuestSerialModel::Uart16550,
            transport: GuestSerialTransport::Mmio {
                base: 0x1000_0000,
                length: 0x100,
                register_shift: 0,
                register_width: AccessWidth::Byte,
            },
            irq: 10,
            clock_hz: 3_686_400,
        }
    );
    assert_eq!(
        loongarch64_profile().serial,
        GuestSerialProfile {
            model: GuestSerialModel::Uart16550,
            transport: GuestSerialTransport::Mmio {
                base: 0x1fe0_01e0,
                length: 0x100,
                register_shift: 0,
                register_width: AccessWidth::Byte,
            },
            irq: 2,
            clock_hz: 100_000_000,
        }
    );
}
