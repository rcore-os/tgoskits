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

extern crate alloc;

mod error;

use alloc::{string::String, vec::Vec};
use core::fmt::{Debug, Display, Formatter, LowerHex, UpperHex};

use ax_memory_addr::{AddrRange, PhysAddr, VirtAddr, def_usize_addr, def_usize_addr_formatter};
pub use error::{VmBackendError, VmBackendResult};

bitflags::bitflags! {
    /// Generic memory mapping permissions and attributes exchanged between
    /// AxVM components.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct MappingFlags: usize {
        /// The memory is readable.
        const READ          = 1 << 0;
        /// The memory is writable.
        const WRITE         = 1 << 1;
        /// The memory is executable.
        const EXECUTE       = 1 << 2;
        /// The memory is user accessible.
        const USER          = 1 << 3;
        /// The memory is device memory.
        const DEVICE        = 1 << 4;
        /// The memory is uncached.
        const UNCACHED      = 1 << 5;
    }
}

impl Debug for MappingFlags {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        Debug::fmt(&self.0, f)
    }
}

/// Virtual machine identifier.
pub type VMId = usize;

/// Virtual CPU identifier within a VM.
pub type VCpuId = usize;

/// Interrupt vector number injected into a guest.
pub type InterruptVector = u8;

/// Interrupt trigger mode.
///
/// Represents the trigger mode of an interrupt in a platform-neutral way.
/// Architectures that do not distinguish between edge and level triggering
/// can ignore this parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptTriggerMode {
    /// Edge-triggered interrupt.
    EdgeTriggered,
    /// Level-triggered interrupt.
    LevelTriggered,
}

/// Identifier of an interrupt line within a virtual machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrqLineId(pub usize);

/// The maximum number of virtual CPUs supported in a virtual machine.
pub const MAX_VCPU_NUM: usize = 64;

/// A set of virtual CPUs.
pub type VCpuSet = ax_cpumask::CpuMask<MAX_VCPU_NUM>;

/// Host virtual address.
pub type HostVirtAddr = VirtAddr;

/// Host physical address.
pub type HostPhysAddr = PhysAddr;

/// Architecture-specific nested paging configuration selected by AxVM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NestedPagingConfig {
    /// Root physical address of the nested page table.
    pub root_paddr: HostPhysAddr,
    /// Number of page-table levels.
    pub levels: usize,
    /// Guest physical address width in bits.
    pub gpa_bits: usize,
    /// Architecture-specific hardware mode encoding.
    pub mode: usize,
}

impl NestedPagingConfig {
    /// Creates a nested paging configuration.
    pub const fn new(
        root_paddr: HostPhysAddr,
        levels: usize,
        gpa_bits: usize,
        mode: usize,
    ) -> Self {
        Self {
            root_paddr,
            levels,
            gpa_bits,
            mode,
        }
    }
}

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

/// The width of a guest bus access.
///
/// The term "word" follows the x86 convention and means 16 bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AccessWidth {
    /// 8-bit access.
    Byte,
    /// 16-bit access.
    Word,
    /// 32-bit access.
    Dword,
    /// 64-bit access.
    Qword,
}

impl TryFrom<usize> for AccessWidth {
    type Error = ();

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Byte),
            2 => Ok(Self::Word),
            4 => Ok(Self::Dword),
            8 => Ok(Self::Qword),
            _ => Err(()),
        }
    }
}

impl From<AccessWidth> for usize {
    fn from(width: AccessWidth) -> usize {
        match width {
            AccessWidth::Byte => 1,
            AccessWidth::Word => 2,
            AccessWidth::Dword => 4,
            AccessWidth::Qword => 8,
        }
    }
}

impl AccessWidth {
    /// Returns the size of this access in bytes.
    pub fn size(&self) -> usize {
        (*self).into()
    }

    /// Returns the bit range covered by this access.
    pub fn bits_range(&self) -> core::ops::Range<usize> {
        match self {
            AccessWidth::Byte => 0..8,
            AccessWidth::Word => 0..16,
            AccessWidth::Dword => 0..32,
            AccessWidth::Qword => 0..64,
        }
    }
}

