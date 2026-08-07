//! Architecture machine profiles for guest-visible platform devices.
//!
//! User configuration selects physical devices only. Virtual platform resources
//! are owned by the machine profile; firmware-backed hosts may replace the
//! default virtual UART with the host-selected UART's register model, layout,
//! address, and firmware identity before VM construction.

use std::{string::String, vec, vec::Vec};

use axdevice_base::AccessWidth;

use crate::{arch::CurrentArch, architecture::MachinePlatform};

mod factory;
mod gic;
mod ivc;
mod plic;
mod serial;
mod timer;

pub(crate) use factory::{
    SERIAL_REGISTRATIONS, fallback_profile as default_serial_profile, is_serial_model,
    model_name as serial_model_name,
};
pub(crate) use gic::AARCH64_GIC_REDISTRIBUTOR_FRAME_SIZE;
pub use gic::{
    GuestGicCpuRegion, GuestGicProfile, GuestGicProfileError, GuestGicRedistributorProfile,
    GuestItsProfile,
};
pub use ivc::GuestIvcChannel;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub(crate) use ivc::resolved_ivc_channels;
pub use plic::{GuestPlicProfile, GuestPlicProfileError};
#[cfg(any(
    target_arch = "aarch64",
    target_arch = "riscv64",
    target_arch = "x86_64"
))]
pub(crate) use serial::ResolvedSerialDevice;
#[cfg(any(
    target_arch = "loongarch64",
    all(target_arch = "x86_64", feature = "host-fs")
))]
pub(crate) use serial::host_serial_from_acpi;
pub(crate) use serial::resolved_serial_devices;
pub use serial::{
    GuestClockReference, GuestSerialAcpiIdentity, GuestSerialFdtIdentity, GuestSerialFdtInterrupt,
    GuestSerialFirmwareIdentity, GuestSerialModel, GuestSerialProfile, GuestSerialTransport,
    HostSerialSnapshot,
};
pub use timer::GuestTimerProfile;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64", test))]
pub(crate) use timer::decode_timer_ppi;

/// One guest-visible MMIO region selected by a machine profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuestMmioRegion {
    /// Register base.
    pub base: usize,
    /// Register span.
    pub length: usize,
}

/// Virtual platform resources for one architecture machine.
#[derive(Clone, Debug)]
pub struct MachineProfile {
    /// Mandatory guest serial port.
    pub serial: GuestSerialProfile,
    /// Common FDT interrupt encoding, if this machine uses the common FDT path.
    pub serial_fdt_interrupt: Option<GuestSerialFdtInterrupt>,
    /// Machine-owned architectural timer resources, when described in FDT.
    pub timer: Option<GuestTimerProfile>,
    /// Machine-owned GIC resources, when used by this architecture.
    pub gic: Option<GuestGicProfile>,
    /// Machine-owned PLIC resources, when used by this architecture.
    pub plic: Option<GuestPlicProfile>,
    /// Physical-device discovery root used for default passthrough assignment.
    ///
    /// `None` means that the architecture's address-space policy alone
    /// provides the default mapping and no unresolved discovery selector
    /// should enter the runtime mapping planner.
    pub default_passthrough_device_path: Option<&'static str>,
}

fn x86_64_profile() -> MachineProfile {
    let serial = GuestSerialProfile {
        model: GuestSerialModel::Uart16550,
        transport: GuestSerialTransport::Port {
            base: 0x3f8,
            length: 8,
        },
        irq: 4,
        clock_hz: 1_843_200,
    };
    MachineProfile {
        serial,
        serial_fdt_interrupt: None,
        timer: None,
        gic: None,
        plic: None,
        default_passthrough_device_path: None,
    }
}

