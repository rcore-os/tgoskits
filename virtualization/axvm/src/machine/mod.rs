//! Architecture machine profiles for guest-visible platform devices.
//!
//! User configuration selects physical devices only. Virtual platform resources
//! are owned by the machine profile; firmware-backed hosts may replace the
//! default virtual UART with the host-selected UART's register model, layout,
//! address, and firmware identity before VM construction.

use alloc::{vec, vec::Vec};

use axdevice_base::AccessWidth;
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

use crate::{arch::CurrentArch, architecture::MachinePlatform};

mod factory;
mod gic;
mod serial;
mod timer;

pub(crate) use factory::register_machine_device_factories_from_config;
pub(crate) use gic::AARCH64_GIC_REDISTRIBUTOR_FRAME_SIZE;
pub use gic::{
    GuestGicCpuRegion, GuestGicProfile, GuestGicProfileError, GuestGicRedistributorProfile,
    GuestItsProfile,
};
pub(crate) use serial::serial_device_config;
pub use serial::{
    GuestClockReference, GuestSerialFdtIdentity, GuestSerialFdtInterrupt, GuestSerialModel,
    GuestSerialProfile, GuestSerialTransport,
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

/// Host firmware resources retained by the virtual PLIC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestPlicProfile {
    /// Absolute path of the host PLIC node.
    pub node_path: alloc::string::String,
    /// PLIC node phandle, when supplied by firmware.
    pub node_phandle: Option<u32>,
    /// Guest-visible PLIC register base.
    pub base: usize,
    /// Guest-visible PLIC register span.
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
    /// Physical-device discovery root used for default passthrough assignment.
    ///
    /// `None` means that the architecture's address-space policy alone
    /// provides the default mapping and no unresolved discovery selector
    /// should enter the runtime mapping planner.
    pub default_passthrough_device_path: Option<&'static str>,
    /// Internal device construction descriptors.
    pub emulated_devices: Vec<EmulatedDeviceConfig>,
}

fn device(
    name: &str,
    base_gpa: usize,
    length: usize,
    irq_id: usize,
    emu_type: EmulatedDeviceType,
    cfg_list: Vec<usize>,
) -> EmulatedDeviceConfig {
    EmulatedDeviceConfig {
        name: name.into(),
        base_gpa,
        length,
        irq_id,
        emu_type,
        cfg_list,
    }
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
        default_passthrough_device_path: None,
        emulated_devices: vec![
            serial_device_config(serial),
            device("fw_cfg", 0x510, 0x0c, 0, EmulatedDeviceType::FwCfg, vec![]),
            device(
                "ioapic",
                0xfec0_0000,
                0x1000,
                0,
                EmulatedDeviceType::X86IoApic,
                vec![],
            ),
            device("pit", 0x40, 0x22, 0, EmulatedDeviceType::X86Pit, vec![]),
            device("pic", 0x20, 2, 0, EmulatedDeviceType::X86Pic, vec![]),
            device("cmos", 0x70, 2, 0, EmulatedDeviceType::X86Cmos, vec![]),
            device(
                "pci-config",
                0xcf8,
                8,
                0,
                EmulatedDeviceType::X86PciConfig,
                vec![],
            ),
            device(
                "acpi-pm",
                0x600,
                0x80,
                9,
                EmulatedDeviceType::X86AcpiPmTimer,
                vec![],
            ),
        ],
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
        default_passthrough_device_path: Some("/"),
        emulated_devices: vec![
            device(
                "vgic",
                0x0800_0000,
                0x1_0000,
                0,
                EmulatedDeviceType::InterruptController,
                vec![],
            ),
            device(
                "gic-redistributor",
                0x080a_0000,
                cpu_num.saturating_mul(AARCH64_GIC_REDISTRIBUTOR_FRAME_SIZE),
                0,
                EmulatedDeviceType::GicCpuRegion,
                vec![cpu_num, AARCH64_GIC_REDISTRIBUTOR_FRAME_SIZE],
            ),
            serial_device_config(serial),
        ],
    }
}

fn riscv64_profile(cpu_num: usize) -> MachineProfile {
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
        default_passthrough_device_path: Some("/"),
        emulated_devices: vec![
            device(
                "plic",
                0x0c00_0000,
                0x60_0000,
                0,
                EmulatedDeviceType::PPPTGlobal,
                vec![cpu_num * 2],
            ),
            serial_device_config(serial),
        ],
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
        default_passthrough_device_path: Some("/"),
        emulated_devices: vec![
            device(
                "fw_cfg",
                0x1e02_0000,
                0x18,
                0,
                EmulatedDeviceType::FwCfg,
                vec![],
            ),
            device(
                "pch-pic",
                0x1000_0000,
                0x1000,
                0,
                EmulatedDeviceType::LoongArchPchPic,
                vec![],
            ),
            serial_device_config(serial),
        ],
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