/// The port number of an x86 I/O operation.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Port(pub u16);

impl Port {
    /// Creates a new [`Port`].
    pub const fn new(port: u16) -> Self {
        Self(port)
    }

    /// Returns the raw port number.
    pub const fn number(&self) -> u16 {
        self.0
    }
}

impl LowerHex for Port {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "Port({:#x})", self.0)
    }
}

impl UpperHex for Port {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "Port({:#X})", self.0)
    }
}

impl Debug for Port {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "Port({})", self.0)
    }
}

/// A system register address.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct SysRegAddr(pub usize);

impl SysRegAddr {
    /// Creates a new [`SysRegAddr`].
    pub const fn new(addr: usize) -> Self {
        Self(addr)
    }

    /// Returns the raw register address.
    pub const fn addr(&self) -> usize {
        self.0
    }
}

impl From<usize> for SysRegAddr {
    fn from(value: usize) -> Self {
        Self(value)
    }
}

impl LowerHex for SysRegAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "SysRegAddr({:#x})", self.0)
    }
}

impl UpperHex for SysRegAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "SysRegAddr({:#X})", self.0)
    }
}

impl Debug for SysRegAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "SysRegAddr({})", self.0)
    }
}

/// Information about a nested guest page-table fault.
#[derive(Debug)]
pub struct NestedPageFaultInfo {
    /// Access type that caused the nested page fault.
    pub access_flags: MappingFlags,
    /// Guest physical address that caused the nested page fault.
    pub fault_guest_paddr: GuestPhysAddr,
}

/// Legacy/common normalized VM event.
///
/// New AxVM architecture backends should expose their raw VM-exit type through
/// [`VmArchVcpuOps::Exit`] and handle it inside their `axvm::arch` module.
/// This enum remains for compatibility and as a transitional normalized event
/// shape for backends that have not split out an architecture-owned exit enum.
#[non_exhaustive]
#[derive(Debug)]
pub enum VmExit {
    /// A guest instruction triggered a hypercall to the hypervisor.
    Hypercall {
        /// Hypercall number.
        nr: u64,
        /// Hypercall arguments.
        args: [u64; 6],
    },
    /// The guest performed an MMIO read.
    MmioRead {
        /// Guest physical address being read.
        addr: GuestPhysAddr,
        /// Access width.
        width: AccessWidth,
        /// Destination guest register.
        reg: usize,
        /// Destination register width.
        reg_width: AccessWidth,
        /// Whether the value should be sign-extended.
        signed_ext: bool,
    },
    /// The guest performed an MMIO write.
    MmioWrite {
        /// Guest physical address being written.
        addr: GuestPhysAddr,
        /// Access width.
        width: AccessWidth,
        /// Value written by the guest.
        data: u64,
    },
    /// The guest performed a system register read.
    SysRegRead {
        /// System register address.
        addr: SysRegAddr,
        /// Destination guest register.
        reg: usize,
    },
    /// The guest performed a system register write.
    SysRegWrite {
        /// System register address.
        addr: SysRegAddr,
        /// Value written by the guest.
        value: u64,
    },
    /// The guest performed an x86 port I/O read.
    IoRead {
        /// Port number.
        port: Port,
        /// Access width.
        width: AccessWidth,
    },
    /// The guest performed an x86 port I/O write.
    IoWrite {
        /// Port number.
        port: Port,
        /// Access width.
        width: AccessWidth,
        /// Value written by the guest.
        data: u64,
    },
    /// An external interrupt was delivered to the vCPU.
    ExternalInterrupt {
        /// Interrupt vector number.
        vector: u64,
    },
    /// A nested page fault occurred during guest execution.
    NestedPageFault {
        /// Guest physical address that caused the fault.
        addr: GuestPhysAddr,
        /// Access type that caused the fault.
        access_flags: MappingFlags,
    },
    /// The guest halted.
    Halt,
    /// The guest reached an idle instruction.
    Idle,
    /// The guest requested secondary CPU startup.
    CpuUp {
        /// Target CPU identifier in the architecture namespace.
        target_cpu: u64,
        /// Secondary entry point.
        entry_point: GuestPhysAddr,
        /// Secondary boot argument.
        arg: u64,
    },
    /// The guest powered down one vCPU.
    CpuDown {
        /// Architecture power-state payload.
        _state: u64,
    },
    /// The guest requested VM shutdown.
    SystemDown,
    /// No VMM action is required.
    Nothing,
    /// Hardware virtualization preemption timer expired.
    PreemptionTimer,
    /// The guest completed interrupt service with EOI.
    InterruptEnd {
        /// EOI vector, when available.
        vector: Option<u8>,
    },
    /// VM entry failed.
    FailEntry {
        /// Architecture-specific failure code.
        hardware_entry_failure_reason: u64,
    },
    /// The guest requested an IPI.
    SendIPI {
        /// Target CPU identifier in the architecture namespace.
        target_cpu: u64,
        /// Auxiliary target selector.
        target_cpu_aux: u64,
        /// Whether to broadcast to all CPUs except the sender.
        send_to_all: bool,
        /// Whether to target the current vCPU.
        send_to_self: bool,
        /// IPI vector.
        vector: u64,
    },
}