fn aarch64_profile(cpu_num: usize) -> MachineProfile {
    let cpu_num = cpu_num.max(1);
    let serial = GuestSerialProfile {
        model: GuestSerialModel::Pl011,
        transport: GuestSerialTransport::Mmio {
            base: 0x0900_0000,
            length: 0x1000,
            register_shift: 0,
            register_width: AccessWidth::Dword,
        },
        irq: 33,
        clock_hz: 24_000_000,
    };
    MachineProfile {
        serial,
        serial_fdt_interrupt: Some(GuestSerialFdtInterrupt::GicSpi),
        timer: Some(GuestTimerProfile {
            node_path: "/timer".into(),
            node_phandle: None,
            interrupt_parent: None,
            interrupt_specifiers: vec![
                vec![1, 13, 4],
                vec![1, 14, 4],
                vec![1, 11, 4],
                vec![1, 10, 4],
            ],
            secure_physical_intid: 29,
            nonsecure_physical_intid: 30,
            virtual_intid: 27,
            hypervisor_intid: 26,
            clock_frequency_hz: None,
        }),
        gic: Some(GuestGicProfile {
            compatible: String::from("arm,gic-v3"),
            node_path: String::from("/intc@8000000"),
            node_phandle: None,
            distributor: GuestMmioRegion {
                base: 0x0800_0000,
                length: 0x1_0000,
            },
            cpu_region: GuestGicCpuRegion::Redistributors(GuestGicRedistributorProfile {
                regions: vec![GuestMmioRegion {
                    base: 0x080a_0000,
                    length: cpu_num.saturating_mul(AARCH64_GIC_REDISTRIBUTOR_FRAME_SIZE),
                }],
                stride: AARCH64_GIC_REDISTRIBUTOR_FRAME_SIZE,
            }),
            its: Vec::new(),
        }),
        plic: None,
        default_passthrough_device_path: Some("/"),
    }
}

fn riscv64_profile(_cpu_num: usize) -> MachineProfile {
    let serial = GuestSerialProfile {
        model: GuestSerialModel::Uart16550,
        transport: GuestSerialTransport::Mmio {
            base: 0x1000_0000,
            length: 0x100,
            register_shift: 0,
            register_width: AccessWidth::Byte,
        },
        irq: 10,
        clock_hz: 3_686_400,
    };
    MachineProfile {
        serial,
        serial_fdt_interrupt: Some(GuestSerialFdtInterrupt::PlicSource),
        timer: None,
        gic: None,
        plic: Some(GuestPlicProfile {
            node_path: String::from("/soc/plic@c000000"),
            node_phandle: None,
            base: 0x0c00_0000,
            length: 0x60_0000,
        }),
        default_passthrough_device_path: Some("/"),
    }
}

fn loongarch64_profile() -> MachineProfile {
    let serial = GuestSerialProfile {
        model: GuestSerialModel::Uart16550,
        transport: GuestSerialTransport::Mmio {
            base: 0x1fe0_01e0,
            length: 0x100,
            register_shift: 0,
            register_width: AccessWidth::Byte,
        },
        irq: 2,
        clock_hz: 100_000_000,
    };
    MachineProfile {
        serial,
        serial_fdt_interrupt: None,
        timer: None,
        gic: None,
        plic: None,
        default_passthrough_device_path: Some("/"),
    }
}

/// Architecture identity used by host tools that inspect machine profiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MachineArchitecture {
    X86_64,
    Aarch64,
    Riscv64,
    LoongArch64,
}

/// Returns the fixed profile for an architecture machine.
pub fn machine_profile_for(architecture: MachineArchitecture, cpu_num: usize) -> MachineProfile {
    match architecture {
        MachineArchitecture::X86_64 => x86_64_profile(),
        MachineArchitecture::Aarch64 => aarch64_profile(cpu_num),
        MachineArchitecture::Riscv64 => riscv64_profile(cpu_num),
        MachineArchitecture::LoongArch64 => loongarch64_profile(),
    }
}

/// Returns the machine profile selected by the architecture boundary.
pub fn current_machine_profile(cpu_num: usize) -> MachineProfile {
    machine_profile_for(CurrentArch::MACHINE_ARCHITECTURE, cpu_num)
}

#[cfg(test)]
mod tests;
