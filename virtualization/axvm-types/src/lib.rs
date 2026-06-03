// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Shared base types for AxVM and virtualization capability components.
//!
//! This crate intentionally contains only small value types and aliases. It is
//! not a host capability API and must not depend on any OS-specific crate.

#![no_std]

use core::fmt::{Display, Formatter};

use ax_memory_addr::{AddrRange, PhysAddr, VirtAddr, def_usize_addr, def_usize_addr_formatter};

/// Virtual machine identifier.
pub type VMId = usize;

/// Virtual CPU identifier within a VM.
pub type VCpuId = usize;

/// Interrupt vector number injected into a guest.
pub type InterruptVector = u8;

/// The maximum number of virtual CPUs supported in a virtual machine.
pub const MAX_VCPU_NUM: usize = 64;

/// A set of virtual CPUs.
pub type VCpuSet = ax_cpumask::CpuMask<MAX_VCPU_NUM>;

/// Host virtual address.
pub type HostVirtAddr = VirtAddr;

/// Host physical address.
pub type HostPhysAddr = PhysAddr;

def_usize_addr! {
    /// Guest virtual address.
    pub type GuestVirtAddr;

    /// Guest physical address.
    pub type GuestPhysAddr;
}

def_usize_addr_formatter! {
    GuestVirtAddr = "GVA:{}";
    GuestPhysAddr = "GPA:{}";
}

/// Guest virtual address range.
pub type GuestVirtAddrRange = AddrRange<GuestVirtAddr>;

/// Guest physical address range.
pub type GuestPhysAddrRange = AddrRange<GuestPhysAddr>;

/// Common AxVM result type.
pub type AxVmResult<T = ()> = ax_errno::AxResult<T>;

/// Common AxVM error type.
pub type AxVmError = ax_errno::AxError;

/// The type of emulated device.
///
/// Allocation scheme:
/// - 0x00 - 0x1F: Special devices, and abstract device types that does not specify a concrete
///   interface or implementation. The device objects created from these types depend on the target
///   architecture and the specific implementation of the hypervisor.
/// - 0x20 - 0x7F: Concrete emulated device types.
///   - 0x20 - 0x2F: Interrupt controller devices.
///   - 0x30 - 0x3F: Reserved for future use.
/// - 0x80 - 0xDF: Reserved for future use.
/// - 0xE0 - 0xEF: Virtio devices.
/// - 0xF0 - 0xFF: Reserved for future use.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum EmulatedDeviceType {
    // Special devices and abstract device types.
    /// Dummy device type.
    #[default]
    Dummy               = 0x0,
    /// Interrupt controller device, e.g. vGICv2 in aarch64, vLAPIC in x86.
    InterruptController = 0x1,
    /// Console (serial) device.
    Console             = 0x2,
    /// An emulated device that provides Inter-VM Communication (IVC) channel.
    ///
    /// This device is used for communication between different VMs,
    /// the corresponding memory region of this device should be marked as `Reserved` in
    /// device tree or ACPI table.
    IVCChannel          = 0xA,

    // Arch-specific interrupt controller devices.
    // 0x20 - 0x22: GPPT (GIC Partial Passthrough) devices.
    /// ARM GIC Partial Passthrough Redistributor device.
    GPPTRedistributor   = 0x20,
    /// ARM GIC Partial Passthrough Distributor device.
    GPPTDistributor     = 0x21,
    /// ARM GIC Partial Passthrough Interrupt Translation Service device.
    GPPTITS             = 0x22,

    // 0x23 - 0x24: x86 platform devices.
    /// x86 virtual IO APIC device.
    X86IoApic           = 0x23,
    /// x86 virtual PIT/8254 timer device.
    X86Pit              = 0x24,

    // 0x30: PPPT (PLIC Partial Passthrough) devices.
    /// RISC-V PLIC Partial Passthrough Global device.
    PPPTGlobal          = 0x30,

    // Virtio devices.
    /// Virtio block device.
    VirtioBlk           = 0xE1,
    /// Virtio net device.
    VirtioNet           = 0xE2,
    /// Virtio console device.
    VirtioConsole       = 0xE3,
    // Following are some other emulated devices that are not currently used and removed from the enum temporarily.
    // /// IOMMU device.
    // IOMMU = 0x6,
    // /// Interrupt ICC SRE device.
    // ICCSRE = 0x7,
    // /// Interrupt ICC SGIR device.
    // SGIR = 0x8,
    // /// Interrupt controller GICR device.
    // GICR = 0x9,
}