/// Architecture-specific vCPU operations consumed by AxVM.
pub trait VmArchVcpuOps: Sized {
    /// Architecture-specific creation configuration.
    type CreateConfig;
    /// Architecture-specific setup configuration.
    type SetupConfig;
    /// Architecture-specific VM-exit type returned by [`Self::run`].
    type Exit: Debug;

    /// Creates a new architecture-specific vCPU.
    fn new(vm_id: VMId, vcpu_id: VCpuId, config: Self::CreateConfig) -> VmBackendResult<Self>;
    /// Sets the guest entry point.
    fn set_entry(&mut self, entry: GuestPhysAddr) -> VmBackendResult;
    /// Sets the nested page table selected by AxVM.
    fn set_nested_page_table(&mut self, config: NestedPagingConfig) -> VmBackendResult;
    /// Completes architecture-specific setup.
    fn setup(&mut self, config: Self::SetupConfig) -> VmBackendResult;
    /// Runs the vCPU until an architecture-specific VM exit.
    fn run(&mut self) -> VmBackendResult<Self::Exit>;
    /// Binds the vCPU to the current physical CPU.
    fn bind(&mut self) -> VmBackendResult;
    /// Unbinds the vCPU from the current physical CPU.
    fn unbind(&mut self) -> VmBackendResult;
    /// Sets a general-purpose register.
    fn set_gpr(&mut self, reg: usize, val: usize);
    /// Decodes an architecture-specific memory fault as a legacy normalized
    /// MMIO event when possible.
    ///
    /// This is kept as a transition helper for backends that still route
    /// device faults through [`VmExit`]. New raw vCPU exits should use
    /// [`Self::Exit`] and be handled in the architecture-local AxVM adapter.
    fn decode_mmio_fault(
        &mut self,
        _fault_addr: GuestPhysAddr,
        _access_flags: MappingFlags,
    ) -> Option<VmExit> {
        None
    }
    /// Injects an interrupt into the vCPU.
    fn inject_interrupt(&mut self, vector: usize) -> VmBackendResult;
    /// Injects an interrupt with trigger-mode metadata.
    fn inject_interrupt_with_trigger(
        &mut self,
        vector: usize,
        trigger: InterruptTriggerMode,
    ) -> VmBackendResult {
        debug_assert!(
            trigger == InterruptTriggerMode::EdgeTriggered,
            "level-triggered interrupt injection requires an architecture-specific implementation"
        );
        self.inject_interrupt(vector)
    }
    /// Processes a guest EOI and returns an external EOI vector when needed.
    fn handle_eoi(&mut self) -> Option<u8> {
        None
    }
    /// Sets the guest return value.
    fn set_return_value(&mut self, val: usize);
}