impl Display for EmulatedDeviceType {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            EmulatedDeviceType::Console => write!(f, "console"),
            EmulatedDeviceType::InterruptController => write!(f, "interrupt controller"),
            EmulatedDeviceType::GPPTRedistributor => {
                write!(f, "gic partial passthrough redistributor")
            }
            EmulatedDeviceType::GPPTDistributor => write!(f, "gic partial passthrough distributor"),
            EmulatedDeviceType::GPPTITS => write!(f, "gic partial passthrough its"),
            EmulatedDeviceType::X86IoApic => write!(f, "x86 io apic"),
            EmulatedDeviceType::X86Pit => write!(f, "x86 pit"),
            EmulatedDeviceType::PPPTGlobal => write!(f, "plic partial passthrough global"),
            // EmulatedDeviceType::IOMMU => write!(f, "iommu"),
            // EmulatedDeviceType::ICCSRE => write!(f, "interrupt icc sre"),
            // EmulatedDeviceType::SGIR => write!(f, "interrupt icc sgir"),
            // EmulatedDeviceType::GICR => write!(f, "interrupt controller gicr"),
            EmulatedDeviceType::IVCChannel => write!(f, "ivc channel"),
            EmulatedDeviceType::Dummy => write!(f, "meta device"),
            EmulatedDeviceType::VirtioBlk => write!(f, "virtio block"),
            EmulatedDeviceType::VirtioNet => write!(f, "virtio net"),
            EmulatedDeviceType::VirtioConsole => write!(f, "virtio console"),
        }
    }
}

impl EmulatedDeviceType {
    /// All known emulated device types.
    pub const ALL: [Self; 13] = [
        EmulatedDeviceType::Dummy,
        EmulatedDeviceType::InterruptController,
        EmulatedDeviceType::Console,
        EmulatedDeviceType::IVCChannel,
        EmulatedDeviceType::GPPTRedistributor,
        EmulatedDeviceType::GPPTDistributor,
        EmulatedDeviceType::GPPTITS,
        EmulatedDeviceType::X86IoApic,
        EmulatedDeviceType::X86Pit,
        EmulatedDeviceType::PPPTGlobal,
        EmulatedDeviceType::VirtioBlk,
        EmulatedDeviceType::VirtioNet,
        EmulatedDeviceType::VirtioConsole,
    ];

    /// Returns all known emulated device types.
    pub const fn all() -> &'static [Self] {
        &Self::ALL
    }

    /// Returns true if the device is removable.
    pub fn removable(&self) -> bool {
        matches!(
            *self,
            EmulatedDeviceType::InterruptController
                // | EmulatedDeviceType::SGIR
                // | EmulatedDeviceType::ICCSRE
                | EmulatedDeviceType::GPPTRedistributor
                | EmulatedDeviceType::X86IoApic
                | EmulatedDeviceType::X86Pit
                | EmulatedDeviceType::VirtioBlk
                | EmulatedDeviceType::VirtioNet
                // | EmulatedDeviceType::GICR
                | EmulatedDeviceType::VirtioConsole
        )
    }

    /// Converts a `usize` value to an `EmulatedDeviceType`.
    pub const fn from_usize(value: usize) -> Option<Self> {
        match value {
            0x0 => Some(EmulatedDeviceType::Dummy),
            0x1 => Some(EmulatedDeviceType::InterruptController),
            0x2 => Some(EmulatedDeviceType::Console),
            0xA => Some(EmulatedDeviceType::IVCChannel),
            0x20 => Some(EmulatedDeviceType::GPPTRedistributor),
            0x21 => Some(EmulatedDeviceType::GPPTDistributor),
            0x22 => Some(EmulatedDeviceType::GPPTITS),
            0x23 => Some(EmulatedDeviceType::X86IoApic),
            0x24 => Some(EmulatedDeviceType::X86Pit),
            0x30 => Some(EmulatedDeviceType::PPPTGlobal),
            0xE1 => Some(EmulatedDeviceType::VirtioBlk),
            0xE2 => Some(EmulatedDeviceType::VirtioNet),
            0xE3 => Some(EmulatedDeviceType::VirtioConsole),
            // 0x6 => EmulatedDeviceType::IOMMU,
            // 0x7 => EmulatedDeviceType::ICCSRE,
            // 0x8 => EmulatedDeviceType::SGIR,
            // 0x9 => EmulatedDeviceType::GICR,
            _ => None,
        }
    }
}

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
impl ax_page_table_multiarch::riscv::SvVirtAddr for GuestPhysAddr {
    /// Flushes the TLB for the entire address space.
    ///
    /// `hfence.vvma` does not access host memory.
    fn flush_tlb(_vaddr: Option<Self>) {
        unsafe {
            core::arch::asm!("hfence.vvma", options(nostack, nomem, preserves_flags));
        }
    }
}