/// Architecture-specific per-CPU virtualization state consumed by AxVM.
pub trait VmArchPerCpuOps: Sized {
    /// Creates a new per-CPU state.
    fn new(cpu_id: usize) -> VmBackendResult<Self>;
    /// Whether virtualization is enabled on the current CPU.
    fn is_enabled(&self) -> bool;
    /// Enables virtualization on the current CPU.
    fn hardware_enable(&mut self) -> VmBackendResult;
    /// Disables virtualization on the current CPU.
    fn hardware_disable(&mut self) -> VmBackendResult;
    /// Returns the max guest page table levels supported by this architecture.
    fn max_guest_page_table_levels(&self) -> usize {
        4
    }
    /// Returns the guest physical address width supported by this CPU.
    fn guest_phys_addr_bits(&self) -> usize {
        match self.max_guest_page_table_levels() {
            0..=3 => 39,
            _ => 48,
        }
    }
}

/// Execution state of an AxVM-owned vCPU wrapper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VmVcpuState {
    /// Invalid state.
    Invalid = 0,
    /// Initial state after vCPU creation.
    Created = 1,
    /// vCPU is initialized and free.
    Free    = 2,
    /// vCPU is bound and ready to run.
    Ready   = 3,
    /// vCPU is currently running.
    Running = 4,
    /// vCPU is blocked.
    Blocked = 5,
}

/// A part of `AxVMConfig`, which represents guest VM type.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum VMType {
    /// Host VM, used for boot from Linux like Jailhouse do, named "type1.5".
    VMTHostVM = 0,
    /// Guest RTOS, generally a simple guest OS with most of the resource passthrough.
    #[default]
    VMTRTOS   = 1,
    /// Guest Linux, generally a full-featured guest OS with complicated device emulation requirements.
    VMTLinux  = 2,
}

impl From<usize> for VMType {
    fn from(value: usize) -> Self {
        match value {
            0 => Self::VMTHostVM,
            1 => Self::VMTRTOS,
            2 => Self::VMTLinux,
            _ => Self::default(),
        }
    }
}

impl From<VMType> for usize {
    fn from(value: VMType) -> Self {
        value as usize
    }
}

/// Guest physical address space population policy.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AddressSpacePolicy {
    /// Start from an empty guest physical address space and map only explicit
    /// guest memory, boot-description regions, and explicitly configured
    /// passthrough resources.
    #[default]
    Virtualized,
    /// Start from a host-physical identity passthrough address space, then
    /// punch holes for guest memory, boot-description regions, emulated
    /// devices, and reserved ranges.
    Passthrough,
}

/// The type of memory mapping used for VM memory regions.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum VmMemMappingType {
    /// The memory region is allocated by the VM monitor.
    #[default]
    MapAlloc     = 0,
    /// The memory region is identical to the host physical memory region.
    MapIdentical = 1,
    /// The memory region is reserved memory for the guest OS.
    MapReserved  = 2,
}

/// Configuration for a virtual machine memory region.
#[derive(Debug, Default, Clone)]
pub struct VmMemConfig {
    /// The start address of the memory region in GPA (Guest Physical Address).
    pub gpa: usize,
    /// The size of the memory region in bytes.
    pub size: usize,
    /// The mappings flags of the memory region.
    pub flags: usize,
    /// The type of memory mapping.
    pub map_type: VmMemMappingType,
}

/// A part of `AxVMConfig`, which represents the configuration of an emulated device for a virtual machine.
#[derive(Debug, Default, Clone)]
pub struct EmulatedDeviceConfig {
    /// The name of the device.
    pub name: String,
    /// The base GPA (Guest Physical Address) of the device.
    pub base_gpa: usize,
    /// The address length of the device.
    pub length: usize,
    /// The IRQ (Interrupt Request) ID of the device.
    pub irq_id: usize,
    /// The type of emulated device.
    pub emu_type: EmulatedDeviceType,
    /// The config list of the device.
    pub cfg_list: Vec<usize>,
}

/// A part of `AxVMConfig`, which represents the configuration of a pass-through device for a virtual machine.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct PassThroughDeviceConfig {
    /// The name of the device.
    pub name: String,
    /// The base GPA (Guest Physical Address) of the device.
    pub base_gpa: usize,
    /// The base HPA (Host Physical Address) of the device.
    pub base_hpa: usize,
    /// The address length of the device.
    pub length: usize,
    /// The IRQ (Interrupt Request) ID of the device.
    pub irq_id: usize,
}

/// A part of `AxVMConfig`, which represents the configuration of a pass-through address for a virtual machine.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct PassThroughAddressConfig {
    /// The base GPA (Guest Physical Address).
    pub base_gpa: usize,
    /// The address length.
    pub length: usize,
}

/// A guest physical address range reserved from default passthrough mapping.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReservedAddressConfig {
    /// The base GPA (Guest Physical Address).
    pub base_gpa: usize,
    /// The address length.
    pub length: usize,
}

/// A part of `AxVMConfig`, which represents a host I/O port range passed through
/// to a virtual machine.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PassThroughPortConfig {
    /// The first host I/O port number.
    pub base: u16,
    /// The number of ports in this range.
    pub length: u16,
}

/// Describes how a guest VM should enter its boot image.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum VMBootProtocol {
    /// Enter the configured kernel entry directly without a firmware image.
    #[default]
    Direct,
    /// Use the legacy x86 axvm-bios/multiboot trampoline.
    Multiboot,
    /// Load an external UEFI firmware image and enter it without multiboot patching.
    Uefi,
}

/// Specifies how the VM should handle interrupts and interrupt controllers.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum VMInterruptMode {
    /// The VM will not handle interrupts, and the guest OS should not use interrupts.
    #[default]
    NoIrq,
    /// The VM will use the emulated interrupt controller to handle interrupts.
    Emulated,
    /// Physical device interrupts are forwarded to the guest, while emulated
    /// devices and virtual timers use software interrupt injection.
    Hybrid,
    /// The VM will use the passthrough interrupt controller (including GPPT) to handle interrupts.
    Passthrough,
}

/// A GIC shared peripheral interrupt represented by its zero-based SPI offset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Aarch64GicSpi(u32);

impl Aarch64GicSpi {
    const INTID_BASE: u32 = 32;
    const SPI_COUNT: u32 = 988;

    /// Creates a validated GIC SPI from its zero-based offset.
    pub const fn new(offset: u32) -> Option<Self> {
        if offset < Self::SPI_COUNT {
            Some(Self(offset))
        } else {
            None
        }
    }

    /// Returns the architectural GIC interrupt ID.
    pub const fn intid(self) -> u32 {
        self.0 + Self::INTID_BASE
    }
}

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
    /// QEMU fw_cfg MMIO device.
    FwCfg               = 0x3,
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
    /// LoongArch virtual PCH-PIC device.
    LoongArchPchPic     = 0x25,

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aarch64_gic_spi_validates_offset_and_intid() {
        let first = Aarch64GicSpi::new(0).unwrap();
        let last = Aarch64GicSpi::new(987).unwrap();

        assert_eq!(first.intid(), 32);
        assert_eq!(last.intid(), 1019);
        assert!(Aarch64GicSpi::new(988).is_none());
    }

    struct MockPerCpu {
        enabled: bool,
    }

    impl VmArchPerCpuOps for MockPerCpu {
        fn new(_cpu_id: usize) -> VmBackendResult<Self> {
            Ok(Self { enabled: false })
        }

        fn is_enabled(&self) -> bool {
            self.enabled
        }

        fn hardware_enable(&mut self) -> VmBackendResult {
            self.enabled = true;
            Ok(())
        }

        fn hardware_disable(&mut self) -> VmBackendResult {
            self.enabled = false;
            Ok(())
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    enum MockExit {
        SysRegRead { reg: usize },
    }

    struct MockVcpu;

    impl VmArchVcpuOps for MockVcpu {
        type CreateConfig = ();
        type SetupConfig = ();
        type Exit = MockExit;

        fn new(
            _vm_id: VMId,
            _vcpu_id: VCpuId,
            _config: Self::CreateConfig,
        ) -> VmBackendResult<Self> {
            Ok(Self)
        }

        fn set_entry(&mut self, _entry: GuestPhysAddr) -> VmBackendResult {
            Ok(())
        }

        fn set_nested_page_table(&mut self, _config: NestedPagingConfig) -> VmBackendResult {
            Ok(())
        }

        fn setup(&mut self, _config: Self::SetupConfig) -> VmBackendResult {
            Ok(())
        }

        fn run(&mut self) -> VmBackendResult<Self::Exit> {
            Ok(MockExit::SysRegRead { reg: 2 })
        }

        fn bind(&mut self) -> VmBackendResult {
            Ok(())
        }

        fn unbind(&mut self) -> VmBackendResult {
            Ok(())
        }

        fn set_gpr(&mut self, _reg: usize, _val: usize) {}

        fn inject_interrupt(&mut self, _vector: usize) -> VmBackendResult {
            Ok(())
        }

        fn set_return_value(&mut self, _val: usize) {}
    }

    #[test]
    fn vcpu_protocol_lives_in_axvm_types() {
        let mut percpu = MockPerCpu::new(0).unwrap();
        assert!(!percpu.is_enabled());
        percpu.hardware_enable().unwrap();
        assert!(percpu.is_enabled());

        let mut vcpu = MockVcpu::new(1, 0, ()).unwrap();
        vcpu.set_entry(GuestPhysAddr::from(0x8020_0000)).unwrap();
        vcpu.set_nested_page_table(NestedPagingConfig::new(
            HostPhysAddr::from(0x1000),
            4,
            48,
            0,
        ))
        .unwrap();
        vcpu.setup(()).unwrap();
        assert!(matches!(
            vcpu.run().unwrap(),
            MockExit::SysRegRead { reg: 2 }
        ));
    }

    #[test]
    fn vm_exit_keeps_access_width_and_state_types() {
        let state = VmVcpuState::Created;
        assert_eq!(state as u8, 1);

        let exit = VmExit::MmioRead {
            addr: GuestPhysAddr::from(0x1000),
            width: AccessWidth::Dword,
            reg: 3,
            reg_width: AccessWidth::Qword,
            signed_ext: true,
        };
        assert!(matches!(
            exit,
            VmExit::MmioRead {
                width: AccessWidth::Dword,
                reg: 3,
                ..
            }
        ));
    }
}

impl Display for EmulatedDeviceType {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            EmulatedDeviceType::Console => write!(f, "console"),
            EmulatedDeviceType::FwCfg => write!(f, "fw_cfg"),
            EmulatedDeviceType::InterruptController => write!(f, "interrupt controller"),
            EmulatedDeviceType::GPPTRedistributor => {
                write!(f, "gic partial passthrough redistributor")
            }
            EmulatedDeviceType::GPPTDistributor => write!(f, "gic partial passthrough distributor"),
            EmulatedDeviceType::GPPTITS => write!(f, "gic partial passthrough its"),
            EmulatedDeviceType::X86IoApic => write!(f, "x86 io apic"),
            EmulatedDeviceType::X86Pit => write!(f, "x86 pit"),
            EmulatedDeviceType::LoongArchPchPic => write!(f, "loongarch pch pic"),
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
    pub const ALL: [Self; 15] = [
        EmulatedDeviceType::Dummy,
        EmulatedDeviceType::InterruptController,
        EmulatedDeviceType::Console,
        EmulatedDeviceType::FwCfg,
        EmulatedDeviceType::IVCChannel,
        EmulatedDeviceType::GPPTRedistributor,
        EmulatedDeviceType::GPPTDistributor,
        EmulatedDeviceType::GPPTITS,
        EmulatedDeviceType::X86IoApic,
        EmulatedDeviceType::X86Pit,
        EmulatedDeviceType::LoongArchPchPic,
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
            0x3 => Some(EmulatedDeviceType::FwCfg),
            0xA => Some(EmulatedDeviceType::IVCChannel),
            0x20 => Some(EmulatedDeviceType::GPPTRedistributor),
            0x21 => Some(EmulatedDeviceType::GPPTDistributor),
            0x22 => Some(EmulatedDeviceType::GPPTITS),
            0x23 => Some(EmulatedDeviceType::X86IoApic),
            0x24 => Some(EmulatedDeviceType::X86Pit),
            0x25 => Some(EmulatedDeviceType::LoongArchPchPic),
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
